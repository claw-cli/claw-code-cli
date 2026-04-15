use clawcr_core::{SessionId, TextItem, ToolResultItem, TurnId, TurnItem, TurnStatus};
use tracing::warn;

use super::ServerRuntime;
use crate::persistence::build_item_record_with_output_items;
use crate::projection::history_item_from_turn_item;
use crate::{EventContext, ItemDeltaKind, ItemDeltaPayload, ItemKind, ServerEvent, TurnSummary};

impl ServerRuntime {
    pub(super) async fn emit_plan_update(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        turn: &TurnSummary,
        tool_use_id: &str,
        content: &str,
    ) {
        let (item_id, item_seq) = self
            .start_item(
                session_id,
                turn_id,
                ItemKind::Plan,
                serde_json::json!({
                    "title": "Plan",
                    "text": "",
                    "tool_use_id": tool_use_id,
                }),
            )
            .await;
        self.broadcast_event(ServerEvent::ItemDelta {
            delta_kind: ItemDeltaKind::PlanDelta,
            payload: ItemDeltaPayload {
                context: EventContext {
                    session_id,
                    turn_id: Some(turn_id),
                    item_id: Some(item_id),
                    seq: 0,
                },
                delta: content.to_string(),
                stream_index: None,
                channel: None,
            },
        })
        .await;
        let plan_item = TurnItem::Plan(TextItem {
            text: content.to_string(),
        });
        let tool_result_item = TurnItem::ToolResult(ToolResultItem {
            tool_call_id: tool_use_id.to_string(),
            output: serde_json::Value::String(content.to_string()),
            is_error: false,
        });
        if let Some(session_arc) = self.sessions.lock().await.get(&session_id).cloned() {
            let record = {
                let mut session = session_arc.lock().await;
                if let Some(history_item) = history_item_from_turn_item(&plan_item) {
                    session.history_items.push(history_item);
                }
                session.record.clone()
            };
            if let Some(record) = record {
                let item = build_item_record_with_output_items(
                    session_id,
                    turn_id,
                    item_id,
                    item_seq,
                    vec![tool_result_item, plan_item.clone()],
                    Some(TurnStatus::Running),
                    None,
                );
                if let Err(error) = self.rollout_store.append_item(&record, item) {
                    warn!(session_id = %session_id, error = %error, "failed to persist item line");
                }
            }
        }
        self.emit_item_completed(
            session_id,
            turn_id,
            item_id,
            ItemKind::Plan,
            serde_json::json!({ "title": "Plan", "text": content, "tool_use_id": tool_use_id }),
        )
        .await;
        let current_turn = self.current_turn_summary(session_id, turn_id, turn).await;
        self.broadcast_event(ServerEvent::TurnPlanUpdated(crate::TurnEventPayload {
            session_id,
            turn: current_turn,
        }))
        .await;
    }
}
