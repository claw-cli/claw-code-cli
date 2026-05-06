use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::WidgetRef;

use super::popup_consts::MAX_POPUP_ROWS;
use super::scroll_state::ScrollState;
use super::selection_popup_common::GenericDisplayRow;
use super::selection_popup_common::render_rows;
use super::slash_commands;
use crate::render::Insets;
use crate::render::RectExt;
use crate::slash_command::SlashCommand;

const ALIAS_COMMANDS: &[SlashCommand] = &[];

/// A selectable item in the popup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommandItem {
    Builtin(SlashCommand),
}

pub(crate) struct CommandPopup {
    command_filter: String,
    builtins: Vec<(&'static str, SlashCommand)>,
    state: ScrollState,
    accent_color: Color,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CommandPopupFlags {
    pub(crate) collaboration_modes_enabled: bool,
    pub(crate) connectors_enabled: bool,
    pub(crate) plugins_command_enabled: bool,
    pub(crate) fast_command_enabled: bool,
    pub(crate) personality_command_enabled: bool,
    pub(crate) realtime_conversation_enabled: bool,
    pub(crate) audio_device_selection_enabled: bool,
    pub(crate) windows_degraded_sandbox_active: bool,
}

impl From<CommandPopupFlags> for slash_commands::BuiltinCommandFlags {
    fn from(value: CommandPopupFlags) -> Self {
        Self {
            collaboration_modes_enabled: value.collaboration_modes_enabled,
            connectors_enabled: value.connectors_enabled,
            plugins_command_enabled: value.plugins_command_enabled,
            fast_command_enabled: value.fast_command_enabled,
            personality_command_enabled: value.personality_command_enabled,
            realtime_conversation_enabled: value.realtime_conversation_enabled,
            audio_device_selection_enabled: value.audio_device_selection_enabled,
            allow_elevate_sandbox: value.windows_degraded_sandbox_active,
        }
    }
}

impl CommandPopup {
    pub(crate) fn new(flags: CommandPopupFlags, accent_color: Color) -> Self {
        // Keep built-in availability in sync with the composer.
        let builtins: Vec<(&'static str, SlashCommand)> =
            slash_commands::builtins_for_input(flags.into())
                .into_iter()
                .collect();
        Self {
            command_filter: String::new(),
            builtins,
            state: ScrollState::new(),
            accent_color,
        }
    }

    /// Update the filter string based on the current composer text. The text
    /// passed in is expected to start with a leading '/'. Everything after the
    /// *first* '/' on the *first* line becomes the active filter that is used
    /// to narrow down the list of available commands.
    pub(crate) fn on_composer_text_change(&mut self, text: String) {
        let first_line = text.lines().next().unwrap_or("");

        if let Some(stripped) = first_line.strip_prefix('/') {
            // Extract the *first* token (sequence of non-whitespace
            // characters) after the slash so that `/clear something` still
            // shows the help for `/clear`.
            let token = stripped.trim_start();
            let cmd_token = token.split_whitespace().next().unwrap_or("");

            // Update the filter keeping the original case (commands are all
            // lower-case for now but this may change in the future).
            self.command_filter = cmd_token.to_string();
        } else {
            // The composer no longer starts with '/'. Reset the filter so the
            // popup shows the *full* command list if it is still displayed
            // for some reason.
            self.command_filter.clear();
        }

        // Reset or clamp selected index based on new filtered list.
        let matches_len = self.filtered_items().len();
        self.state.clamp_selection(matches_len);
        self.state
            .ensure_visible(matches_len, MAX_POPUP_ROWS.min(matches_len));
    }

    /// Determine the preferred height of the popup for a given width.
    /// Accounts for wrapped descriptions so that long tooltips don't overflow.
    pub(crate) fn calculate_required_height(&self, width: u16) -> u16 {
        use super::selection_popup_common::measure_rows_height;
        let rows = self.rows_from_matches(self.filtered());

        measure_rows_height(&rows, &self.state, MAX_POPUP_ROWS, width)
    }

    /// Compute exact/prefix matches over built-in commands and user prompts,
    /// paired with optional highlight indices. Preserves the original
    /// presentation order for built-ins and prompts.
    fn filtered(&self) -> Vec<(CommandItem, Option<Vec<usize>>)> {
        let filter = self.command_filter.trim();
        let mut out: Vec<(CommandItem, Option<Vec<usize>>)> = Vec::new();
        if filter.is_empty() {
            for (_, cmd) in self.builtins.iter() {
                if ALIAS_COMMANDS.contains(cmd) {
                    continue;
                }
                out.push((CommandItem::Builtin(*cmd), None));
            }
            return out;
        }

        let filter_lower = filter.to_lowercase();
        let filter_chars = filter.chars().count();
        let mut exact: Vec<(CommandItem, Option<Vec<usize>>)> = Vec::new();
        let mut prefix: Vec<(CommandItem, Option<Vec<usize>>)> = Vec::new();
        let indices_for = |offset| Some((offset..offset + filter_chars).collect());

        let mut push_match =
            |item: CommandItem, display: &str, name: Option<&str>, name_offset: usize| {
                let display_lower = display.to_lowercase();
                let name_lower = name.map(str::to_lowercase);
                let display_exact = display_lower == filter_lower;
                let name_exact = name_lower.as_deref() == Some(filter_lower.as_str());
                if display_exact || name_exact {
                    let offset = if display_exact { 0 } else { name_offset };
                    exact.push((item, indices_for(offset)));
                    return;
                }
                let display_prefix = display_lower.starts_with(&filter_lower);
                let name_prefix = name_lower
                    .as_ref()
                    .is_some_and(|name| name.starts_with(&filter_lower));
                if display_prefix || name_prefix {
                    let offset = if display_prefix { 0 } else { name_offset };
                    prefix.push((item, indices_for(offset)));
                }
            };

        for (_, cmd) in self.builtins.iter() {
            push_match(CommandItem::Builtin(*cmd), cmd.command(), None, 0);
        }

        out.extend(exact);
        out.extend(prefix);
        out
    }

    fn filtered_items(&self) -> Vec<CommandItem> {
        self.filtered().into_iter().map(|(c, _)| c).collect()
    }

    fn rows_from_matches(
        &self,
        matches: Vec<(CommandItem, Option<Vec<usize>>)>,
    ) -> Vec<GenericDisplayRow> {
        matches
            .into_iter()
            .map(|(item, indices)| {
                let CommandItem::Builtin(cmd) = item;
                let name = format!("/{}", cmd.command());
                let description = cmd.description().to_string();
                GenericDisplayRow {
                    name,
                    name_prefix_spans: Vec::new(),
                    match_indices: indices.map(|v| v.into_iter().map(|i| i + 1).collect()),
                    display_shortcut: None,
                    description: Some(description),
                    category_tag: None,
                    wrap_indent: None,
                    is_disabled: false,
                    disabled_reason: None,
                }
            })
            .collect()
    }

    /// Move the selection cursor one step up.
    pub(crate) fn move_up(&mut self) {
        let len = self.filtered_items().len();
        self.state.move_up_wrap(len);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    /// Move the selection cursor one step down.
    pub(crate) fn move_down(&mut self) {
        let matches_len = self.filtered_items().len();
        self.state.move_down_wrap(matches_len);
        self.state
            .ensure_visible(matches_len, MAX_POPUP_ROWS.min(matches_len));
    }

    /// Return currently selected command, if any.
    pub(crate) fn selected_item(&self) -> Option<CommandItem> {
        let matches = self.filtered_items();
        self.state
            .selected_idx
            .and_then(|idx| matches.get(idx).copied())
    }
}

impl WidgetRef for CommandPopup {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let rows = self.rows_from_matches(self.filtered());
        render_rows(
            area.inset(Insets::tlbr(
                /*top*/ 0, /*left*/ 2, /*bottom*/ 0, /*right*/ 0,
            )),
            buf,
            &rows,
            &self.state,
            MAX_POPUP_ROWS,
            "no matches",
            self.accent_color,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn filter_returns_empty_for_unknown_prefix() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default(), Color::Cyan);
        popup.on_composer_text_change("/in".to_string());
        assert!(popup.filtered_items().is_empty());
    }

    #[test]
    fn exact_match_selects_model() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default(), Color::Cyan);
        popup.on_composer_text_change("/model".to_string());

        let selected = popup.selected_item();
        match selected {
            Some(CommandItem::Builtin(cmd)) => assert_eq!(cmd.command(), "model"),
            None => panic!("expected a selected command for exact match"),
        }
    }

    #[test]
    fn model_is_first_suggestion_for_mo() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default(), Color::Cyan);
        popup.on_composer_text_change("/mo".to_string());
        let matches = popup.filtered_items();
        match matches.first() {
            Some(CommandItem::Builtin(cmd)) => assert_eq!(cmd.command(), "model"),
            None => panic!("expected at least one match for '/mo'"),
        }
    }

    #[test]
    fn filtered_commands_keep_presentation_order_for_prefix() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default(), Color::Cyan);
        popup.on_composer_text_change("/m".to_string());

        let cmds: Vec<&str> = popup
            .filtered_items()
            .into_iter()
            .map(|item| match item {
                CommandItem::Builtin(cmd) => cmd.command(),
            })
            .collect();
        assert_eq!(cmds, vec!["model"]);
    }

    #[test]
    fn prefix_filter_limits_matches_for_ac() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default(), Color::Cyan);
        popup.on_composer_text_change("/ac".to_string());

        let cmds: Vec<&str> = popup
            .filtered_items()
            .into_iter()
            .map(|item| match item {
                CommandItem::Builtin(cmd) => cmd.command(),
            })
            .collect();
        assert!(
            !cmds.contains(&"compact"),
            "expected prefix search for '/ac' to exclude 'compact', got {cmds:?}"
        );
    }

    #[test]
    fn exit_is_visible_for_matching_prefix() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default(), Color::Cyan);
        popup.on_composer_text_change("/ex".to_string());

        match popup.selected_item() {
            Some(CommandItem::Builtin(cmd)) => assert_eq!(cmd.command(), "exit"),
            other => panic!("expected exit to be selected for exact match, got {other:?}"),
        }
    }

    #[test]
    fn popup_lists_only_supported_commands() {
        let mut popup = CommandPopup::new(CommandPopupFlags::default(), Color::Cyan);
        popup.on_composer_text_change("/".to_string());

        let cmds: Vec<&str> = popup
            .filtered_items()
            .into_iter()
            .map(|item| match item {
                CommandItem::Builtin(cmd) => cmd.command(),
            })
            .collect();
        assert_eq!(
            cmds,
            vec![
                "theme", "model", "compact", "thinking", "resume", "new", "status", "clear",
                "onboard", "diff", "btw", "exit",
            ]
        );
    }

    #[test]
    fn settings_command_hidden_when_audio_device_selection_is_disabled() {
        let mut popup = CommandPopup::new(
            CommandPopupFlags {
                collaboration_modes_enabled: false,
                connectors_enabled: false,
                plugins_command_enabled: false,
                fast_command_enabled: false,
                personality_command_enabled: true,
                realtime_conversation_enabled: true,
                audio_device_selection_enabled: false,
                windows_degraded_sandbox_active: false,
            },
            Color::Cyan,
        );
        popup.on_composer_text_change("/aud".to_string());

        let cmds: Vec<&str> = popup
            .filtered_items()
            .into_iter()
            .map(|item| match item {
                CommandItem::Builtin(cmd) => cmd.command(),
            })
            .collect();

        assert!(
            !cmds.contains(&"settings"),
            "expected '/settings' to be hidden when audio device selection is disabled, got {cmds:?}"
        );
    }

    #[test]
    fn debug_commands_are_hidden_from_popup() {
        let popup = CommandPopup::new(CommandPopupFlags::default(), Color::Cyan);
        let cmds: Vec<&str> = popup
            .filtered_items()
            .into_iter()
            .map(|item| match item {
                CommandItem::Builtin(cmd) => cmd.command(),
            })
            .collect();

        assert!(
            !cmds.iter().any(|name| name.starts_with("debug")),
            "expected no /debug* command in popup menu, got {cmds:?}"
        );
    }
}
