use std::collections::VecDeque;

use chrono::Utc;
use devo_core::{
    ItemId, SessionId, SessionRecord, SessionTitleFinalSource, SessionTitleState, TurnId,
    TurnRecord, TurnStatus,
};
use devo_protocol::{SkillChangedParams, SkillListParams, SkillListResult};
use devo_server::{
    ActiveTurnSteeringState, ApprovalDecisionValue, ApprovalRequestPayload, ApprovalRespondParams,
    ApprovalScopeValue, ClientRequest, ClientTransportKind, DefaultProjection, EventContext,
    EventsSubscribeParams, InitializeParams, InputItem, ItemDeltaKind, ItemDeltaPayload,
    PendingServerRequestContext, ProtocolError, ProtocolErrorCode, ServerEvent, ServerRequestKind,
    SessionMetadata, SessionProjector, SessionRuntimeStatus, SessionTitleUpdateParams,
    SteerInputRecord, TurnKind, TurnProjector,
};
use pretty_assertions::assert_eq;

#[test]
fn initialize_params_roundtrip() {
    let params = InitializeParams {
        client_name: "desktop".into(),
        client_version: "1.0.0".into(),
        transport: ClientTransportKind::Stdio,
        supports_streaming: true,
        supports_binary_images: false,
        opt_out_notification_methods: vec!["turn/plan/updated".into()],
    };

    let json = serde_json::to_string(&params).expect("serialize");
    let restored: InitializeParams = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(params, restored);
}

#[test]
fn skill_list_params_roundtrip() {
    let params = SkillListParams { cwd: None };
    let json = serde_json::to_string(&params).expect("serialize");
    let restored: SkillListParams = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(params, restored);
}

#[test]
fn skill_changed_params_roundtrip() {
    let params = SkillChangedParams { cwd: None };
    let json = serde_json::to_string(&params).expect("serialize");
    let restored: SkillChangedParams = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(params, restored);
}

#[test]
fn skill_list_result_serializes_expected_shape() {
    let result = SkillListResult {
        skills: vec![
            devo_protocol::SkillRecord {
                id: "rust-docs".into(),
                name: "Rust Documentation".into(),
                description: "Official Rust docs skill".into(),
                path: std::path::PathBuf::from("/skills/rust/SKILL.md"),
                enabled: true,
                source: devo_protocol::SkillSource::User,
            },
            devo_protocol::SkillRecord {
                id: "python-guide".into(),
                name: "Python Guide".into(),
                description: "Python programming skill".into(),
                path: std::path::PathBuf::from("/skills/python/SKILL.md"),
                enabled: false,
                source: devo_protocol::SkillSource::Workspace {
                    cwd: std::path::PathBuf::from("/workspace"),
                },
            },
        ],
    };

    let json = serde_json::to_string(&result).expect("serialize");
    assert!(json.contains("\"skills\""));
    assert!(json.contains("\"rust-docs\""));
}

#[test]
fn approval_response_roundtrip() {
    let payload = ApprovalRespondParams {
        session_id: SessionId::new(),
        turn_id: TurnId::new(),
        approval_id: "approval-1".into(),
        decision: ApprovalDecisionValue::Approve,
        scope: ApprovalScopeValue::Session,
    };

    let json = serde_json::to_string(&payload).expect("serialize");
    let restored: ApprovalRespondParams = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(payload, restored);
}

#[test]
fn event_context_keeps_correlation_ids() {
    let context = EventContext {
        session_id: SessionId::new(),
        turn_id: Some(TurnId::new()),
        item_id: None,
        seq: 7,
    };

    assert_eq!(context.seq, 7);
    assert!(context.turn_id.is_some());
}

#[test]
fn input_item_serializes_tagged_shape() {
    let input = InputItem::Skill {
        id: "rust-docs".into(),
    };

    let json = serde_json::to_string(&input).expect("serialize");
    assert!(json.contains("\"type\":\"skill\""));
}

#[test]
fn protocol_error_uses_spec_code_strings() {
    let payload = ProtocolError {
        code: ProtocolErrorCode::NotInitialized,
        message: "handshake incomplete".into(),
        data: serde_json::json!({}),
    };

    let json = serde_json::to_string(&payload).expect("serialize");
    assert!(json.contains("NotInitialized"));
}

#[test]
fn server_request_payload_roundtrip() {
    let payload = ApprovalRequestPayload {
        request: PendingServerRequestContext {
            request_id: "req-1".into(),
            request_kind: ServerRequestKind::ItemPermissionsRequestApproval,
            session_id: SessionId::new(),
            turn_id: Some(TurnId::new()),
            item_id: None,
        },
        approval_id: "approval-1".into(),
        action_summary: "run shell command".into(),
        justification: "writes files".into(),
    };

    let json = serde_json::to_string(&payload).expect("serialize");
    let restored: ApprovalRequestPayload = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(payload, restored);
}

#[test]
fn subscribe_params_allow_optional_filters() {
    let payload = EventsSubscribeParams {
        session_id: None,
        event_types: Some(vec!["turn/completed".into()]),
    };

    let json = serde_json::to_string(&payload).expect("serialize");
    let restored: EventsSubscribeParams = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(payload, restored);
}

#[test]
fn steering_state_preserves_queue_order() {
    let first = SteerInputRecord {
        item_id: ItemId::new(),
        received_at: Utc::now(),
        input: vec![InputItem::Text {
            text: "first".into(),
        }],
    };
    let second = SteerInputRecord {
        item_id: ItemId::new(),
        received_at: Utc::now(),
        input: vec![InputItem::Text {
            text: "second".into(),
        }],
    };

    let state = ActiveTurnSteeringState {
        turn_id: TurnId::new(),
        turn_kind: TurnKind::Regular,
        pending_inputs: VecDeque::from([first.clone(), second.clone()]),
    };

    assert_eq!(state.pending_inputs[0], first);
    assert_eq!(state.pending_inputs[1], second);
}

#[test]
fn session_projection_maps_core_record() {
    let projection = DefaultProjection;
    let session = SessionRecord {
        id: SessionId::new(),
        rollout_path: "rollout.jsonl".into(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        source: "api".into(),
        agent_nickname: None,
        agent_role: None,
        agent_path: None,
        model_provider: "anthropic".into(),
        model: Some("claude-sonnet".into()),
        thinking: None,
        cwd: ".".into(),
        cli_version: "0.1.0".into(),
        title: Some("Test".into()),
        title_state: SessionTitleState::Final(SessionTitleFinalSource::ExplicitCreate),
        sandbox_policy: "workspace-write".into(),
        approval_mode: "never".into(),
        tokens_used: 0,
        first_user_message: None,
        archived_at: None,
        git_sha: None,
        git_branch: None,
        git_origin_url: None,
        parent_session_id: None,
        session_context: None,
        latest_turn_context: None,
        schema_version: 2,
    };

    let projected = projection.project_session(&session, false, SessionRuntimeStatus::Idle);
    assert_eq!(projected.session_id, session.id);
    assert_eq!(projected.model, session.model);
}

#[test]
fn turn_projection_preserves_turn_status_vocabulary() {
    let projection = DefaultProjection;
    let turn = TurnRecord {
        id: TurnId::new(),
        session_id: SessionId::new(),
        sequence: 1,
        started_at: Utc::now(),
        completed_at: None,
        status: TurnStatus::Running,
        kind: devo_core::TurnKind::Regular,
        model: "claude-sonnet".into(),
        thinking: None,
        request_model: "claude-sonnet".into(),
        request_thinking: None,
        input_token_estimate: None,
        usage: None,
        session_context: None,
        turn_context: None,
        schema_version: 2,
    };

    let projected = projection.project_turn(&turn);
    assert_eq!(projected.status, TurnStatus::Running);
}

#[test]
fn event_enum_carries_delta_kind() {
    let event = ServerEvent::ItemDelta {
        delta_kind: ItemDeltaKind::AgentMessageDelta,
        payload: ItemDeltaPayload {
            context: EventContext {
                session_id: SessionId::new(),
                turn_id: Some(TurnId::new()),
                item_id: Some(ItemId::new()),
                seq: 5,
            },
            delta: "hi".into(),
            stream_index: None,
            channel: None,
        },
    };

    let json = serde_json::to_string(&event).expect("serialize");
    assert!(json.contains("agent_message_delta"));
}

#[test]
fn request_envelope_keeps_method_and_id() {
    let request = ClientRequest {
        id: serde_json::json!(1),
        method: "session/start".into(),
        params: serde_json::json!({"cwd":"C:/repo"}),
    };

    let json = serde_json::to_string(&request).expect("serialize");
    assert!(json.contains("\"method\":\"session/start\""));
    assert!(json.contains("\"id\":1"));
}

#[test]
fn session_title_update_params_roundtrip() {
    let params = SessionTitleUpdateParams {
        session_id: SessionId::new(),
        title: "Renamed session".into(),
    };

    let json = serde_json::to_string(&params).expect("serialize");
    let restored: SessionTitleUpdateParams = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(params, restored);
}

#[test]
fn session_title_updated_event_serializes_expected_kind() {
    let event = ServerEvent::SessionTitleUpdated(devo_server::SessionEventPayload {
        session: SessionMetadata {
            session_id: SessionId::new(),
            cwd: ".".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            title: Some("Renamed session".into()),
            title_state: SessionTitleState::Final(SessionTitleFinalSource::UserRename),
            ephemeral: false,
            model: Some("claude-sonnet".into()),
            thinking: None,
            total_input_tokens: 0,
            total_output_tokens: 0,
            prompt_token_estimate: 0,
            status: SessionRuntimeStatus::Idle,
        },
    });

    let json = serde_json::to_string(&event).expect("serialize");
    assert!(json.contains("session_title_updated"));
}

#[test]
fn session_compaction_events_serialize_expected_kinds() {
    let metadata = SessionMetadata {
        session_id: SessionId::new(),
        cwd: ".".into(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        title: Some("Compacting session".into()),
        title_state: SessionTitleState::Unset,
        ephemeral: false,
        model: Some("claude-sonnet".into()),
        thinking: None,
        total_input_tokens: 0,
        total_output_tokens: 0,
        prompt_token_estimate: 0,
        status: SessionRuntimeStatus::Idle,
    };

    let started = ServerEvent::SessionCompactionStarted(devo_server::SessionEventPayload {
        session: metadata.clone(),
    });
    let completed = ServerEvent::SessionCompactionCompleted(devo_server::SessionEventPayload {
        session: metadata,
    });
    let failed =
        ServerEvent::SessionCompactionFailed(devo_server::SessionCompactionFailedPayload {
            session_id: SessionId::new(),
            message: "boom".into(),
        });

    assert!(
        serde_json::to_string(&started)
            .expect("serialize")
            .contains("session_compaction_started")
    );
    assert!(
        serde_json::to_string(&completed)
            .expect("serialize")
            .contains("session_compaction_completed")
    );
    assert!(
        serde_json::to_string(&failed)
            .expect("serialize")
            .contains("session_compaction_failed")
    );
}
