use std::collections::HashMap;
use std::sync::Arc;

use devo_protocol::ToolDefinition;

use crate::errors::ToolDispatchError;
use crate::invocation::{ToolInvocation, ToolOutput};
use crate::tool_handler::ToolHandler;
use crate::tool_spec::{ToolExecutionMode, ToolSpec};
use crate::unified_exec::store::ProcessStore;

#[derive(Clone)]
pub struct ToolRegistry {
    pub(crate) handlers: HashMap<String, Arc<dyn ToolHandler>>,
    pub(crate) specs: Vec<ToolSpec>,
    pub(crate) spec_index: HashMap<String, usize>,
    pub(crate) unified_exec_store: Option<Arc<ProcessStore>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        ToolRegistry {
            handlers: HashMap::new(),
            specs: Vec::new(),
            spec_index: HashMap::new(),
            unified_exec_store: None,
        }
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn ToolHandler>> {
        self.handlers.get(name)
    }

    pub fn spec(&self, name: &str) -> Option<&ToolSpec> {
        self.spec_index.get(name).map(|&idx| &self.specs[idx])
    }

    pub fn is_read_only(&self, name: &str) -> bool {
        self.spec(name)
            .is_some_and(|s| s.execution_mode == ToolExecutionMode::ReadOnly)
    }

    pub fn supports_parallel(&self, name: &str) -> bool {
        self.spec(name).is_some_and(|s| s.supports_parallel)
    }

    pub async fn dispatch(
        &self,
        name: &str,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn ToolOutput>, ToolDispatchError> {
        let handler = self
            .handlers
            .get(name)
            .ok_or_else(|| ToolDispatchError::UnknownTool {
                name: name.to_string(),
            })?;
        handler
            .handle(invocation, None)
            .await
            .map_err(ToolDispatchError::from)
    }

    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.specs
            .iter()
            .map(|spec| ToolDefinition {
                name: spec.name.clone(),
                description: spec.description.clone(),
                input_schema: unified_exec_input_schema(
                    &spec.name,
                    spec.input_schema.to_json_value(),
                ),
                output_schema: unified_exec_output_schema(&spec.name),
            })
            .collect()
    }

    pub fn all_handlers(&self) -> impl Iterator<Item = (&String, &Arc<dyn ToolHandler>)> {
        self.handlers.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    pub async fn terminate_unified_exec_processes(&self) {
        if let Some(store) = &self.unified_exec_store {
            store.terminate_all().await;
        }
    }
}

fn unified_exec_input_schema(tool_name: &str, mut schema: serde_json::Value) -> serde_json::Value {
    if tool_name != "exec_command" {
        return schema;
    }

    let Some(properties) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return schema;
    };

    properties.insert(
        "sandbox_permissions".to_string(),
        serde_json::json!({
            "type": "string",
            "description": "Sandbox permissions for the command. Use \"with_additional_permissions\" to request additional sandboxed filesystem or network permissions (preferred), or \"require_escalated\" to request running without sandbox restrictions; defaults to \"use_default\".",
            "enum": ["use_default", "require_escalated", "with_additional_permissions"]
        }),
    );
    properties.insert(
        "additional_permissions".to_string(),
        additional_permissions_schema(),
    );
    properties.insert(
        "justification".to_string(),
        serde_json::json!({
            "type": "string",
            "description": "Only set if sandbox_permissions is \"require_escalated\".\nRequest approval from the user to run this command outside the sandbox.\nPhrased as a simple question that summarizes the purpose of the\ncommand as it relates to the task at hand - e.g. 'Do you want to\nfetch and pull the latest version of this git branch?'"
        }),
    );
    properties.insert(
        "prefix_rule".to_string(),
        serde_json::json!({
            "type": "array",
            "description": "Only specify when sandbox_permissions is `require_escalated`.\nSuggest a prefix command pattern that will allow you to fulfill similar requests from the user in the future.\nShould be a short but reasonable prefix, e.g. [\"git\", \"pull\"] or [\"uv\", \"run\"] or [\"pytest\"].",
            "items": { "type": "string" }
        }),
    );

    schema
}

fn additional_permissions_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "network": {
                "type": "object",
                "properties": {
                    "enabled": {
                        "type": "boolean",
                        "description": "Set to true to request network access."
                    }
                },
                "additionalProperties": false
            },
            "file_system": {
                "type": "object",
                "properties": {
                    "read": {
                        "type": "array",
                        "description": "Absolute paths to grant read access to.",
                        "items": { "type": "string" }
                    },
                    "write": {
                        "type": "array",
                        "description": "Absolute paths to grant write access to.",
                        "items": { "type": "string" }
                    }
                },
                "additionalProperties": false
            }
        },
        "additionalProperties": false
    })
}

fn unified_exec_output_schema(tool_name: &str) -> Option<serde_json::Value> {
    if tool_name != "exec_command" && tool_name != "write_stdin" {
        return None;
    }

    Some(serde_json::json!({
        "type": "object",
        "properties": {
            "chunk_id": {
                "type": "string",
                "description": "Chunk identifier included when the response reports one."
            },
            "wall_time_seconds": {
                "type": "number",
                "description": "Elapsed wall time spent waiting for output in seconds."
            },
            "exit_code": {
                "type": "number",
                "description": "Process exit code when the command finished during this call."
            },
            "session_id": {
                "type": "number",
                "description": "Session identifier to pass to write_stdin when the process is still running."
            },
            "original_token_count": {
                "type": "number",
                "description": "Approximate token count before output truncation."
            },
            "output": {
                "type": "string",
                "description": "Command output text, possibly truncated."
            }
        },
        "required": ["wall_time_seconds", "output"],
        "additionalProperties": false
    }))
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ToolRegistryBuilder {
    handlers: HashMap<String, Arc<dyn ToolHandler>>,
    specs: Vec<ToolSpec>,
    spec_index: HashMap<String, usize>,
    unified_exec_store: Option<Arc<ProcessStore>>,
}

impl ToolRegistryBuilder {
    pub fn new() -> Self {
        ToolRegistryBuilder {
            handlers: HashMap::new(),
            specs: Vec::new(),
            spec_index: HashMap::new(),
            unified_exec_store: None,
        }
    }

    pub fn push_spec(&mut self, spec: ToolSpec) {
        let name = spec.name.clone();
        self.spec_index.insert(name, self.specs.len());
        self.specs.push(spec);
    }

    pub fn register_handler(&mut self, name: &str, handler: Arc<dyn ToolHandler>) {
        self.handlers.insert(name.to_string(), handler);
    }

    pub fn set_unified_exec_store(&mut self, store: Arc<ProcessStore>) {
        self.unified_exec_store = Some(store);
    }

    pub fn build(self) -> ToolRegistry {
        ToolRegistry {
            handlers: self.handlers,
            specs: self.specs,
            spec_index: self.spec_index,
            unified_exec_store: self.unified_exec_store,
        }
    }
}

impl Default for ToolRegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ToolExecutionError;
    use crate::events::ToolProgressSender;
    use crate::invocation::{FunctionToolOutput, ToolCallId, ToolName, ToolOutput};
    use crate::json_schema::JsonSchema;
    use crate::tool_spec::{ToolExecutionMode, ToolOutputMode, ToolSpec};
    use async_trait::async_trait;
    use std::path::PathBuf;

    struct EchoHandler;

    #[async_trait]
    impl ToolHandler for EchoHandler {
        fn tool_kind(&self) -> crate::handler_kind::ToolHandlerKind {
            crate::handler_kind::ToolHandlerKind::Read
        }

        async fn handle(
            &self,
            _invocation: ToolInvocation,
            _progress: Option<ToolProgressSender>,
        ) -> Result<Box<dyn ToolOutput>, ToolExecutionError> {
            Ok(Box::new(FunctionToolOutput::success("echo")))
        }
    }

    #[test]
    fn registry_register_and_get() {
        let mut builder = ToolRegistryBuilder::new();
        builder.register_handler("echo", Arc::new(EchoHandler));
        builder.push_spec(ToolSpec {
            name: "echo".into(),
            description: String::new(),
            input_schema: JsonSchema::object(Default::default(), None, None),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::ReadOnly,
            capability_tags: vec![],
            supports_parallel: true,
        });
        let registry = builder.build();
        assert!(registry.get("echo").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn registry_tool_definitions() {
        let mut builder = ToolRegistryBuilder::new();
        builder.register_handler("echo", Arc::new(EchoHandler));
        builder.push_spec(ToolSpec {
            name: "echo".into(),
            description: "test".into(),
            input_schema: JsonSchema::string(None),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::ReadOnly,
            capability_tags: vec![],
            supports_parallel: true,
        });
        let registry = builder.build();
        let defs = registry.tool_definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "echo");
        assert_eq!(defs[0].description, "test");
    }

    #[test]
    fn registry_adds_output_schema_for_unified_exec_tools() {
        let mut builder = ToolRegistryBuilder::new();
        builder.push_spec(ToolSpec {
            name: "exec_command".into(),
            description: "exec".into(),
            input_schema: JsonSchema::object(Default::default(), None, None),
            output_mode: ToolOutputMode::Mixed,
            execution_mode: ToolExecutionMode::Mutating,
            capability_tags: vec![],
            supports_parallel: true,
        });

        let registry = builder.build();
        let defs = registry.tool_definitions();

        assert!(defs[0].output_schema.is_some());
    }

    #[test]
    fn registry_adds_permission_fields_for_exec_command() {
        let mut builder = ToolRegistryBuilder::new();
        builder.push_spec(ToolSpec {
            name: "exec_command".into(),
            description: "exec".into(),
            input_schema: JsonSchema::object(Default::default(), None, None),
            output_mode: ToolOutputMode::Mixed,
            execution_mode: ToolExecutionMode::Mutating,
            capability_tags: vec![],
            supports_parallel: true,
        });

        let registry = builder.build();
        let defs = registry.tool_definitions();
        let properties = defs[0]
            .input_schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("object schema should have properties");

        assert!(properties.contains_key("sandbox_permissions"));
        assert!(properties.contains_key("additional_permissions"));
        assert!(properties.contains_key("justification"));
        assert!(properties.contains_key("prefix_rule"));
        assert_eq!(
            properties["additional_permissions"]["properties"]["network"]["properties"]["enabled"]
                ["type"],
            "boolean"
        );
        assert_eq!(
            properties["additional_permissions"]["properties"]["file_system"]["properties"]["read"]
                ["items"]["type"],
            "string"
        );
    }

    #[tokio::test]
    async fn registry_dispatch_unknown_tool() {
        let builder = ToolRegistryBuilder::new();
        let registry = builder.build();
        let invocation = ToolInvocation {
            call_id: ToolCallId("c1".into()),
            tool_name: ToolName("nonexistent".into()),
            session_id: "s1".into(),
            cwd: PathBuf::from("/tmp"),
            input: serde_json::json!({}),
        };
        let result = registry.dispatch("nonexistent", invocation).await;
        match result {
            Err(ToolDispatchError::UnknownTool { name }) => assert_eq!(name, "nonexistent"),
            Err(other) => panic!("expected UnknownTool error, got: {other}"),
            Ok(_) => panic!("expected error, got Ok"),
        }
    }

    #[tokio::test]
    async fn registry_supports_parallel() {
        let mut builder = ToolRegistryBuilder::new();
        builder.register_handler("read", Arc::new(EchoHandler));
        builder.push_spec(ToolSpec {
            name: "read".into(),
            description: String::new(),
            input_schema: JsonSchema::object(Default::default(), None, None),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::ReadOnly,
            capability_tags: vec![],
            supports_parallel: true,
        });
        let registry = builder.build();
        assert!(registry.supports_parallel("read"));
    }

    #[test]
    fn registry_builder_default() {
        let builder = ToolRegistryBuilder::default();
        let registry = builder.build();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn registry_is_read_only() {
        let mut builder = ToolRegistryBuilder::new();
        builder.register_handler("read", Arc::new(EchoHandler));
        builder.push_spec(ToolSpec {
            name: "read".into(),
            description: String::new(),
            input_schema: JsonSchema::string(None),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::ReadOnly,
            capability_tags: vec![],
            supports_parallel: true,
        });
        builder.register_handler("write", Arc::new(EchoHandler));
        builder.push_spec(ToolSpec {
            name: "write".into(),
            description: String::new(),
            input_schema: JsonSchema::string(None),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::Mutating,
            capability_tags: vec![],
            supports_parallel: false,
        });
        let registry = builder.build();
        assert!(registry.is_read_only("read"));
        assert!(!registry.is_read_only("write"));
        assert!(!registry.is_read_only("nonexistent"));
    }

    #[test]
    fn registry_spec_lookup() {
        let mut builder = ToolRegistryBuilder::new();
        builder.register_handler("tool", Arc::new(EchoHandler));
        builder.push_spec(ToolSpec {
            name: "tool".into(),
            description: "desc".into(),
            input_schema: JsonSchema::string(None),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::Mutating,
            capability_tags: vec![],
            supports_parallel: false,
        });
        let registry = builder.build();
        let spec = registry.spec("tool");
        assert!(spec.is_some());
        assert_eq!(spec.unwrap().description, "desc");
        assert!(registry.spec("missing").is_none());
    }

    #[test]
    fn registry_supports_parallel_for_missing_returns_false() {
        let registry = ToolRegistryBuilder::new().build();
        assert!(!registry.supports_parallel("nonexistent"));
    }

    #[tokio::test]
    async fn registry_dispatch_success() {
        let mut builder = ToolRegistryBuilder::new();
        builder.register_handler("echo", Arc::new(EchoHandler));
        builder.push_spec(ToolSpec {
            name: "echo".into(),
            description: String::new(),
            input_schema: JsonSchema::string(None),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::ReadOnly,
            capability_tags: vec![],
            supports_parallel: true,
        });
        let registry = builder.build();
        let invocation = ToolInvocation {
            call_id: ToolCallId("c1".into()),
            tool_name: ToolName("echo".into()),
            session_id: "s1".into(),
            cwd: PathBuf::from("/tmp"),
            input: serde_json::json!({}),
        };
        let result = registry.dispatch("echo", invocation).await;
        assert!(result.is_ok());
    }
}
