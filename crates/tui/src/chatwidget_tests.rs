use std::path::PathBuf;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use devo_protocol::ApprovalDecisionValue;
use devo_protocol::ApprovalScopeValue;
use devo_protocol::InputItem;
use devo_protocol::ItemId;
use devo_protocol::Model;
use devo_protocol::PermissionPreset;
use devo_protocol::ReasoningEffort;
use devo_protocol::SessionId;
use devo_protocol::ThinkingCapability;
use devo_protocol::TurnId;
use pretty_assertions::assert_eq;
use tokio::sync::mpsc;

use crate::app_command::AppCommand;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::chatwidget::ChatWidget;
use crate::chatwidget::ChatWidgetInit;
use crate::chatwidget::ThinkingListEntry;
use crate::chatwidget::TuiSessionState;
use crate::render::renderable::Renderable;
use crate::slash_command::built_in_slash_commands;
use crate::tui::frame_requester::FrameRequester;

fn widget_with_model(
    model: Model,
    cwd: PathBuf,
) -> (ChatWidget, mpsc::UnboundedReceiver<AppEvent>) {
    widget_with_model_and_thinking(model, cwd, None)
}

fn widget_with_model_and_thinking(
    model: Model,
    cwd: PathBuf,
    initial_thinking_selection: Option<String>,
) -> (ChatWidget, mpsc::UnboundedReceiver<AppEvent>) {
    let (app_event_tx, app_event_rx) = mpsc::unbounded_channel();
    let widget = ChatWidget::new_with_app_event(ChatWidgetInit {
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx: AppEventSender::new(app_event_tx),
        initial_session: TuiSessionState::new(cwd, Some(model)),
        initial_thinking_selection,
        initial_permission_preset: devo_protocol::PermissionPreset::Default,
        initial_user_message: None,
        enhanced_keys_supported: true,
        is_first_run: false,
        available_models: Vec::new(),
        saved_model_slugs: Vec::new(),
        show_model_onboarding: false,
        startup_tooltip_override: None,
        initial_theme_name: None,
    });
    (widget, app_event_rx)
}

fn onboarding_widget_with_model(
    model: Model,
    cwd: PathBuf,
) -> (ChatWidget, mpsc::UnboundedReceiver<AppEvent>) {
    let (app_event_tx, app_event_rx) = mpsc::unbounded_channel();
    let widget = ChatWidget::new_with_app_event(ChatWidgetInit {
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx: AppEventSender::new(app_event_tx),
        initial_session: TuiSessionState::new(cwd, Some(model)),
        initial_thinking_selection: None,
        initial_permission_preset: devo_protocol::PermissionPreset::Default,
        initial_user_message: None,
        enhanced_keys_supported: true,
        is_first_run: false,
        available_models: Vec::new(),
        saved_model_slugs: Vec::new(),
        show_model_onboarding: true,
        startup_tooltip_override: None,
        initial_theme_name: None,
    });
    (widget, app_event_rx)
}

fn rendered_rows(widget: &ChatWidget, width: u16, height: u16) -> Vec<String> {
    let area = ratatui::layout::Rect::new(0, 0, width, height);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    widget.render(area, &mut buf);
    (0..area.height)
        .map(|row| {
            (0..area.width)
                .map(|col| buf[(col, row)].symbol())
                .collect::<String>()
        })
        .collect()
}

fn scrollback_contains_text(lines: &[crate::history_cell::ScrollbackLine], text: &str) -> bool {
    lines.iter().any(|line| {
        line.line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
            .contains(text)
    })
}

fn find_row_index(rows: &[String], needle: &str) -> Option<usize> {
    rows.iter().position(|row| row.contains(needle))
}

fn scrollback_plain_lines(lines: &[crate::history_cell::ScrollbackLine]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

fn trim_trailing_blank_scrollback_lines(
    mut lines: Vec<crate::history_cell::ScrollbackLine>,
) -> Vec<crate::history_cell::ScrollbackLine> {
    while lines.last().is_some_and(|line| {
        line.line
            .spans
            .iter()
            .all(|span| span.content.trim().is_empty())
    }) {
        lines.pop();
    }
    lines
}

fn indices_containing(lines: &[String], needles: &[&str]) -> Vec<usize> {
    needles
        .iter()
        .map(|needle| {
            lines
                .iter()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("missing {needle} in:\n{}", lines.join("\n")))
        })
        .collect()
}

#[test]
fn resume_command_opens_loading_browser_immediately() {
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, PathBuf::from("."));

    widget.handle_app_event(AppEvent::Command(AppCommand::RunUserShellCommand {
        command: "session list".to_string(),
    }));

    assert!(widget.is_resume_browser_open());

    let rows = rendered_rows(&widget, 80, 12);
    assert!(
        rows.iter()
            .any(|row| row.contains("Loading saved sessions"))
    );
}

#[test]
fn approval_request_renders_bottom_pane_menu_and_accepts_once() {
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, mut app_event_rx) = widget_with_model(model, PathBuf::from("."));
    let session_id = SessionId::new();
    let turn_id = TurnId::new();

    widget.handle_worker_event(crate::events::WorkerEvent::ApprovalRequest {
        session_id,
        turn_id,
        approval_id: "approval-call-1".to_string(),
        action_summary: "write src/main.rs".to_string(),
        justification: "Tool execution requires approval.".to_string(),
        resource: Some("FileWrite".to_string()),
        available_scopes: vec!["once".to_string(), "session".to_string()],
        path: Some("src/main.rs".to_string()),
        host: None,
        target: None,
    });

    let scrollback = widget.drain_scrollback_lines(80);
    assert!(!scrollback_contains_text(
        &scrollback,
        "Permission required"
    ));

    let rendered = rendered_rows(&widget, 80, 16).join("\n");
    assert!(rendered.contains("Permission approval required"));
    assert!(rendered.contains("Approve once"));
    assert!(rendered.contains("Approve for session"));
    assert!(rendered.contains("Deny"));

    widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let event = app_event_rx.try_recv().expect("approval response event");
    assert_eq!(
        event,
        AppEvent::Command(AppCommand::ApprovalRespond {
            session_id,
            turn_id,
            approval_id: "approval-call-1".to_string(),
            decision: ApprovalDecisionValue::Approve,
            scope: ApprovalScopeValue::Once,
        })
    );
}

#[test]
fn approval_request_bottom_pane_menu_denies_with_n_shortcut() {
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, mut app_event_rx) = widget_with_model(model, PathBuf::from("."));
    let session_id = SessionId::new();
    let turn_id = TurnId::new();

    widget.handle_worker_event(crate::events::WorkerEvent::ApprovalRequest {
        session_id,
        turn_id,
        approval_id: "approval-call-2".to_string(),
        action_summary: "run shell command".to_string(),
        justification: "Tool execution requires approval.".to_string(),
        resource: Some("ShellExec".to_string()),
        available_scopes: vec!["once".to_string()],
        path: None,
        host: None,
        target: Some("cargo test".to_string()),
    });

    let rendered = rendered_rows(&widget, 80, 16).join("\n");
    assert!(rendered.contains("Permission approval required"));
    assert!(rendered.contains("run shell command"));
    assert!(rendered.contains("Deny"));

    widget.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));

    let event = app_event_rx.try_recv().expect("approval response event");
    assert_eq!(
        event,
        AppEvent::Command(AppCommand::ApprovalRespond {
            session_id,
            turn_id,
            approval_id: "approval-call-2".to_string(),
            decision: ApprovalDecisionValue::Deny,
            scope: ApprovalScopeValue::Once,
        })
    );
}

#[test]
fn submitted_prompt_requests_on_request_approval_policy() {
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, mut app_event_rx) = widget_with_model(model, PathBuf::from("."));

    widget.submit_text("please edit a file".to_string());

    let event = app_event_rx.try_recv().expect("user turn event");
    let AppEvent::Command(AppCommand::UserTurn {
        approval_policy, ..
    }) = event
    else {
        panic!("expected user turn command");
    };
    assert_eq!(approval_policy, Some("on-request".to_string()));
}

#[test]
fn permissions_command_opens_bottom_pane_picker_and_updates_default() {
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, mut app_event_rx) = widget_with_model(model, PathBuf::from("."));
    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: TurnId::new(),
    });

    widget.handle_app_event(AppEvent::RunSlashCommand {
        command: "permissions".to_string(),
    });

    let rendered = rendered_rows(&widget, 100, 18).join("\n");
    assert!(rendered.contains("Update Model Permissions"));
    assert!(rendered.contains("Read Only"));
    assert!(rendered.contains("Default (current)"));
    assert!(rendered.contains("Auto-review"));
    assert!(rendered.contains("Full Access"));

    widget.handle_key_event(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));

    let event = app_event_rx.try_recv().expect("permissions update event");
    assert_eq!(
        event,
        AppEvent::Command(AppCommand::UpdatePermissions {
            preset: devo_protocol::PermissionPreset::ReadOnly,
        })
    );
}

#[test]
fn permissions_command_marks_initial_project_preset_current() {
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let mut widget = ChatWidget::new_with_app_event(ChatWidgetInit {
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx: AppEventSender::new(app_event_tx),
        initial_session: TuiSessionState::new(PathBuf::from("."), Some(model)),
        initial_thinking_selection: None,
        initial_permission_preset: PermissionPreset::FullAccess,
        initial_user_message: None,
        enhanced_keys_supported: true,
        is_first_run: false,
        available_models: Vec::new(),
        saved_model_slugs: Vec::new(),
        show_model_onboarding: false,
        startup_tooltip_override: None,
        initial_theme_name: None,
    });

    widget.handle_app_event(AppEvent::RunSlashCommand {
        command: "permissions".to_string(),
    });

    let rendered = rendered_rows(&widget, 100, 18).join("\n");
    assert!(rendered.contains("Full Access (current)"));
}

#[test]
fn thinking_entries_are_generated_from_model_capability_options() {
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        thinking_capability: ThinkingCapability::Levels(vec![
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
        ]),
        default_reasoning_effort: Some(ReasoningEffort::Medium),
        ..Model::default()
    };
    let (widget, _app_event_rx) = widget_with_model(model, PathBuf::from("."));

    assert_eq!(
        widget.thinking_entries(),
        vec![
            ThinkingListEntry {
                is_current: false,
                label: "Low".to_string(),
                description: "Fastest, cheapest, least deliberative".to_string(),
                value: "low".to_string(),
            },
            ThinkingListEntry {
                is_current: true,
                label: "Medium".to_string(),
                description: "Balanced speed and deliberation".to_string(),
                value: "medium".to_string(),
            },
        ]
    );
}

#[test]
fn initial_thinking_selection_overrides_model_default() {
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        thinking_capability: ThinkingCapability::Levels(vec![
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
        ]),
        default_reasoning_effort: Some(ReasoningEffort::Medium),
        ..Model::default()
    };
    let (widget, _app_event_rx) =
        widget_with_model_and_thinking(model, PathBuf::from("."), Some("low".to_string()));

    assert_eq!(widget.current_thinking_selection(), Some("low"));
}

#[test]
fn slash_command_list_does_not_include_thinking() {
    let commands = built_in_slash_commands();
    assert!(!commands.iter().any(|(name, _)| *name == "thinking"));
}

#[test]
fn busy_widget_blocks_model_change_with_transcript_message() {
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, mut app_event_rx) = widget_with_model(model, PathBuf::from("."));

    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    widget.handle_paste("/model".to_string());
    widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(app_event_rx.try_recv().is_err());

    let scrollback = widget
        .drain_scrollback_lines(80)
        .into_iter()
        .map(|line| {
            line.line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(scrollback.contains("Cannot change model while generating"));
}

#[test]
fn toggle_with_levels_treats_enabled_as_default_effort_in_picker() {
    let model = Model {
        slug: "deepseek-v4".to_string(),
        display_name: "Deepseek V4".to_string(),
        thinking_capability: ThinkingCapability::ToggleWithLevels(vec![
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ]),
        default_reasoning_effort: Some(ReasoningEffort::High),
        ..Model::default()
    };
    let (widget, _app_event_rx) =
        widget_with_model_and_thinking(model, PathBuf::from("."), Some("enabled".to_string()));

    assert_eq!(
        widget.thinking_entries(),
        vec![
            ThinkingListEntry {
                is_current: false,
                label: "Off".to_string(),
                description: "Disable thinking for this turn".to_string(),
                value: "disabled".to_string(),
            },
            ThinkingListEntry {
                is_current: true,
                label: "High".to_string(),
                description: "More deliberate for harder tasks".to_string(),
                value: "high".to_string(),
            },
            ThinkingListEntry {
                is_current: false,
                label: "Max".to_string(),
                description: "Most deliberate, highest effort".to_string(),
                value: "max".to_string(),
            },
        ]
    );
}

#[test]
fn thinking_entries_show_off_and_levels_for_toggle_models_with_supported_levels() {
    let model = devo_core::ModelPreset {
        slug: "deepseek-v4".to_string(),
        display_name: "Deepseek V4".to_string(),
        thinking_capability: ThinkingCapability::Toggle,
        supported_reasoning_levels: vec![ReasoningEffort::High, ReasoningEffort::Max],
        default_reasoning_effort: None,
        ..devo_core::ModelPreset::default()
    }
    .into();
    let (widget, _app_event_rx) = widget_with_model(model, PathBuf::from("."));

    assert_eq!(
        widget.thinking_entries(),
        vec![
            ThinkingListEntry {
                is_current: false,
                label: "Off".to_string(),
                description: "Disable thinking for this turn".to_string(),
                value: "disabled".to_string(),
            },
            ThinkingListEntry {
                is_current: true,
                label: "High".to_string(),
                description: "More deliberate for harder tasks".to_string(),
                value: "high".to_string(),
            },
            ThinkingListEntry {
                is_current: false,
                label: "Max".to_string(),
                description: "Most deliberate, highest effort".to_string(),
                value: "max".to_string(),
            },
        ]
    );
}

#[test]
fn submit_text_emits_user_turn_with_model_and_thinking() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        thinking_capability: ThinkingCapability::Toggle,
        ..Model::default()
    };
    let (mut widget, mut app_event_rx) = widget_with_model(model, cwd.clone());

    widget.set_thinking_selection(Some("disabled".to_string()));
    widget.submit_text("hello".to_string());

    assert_eq!(
        app_event_rx.try_recv().expect("command event is emitted"),
        AppEvent::Command(AppCommand::UserTurn {
            input: vec![InputItem::Text {
                text: "hello".to_string(),
            }],
            cwd: Some(cwd),
            model: Some("test-model".to_string()),
            thinking: Some("disabled".to_string()),
            sandbox: None,
            approval_policy: Some("on-request".to_string()),
        })
    );
}

#[test]
fn typed_character_submits_after_paste_burst_flush() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, mut app_event_rx) = widget_with_model(model, cwd.clone());

    widget.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    std::thread::sleep(crate::bottom_pane::ChatComposer::recommended_paste_flush_delay());
    widget.pre_draw_tick();
    widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let emitted_command = std::iter::from_fn(|| app_event_rx.try_recv().ok())
        .find(|event| matches!(event, AppEvent::Command(_)))
        .expect("command event is emitted");
    assert_eq!(
        emitted_command,
        AppEvent::Command(AppCommand::UserTurn {
            input: vec![InputItem::Text {
                text: "a".to_string(),
            }],
            cwd: Some(cwd),
            model: Some("test-model".to_string()),
            thinking: None,
            sandbox: None,
            approval_policy: Some("on-request".to_string()),
        })
    );
}

#[test]
fn key_release_does_not_duplicate_text_input() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, mut app_event_rx) = widget_with_model(model, cwd.clone());

    widget.handle_key_event(KeyEvent {
        code: KeyCode::Char('a'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    });
    widget.handle_key_event(KeyEvent {
        code: KeyCode::Char('a'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Release,
        state: crossterm::event::KeyEventState::NONE,
    });
    std::thread::sleep(crate::bottom_pane::ChatComposer::recommended_paste_flush_delay());
    widget.pre_draw_tick();
    widget.handle_key_event(KeyEvent {
        code: KeyCode::Enter,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    });

    let emitted_command = std::iter::from_fn(|| app_event_rx.try_recv().ok())
        .find(|event| matches!(event, AppEvent::Command(_)))
        .expect("command event is emitted");
    assert_eq!(
        emitted_command,
        AppEvent::Command(AppCommand::UserTurn {
            input: vec![InputItem::Text {
                text: "a".to_string(),
            }],
            cwd: Some(cwd),
            model: Some("test-model".to_string()),
            thinking: None,
            sandbox: None,
            approval_policy: Some("on-request".to_string()),
        })
    );
}

#[test]
fn startup_header_mascot_animation_advances_on_pre_draw_tick() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);

    assert_eq!(widget.startup_header_mascot_frame_index(), 0);

    widget.force_startup_header_animation_due();
    widget.pre_draw_tick();

    assert_eq!(widget.startup_header_mascot_frame_index(), 1);
}

#[test]
fn onboarding_view_is_active_on_first_run() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (_widget, _app_event_rx) = onboarding_widget_with_model(model, cwd);
    // Onboarding view is pushed onto the view stack on first run.
    // The UI is now managed by the OnboardingView via the bottom pane view stack.
}

#[test]
fn onboarding_validation_succeeded_clears_active_state() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "anthropic-messages-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = onboarding_widget_with_model(model, cwd);

    // Simulate validation success from the worker.
    widget.handle_worker_event(crate::events::WorkerEvent::ProviderValidationSucceeded {
        reply_preview: "OK".to_string(),
    });

    // After validation, placeholder should be reset to default.
    assert_eq!(widget.placeholder_text(), "Ask Devo");
}

#[test]
fn streamed_lines_stay_in_live_viewport_until_turn_finishes() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model.clone(), cwd);

    let base_height = widget.desired_height(80);
    for index in 0..12 {
        widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(format!(
            "line {index}\n"
        )));
    }

    assert!(widget.desired_height(80) > base_height);

    let committed_before_finish = widget.drain_scrollback_lines(80);
    let committed_before_finish_text = committed_before_finish
        .iter()
        .flat_map(|line| line.line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(!committed_before_finish_text.contains("line 0"));
    assert!(!committed_before_finish_text.contains("line 11"));

    widget.handle_worker_event(crate::events::WorkerEvent::TurnFinished {
        stop_reason: "stop".to_string(),
        turn_count: 1,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        last_query_total_tokens: 0,
        last_query_input_tokens: 0,
        prompt_token_estimate: 0,
    });

    let committed_after_finish = widget.drain_scrollback_lines(80);
    let committed_after_finish_text = committed_after_finish
        .iter()
        .flat_map(|line| line.line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(committed_after_finish_text.contains("line 0"));
    assert!(committed_after_finish_text.contains("line 11"));
}

#[test]
fn committed_history_drains_to_scrollback_lines() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model.clone(), cwd.clone());

    let initial_lines = widget.drain_scrollback_lines(80);
    assert!(!initial_lines.is_empty());

    widget.handle_worker_event(crate::events::WorkerEvent::TurnFinished {
        stop_reason: "done".to_string(),
        turn_count: 1,
        total_input_tokens: 10,
        total_output_tokens: 20,
        total_cache_read_tokens: 0,
        last_query_total_tokens: 30,
        last_query_input_tokens: 10,
        prompt_token_estimate: 10,
    });

    let committed_lines = trim_trailing_blank_scrollback_lines(widget.drain_scrollback_lines(80));
    // TurnSummaryCell is now added on TurnFinished, so scrollback is non-empty.
    assert!(
        !committed_lines.is_empty(),
        "TurnSummaryCell should be committed"
    );
    assert!(
        committed_lines.iter().any(|line| {
            line.line
                .spans
                .iter()
                .any(|span| span.content.contains("▣"))
        }),
        "expected ▣ symbol in turn summary"
    );
}

#[test]
fn streamed_history_stays_empty_until_turn_finishes() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model.clone(), cwd.clone());

    let _ = widget.drain_scrollback_lines(80);
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(
        "first\nsecond\n".to_string(),
    ));

    let committed_lines = trim_trailing_blank_scrollback_lines(widget.drain_scrollback_lines(80));
    assert!(committed_lines.is_empty());
}

#[test]
fn batched_history_inserts_separator_and_trailing_blank_lines() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model.clone(), cwd.clone());

    let _ = widget.drain_scrollback_lines(80);
    widget.add_to_history(crate::history_cell::new_info_event(
        "first".to_string(),
        None,
    ));
    widget.add_to_history(crate::history_cell::new_info_event(
        "second".to_string(),
        None,
    ));

    let committed_lines = widget.drain_scrollback_lines(80);
    let blank_lines = committed_lines
        .iter()
        .filter(|line| {
            line.line
                .spans
                .iter()
                .all(|span| span.content.trim().is_empty())
        })
        .count();

    assert_eq!(
        2, blank_lines,
        "unexpected blank lines: {committed_lines:?}"
    );
}

#[test]
fn session_switch_restores_header_and_double_blank_line_before_user_input() {
    let initial_cwd = std::env::current_dir().expect("current directory is available");
    let resumed_cwd = initial_cwd.join("resumed");
    let model = Model {
        slug: "initial-model".to_string(),
        display_name: "Initial Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, initial_cwd);

    let _ = widget.drain_scrollback_lines(80);
    widget.add_to_history(crate::history_cell::new_info_event(
        "session 1 lingering line".to_string(),
        None,
    ));
    let _ = widget.drain_scrollback_lines(80);
    widget.handle_worker_event(crate::events::WorkerEvent::SessionSwitched {
        session_id: "session-1".to_string(),
        cwd: resumed_cwd.clone(),
        title: Some("Resumed".to_string()),
        model: Some("resumed-model".to_string()),
        thinking: None,
        reasoning_effort: None,
        total_input_tokens: 3,
        total_output_tokens: 5,
        total_cache_read_tokens: 0,
        last_query_total_tokens: 8,
        last_query_input_tokens: 3,
        prompt_token_estimate: 3,
        history_items: vec![
            crate::events::TranscriptItem::new(
                crate::events::TranscriptItemKind::User,
                String::new(),
                "hello".to_string(),
            ),
            crate::events::TranscriptItem::new(
                crate::events::TranscriptItemKind::Assistant,
                String::new(),
                "world".to_string(),
            ),
        ],
        loaded_item_count: 2,
        pending_texts: vec![],
    });

    let committed_lines = widget.drain_scrollback_lines(80);
    let committed_text = committed_lines
        .iter()
        .flat_map(|line| line.line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    let committed_rows = committed_lines
        .iter()
        .map(|line| {
            line.line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();

    // The header box is rendered only once on initial launch, not on session switch.
    assert_eq!(0, committed_text.matches("directory:").count());
    assert!(committed_text.contains("hello"));
    assert!(committed_text.contains("world"));
    assert!(!committed_text.contains("session 1 lingering line"));
    assert!(
        committed_rows
            .windows(3)
            .any(|window| window[0].trim_end() == "▌"
                && window[1].contains("hello")
                && window[2].trim_end() == "▌"),
        "expected blank line spacing with colored bar before restored user input: {committed_lines:?}"
    );
}

#[test]
fn turn_finished_does_not_add_completion_status_line_to_history() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model.clone(), cwd.clone());

    let _ = widget.drain_scrollback_lines(80);
    widget.handle_worker_event(crate::events::WorkerEvent::TurnFinished {
        stop_reason: "Completed".to_string(),
        turn_count: 1,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        last_query_total_tokens: 0,
        last_query_input_tokens: 0,
        prompt_token_estimate: 0,
    });

    let committed_lines = widget.drain_scrollback_lines(80);
    assert!(!committed_lines.iter().any(|line| {
        line.line
            .spans
            .iter()
            .any(|span| span.content.contains("Turn completed (Completed)"))
    }));
}

#[test]
fn completed_turn_summary_keeps_duration_for_text_turns() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);

    let _ = widget.drain_scrollback_lines(80);
    widget.force_task_elapsed_seconds(3);
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta("hello".to_string()));
    widget.handle_worker_event(crate::events::WorkerEvent::TurnFinished {
        stop_reason: "Completed".to_string(),
        turn_count: 1,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        last_query_total_tokens: 0,
        last_query_input_tokens: 0,
        prompt_token_estimate: 0,
    });

    let committed = widget
        .drain_scrollback_lines(80)
        .into_iter()
        .map(|line| {
            line.line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(committed.contains("▣"));
    assert!(committed.contains("Test Model"));
    assert!(committed.contains("3s"));
}

#[test]
fn active_response_renders_generating_status_without_devo_title() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);

    let _ = widget.drain_scrollback_lines(80);
    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta("hello".to_string()));

    let rendered = rendered_rows(&widget, 80, 12).join("\n");
    assert!(!rendered.contains("Devo -"));
}

#[test]
fn streaming_pending_ai_reply_respects_wrap_limit_before_finalize() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);
    widget.handle_app_event(AppEvent::ClearTranscript);
    let _ = widget.drain_scrollback_lines(80);

    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(
        "see https://example.test/path/abcdef12345 tail words".to_string(),
    ));

    let rendered = rendered_rows(&widget, 24, 12).join("\n");
    assert!(
        rendered.contains("tail words"),
        "expected pending streaming reply to wrap suffix words together, got:\n{rendered}"
    );
}

#[test]
fn active_assistant_markdown_does_not_double_wrap() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);
    let body = format!("{} betabet gamma", ["alpha"; 12].join(" "));

    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(body));

    let rendered = rendered_rows(&widget, 80, 12).join("\n");
    assert!(
        rendered.contains("betabet gamma"),
        "expected active assistant markdown to keep trailing words together, got:\n{rendered}"
    );
}

#[test]
fn active_assistant_multiline_text_has_no_extra_blank_rows() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);

    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(
        "Line1\nLine2\nLine3\n".to_string(),
    ));

    let rows = rendered_rows(&widget, 80, 12);
    let line1 = find_row_index(&rows, "Line1").expect("missing Line1");
    let line2 = find_row_index(&rows, "Line2").expect("missing Line2");
    let line3 = find_row_index(&rows, "Line3").expect("missing Line3");
    assert_eq!(line2, line1 + 1, "unexpected rows:\n{}", rows.join("\n"));
    assert_eq!(line3, line2 + 1, "unexpected rows:\n{}", rows.join("\n"));
}

#[test]
fn active_assistant_renders_resume_like_markdown_without_fragment_gaps() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);

    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(
        "## devo-cli -- Binary entry point that assembles all crates\n\n".to_string(),
    ));
    widget.pre_draw_tick();
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(
        "4 source files, produces the devo binary.\n\n".to_string(),
    ));
    widget.pre_draw_tick();
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(
        "Command dispatch (/crates/cli/src/main.rs)\n\n".to_string(),
    ));
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(
        "devo                 -> run_agent()            interactive TUI (default)\n".to_string(),
    ));

    let rows = rendered_rows(&widget, 180, 24);
    let indices = indices_containing(
        &rows,
        &[
            "devo-cli",
            "4 source files",
            "Command dispatch",
            "run_agent",
        ],
    );

    assert_eq!(
        indices
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .collect::<Vec<_>>(),
        vec![2, 2, 2],
        "expected active assistant markdown blocks to have one separator row, not doubled gaps:\n{}",
        rows.join("\n")
    );
}

#[test]
fn committed_assistant_markdown_does_not_double_wrap() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);
    let body = format!("{} betabet gamma", ["alpha"; 12].join(" "));

    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(body));
    widget.handle_worker_event(crate::events::WorkerEvent::TurnFinished {
        stop_reason: "Completed".to_string(),
        turn_count: 1,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        last_query_total_tokens: 0,
        last_query_input_tokens: 0,
        prompt_token_estimate: 0,
    });

    let committed = widget
        .drain_scrollback_lines(80)
        .into_iter()
        .map(|line| {
            line.line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        committed.contains("betabet gamma"),
        "expected committed assistant markdown to keep trailing words together, got:\n{committed}"
    );
}

#[test]
fn committed_assistant_multiline_text_has_no_extra_blank_rows() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);

    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(
        "Line1\nLine2\nLine3\n".to_string(),
    ));
    widget.handle_worker_event(crate::events::WorkerEvent::TurnFinished {
        stop_reason: "Completed".to_string(),
        turn_count: 1,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        last_query_total_tokens: 0,
        last_query_input_tokens: 0,
        prompt_token_estimate: 0,
    });

    let lines = scrollback_plain_lines(&trim_trailing_blank_scrollback_lines(
        widget.drain_scrollback_lines(80),
    ));
    let line1 = lines
        .iter()
        .position(|line| line.contains("Line1"))
        .unwrap();
    let line2 = lines
        .iter()
        .position(|line| line.contains("Line2"))
        .unwrap();
    let line3 = lines
        .iter()
        .position(|line| line.contains("Line3"))
        .unwrap();
    assert_eq!(line2, line1 + 1, "unexpected lines:\n{}", lines.join("\n"));
    assert_eq!(line3, line2 + 1, "unexpected lines:\n{}", lines.join("\n"));
}

#[test]
fn tool_call_start_and_finish_are_both_visible_in_history() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);
    let _ = widget.drain_scrollback_lines(80);

    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::ToolCall {
        tool_use_id: "tool-1".to_string(),
        summary: "powershell -NoProfile -Command Get-Date".to_string(),
    });

    let running = scrollback_plain_lines(&widget.drain_scrollback_lines(80)).join("\n");
    assert!(
        running.contains("Running powershell -NoProfile -Command Get-Date"),
        "expected running tool cell, got:\n{running}"
    );

    widget.handle_worker_event(crate::events::WorkerEvent::ToolResult {
        tool_use_id: "tool-1".to_string(),
        title: "powershell -NoProfile -Command Get-Date".to_string(),
        preview: "2026-05-09".to_string(),
        is_error: false,
        truncated: false,
    });

    let ran = scrollback_plain_lines(&widget.drain_scrollback_lines(80)).join("\n");
    assert!(
        ran.contains("Ran powershell -NoProfile -Command Get-Date"),
        "expected ran tool cell, got:\n{ran}"
    );
    assert!(
        ran.contains("2026-05-09"),
        "expected tool output, got:\n{ran}"
    );
}

#[test]
fn reasoning_text_commits_to_history_when_turn_finishes() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);

    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::ReasoningDelta(
        "thinking text\n".to_string(),
    ));

    let empty_scrollback = widget.drain_scrollback_lines(80);
    assert!(!scrollback_contains_text(
        &empty_scrollback,
        "thinking text"
    ));

    widget.handle_worker_event(crate::events::WorkerEvent::TurnFinished {
        stop_reason: "stop".to_string(),
        turn_count: 1,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        last_query_total_tokens: 0,
        last_query_input_tokens: 0,
        prompt_token_estimate: 0,
    });

    let scrollback = widget.drain_scrollback_lines(80);
    assert!(scrollback_contains_text(&scrollback, "thinking text"));
}

#[test]
fn restored_reasoning_text_is_visible_in_transcript() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd.clone());

    widget.handle_worker_event(crate::events::WorkerEvent::SessionSwitched {
        session_id: "session-1".to_string(),
        cwd,
        title: None,
        model: Some("test-model".to_string()),
        thinking: None,
        reasoning_effort: None,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        last_query_total_tokens: 0,
        last_query_input_tokens: 0,
        prompt_token_estimate: 0,
        history_items: vec![crate::events::TranscriptItem::new(
            crate::events::TranscriptItemKind::Reasoning,
            "",
            "thinking text",
        )],
        loaded_item_count: 1,
        pending_texts: vec![],
    });

    let scrollback = widget.drain_scrollback_lines(80);
    assert!(scrollback_contains_text(&scrollback, "thinking text"));
}

#[test]
fn reasoning_and_assistant_stream_in_separate_cells() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);

    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::ReasoningDelta(
        "thinking".to_string(),
    ));
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(
        "final answer line 1\nfinal answer line 2\n".to_string(),
    ));

    let before_rows = rendered_rows(&widget, 80, 16);
    let before = before_rows.join("\n");
    assert!(
        before.contains("thinking") && before.contains("final answer line 1"),
        "reasoning/text should both be visible while streaming:\n{before}"
    );
    let reasoning_row = find_row_index(&before_rows, "thinking").expect("missing reasoning row");
    let assistant_row =
        find_row_index(&before_rows, "final answer line 1").expect("missing assistant row");
    assert_eq!(
        assistant_row,
        reasoning_row + 2,
        "expected one blank row between live cells"
    );
    assert!(
        before_rows[reasoning_row + 1].trim().is_empty(),
        "expected blank separator row, got: {:?}",
        before_rows[reasoning_row + 1]
    );

    widget.pre_draw_tick();
    let committed_before_reasoning_complete =
        trim_trailing_blank_scrollback_lines(widget.drain_scrollback_lines(80));
    assert!(
        !scrollback_contains_text(&committed_before_reasoning_complete, "final answer line 1"),
        "assistant output should stay live, not drain to scrollback while reasoning is pending"
    );
    let active_before_reasoning_complete = rendered_rows(&widget, 80, 16).join("\n");
    assert!(
        active_before_reasoning_complete.contains("final answer line 1"),
        "assistant output should remain visible in the active viewport:\n{active_before_reasoning_complete}"
    );

    widget.handle_worker_event(crate::events::WorkerEvent::ReasoningCompleted(
        "thinking".to_string(),
    ));

    // Reasoning is now committed to scrollback on ReasoningCompleted,
    // no longer visible in the live viewport.
    let after = rendered_rows(&widget, 80, 16).join("\n");
    assert!(
        !after.contains("thinking"),
        "reasoning text should commit to scrollback, not remain in viewport:\n{after}"
    );

    let committed_after_reasoning_complete =
        trim_trailing_blank_scrollback_lines(widget.drain_scrollback_lines(80));
    let committed_after_text = committed_after_reasoning_complete
        .iter()
        .flat_map(|line| line.line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(
        committed_after_text.contains("thinking"),
        "reasoning text should be in scrollback after ReasoningCompleted: {committed_after_reasoning_complete:?}"
    );
    let after_reasoning_rows = rendered_rows(&widget, 80, 16).join("\n");
    assert!(
        after_reasoning_rows.contains("final answer line 2"),
        "undrained assistant output should remain active after reasoning completes:\n{after_reasoning_rows}"
    );
}

#[test]
fn lifecycle_text_items_render_as_ordered_sibling_cells() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);
    let reasoning_id = ItemId::new();
    let assistant_id = ItemId::new();

    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextItemStarted {
        item_id: reasoning_id,
        kind: crate::events::TextItemKind::Reasoning,
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextItemDelta {
        item_id: reasoning_id,
        kind: crate::events::TextItemKind::Reasoning,
        delta: "thinking".to_string(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextItemStarted {
        item_id: assistant_id,
        kind: crate::events::TextItemKind::Assistant,
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextItemDelta {
        item_id: assistant_id,
        kind: crate::events::TextItemKind::Assistant,
        delta: "Line1\nLine2\n".to_string(),
    });

    let rows = rendered_rows(&widget, 80, 16);
    let reasoning_row = find_row_index(&rows, "thinking").expect("missing reasoning row");
    let line1 = find_row_index(&rows, "Line1").expect("missing assistant row");
    let line2 = find_row_index(&rows, "Line2").expect("missing second assistant row");
    assert_eq!(
        line1,
        reasoning_row + 2,
        "unexpected rows:\n{}",
        rows.join("\n")
    );
    assert_eq!(line2, line1 + 1, "unexpected rows:\n{}", rows.join("\n"));

    widget.handle_worker_event(crate::events::WorkerEvent::TextItemCompleted {
        item_id: reasoning_id,
        kind: crate::events::TextItemKind::Reasoning,
        final_text: "thinking".to_string(),
    });
    let rows_after_reasoning = rendered_rows(&widget, 80, 16);
    assert!(
        !rows_after_reasoning
            .iter()
            .any(|row| row.contains("thinking")),
        "completed reasoning should leave active viewport:\n{}",
        rows_after_reasoning.join("\n")
    );
    assert!(
        rows_after_reasoning.iter().any(|row| row.contains("Line1")),
        "assistant should remain active:\n{}",
        rows_after_reasoning.join("\n")
    );
}

#[test]
fn lifecycle_text_items_keep_reasoning_before_assistant_when_events_arrive_out_of_order() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);
    let reasoning_id = ItemId::new();
    let assistant_id = ItemId::new();

    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextItemStarted {
        item_id: assistant_id,
        kind: crate::events::TextItemKind::Assistant,
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextItemDelta {
        item_id: assistant_id,
        kind: crate::events::TextItemKind::Assistant,
        delta: "answer line\n".to_string(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextItemStarted {
        item_id: reasoning_id,
        kind: crate::events::TextItemKind::Reasoning,
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextItemDelta {
        item_id: reasoning_id,
        kind: crate::events::TextItemKind::Reasoning,
        delta: "thinking text".to_string(),
    });

    let rows = rendered_rows(&widget, 80, 16);
    let reasoning_row = find_row_index(&rows, "thinking text").expect("missing reasoning row");
    let assistant_row = find_row_index(&rows, "answer line").expect("missing assistant row");
    assert!(
        reasoning_row < assistant_row,
        "reasoning should render above assistant:\n{}",
        rows.join("\n")
    );

    widget.handle_worker_event(crate::events::WorkerEvent::TextItemCompleted {
        item_id: assistant_id,
        kind: crate::events::TextItemKind::Assistant,
        final_text: "answer line".to_string(),
    });
    let committed_before_reasoning = widget.drain_scrollback_lines(80);
    assert!(
        !scrollback_contains_text(&committed_before_reasoning, "answer line"),
        "assistant should wait for prior reasoning before committing: {committed_before_reasoning:?}"
    );

    widget.handle_worker_event(crate::events::WorkerEvent::TextItemCompleted {
        item_id: reasoning_id,
        kind: crate::events::TextItemKind::Reasoning,
        final_text: "thinking text".to_string(),
    });
    let committed = scrollback_plain_lines(&trim_trailing_blank_scrollback_lines(
        widget.drain_scrollback_lines(80),
    ))
    .join("\n");
    let reasoning_index = committed
        .find("thinking text")
        .expect("missing committed reasoning");
    let assistant_index = committed
        .find("answer line")
        .expect("missing committed assistant");
    assert!(
        reasoning_index < assistant_index,
        "reasoning should commit before assistant:\n{committed}"
    );
}

#[test]
fn assistant_stream_commit_tick_runs_while_reasoning_is_pending() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);
    let reasoning_id = ItemId::new();
    let assistant_id = ItemId::new();

    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextItemStarted {
        item_id: reasoning_id,
        kind: crate::events::TextItemKind::Reasoning,
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextItemDelta {
        item_id: reasoning_id,
        kind: crate::events::TextItemKind::Reasoning,
        delta: "thinking text".to_string(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextItemStarted {
        item_id: assistant_id,
        kind: crate::events::TextItemKind::Assistant,
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextItemDelta {
        item_id: assistant_id,
        kind: crate::events::TextItemKind::Assistant,
        delta: "first line\nsecond line\n".to_string(),
    });

    widget.pre_draw_tick();
    let committed = scrollback_plain_lines(&widget.drain_scrollback_lines(80)).join("\n");
    let active = rendered_rows(&widget, 80, 16).join("\n");
    assert!(
        !committed.contains("first line"),
        "assistant stream should stay out of scrollback until completion:\n{committed}"
    );
    assert!(
        active.contains("first line"),
        "assistant stream should remain visible even with pending reasoning:\n{active}"
    );
}

// TODO: Still buggy here, need to be fixed.
// #[test]
// fn slash_popup_shows_active_filter_hint() {
//     let cwd = std::env::current_dir().expect("current directory is available");
//     let model = Model {
//         slug: "test-model".to_string(),
//         display_name: "Test Model".to_string(),
//         ..Model::default()
//     };
//     let (mut widget, _app_event_rx) = widget_with_model(model, cwd);

//     widget.handle_paste("/m".to_string());

//     let rendered = rendered_rows(&widget, 80, 6).join("\n");
//     assert!(rendered.contains("filter: /m"));
//     assert!(rendered.contains("/model"));
// }

#[test]
fn slash_model_opens_model_picker_instead_of_printing_current_model() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let alt_model = Model {
        slug: "second-model".to_string(),
        display_name: "Second Model".to_string(),
        thinking_capability: ThinkingCapability::Levels(vec![
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ]),
        default_reasoning_effort: Some(ReasoningEffort::High),
        ..Model::default()
    };
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let mut widget = ChatWidget::new_with_app_event(ChatWidgetInit {
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx: AppEventSender::new(app_event_tx),
        initial_session: TuiSessionState::new(cwd, Some(model.clone())),
        initial_thinking_selection: None,
        initial_permission_preset: devo_protocol::PermissionPreset::Default,
        initial_user_message: None,
        enhanced_keys_supported: true,
        is_first_run: false,
        available_models: vec![model, alt_model],
        saved_model_slugs: vec!["test-model".into(), "second-model".into()],
        show_model_onboarding: false,
        startup_tooltip_override: None,
        initial_theme_name: None,
    });

    widget.handle_app_event(AppEvent::RunSlashCommand {
        command: "model".to_string(),
    });

    assert_eq!(widget.placeholder_text(), "Ask Devo");
    assert_eq!(
        widget.current_model().map(|m| m.slug.as_str()),
        Some("test-model")
    );
}

#[test]
fn session_switch_updates_session_identity_projection() {
    let initial_cwd = std::env::current_dir().expect("current directory is available");
    let resumed_cwd = initial_cwd.join("resumed");
    let model = Model {
        slug: "initial-model".to_string(),
        display_name: "Initial Model".to_string(),
        ..Model::default()
    };
    let resumed_model = Model {
        slug: "resumed-model".to_string(),
        display_name: "Resumed Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, initial_cwd);

    widget.handle_worker_event(crate::events::WorkerEvent::SessionSwitched {
        session_id: "session-1".to_string(),
        cwd: resumed_cwd.clone(),
        title: Some("Resumed".to_string()),
        model: Some("resumed-model".to_string()),
        thinking: None,
        reasoning_effort: None,
        total_input_tokens: 3,
        total_output_tokens: 5,
        total_cache_read_tokens: 0,
        last_query_total_tokens: 8,
        last_query_input_tokens: 3,
        prompt_token_estimate: 3,
        history_items: Vec::new(),
        loaded_item_count: 0,
        pending_texts: vec![],
    });

    assert_eq!(widget.current_cwd(), resumed_cwd.as_path());
    assert_eq!(
        widget.current_model(),
        Some(&Model {
            display_name: "resumed-model".to_string(),
            ..resumed_model
        })
    );
}

#[test]
fn status_summary_uses_last_turn_total_when_idle_and_live_estimate_while_busy() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);

    widget.handle_worker_event(crate::events::WorkerEvent::SessionSwitched {
        session_id: "session-1".to_string(),
        cwd: std::env::current_dir().expect("current directory is available"),
        title: Some("Resumed".to_string()),
        model: Some("test-model".to_string()),
        thinking: None,
        reasoning_effort: None,
        total_input_tokens: 12,
        total_output_tokens: 18,
        total_cache_read_tokens: 4,
        last_query_total_tokens: 42,
        last_query_input_tokens: 42,
        prompt_token_estimate: 12,
        history_items: Vec::new(),
        loaded_item_count: 0,
        pending_texts: vec![],
    });

    let idle_summary = widget.status_summary_text();
    assert!(idle_summary.contains("↑12"));
    assert!(idle_summary.contains("↺4 33%"));
    assert!(idle_summary.contains("↓18"));
    assert!(idle_summary.contains("42/190k"));

    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::UsageUpdated {
        total_input_tokens: 7,
        total_output_tokens: 2,
        total_cache_read_tokens: 6,
        last_query_total_tokens: 9,
        last_query_input_tokens: 7,
    });

    let busy_summary = widget.status_summary_text();
    assert!(busy_summary.contains("↑7"));
    assert!(busy_summary.contains("↺6 86%"));
    assert!(busy_summary.contains("7/190k"));

    widget.handle_worker_event(crate::events::WorkerEvent::TurnFinished {
        stop_reason: "stop".to_string(),
        turn_count: 2,
        total_input_tokens: 19,
        total_output_tokens: 20,
        total_cache_read_tokens: 6,
        last_query_total_tokens: 9,
        last_query_input_tokens: 7,
        prompt_token_estimate: 7,
    });

    let finished_summary = widget.status_summary_text();
    assert!(finished_summary.contains("↑19"));
    assert!(finished_summary.contains("↺6 32%"));
    assert!(finished_summary.contains("7/190k"));
}

#[test]
fn streaming_controller_is_initialized_and_commit_ticks_drain_lines() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);

    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    assert!(!widget.has_stream_controller());

    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(
        "first line\nsecond line\n".to_string(),
    ));
    assert!(widget.has_stream_controller());

    widget.pre_draw_tick();
    let first_pass = rendered_rows(&widget, 80, 12).join("\n");
    assert!(first_pass.contains("first line"));
    assert!(first_pass.contains("second line"));
    let first_scrollback = scrollback_plain_lines(&widget.drain_scrollback_lines(80)).join("\n");
    assert!(!first_scrollback.contains("first line"));

    widget.pre_draw_tick();
    let second_pass = rendered_rows(&widget, 80, 12).join("\n");
    assert!(second_pass.contains("second line"));
}

#[test]
fn new_session_prepared_appends_header_after_existing_history_and_resets_status() {
    let initial_cwd = std::env::current_dir().expect("current directory is available");
    let resumed_cwd = initial_cwd.join("resumed");
    let model = Model {
        slug: "initial-model".to_string(),
        display_name: "Initial Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, initial_cwd.clone());

    widget.handle_worker_event(crate::events::WorkerEvent::SessionSwitched {
        session_id: "session-1".to_string(),
        cwd: resumed_cwd,
        title: None,
        model: Some("resumed-model".to_string()),
        thinking: None,
        reasoning_effort: None,
        total_input_tokens: 30,
        total_output_tokens: 5,
        total_cache_read_tokens: 12,
        last_query_total_tokens: 25,
        last_query_input_tokens: 20,
        prompt_token_estimate: 20,
        history_items: Vec::new(),
        loaded_item_count: 0,
        pending_texts: vec![],
    });
    widget.add_to_history(crate::history_cell::new_info_event(
        "old session line".to_string(),
        None,
    ));

    widget.handle_worker_event(crate::events::WorkerEvent::NewSessionPrepared {
        cwd: initial_cwd.clone(),
        model: "new-session-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        last_query_total_tokens: 25,
        last_query_input_tokens: 20,
        total_cache_read_tokens: 12,
    });

    assert_eq!(widget.current_cwd(), initial_cwd.as_path());
    assert_eq!(
        widget.current_model().map(|model| model.slug.as_str()),
        Some("new-session-model")
    );

    let summary = widget.status_summary_text();
    assert!(summary.contains("↑0"));
    assert!(summary.contains("↺0 0%"));
    assert!(summary.contains("↓0"));
    assert!(summary.contains("0/190k"));

    let transcript_lines = scrollback_plain_lines(
        &widget
            .transcript_overlay_lines(80)
            .into_iter()
            .map(crate::history_cell::ScrollbackLine::new)
            .collect::<Vec<_>>(),
    );
    let transcript_text = transcript_lines.join("\n");
    assert!(transcript_text.contains("old session line"));
    let old_line_index = find_row_index(&transcript_lines, "old session line")
        .expect("old session line remains in transcript");
    let header_index =
        find_row_index(&transcript_lines, "Devo").expect("new session header is appended");
    assert!(header_index > old_line_index);
}

#[test]
fn new_session_prepared_does_not_duplicate_startup_header_without_history() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd.clone());

    widget.handle_worker_event(crate::events::WorkerEvent::NewSessionPrepared {
        cwd,
        model: "new-session-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        last_query_total_tokens: 10,
        last_query_input_tokens: 10,
        total_cache_read_tokens: 4,
    });

    let rows = rendered_rows(&widget, 80, 16);
    assert_eq!(rows.iter().filter(|row| row.contains("Devo")).count(), 1);
    assert!(widget.status_summary_text().contains("↺0 0%"));
}

#[test]
fn model_selection_updates_session_projection_and_emits_context_override() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let alt_model = Model {
        slug: "second-model".to_string(),
        display_name: "Second Model".to_string(),
        thinking_capability: ThinkingCapability::Levels(vec![
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ]),
        default_reasoning_effort: Some(ReasoningEffort::High),
        ..Model::default()
    };
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let mut widget = ChatWidget::new_with_app_event(ChatWidgetInit {
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx: AppEventSender::new(app_event_tx),
        initial_session: TuiSessionState::new(cwd, Some(model.clone())),
        initial_thinking_selection: None,
        initial_permission_preset: devo_protocol::PermissionPreset::Default,
        initial_user_message: None,
        enhanced_keys_supported: true,
        is_first_run: false,
        available_models: vec![model, alt_model.clone()],
        saved_model_slugs: vec!["test-model".into(), "second-model".into()],
        show_model_onboarding: false,
        startup_tooltip_override: None,
        initial_theme_name: None,
    });

    widget.handle_app_event(AppEvent::ModelSelected {
        model: "second-model".to_string(),
    });
    widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    widget.submit_text("hello".to_string());

    assert_eq!(widget.current_model(), Some(&alt_model));
    assert_eq!(
        app_event_rx
            .try_recv()
            .expect("context override command is emitted"),
        AppEvent::Command(AppCommand::OverrideTurnContext {
            cwd: None,
            model: Some("second-model".to_string()),
            thinking: Some(Some("high".to_string())),
            sandbox: None,
            approval_policy: None,
        })
    );
    assert_eq!(
        app_event_rx.try_recv().expect("command event is emitted"),
        AppEvent::Command(AppCommand::UserTurn {
            input: vec![InputItem::Text {
                text: "hello".to_string(),
            }],
            cwd: Some(widget.current_cwd().to_path_buf()),
            model: Some("second-model".to_string()),
            thinking: Some("high".to_string()),
            sandbox: None,
            approval_policy: Some("on-request".to_string()),
        })
    );
}

#[test]
fn model_selection_with_thinking_support_waits_for_second_step() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let alt_model = Model {
        slug: "second-model".to_string(),
        display_name: "Second Model".to_string(),
        thinking_capability: ThinkingCapability::Levels(vec![
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ]),
        default_reasoning_effort: Some(ReasoningEffort::High),
        ..Model::default()
    };
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let mut widget = ChatWidget::new_with_app_event(ChatWidgetInit {
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx: AppEventSender::new(app_event_tx),
        initial_session: TuiSessionState::new(cwd, Some(model)),
        initial_thinking_selection: None,
        initial_permission_preset: devo_protocol::PermissionPreset::Default,
        initial_user_message: None,
        enhanced_keys_supported: true,
        is_first_run: false,
        available_models: vec![alt_model.clone()],
        saved_model_slugs: vec!["second-model".into()],
        show_model_onboarding: false,
        startup_tooltip_override: None,
        initial_theme_name: None,
    });

    widget.handle_app_event(AppEvent::ModelSelected {
        model: "second-model".to_string(),
    });

    assert_eq!(widget.current_model(), Some(&alt_model));
    assert!(app_event_rx.try_recv().is_err());

    widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(
        app_event_rx
            .try_recv()
            .expect("context override command is emitted"),
        AppEvent::Command(AppCommand::OverrideTurnContext {
            cwd: None,
            model: Some("second-model".to_string()),
            thinking: Some(Some("high".to_string())),
            sandbox: None,
            approval_policy: None,
        })
    );
}

#[test]
fn model_selection_without_thinking_support_finishes_immediately() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let base_model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let alt_model = Model {
        slug: "plain-model".to_string(),
        display_name: "Plain Model".to_string(),
        thinking_capability: ThinkingCapability::Unsupported,
        ..Model::default()
    };
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let mut widget = ChatWidget::new_with_app_event(ChatWidgetInit {
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx: AppEventSender::new(app_event_tx),
        initial_session: TuiSessionState::new(cwd, Some(base_model)),
        initial_thinking_selection: None,
        initial_permission_preset: devo_protocol::PermissionPreset::Default,
        initial_user_message: None,
        enhanced_keys_supported: true,
        is_first_run: false,
        available_models: vec![alt_model.clone()],
        saved_model_slugs: vec!["plain-model".into()],
        show_model_onboarding: false,
        startup_tooltip_override: None,
        initial_theme_name: None,
    });

    widget.handle_app_event(AppEvent::ModelSelected {
        model: "plain-model".to_string(),
    });

    assert_eq!(widget.current_model(), Some(&alt_model));
    assert_eq!(
        app_event_rx
            .try_recv()
            .expect("context override command is emitted"),
        AppEvent::Command(AppCommand::OverrideTurnContext {
            cwd: None,
            model: Some("plain-model".to_string()),
            thinking: Some(None),
            sandbox: None,
            approval_policy: None,
        })
    );
}

#[test]
fn flushed_assistant_lines_after_reasoning_are_in_one_cell() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);

    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    // Activate reasoning pause
    widget.handle_worker_event(crate::events::WorkerEvent::ReasoningDelta(
        "thinking".to_string(),
    ));
    // Queue assistant lines while reasoning is active
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(
        "line one\nline two\nline three\n".to_string(),
    ));
    // Complete reasoning; assistant stays active until its own item or turn completes.
    widget.handle_worker_event(crate::events::WorkerEvent::ReasoningCompleted(
        "thinking".to_string(),
    ));

    let committed = trim_trailing_blank_scrollback_lines(widget.drain_scrollback_lines(80));
    let committed_text = committed
        .iter()
        .flat_map(|l| l.line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(committed_text.contains("thinking"));
    assert!(!committed_text.contains("line one"));

    widget.handle_worker_event(crate::events::WorkerEvent::TurnFinished {
        stop_reason: "Completed".to_string(),
        turn_count: 1,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        last_query_total_tokens: 0,
        last_query_input_tokens: 0,
        prompt_token_estimate: 0,
    });

    let committed = widget.drain_scrollback_lines(80);
    let non_blank: Vec<&crate::history_cell::ScrollbackLine> = committed
        .iter()
        .filter(|l| {
            !l.line
                .spans
                .iter()
                .all(|span| span.content.trim().is_empty())
        })
        .collect();
    let text = non_blank
        .iter()
        .flat_map(|l| l.line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(text.contains("line one"));
    assert!(text.contains("line two"));
    assert!(text.contains("line three"));
}

#[test]
fn completed_streaming_assistant_consolidates_to_source_backed_cell() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);

    let _ = widget.drain_scrollback_lines(80);
    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(
        "## Architecture\n\nA. Input pipeline\n\n".to_string(),
    ));
    widget.pre_draw_tick();
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(
        "TuiEvent".to_string(),
    ));
    widget.handle_worker_event(crate::events::WorkerEvent::TurnFinished {
        stop_reason: "Completed".to_string(),
        turn_count: 1,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        last_query_total_tokens: 0,
        last_query_input_tokens: 0,
        prompt_token_estimate: 0,
    });

    let committed = widget.drain_scrollback_lines(80);
    let text = committed
        .iter()
        .flat_map(|line| line.line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert_eq!(
        text.matches("Architecture").count(),
        1,
        "completed assistant history should be consolidated without replay: {text}"
    );
    assert!(text.contains("TuiEvent"));
}

#[test]
fn reasoning_appears_exactly_once_after_full_turn() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);

    let _ = widget.drain_scrollback_lines(80);
    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::ReasoningDelta(
        "I am a unique thought".to_string(),
    ));
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(
        "final answer\n".to_string(),
    ));
    widget.handle_worker_event(crate::events::WorkerEvent::ReasoningCompleted(
        "I am a unique thought".to_string(),
    ));
    widget.handle_worker_event(crate::events::WorkerEvent::TurnFinished {
        stop_reason: "stop".to_string(),
        turn_count: 1,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        last_query_total_tokens: 0,
        last_query_input_tokens: 0,
        prompt_token_estimate: 0,
    });

    let scrollback = widget.drain_scrollback_lines(80);
    let full_text = scrollback
        .iter()
        .flat_map(|line| line.line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert_eq!(
        full_text.matches("I am a unique thought").count(),
        1,
        "reasoning should appear exactly once in scrollback, got:\n{full_text}"
    );
}

#[test]
fn live_reasoning_cell_renders_without_duplication() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, cwd);

    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
        reasoning_effort: None,
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::ReasoningDelta(
        "step by step analysis".to_string(),
    ));

    let rows = rendered_rows(&widget, 80, 12);
    let before = rows.join("\n");
    // Reasoning text should be visible and appear exactly once.
    assert!(
        before.contains("step by step analysis"),
        "reasoning text should be visible:\n{before}"
    );
    let occurrences = before.matches("step by step analysis").count();
    assert_eq!(
        occurrences, 1,
        "reasoning should appear exactly once, got {occurrences}:\n{before}"
    );
}

#[test]
fn transcript_overlay_lines_include_full_completed_tool_output() {
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, PathBuf::from("."));
    let output = (1..=8)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n");

    widget.handle_worker_event(crate::events::WorkerEvent::ToolCall {
        tool_use_id: "tool-1".to_string(),
        summary: "bash".to_string(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::ToolResult {
        tool_use_id: "tool-1".to_string(),
        title: "bash".to_string(),
        preview: output,
        is_error: false,
        truncated: false,
    });

    let inline = scrollback_plain_lines(&widget.drain_scrollback_lines(80)).join("\n");
    let transcript = widget
        .transcript_overlay_lines(80)
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !inline.contains("line 5"),
        "inline output should stay compact: {inline}"
    );
    assert!(
        transcript.contains("line 5") && transcript.contains("line 8"),
        "transcript output should include the full tool output: {transcript}"
    );
}

#[test]
fn transcript_overlay_lines_include_running_tool_output_delta() {
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = widget_with_model(model, PathBuf::from("."));

    widget.handle_worker_event(crate::events::WorkerEvent::ToolCall {
        tool_use_id: "tool-1".to_string(),
        summary: "bash".to_string(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::ToolOutputDelta {
        tool_use_id: "tool-1".to_string(),
        delta: "streamed output line".to_string(),
    });

    let transcript = widget
        .transcript_overlay_lines(80)
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        transcript.contains("streamed output line"),
        "transcript output should include running tool deltas: {transcript}"
    );
}
