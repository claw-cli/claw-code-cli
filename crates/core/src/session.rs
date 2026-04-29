use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use devo_safety::legacy_permissions::PermissionMode;

use devo_protocol::{PendingInputItem, TurnKind};

use crate::state::turn::TurnState;
use crate::{AgentsMdConfig, Message, Model, SessionContext, TokenBudget, TurnContext};

/// Configuration for a session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub token_budget: TokenBudget,
    pub permission_mode: PermissionMode,
    pub agents_md: AgentsMdConfig,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            token_budget: TokenBudget::default(),
            permission_mode: PermissionMode::AutoApprove,
            agents_md: AgentsMdConfig::default(),
        }
    }
}

/// Per-turn execution settings resolved before the query loop starts.
#[derive(Debug, Clone)]
pub struct TurnConfig {
    pub model: Model,
    pub thinking_selection: Option<String>,
}

/// Mutable state for one conversation session.
///
/// This corresponds to the session-level state in Claude Code's
/// `AppStateStore` and `QueryEngine`, but stripped of UI concerns.
pub struct SessionState {
    pub id: String,
    pub config: SessionConfig,
    pub messages: Vec<Message>,
    pub prompt_messages: Option<Vec<Message>>,
    pub session_context: Option<SessionContext>,
    pub latest_turn_context: Option<TurnContext>,
    pub cwd: PathBuf,
    pub turn_count: usize,
    pub total_input_tokens: usize,
    pub total_output_tokens: usize,
    pub total_cache_creation_tokens: usize,
    pub total_cache_read_tokens: usize,
    pub prompt_token_estimate: usize,
    /// Input tokens reported by the model for the most recent turn.
    /// Used by `TokenBudget::should_compact()` to decide when to compact.
    pub last_input_tokens: usize,
    /// Thread-safe inbox for pending inputs pushed from server handlers
    /// while the query loop is running.
    pub pending_user_prompts: Arc<Mutex<VecDeque<PendingInputItem>>>,
    /// Turn-scoped state (Some while a turn is active).
    pub(crate) turn_state: Option<TurnState>,
    /// Items queued for next turn when current turn ends with unconsumed input.
    pub idle_pending_input: VecDeque<PendingInputItem>,
}

impl SessionState {
    pub fn new(config: SessionConfig, cwd: PathBuf) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            config,
            messages: Vec::new(),
            prompt_messages: None,
            session_context: None,
            latest_turn_context: None,
            cwd,
            turn_count: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_creation_tokens: 0,
            total_cache_read_tokens: 0,
            prompt_token_estimate: 0,
            last_input_tokens: 0,
            pending_user_prompts: Arc::new(Mutex::new(VecDeque::new())),
            turn_state: None,
            idle_pending_input: VecDeque::new(),
        }
    }

    pub fn push_message(&mut self, msg: Message) {
        self.messages.push(msg.clone());
        if let Some(prompt_messages) = self.prompt_messages.as_mut() {
            prompt_messages.push(msg);
        }
    }

    pub fn to_request_messages(&self) -> Vec<devo_protocol::RequestMessage> {
        self.prompt_source_messages()
            .iter()
            .map(|m| m.to_request_message())
            .collect()
    }

    pub fn prompt_source_messages(&self) -> &[Message] {
        self.prompt_messages
            .as_deref()
            .unwrap_or(self.messages.as_slice())
    }

    pub fn set_prompt_messages(&mut self, messages: Vec<Message>) {
        self.prompt_messages = Some(messages);
    }

    pub fn clear_prompt_messages(&mut self) {
        self.prompt_messages = None;
    }

    pub fn insert_context_message(&mut self, msg: Message) {
        crate::history::insert_context_diff_message(&mut self.messages, msg.clone());
        if let Some(prompt_messages) = self.prompt_messages.as_mut() {
            crate::history::insert_context_diff_message(prompt_messages, msg);
        }
    }

    pub fn enqueue_pending_input(&self, item: PendingInputItem) {
        self.pending_user_prompts
            .lock()
            .expect("pending user prompts mutex should not be poisoned")
            .push_back(item);
    }

    pub fn drain_pending_user_prompts(&self) -> Vec<PendingInputItem> {
        let mut pending = self
            .pending_user_prompts
            .lock()
            .expect("pending user prompts mutex should not be poisoned");
        pending.drain(..).collect()
    }

    pub fn start_turn(&mut self, kind: TurnKind) {
        // Move idle-pending items into the new turn's pending_input.
        let idle = std::mem::take(&mut self.idle_pending_input);
        let mut turn = TurnState::new(kind);
        turn.pending_input = idle.into();
        self.turn_state = Some(turn);
    }

    pub fn end_turn(&mut self) {
        if let Some(turn) = self.turn_state.take() {
            // Unconsumed pending input goes back to idle queue.
            self.idle_pending_input.extend(turn.pending_input);
        }
    }

    /// Merge turn-scoped pending input with the cross-thread inbox.
    pub fn take_turn_pending_input(&mut self) -> Vec<PendingInputItem> {
        let mut result = Vec::new();
        if let Some(turn) = self.turn_state.as_mut() {
            result = turn.take_pending_input();
        }
        result.extend(self.drain_pending_user_prompts());
        result
    }

    pub fn queue_for_next_turn(&mut self, items: Vec<PendingInputItem>) {
        self.idle_pending_input.extend(items);
    }

    pub fn take_queued_for_next_turn(&mut self) -> Vec<PendingInputItem> {
        self.idle_pending_input.drain(..).collect()
    }

    pub fn has_queued_for_next_turn(&self) -> bool {
        !self.idle_pending_input.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_config_default_values() {
        let config = SessionConfig::default();
        assert_eq!(config.permission_mode, PermissionMode::AutoApprove);
    }

    #[test]
    fn session_state_new_initializes_correctly() {
        let config = SessionConfig::default();
        let cwd = PathBuf::from("/tmp");
        let state = SessionState::new(config, cwd.clone());

        assert!(!state.id.is_empty());
        assert!(state.messages.is_empty());
        assert!(state.session_context.is_none());
        assert!(state.latest_turn_context.is_none());
        assert_eq!(state.cwd, cwd);
        assert_eq!(state.turn_count, 0);
        assert_eq!(state.total_input_tokens, 0);
        assert_eq!(state.total_output_tokens, 0);
    }

    #[test]
    fn session_state_push_message() {
        let mut state = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        state.push_message(Message::user("hello"));
        state.push_message(Message::assistant_text("hi"));
        assert_eq!(state.messages.len(), 2);
    }

    #[test]
    fn session_state_to_request_messages() {
        let mut state = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        state.push_message(Message::user("hello"));
        state.push_message(Message::assistant_text("hi"));

        let req_msgs = state.to_request_messages();
        assert_eq!(req_msgs.len(), 2);
        assert_eq!(req_msgs[0].role, "user");
        assert_eq!(req_msgs[1].role, "assistant");
    }

    #[test]
    fn session_state_unique_ids() {
        let s1 = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        let s2 = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        assert_ne!(s1.id, s2.id);
    }

    #[test]
    fn session_state_drains_pending_user_prompts() {
        use chrono::Utc;
        let state = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        state.enqueue_pending_input(PendingInputItem {
            kind: devo_protocol::PendingInputKind::UserText {
                text: "first".to_string(),
            },
            metadata: None,
            created_at: Utc::now(),
        });
        state.enqueue_pending_input(PendingInputItem {
            kind: devo_protocol::PendingInputKind::UserText {
                text: "second".to_string(),
            },
            metadata: None,
            created_at: Utc::now(),
        });

        let drained = state.drain_pending_user_prompts();
        assert_eq!(drained.len(), 2);
        assert!(state.drain_pending_user_prompts().is_empty());
    }

    #[test]
    fn session_state_start_turn_creates_turn_state() {
        let mut state = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        assert!(state.turn_state.is_none());
        state.start_turn(TurnKind::Regular);
        assert!(state.turn_state.is_some());
        assert_eq!(state.turn_state.as_ref().unwrap().kind, TurnKind::Regular);
    }

    #[test]
    fn session_state_start_turn_drains_idle_queue() {
        use chrono::Utc;
        let mut state = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        state.idle_pending_input.push_back(PendingInputItem {
            kind: devo_protocol::PendingInputKind::UserText {
                text: "idle".to_string(),
            },
            metadata: None,
            created_at: Utc::now(),
        });
        state.start_turn(TurnKind::Regular);
        let pending = state.take_turn_pending_input();
        assert_eq!(pending.len(), 1);
        assert!(state.idle_pending_input.is_empty());
    }

    #[test]
    fn session_state_end_turn_moves_unconsumed_to_idle() {
        use chrono::Utc;
        let mut state = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        state.start_turn(TurnKind::Regular);
        // Push an item into the turn's pending input directly.
        if let Some(turn) = state.turn_state.as_mut() {
            turn.push_pending_input(PendingInputItem {
                kind: devo_protocol::PendingInputKind::UserText {
                    text: "unconsumed".to_string(),
                },
                metadata: None,
                created_at: Utc::now(),
            });
        }
        state.end_turn();
        assert!(state.turn_state.is_none());
        assert_eq!(state.idle_pending_input.len(), 1);
    }

    #[test]
    fn session_state_take_turn_pending_merges_turn_and_inbox() {
        use chrono::Utc;
        let mut state = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        state.start_turn(TurnKind::Regular);
        // Push to turn-scoped pending.
        if let Some(turn) = state.turn_state.as_mut() {
            turn.push_pending_input(PendingInputItem {
                kind: devo_protocol::PendingInputKind::UserText {
                    text: "turn-item".to_string(),
                },
                metadata: None,
                created_at: Utc::now(),
            });
        }
        // Push to cross-thread inbox.
        state.enqueue_pending_input(PendingInputItem {
            kind: devo_protocol::PendingInputKind::UserText {
                text: "inbox-item".to_string(),
            },
            metadata: None,
            created_at: Utc::now(),
        });
        let merged = state.take_turn_pending_input();
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn session_state_queue_and_take_for_next_turn() {
        use chrono::Utc;
        let mut state = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        assert!(!state.has_queued_for_next_turn());
        state.queue_for_next_turn(vec![PendingInputItem {
            kind: devo_protocol::PendingInputKind::UserText {
                text: "queued".to_string(),
            },
            metadata: None,
            created_at: Utc::now(),
        }]);
        assert!(state.has_queued_for_next_turn());
        let taken = state.take_queued_for_next_turn();
        assert_eq!(taken.len(), 1);
        assert!(!state.has_queued_for_next_turn());
    }

    #[test]
    fn session_state_take_turn_pending_without_turn_drains_inbox_only() {
        use chrono::Utc;
        let state = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        state.enqueue_pending_input(PendingInputItem {
            kind: devo_protocol::PendingInputKind::UserText {
                text: "direct".to_string(),
            },
            metadata: None,
            created_at: Utc::now(),
        });
        // No turn started — take_turn_pending_input should still drain the inbox.
        let mut state_mut = state;
        let items = state_mut.take_turn_pending_input();
        assert_eq!(items.len(), 1);
    }
}
