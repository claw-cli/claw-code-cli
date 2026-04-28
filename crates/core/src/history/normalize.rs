//! Normalization utilities for `ResponseItem` sequences.
//!
//! This module provides:
//! - ToolCall / ToolCallOutput pairing: when an item is removed, its
//!   counterpart is also removed.
//! - Modality-based filtering: items whose content types are not supported
//!   by the model are removed.
//! - Reason-item stripping: `Reason` items can be removed before compaction.

use devo_protocol::InputModality;

use crate::response_item::ResponseItem;

/// Removes items at `index` and also removes the paired counterpart
/// (ToolCall ↔ ToolCallOutput) if one exists.
///
/// When the removed item is a `ToolCall`, the corresponding
/// `ToolCallOutput` (matched by `id` ↔ `tool_use_id`) is also
/// removed. Similarly, when the removed item is a `ToolCallOutput`,
/// the corresponding `ToolCall` is removed.
///
/// Returns the removed item(s). The primary item is always first;
/// the paired item, if found and removed, is second.
pub fn remove_paired(items: &mut Vec<ResponseItem>, index: usize) -> Vec<ResponseItem> {
    if index >= items.len() {
        return Vec::new();
    }

    let removed = items.remove(index);

    // Try to find and remove the paired item.
    let paired_idx = find_pair_index(items, &removed);

    let mut result = vec![removed];
    if let Some(pidx) = paired_idx {
        result.push(items.remove(pidx));
    }

    result
}

/// Finds the index of the paired item for the given removed item.
fn find_pair_index(items: &[ResponseItem], removed: &ResponseItem) -> Option<usize> {
    match removed {
        ResponseItem::ToolCall { id, .. } => items.iter().position(|item| match item {
            ResponseItem::ToolCallOutput { tool_use_id, .. } => tool_use_id == id,
            _ => false,
        }),
        ResponseItem::ToolCallOutput { tool_use_id, .. } => {
            items.iter().position(|item| match item {
                ResponseItem::ToolCall { id, .. } => id == tool_use_id,
                _ => false,
            })
        }
        _ => None,
    }
}

/// Filters a slice of `ResponseItem`s, keeping only content types that match
/// the model's supported modalities.
///
/// For `Message` items, content blocks whose types are not in the supported
/// modalities are removed. If a `Message` becomes empty after filtering, the
/// entire message is removed.
///
/// Currently supported modalities:
/// - `InputModality::Text` — keeps `Text` content blocks
/// - `InputModality::Image` — keeps `Image` content blocks (when added)
///
/// `Reason`, `ToolCall`, and `ToolCallOutput` items are always preserved
/// regardless of modality.
pub fn filter_by_modality(
    items: &[ResponseItem],
    modalities: &[InputModality],
) -> Vec<ResponseItem> {
    let supports_text = modalities.contains(&InputModality::Text);
    let _supports_image = modalities.contains(&InputModality::Image);

    items
        .iter()
        .filter_map(|item| match item {
            ResponseItem::Message(msg) => {
                // Filter content blocks within the message based on modality.
                let filtered_content: Vec<_> = msg
                    .content
                    .iter()
                    .filter(|block| match block {
                        devo_protocol::ContentBlock::Text { .. } => supports_text,
                        devo_protocol::ContentBlock::Reasoning { .. } => supports_text,
                        devo_protocol::ContentBlock::ToolUse { .. } => true,
                        devo_protocol::ContentBlock::ToolResult { .. } => true,
                    })
                    .cloned()
                    .collect();

                if filtered_content.is_empty() {
                    None // Remove empty messages
                } else {
                    Some(ResponseItem::Message(devo_protocol::Message {
                        role: msg.role,
                        content: filtered_content,
                    }))
                }
            }
            // Non-message items are preserved as-is.
            other => Some(other.clone()),
        })
        .collect()
}

/// Removes all `Reason` items from the slice and returns the filtered vector.
///
/// This is used before compaction to strip reasoning text that is not
/// useful for the summary.
pub fn filter_reason(items: &[ResponseItem]) -> Vec<ResponseItem> {
    items
        .iter()
        .filter(|item| !item.is_reason())
        .cloned()
        .collect()
}

/// Ensures tool-call / tool-call-output items are properly paired.
///
/// Any `ToolCall` without a matching `ToolCallOutput` (and vice versa)
/// is removed from the sequence. This operates on a **mutable** slice
/// since it is typically called before prompt building.
pub fn pair_tool_call_items(items: &mut Vec<ResponseItem>) {
    let mut i = 0;
    while i < items.len() {
        let remove = match &items[i] {
            ResponseItem::ToolCall { id, .. } => {
                // Check if there is a matching ToolCallOutput anywhere
                !items.iter().any(|item| match item {
                    ResponseItem::ToolCallOutput { tool_use_id, .. } => tool_use_id == id,
                    _ => false,
                })
            }
            ResponseItem::ToolCallOutput { tool_use_id, .. } => {
                // Check if there is a matching ToolCall anywhere
                !items.iter().any(|item| match item {
                    ResponseItem::ToolCall { id, .. } => id == tool_use_id,
                    _ => false,
                })
            }
            _ => false,
        };

        if remove {
            items.remove(i);
        } else {
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::response_item::ResponseItem;
    use devo_protocol::Message;

    #[test]
    fn remove_paired_removes_tool_call_and_output() {
        let mut items = vec![
            ResponseItem::ToolCall {
                id: "tc-1".into(),
                name: "bash".into(),
                input: serde_json::json!({"cmd": "ls"}),
            },
            ResponseItem::Message(Message::user("hello")),
            ResponseItem::ToolCallOutput {
                tool_use_id: "tc-1".into(),
                content: "ok".into(),
                is_error: false,
            },
        ];

        let removed = remove_paired(&mut items, 0);
        assert_eq!(removed.len(), 2);
        assert!(removed[0].is_tool_call());
        assert!(removed[1].is_tool_call_output());
        assert_eq!(items.len(), 1);
        assert!(items[0].is_message());
    }

    #[test]
    fn remove_paired_removes_output_and_tool_call() {
        let mut items = vec![
            ResponseItem::Message(Message::user("hello")),
            ResponseItem::ToolCall {
                id: "tc-1".into(),
                name: "bash".into(),
                input: serde_json::json!({"cmd": "ls"}),
            },
            ResponseItem::ToolCallOutput {
                tool_use_id: "tc-1".into(),
                content: "ok".into(),
                is_error: false,
            },
        ];

        let removed = remove_paired(&mut items, 2);
        assert_eq!(removed.len(), 2);
        assert!(removed[0].is_tool_call_output());
        assert!(removed[1].is_tool_call());
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn remove_paired_no_pair_for_message() {
        let mut items = vec![
            ResponseItem::Message(Message::user("hello")),
            ResponseItem::Message(Message::assistant_text("world")),
        ];

        let removed = remove_paired(&mut items, 0);
        assert_eq!(removed.len(), 1);
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn remove_paired_out_of_bounds() {
        let mut items: Vec<ResponseItem> = Vec::new();
        let removed = remove_paired(&mut items, 0);
        assert!(removed.is_empty());
    }

    #[test]
    fn filter_by_modality_keeps_text() {
        let items = vec![
            ResponseItem::Message(Message::user("hello")),
            ResponseItem::Message(Message::assistant_text("world")),
        ];

        let filtered = filter_by_modality(&items, &[InputModality::Text]);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn filter_by_modality_keeps_all_non_message() {
        let items = vec![
            ResponseItem::Reason {
                text: "thinking".into(),
            },
            ResponseItem::ToolCall {
                id: "tc-1".into(),
                name: "bash".into(),
                input: serde_json::json!({"cmd": "ls"}),
            },
        ];

        let filtered = filter_by_modality(&items, &[InputModality::Text]);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn filter_reason_removes_reason_items() {
        let items = vec![
            ResponseItem::Reason {
                text: "thinking".into(),
            },
            ResponseItem::Message(Message::user("hello")),
            ResponseItem::Reason {
                text: "more thinking".into(),
            },
            ResponseItem::Message(Message::assistant_text("world")),
        ];

        let filtered = filter_reason(&items);
        assert_eq!(filtered.len(), 2);
        assert!(filtered[0].is_message());
        assert!(filtered[1].is_message());
    }

    #[test]
    fn pair_tool_call_items_removes_orphan_tool_call() {
        let mut items = vec![
            ResponseItem::ToolCall {
                id: "tc-1".into(),
                name: "bash".into(),
                input: serde_json::json!({"cmd": "ls"}),
            },
            ResponseItem::Message(Message::user("hello")),
        ];

        pair_tool_call_items(&mut items);
        assert_eq!(items.len(), 1);
        assert!(items[0].is_message());
    }

    #[test]
    fn pair_tool_call_items_removes_orphan_output() {
        let mut items = vec![
            ResponseItem::Message(Message::user("hello")),
            ResponseItem::ToolCallOutput {
                tool_use_id: "tc-1".into(),
                content: "ok".into(),
                is_error: false,
            },
        ];

        pair_tool_call_items(&mut items);
        assert_eq!(items.len(), 1);
        assert!(items[0].is_message());
    }

    #[test]
    fn pair_tool_call_items_keeps_paired() {
        let mut items = vec![
            ResponseItem::ToolCall {
                id: "tc-1".into(),
                name: "bash".into(),
                input: serde_json::json!({"cmd": "ls"}),
            },
            ResponseItem::ToolCallOutput {
                tool_use_id: "tc-1".into(),
                content: "ok".into(),
                is_error: false,
            },
        ];

        pair_tool_call_items(&mut items);
        assert_eq!(items.len(), 2);
    }
}
