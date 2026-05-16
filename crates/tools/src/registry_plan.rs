use std::collections::BTreeMap;

use crate::handler_kind::ToolHandlerKind;
use crate::json_schema::JsonSchema;
use crate::tool_spec::{ToolCapabilityTag, ToolExecutionMode, ToolOutputMode, ToolSpec};

const BASH_DESCRIPTION: &str = include_str!("bash.txt");
const READ_DESCRIPTION: &str = include_str!("read.txt");
const APPLY_PATCH_DESCRIPTION: &str = include_str!("apply_patch.txt");

#[derive(Debug, Clone)]
pub struct ToolRegistryPlan {
    pub specs: Vec<ToolSpec>,
    pub handlers: Vec<(ToolHandlerKind, String)>,
}

impl ToolRegistryPlan {
    pub fn new() -> Self {
        ToolRegistryPlan {
            specs: Vec::new(),
            handlers: Vec::new(),
        }
    }

    fn push(&mut self, spec: ToolSpec, kind: ToolHandlerKind) {
        let name = spec.name.clone();
        self.specs.push(spec);
        self.handlers.push((kind, name));
    }
}

impl Default for ToolRegistryPlan {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct ToolPlanConfig {
    pub use_shell_command: bool,
    pub use_unified_exec: bool,
}

impl ToolPlanConfig {
    pub fn validate(&self) {
        // No incompatible combinations currently exist.
        // - use_shell_command and use_unified_exec are independent (shell_command replaces bash,
        //   unified exec adds new tools)
        // - both can be true simultaneously with no conflict
    }
}

impl Default for ToolPlanConfig {
    fn default() -> Self {
        ToolPlanConfig {
            use_shell_command: false,
            use_unified_exec: true,
        }
    }
}

fn bash_schema() -> JsonSchema {
    JsonSchema::object(
        BTreeMap::from([
            (
                "command".to_string(),
                JsonSchema::string(Some(
                    "The shell command to execute in the selected platform shell",
                )),
            ),
            (
                "cmd".to_string(),
                JsonSchema::string(Some("Alias for command")),
            ),
            (
                "timeout".to_string(),
                JsonSchema::integer(Some("Optional timeout in milliseconds")),
            ),
            (
                "workdir".to_string(),
                JsonSchema::string(Some(
                    "The working directory to run the command in. Defaults to the current directory. Use this instead of 'cd' commands.",
                )),
            ),
            (
                "description".to_string(),
                JsonSchema::string(Some(
                    "Clear, concise description of what this command does in 5-10 words.",
                )),
            ),
            (
                "shell".to_string(),
                JsonSchema::string(Some(
                    "Optional shell binary to launch. Defaults to the user's default shell.",
                )),
            ),
            (
                "tty".to_string(),
                JsonSchema::boolean(Some(
                    "Whether to allocate a TTY for the command. Defaults to false.",
                )),
            ),
            (
                "login".to_string(),
                JsonSchema::boolean(Some(
                    "Whether to run the shell with login shell semantics. Defaults to true.",
                )),
            ),
            (
                "yield_time_ms".to_string(),
                JsonSchema::number(Some(
                    "How long to wait (in milliseconds) for output before yielding.",
                )),
            ),
            (
                "max_output_tokens".to_string(),
                JsonSchema::number(Some(
                    "Maximum number of tokens to return. Excess output will be truncated.",
                )),
            ),
        ]),
        Some(vec!["command".to_string()]),
        Some(false),
    )
}

fn bash_description() -> String {
    let chaining = if cfg!(windows) {
        "If commands depend on each other and must run sequentially, use a single PowerShell command string. In Windows PowerShell 5.1, do not rely on Bash chaining semantics like `cmd1 && cmd2`; prefer `cmd1; if ($?) { cmd2 }` when the later command depends on earlier success."
    } else {
        "If commands depend on each other and must run sequentially, use a single shell command and chain with `&&` when later commands depend on earlier success."
    };

    let shell = if cfg!(windows) { "powershell" } else { "bash" };

    BASH_DESCRIPTION
        .replace(
            "${directory}",
            &std::env::current_dir().map_or_else(|_| ".".to_string(), |p| p.display().to_string()),
        )
        .replace("${os}", std::env::consts::OS)
        .replace("${shell}", shell)
        .replace("${chaining}", chaining)
        .replace("${maxBytes}", "64 KB")
}

fn shell_command_schema() -> JsonSchema {
    JsonSchema::object(
        BTreeMap::from([
            (
                "cmd".to_string(),
                JsonSchema::string(Some("Shell command to execute.")),
            ),
            (
                "workdir".to_string(),
                JsonSchema::string(Some(
                    "Optional working directory. Defaults to current directory.",
                )),
            ),
            (
                "shell".to_string(),
                JsonSchema::string(Some(
                    "Shell binary to launch (e.g. 'pwsh' or 'powershell' on Windows, 'bash' elsewhere).",
                )),
            ),
            (
                "tty".to_string(),
                JsonSchema::boolean(Some(
                    "Whether to allocate a TTY for the command. Defaults to false.",
                )),
            ),
            (
                "yield_time_ms".to_string(),
                JsonSchema::number(Some("How long to wait (in ms) for output before yielding.")),
            ),
            (
                "max_output_tokens".to_string(),
                JsonSchema::number(Some("Maximum number of tokens to return.")),
            ),
        ]),
        Some(vec!["cmd".to_string()]),
        Some(false),
    )
}

fn read_schema() -> JsonSchema {
    JsonSchema::object(
        BTreeMap::from([
            (
                "filePath".to_string(),
                JsonSchema::string(Some("The absolute path to the file or directory to read")),
            ),
            (
                "offset".to_string(),
                JsonSchema::integer(Some(
                    "The line number to start reading from (1-indexed, default 1)",
                )),
            ),
            (
                "limit".to_string(),
                JsonSchema::integer(Some(
                    "The maximum number of lines to read (no limit by default)",
                )),
            ),
        ]),
        Some(vec!["filePath".to_string()]),
        Some(false),
    )
}

fn write_schema() -> JsonSchema {
    JsonSchema::object(
        BTreeMap::from([
            (
                "filePath".to_string(),
                JsonSchema::string(Some("The absolute path to the file to write")),
            ),
            (
                "content".to_string(),
                JsonSchema::string(Some("The full file content to write")),
            ),
        ]),
        Some(vec!["filePath".to_string(), "content".to_string()]),
        Some(false),
    )
}

fn glob_schema() -> JsonSchema {
    JsonSchema::object(
        BTreeMap::from([
            (
                "pattern".to_string(),
                JsonSchema::string(Some("The glob pattern to match files against")),
            ),
            (
                "path".to_string(),
                JsonSchema::string(Some("The directory to search in. Defaults to current dir.")),
            ),
        ]),
        Some(vec!["pattern".to_string()]),
        Some(false),
    )
}

fn grep_schema() -> JsonSchema {
    JsonSchema::object(
        BTreeMap::from([
            (
                "pattern".to_string(),
                JsonSchema::string(Some("The regex pattern to search for")),
            ),
            (
                "include".to_string(),
                JsonSchema::string(Some("File pattern to include (e.g. '*.rs')")),
            ),
            (
                "path".to_string(),
                JsonSchema::string(Some("The directory to search in. Defaults to current dir.")),
            ),
        ]),
        Some(vec!["pattern".to_string()]),
        Some(false),
    )
}

fn apply_patch_schema() -> JsonSchema {
    JsonSchema::object(
        BTreeMap::from([(
            "patchText".to_string(),
            JsonSchema::string(Some(
                "The full patch text that describes all changes to be made",
            )),
        )]),
        Some(vec!["patchText".to_string()]),
        Some(false),
    )
}

fn plan_schema() -> JsonSchema {
    JsonSchema::object(
        BTreeMap::from([
            (
                "explanation".to_string(),
                JsonSchema::string(Some("Optional explanation for the plan update")),
            ),
            (
                "plan".to_string(),
                JsonSchema::array(
                    JsonSchema::object(
                        BTreeMap::from([
                            (
                                "step".to_string(),
                                JsonSchema::string(Some("Description of the plan step")),
                            ),
                            (
                                "status".to_string(),
                                JsonSchema::string(Some("Status of the step")),
                            ),
                        ]),
                        Some(vec!["step".to_string(), "status".to_string()]),
                        Some(false),
                    ),
                    Some("List of plan items"),
                ),
            ),
        ]),
        Some(vec!["plan".to_string()]),
        Some(false),
    )
}

fn question_schema() -> JsonSchema {
    JsonSchema::object(
        BTreeMap::from([(
            "question".to_string(),
            JsonSchema::string(Some("The question to ask the user")),
        )]),
        Some(vec!["question".to_string()]),
        Some(false),
    )
}

fn task_schema() -> JsonSchema {
    JsonSchema::object(
        BTreeMap::from([(
            "description".to_string(),
            JsonSchema::string(Some(
                "A clear, concise description of the task to accomplish",
            )),
        )]),
        Some(vec!["description".to_string()]),
        Some(false),
    )
}

fn webfetch_schema() -> JsonSchema {
    JsonSchema::object(
        BTreeMap::from([
            (
                "url".to_string(),
                JsonSchema::string(Some("The URL to fetch content from")),
            ),
            (
                "format".to_string(),
                JsonSchema::string(Some("The format to return (text, markdown, or html)")),
            ),
            (
                "timeout".to_string(),
                JsonSchema::integer(Some("Optional timeout in seconds")),
            ),
        ]),
        Some(vec!["url".to_string()]),
        Some(false),
    )
}

fn websearch_schema() -> JsonSchema {
    JsonSchema::object(
        BTreeMap::from([(
            "query".to_string(),
            JsonSchema::string(Some("The search query")),
        )]),
        Some(vec!["query".to_string()]),
        Some(false),
    )
}

fn skill_schema() -> JsonSchema {
    JsonSchema::object(
        BTreeMap::from([
            (
                "name".to_string(),
                JsonSchema::string(Some("The skill name to load")),
            ),
            (
                "args".to_string(),
                JsonSchema::string(Some("Optional arguments for the skill")),
            ),
        ]),
        Some(vec!["name".to_string()]),
        Some(false),
    )
}

fn lsp_schema() -> JsonSchema {
    JsonSchema::object(
        BTreeMap::from([
            (
                "filePath".to_string(),
                JsonSchema::string(Some("The absolute path to the file")),
            ),
            (
                "line".to_string(),
                JsonSchema::integer(Some("Line number (0-indexed)")),
            ),
            (
                "character".to_string(),
                JsonSchema::integer(Some("Character offset")),
            ),
        ]),
        Some(vec![
            "filePath".to_string(),
            "line".to_string(),
            "character".to_string(),
        ]),
        Some(false),
    )
}

fn exec_command_schema() -> JsonSchema {
    JsonSchema::object(
        BTreeMap::from([
            (
                "cmd".to_string(),
                JsonSchema::string(Some("Shell command to execute")),
            ),
            (
                "command".to_string(),
                JsonSchema::string(Some("Alias for cmd")),
            ),
            (
                "workdir".to_string(),
                JsonSchema::string(Some("Working directory. Defaults to current directory.")),
            ),
            (
                "shell".to_string(),
                JsonSchema::string(Some(
                    "Shell binary to launch (e.g. 'bash' or 'powershell').",
                )),
            ),
            (
                "login".to_string(),
                JsonSchema::boolean(Some(
                    "Whether to run the shell with login shell semantics. Defaults to true.",
                )),
            ),
            (
                "tty".to_string(),
                JsonSchema::boolean(Some(
                    "Whether to allocate a PTY. Must be true for write_stdin to work.",
                )),
            ),
            (
                "yield_time_ms".to_string(),
                JsonSchema::number(Some(
                    "How long to wait (in ms) for output before returning. Default 10000.",
                )),
            ),
            (
                "max_output_tokens".to_string(),
                JsonSchema::number(Some("Maximum number of tokens of output to return.")),
            ),
        ]),
        Some(vec!["cmd".to_string()]),
        Some(false),
    )
}

fn write_stdin_schema() -> JsonSchema {
    JsonSchema::object(
        BTreeMap::from([
            (
                "session_id".to_string(),
                JsonSchema::integer(Some("Session ID of the running exec_command process")),
            ),
            (
                "chars".to_string(),
                JsonSchema::string(Some(
                    "Bytes to write to stdin. Empty string to poll for output.",
                )),
            ),
            (
                "yield_time_ms".to_string(),
                JsonSchema::number(Some(
                    "How long to wait (in ms) for output before returning. Default 250.",
                )),
            ),
            (
                "max_output_tokens".to_string(),
                JsonSchema::number(Some("Maximum number of tokens of output to return.")),
            ),
        ]),
        Some(vec!["session_id".to_string()]),
        Some(false),
    )
}

fn invalid_schema() -> JsonSchema {
    JsonSchema::object(BTreeMap::new(), None, Some(false))
}

pub fn build_tool_registry_plan(config: &ToolPlanConfig) -> ToolRegistryPlan {
    config.validate();
    let mut plan = ToolRegistryPlan::new();

    if config.use_shell_command {
        plan.push(
            ToolSpec {
                name: "shell_command".to_string(),
                description: "Runs a command in a shell. Use this tool when you need to execute a command or start a long-running process. Prefer it over the 'bash' tool.".to_string(),
                input_schema: shell_command_schema(),
                output_mode: ToolOutputMode::Mixed,
                execution_mode: ToolExecutionMode::Mutating,
                capability_tags: vec![ToolCapabilityTag::ExecuteProcess],
                supports_parallel: false,
            },
            ToolHandlerKind::ShellCommand,
        );
    } else {
        plan.push(
            ToolSpec {
                name: "bash".to_string(),
                description: bash_description(),
                input_schema: bash_schema(),
                output_mode: ToolOutputMode::Mixed,
                execution_mode: ToolExecutionMode::Mutating,
                capability_tags: vec![ToolCapabilityTag::ExecuteProcess],
                supports_parallel: false,
            },
            ToolHandlerKind::Bash,
        );
    }

    plan.push(
        ToolSpec {
            name: "read".to_string(),
            description: READ_DESCRIPTION.to_string(),
            input_schema: read_schema(),
            output_mode: ToolOutputMode::Mixed,
            execution_mode: ToolExecutionMode::ReadOnly,
            capability_tags: vec![ToolCapabilityTag::ReadFiles],
            supports_parallel: true,
        },
        ToolHandlerKind::Read,
    );

    plan.push(
        ToolSpec {
            name: "write".to_string(),
            description: "Write content to a file. Creates the file if it does not exist, or overwrites the existing file.".to_string(),
            input_schema: write_schema(),
            output_mode: ToolOutputMode::Mixed,
            execution_mode: ToolExecutionMode::Mutating,
            capability_tags: vec![ToolCapabilityTag::WriteFiles],
            supports_parallel: false,
        },
        ToolHandlerKind::Write,
    );

    plan.push(
        ToolSpec {
            name: "glob".to_string(),
            description: "Fast file pattern matching tool that works with any codebase size."
                .to_string(),
            input_schema: glob_schema(),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::ReadOnly,
            capability_tags: vec![ToolCapabilityTag::SearchWorkspace],
            supports_parallel: true,
        },
        ToolHandlerKind::Glob,
    );

    plan.push(
        ToolSpec {
            name: "grep".to_string(),
            description: "Fast content search tool that works with any codebase size.".to_string(),
            input_schema: grep_schema(),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::ReadOnly,
            capability_tags: vec![ToolCapabilityTag::SearchWorkspace],
            supports_parallel: true,
        },
        ToolHandlerKind::Grep,
    );

    plan.push(
        ToolSpec {
            name: "apply_patch".to_string(),
            description: APPLY_PATCH_DESCRIPTION.to_string(),
            input_schema: apply_patch_schema(),
            output_mode: ToolOutputMode::Mixed,
            execution_mode: ToolExecutionMode::Mutating,
            capability_tags: vec![ToolCapabilityTag::WriteFiles],
            supports_parallel: false,
        },
        ToolHandlerKind::ApplyPatch,
    );

    plan.push(
        ToolSpec {
            name: "update_plan".to_string(),
            description: "Updates the task plan.\nProvide an optional explanation and a list of plan items, each with a step and status.\nAt most one step can be in_progress at a time.".to_string(),
            input_schema: plan_schema(),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::Mutating,
            capability_tags: vec![],
            supports_parallel: false,
        },
        ToolHandlerKind::Plan,
    );

    plan.push(
        ToolSpec {
            name: "question".to_string(),
            description:
                "Ask the user a question to gather additional information or clarification."
                    .to_string(),
            input_schema: question_schema(),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::ReadOnly,
            capability_tags: vec![],
            supports_parallel: true,
        },
        ToolHandlerKind::Question,
    );

    plan.push(
        ToolSpec {
            name: "task".to_string(),
            description: "Launch a new agent to handle complex, multistep tasks autonomously."
                .to_string(),
            input_schema: task_schema(),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::Mutating,
            capability_tags: vec![],
            supports_parallel: false,
        },
        ToolHandlerKind::Task,
    );

    plan.push(
        ToolSpec {
            name: "webfetch".to_string(),
            description:
                "Fetches content from a specified URL and returns it in the requested format."
                    .to_string(),
            input_schema: webfetch_schema(),
            output_mode: ToolOutputMode::Mixed,
            execution_mode: ToolExecutionMode::ReadOnly,
            capability_tags: vec![ToolCapabilityTag::NetworkAccess],
            supports_parallel: true,
        },
        ToolHandlerKind::WebFetch,
    );

    plan.push(
        ToolSpec {
            name: "websearch".to_string(),
            description: "Search the web for information.".to_string(),
            input_schema: websearch_schema(),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::ReadOnly,
            capability_tags: vec![ToolCapabilityTag::NetworkAccess],
            supports_parallel: true,
        },
        ToolHandlerKind::WebSearch,
    );

    plan.push(
        ToolSpec {
            name: "skill".to_string(),
            description: "Load a specialized skill when the task at hand matches one of the available skills.".to_string(),
            input_schema: skill_schema(),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::ReadOnly,
            capability_tags: vec![],
            supports_parallel: true,
        },
        ToolHandlerKind::Skill,
    );

    plan.push(
        ToolSpec {
            name: "lsp".to_string(),
            description:
                "Get language server protocol information about a file at a specific position."
                    .to_string(),
            input_schema: lsp_schema(),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::ReadOnly,
            capability_tags: vec![ToolCapabilityTag::SearchWorkspace],
            supports_parallel: true,
        },
        ToolHandlerKind::Lsp,
    );

    plan.push(
        ToolSpec {
            name: "invalid".to_string(),
            description: "A tool that always returns an error. Useful for testing error handling."
                .to_string(),
            input_schema: invalid_schema(),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::ReadOnly,
            capability_tags: vec![],
            supports_parallel: true,
        },
        ToolHandlerKind::Invalid,
    );

    if config.use_unified_exec {
        plan.push(
            ToolSpec {
                name: "exec_command".to_string(),
                description:
                    "Run a shell command in a PTY and return output. If the process runs longer than yield_time_ms, a session_id is returned so you can interact with the process using write_stdin."
                        .to_string(),
                input_schema: exec_command_schema(),
                output_mode: ToolOutputMode::Mixed,
                execution_mode: ToolExecutionMode::Mutating,
                capability_tags: vec![ToolCapabilityTag::ExecuteProcess],
                supports_parallel: true,
            },
            ToolHandlerKind::ExecCommand,
        );
        plan.push(
            ToolSpec {
                name: "write_stdin".to_string(),
                description:
                    "Write bytes to stdin of a running unified exec session, or poll for output without writing. Returns any output produced since the last write_stdin."
                        .to_string(),
                input_schema: write_stdin_schema(),
                output_mode: ToolOutputMode::Mixed,
                execution_mode: ToolExecutionMode::Mutating,
                capability_tags: vec![ToolCapabilityTag::ExecuteProcess],
                supports_parallel: false,
            },
            ToolHandlerKind::WriteStdin,
        );
    }

    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_default_starts_empty() {
        let plan = ToolRegistryPlan::new();
        assert!(plan.specs.is_empty());
        assert!(plan.handlers.is_empty());
    }

    #[test]
    fn plan_push_adds_spec_and_handler() {
        let mut plan = ToolRegistryPlan::new();
        plan.push(
            ToolSpec::new("test", "desc", JsonSchema::string(None)),
            ToolHandlerKind::Read,
        );
        assert_eq!(plan.specs.len(), 1);
        assert_eq!(plan.handlers.len(), 1);
        assert_eq!(plan.handlers[0].0, ToolHandlerKind::Read);
        assert_eq!(plan.handlers[0].1, "test");
    }

    #[test]
    fn config_default_has_unified_exec_enabled() {
        let config = ToolPlanConfig::default();
        assert!(config.use_unified_exec);
        assert!(!config.use_shell_command);
    }

    #[test]
    fn config_validate_does_not_panic() {
        let config = ToolPlanConfig::default();
        config.validate(); // should not panic
    }

    #[test]
    fn schema_exec_command_requires_cmd() {
        let schema = exec_command_schema();
        let required = schema.required.as_ref().unwrap();
        assert!(required.contains(&"cmd".to_string()));
    }

    #[test]
    fn schema_write_stdin_requires_session_id() {
        let schema = write_stdin_schema();
        let required = schema.required.as_ref().unwrap();
        assert!(required.contains(&"session_id".to_string()));
    }

    #[test]
    fn schema_invalid_has_no_required() {
        let schema = invalid_schema();
        // invalid tool has no required fields and no properties
        assert!(schema.properties.as_ref().unwrap().is_empty());
    }

    #[test]
    fn bash_schema_has_command_and_cmd() {
        let schema = bash_schema();
        let props = schema.properties.as_ref().unwrap();
        assert!(props.contains_key("command"));
        assert!(props.contains_key("cmd"));
        assert!(props.contains_key("tty"));
    }

    #[test]
    fn shell_command_schema_has_cmd() {
        let schema = shell_command_schema();
        let props = schema.properties.as_ref().unwrap();
        assert!(props.contains_key("cmd"));
    }

    #[test]
    fn plan_builder_without_unified_exec() {
        let plan = build_tool_registry_plan(&ToolPlanConfig {
            use_unified_exec: false,
            ..ToolPlanConfig::default()
        });
        let handler_names: Vec<&str> = plan.handlers.iter().map(|(_, n)| n.as_str()).collect();
        assert!(!handler_names.contains(&"exec_command"));
        assert!(!handler_names.contains(&"write_stdin"));
    }
}
