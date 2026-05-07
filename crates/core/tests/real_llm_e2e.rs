use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use devo_core::AgentsMdConfig;
use devo_core::Message;
use devo_core::Model;
use devo_core::ProviderWireApi;
use devo_core::QueryEvent;
use devo_core::SessionConfig;
use devo_core::SessionState;
use devo_core::ThinkingCapability;
use devo_core::TokenBudget;
use devo_core::TurnConfig;
use devo_core::default_base_instructions;
use devo_core::query;
use devo_provider::ModelProviderSDK;
use devo_provider::openai::OpenAIProvider;
use devo_tools::ToolRegistry;
use devo_tools::ToolRuntime;

#[derive(Debug, Clone)]
struct RealLlmConfig {
    base_url: String,
    api_key: String,
    model_slug: String,
    max_tokens: u32,
}

impl RealLlmConfig {
    fn from_env() -> Result<Self> {
        Ok(Self {
            base_url: std::env::var("DEVO_E2E_BASE_URL").context("missing DEVO_E2E_BASE_URL")?,
            api_key: std::env::var("DEVO_E2E_API_KEY").context("missing DEVO_E2E_API_KEY")?,
            model_slug: std::env::var("DEVO_E2E_MODEL").context("missing DEVO_E2E_MODEL")?,
            max_tokens: std::env::var("DEVO_E2E_MAX_TOKENS")
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(1024),
        })
    }

    // default take openai chat completions SDK as provider.
    fn provider(&self) -> Arc<dyn ModelProviderSDK> {
        Arc::new(OpenAIProvider::new(self.base_url.clone()).with_api_key(self.api_key.clone()))
    }

    fn model(&self) -> Model {
        Model {
            slug: self.model_slug.clone(),
            display_name: self.model_slug.clone(),
            provider: ProviderWireApi::OpenAIChatCompletions,
            thinking_capability: ThinkingCapability::Unsupported,
            base_instructions: default_base_instructions().to_string(),
            max_tokens: Some(self.max_tokens),
            ..Model::default()
        }
    }
}

fn write_file(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent directory for {}", path.display()))?;
    }
    std::fs::write(path, content).with_context(|| format!("write {}", path.display()))
}

fn unique_temp_dir(name: &str) -> Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time before UNIX_EPOCH")?
        .as_nanos();
    let path = std::env::temp_dir().join(format!("devo-real-llm-{name}-{nanos}"));
    std::fs::create_dir_all(&path)
        .with_context(|| format!("create temp directory {}", path.display()))?;
    Ok(path)
}

fn seed_long_history(session: &mut SessionState, turns: usize, chars_per_message: usize) {
    let payload = "history ".repeat(chars_per_message / 8);
    for index in 0..turns {
        session.push_message(Message::user(format!("user-{index}: {payload}")));
        session.push_message(Message::assistant_text(format!(
            "assistant-{index}: {payload}"
        )));
    }
}

fn collect_text_messages(session: &SessionState) -> Vec<String> {
    session
        .messages
        .iter()
        .flat_map(|message| {
            message.content.iter().filter_map(|block| match block {
                devo_core::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
        })
        .collect()
}

fn last_usage(events: &[QueryEvent]) -> Option<(usize, usize)> {
    events.iter().rev().find_map(|event| match event {
        QueryEvent::Usage {
            input_tokens,
            output_tokens,
            ..
        } => Some((*input_tokens, *output_tokens)),
        _ => None,
    })
}

async fn run_query(
    session: &mut SessionState,
    model: Model,
    thinking_selection: Option<String>,
) -> Result<Vec<QueryEvent>> {
    let registry = Arc::new(ToolRegistry::new());
    let runtime = ToolRuntime::new_without_permissions(Arc::clone(&registry));
    let provider = RealLlmConfig::from_env()?.provider();
    let seen_events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let callback_events = Arc::clone(&seen_events);
    let callback = Arc::new(move |event: QueryEvent| {
        callback_events
            .lock()
            .expect("query event callback mutex should not be poisoned")
            .push(event);
    });

    query(
        session,
        &TurnConfig {
            model,
            thinking_selection,
        },
        provider,
        registry,
        &runtime,
        Some(callback),
    )
    .await
    .context("execute live query")?;

    Ok(seen_events
        .lock()
        .expect("query event callback mutex should not be poisoned")
        .clone())
}

#[tokio::test]
#[ignore = "requires DEVO_E2E_BASE_URL, DEVO_E2E_API_KEY, and DEVO_E2E_MODEL"]
async fn real_llm_session_compaction_roundtrip() -> Result<()> {
    let config = RealLlmConfig::from_env()?;
    let workspace = unique_temp_dir("compaction")?;
    write_file(
        &workspace.join("AGENTS.md"),
        "You are running a live compaction integration test. Keep replies short.",
    )?;

    let mut session = SessionState::new(
        SessionConfig {
            token_budget: TokenBudget::new(4_096, 512),
            agents_md: AgentsMdConfig::default(),
            ..SessionConfig::default()
        },
        workspace.clone(),
    );
    seed_long_history(&mut session, 12, 10_000);
    session.last_input_tokens = 100_000;
    session.push_message(Message::user(
        "Reply in one short sentence confirming whether you still know the task.",
    ));

    let events = run_query(&mut session, config.model(), None).await?;
    let texts = collect_text_messages(&session);

    assert!(
        texts
            .iter()
            .any(|text| text.contains("<compaction_summary>")),
        "expected a compaction summary marker in session history after live compaction"
    );
    assert!(
        session
            .messages
            .last()
            .is_some_and(|message| message.role == devo_core::Role::Assistant),
        "expected an assistant response after compaction-triggered live query"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, QueryEvent::TurnComplete { .. })),
        "expected turn completion event from live provider"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "requires DEVO_E2E_BASE_URL, DEVO_E2E_API_KEY, and DEVO_E2E_MODEL"]
async fn real_llm_context_diffs_and_agents_updates() -> Result<()> {
    let config = RealLlmConfig::from_env()?;
    let root = unique_temp_dir("context-diff")?;
    let nested = root.join("nested");
    std::fs::create_dir_all(root.join(".git")).context("create fake project root marker")?;
    write_file(
        &root.join("AGENTS.md"),
        "Root instructions: prefer concise answers.",
    )?;
    write_file(
        &nested.join("AGENTS.md"),
        "Nested instructions: mention nested scope if asked.",
    )?;

    let mut session = SessionState::new(SessionConfig::default(), root.clone());
    session.push_message(Message::user("Say hello briefly."));
    run_query(&mut session, config.model(), None).await?;

    session.cwd = nested.clone();
    session.push_message(Message::user(
        "Reply briefly and acknowledge if any context changed.",
    ));
    run_query(&mut session, config.model(), Some(String::from("disabled"))).await?;

    let texts = collect_text_messages(&session);
    assert!(
        texts.iter().any(|text| text.contains("<context_changes>")),
        "expected context diff message after changing cwd/thinking selection"
    );
    assert!(
        texts
            .iter()
            .any(|text| text.contains("<agents_md_updates>")),
        "expected AGENTS.md diff message after entering nested directory"
    );
    assert!(
        session
            .latest_turn_context
            .as_ref()
            .is_some_and(|context| context.environment.cwd == nested),
        "expected latest turn context cwd to track the nested workspace"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "requires DEVO_E2E_BASE_URL, DEVO_E2E_API_KEY, and DEVO_E2E_MODEL"]
async fn real_llm_session_token_totals_accumulate_across_queries() -> Result<()> {
    let config = RealLlmConfig::from_env()?;
    let workspace = unique_temp_dir("accumulation")?;
    write_file(
        &workspace.join("AGENTS.md"),
        "You are running a live token accumulation integration test. Keep replies short.",
    )?;

    let mut session = SessionState::new(SessionConfig::default(), workspace);

    session.push_message(Message::user("Reply with a short confirmation."));
    let first_events = run_query(&mut session, config.model(), None).await?;
    let (first_input_tokens, first_output_tokens) =
        last_usage(&first_events).context("missing first query usage event")?;

    session.push_message(Message::user("Reply with another short confirmation."));
    let second_events = run_query(&mut session, config.model(), None).await?;
    let (second_input_tokens, second_output_tokens) =
        last_usage(&second_events).context("missing second query usage event")?;

    assert_eq!(
        session.total_input_tokens,
        first_input_tokens + second_input_tokens
    );
    assert_eq!(
        session.total_output_tokens,
        first_output_tokens + second_output_tokens
    );
    assert_eq!(session.last_input_tokens, second_input_tokens);
    assert!(session.total_input_tokens >= first_input_tokens);
    assert!(session.total_output_tokens >= first_output_tokens);

    Ok(())
}
