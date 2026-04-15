use super::*;
use crate::events::ThinkingListEntry;
use crate::onboarding::save_last_used_model;
use clawcr_core::{ModelCatalog, SessionId};
use clawcr_utils::find_clawcr_home;
use std::io::{BufRead, BufReader};
use std::time::{SystemTime, UNIX_EPOCH};

impl TuiApp {
    pub(crate) fn dismiss_aux_panel(&mut self) {
        self.aux_panel = None;
        self.aux_panel_selection = 0;
    }

    pub(crate) fn dismiss_slash_popup(&mut self) {
        self.input.clear();
        self.reset_slash_selection();
    }

    fn emit_inline_command_echo(&mut self, command: &str) {
        if self.inline_mode {
            self.pending_inline_history
                .push(crate::transcript::format_shell_command_echo(command));
        }
    }

    pub(crate) fn show_aux_panel(&mut self, title: impl Into<String>, body: impl Into<String>) {
        self.aux_panel = Some(AuxPanel {
            title: title.into(),
            content: AuxPanelContent::Text(body.into()),
        });
        self.aux_panel_selection = 0;
    }

    pub(crate) fn show_session_panel(&mut self, sessions: Vec<SessionListEntry>) {
        self.aux_panel_selection = sessions
            .iter()
            .position(|session| session.is_active)
            .unwrap_or(0);
        self.aux_panel = Some(AuxPanel {
            title: "Sessions".to_string(),
            content: AuxPanelContent::SessionList(sessions),
        });
    }

    pub(crate) fn show_model_switch_panel(&mut self) {
        let entries = self.model_switch_entries();
        self.aux_panel_selection = entries
            .iter()
            .position(|entry| entry.is_current)
            .unwrap_or(0);
        self.aux_panel = Some(AuxPanel {
            title: "Models".to_string(),
            content: AuxPanelContent::ModelList(entries),
        });
    }

    pub(crate) fn show_thinking_panel(&mut self) {
        let entries = self.thinking_entries();
        self.aux_panel_selection = entries
            .iter()
            .position(|entry| entry.is_current)
            .unwrap_or(0);
        self.aux_panel = Some(AuxPanel {
            title: "Thinking".to_string(),
            content: AuxPanelContent::ThinkingList(entries),
        });
    }

    #[cfg(test)]
    pub(crate) fn show_model_panel(&mut self) {
        self.show_onboarding_model_panel();
    }

    pub(crate) fn show_onboarding_model_panel(&mut self) {
        let entries = self.onboarding_model_picker_entries();
        self.aux_panel_selection = entries
            .iter()
            .position(|entry| entry.is_current)
            .unwrap_or(0);
        self.aux_panel = Some(AuxPanel {
            title: String::new(),
            content: AuxPanelContent::ModelList(entries),
        });
    }

    pub(crate) fn model_switch_entries(&self) -> Vec<ModelListEntry> {
        let mut entries = self
            .saved_models
            .iter()
            .map(|model| ModelListEntry {
                slug: model.model.clone(),
                display_name: model.model.clone(),
                provider: model.provider,
                description: model
                    .base_url
                    .as_ref()
                    .map(|base_url| format!("saved model from {base_url}")),
                is_current: model.model == self.model,
                is_builtin: false,
                is_custom_mode: false,
            })
            .collect::<Vec<_>>();

        if entries.is_empty() {
            entries.extend(
                self.model_catalog
                    .list_visible()
                    .iter()
                    .map(|model| ModelListEntry {
                        slug: model.slug.clone(),
                        display_name: model.display_name.clone(),
                        provider: model.provider_family(),
                        description: model.description.clone(),
                        is_current: model.slug == self.model,
                        is_builtin: true,
                        is_custom_mode: false,
                    }),
            );
        }

        if entries.is_empty() {
            entries.push(ModelListEntry {
                slug: self.model.clone(),
                display_name: self.model.clone(),
                provider: self.provider,
                description: Some("current model".to_string()),
                is_current: true,
                is_builtin: false,
                is_custom_mode: false,
            });
        }

        if !entries.iter().any(|entry| entry.is_current) {
            entries.insert(
                0,
                ModelListEntry {
                    slug: self.model.clone(),
                    display_name: self.model.clone(),
                    provider: self.provider,
                    description: Some("current model".to_string()),
                    is_current: true,
                    is_builtin: false,
                    is_custom_mode: false,
                },
            );
        }

        entries.push(ModelListEntry {
            slug: "__add_model__".to_string(),
            display_name: "Add model".to_string(),
            provider: self.provider,
            description: Some("Open onboarding to add another model".to_string()),
            is_current: false,
            is_builtin: false,
            is_custom_mode: true,
        });
        entries
    }

    pub(crate) fn onboarding_model_picker_entries(&self) -> Vec<ModelListEntry> {
        let mut entries = Vec::new();

        for model in self.model_catalog.list_visible() {
            entries.push(ModelListEntry {
                slug: model.slug.clone(),
                display_name: model.display_name.clone(),
                provider: model.provider_family(),
                description: model.description.clone(),
                is_current: model.slug == self.model,
                is_builtin: true,
                is_custom_mode: false,
            });
        }

        if !self.show_model_onboarding && !entries.iter().any(|entry| entry.slug == self.model) {
            entries.insert(
                0,
                ModelListEntry {
                    slug: self.model.clone(),
                    display_name: self.model.clone(),
                    provider: self.provider,
                    description: Some("current model".to_string()),
                    is_current: true,
                    is_builtin: false,
                    is_custom_mode: false,
                },
            );
        }

        if self.show_model_onboarding {
            entries.push(ModelListEntry {
                slug: "__custom__".to_string(),
                display_name: "Custom model".to_string(),
                provider: self.provider,
                description: Some("enter a model name manually".to_string()),
                is_current: false,
                is_builtin: false,
                is_custom_mode: true,
            });
        }

        if entries.is_empty() {
            entries.push(ModelListEntry {
                slug: self.model.clone(),
                display_name: self.model.clone(),
                provider: self.provider,
                description: Some("current model".to_string()),
                is_current: true,
                is_builtin: false,
                is_custom_mode: false,
            });
        }

        entries
    }

    pub(crate) fn set_model(&mut self, model: String) -> Result<()> {
        self.worker.set_model(model.clone())?;
        save_last_used_model(self.provider, &model)?;
        self.model = model;
        Ok(())
    }

    pub(crate) fn reconfigure_saved_model(
        &mut self,
        model: String,
        base_url: Option<String>,
        api_key: Option<String>,
    ) -> Result<()> {
        if base_url.is_none() && api_key.is_none() {
            self.set_model(model)
        } else {
            self.worker
                .reconfigure_provider(model.clone(), base_url, api_key)?;
            save_last_used_model(self.provider, &model)?;
            self.model = model;
            Ok(())
        }
    }

    pub(crate) fn saved_model_entry(&self, model: &str) -> Option<&SavedModelEntry> {
        self.saved_models.iter().find(|entry| entry.model == model)
    }

    pub(crate) fn onboarding_provider_for_model(
        &self,
        model: &str,
    ) -> clawcr_protocol::ProviderFamily {
        if let Some(entry) = self.saved_model_entry(model) {
            return entry.provider;
        }
        if let Some(entry) = self.model_catalog.get(model) {
            return entry.provider_family();
        }
        self.provider
    }

    pub(crate) fn handle_slash_command(&mut self, prompt: String) -> Result<()> {
        let trimmed = prompt.trim();
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let command = parts.next().unwrap_or_default();
        let argument = parts.next().map(str::trim).unwrap_or_default();

        // Slash commands update local UI immediately, and only call the worker when
        // the command needs server-side state to change.
        match command {
            "/exit" => {
                self.emit_inline_command_echo(trimmed);
                self.dismiss_aux_panel();
                self.dismiss_slash_popup();
                self.reset_slash_selection();
                self.busy = false;
                self.last_ctrl_c_at = None;
                self.status_message = "Exiting".to_string();
                self.should_quit = true;
                Ok(())
            }
            "/status" => {
                self.emit_inline_command_echo(trimmed);
                self.show_aux_panel(
                    "Status",
                    format!(
                        "turns: {}\nmodel: {}\ntokens: {} in / {} out\nbusy: {}",
                        self.turn_count,
                        self.model,
                        self.total_input_tokens,
                        self.total_output_tokens,
                        self.busy
                    ),
                );
                self.status_message = "Session status shown".to_string();
                Ok(())
            }
            "/onboard" => {
                self.emit_inline_command_echo(trimmed);
                self.start_onboarding();
                Ok(())
            }
            "/sessions" => {
                self.emit_inline_command_echo(trimmed);
                let sessions = local_session_entries().unwrap_or_default();
                if sessions.is_empty() {
                    self.show_aux_panel("Sessions", "No sessions found");
                } else {
                    self.show_session_panel(sessions);
                }
                self.status_message = "Listing sessions".to_string();
                self.worker.list_sessions()?;
                Ok(())
            }
            "/thinking" => {
                self.emit_inline_command_echo(trimmed);
                self.show_thinking_panel();
                self.status_message = "Thinking options shown".to_string();
                Ok(())
            }
            "/new" => {
                self.emit_inline_command_echo(trimmed);
                self.worker.start_new_session()?;
                self.aux_panel = None;
                self.aux_panel_selection = 0;
                self.status_message = "New session ready; send a prompt to start it".to_string();
                Ok(())
            }
            "/rename" => {
                self.emit_inline_command_echo(trimmed);
                if argument.is_empty() {
                    anyhow::bail!("usage: /rename <new title>");
                }
                self.worker.rename_session(argument.to_string())?;
                self.status_message = "Renaming current session".to_string();
                Ok(())
            }
            "/session" => {
                self.emit_inline_command_echo(trimmed);
                if argument.is_empty() || argument == "list" {
                    let sessions = local_session_entries().unwrap_or_default();
                    if sessions.is_empty() {
                        self.show_aux_panel("Sessions", "No sessions found");
                    } else {
                        self.show_session_panel(sessions);
                    }
                    self.status_message = "Listing sessions".to_string();
                    self.worker.list_sessions()?;
                    return Ok(());
                }

                let mut session_parts = argument.splitn(2, char::is_whitespace);
                let subcommand = session_parts.next().unwrap_or_default();
                let rest = session_parts.next().map(str::trim).unwrap_or_default();

                match subcommand {
                    "new" => {
                        self.worker.start_new_session()?;
                        self.aux_panel = None;
                        self.aux_panel_selection = 0;
                        self.status_message =
                            "New session ready; send a prompt to start it".to_string();
                        Ok(())
                    }
                    "rename" => {
                        if rest.is_empty() {
                            anyhow::bail!("usage: /session rename <new title>");
                        }
                        self.worker.rename_session(rest.to_string())?;
                        self.status_message = "Renaming current session".to_string();
                        Ok(())
                    }
                    "switch" => {
                        if rest.is_empty() {
                            anyhow::bail!("usage: /session switch <session_id>");
                        }
                        let session_id = rest.parse::<SessionId>().map_err(|error| {
                            anyhow::anyhow!("invalid session id `{rest}`: {error}")
                        })?;
                        self.worker.switch_session(session_id)?;
                        self.status_message = format!("Switching to session {rest}");
                        Ok(())
                    }
                    _ => {
                        let session_id = argument.parse::<SessionId>().map_err(|error| {
                            anyhow::anyhow!("invalid session command `{argument}`: {error}")
                        })?;
                        self.worker.switch_session(session_id)?;
                        self.status_message = format!("Switching to session {argument}");
                        Ok(())
                    }
                }
            }
            "/model" => {
                self.emit_inline_command_echo(trimmed);
                if argument.is_empty() {
                    self.show_model_switch_panel();
                    self.status_message = "Model switcher shown".to_string();
                    return Ok(());
                }

                if let Some(model) = self
                    .saved_models
                    .iter()
                    .find(|entry| entry.model == argument)
                    .cloned()
                {
                    self.reconfigure_saved_model(model.model, model.base_url, model.api_key)?;
                } else {
                    self.set_model(argument.to_string())?;
                }
                self.aux_panel = None;
                self.aux_panel_selection = 0;
                self.status_message = format!("Model set to {}", self.model);
                Ok(())
            }
            _ => self.submit_prompt(prompt),
        }
    }

    pub(crate) fn slash_suggestions(&self) -> Vec<SlashCommandSpec> {
        matching_slash_commands(self.input.text())
    }

    pub(crate) fn has_slash_suggestions(&self) -> bool {
        !self.slash_suggestions().is_empty()
    }

    pub(crate) fn has_selectable_aux_panel(&self) -> bool {
        matches!(
            self.aux_panel.as_ref().map(|panel| &panel.content),
            Some(AuxPanelContent::SessionList(_) | AuxPanelContent::ModelList(_))
                | Some(AuxPanelContent::ThinkingList(_))
        )
    }

    pub(crate) fn is_onboarding_model_picker_open(&self) -> bool {
        self.show_model_onboarding
            && matches!(
                self.aux_panel.as_ref().map(|panel| &panel.content),
                Some(AuxPanelContent::ModelList(_))
            )
    }

    pub(crate) fn begin_custom_model_onboarding(&mut self) {
        self.aux_panel = None;
        self.aux_panel_selection = 0;
        self.onboarding_custom_model_pending = true;
        self.onboarding_base_url_pending = false;
        self.onboarding_api_key_pending = false;
        self.onboarding_selected_model = None;
        self.onboarding_selected_model_is_custom = true;
        self.onboarding_selected_base_url = None;
        self.onboarding_selected_api_key = None;
        self.onboarding_prompt = Some("model name".to_string());
        self.status_message.clear();
        self.input.clear();
    }

    pub(crate) fn begin_model_credentials_onboarding(&mut self, model: String) {
        self.aux_panel = None;
        self.aux_panel_selection = 0;
        self.onboarding_custom_model_pending = false;
        self.onboarding_base_url_pending = true;
        self.onboarding_api_key_pending = false;
        self.onboarding_selected_model = Some(model);
        self.onboarding_selected_model_is_custom = false;
        self.onboarding_selected_base_url = None;
        self.onboarding_selected_api_key = None;
        self.onboarding_prompt = Some("base url".to_string());
        self.status_message.clear();
        self.input.clear();
    }

    pub(crate) fn exit_onboarding(&mut self) {
        self.show_model_onboarding = false;
        self.aux_panel = None;
        self.aux_panel_selection = 0;
        self.onboarding_custom_model_pending = false;
        self.onboarding_prompt = None;
        self.onboarding_prompt_history.clear();
        self.onboarding_base_url_pending = false;
        self.onboarding_api_key_pending = false;
        self.onboarding_selected_model = None;
        self.onboarding_selected_model_is_custom = false;
        self.onboarding_selected_base_url = None;
        self.onboarding_selected_api_key = None;
        self.input.clear();
        self.status_message = "Onboarding dismissed".to_string();
    }

    pub(crate) fn start_onboarding(&mut self) {
        self.show_model_onboarding = true;
        self.aux_panel = None;
        self.aux_panel_selection = 0;
        self.onboarding_custom_model_pending = false;
        self.onboarding_prompt = None;
        self.onboarding_prompt_history.clear();
        self.onboarding_base_url_pending = false;
        self.onboarding_api_key_pending = false;
        self.onboarding_selected_model = None;
        self.onboarding_selected_model_is_custom = false;
        self.onboarding_selected_base_url = None;
        self.onboarding_selected_api_key = None;
        self.input.clear();
        self.show_onboarding_model_panel();
        self.status_message = "Onboarding started".to_string();
    }

    pub(crate) fn handle_escape(&mut self) -> bool {
        if self.onboarding_api_key_pending {
            self.onboarding_api_key_pending = false;
            self.onboarding_prompt = Some("base url".to_string());
            self.input.clear();
            return true;
        }
        if self.onboarding_base_url_pending {
            self.onboarding_base_url_pending = false;
            self.onboarding_selected_base_url = None;
            self.input.clear();
            if self.onboarding_selected_model_is_custom {
                self.onboarding_custom_model_pending = true;
                self.onboarding_prompt = Some("model name".to_string());
            } else {
                self.onboarding_prompt = None;
                self.show_onboarding_model_panel();
            }
            return true;
        }
        if self.onboarding_custom_model_pending {
            self.onboarding_custom_model_pending = false;
            self.onboarding_selected_model = None;
            self.onboarding_selected_model_is_custom = false;
            self.onboarding_prompt = None;
            self.input.clear();
            self.show_onboarding_model_panel();
            return true;
        }
        if self.is_onboarding_model_picker_open() {
            self.exit_onboarding();
            return true;
        }
        false
    }

    pub(crate) fn reset_slash_selection(&mut self) {
        self.slash_selection = 0;
    }

    pub(crate) fn move_slash_selection(&mut self, delta: isize) {
        let suggestions = self.slash_suggestions();
        if suggestions.is_empty() {
            self.slash_selection = 0;
            return;
        }
        let len = suggestions.len() as isize;
        let next = (self.slash_selection as isize + delta).rem_euclid(len);
        self.slash_selection = next as usize;
    }

    pub(crate) fn try_apply_slash_suggestion(&mut self) -> bool {
        let suggestions = self.slash_suggestions();
        if suggestions.is_empty() {
            return false;
        }
        let selected = suggestions[self.slash_selection.min(suggestions.len() - 1)];
        self.input.replace(selected.name);
        self.reset_slash_selection();
        true
    }

    pub(crate) fn move_aux_panel_selection(&mut self, delta: isize) {
        let len = self
            .aux_panel
            .as_ref()
            .map(|panel| match &panel.content {
                AuxPanelContent::SessionList(sessions) => sessions.len(),
                AuxPanelContent::ModelList(models) => models.len(),
                AuxPanelContent::ThinkingList(thinking) => thinking.len(),
                AuxPanelContent::Text(_) => 0,
            })
            .unwrap_or(0);
        if len == 0 {
            self.aux_panel_selection = 0;
            return;
        }

        let len = len as isize;
        let next = (self.aux_panel_selection as isize + delta).rem_euclid(len);
        self.aux_panel_selection = next as usize;
    }

    pub(crate) fn try_accept_aux_panel_selection(&mut self) -> bool {
        let Some(panel) = self.aux_panel.as_ref() else {
            return false;
        };
        if !self.input.is_blank() {
            return false;
        }

        // Session and model pickers are only actionable when the composer is empty,
        // which prevents accidental selection while the user is typing a prompt.
        match &panel.content {
            AuxPanelContent::SessionList(sessions) => {
                if sessions.is_empty() {
                    return false;
                }
                let selected =
                    sessions[self.aux_panel_selection.min(sessions.len() - 1)].session_id;
                if let Err(error) = self.worker.switch_session(selected) {
                    self.push_item(
                        TranscriptItemKind::Error,
                        "Switch failed",
                        error.to_string(),
                    );
                    self.status_message = "Failed to switch session".to_string();
                } else {
                    self.status_message = format!("Switching to session {selected}");
                }
                true
            }
            AuxPanelContent::ModelList(models) => {
                if models.is_empty() {
                    return false;
                }
                let selected = models[self.aux_panel_selection.min(models.len() - 1)].clone();
                if selected.is_custom_mode {
                    if self.show_model_onboarding {
                        self.begin_custom_model_onboarding();
                    } else {
                        self.start_onboarding();
                    }
                    return true;
                }
                // Only an exact saved model slug should bypass onboarding.
                // Having other entries in config.toml must not suppress the
                // connection setup for a newly selected model.
                if self.show_model_onboarding && self.saved_model_entry(&selected.slug).is_none() {
                    self.begin_model_credentials_onboarding(selected.slug.clone());
                    return true;
                }
                let Some(saved_model) = self.saved_model_entry(&selected.slug).cloned() else {
                    if let Err(error) = self.set_model(selected.slug.clone()) {
                        self.push_item(
                            TranscriptItemKind::Error,
                            "Model switch failed",
                            error.to_string(),
                        );
                        self.status_message = "Failed to switch model".to_string();
                    } else {
                        self.status_message = format!("Model set to {}", self.model);
                    }
                    self.aux_panel = None;
                    self.aux_panel_selection = 0;
                    return true;
                };
                self.aux_panel = None;
                self.aux_panel_selection = 0;
                self.onboarding_custom_model_pending = false;
                self.onboarding_selected_model_is_custom = false;
                self.onboarding_base_url_pending = false;
                self.onboarding_api_key_pending = false;
                self.onboarding_selected_model = None;
                self.onboarding_selected_base_url = None;
                self.onboarding_selected_api_key = None;
                self.onboarding_prompt = None;
                if let Err(error) = self.reconfigure_saved_model(
                    saved_model.model.clone(),
                    saved_model.base_url.clone(),
                    saved_model.api_key.clone(),
                ) {
                    self.push_item(
                        TranscriptItemKind::Error,
                        "Model switch failed",
                        error.to_string(),
                    );
                    self.status_message = "Failed to switch model".to_string();
                } else {
                    self.status_message = format!("Model set to {}", self.model);
                }
                true
            }
            AuxPanelContent::ThinkingList(thinking) => {
                if thinking.is_empty() {
                    return false;
                }
                let selected = thinking[self.aux_panel_selection.min(thinking.len() - 1)].clone();
                self.thinking_selection = Some(selected.value.clone());
                if let Err(error) = self.worker.set_thinking(self.thinking_selection.clone()) {
                    self.push_item(
                        TranscriptItemKind::Error,
                        "Thinking update failed",
                        error.to_string(),
                    );
                    self.status_message = "Failed to update thinking mode".to_string();
                } else {
                    self.status_message = format!("Thinking set to {}", selected.label);
                }
                self.aux_panel = None;
                self.aux_panel_selection = 0;
                true
            }
            AuxPanelContent::Text(_) => false,
        }
    }

    pub(crate) fn thinking_entries(&self) -> Vec<ThinkingListEntry> {
        let Some(model) = self.model_catalog.get(&self.model) else {
            return Vec::new();
        };
        let capability = model.effective_thinking_capability();
        let options = capability.options();
        let current = self
            .thinking_selection
            .as_deref()
            .map(str::to_lowercase)
            .unwrap_or_else(|| model.default_thinking_selection().unwrap_or_default());

        options
            .into_iter()
            .map(|option| ThinkingListEntry {
                is_current: option.value == current || option.label.to_lowercase() == current,
                label: option.label,
                description: option.description,
                value: option.value,
            })
            .collect()
    }

    pub(crate) fn finish_onboarding_selection(&mut self) -> Result<()> {
        let Some(model) = self.onboarding_selected_model.take() else {
            return Ok(());
        };
        let base_url = self.onboarding_selected_base_url.take();
        let api_key = self.onboarding_selected_api_key.take();

        // Persist the choice first, then reconfigure the worker so the live session
        // immediately reflects the onboarding selection.
        save_onboarding_config(
            self.provider,
            &model,
            base_url.as_deref(),
            api_key.as_deref(),
        )?;
        self.worker
            .reconfigure_provider(model.clone(), base_url, api_key)?;
        self.model = model.clone();
        self.aux_panel = None;
        self.aux_panel_selection = 0;
        self.onboarding_custom_model_pending = false;
        self.onboarding_selected_model_is_custom = false;
        self.onboarding_prompt = None;
        self.onboarding_prompt_history.clear();
        self.onboarding_base_url_pending = false;
        self.onboarding_api_key_pending = false;
        self.status_message = format!("Onboarding complete. Model set to {model}");
        if self.show_model_onboarding && !self.onboarding_announced {
            self.push_item(
                TranscriptItemKind::System,
                "Onboarding",
                "Onboarding complete. Run `clawcr onboard` any time to revisit builtin models.",
            );
            self.onboarding_announced = true;
            self.show_model_onboarding = false;
        }
        Ok(())
    }
}

fn local_session_entries() -> Result<Vec<SessionListEntry>> {
    let root = find_clawcr_home()?.join("sessions");
    let mut entries = Vec::new();
    if !root.exists() {
        return Ok(entries);
    }

    for path in walk_rollout_files(&root)? {
        if let Some(entry) = read_rollout_session_entry(&path)? {
            entries.push(entry);
        }
    }

    entries.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(entries)
}

fn walk_rollout_files(root: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            files.extend(walk_rollout_files(&path)?);
        } else if file_type.is_file()
            && path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
        {
            files.push(path);
        }
    }
    Ok(files)
}

fn read_rollout_session_entry(path: &std::path::Path) -> Result<Option<SessionListEntry>> {
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file).lines();
    let mut session_id = None;
    let mut title: Option<String> = None;
    let mut updated_at: Option<String> = None;

    for line in reader {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(line_value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };

        if let Some(meta) = line_value.get("SessionMeta") {
            if let Some(session) = meta.get("session") {
                session_id = session
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| value.parse::<SessionId>().ok());
                title = session
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned);
                updated_at = session
                    .get("updated_at")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned);
            }
            continue;
        }

        if let Some(updated) = line_value.get("SessionTitleUpdated") {
            title = updated
                .get("title")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned);
            updated_at = updated
                .get("timestamp")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned);
        }
    }

    let session_id = session_id.unwrap_or_else(SessionId::new);
    let title = title.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .map(|stem| stem.strip_prefix("rollout-").unwrap_or(stem).to_string())
            .unwrap_or_else(|| "(untitled)".to_string())
    });
    let updated_at = updated_at.unwrap_or_else(|| {
        path.metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .map(format_system_time)
            .unwrap_or_else(|| "(unknown)".to_string())
    });

    Ok(Some(SessionListEntry {
        session_id,
        title,
        updated_at,
        is_active: false,
    }))
}

fn format_system_time(time: SystemTime) -> String {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => format!("unix {}", duration.as_secs()),
        Err(_) => "(unknown)".to_string(),
    }
}
