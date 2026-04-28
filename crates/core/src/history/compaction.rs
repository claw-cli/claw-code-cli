//! Compaction — summarise conversation history via a separate LLM call
//! when the token budget is exceeded.
//!
//! Two compaction modes:
//!
//! * **Auto** — triggered automatically when the context window is reached.
//!   The operation is skipped if the history is already within budget.
//! * **Proactive** — explicitly requested by the user (e.g. via a `/compact`
//!   command). The compaction always runs, regardless of the current budget.
//!
//! The compaction flow:
//!
//! 1. Filter out `Reason` items (reasoning text is not useful for summaries).
//! 2. Separate items into a "to‑compact" (old) and "to‑preserve" (recent) set
//!    based on a user‑message token budget.
//! 3. Call the summarizer LLM with the `prompt.md` template.
//! 4. Wrap the returned summary with the `summary_prefix.md` template.
//! 5. Build a new history: `[summary_msg, …preserved_items]`.
//! 6. If the summarizer LLM call fails with a context‑length error, move the
//!    newest to‑compact item back into the preserve set and retry.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde::Serialize;
use tokio::time::sleep;

use devo_protocol::Message;
use devo_protocol::RequestContent;
use devo_protocol::RequestMessage;
use devo_protocol::Role;

use crate::context::TokenBudget;
use crate::response_item::ResponseItem;

use super::normalize;
use super::ContextView;
use super::History;
use super::TokenInfo;

const SUMMARIZATION_PROMPT: &str = include_str!("../../prompts/compact/prompt.md");
const SUMMARY_PREFIX: &str = include_str!("../../prompts/compact/summary_prefix.md");
const COMPACT_USER_MESSAGE_MAX_TOKENS: usize = 20_000;

// ---------------------------------------------------------------------------
// CompactionError
// ---------------------------------------------------------------------------

/// Errors that can occur during history compaction.
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
pub enum CompactionError {
    /// The summarizer provider call failed.
    #[error("summarization failed: {message}")]
    SummarizationFailed {
        /// Human-readable failure description.
        message: String,
    },
    /// The summarizer's context window was exceeded by the input.
    #[error("summarizer context window exceeded")]
    ContextTooLong,
    /// The summarizer returned an empty response.
    #[error("summarizer returned empty response")]
    EmptyResponse,
    /// Compaction is not possible after exhausting retries.
    #[error("compaction not possible after {retries} retries")]
    NotPossible {
        /// Number of retries attempted.
        retries: u32,
    },
}

// ---------------------------------------------------------------------------
// HistorySummarizer trait
// ---------------------------------------------------------------------------

/// Pluggable interface for the LLM call that produces a compaction summary.
///
/// Implementations are provided by the caller (e.g. the query loop) so that
/// this module does not depend directly on a specific provider SDK.
#[async_trait]
pub trait HistorySummarizer: Send + Sync {
    /// Send `messages` (system prompt followed by the to‑compact history)
    /// to the model and return the generated summary text.
    async fn summarize(
        &self,
        messages: Vec<RequestMessage>,
    ) -> Result<String, CompactionError>;
}

// ---------------------------------------------------------------------------
// CompactionConfig
// ---------------------------------------------------------------------------

/// Configuration for the compaction process.
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// Token budget used to decide whether compaction is needed.
    pub budget: TokenBudget,
    /// How compaction was triggered — automatic or proactive.
    pub kind: CompactionKind,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            budget: TokenBudget::default(),
            kind: CompactionKind::Auto,
        }
    }
}

// ---------------------------------------------------------------------------
// CompactionKind — how compaction was triggered
// ---------------------------------------------------------------------------

/// Whether compaction was triggered automatically or proactively by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionKind {
    /// Automatic compaction triggered when the context window is reached.
    /// Skips if the history is already within budget.
    Auto,
    /// Proactive compaction explicitly requested by the user.
    /// Always runs regardless of the current token budget.
    Proactive,
}

// ---------------------------------------------------------------------------
// CompactAction — describes how to act on a compaction decision
// ---------------------------------------------------------------------------

/// Describes the outcome of a single compaction attempt.
#[derive(Debug)]
pub enum CompactAction {
    /// Compaction succeeded, yielding a new history.
    Replaced(Box<History>),
    /// Compaction was not needed (history is within budget).
    Skipped,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Determine whether compaction should run for the given token info.
pub fn should_compact(token_info: &TokenInfo, budget: &TokenBudget) -> bool {
    if token_info.input_tokens == 0 && token_info.cached_input_tokens == 0 {
        return false;
    }
    let current = token_info
        .input_tokens
        .saturating_add(token_info.cached_input_tokens)
        .saturating_add(token_info.output_tokens);
    budget.should_compact(current)
}

/// Compact the history using an LLM-backed summarizer.
pub async fn compact_history(
    items: &[ResponseItem],
    token_info: &TokenInfo,
    context: &ContextView,
    summarizer: &dyn HistorySummarizer,
    config: &CompactionConfig,
) -> Result<CompactAction, CompactionError> {
    // For auto compaction, skip if already within budget.
    // Proactive compaction always proceeds regardless of budget.
    if config.kind == CompactionKind::Auto && !should_compact(token_info, &config.budget) {
        return Ok(CompactAction::Skipped);
    }

    // 1. Filter out Reason items.
    let filtered = normalize::filter_reason(items);

    // 2. Split into "to compact" (old) and "to preserve" (recent).
    let (mut to_compact, mut preserved) =
        split_by_user_message_budget(&filtered, COMPACT_USER_MESSAGE_MAX_TOKENS);

    if to_compact.is_empty() {
        // Nothing to compact — everything is within the preserve budget.
        return Ok(CompactAction::Skipped);
    }

    // 3. Attempt compaction with retry.
    //
    //    * The summarizer LLM may fail with `ContextTooLong` when the
    //      formatted history text exceeds its context window.  In that case
    //      we move the newest to‑compact item into the preserve set
    //      (reducing what the summarizer has to process) and retry
    //      immediately — this always converges because `to_compact`
    //      shrinks with each iteration.
    //    * Other errors (network blips, rate limits) are retried with
    //      exponential backoff up to 5 attempts.
    let mut transient_retries = 0u32;
    const MAX_TRANSIENT_RETRIES: u32 = 5;

    loop {
        let mut messages = Vec::with_capacity(to_compact.len() + 1);
        messages.push(RequestMessage {
            role: "system".to_string(),
            content: vec![RequestContent::Text {
                text: SUMMARIZATION_PROMPT.trim().to_string(),
            }],
        });
        for item in &to_compact {
            messages.push(RequestMessage::from(item));
        }

        let summary = match summarizer.summarize(messages).await {
            Ok(s) => s,
            Err(CompactionError::ContextTooLong) => {
                if to_compact.is_empty() {
                    // All items were moved to preserve — nothing to compact.
                    return Ok(CompactAction::Skipped);
                }
                // Move the newest to‑compact item into the preserve set
                // so the summarizer receives less input on the next try.
                let last = to_compact.pop().unwrap();
                preserved.insert(0, last);
                continue;
            }
            Err(e) => {
                transient_retries += 1;
                if transient_retries >= MAX_TRANSIENT_RETRIES {
                    return Err(e);
                }
                // Exponential backoff: 2^(retries) * 100ms
                let delay = Duration::from_millis(100 * (1 << transient_retries));
                sleep(delay).await;
                continue;
            }
        };

        if summary.trim().is_empty() {
            return Err(CompactionError::EmptyResponse);
        }

        // Wrap with the summary prefix.
        let prefixed_summary = if SUMMARY_PREFIX.is_empty() {
            summary.trim().to_string()
        } else {
            format!("{}\n{}", SUMMARY_PREFIX.trim(), summary.trim())
        };

        // Build the compacted history: [summary_msg, …preserved_items].
        let mut compacted_items = Vec::new();
        compacted_items.push(ResponseItem::Message(Message {
            role: Role::Assistant,
            content: vec![devo_protocol::ContentBlock::Text {
                text: prefixed_summary,
            }],
        }));
        compacted_items.extend(preserved);

        let mut history = History::new(context.clone());
        for item in compacted_items {
            history.push(item);
        }
        // Preserve the original token info as an estimate.
        history.token_info = token_info.clone();

        return Ok(CompactAction::Replaced(Box::new(history)));
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Splits items into a "to compact" prefix and a "to preserve" suffix.
///
/// The split is based on a token budget for preserving the most recent user
/// messages. The function walks backward from the end, accumulating user
/// message tokens until the budget is exhausted, and everything before that
/// point is marked for compaction.
/// Splits items into a "to compact" prefix and a "to preserve" suffix.
///
/// Walks backward from the end, accumulating item token estimates until the
/// budget is exhausted. Everything before the split point is marked for
/// compaction; everything from the split point onward is preserved.
/// At least one item is always preserved (the last item) when items exist,
/// regardless of the budget.
fn split_by_user_message_budget(
    items: &[ResponseItem],
    budget_tokens: usize,
) -> (Vec<ResponseItem>, Vec<ResponseItem>) {
    if items.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let mut used_tokens: usize = 0;
    let mut preserve_from: usize = 0;

    for i in (0..items.len()).rev() {
        let item_tokens = estimate_item_tokens(&items[i]);

        if used_tokens + item_tokens > budget_tokens && i + 1 < items.len() {
            // Budget exhausted — split after this item.
            // Only split if there would still be something to preserve.
            preserve_from = i + 1;
            break;
        }

        used_tokens += item_tokens;
        preserve_from = i;
    }

    if preserve_from == 0 {
        // Everything fits in the preserve budget (or the budget is very
        // large) — nothing to compact.
        (Vec::new(), items.to_vec())
    } else {
        let compact = items[..preserve_from].to_vec();
        let preserve = items[preserve_from..].to_vec();
        (compact, preserve)
    }
}

/// Estimates the byte-length-based token count for a single item.
fn estimate_item_tokens(item: &ResponseItem) -> usize {
    let text = match item {
        ResponseItem::Reason { text } => text.clone(),
        ResponseItem::Message(msg) => msg
            .content
            .iter()
            .filter_map(|block| match block {
                devo_protocol::ContentBlock::Text { text } => Some(text.clone()),
                devo_protocol::ContentBlock::Reasoning { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        ResponseItem::ToolCall { name, input, .. } => {
            format!("{}: {}", name, input)
        }
        ResponseItem::ToolCallOutput { content, .. } => content.clone(),
    };
    // Rough estimate: ~4 bytes per token.
    text.len().div_ceil(4)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::context::TokenBudget;
    use crate::response_item::ResponseItem;

    #[test]
    fn should_compact_false_when_no_tokens() {
        let info = TokenInfo::default();
        let budget = TokenBudget::new(200_000, 8192);
        assert!(!should_compact(&info, &budget));
    }

    #[test]
    fn split_by_user_message_budget_all_preserved() {
        let items = vec![
            ResponseItem::Message(Message::user("short")),
            ResponseItem::Message(Message::assistant_text("ok")),
        ];
        let (compact, preserve) = split_by_user_message_budget(&items, 10_000);
        assert!(compact.is_empty());
        assert_eq!(preserve.len(), 2);
    }

    #[test]
    fn split_by_user_message_budget_boundary() {
        let items = vec![
            ResponseItem::Message(Message::user("a".repeat(400))), // ~100 tokens
            ResponseItem::Message(Message::assistant_text("b".repeat(400))),
            ResponseItem::Message(Message::user("c".repeat(400))),
            ResponseItem::Message(Message::assistant_text("d".repeat(400))),
        ];

        // Budget enough for about 2 items.
        let (compact, preserve) = split_by_user_message_budget(&items, 200);
        assert!(!preserve.is_empty());

        // The preserved part should be the tail.
        assert_eq!(preserve.len() + compact.len(), items.len());
    }

    #[test]
    fn compact_action_debug() {
        let action = CompactAction::Skipped;
        assert_eq!(format!("{:?}", action), "Skipped");
    }

    #[test]
    fn estimate_item_tokens_for_different_variants() {
        let reason = ResponseItem::Reason {
            text: "thinking deeply".into(),
        };
        assert!(estimate_item_tokens(&reason) > 0);

        let msg = ResponseItem::Message(Message::user("hello world"));
        assert!(estimate_item_tokens(&msg) > 0);

        let tc = ResponseItem::ToolCall {
            id: "tc-1".into(),
            name: "bash".into(),
            input: serde_json::json!({"cmd": "ls"}),
        };
        assert!(estimate_item_tokens(&tc) > 0);

        let tco = ResponseItem::ToolCallOutput {
            tool_use_id: "tc-1".into(),
            content: "done".into(),
            is_error: false,
        };
        assert!(estimate_item_tokens(&tco) > 0);
    }
}
