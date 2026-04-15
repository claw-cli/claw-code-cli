use std::path::PathBuf;

use clawcr_core::{Model, PresetModelCatalog};
use clawcr_provider::ProviderFamily;
use pretty_assertions::assert_eq;

use crate::app::TuiApp;
use crate::events::{TranscriptItemKind, WorkerEvent};
use crate::input::InputBuffer;
use crate::worker::QueryWorkerHandle;

fn test_app() -> TuiApp {
    TuiApp {
        model: "test-model".to_string(),
        provider: ProviderFamily::Anthropic,
        cwd: PathBuf::from("."),
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
        pending_status_index: None,
        pending_assistant_index: None,
        worker: QueryWorkerHandle::stub(),
        model_catalog: PresetModelCatalog::new(vec![Model {
            slug: "test-model".to_string(),
            display_name: "Test Model".to_string(),
            provider_family: ProviderFamily::Anthropic,
            thinking_capability: clawcr_core::ThinkingCapability::Toggle,
            ..Model::default()
        }]),
        saved_models: vec![],
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
        inline_mode: false,
        terminal_width: 80,
        inline_assistant_stream_open: false,
        inline_assistant_pending_line: String::new(),
        inline_assistant_header_emitted: false,
        pending_inline_history: Vec::new(),
    }
}

#[tokio::test]
async fn plan_updates_append_assistant_item_and_clear_pending_tool_state() {
    let mut app = test_app();

    app.handle_worker_event(WorkerEvent::ToolCall {
        tool_use_id: "tool-1".to_string(),
        summary: "update_plan: Tracking runtime work".to_string(),
        detail: None,
    });
    app.handle_worker_event(WorkerEvent::PlanUpdated {
        tool_use_id: "tool-1".to_string(),
        text: "Tracking runtime work\n\n[\n  {\n    \"status\": \"in_progress\",\n    \"step\": \"Wire live plan output\"\n  }\n]".to_string(),
    });

    assert!(app.pending_tool_items.is_empty());
    assert_eq!(app.transcript.len(), 2);
    assert_eq!(app.transcript[0].kind, TranscriptItemKind::ToolCall);
    assert_eq!(app.transcript[1].kind, TranscriptItemKind::Assistant);
    assert_eq!(
        app.transcript[1].body,
        "Tracking runtime work\n\n[\n  {\n    \"status\": \"in_progress\",\n    \"step\": \"Wire live plan output\"\n  }\n]"
    );
}
