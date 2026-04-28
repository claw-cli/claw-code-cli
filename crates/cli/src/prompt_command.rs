use anyhow::Result;
use devo_core::ModelCatalog;
use devo_core::PresetModelCatalog;
use devo_core::load_config;
use devo_core::resolve_provider_settings;
use devo_core::{AppConfig, AppConfigLoader, FileSystemAppConfigLoader};
use devo_utils::find_devo_home;

pub(crate) async fn run_prompt(
    input: &str,
    model_override: Option<&str>,
    log_level: Option<&str>,
) -> Result<()> {
    if let Some(level) = log_level {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(level))
            .try_init();
    }
    use devo_core::SessionConfig;
    use devo_core::SessionState;
    use devo_core::default_base_instructions;
    use devo_tools::ToolOrchestrator;
    use devo_tools::ToolRegistry;

    let cwd = std::env::current_dir()?;
    let _stored_config = load_config().unwrap_or_default();
    let mut resolved = resolve_provider_settings()
        .map_err(|e| anyhow::anyhow!("failed to resolve provider: {e}"))?;

    if let Some(model) = model_override {
        resolved.model = model.to_string();
    }

    let home_dir = find_devo_home()?;
    let app_config = FileSystemAppConfigLoader::new(home_dir.clone())
        .load(Some(cwd.as_path()))
        .unwrap_or_else(|_| AppConfig::default());
    let provider =
        devo_server::load_server_provider(&home_dir.join("config.toml"), Some(&resolved.model))?;

    let mut session_state = SessionState::new(
        SessionConfig {
            agents_md: app_config.agents_md_config(),
            ..SessionConfig::default()
        },
        cwd.clone(),
    );
    session_state.push_message(devo_core::Message::user(input.to_string()));

    let registry = {
        let mut reg = ToolRegistry::new();
        devo_tools::register_builtin_tools(&mut reg);
        std::sync::Arc::new(reg)
    };
    let orchestrator = ToolOrchestrator::new(std::sync::Arc::clone(&registry));
    let model_catalog = PresetModelCatalog::load()?;

    let turn_config = devo_core::TurnConfig {
        model: model_catalog
            .get(&resolved.model)
            .cloned()
            .unwrap_or_else(|| devo_core::Model {
                slug: resolved.model.clone(),
                base_instructions: default_base_instructions().to_string(),
                ..Default::default()
            }),
        thinking_selection: None,
    };

    eprintln!("devo [prompt] model={} sending...", resolved.model);

    let result = devo_core::query(
        &mut session_state,
        &turn_config,
        provider.provider.clone(),
        registry,
        &orchestrator,
        None,
    )
    .await;

    match result {
        Ok(()) => match latest_assistant_text(&session_state.messages) {
            Some(text) => println!("{}", text),
            None => eprintln!("devo [prompt] empty response"),
        },
        Err(e) => {
            anyhow::bail!("prompt failed: {e}");
        }
    }

    Ok(())
}

fn latest_assistant_text(messages: &[devo_core::Message]) -> Option<&str> {
    messages.iter().rev().find_map(|message| {
        if message.role != devo_core::Role::Assistant {
            return None;
        }
        message.content.iter().find_map(|block| match block {
            devo_core::ContentBlock::Reasoning { text } => Some(text.as_str()),
            devo_core::ContentBlock::Text { text } => Some(text.as_str()),
            devo_core::ContentBlock::ToolUse { .. }
            | devo_core::ContentBlock::ToolResult { .. } => None,
        })
    })
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::latest_assistant_text;
    use devo_core::ContentBlock;
    use devo_core::Message;
    use devo_core::Role;

    #[test]
    fn latest_assistant_text_returns_none_for_empty_messages() {
        assert_eq!(latest_assistant_text(&[]), None);
    }

    #[test]
    fn latest_assistant_text_ignores_user_messages() {
        assert_eq!(latest_assistant_text(&[Message::user("hello")]), None);
    }

    #[test]
    fn latest_assistant_text_returns_latest_assistant_text() {
        let messages = vec![
            Message::assistant_text("older"),
            Message::user("next"),
            Message::assistant_text("newer"),
        ];

        assert_eq!(latest_assistant_text(&messages), Some("newer"));
    }

    #[test]
    fn latest_assistant_text_skips_assistant_messages_without_text() {
        let messages = vec![
            Message::assistant_text("fallback"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "tool-call".to_string(),
                    name: "tool".to_string(),
                    input: serde_json::json!({}),
                }],
            },
        ];

        assert_eq!(latest_assistant_text(&messages), Some("fallback"));
    }

    #[test]
    fn latest_assistant_text_uses_first_text_block_within_latest_assistant_message() {
        let messages = vec![Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "tool-call".to_string(),
                    content: "ignored".to_string(),
                    is_error: false,
                },
                ContentBlock::Text {
                    text: "first text".to_string(),
                },
                ContentBlock::Text {
                    text: "second text".to_string(),
                },
            ],
        }];

        assert_eq!(latest_assistant_text(&messages), Some("first text"));
    }
}
