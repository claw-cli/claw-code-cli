//! Interactive terminal UI.
//!
//! public entry point for launching the CLI TUI.
#![allow(dead_code)]
mod app;
pub(crate) mod app_command;
mod app_event;
mod app_event_sender;
mod bottom_pane;
mod chatwidget;
#[cfg(test)]
mod chatwidget_tests;
mod clipboard_copy;
mod clipboard_paste;
mod color;
mod custom_terminal;
#[cfg(test)]
mod custom_terminal_clear_tests;
mod diff_render;
mod events;
mod exec_cell;
mod exec_command;
mod get_git_diff;
mod history_cell;
mod host;
mod insert_history;
mod key_hint;
mod line_truncation;
mod live_wrap;
mod markdown;
pub mod markdown_render;
mod markdown_stream;
mod onboarding;
mod render;
mod shimmer;
mod slash_command;
mod state;
mod status_indicator_widget;
mod streaming;
mod style;
mod terminal_palette;
mod test_backend;
mod text_formatting;
mod theme;
mod tui;
mod ui_consts;
mod version;
mod worker;
mod wrapping;

pub use host::run_interactive_tui;

pub use app::AppExit;
pub use app::InitialTuiSession;
pub use app::InteractiveTuiConfig;
pub use events::SavedModelEntry;
