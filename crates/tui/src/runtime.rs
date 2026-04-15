use super::*;

impl TuiApp {
    /// Runs the interactive UI until the user exits.
    pub(crate) async fn run(config: InteractiveTuiConfig) -> Result<AppExit> {
        // Spawn the worker first.
        let worker = QueryWorkerHandle::spawn(QueryWorkerConfig {
            model: config.model.clone(),
            cwd: config.cwd.clone(),
            server_env: config.server_env,
            server_log_level: config.server_log_level,
            thinking_selection: None,
        });

        let mut app = Self {
            model: config.model,
            provider: config.provider,
            cwd: config.cwd,
            transcript: Vec::new(),
            input: InputBuffer::new(),
            status_message: "Ready".to_string(),
            busy: false,
            spinner_index: 0,
            scroll: 0,
            follow_output: true,
            turn_count: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            slash_selection: 0,
            aux_panel: None,
            pending_status_index: None,
            pending_assistant_index: None,
            pending_reasoning_index: None,
            pending_tool_items: std::collections::HashMap::new(),
            thinking_selection: None,
            worker,
            model_catalog: config.model_catalog,
            saved_models: config.saved_models,
            show_model_onboarding: config.show_model_onboarding,
            onboarding_announced: false,
            onboarding_custom_model_pending: false,
            onboarding_prompt: None,
            onboarding_prompt_history: Vec::new(),
            onboarding_base_url_pending: false,
            onboarding_api_key_pending: false,
            onboarding_selected_model: None,
            onboarding_selected_model_is_custom: false,
            onboarding_selected_base_url: None,
            onboarding_selected_api_key: None,
            aux_panel_selection: 0,
            last_ctrl_c_at: None,
            paste_burst: PasteBurst::default(),
            should_quit: false,
            inline_mode: false,
            terminal_width: 80,
            inline_assistant_stream_open: false,
            inline_assistant_pending_line: String::new(),
            inline_assistant_header_emitted: false,
            pending_inline_history: Vec::new(),
        };

        if app.show_model_onboarding {
            app.show_onboarding_model_panel();
            app.onboarding_prompt = None;
            app.status_message.clear();
        }

        let mut terminal = ManagedTerminal::new(config.terminal_mode)?;
        app.inline_mode = !terminal.uses_alternate_screen();
        app.terminal_width = terminal.area().width;
        if app.inline_mode {
            terminal.insert_history_block(&crate::transcript::format_startup_banner(
                &app.model,
                &app.cwd,
                env!("CARGO_PKG_VERSION"),
                app.terminal_width,
            ))?;
        }
        let mut event_stream = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(80));
        let mut needs_redraw = true;

        loop {
            // Only repaint after a state change; this keeps the UI responsive and
            // avoids unnecessary full-screen redraws.
            if needs_redraw {
                if app.inline_mode {
                    terminal.set_inline_viewport_height(render::inline_viewport_height(
                        &app,
                        terminal.area().width,
                    ))?;
                    terminal.flush_pending_inline_history(&mut app.pending_inline_history)?;
                }
                terminal
                    .terminal_mut()
                    .draw(|frame| render::draw(frame, &app, app.inline_mode))?;
                needs_redraw = false;
            }

            if app.should_quit {
                break;
            }

            tokio::select! {
                maybe_event = event_stream.next() => {
                    match maybe_event {
                        Some(Ok(event)) => {
                            // Any terminal input can affect composer state, scrolling,
                            // or selection state, so accepted input invalidates the frame.
                            if let Event::Resize(width, _) = event {
                                app.terminal_width = width;
                            } else {
                                app.terminal_width = terminal.area().width;
                            }
                            app.handle_terminal_event(event, terminal.area())?;
                            needs_redraw = true;
                        }
                        Some(Err(error)) => {
                            app.push_item(
                                TranscriptItemKind::Error,
                                "Terminal error",
                                error.to_string(),
                            );
                            app.status_message = "Terminal input error".to_string();
                            needs_redraw = true;
                        }
                        None => break,
                    }
                }
                maybe_event = app.worker.event_rx.recv() => {
                    match maybe_event {
                        Some(event) => {
                            // Worker events are the source of transcript and session updates.
                            app.handle_worker_event(event);
                            needs_redraw = true;
                        }
                        None => {
                            app.status_message = "Background worker stopped".to_string();
                            break;
                        }
                    }
                }
                _ = tick.tick() => {
                    // The tick drives spinner animation, delayed fold transitions,
                    // and buffered paste flushes that are waiting on idle time.
                    let mut redraw = app.advance_transcript_folds(Instant::now());
                    if app.busy {
                        app.spinner_index = app.spinner_index.wrapping_add(1);
                        redraw = true;
                    }
                    if app.flush_pending_paste_burst(false) {
                        redraw = true;
                    }
                    if redraw {
                        needs_redraw = true;
                    }
                }
            }
        }

        app.worker.shutdown().await?;
        Ok(AppExit {
            turn_count: app.turn_count,
            total_input_tokens: app.total_input_tokens,
            total_output_tokens: app.total_output_tokens,
        })
    }

    pub(crate) fn transcript_area(&self, full_area: Rect) -> Rect {
        let content_area = render::centered_content_area(full_area);
        let composer_height = render::composer_height(self, content_area);
        let transcript_height = render::transcript_height(self, content_area);
        let [transcript_area, _, _, _] = Layout::vertical([
            Constraint::Length(transcript_height),
            Constraint::Length(1),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .areas(content_area);
        transcript_area
    }

    pub(crate) fn handle_terminal_event(
        &mut self,
        event: Event,
        terminal_area: Rect,
    ) -> Result<()> {
        match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                // Flush buffered paste text before any navigation or command key so
                // mixed keyboard and paste input stays in the expected order.
                if !matches!(key.code, KeyCode::Char(_) | KeyCode::Enter) {
                    self.flush_pending_paste_burst(true);
                }
                self.handle_key(key, terminal_area)
            }
            Event::Paste(text) => {
                self.flush_pending_paste_burst(true);
                self.input.insert_str(&text);
                self.reset_slash_selection();
                self.aux_panel = None;
                self.aux_panel_selection = 0;
            }
            Event::Resize(_, _) => {}
            Event::Mouse(mouse) => {
                if self.inline_mode {
                    return Ok(());
                }
                self.flush_pending_paste_burst(true);
                use crossterm::event::MouseEventKind;
                match mouse.kind {
                    MouseEventKind::ScrollDown => {
                        if self.follow_output {
                            self.scroll =
                                render::get_max_scroll(self, self.transcript_area(terminal_area));
                            self.follow_output = false;
                        }
                        self.scroll = self.scroll.saturating_add(1);
                    }
                    MouseEventKind::ScrollUp => {
                        if self.follow_output {
                            self.scroll =
                                render::get_max_scroll(self, self.transcript_area(terminal_area));
                            self.follow_output = false;
                        }
                        self.scroll = self.scroll.saturating_sub(1);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent, terminal_area: Rect) {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.handle_ctrl_c();
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.transcript.clear();
                self.pending_status_index = None;
                self.pending_assistant_index = None;
                self.status_message = "Transcript cleared".to_string();
                if self.inline_mode {
                    self.close_inline_assistant_stream();
                    self.pending_inline_history
                        .push("\n^L clear screen\n".to_string());
                    print!("\x1b[2J\x1b[H");
                    let mut stdout = std::io::stdout();
                    let _ = std::io::Write::flush(&mut stdout);
                }
            }
            KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.flush_pending_paste_burst(true);
                self.input.insert_char('\n');
            }
            KeyCode::Enter if !self.busy => {
                // Enter has three roles depending on current state:
                // accept a pasted multiline burst, execute a slash command, or submit.
                if self.paste_burst.push_newline(Instant::now()) {
                    return;
                }
                self.flush_pending_paste_burst(true);
                if self.has_slash_suggestions() && self.try_apply_slash_suggestion() {
                    let prompt = self.input.take();
                    if let Err(error) = self.handle_submission(prompt) {
                        self.push_item(
                            TranscriptItemKind::Error,
                            "Submit failed",
                            error.to_string(),
                        );
                        self.status_message = "Failed to submit prompt".to_string();
                    }
                    return;
                }
                if self.try_accept_aux_panel_selection() {
                    return;
                }
                let prompt = self.input.take();
                if let Err(error) = self.handle_submission(prompt) {
                    self.push_item(
                        TranscriptItemKind::Error,
                        "Submit failed",
                        error.to_string(),
                    );
                    self.status_message = "Failed to submit prompt".to_string();
                }
            }
            KeyCode::Backspace if self.has_selectable_aux_panel() && self.input.is_blank() => {}
            KeyCode::Backspace => {
                self.flush_pending_paste_burst(true);
                self.input.backspace();
                self.reset_slash_selection();
                self.aux_panel = None;
                self.aux_panel_selection = 0;
            }
            KeyCode::Delete if self.has_selectable_aux_panel() && self.input.is_blank() => {}
            KeyCode::Delete => {
                self.flush_pending_paste_burst(true);
                self.input.delete();
                self.reset_slash_selection();
                self.aux_panel = None;
                self.aux_panel_selection = 0;
            }
            KeyCode::Tab if self.try_apply_slash_suggestion() => {}
            KeyCode::Left => {
                self.flush_pending_paste_burst(true);
                self.input.move_left();
            }
            KeyCode::Right => {
                self.flush_pending_paste_burst(true);
                self.input.move_right();
            }
            KeyCode::Home => {
                self.flush_pending_paste_burst(true);
                self.input.move_home();
                self.scroll = 0;
                self.follow_output = false;
            }
            KeyCode::End => {
                self.flush_pending_paste_burst(true);
                self.input.move_end();
                self.follow_output = true;
            }
            KeyCode::Up => {
                if self.has_selectable_aux_panel() {
                    self.move_aux_panel_selection(-1);
                } else if self.has_slash_suggestions() {
                    self.move_slash_selection(-1);
                } else if !self.inline_mode {
                    if self.follow_output {
                        self.scroll =
                            render::get_max_scroll(self, self.transcript_area(terminal_area));
                        self.follow_output = false;
                    }
                    self.scroll = self.scroll.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if self.has_selectable_aux_panel() {
                    self.move_aux_panel_selection(1);
                } else if self.has_slash_suggestions() {
                    self.move_slash_selection(1);
                } else if !self.inline_mode {
                    if self.follow_output {
                        self.scroll =
                            render::get_max_scroll(self, self.transcript_area(terminal_area));
                        self.follow_output = false;
                    }
                    self.scroll = self.scroll.saturating_add(1);
                }
            }
            KeyCode::PageUp => {
                if !self.inline_mode {
                    if self.follow_output {
                        self.scroll =
                            render::get_max_scroll(self, self.transcript_area(terminal_area));
                        self.follow_output = false;
                    }
                    self.scroll = self.scroll.saturating_sub(10);
                }
            }
            KeyCode::PageDown => {
                if !self.inline_mode {
                    if self.follow_output {
                        self.scroll =
                            render::get_max_scroll(self, self.transcript_area(terminal_area));
                        self.follow_output = false;
                    }
                    self.scroll = self.scroll.saturating_add(10);
                }
            }
            KeyCode::Esc => {
                self.flush_pending_paste_burst(true);
                if self.has_slash_suggestions() {
                    self.dismiss_slash_popup();
                    self.dismiss_aux_panel();
                } else if !self.handle_escape() {
                    self.input.clear();
                    self.reset_slash_selection();
                    self.dismiss_aux_panel();
                }
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.is_onboarding_model_picker_open()
                    && self.input.is_blank() =>
            {
                if matches!(ch, 'c' | 'C') {
                    self.begin_custom_model_onboarding();
                }
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.paste_burst.push_char(ch, Instant::now()) {
                    return;
                }
                self.input.insert_char(ch);
                self.reset_slash_selection();
                self.dismiss_aux_panel();
            }
            _ => {}
        }
    }

    pub(crate) fn flush_pending_paste_burst(&mut self, force: bool) -> bool {
        let Some(text) = self.paste_burst.take_if_due(Instant::now(), force) else {
            return false;
        };
        // Insert the paste as one batch so a terminal paste behaves like a single
        // editing action instead of a sequence of character events.
        self.input.insert_str(&text);
        self.reset_slash_selection();
        self.aux_panel = None;
        self.aux_panel_selection = 0;
        true
    }

    pub(crate) fn handle_ctrl_c(&mut self) {
        const EXIT_CONFIRM_WINDOW: Duration = Duration::from_secs(2);

        let now = Instant::now();
        // The first Ctrl+C interrupts a running turn or arms exit confirmation.
        // A second press within the window exits the app.
        if self
            .last_ctrl_c_at
            .is_some_and(|previous| now.duration_since(previous) <= EXIT_CONFIRM_WINDOW)
        {
            self.should_quit = true;
            self.status_message = "Exiting".to_string();
            return;
        }

        self.last_ctrl_c_at = Some(now);
        if self.busy {
            if let Err(error) = self.worker.interrupt_turn() {
                self.push_item(
                    TranscriptItemKind::Error,
                    "Interrupt failed",
                    error.to_string(),
                );
                self.status_message = "Failed to interrupt active turn".to_string();
                return;
            }
            self.status_message =
                "Interrupt requested. Press Ctrl+C again within 2s to exit.".to_string();
        } else {
            self.status_message = "Press Ctrl+C again within 2s to exit.".to_string();
        }
    }

    pub(crate) fn handle_submission(&mut self, prompt: String) -> Result<()> {
        // Onboarding states consume input locally; only normal prompts reach the worker.
        if self.onboarding_custom_model_pending {
            let model = prompt.trim();
            if model.is_empty() {
                self.onboarding_prompt = Some("model name".to_string());
                return Ok(());
            }

            self.onboarding_custom_model_pending = false;
            self.onboarding_selected_model = Some(model.to_string());
            self.onboarding_selected_model_is_custom = true;
            self.onboarding_base_url_pending = true;
            self.aux_panel = None;
            self.aux_panel_selection = 0;
            self.input.clear();
            self.onboarding_prompt = Some("base url".to_string());
            self.status_message.clear();
            return Ok(());
        }

        if self.onboarding_base_url_pending {
            let base_url = prompt.trim();
            if !base_url.is_empty()
                && !(base_url.starts_with("http://") || base_url.starts_with("https://"))
            {
                self.status_message = "Base URL must start with http:// or https://".to_string();
                self.onboarding_prompt = Some("base url".to_string());
                return Ok(());
            }
            self.onboarding_base_url_pending = false;
            self.onboarding_api_key_pending = true;
            self.onboarding_selected_base_url = if base_url.is_empty() {
                None
            } else {
                Some(base_url.to_string())
            };
            self.onboarding_prompt_history.push(format!(
                "base url> {}",
                self.onboarding_selected_base_url.as_deref().unwrap_or("")
            ));
            if let Some(model) = self.onboarding_selected_model.clone() {
                self.push_item(
                    TranscriptItemKind::System,
                    "Onboarding",
                    format!(
                        "base url> {}",
                        self.onboarding_selected_base_url
                            .as_deref()
                            .unwrap_or("(empty)")
                    ),
                );
                self.status_message = format!("Base URL saved for {model}");
            }
            self.input.clear();
            self.onboarding_prompt = Some("api key".to_string());
            return Ok(());
        }

        if self.onboarding_api_key_pending {
            let api_key = prompt.trim();
            self.onboarding_api_key_pending = false;
            self.onboarding_selected_api_key = if api_key.is_empty() {
                None
            } else {
                Some(api_key.to_string())
            };
            self.onboarding_prompt_history.push(format!(
                "api key> {}",
                self.onboarding_selected_api_key
                    .as_deref()
                    .map(super::worker_events::mask_secret)
                    .unwrap_or_else(String::new)
            ));
            let Some(model) = self.onboarding_selected_model.clone() else {
                anyhow::bail!("onboarding model selection was lost before validation");
            };
            let provider = self.onboarding_provider_for_model(&model);
            self.busy = true;
            self.status_message = "Validating provider connection".to_string();
            self.worker.validate_provider(
                provider,
                model,
                self.onboarding_selected_base_url.clone(),
                self.onboarding_selected_api_key.clone(),
            )?;
            return Ok(());
        }

        if prompt.trim_start().starts_with('/') {
            return self.handle_slash_command(prompt);
        }
        self.submit_prompt(prompt)
    }

    pub(crate) fn submit_prompt(&mut self, prompt: String) -> Result<()> {
        if self.input.is_blank() && prompt.trim().is_empty() {
            return Ok(());
        }

        self.close_inline_assistant_stream();
        self.push_item(TranscriptItemKind::User, "You", prompt.clone());
        self.pending_status_index =
            Some(self.push_item(TranscriptItemKind::System, "Thinking", ""));
        self.follow_output = true;
        self.busy = true;
        self.reset_slash_selection();
        self.aux_panel = None;
        self.aux_panel_selection = 0;
        self.pending_assistant_index = None;
        self.worker.submit_prompt(prompt)
    }
}
