use std::path::PathBuf;
use std::time::Instant;

use clawcr_core::{
    BuiltinModelCatalog, ModelConfig, ModelVisibility, ProviderKind, ReasoningLevel, SessionId,
    ThinkingCapability,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pretty_assertions::assert_eq;
use ratatui::layout::Rect;

use crate::app::{AuxPanelContent, TuiApp};
use crate::{
    events::{SessionListEntry, TranscriptItem, TranscriptItemKind, WorkerEvent},
    input::InputBuffer,
    render,
    worker::QueryWorkerHandle,
    SavedModelEntry,
};

fn test_app() -> TuiApp {
    TuiApp {
        model: "test-model".to_string(),
        provider: ProviderKind::Anthropic,
        cwd: PathBuf::from("."),
        transcript: Vec::new(),
        input: InputBuffer::new(),
        status_message: "Ready".to_string(),
        busy: false,
        spinner_index: 0,
        scroll: 0,
        follow_output: true,
        turn_count: 3,
        total_input_tokens: 10,
        total_output_tokens: 20,
        slash_selection: 0,
        pending_status_index: None,
        pending_assistant_index: None,
        worker: QueryWorkerHandle::stub(),
        model_catalog: BuiltinModelCatalog::new(vec![ModelConfig {
            slug: "test-model".to_string(),
            display_name: "test-model".to_string(),
            provider: ProviderKind::Anthropic,
            description: None,
            default_reasoning_level: ReasoningLevel::Medium,
            supported_reasoning_levels: vec![ReasoningLevel::Low, ReasoningLevel::Medium],
            thinking_capability: Some(ThinkingCapability::Toggle),
            base_instructions: String::new(),
            context_window: 200_000,
            effective_context_window_percent: 90,
            auto_compact_token_limit: None,
            truncation_policy: clawcr_core::TruncationPolicyConfig::default(),
            input_modalities: vec![clawcr_core::InputModality::Text],
            supports_image_detail_original: false,
            visibility: ModelVisibility::Visible,
            supported_in_api: true,
            priority: 0,
        }]),
        saved_models: vec![SavedModelEntry {
            model: "test-model".to_string(),
            provider: ProviderKind::Anthropic,
            base_url: None,
            api_key: None,
        }],
        show_model_onboarding: false,
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
        aux_panel: None,
        aux_panel_selection: 0,
        thinking_selection: None,
        pending_tool_items: std::collections::HashMap::new(),
        last_ctrl_c_at: None,
        paste_burst: crate::paste_burst::PasteBurst::default(),
        should_quit: false,
    }
}

#[tokio::test]
async fn assistant_text_deltas_append_to_same_item() {
    let mut app = test_app();
    app.handle_worker_event(WorkerEvent::TextDelta("hel".to_string()));
    app.handle_worker_event(WorkerEvent::TextDelta("lo".to_string()));

    assert_eq!(app.transcript.len(), 1);
    assert_eq!(app.transcript[0].kind, TranscriptItemKind::Assistant);
    assert_eq!(app.transcript[0].body, "hello");
}

#[tokio::test]
async fn tool_results_create_separate_items() {
    let mut app = test_app();
    app.handle_worker_event(WorkerEvent::ToolResult {
        tool_use_id: "tool-1".to_string(),
        preview: "done".to_string(),
        is_error: false,
        truncated: false,
    });

    assert_eq!(app.transcript.len(), 1);
    assert_eq!(app.transcript[0].kind, TranscriptItemKind::ToolResult);
    assert_eq!(app.transcript[0].body, "done");
}

#[tokio::test]
async fn tool_result_fold_progresses_to_hidden_compact_state() {
    let mut item = TranscriptItem::new(
        TranscriptItemKind::ToolResult,
        "Tool output",
        (1..=12)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .with_tool_fold();

    let first = item.fold_next_at.expect("fold deadline");
    assert!(!item.advance_fold(Instant::now()));
    assert!(item.advance_fold(first));
    assert_eq!(item.fold_stage, 1);

    let second = item.fold_next_at.expect("second fold deadline");
    assert!(item.advance_fold(second));
    assert_eq!(item.fold_stage, 2);

    let third = item.fold_next_at.expect("third fold deadline");
    assert!(item.advance_fold(third));
    assert_eq!(item.fold_stage, 3);
    assert!(item.fold_next_at.is_none());
    assert!(!item.advance_fold(third));
}

#[tokio::test]
async fn slash_status_shows_bottom_panel() {
    let mut app = test_app();

    app.handle_slash_command("/status".to_string())
        .expect("status command should succeed");

    assert!(app.transcript.is_empty());
    assert_eq!(
        app.aux_panel.as_ref().map(|panel| panel.title.as_str()),
        Some("Status")
    );
    assert!(app.aux_panel.as_ref().is_some_and(
        |panel| matches!(&panel.content, AuxPanelContent::Text(body) if body.contains("turns: 3"))
    ));
}

#[tokio::test]
async fn slash_sessions_requests_listing() {
    let mut app = test_app();

    app.handle_slash_command("/sessions".to_string())
        .expect("sessions command should succeed");

    assert_eq!(app.status_message, "Ready");
}

#[tokio::test]
async fn slash_new_requests_new_session() {
    let mut app = test_app();

    app.handle_slash_command("/new".to_string())
        .expect("new command should succeed");

    assert_eq!(
        app.status_message,
        "New session ready; send a prompt to start it"
    );
}

#[tokio::test]
async fn slash_model_shows_bottom_panel() {
    let mut app = test_app();

    app.handle_slash_command("/model".to_string())
        .expect("model command should succeed");

    assert!(app.transcript.is_empty());
    assert_eq!(
        app.aux_panel.as_ref().map(|panel| panel.title.as_str()),
        Some("Models")
    );
    assert!(app
            .aux_panel
            .as_ref()
            .is_some_and(|panel| matches!(&panel.content, AuxPanelContent::ModelList(entries) if entries.iter().any(|entry| entry.slug == "test-model") && entries.iter().any(|entry| entry.is_custom_mode))));
}

#[tokio::test]
async fn slash_thinking_shows_bottom_panel() {
    let mut app = test_app();

    app.handle_slash_command("/thinking".to_string())
        .expect("thinking command should succeed");

    assert!(app.transcript.is_empty());
    assert_eq!(
        app.aux_panel.as_ref().map(|panel| panel.title.as_str()),
        Some("Thinking")
    );
    assert!(app.aux_panel.as_ref().is_some_and(
        |panel| matches!(&panel.content, AuxPanelContent::ThinkingList(entries) if !entries.is_empty())
    ));
}

#[tokio::test]
async fn slash_onboard_starts_onboarding_flow() {
    let mut app = test_app();

    app.handle_slash_command("/onboard".to_string())
        .expect("onboard command should succeed");

    assert!(app.show_model_onboarding);
    assert!(app.is_onboarding_model_picker_open());
    assert_eq!(app.status_message, "Onboarding started");
}

#[tokio::test]
async fn slash_rename_requires_title() {
    let mut app = test_app();

    assert!(app.handle_slash_command("/rename".to_string()).is_err());
}

#[tokio::test]
async fn slash_exit_requests_shutdown() {
    let mut app = test_app();

    app.handle_slash_command("/exit".to_string())
        .expect("exit command should succeed");

    assert!(app.should_quit);
}

#[tokio::test]
async fn ctrl_c_requires_confirmation_when_idle() {
    let mut app = test_app();

    app.handle_ctrl_c();
    assert!(!app.should_quit);
    assert_eq!(app.status_message, "Press Ctrl+C again within 2s to exit.");

    app.handle_ctrl_c();
    assert!(app.should_quit);
}

#[tokio::test]
async fn ctrl_c_requests_interrupt_before_exit_when_busy() {
    let mut app = test_app();
    app.busy = true;

    app.handle_ctrl_c();
    assert!(!app.should_quit);
    assert_eq!(
        app.status_message,
        "Interrupt requested. Press Ctrl+C again within 2s to exit."
    );

    app.handle_ctrl_c();
    assert!(app.should_quit);
}

#[tokio::test]
async fn slash_completion_applies_selected_command() {
    let mut app = test_app();
    app.input.replace("/e");

    assert!(app.try_apply_slash_suggestion());
    assert_eq!(app.input.text(), "/exit");
}

#[tokio::test]
async fn slash_suggestions_include_onboard() {
    let mut app = test_app();
    app.input.replace("/o");

    assert!(app
        .slash_suggestions()
        .iter()
        .any(|suggestion| suggestion.name == "/onboard"));
}

#[tokio::test]
async fn enter_executes_highlighted_slash_command() {
    let mut app = test_app();
    app.input.replace("/");
    app.slash_selection = app
        .slash_suggestions()
        .iter()
        .position(|suggestion| suggestion.name == "/exit")
        .expect("exit suggestion should exist");

    app.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        Rect::default(),
    );

    assert!(app.should_quit);
}

#[tokio::test]
async fn model_panel_selection_updates_model() {
    let mut app = test_app();

    app.handle_slash_command("/model".to_string())
        .expect("model command should succeed");
    app.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        Rect::default(),
    );

    assert_eq!(app.model, "test-model");
}

#[tokio::test]
async fn onboarding_model_panel_includes_custom_entry() {
    let mut app = test_app();
    app.show_model_onboarding = true;

    app.show_model_panel();

    assert!(app.aux_panel.as_ref().is_some_and(|panel| {
        matches!(
            &panel.content,
            AuxPanelContent::ModelList(entries)
                if entries.iter().any(|entry| entry.is_custom_mode)
        )
    }));
}

#[tokio::test]
async fn onboarding_model_picker_ignores_plain_typing() {
    let mut app = test_app();
    app.show_model_onboarding = true;
    app.show_model_panel();

    app.handle_key(
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        Rect::default(),
    );

    assert!(app.input.is_blank());
    assert!(app.has_selectable_aux_panel());
}

#[tokio::test]
async fn onboarding_model_picker_allows_custom_shortcut() {
    let mut app = test_app();
    app.show_model_onboarding = true;
    app.show_model_panel();

    app.handle_key(
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        Rect::default(),
    );

    assert!(app.onboarding_custom_model_pending);
    assert_eq!(app.onboarding_prompt.as_deref(), Some("model name"));
    assert!(app.aux_panel.is_none());
}

#[tokio::test]
async fn onboarding_model_picker_enter_on_custom_row_starts_custom_flow() {
    let mut app = test_app();
    app.show_model_onboarding = true;
    app.show_model_panel();
    app.aux_panel_selection = app
        .aux_panel
        .as_ref()
        .and_then(|panel| match &panel.content {
            AuxPanelContent::ModelList(entries) => {
                entries.iter().position(|entry| entry.is_custom_mode)
            }
            _ => None,
        })
        .expect("custom row should exist");

    app.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        Rect::default(),
    );

    assert!(app.onboarding_custom_model_pending);
    assert_eq!(app.onboarding_prompt.as_deref(), Some("model name"));
    assert!(app.aux_panel.is_none());
}

#[tokio::test]
async fn onboarding_model_picker_enter_on_builtin_row_prompts_for_connection() {
    let mut app = test_app();
    app.show_model_onboarding = true;
    app.saved_models = vec![SavedModelEntry {
        model: "existing-model".to_string(),
        provider: ProviderKind::Anthropic,
        base_url: Some("https://example.invalid/v1".to_string()),
        api_key: Some("secret".to_string()),
    }];
    app.model_catalog = BuiltinModelCatalog::new(vec![ModelConfig {
        slug: "new-anthropic-model".to_string(),
        display_name: "New Anthropic Model".to_string(),
        provider: ProviderKind::Anthropic,
        description: Some("test model".to_string()),
        visibility: ModelVisibility::Visible,
        ..ModelConfig::default()
    }]);
    app.show_model_panel();
    app.aux_panel_selection = app
        .aux_panel
        .as_ref()
        .and_then(|panel| match &panel.content {
            AuxPanelContent::ModelList(entries) => entries
                .iter()
                .position(|entry| entry.slug == "new-anthropic-model"),
            _ => None,
        })
        .expect("builtin row should exist");

    app.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        Rect::default(),
    );

    assert!(!app.onboarding_custom_model_pending);
    assert!(app.onboarding_base_url_pending);
    assert_eq!(
        app.onboarding_selected_model.as_deref(),
        Some("new-anthropic-model")
    );
    assert_eq!(app.onboarding_prompt.as_deref(), Some("base url"));
    assert!(app.aux_panel.is_none());
}

#[tokio::test]
async fn onboarding_rejects_base_url_without_http_scheme() {
    let mut app = test_app();
    app.onboarding_base_url_pending = true;
    app.onboarding_selected_model = Some("test-model".to_string());

    app.handle_submission("localhost:11434".to_string())
        .expect("submission should not crash");

    assert!(app.onboarding_base_url_pending);
    assert_eq!(app.onboarding_prompt.as_deref(), Some("base url"));
    assert_eq!(
        app.status_message,
        "Base URL must start with http:// or https://"
    );
}

#[tokio::test]
async fn onboarding_escape_steps_back_to_model_list() {
    let mut app = test_app();
    app.show_model_onboarding = true;
    app.begin_custom_model_onboarding();

    app.handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        Rect::default(),
    );

    assert!(app.is_onboarding_model_picker_open());
    assert!(app.onboarding_prompt.is_none());
    assert!(!app.onboarding_custom_model_pending);
}

#[tokio::test]
async fn onboarding_escape_from_root_dismisses_onboarding() {
    let mut app = test_app();
    app.show_model_onboarding = true;
    app.show_model_panel();

    app.handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        Rect::default(),
    );

    assert!(!app.show_model_onboarding);
    assert!(app.aux_panel.is_none());
    assert_eq!(app.status_message, "Onboarding dismissed");
}

#[tokio::test]
async fn session_new_command_updates_status() {
    let mut app = test_app();

    app.handle_slash_command("/new".to_string())
        .expect("slash command should succeed");

    assert_eq!(
        app.status_message,
        "New session ready; send a prompt to start it"
    );
    assert_eq!(app.aux_panel, None);
}

#[tokio::test]
async fn provider_validation_failure_returns_to_api_key_step() {
    let mut app = test_app();
    app.busy = true;
    app.onboarding_selected_model = Some("test-model".to_string());

    app.handle_worker_event(WorkerEvent::ProviderValidationFailed {
        message: "connection refused".to_string(),
    });

    assert!(!app.busy);
    assert!(app.onboarding_api_key_pending);
    assert_eq!(app.onboarding_prompt.as_deref(), Some("api key"));
    assert!(app.status_message.contains("connection refused"));
}

#[tokio::test]
async fn turn_failed_uses_specific_error_status_message() {
    let mut app = test_app();
    app.busy = true;

    app.handle_worker_event(WorkerEvent::TurnFailed {
        message: "anthropic provider requires an API key".to_string(),
        turn_count: 3,
        total_input_tokens: 10,
        total_output_tokens: 20,
    });

    assert_eq!(
        app.transcript.last(),
        Some(&TranscriptItem::new(
            TranscriptItemKind::Error,
            "Error",
            "anthropic provider requires an API key"
        ))
    );
    assert_eq!(app.status_message, "Query failed; see error above");
}

#[tokio::test]
async fn new_session_prepared_clears_transcript_and_busy_state() {
    let mut app = test_app();
    app.busy = true;
    app.transcript.push(TranscriptItem::new(
        TranscriptItemKind::User,
        "You",
        "old session",
    ));
    app.pending_status_index = Some(0);

    app.handle_worker_event(WorkerEvent::NewSessionPrepared);

    assert!(app.transcript.is_empty());
    assert!(!app.busy);
    assert_eq!(
        app.status_message,
        "New session ready; send a prompt to start it"
    );
}

#[tokio::test]
async fn tool_call_breaks_assistant_stream_into_new_segment() {
    let mut app = test_app();
    app.handle_worker_event(WorkerEvent::TextDelta("before".to_string()));
    app.handle_worker_event(WorkerEvent::ToolCall {
        tool_use_id: "tool-1".to_string(),
        summary: "bash: date".to_string(),
        detail: Some("{\n  \"command\": \"date\"\n}".to_string()),
    });
    app.handle_worker_event(WorkerEvent::TextDelta("after".to_string()));

    assert_eq!(
        app.transcript,
        vec![
            TranscriptItem::new(TranscriptItemKind::Assistant, "Assistant", "before"),
            TranscriptItem::new(TranscriptItemKind::ToolCall, "bash: date", ""),
            TranscriptItem::new(TranscriptItemKind::Assistant, "Assistant", "after"),
        ]
    );
}

#[tokio::test]
async fn tool_result_readds_thinking_while_turn_is_still_busy() {
    let mut app = test_app();
    app.busy = true;
    app.pending_status_index = Some(app.push_item(TranscriptItemKind::System, "Thinking", ""));

    app.handle_worker_event(WorkerEvent::ToolResult {
        tool_use_id: "tool-1".to_string(),
        preview: "2026-04-06 23:58:56".to_string(),
        is_error: false,
        truncated: false,
    });

    assert_eq!(app.transcript.len(), 2);
    assert_eq!(app.transcript[0].kind, TranscriptItemKind::ToolResult);
    assert_eq!(app.transcript[0].title, "Tool output");
    assert_eq!(app.transcript[0].body, "2026-04-06 23:58:56");
    assert_eq!(app.transcript[0].fold_stage, 0);
    assert!(app.transcript[0].fold_next_at.is_some());
    assert_eq!(
        app.transcript[1],
        TranscriptItem::new(TranscriptItemKind::System, "Thinking", "")
    );
}

#[tokio::test]
async fn submit_prompt_inserts_status_line_below_user_message() {
    let mut app = test_app();

    app.submit_prompt("hello".to_string())
        .expect("submit should succeed");

    assert_eq!(
        app.transcript,
        vec![
            TranscriptItem::new(TranscriptItemKind::User, "You", "hello"),
            TranscriptItem::new(TranscriptItemKind::System, "Thinking", ""),
        ]
    );
}

#[tokio::test]
async fn transcript_area_tracks_content_height_when_short() {
    let app = test_app();
    let area = Rect::new(0, 0, 80, 24);

    assert_eq!(render::transcript_height(&app, area), 7);
    assert!(app.transcript_area(area).height < area.height);
}

#[tokio::test]
async fn session_switched_event_keeps_current_model_and_restores_transcript() {
    let mut app = test_app();

    app.handle_worker_event(WorkerEvent::SessionSwitched {
        session_id: "00000000-0000-0000-0000-000000000001".to_string(),
        title: Some("Saved session".to_string()),
        model: Some("restored-model".to_string()),
        total_input_tokens: 42,
        total_output_tokens: 7,
        history_items: vec![TranscriptItem::new(
            TranscriptItemKind::User,
            "You",
            "restored prompt",
        )],
        loaded_item_count: 7,
    });

    assert_eq!(app.model, "test-model");
    assert_eq!(app.total_input_tokens, 42);
    assert_eq!(app.total_output_tokens, 7);
    assert_eq!(app.transcript.len(), 1);
    assert_eq!(app.transcript[0].kind, TranscriptItemKind::User);
    assert_eq!(app.transcript[0].body, "restored prompt");
}

#[tokio::test]
async fn session_renamed_event_adds_transcript_note() {
    let mut app = test_app();

    app.handle_worker_event(WorkerEvent::SessionRenamed {
        session_id: "00000000-0000-0000-0000-000000000001".to_string(),
        title: "Renamed session".to_string(),
    });

    assert_eq!(app.status_message, "Session renamed");
    assert_eq!(app.transcript.len(), 1);
    assert!(app.transcript[0].body.contains("Renamed session"));
}

#[tokio::test]
async fn session_title_updated_event_refreshes_visible_session_list() {
    let mut app = test_app();
    let session_id = SessionId::new();
    app.show_session_panel(vec![SessionListEntry {
        session_id,
        title: "(untitled)".to_string(),
        updated_at: "2026-04-06 08:00:00 UTC".to_string(),
        is_active: true,
    }]);

    app.handle_worker_event(WorkerEvent::SessionTitleUpdated {
        session_id: session_id.to_string(),
        title: "Generated title".to_string(),
    });

    assert_eq!(app.status_message, "Session titled: Generated title");
    assert!(app.aux_panel.as_ref().is_some_and(|panel| {
        matches!(
            &panel.content,
            AuxPanelContent::SessionList(entries)
                if entries.iter().any(|entry| entry.title == "Generated title")
        )
    }));
}

#[tokio::test]
async fn sessions_listed_event_updates_bottom_panel_not_transcript() {
    let mut app = test_app();

    app.handle_worker_event(WorkerEvent::SessionsListed {
        sessions: vec![SessionListEntry {
            session_id: SessionId::new(),
            title: "Saved conversation".to_string(),
            updated_at: "2026-04-06 08:00:00 UTC".to_string(),
            is_active: true,
        }],
    });

    assert!(app.transcript.is_empty());
    assert_eq!(
        app.aux_panel.as_ref().map(|panel| panel.title.as_str()),
        Some("Sessions")
    );
    assert!(app
            .aux_panel
            .as_ref()
            .is_some_and(|panel| matches!(&panel.content, AuxPanelContent::SessionList(entries) if entries.iter().any(|entry| entry.title == "Saved conversation"))));
}

#[tokio::test]
async fn session_panel_selection_moves_with_up_and_down() {
    let mut app = test_app();
    app.show_session_panel(vec![
        SessionListEntry {
            session_id: SessionId::new(),
            title: "First".to_string(),
            updated_at: "2026-04-06 08:00:00 UTC".to_string(),
            is_active: true,
        },
        SessionListEntry {
            session_id: SessionId::new(),
            title: "Second".to_string(),
            updated_at: "2026-04-06 09:00:00 UTC".to_string(),
            is_active: false,
        },
    ]);

    app.move_aux_panel_selection(1);
    assert_eq!(app.aux_panel_selection, 1);

    app.move_aux_panel_selection(-1);
    assert_eq!(app.aux_panel_selection, 0);

    app.move_aux_panel_selection(-1);
    assert_eq!(app.aux_panel_selection, 1);

    app.move_aux_panel_selection(1);
    assert_eq!(app.aux_panel_selection, 0);
}

#[tokio::test]
async fn slash_selection_wraps_around() {
    let mut app = test_app();
    app.input.replace("/");

    app.move_slash_selection(-1);
    assert_eq!(app.slash_selection, app.slash_suggestions().len() - 1);

    app.move_slash_selection(1);
    assert_eq!(app.slash_selection, 0);
}

#[tokio::test]
async fn interrupted_turn_adds_status_line_to_transcript() {
    let mut app = test_app();
    app.pending_status_index = Some(app.push_item(TranscriptItemKind::System, "Thinking", ""));
    app.busy = true;

    app.handle_worker_event(WorkerEvent::TurnFinished {
        stop_reason: "Interrupted".to_string(),
        turn_count: 1,
        total_input_tokens: 0,
        total_output_tokens: 0,
    });

    assert_eq!(
        app.transcript,
        vec![TranscriptItem::new(
            TranscriptItemKind::System,
            "Interrupted",
            "",
        )]
    );
}

#[tokio::test]
async fn completed_turn_adds_complete_status_line_to_transcript() {
    let mut app = test_app();
    app.pending_status_index = Some(app.push_item(TranscriptItemKind::System, "Thinking", ""));
    app.busy = true;

    app.handle_worker_event(WorkerEvent::TurnFinished {
        stop_reason: "Completed".to_string(),
        turn_count: 1,
        total_input_tokens: 0,
        total_output_tokens: 0,
    });

    assert_eq!(
        app.transcript,
        vec![TranscriptItem::new(
            TranscriptItemKind::System,
            "Complete",
            ""
        )]
    );
}
