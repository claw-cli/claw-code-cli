// New modules
pub mod errors;
pub mod events;
pub mod handler_kind;
pub mod handlers;
pub mod invocation;
pub mod json_schema;
pub mod registry;
pub mod registry_plan;
pub mod router;
pub mod tool_handler;
pub mod tool_spec;
pub mod tool_summary;
pub mod unified_exec;

// Existing modules (tools)
mod apply_patch;
mod read;
mod shell_exec;
mod tool;

// New re-exports
pub use errors::*;
pub use events::*;
pub use handler_kind::ToolHandlerKind;
pub use invocation::{FunctionToolOutput, ToolCallId, ToolContent, ToolInvocation, ToolName};
pub use json_schema::JsonSchema;
pub use registry::*;
pub use registry_plan::*;
pub use router::*;
pub use tool_handler::ToolHandler;
pub use tool_spec::*;

pub use tool::ToolOutput;

/// Create a fully-configured tool registry with all built-in tools.
/// This is the recommended way to bootstrap tools.
pub fn create_default_tool_registry() -> registry::ToolRegistry {
    handlers::build_registry_from_plan(&ToolPlanConfig::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expected_tool_names_default() -> [&'static str; 17] {
        [
            "bash",
            "read",
            "write",
            "glob",
            "grep",
            "invalid",
            "question",
            "task",
            "todowrite",
            "webfetch",
            "websearch",
            "skill",
            "apply_patch",
            "lsp",
            "update_plan",
            "exec_command",
            "write_stdin",
        ]
    }

    #[test]
    fn registry_from_plan_contains_all_tools_default() {
        let registry = handlers::build_registry_from_plan(&ToolPlanConfig::default());

        for name in &expected_tool_names_default() {
            assert!(
                registry.get(name).is_some(),
                "expected tool '{name}' to be registered"
            );
        }
        // shell_command not registered by default (use_shell_command = false)
        assert!(registry.get("shell_command").is_none());
    }

    #[test]
    fn registry_from_plan_uses_shell_command_when_configured() {
        let config = ToolPlanConfig {
            use_shell_command: true,
            ..ToolPlanConfig::default()
        };
        let registry = handlers::build_registry_from_plan(&config);

        // When use_shell_command = true, bash is replaced by shell_command
        assert!(registry.get("bash").is_none());
        assert!(
            registry.get("shell_command").is_some(),
            "expected shell_command tool to be registered"
        );
    }

    #[test]
    fn registry_from_plan_without_unified_exec() {
        let config = ToolPlanConfig {
            use_unified_exec: false,
            ..ToolPlanConfig::default()
        };
        let registry = handlers::build_registry_from_plan(&config);
        assert!(
            registry.get("exec_command").is_none(),
            "exec_command should not be registered when use_unified_exec is false"
        );
        assert!(
            registry.get("write_stdin").is_none(),
            "write_stdin should not be registered when use_unified_exec is false"
        );
    }

    #[test]
    fn builtin_tools_have_nonempty_definitions() {
        let registry = handlers::build_registry_from_plan(&ToolPlanConfig::default());
        let defs = registry.tool_definitions();
        for def in &defs {
            assert!(!def.name.is_empty());
            assert!(!def.description.is_empty());
            assert!(def.input_schema.is_object());
        }
    }
}
