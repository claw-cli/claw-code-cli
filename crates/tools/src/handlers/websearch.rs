use async_trait::async_trait;

use crate::errors::ToolExecutionError;
use crate::events::ToolProgressSender;
use crate::handler_kind::ToolHandlerKind;
use crate::invocation::FunctionToolOutput;
use crate::invocation::ToolInvocation;
use crate::invocation::ToolOutput;
use crate::tool_handler::ToolHandler;

// TODO: WebSearch is a critical agent tool because it gives the agent access to
// external information beyond the local workspace. It should be designed as an
// extensible, pluggable provider interface, allowing the runtime to connect to
// multiple public search engines now and enterprise/private knowledge bases in
// the future.
//
// Define a stable WebSearch API contract here, including the request/response
// schema, provider configuration, authentication, timeout/retry behavior, rate
// limits, error handling, and fallback strategy. The agent core should depend on
// the WebSearch abstraction rather than any specific search provider.

pub struct WebSearchHandler;

#[async_trait]
impl ToolHandler for WebSearchHandler {
    fn tool_kind(&self) -> ToolHandlerKind {
        ToolHandlerKind::WebSearch
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
        _progress: Option<ToolProgressSender>,
    ) -> Result<Box<dyn ToolOutput>, ToolExecutionError> {
        let query = invocation.input["query"].as_str().unwrap_or("");
        let client = reqwest::Client::new();
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "web_search_exa",
                "arguments": {
                    "query": query,
                    "type": invocation.input["type"].as_str().unwrap_or("auto"),
                    "numResults": invocation.input["numResults"].as_u64().unwrap_or(8),
                    "livecrawl": invocation.input["livecrawl"].as_str().unwrap_or("fallback"),
                    "contextMaxCharacters": invocation.input["contextMaxCharacters"].as_u64()
                }
            }
        });

        let res = client
            .post("https://mcp.exa.ai/mcp")
            .json(&payload)
            .send()
            .await
            .map_err(|e| ToolExecutionError::ExecutionFailed {
                message: format!("Search request failed: {e}"),
            })?;

        if !res.status().is_success() {
            return Ok(Box::new(FunctionToolOutput::error(format!(
                "Search error ({})",
                res.status()
            ))));
        }

        let text = res
            .text()
            .await
            .map_err(|e| ToolExecutionError::ExecutionFailed {
                message: format!("Failed to read search response: {e}"),
            })?;

        Ok(Box::new(FunctionToolOutput::success(text)))
    }
}
