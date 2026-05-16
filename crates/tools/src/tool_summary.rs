// TODO: Current tool sumary is a function with match, we need to refactor the summary as tool trait
//       and implement the summary as a method of a tool.

use std::path::Path;
use std::path::PathBuf;

fn make_relative(cwd: &Path, path: &str) -> String {
    let p = PathBuf::from(path);
    if !p.is_absolute() {
        return path.to_string();
    }
    // Try Path::strip_prefix first (handles platform semantics correctly)
    if let Ok(rel) = p.strip_prefix(cwd) {
        let lossy = rel.to_string_lossy();
        let s = lossy.replace('\\', "/");
        if s.is_empty() { ".".to_string() } else { s }
    } else {
        // Fallback: string-level comparison with forward-slash normalization
        let cwd_str = cwd.to_string_lossy().replace('\\', "/");
        let path_str = p.to_string_lossy().replace('\\', "/");
        if let Some(rest) = path_str.strip_prefix(&cwd_str) {
            let rel = rest.trim_start_matches('/');
            if rel.is_empty() {
                ".".to_string()
            } else {
                rel.to_string()
            }
        } else {
            path.to_string()
        }
    }
}

/// Compute a human-readable summary/title for a tool call, based on the tool
/// name and its input arguments. Paths are made relative to `cwd`.
pub fn tool_summary(name: &str, input: &serde_json::Value, cwd: &Path) -> String {
    match name {
        "bash" | "shell_command" => {
            let cmd = input
                .get("command")
                .or_else(|| input.get("cmd"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("{name}: {cmd}")
        }
        "exec_command" => {
            let cmd = input
                .get("cmd")
                .or_else(|| input.get("command"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("exec: {cmd}")
        }
        "read" => {
            let path = input["filePath"].as_str().unwrap_or("");
            let rel = make_relative(cwd, path);
            let mut s = format!("read: {rel}");
            let offset = input["offset"].as_u64();
            let limit = input["limit"].as_u64();
            match (offset, limit) {
                (Some(o), Some(l)) => s.push_str(&format!(" (offset:{o}, limit:{l})")),
                (Some(o), None) => s.push_str(&format!(" (offset:{o})")),
                (None, Some(l)) => s.push_str(&format!(" (limit:{l})")),
                (None, None) => {}
            }
            s
        }
        "write" => {
            let path = input["filePath"].as_str().unwrap_or("");
            let rel = make_relative(cwd, path);
            format!("write: {rel}")
        }
        "grep" => {
            let pattern = input["pattern"].as_str().unwrap_or("");
            let path = input["path"].as_str().unwrap_or(".");
            let rel = make_relative(cwd, path);
            format!("grep: '{pattern}' in {rel}")
        }
        "glob" => {
            let pattern = input["pattern"].as_str().unwrap_or("");
            let path = input["path"].as_str().unwrap_or(".");
            let rel = make_relative(cwd, path);
            format!("glob: {pattern} in {rel}")
        }
        "apply_patch" => "apply_patch".to_string(),
        "webfetch" => {
            let url = input["url"].as_str().unwrap_or("");
            format!("webfetch: {url}")
        }
        "websearch" => {
            let q = input["query"].as_str().unwrap_or("");
            format!("websearch: {q}")
        }
        "skill" => {
            let name = input["name"].as_str().unwrap_or("");
            format!("skill: {name}")
        }
        "question" => "question".to_string(),
        "update_plan" => "update_plan".to_string(),
        "task" => {
            let desc = input["description"].as_str().unwrap_or("");
            format!("task: {desc}")
        }
        "lsp" => {
            let path = input["filePath"].as_str().unwrap_or("");
            let rel = make_relative(cwd, path);
            let line = input["line"]
                .as_i64()
                .map(|l| l.to_string())
                .unwrap_or_else(|| "?".into());
            let col = input["character"]
                .as_i64()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".into());
            format!("lsp: {rel}:{line}:{col}")
        }
        "invalid" => "invalid".to_string(),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cwd() -> PathBuf {
        PathBuf::from("/project")
    }

    #[test]
    fn bash_summary() {
        let input = json!({"cmd": "echo hello"});
        let s = tool_summary("bash", &input, &cwd());
        assert_eq!(s, "bash: echo hello");
    }

    #[test]
    fn shell_command_summary() {
        let input = json!({"command": "npm run build"});
        let s = tool_summary("shell_command", &input, &cwd());
        assert_eq!(s, "shell_command: npm run build");
    }

    #[test]
    fn exec_command_summary() {
        let input = json!({"cmd": "make test"});
        let s = tool_summary("exec_command", &input, &cwd());
        assert_eq!(s, "exec: make test");
    }

    #[test]
    fn read_summary_offset_limit() {
        let input = json!({"filePath": "src/main.rs", "offset": 10, "limit": 50});
        let s = tool_summary("read", &input, &cwd());
        assert_eq!(s, "read: src/main.rs (offset:10, limit:50)");
    }

    #[test]
    fn read_summary_offset_only() {
        let input = json!({"filePath": "src/main.rs", "offset": 100});
        let s = tool_summary("read", &input, &cwd());
        assert_eq!(s, "read: src/main.rs (offset:100)");
    }

    #[test]
    fn read_summary_limit_only() {
        let input = json!({"filePath": "src/main.rs", "limit": 25});
        let s = tool_summary("read", &input, &cwd());
        assert_eq!(s, "read: src/main.rs (limit:25)");
    }

    #[test]
    fn read_summary_no_offset_limit() {
        let input = json!({"filePath": "src/main.rs"});
        let s = tool_summary("read", &input, &cwd());
        assert_eq!(s, "read: src/main.rs");
    }

    #[test]
    fn read_summary_absolute_path_kept_when_outside_cwd() {
        let cwd = PathBuf::from("/project");
        let input = json!({"filePath": "/tmp/foo.txt"});
        let s = tool_summary("read", &input, &cwd);
        assert_eq!(s, "read: /tmp/foo.txt");
    }

    #[test]
    fn write_summary() {
        let input = json!({"filePath": "src/lib.rs"});
        let s = tool_summary("write", &input, &cwd());
        assert_eq!(s, "write: src/lib.rs");
    }

    #[test]
    fn grep_summary() {
        let input = json!({"pattern": "TODO", "path": "src/"});
        let s = tool_summary("grep", &input, &cwd());
        assert_eq!(s, "grep: 'TODO' in src/");
    }

    #[test]
    fn glob_summary() {
        let input = json!({"pattern": "**/*.rs", "path": "src"});
        let s = tool_summary("glob", &input, &cwd());
        assert_eq!(s, "glob: **/*.rs in src");
    }

    #[test]
    fn lsp_summary() {
        let input = json!({"filePath": "src/lib.rs", "line": 10, "character": 5});
        let s = tool_summary("lsp", &input, &cwd());
        assert_eq!(s, "lsp: src/lib.rs:10:5");
    }

    #[test]
    fn make_relative_from_cwd() {
        let cwd = std::env::current_dir().unwrap_or_default();
        let sub = cwd.join("src").join("main.rs");
        let sub_str = sub.to_string_lossy().to_string();
        let rel = make_relative(&cwd, &sub_str);
        assert!(
            rel == "src/main.rs" || rel == "src\\main.rs",
            "make_relative('{sub_str}') = '{rel}', expected 'src/main.rs'"
        );
    }
}
