mod apply_patch;
mod bash;
mod exec_command;
mod file_write;
mod glob;
mod grep;
mod invalid;
mod lsp;
mod plan;
mod question;
mod read;
mod shell_command;
mod skill;
mod task;
mod webfetch;
mod websearch;

pub use apply_patch::ApplyPatchHandler;
pub use bash::BashHandler;
pub use exec_command::{ExecCommandHandler, WriteStdinHandler};
pub use file_write::WriteHandler;
pub use glob::GlobHandler;
pub use grep::GrepHandler;
pub use invalid::InvalidHandler;
pub use lsp::LspHandler;
pub use plan::PlanHandler;
pub use question::QuestionHandler;
pub use read::ReadHandler;
pub use shell_command::ShellCommandHandler;
pub use skill::SkillHandler;
pub use task::TaskHandler;
pub use webfetch::WebFetchHandler;
pub use websearch::WebSearchHandler;

use std::sync::Arc;

use crate::handler_kind::ToolHandlerKind;
use crate::registry::ToolRegistryBuilder;
use crate::registry_plan::{ToolPlanConfig, build_tool_registry_plan};
use crate::tool_handler::ToolHandler;
use crate::unified_exec::store::ProcessStore;

pub fn build_registry_from_plan(config: &ToolPlanConfig) -> crate::registry::ToolRegistry {
    let plan = build_tool_registry_plan(config);
    let mut builder = ToolRegistryBuilder::new();

    for spec in plan.specs {
        builder.push_spec(spec);
    }

    let process_store = Arc::new(ProcessStore::new());
    builder.set_unified_exec_store(Arc::clone(&process_store));

    for (kind, name) in plan.handlers {
        let handler: Arc<dyn ToolHandler> = match kind {
            ToolHandlerKind::Bash => Arc::new(BashHandler),
            ToolHandlerKind::ShellCommand => Arc::new(ShellCommandHandler),
            ToolHandlerKind::Read => Arc::new(ReadHandler),
            ToolHandlerKind::Write => Arc::new(WriteHandler),
            ToolHandlerKind::Glob => Arc::new(GlobHandler),
            ToolHandlerKind::Grep => Arc::new(GrepHandler),
            ToolHandlerKind::ApplyPatch => Arc::new(ApplyPatchHandler),
            ToolHandlerKind::Plan => Arc::new(PlanHandler),
            ToolHandlerKind::Question => Arc::new(QuestionHandler),
            ToolHandlerKind::Task => Arc::new(TaskHandler),
            ToolHandlerKind::WebFetch => Arc::new(WebFetchHandler),
            ToolHandlerKind::WebSearch => Arc::new(WebSearchHandler),
            ToolHandlerKind::Skill => Arc::new(SkillHandler),
            ToolHandlerKind::Lsp => Arc::new(LspHandler),
            ToolHandlerKind::Invalid => Arc::new(InvalidHandler),
            ToolHandlerKind::ExecCommand => {
                Arc::new(ExecCommandHandler::new(Arc::clone(&process_store)))
            }
            ToolHandlerKind::WriteStdin => {
                Arc::new(WriteStdinHandler::new(Arc::clone(&process_store)))
            }
        };
        builder.register_handler(&name, handler);
    }

    builder.build()
}
