//! Interactive terminal UI.
//!
//! public entry point for launching the CLI TUI.

mod app;
mod events;
mod input;
mod onboarding;
mod paste_burst;
mod render;
mod slash;
mod terminal;
mod transcript;
mod worker;

pub use app::AppExit;
pub use app::InteractiveTuiConfig;
pub use app::run_interactive_tui;
pub use events::SavedModelEntry;
pub use terminal::TerminalMode;

#[cfg(test)]
mod plan_tests;

#[cfg(test)]
mod tests;
