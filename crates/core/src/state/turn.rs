use devo_protocol::{PendingInputItem, TurnKind};

#[derive(Debug)]
pub(crate) struct TurnState {
    #[allow(dead_code)]
    pub kind: TurnKind,
    pub pending_input: Vec<PendingInputItem>,
    #[allow(dead_code)]
    pub steer_metadata: Option<serde_json::Value>,
}

impl TurnState {
    pub fn new(kind: TurnKind) -> Self {
        Self {
            kind,
            pending_input: Vec::new(),
            steer_metadata: None,
        }
    }

    pub fn push_pending_input(&mut self, item: PendingInputItem) {
        self.pending_input.push(item);
    }

    #[allow(dead_code)]
    pub fn prepend_pending_input(&mut self, items: Vec<PendingInputItem>) {
        let mut all = items;
        all.extend(std::mem::take(&mut self.pending_input));
        self.pending_input = all;
    }

    pub fn take_pending_input(&mut self) -> Vec<PendingInputItem> {
        std::mem::take(&mut self.pending_input)
    }

    #[allow(dead_code)]
    pub fn has_pending_input(&self) -> bool {
        !self.pending_input.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use pretty_assertions::assert_eq;

    use super::*;

    fn sample_item() -> PendingInputItem {
        PendingInputItem {
            kind: devo_protocol::PendingInputKind::UserText {
                text: "test".into(),
            },
            metadata: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn turn_state_new_has_empty_pending_input() {
        let mut state = TurnState::new(TurnKind::Regular);
        assert!(!state.has_pending_input());
        assert!(state.take_pending_input().is_empty());
    }

    #[test]
    fn turn_state_push_and_take() {
        let mut state = TurnState::new(TurnKind::Regular);
        state.push_pending_input(sample_item());
        assert!(state.has_pending_input());
        let taken = state.take_pending_input();
        assert_eq!(taken.len(), 1);
        assert!(!state.has_pending_input());
    }

    #[test]
    fn turn_state_prepend_puts_items_first() {
        let mut state = TurnState::new(TurnKind::Regular);
        state.push_pending_input(sample_item());
        let extra = vec![PendingInputItem {
            kind: devo_protocol::PendingInputKind::UserText {
                text: "extra".into(),
            },
            metadata: None,
            created_at: Utc::now(),
        }];
        state.prepend_pending_input(extra);
        let taken = state.take_pending_input();
        assert_eq!(taken.len(), 2);
        assert!(
            matches!(taken[0].kind, devo_protocol::PendingInputKind::UserText { ref text } if text == "extra")
        );
    }

    #[test]
    fn turn_state_kind() {
        let state = TurnState::new(TurnKind::Review);
        assert_eq!(state.kind, TurnKind::Review);
    }

    #[test]
    fn turn_state_multiple_pushes() {
        let mut state = TurnState::new(TurnKind::Regular);
        state.push_pending_input(sample_item());
        state.push_pending_input(sample_item());
        state.push_pending_input(sample_item());
        assert_eq!(state.take_pending_input().len(), 3);
    }
}
