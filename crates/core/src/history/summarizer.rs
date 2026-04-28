use std::sync::Arc;

use async_trait::async_trait;
use devo_protocol::{Model, ModelRequest, RequestMessage, ResponseContent, SamplingControls};
use devo_provider::ModelProviderSDK;

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

        if text.is_empty() {
            return Err(CompactionError::EmptyResponse);
        }

        Ok(text)
    }
}
