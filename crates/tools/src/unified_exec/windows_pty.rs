#![cfg(windows)]
#![allow(clippy::upper_case_acronyms)]

use anyhow::{Error, bail, ensure};
use filedescriptor::{FileDescriptor, OwnedHandle, Pipe};
use lazy_static::lazy_static;
use portable_pty::cmdbuilder::CommandBuilder;
use portable_pty::{
    Child, ChildKiller, ExitStatus, MasterPty, PtyPair, PtySize, PtySystem, SlavePty,
};
use shared_library::shared_library;
use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{Error as IoError, Result as IoResult};
use std::mem;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Mutex};
use winapi::shared::minwindef::DWORD;
use winapi::shared::ntdef::NTSTATUS;
use winapi::shared::ntstatus::STATUS_SUCCESS;
use winapi::shared::winerror::{HRESULT, S_OK};
use winapi::um::handleapi::*;
use winapi::um::minwinbase::STILL_ACTIVE;
use winapi::um::processthreadsapi::*;
use winapi::um::synchapi::WaitForSingleObject;
use winapi::um::winbase::{
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE, STARTF_USESTDHANDLES,
    STARTUPINFOEXW,
};
use winapi::um::wincon::COORD;
use winapi::um::winnt::{HANDLE, OSVERSIONINFOW};

type HPCON = HANDLE;

const PSEUDOCONSOLE_RESIZE_QUIRK: DWORD = 0x2;
const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x00020016;
const MIN_CONPTY_BUILD: u32 = 17_763;

shared_library!(ConPtyFuncs,
    pub fn CreatePseudoConsole(
        size: COORD,
        hInput: HANDLE,
        hOutput: HANDLE,
        flags: DWORD,
        hpc: *mut HPCON
    ) -> HRESULT,
    pub fn ResizePseudoConsole(hpc: HPCON, size: COORD) -> HRESULT,
    pub fn ClosePseudoConsole(hpc: HPCON),
);

shared_library!(Ntdll,
    pub fn RtlGetVersion(version_info: *mut OSVERSIONINFOW) -> NTSTATUS,
);

lazy_static! {
    static ref CONPTY: ConPtyFuncs = load_conpty();
}

fn load_conpty() -> ConPtyFuncs {
    let kernel = ConPtyFuncs::open(Path::new("kernel32.dll")).expect(
        "this system does not support conpty. Windows 10 October 2018 or newer is required",
    );

    ConPtyFuncs::open(Path::new("conpty.dll")).unwrap_or(kernel)
}

pub fn conpty_supported() -> bool {
    windows_build_number().is_some_and(|build| build >= MIN_CONPTY_BUILD)
}

fn windows_build_number() -> Option<u32> {
    let ntdll = Ntdll::open(Path::new("ntdll.dll")).ok()?;
    let mut info: OSVERSIONINFOW = unsafe { mem::zeroed() };
    info.dwOSVersionInfoSize = mem::size_of::<OSVERSIONINFOW>() as u32;
    let status = unsafe { (ntdll.RtlGetVersion)(&mut info) };
    (status == STATUS_SUCCESS).then_some(info.dwBuildNumber)
}

pub struct ConPtySystem;

impl PtySystem for ConPtySystem {
    fn openpty(&self, size: PtySize) -> anyhow::Result<PtyPair> {
        if !conpty_supported() {
            bail!("ConPTY requires Windows 10 October 2018 or newer");
        }

        let stdin = Pipe::new()?;
        let stdout = Pipe::new()?;
        let con = PsuedoCon::new(
            COORD {
                X: size.cols as i16,
                Y: size.rows as i16,
            },
            stdin.read,
            stdout.write,
        )?;

        let master = ConPtyMasterPty {
            inner: Arc::new(Mutex::new(Inner {
                con,
                readable: stdout.read,
                writable: Some(stdin.write),
                size,
            })),
        };
        let slave = ConPtySlavePty {
            inner: Arc::clone(&master.inner),
        };

        Ok(PtyPair {
            master: Box::new(master),
            slave: Box::new(slave),
        })
    }
}

struct Inner {
    con: PsuedoCon,
    readable: FileDescriptor,
    writable: Option<FileDescriptor>,
    size: PtySize,
}

pub struct ConPtyMasterPty {
    inner: Arc<Mutex<Inner>>,
}

pub struct ConPtySlavePty {
    inner: Arc<Mutex<Inner>>,
}

impl MasterPty for ConPtyMasterPty {
    fn resize(&self, size: PtySize) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.con.resize(COORD {
            X: size.cols as i16,
            Y: size.rows as i16,
        })?;
        inner.size = size;
        Ok(())
    }

    fn get_size(&self) -> Result<PtySize, Error> {
        Ok(self.inner.lock().unwrap().size)
    }

    fn try_clone_reader(&self) -> anyhow::Result<Box<dyn std::io::Read + Send>> {
        Ok(Box::new(self.inner.lock().unwrap().readable.try_clone()?))
    }

    fn take_writer(&self) -> anyhow::Result<Box<dyn std::io::Write + Send>> {
        Ok(Box::new(
            self.inner
                .lock()
                .unwrap()
                .writable
                .take()
                .ok_or_else(|| anyhow::anyhow!("writer already taken"))?,
        ))
    }
}

impl SlavePty for ConPtySlavePty {
    fn spawn_command(&self, cmd: CommandBuilder) -> anyhow::Result<Box<dyn Child + Send + Sync>> {
        let child = self.inner.lock().unwrap().con.spawn_command(cmd)?;
        Ok(Box::new(child))
    }
}

pub struct PsuedoCon {
    con: HPCON,
    _input: FileDescriptor,
    _output: FileDescriptor,
}

unsafe impl Send for PsuedoCon {}
unsafe impl Sync for PsuedoCon {}

impl Drop for PsuedoCon {
    fn drop(&mut self) {
        unsafe { (CONPTY.ClosePseudoConsole)(self.con) };
    }
}

impl PsuedoCon {
    fn new(size: COORD, input: FileDescriptor, output: FileDescriptor) -> Result<Self, Error> {
        let mut con: HPCON = INVALID_HANDLE_VALUE;
        let result = unsafe {
            (CONPTY.CreatePseudoConsole)(
                size,
                input.as_raw_handle() as _,
                output.as_raw_handle() as _,
                PSEUDOCONSOLE_RESIZE_QUIRK,
                &mut con,
            )
        };
        ensure!(
            result == S_OK,
            "failed to create pseudo console: HRESULT {result}"
        );
        Ok(Self {
            con,
            _input: input,
            _output: output,
        })
    }

    fn resize(&self, size: COORD) -> Result<(), Error> {
        let result = unsafe { (CONPTY.ResizePseudoConsole)(self.con, size) };
        ensure!(
            result == S_OK,
            "failed to resize console to {}x{}: HRESULT: {}",
            size.X,
            size.Y,
            result
        );
        Ok(())
    }

    fn spawn_command(&self, cmd: CommandBuilder) -> anyhow::Result<WinChild> {
        let mut startup_info: STARTUPINFOEXW = unsafe { mem::zeroed() };
        startup_info.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;
        startup_info.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup_info.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
        startup_info.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
        startup_info.StartupInfo.hStdError = INVALID_HANDLE_VALUE;

        let mut attrs = ProcThreadAttributeList::with_capacity(/*num_attributes*/ 1)?;
        attrs.set_pty(self.con)?;
        startup_info.lpAttributeList = attrs.as_mut_ptr();

        let mut process_info: PROCESS_INFORMATION = unsafe { mem::zeroed() };
        let (mut exe, mut cmdline) = build_cmdline(&cmd)?;
        let cmd_os = OsString::from_wide(&cmdline);
        let cwd = resolve_current_directory(&cmd);
        let mut env_block = build_environment_block(&cmd);

        let res = unsafe {
            CreateProcessW(
                exe.as_mut_ptr(),
                cmdline.as_mut_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                env_block.as_mut_ptr() as *mut _,
                cwd.as_ref().map_or(ptr::null(), Vec::as_ptr),
                &mut startup_info.StartupInfo,
                &mut process_info,
            )
        };
        if res == 0 {
            let err = IoError::last_os_error();
            let msg = format!(
                "CreateProcessW `{:?}` in cwd `{:?}` failed: {}",
                cmd_os,
                cwd.as_ref().map(|c| OsString::from_wide(c)),
                err
            );
            bail!("{msg}");
        }

        let _main_thread = unsafe { OwnedHandle::from_raw_handle(process_info.hThread as _) };
        let proc = unsafe { OwnedHandle::from_raw_handle(process_info.hProcess as _) };
        Ok(WinChild {
            proc: Mutex::new(proc),
        })
    }
}

struct ProcThreadAttributeList {
    data: Vec<u8>,
}

impl ProcThreadAttributeList {
    fn with_capacity(num_attributes: DWORD) -> Result<Self, Error> {
        let mut bytes_required: usize = 0;
        unsafe {
            InitializeProcThreadAttributeList(
                ptr::null_mut(),
                num_attributes,
                0,
                &mut bytes_required,
            )
        };
        let mut data = Vec::with_capacity(bytes_required);
        unsafe { data.set_len(bytes_required) };

        let attr_ptr = data.as_mut_slice().as_mut_ptr() as *mut _;
        let res = unsafe {
            InitializeProcThreadAttributeList(attr_ptr, num_attributes, 0, &mut bytes_required)
        };
        ensure!(
            res != 0,
            "InitializeProcThreadAttributeList failed: {}",
            IoError::last_os_error()
        );
        Ok(Self { data })
    }

    fn as_mut_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.data.as_mut_slice().as_mut_ptr() as *mut _
    }

    fn set_pty(&mut self, con: HPCON) -> Result<(), Error> {
        let res = unsafe {
            UpdateProcThreadAttribute(
                self.as_mut_ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                con,
                mem::size_of::<HPCON>(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        ensure!(
            res != 0,
            "UpdateProcThreadAttribute failed: {}",
            IoError::last_os_error()
        );
        Ok(())
    }
}

impl Drop for ProcThreadAttributeList {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.as_mut_ptr()) };
    }
}

#[derive(Debug)]
pub struct WinChild {
    proc: Mutex<OwnedHandle>,
}

impl WinChild {
    fn is_complete(&mut self) -> IoResult<Option<ExitStatus>> {
        let mut status: DWORD = 0;
        let proc = self.proc.lock().unwrap().try_clone().unwrap();
        let res = unsafe { GetExitCodeProcess(proc.as_raw_handle() as _, &mut status) };
        if res != 0 {
            if status == STILL_ACTIVE {
                Ok(None)
            } else {
                Ok(Some(ExitStatus::with_exit_code(status)))
            }
        } else {
            Ok(None)
        }
    }

    fn do_kill(&mut self) -> IoResult<()> {
        let proc = self.proc.lock().unwrap().try_clone().unwrap();
        let res = unsafe { TerminateProcess(proc.as_raw_handle() as _, 1) };
        if res == 0 {
            Err(IoError::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl ChildKiller for WinChild {
    fn kill(&mut self) -> IoResult<()> {
        self.do_kill().ok();
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        let proc = self.proc.lock().unwrap().try_clone().unwrap();
        Box::new(WinChildKiller { proc })
    }
}

#[derive(Debug)]
pub struct WinChildKiller {
    proc: OwnedHandle,
}

impl ChildKiller for WinChildKiller {
    fn kill(&mut self) -> IoResult<()> {
        let res = unsafe { TerminateProcess(self.proc.as_raw_handle() as _, 1) };
        if res == 0 {
            Err(IoError::last_os_error())
        } else {
            Ok(())
        }
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        let proc = self.proc.try_clone().unwrap();
        Box::new(WinChildKiller { proc })
    }
}

impl Child for WinChild {
    fn try_wait(&mut self) -> IoResult<Option<ExitStatus>> {
        self.is_complete()
    }

    fn wait(&mut self) -> IoResult<ExitStatus> {
        if let Ok(Some(status)) = self.try_wait() {
            return Ok(status);
        }
        let proc = self.proc.lock().unwrap().try_clone().unwrap();
        unsafe {
            WaitForSingleObject(proc.as_raw_handle() as _, INFINITE);
        }
        let mut status: DWORD = 0;
        let res = unsafe { GetExitCodeProcess(proc.as_raw_handle() as _, &mut status) };
        if res != 0 {
            Ok(ExitStatus::with_exit_code(status))
        } else {
            Err(IoError::last_os_error())
        }
    }

    fn process_id(&self) -> Option<u32> {
        let res = unsafe { GetProcessId(self.proc.lock().unwrap().as_raw_handle() as _) };
        if res == 0 { None } else { Some(res) }
    }

    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        Some(self.proc.lock().unwrap().as_raw_handle())
    }
}

fn resolve_current_directory(cmd: &CommandBuilder) -> Option<Vec<u16>> {
    let home = cmd
        .get_env("USERPROFILE")
        .and_then(|path| Path::new(path).is_dir().then(|| path.to_owned()));
    let cwd = cmd
        .get_cwd()
        .and_then(|path| Path::new(path).is_dir().then(|| path.to_owned()));
    let dir = cwd.or(home)?;

    let mut wide = Vec::new();
    if Path::new(&dir).is_relative() {
        if let Ok(current_dir) = env::current_dir() {
            wide.extend(current_dir.join(&dir).as_os_str().encode_wide());
        } else {
            wide.extend(dir.encode_wide());
        }
    } else {
        wide.extend(dir.encode_wide());
    }
    wide.push(0);
    Some(wide)
}

fn build_environment_block(cmd: &CommandBuilder) -> Vec<u16> {
    let mut block = Vec::new();
    for (key, value) in cmd.iter_full_env_as_str() {
        block.extend(OsStr::new(key).encode_wide());
        block.push(b'=' as u16);
        block.extend(OsStr::new(value).encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

fn build_cmdline(cmd: &CommandBuilder) -> anyhow::Result<(Vec<u16>, Vec<u16>)> {
    let exe_os: OsString = if cmd.is_default_prog() {
        cmd.get_env("ComSpec")
            .unwrap_or(OsStr::new("cmd.exe"))
            .to_os_string()
    } else {
        let argv = cmd.get_argv();
        let Some(first) = argv.first() else {
            bail!("missing program name");
        };
        search_path(cmd, first)
    };

    let mut cmdline = Vec::new();
    append_quoted(&exe_os, &mut cmdline);
    for arg in cmd.get_argv().iter().skip(1) {
        cmdline.push(' ' as u16);
        ensure!(
            !arg.encode_wide().any(|c| c == 0),
            "invalid encoding for command line argument {arg:?}"
        );
        append_quoted(arg, &mut cmdline);
    }
    cmdline.push(0);

    let mut exe: Vec<u16> = exe_os.encode_wide().collect();
    exe.push(0);

    Ok((exe, cmdline))
}

fn search_path(cmd: &CommandBuilder, exe: &OsStr) -> OsString {
    if let Some(path) = cmd.get_env("PATH") {
        let extensions = cmd.get_env("PATHEXT").unwrap_or(OsStr::new(".EXE"));
        for path in env::split_paths(path) {
            let candidate = path.join(exe);
            if candidate.exists() {
                return candidate.into_os_string();
            }

            for ext in env::split_paths(extensions) {
                let ext = ext.to_str().unwrap_or("");
                let path = path
                    .join(exe)
                    .with_extension(ext.strip_prefix('.').unwrap_or(ext));
                if path.exists() {
                    return path.into_os_string();
                }
            }
        }
    }

    exe.to_os_string()
}

fn append_quoted(arg: &OsStr, cmdline: &mut Vec<u16>) {
    if !arg.is_empty()
        && !arg.encode_wide().any(|c| {
            c == ' ' as u16
                || c == '\t' as u16
                || c == '\n' as u16
                || c == '\x0b' as u16
                || c == '"' as u16
        })
    {
        cmdline.extend(arg.encode_wide());
        return;
    }
    cmdline.push('"' as u16);

    let arg: Vec<_> = arg.encode_wide().collect();
    let mut i = 0;
    while i < arg.len() {
        let mut num_backslashes = 0;
        while i < arg.len() && arg[i] == '\\' as u16 {
            i += 1;
            num_backslashes += 1;
        }

        if i == arg.len() {
            for _ in 0..num_backslashes * 2 {
                cmdline.push('\\' as u16);
            }
            break;
        } else if arg[i] == b'"' as u16 {
            for _ in 0..num_backslashes * 2 + 1 {
                cmdline.push('\\' as u16);
            }
            cmdline.push(arg[i]);
        } else {
            for _ in 0..num_backslashes {
                cmdline.push('\\' as u16);
            }
            cmdline.push(arg[i]);
        }
        i += 1;
    }
    cmdline.push('"' as u16);
}
