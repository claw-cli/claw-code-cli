use crate::events::TranscriptItem;
use crate::theme::Theme;
use std::collections::HashMap;

#[derive(Debug, Default)]
pub(crate) struct TuiState {
    pub(crate) title: String,
    pub(crate) status_message: String,
    pub(crate) input: String,
    pub(crate) transcript: Vec<TranscriptItem>,
    pub(crate) busy: bool,
    pub(crate) should_quit: bool,
    pub(crate) scroll: u16,
    pub(crate) follow_output: bool,
    pub(crate) pending_assistant_index: Option<usize>,
    pub(crate) pending_reasoning_index: Option<usize>,
    pub(crate) pending_tool_items: HashMap<String, usize>,
    pub(crate) active_theme: Theme,
}

impl TuiState {
    pub(crate) fn new_with_theme(theme: Theme) -> Self {
        Self {
            title: "claw v2".to_string(),
            status_message: "Ready".to_string(),
            follow_output: true,
            active_theme: theme,
            ..Default::default()
        }
    }
}
