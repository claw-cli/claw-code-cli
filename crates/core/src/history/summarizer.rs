use std::sync::Arc;

use async_trait::async_trait;
use devo_protocol::{Model, ModelRequest, RequestMessage, ResponseContent, SamplingControls};
use devo_provider::ModelProviderSDK;
use tracing::debug;

use super::compaction::{CompactionError, HistorySummarizer};

/// Concrete implementation of `HistorySummarizer` that delegates to a
/// `ModelProviderSDK`.
///
/// Detects `context_length_exceeded` provider errors and maps them to
/// `CompactionError::ContextTooLong` so the compaction retry loop can
/// recover by shrinking the input.
pub struct DefaultHistorySummarizer {
    provider: Arc<dyn ModelProviderSDK>,
    model: String,
    max_tokens: usize,
}

impl DefaultHistorySummarizer {
    pub fn new(provider: Arc<dyn ModelProviderSDK>, model: &Model) -> Self {
        let max_tokens = model.max_tokens.unwrap_or(4096) as usize;
        Self {
            provider,
            model: model.slug.clone(),
            max_tokens,
        }
    }

    /// Convenience constructor when only a model slug and max tokens are
    /// available (e.g. when the `Model` struct cannot be resolved).
    pub fn with_slug(
        provider: Arc<dyn ModelProviderSDK>,
        model_slug: impl Into<String>,
        max_tokens: usize,
    ) -> Self {
        Self {
            provider,
            model: model_slug.into(),
            max_tokens,
        }
    }
}

fn sanitize_compaction_summary(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.contains("DSML")
                && !trimmed.starts_with("<｜")
                && !trimmed.ends_with("｜>")
                && !trimmed.starts_with("<|")
                && !trimmed.ends_with("|>")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

#[async_trait]
impl HistorySummarizer for DefaultHistorySummarizer {
    async fn summarize(&self, messages: Vec<RequestMessage>) -> Result<String, CompactionError> {
        let request = ModelRequest {
            model: self.model.clone(),
            system: None,
            messages,
            max_tokens: self.max_tokens,
            tools: None,
            sampling: SamplingControls::default(),
            thinking: None,
            reasoning_effort: None,
            extra_body: None,
        };
        let request_preview = serde_json::to_string_pretty(&request).unwrap_or_else(|error| {
            format!("<failed to serialize compaction request for logging: {error}>")
        });
        debug!(
            model = %self.model,
            message_count = request.messages.len(),
            max_tokens = request.max_tokens,
            compaction_request = %request_preview,
            "sending LLM compaction request"
        );

        let response = match self.provider.completion(request).await {
            Ok(r) => r,
            Err(e) => {
                let err_msg = e.to_string();
                if err_msg.contains("context_length_exceeded")
                    || err_msg.contains("maximum context length")
                {
                    return Err(CompactionError::ContextTooLong);
                }
                return Err(CompactionError::SummarizationFailed { message: err_msg });
            }
        };

        let text: String = response
            .content
            .iter()
            .filter_map(|block| match block {
                ResponseContent::Text(text) => Some(text.as_str()),
                ResponseContent::ToolUse { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        let text = sanitize_compaction_summary(&text);

        if text.is_empty() {
            return Err(CompactionError::EmptyResponse);
        }

        debug!(
            model = %self.model,
            response_chars = text.len(),
            compaction_response = %text,
            "received LLM compaction response"
        );

        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::sanitize_compaction_summary;

    #[test]
    fn sanitize_compaction_summary_strips_dsml_tool_markup() {
        let input = r#"Progress so far
<｜DSML｜tool_calls>
<｜DSML｜invoke name="grep">
<｜DSML｜parameter name="path" string="true">src</｜DSML｜parameter>
</｜DSML｜invoke>
</｜DSML｜tool_calls>
Next step"#;

        assert_eq!(
            sanitize_compaction_summary(input),
            "Progress so far\nNext step"
        );
    }
}
