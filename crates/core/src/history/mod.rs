pub mod compaction;
pub mod normalize;
pub mod summarizer;

use serde::{Deserialize, Serialize};

use devo_protocol::{InputModality, Message, RequestContent, RequestMessage, Role, UserInput};

use crate::context::ContextualUserFragment;
use crate::response_item::ResponseItem;

// ---------------------------------------------------------------------------
// TokenInfo
// ---------------------------------------------------------------------------

/// Token usage information for the history.
///
/// Stores the token counts as reported by the LLM provider. The design is
/// provider-agnostic and covers the common fields supported by OpenAI chat
/// completions, OpenAI responses, and Anthropic messages APIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TokenInfo {
    /// Total input (prompt) tokens consumed.
    pub input_tokens: usize,
    /// Input tokens served from a cache, when reported by the provider.
    pub cached_input_tokens: usize,
    /// Total output (completion) tokens generated.
    pub output_tokens: usize,
}

impl TokenInfo {
    /// Returns the sum of all tracked tokens.
    pub fn total(&self) -> usize {
        self.input_tokens
            .saturating_add(self.cached_input_tokens)
            .saturating_add(self.output_tokens)
    }

    /// Accumulates another `TokenInfo` into this one.
    pub fn accumulate(&mut self, other: &TokenInfo) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.cached_input_tokens = self
            .cached_input_tokens
            .saturating_add(other.cached_input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
    }
}

// ---------------------------------------------------------------------------
// ContextView
// ---------------------------------------------------------------------------

/// Snapshot of the environment and model context at a point in time.
///
/// Used to detect context changes and produce a "diff prompt" so the LLM
/// can be informed about what has changed since its last view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextView {
    /// Operating system identifier (e.g. "windows", "linux", "macos").
    pub os: String,
    /// Shell name (e.g. "bash", "zsh", "powershell").
    pub shell: String,
    /// IANA timezone identifier (e.g. "Asia/Shanghai", "America/New_York").
    pub timezone: String,
    /// Active model slug.
    pub model: String,
    /// Current thinking / reasoning effort selection, if any.
    pub thinking_effort: Option<String>,
    /// Active persona or system persona identifier, if any.
    pub persona: Option<String>,
    /// Current date in ISO-8601 format (YYYY-MM-DD).
    pub date: String,
    /// Current working directory.
    pub cwd: String,
}

impl ContextView {
    /// Creates a new `ContextView` from the supplied parameters.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        os: impl Into<String>,
        shell: impl Into<String>,
        timezone: impl Into<String>,
        model: impl Into<String>,
        thinking_effort: Option<String>,
        persona: Option<String>,
        date: impl Into<String>,
        cwd: impl Into<String>,
    ) -> Self {
        Self {
            os: os.into(),
            shell: shell.into(),
            timezone: timezone.into(),
            model: model.into(),
            thinking_effort,
            persona,
            date: date.into(),
            cwd: cwd.into(),
        }
    }

    /// Renders the full context as a structured prompt fragment.
    pub fn to_prompt(&self) -> String {
        let mut parts = vec![
            format!("<os>{}</os>", self.os),
            format!("<shell>{}</shell>", self.shell),
            format!("<timezone>{}</timezone>", self.timezone),
            format!("<model>{}</model>", self.model),
            format!("<date>{}</date>", self.date),
            format!("<cwd>{}</cwd>", self.cwd),
        ];
        if let Some(ref effort) = self.thinking_effort {
            parts.push(format!("<thinking_effort>{effort}</thinking_effort>"));
        }
        if let Some(ref persona) = self.persona {
            parts.push(format!("<persona>{persona}</persona>"));
        }
        parts.join("\n")
    }

    /// Produces a diff prompt describing what has changed since `other`.
    ///
    /// When the context has changed (e.g. the user switched model or working
    /// directory), this returns a structured message that can be injected
    /// into the prompt to inform the LLM.
    pub fn diff_since(&self, previous: &ContextView) -> Option<String> {
        let mut changes = Vec::new();

        if self.os != previous.os {
            changes.push(format!("os: {} -> {}", previous.os, self.os));
        }
        if self.shell != previous.shell {
            changes.push(format!("shell: {} -> {}", previous.shell, self.shell));
        }
        if self.timezone != previous.timezone {
            changes.push(format!(
                "timezone: {} -> {}",
                previous.timezone, self.timezone
            ));
        }
        if self.model != previous.model {
            changes.push(format!("model: {} -> {}", previous.model, self.model));
        }
        if self.thinking_effort != previous.thinking_effort {
            changes.push(format!(
                "thinking_effort: {:?} -> {:?}",
                previous.thinking_effort, self.thinking_effort
            ));
        }
        if self.persona != previous.persona {
            changes.push(format!(
                "persona: {:?} -> {:?}",
                previous.persona, self.persona
            ));
        }
        if self.date != previous.date {
            changes.push(format!("date: {} -> {}", previous.date, self.date));
        }
        if self.cwd != previous.cwd {
            changes.push(format!("cwd: {} -> {}", previous.cwd, self.cwd));
        }

        if changes.is_empty() {
            return None;
        }

        Some(format!(
            "<context_changes>\n{}\n</context_changes>",
            changes.join("\n")
        ))
    }
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// Manages a sequence of `ResponseItem`s together with token usage metadata
/// and environment context.
///
/// Provides utilities for mutation, normalization, and prompt preparation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct History {
    /// The ordered sequence of conversation items.
    pub items: Vec<ResponseItem>,
    /// Aggregate token usage for the history.
    pub token_info: TokenInfo,
    /// The environment and model context snapshot.
    pub context: ContextView,
}

impl History {
    /// Creates a new `History` with the given context.
    pub fn new(context: ContextView) -> Self {
        Self {
            items: Vec::new(),
            token_info: TokenInfo::default(),
            context,
        }
    }

    /// Appends a `ResponseItem` to the end of the history.
    pub fn push(&mut self, item: ResponseItem) {
        self.items.push(item);
    }

    /// Inserts a `ResponseItem` at the given index.
    pub fn insert(&mut self, index: usize, item: ResponseItem) {
        self.items.insert(index, item);
    }

    /// Removes the item at the given index.
    pub fn remove(&mut self, index: usize) -> ResponseItem {
        self.items.remove(index)
    }

    /// Replaces all items in-place with the given sequence.
    ///
    /// Used by compaction to atomically swap the full item list without
    /// constructing a new `History` wrapper.
    pub fn replace_items(&mut self, items: Vec<ResponseItem>) {
        self.items = items;
    }

    /// Returns the number of items in the history.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Returns `true` if the history contains no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Removes the last user message (and its associated reasoning / tool-call
    /// items) from the tail of history.
    ///
    /// This is used when the user "draws back" their last message. Returns
    /// `true` if an item was removed.
    ///
    /// The method walks backward from the end and removes everything that
    /// belongs to the last user-initiated turn: the user message itself,
    /// any preceding reasoning and tool-call items from the assistant turn
    /// that responded to it, and the tool-call outputs that followed.
    /// If the preceding item is a `CompactionSummary` fragment, it is removed
    /// together with the user message.
    pub fn remove_tail_user_message(&mut self) -> bool {
        // Find the last user Message from the end.
        let last_user_pos = self.items.iter().rposition(|item| match item {
            ResponseItem::Message(msg) => msg.role == devo_protocol::Role::User,
            _ => false,
        });

        let Some(start) = last_user_pos else {
            return false;
        };

        // If the preceding item is a compaction-summary fragment, remove it too.
        let truncate_at = if start > 0
            && matches!(&self.items[start - 1], ResponseItem::Message(msg) if msg.content.iter().any(|block| {
                matches!(block, devo_protocol::ContentBlock::Text { text } if crate::context::compaction_summary::CompactionSummary::matches_text(text))
            })) {
            start - 1
        } else {
            start
        };

        self.items.truncate(truncate_at);
        true
    }

    /// Prepares the history for an LLM call by:
    ///
    /// 1. Normalizing tool-call / tool-call-output pairing
    /// 2. Filtering items according to the model's supported modalities
    /// 3. Converting to `Vec<RequestMessage>`
    pub fn for_prompt(&self, modalities: &[InputModality]) -> Vec<RequestMessage> {
        let mut items = normalize::filter_by_modality(&self.items, modalities);
        normalize::pair_tool_call_items(&mut items);
        items
            .iter()
            .map(|item| {
                let msg: RequestMessage = item.into();
                msg
            })
            .collect()
    }

    /// Updates the context view and produces a diff prompt if anything changed.
    pub fn update_context(&mut self, new_context: ContextView) -> Option<String> {
        let diff = self.context.diff_since(&new_context);
        self.context = new_context;
        diff
    }

    /// Builds prompt-visible request messages by prepending locked session
    /// inputs and normalizing the existing history items.
    pub fn for_prompt_with_prefix(
        &self,
        prefix_user_inputs: &[UserInput],
        modalities: &[InputModality],
    ) -> Vec<RequestMessage> {
        let mut messages = self.for_prompt(modalities);
        prepend_user_inputs(&mut messages, prefix_user_inputs);
        messages
    }
}

/// Inserts one context-diff message immediately before the most recent user
/// message so that turn-local config changes are scoped to the next request.
pub fn insert_context_diff_message(messages: &mut Vec<Message>, diff: Message) {
    let insert_at = messages
        .iter()
        .rposition(|message| message.role == Role::User)
        .unwrap_or(messages.len());
    messages.insert(insert_at, diff);
}

/// Converts locked prefix `UserInput`s into request messages and prepends them
/// ahead of the existing prompt-visible history.
pub fn prepend_user_inputs(messages: &mut Vec<RequestMessage>, user_inputs: &[UserInput]) {
    messages.splice(
        0..0,
        user_inputs.iter().filter_map(|input| match input {
            UserInput::Text { text, .. } if !text.trim().is_empty() => Some(RequestMessage {
                role: Role::User.as_str().to_string(),
                content: vec![RequestContent::Text { text: text.clone() }],
            }),
            UserInput::Text { .. }
            | UserInput::Image { .. }
            | UserInput::LocalImage { .. }
            | UserInput::Skill { .. }
            | UserInput::Mention { .. }
            | _ => None,
        }),
    );
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::response_item::ResponseItem;
    use devo_protocol::{ContentBlock, Message, Role};

    fn test_context() -> ContextView {
        ContextView::new(
            "linux",
            "bash",
            "UTC",
            "test-model",
            None,
            None,
            "2026-04-27",
            "/home/test",
        )
    }

    #[test]
    fn history_new_is_empty() {
        let h = History::new(test_context());
        assert!(h.is_empty());
        assert_eq!(h.len(), 0);
    }

    #[test]
    fn history_push_and_len() {
        let mut h = History::new(test_context());
        h.push(ResponseItem::Message(Message::user("hello")));
        assert_eq!(h.len(), 1);
        assert!(!h.is_empty());
    }

    #[test]
    fn history_remove_tail_user_message() {
        let mut h = History::new(test_context());
        h.push(ResponseItem::Message(Message::user("hello")));
        h.push(ResponseItem::Message(Message::assistant_text("world")));
        h.push(ResponseItem::ToolCall {
            id: "tc-1".into(),
            name: "bash".into(),
            input: serde_json::json!({"cmd": "ls"}),
        });
        h.push(ResponseItem::ToolCallOutput {
            tool_use_id: "tc-1".into(),
            content: "ok".into(),
            is_error: false,
        });

        assert!(h.remove_tail_user_message());
        assert_eq!(h.len(), 0);
    }

    #[test]
    fn history_remove_tail_user_message_no_user_message() {
        let mut h = History::new(test_context());
        h.push(ResponseItem::Message(Message::assistant_text("hello")));
        assert!(!h.remove_tail_user_message());
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn history_context_diff_no_changes() {
        let ctx = test_context();
        let diff = ctx.diff_since(&ctx);
        assert!(diff.is_none());
    }

    #[test]
    fn history_context_diff_detects_change() {
        let ctx1 = test_context();
        let mut ctx2 = test_context();
        ctx2.cwd = "/home/other".into();
        let diff = ctx2.diff_since(&ctx1);
        assert!(diff.is_some());
        let diff_str = diff.unwrap();
        assert!(diff_str.contains("cwd"));
        assert!(diff_str.contains("/home/test"));
        assert!(diff_str.contains("/home/other"));
    }

    #[test]
    fn history_context_to_prompt_contains_fields() {
        let ctx = test_context();
        let prompt = ctx.to_prompt();
        assert!(prompt.contains("<os>linux</os>"));
        assert!(prompt.contains("<shell>bash</shell>"));
        assert!(prompt.contains("<date>2026-04-27</date>"));
    }

    #[test]
    fn history_for_prompt_respects_modalities() {
        let mut h = History::new(test_context());
        h.push(ResponseItem::Message(Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
        }));
        h.push(ResponseItem::Message(Message::assistant_text("hi")));

        let msgs = h.for_prompt(&[InputModality::Text]);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn history_for_prompt_with_prefix_prepends_user_inputs() {
        let mut h = History::new(test_context());
        h.push(ResponseItem::Message(Message::user("hello")));

        let msgs = h.for_prompt_with_prefix(
            &[UserInput::Text {
                text: "<environment_context>locked</environment_context>".into(),
                text_elements: Vec::new(),
            }],
            &[InputModality::Text],
        );

        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        let RequestContent::Text { text } = &msgs[0].content[0] else {
            panic!("expected text prefix");
        };
        assert!(text.contains("environment_context"));
    }

    #[test]
    fn insert_context_diff_message_places_diff_before_latest_user_message() {
        let mut messages = vec![
            Message::user("first"),
            Message::assistant_text("reply"),
            Message::user("second"),
        ];

        insert_context_diff_message(
            &mut messages,
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "<context_changes>\nmodel: a -> b\n</context_changes>".into(),
                }],
            },
        );

        assert_eq!(messages.len(), 4);
        let ContentBlock::Text { text } = &messages[2].content[0] else {
            panic!("expected diff text");
        };
        assert!(text.contains("<context_changes>"));
        assert_eq!(messages[3], Message::user("second"));
    }

    #[test]
    fn token_info_default() {
        let info = TokenInfo::default();
        assert_eq!(info.input_tokens, 0);
        assert_eq!(info.cached_input_tokens, 0);
        assert_eq!(info.output_tokens, 0);
        assert_eq!(info.total(), 0);
    }

    #[test]
    fn token_info_accumulate() {
        let mut info = TokenInfo {
            input_tokens: 100,
            cached_input_tokens: 10,
            output_tokens: 50,
        };
        info.accumulate(&TokenInfo {
            input_tokens: 50,
            cached_input_tokens: 5,
            output_tokens: 25,
        });
        assert_eq!(info.input_tokens, 150);
        assert_eq!(info.cached_input_tokens, 15);
        assert_eq!(info.output_tokens, 75);
    }

    #[test]
    fn token_info_total() {
        let info = TokenInfo {
            input_tokens: 100,
            cached_input_tokens: 20,
            output_tokens: 50,
        };
        assert_eq!(info.total(), 170);
    }

    #[test]
    fn remove_tail_multiple_turns() {
        let mut h = History::new(test_context());
        // Turn 1
        h.push(ResponseItem::Message(Message::user("first")));
        h.push(ResponseItem::Message(Message::assistant_text("reply1")));
        // Turn 2
        h.push(ResponseItem::Message(Message::user("second")));
        h.push(ResponseItem::Message(Message::assistant_text("reply2")));

        assert!(h.remove_tail_user_message());
        assert_eq!(h.len(), 2);
        // Only Turn 1 remains
        if let Some(item) = h.items.first() {
            match item {
                ResponseItem::Message(msg) => {
                    assert_eq!(msg.role, Role::User);
                }
                _ => panic!("expected user message"),
            }
        }
    }
}
