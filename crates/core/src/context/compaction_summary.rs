use super::ContextualUserFragment;

const SUMMARY_PREFIX: &str = include_str!("../../prompts/compact/summary_prefix.md");

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompactionSummary {
    summary: String,
}

impl CompactionSummary {
    pub(crate) fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
        }
    }
}

impl ContextualUserFragment for CompactionSummary {
    const ROLE: &'static str = "user";
    const START_MARKER: &'static str = "<compaction_summary>";
    const END_MARKER: &'static str = "</compaction_summary>";

    fn body(&self) -> String {
        format!("\n{}\n{}\n", SUMMARY_PREFIX.trim(), self.summary.trim())
    }
}
