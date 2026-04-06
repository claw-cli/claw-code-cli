use std::io::{self, Stdout};

use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

/// Shared terminal type used by the interactive UI.
pub(crate) type AppTerminal = Terminal<CrosstermBackend<Stdout>>;

/// Owns terminal mode changes for the lifetime of the interactive UI.
pub(crate) struct ManagedTerminal {
    /// The ratatui terminal backend used for rendering.
    terminal: AppTerminal,
}

impl ManagedTerminal {
    /// Enters raw mode and the alternate screen before constructing the backend.
    pub(crate) fn new() -> io::Result<Self> {
        let mut stdout = io::stdout();
        terminal::enable_raw_mode()?;
        execute!(stdout, EnterAlternateScreen, Hide)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }

    /// Returns a mutable reference to the underlying ratatui terminal.
    pub(crate) fn terminal_mut(&mut self) -> &mut AppTerminal {
        &mut self.terminal
    }

    /// Restores the terminal to normal mode.
    pub(crate) fn restore(&mut self) -> io::Result<()> {
        terminal::disable_raw_mode()?;
        execute!(self.terminal.backend_mut(), Show, LeaveAlternateScreen)?;
        self.terminal.show_cursor()?;
        Ok(())
    }
}

impl Drop for ManagedTerminal {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
