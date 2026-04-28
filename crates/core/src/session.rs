use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use devo_safety::legacy_permissions::PermissionMode;

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
    pub session_context: Option<SessionContext>,
    pub latest_turn_context: Option<TurnContext>,
    pub cwd: PathBuf,
    pub turn_count: usize,
    pub total_input_tokens: usize,
    pub total_output_tokens: usize,
    pub total_cache_creation_tokens: usize,
    pub total_cache_read_tokens: usize,
    /// Input tokens reported by the model for the most recent turn.
    /// Used by `TokenBudget::should_compact()` to decide when to compact.
    pub last_input_tokens: usize,
    /// User prompts queued while a turn is already running and injected before
    /// the next model request in the same query loop.
    pub pending_user_prompts: Arc<Mutex<VecDeque<String>>>,
}

impl SessionState {
    pub fn new(config: SessionConfig, cwd: PathBuf) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            config,
            messages: Vec::new(),
            session_context: None,
            latest_turn_context: None,
            cwd,
            turn_count: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_creation_tokens: 0,
            total_cache_read_tokens: 0,
            last_input_tokens: 0,
            pending_user_prompts: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub fn push_message(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    pub fn to_request_messages(&self) -> Vec<devo_protocol::RequestMessage> {
        self.messages
            .iter()
            .map(|m| m.to_request_message())
            .collect()
    }

    pub fn enqueue_user_prompt(&self, prompt: String) {
        self.pending_user_prompts
            .lock()
            .expect("pending user prompts mutex should not be poisoned")
            .push_back(prompt);
    }

    pub fn drain_pending_user_prompts(&self) -> Vec<String> {
        let mut pending = self
            .pending_user_prompts
            .lock()
            .expect("pending user prompts mutex should not be poisoned");
        pending.drain(..).collect()
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
        let state = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        state.enqueue_user_prompt("first".to_string());
        state.enqueue_user_prompt("second".to_string());

        assert_eq!(
            state.drain_pending_user_prompts(),
            vec!["first".to_string(), "second".to_string()]
        );
        assert!(state.drain_pending_user_prompts().is_empty());
    }
}
