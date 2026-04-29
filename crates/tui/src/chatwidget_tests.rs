use std::path::PathBuf;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use devo_protocol::InputItem;
use devo_protocol::Model;
use devo_protocol::ReasoningEffort;
use devo_protocol::ThinkingCapability;
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
        initial_user_message: None,
        enhanced_keys_supported: true,
        is_first_run: false,
        available_models: Vec::new(),
        show_model_onboarding: false,
        startup_tooltip_override: None,
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
        initial_user_message: None,
        enhanced_keys_supported: true,
        is_first_run: false,
        available_models: Vec::new(),
        show_model_onboarding: true,
        startup_tooltip_override: None,
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
            approval_policy: None,
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
            approval_policy: None,
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
            approval_policy: None,
        })
    );
}

#[test]
fn onboarding_updates_placeholder_text_for_each_step() {
    let cwd = std::env::current_dir().expect("current directory is available");
    let model = Model {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        ..Model::default()
    };
    let (mut widget, _app_event_rx) = onboarding_widget_with_model(model, cwd);
    assert_eq!(widget.placeholder_text(), "Onboarding: enter model name");

    widget.submit_text("custom-model".to_string());
    assert_eq!(widget.placeholder_text(), "Onboarding: enter base URL");

    widget.submit_text("https://example.com".to_string());
    assert_eq!(widget.placeholder_text(), "Onboarding: enter API key");

    widget.submit_text("secret".to_string());
    assert_eq!(
        widget.placeholder_text(),
        "Onboarding: validating connection"
    );
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
        total_input_tokens: 3,
        total_output_tokens: 5,
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
    });

    let committed_lines = widget.drain_scrollback_lines(80);
    let committed_text = committed_lines
        .iter()
        .flat_map(|line| line.line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    let blank_line_indexes = committed_lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            line.line
                .spans
                .iter()
                .all(|span| span.content.trim().is_empty())
                .then_some(index)
        })
        .collect::<Vec<_>>();

    assert_eq!(1, committed_text.matches("directory:").count());
    assert!(committed_text.contains("hello"));
    assert!(committed_text.contains("world"));
    assert!(!committed_text.contains("session 1 lingering line"));
    assert!(
        blank_line_indexes.windows(2).any(|window| window == [6, 8]),
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
fn active_response_renders_generating_status_without_devo_title() {
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

    widget.handle_worker_event(crate::events::WorkerEvent::TurnStarted {
        model: "test-model".to_string(),
        thinking: None,
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
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(body));
    widget.handle_worker_event(crate::events::WorkerEvent::TurnFinished {
        stop_reason: "Completed".to_string(),
        turn_count: 1,
        total_input_tokens: 0,
        total_output_tokens: 0,
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
        total_input_tokens: 0,
        total_output_tokens: 0,
        prompt_token_estimate: 0,
        history_items: vec![crate::events::TranscriptItem::new(
            crate::events::TranscriptItemKind::Reasoning,
            "",
            "thinking text",
        )],
        loaded_item_count: 1,
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
        turn_id: Default::default(),
    });
    widget.handle_worker_event(crate::events::WorkerEvent::ReasoningDelta(
        "thinking".to_string(),
    ));
    widget.handle_worker_event(crate::events::WorkerEvent::TextDelta(
        "final answer".to_string(),
    ));

    let before_rows = rendered_rows(&widget, 80, 16);
    let before = before_rows.join("\n");
    assert!(
        before.contains("thinking") && before.contains("final answer"),
        "reasoning/text should both be visible while streaming:\n{before}"
    );
    let reasoning_row = find_row_index(&before_rows, "thinking").expect("missing reasoning row");
    let assistant_row =
        find_row_index(&before_rows, "final answer").expect("missing assistant row");
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

    widget.handle_worker_event(crate::events::WorkerEvent::ReasoningCompleted(
        "thinking".to_string(),
    ));

    let after = rendered_rows(&widget, 80, 16).join("\n");
    assert!(
        after.contains("thinking") && after.contains("final answer"),
        "reasoning/text should remain visible in separate cells:\n{after}"
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
        ..Model::default()
    };
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let mut widget = ChatWidget::new_with_app_event(ChatWidgetInit {
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx: AppEventSender::new(app_event_tx),
        initial_session: TuiSessionState::new(cwd, Some(model.clone())),
        initial_thinking_selection: None,
        initial_user_message: None,
        enhanced_keys_supported: true,
        is_first_run: false,
        available_models: vec![model, alt_model],
        show_model_onboarding: false,
        startup_tooltip_override: None,
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
        total_input_tokens: 3,
        total_output_tokens: 5,
        prompt_token_estimate: 3,
        history_items: Vec::new(),
        loaded_item_count: 0,
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
fn new_session_prepared_resets_session_identity_projection() {
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
        total_input_tokens: 3,
        total_output_tokens: 5,
        prompt_token_estimate: 3,
        history_items: Vec::new(),
        loaded_item_count: 0,
    });
    widget.handle_worker_event(crate::events::WorkerEvent::NewSessionPrepared {
        cwd: initial_cwd.clone(),
        model: "new-session-model".to_string(),
        thinking: None,
    });

    assert_eq!(widget.current_cwd(), initial_cwd.as_path());
    assert_eq!(
        widget.current_model().map(|model| model.slug.as_str()),
        Some("new-session-model")
    );
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
        ..Model::default()
    };
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let mut widget = ChatWidget::new_with_app_event(ChatWidgetInit {
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx: AppEventSender::new(app_event_tx),
        initial_session: TuiSessionState::new(cwd, Some(model.clone())),
        initial_thinking_selection: None,
        initial_user_message: None,
        enhanced_keys_supported: true,
        is_first_run: false,
        available_models: vec![model, alt_model.clone()],
        show_model_onboarding: false,
        startup_tooltip_override: None,
    });

    widget.handle_app_event(AppEvent::ModelSelected {
        model: "second-model".to_string(),
    });
    widget.submit_text("hello".to_string());

    assert_eq!(widget.current_model(), Some(&alt_model));
    assert_eq!(
        app_event_rx
            .try_recv()
            .expect("context override command is emitted"),
        AppEvent::Command(AppCommand::OverrideTurnContext {
            cwd: None,
            model: Some("second-model".to_string()),
            thinking: Some(None),
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
            thinking: None,
            sandbox: None,
            approval_policy: None,
        })
    );
}
