use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{Datelike, SecondsFormat, Utc};
use tokio::sync::Mutex;

use clawcr_core::{
    ContentBlock, ItemLine, ItemRecord, Message, Role, RolloutLine, SessionId, SessionMetaLine,
    SessionRecord, SessionTitleFinalSource, SessionTitleState, SessionTitleUpdatedLine, TextItem,
    ToolCallItem, ToolResultItem, TurnId, TurnItem, TurnLine, TurnRecord, TurnStatus, Worklog,
};

use crate::{
    execution::{RuntimeSession, ServerRuntimeDependencies},
    projection::history_item_from_turn_item,
    session::{SessionRuntimeStatus, SessionSummary},
    turn::TurnSummary,
};

/// Owns canonical append-only rollout persistence rooted at the server data directory.
#[derive(Debug, Clone)]
pub(crate) struct RolloutStore {
    /// Root data directory that contains the `sessions/` hierarchy.
    data_root: PathBuf,
}

impl RolloutStore {
    /// Creates a rollout store rooted at the supplied server home directory.
    pub(crate) fn new(data_root: PathBuf) -> Self {
        Self { data_root }
    }

    /// Constructs a canonical durable session record for a newly created session.
    pub(crate) fn create_session_record(
        &self,
        id: SessionId,
        created_at: chrono::DateTime<Utc>,
        cwd: PathBuf,
        title: Option<String>,
        model: Option<String>,
        model_provider: String,
        parent_session_id: Option<SessionId>,
    ) -> SessionRecord {
        let rollout_path = self.rollout_path(created_at, id);
        let title_state = title
            .as_ref()
            .map(|_| SessionTitleState::Final(SessionTitleFinalSource::ExplicitCreate))
            .unwrap_or(SessionTitleState::Unset);
        SessionRecord {
            id,
            rollout_path,
            created_at,
            updated_at: created_at,
            source: "cli".into(),
            agent_nickname: None,
            agent_role: None,
            agent_path: None,
            model_provider,
            model,
            reasoning_effort: None,
            cwd,
            cli_version: env!("CARGO_PKG_VERSION").into(),
            title,
            title_state,
            sandbox_policy: "workspace-write".into(),
            approval_mode: "on-request".into(),
            tokens_used: 0,
            first_user_message: None,
            archived_at: None,
            git_sha: None,
            git_branch: None,
            git_origin_url: None,
            parent_session_id,
            schema_version: 1,
        }
    }

    /// Appends the mandatory session header line to a durable rollout file.
    pub(crate) fn append_session_meta(&self, record: &SessionRecord) -> Result<()> {
        self.append_line(
            &record.rollout_path,
            &RolloutLine::SessionMeta(Box::new(SessionMetaLine {
                timestamp: Utc::now(),
                session: record.clone(),
            })),
        )
    }

    /// Appends one turn line to the durable rollout journal.
    pub(crate) fn append_turn(&self, record: &SessionRecord, turn: TurnRecord) -> Result<()> {
        self.append_line(
            &record.rollout_path,
            &RolloutLine::Turn(TurnLine {
                timestamp: Utc::now(),
                turn,
            }),
        )
    }

    /// Appends one item line to the durable rollout journal.
    pub(crate) fn append_item(&self, record: &SessionRecord, item: ItemRecord) -> Result<()> {
        self.append_line(
            &record.rollout_path,
            &RolloutLine::Item(ItemLine {
                timestamp: Utc::now(),
                item,
            }),
        )
    }

    /// Appends one session-title update line to the durable rollout journal.
    pub(crate) fn append_title_update(
        &self,
        record: &SessionRecord,
        title: String,
        title_state: SessionTitleState,
        previous_title: Option<String>,
    ) -> Result<()> {
        self.append_line(
            &record.rollout_path,
            &RolloutLine::SessionTitleUpdated(SessionTitleUpdatedLine {
                timestamp: Utc::now(),
                session_id: record.id,
                title,
                title_state,
                previous_title,
            }),
        )
    }

    /// Loads every durable session that can be rebuilt from canonical rollout files.
    pub(crate) fn load_sessions(
        &self,
        deps: &ServerRuntimeDependencies,
    ) -> Result<HashMap<SessionId, std::sync::Arc<Mutex<RuntimeSession>>>> {
        let mut sessions = HashMap::new();
        for rollout_path in self.rollout_paths()? {
            let recovered = self
                .load_session_from_rollout(&rollout_path, deps)
                .with_context(|| format!("replay rollout {}", rollout_path.display()))?;
            sessions.insert(recovered.summary.session_id, recovered.shared());
        }
        Ok(sessions)
    }

    fn load_session_from_rollout(
        &self,
        rollout_path: &Path,
        deps: &ServerRuntimeDependencies,
    ) -> Result<RuntimeSession> {
        let file = File::open(rollout_path)
            .with_context(|| format!("open rollout file {}", rollout_path.display()))?;
        let reader = BufReader::new(file);
        let mut replay = ReplayState::default();
        let mut lines = reader.lines().peekable();

        while let Some(line) = lines.next() {
            let line =
                line.with_context(|| format!("read line from {}", rollout_path.display()))?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<RolloutLine>(&line) {
                Ok(parsed) => replay.apply_line(parsed)?,
                Err(error) => {
                    if lines.peek().is_none() {
                        break;
                    }
                    return Err(error).with_context(|| {
                        format!("parse rollout line in {}", rollout_path.display())
                    });
                }
            }
        }

        replay.into_runtime_session(deps)
    }

    fn rollout_paths(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        let root = self.data_root.join("sessions");
        if !root.exists() {
            return Ok(files);
        }
        collect_rollout_files(&root, &mut files)?;
        files.sort();
        Ok(files)
    }

    fn rollout_path(&self, created_at: chrono::DateTime<Utc>, session_id: SessionId) -> PathBuf {
        let partition = self
            .data_root
            .join("sessions")
            .join(format!("{:04}", created_at.year()))
            .join(format!("{:02}", created_at.month()))
            .join(format!("{:02}", created_at.day()));
        let timestamp = created_at
            .to_rfc3339_opts(SecondsFormat::Secs, true)
            .replace(':', "-");
        partition.join(format!("rollout-{timestamp}-{session_id}.jsonl"))
    }

    fn append_line(&self, rollout_path: &Path, line: &RolloutLine) -> Result<()> {
        if let Some(parent) = rollout_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create rollout directory {}", parent.display()))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(rollout_path)
            .with_context(|| format!("open rollout file {}", rollout_path.display()))?;
        serde_json::to_writer(&mut file, line)
            .with_context(|| format!("serialize rollout line {}", rollout_path.display()))?;
        file.write_all(b"\n")
            .with_context(|| format!("write rollout newline {}", rollout_path.display()))?;
        file.flush()
            .with_context(|| format!("flush rollout file {}", rollout_path.display()))?;
        Ok(())
    }
}

#[derive(Default)]
struct ReplayState {
    session: Option<SessionRecord>,
    latest_turn: Option<TurnRecord>,
    latest_turn_summary: Option<TurnSummary>,
    loaded_item_count: u64,
    next_item_seq: u64,
    turns_seen: u32,
    total_input_tokens: usize,
    total_output_tokens: usize,
    total_cache_creation_tokens: usize,
    total_cache_read_tokens: usize,
    last_input_tokens: usize,
    messages: Vec<Message>,
    history_items: Vec<crate::SessionHistoryItem>,
    pending_items: Vec<ReplayHistoryItem>,
}

impl ReplayState {
    fn apply_line(&mut self, line: RolloutLine) -> Result<()> {
        match line {
            RolloutLine::SessionMeta(line) => {
                self.session = Some(line.session);
            }
            RolloutLine::Turn(line) => {
                self.turns_seen = self.turns_seen.max(line.turn.sequence);
                if let Some(usage) = &line.turn.usage {
                    self.total_input_tokens += usage.input_tokens as usize;
                    self.total_output_tokens += usage.output_tokens as usize;
                    self.total_cache_creation_tokens +=
                        usage.cache_creation_input_tokens.unwrap_or(0) as usize;
                    self.total_cache_read_tokens +=
                        usage.cache_read_input_tokens.unwrap_or(0) as usize;
                    self.last_input_tokens = usage.input_tokens as usize;
                }
                self.latest_turn_summary = Some(TurnSummary {
                    turn_id: line.turn.id,
                    session_id: line.turn.session_id,
                    sequence: line.turn.sequence,
                    status: line.turn.status.clone(),
                    model_slug: line.turn.model_slug.clone(),
                    started_at: line.turn.started_at,
                    completed_at: line.turn.completed_at,
                    usage: line.turn.usage.clone(),
                });
                self.latest_turn = Some(line.turn);
            }
            RolloutLine::Item(line) => {
                self.loaded_item_count += 1;
                self.next_item_seq = self.next_item_seq.max(line.item.seq + 1);
                self.collect_item_line(line.item);
            }
            RolloutLine::SessionTitleUpdated(line) => {
                let session = self
                    .session
                    .as_mut()
                    .context("title update without session header")?;
                session.title = Some(line.title);
                session.title_state = line.title_state;
                session.updated_at = line.timestamp;
            }
            RolloutLine::CompactionSnapshot(_) => {}
        }
        Ok(())
    }

    fn into_runtime_session(self, deps: &ServerRuntimeDependencies) -> Result<RuntimeSession> {
        let record = self.session.context("missing SessionMetaLine in rollout")?;
        let mut core_session = deps.new_session_state(record.id, record.cwd.clone());
        let mut ordered_items = self.pending_items;
        ordered_items.sort_by(|left, right| {
            left.seq
                .cmp(&right.seq)
                .then_with(|| left.timestamp.cmp(&right.timestamp))
                .then_with(|| left.record_timestamp.cmp(&right.record_timestamp))
                .then_with(|| left.line_timestamp.cmp(&right.line_timestamp))
                .then_with(|| left.bucket_priority.cmp(&right.bucket_priority))
                .then_with(|| left.intra_record_order.cmp(&right.intra_record_order))
        });

        let mut replayed_messages = self.messages;
        let mut replayed_history_items = self.history_items;
        let mut tool_names_by_id = HashMap::new();
        for pending_item in ordered_items {
            apply_turn_item(
                &mut replayed_messages,
                &mut replayed_history_items,
                &mut tool_names_by_id,
                pending_item.turn_item,
            );
        }

        core_session.messages = replayed_messages;
        core_session.turn_count = self.turns_seen as usize;
        core_session.total_input_tokens = self.total_input_tokens;
        core_session.total_output_tokens = self.total_output_tokens;
        core_session.total_cache_creation_tokens = self.total_cache_creation_tokens;
        core_session.total_cache_read_tokens = self.total_cache_read_tokens;
        core_session.last_input_tokens = self.last_input_tokens;

        let summary = SessionSummary {
            session_id: record.id,
            cwd: record.cwd.clone(),
            created_at: record.created_at,
            updated_at: record.updated_at,
            title: record.title.clone(),
            title_state: record.title_state.clone(),
            ephemeral: false,
            resolved_model: record.model.clone(),
            total_input_tokens: self.total_input_tokens,
            total_output_tokens: self.total_output_tokens,
            status: SessionRuntimeStatus::Idle,
        };

        Ok(RuntimeSession {
            record: Some(record),
            summary,
            core_session: std::sync::Arc::new(Mutex::new(core_session)),
            active_turn: None,
            latest_turn: self.latest_turn_summary,
            loaded_item_count: self.loaded_item_count,
            history_items: replayed_history_items,
            steering_queue: std::collections::VecDeque::new(),
            active_task: None,
            next_item_seq: self.next_item_seq.max(1),
        })
    }

    fn collect_item_line(&mut self, item: ItemRecord) {
        let record_timestamp = item.timestamp;
        let line_timestamp = record_timestamp;
        let seq = item.seq;
        let mut intra_record_order = 0usize;

        for turn_item in item.output_items {
            self.pending_items.push(ReplayHistoryItem {
                seq,
                timestamp: record_timestamp,
                record_timestamp,
                line_timestamp,
                bucket_priority: 0,
                intra_record_order,
                turn_item,
            });
            intra_record_order += 1;
        }

        for turn_item in item.input_items {
            self.pending_items.push(ReplayHistoryItem {
                seq,
                timestamp: record_timestamp,
                record_timestamp,
                line_timestamp,
                bucket_priority: 1,
                intra_record_order,
                turn_item,
            });
            intra_record_order += 1;
        }
    }
}

#[derive(Debug)]
struct ReplayHistoryItem {
    seq: u64,
    timestamp: chrono::DateTime<Utc>,
    record_timestamp: chrono::DateTime<Utc>,
    line_timestamp: chrono::DateTime<Utc>,
    bucket_priority: u8,
    intra_record_order: usize,
    turn_item: TurnItem,
}

fn apply_turn_item(
    messages: &mut Vec<Message>,
    history_items: &mut Vec<crate::SessionHistoryItem>,
    tool_names_by_id: &mut HashMap<String, String>,
    item: TurnItem,
) {
    if let Some(history_item) = replay_history_item_from_turn_item(&item, tool_names_by_id) {
        history_items.push(history_item);
    }
    match item {
        TurnItem::UserMessage(TextItem { text }) | TurnItem::SteerInput(TextItem { text }) => {
            messages.push(Message::user(text));
        }
        TurnItem::AgentMessage(TextItem { text }) => {
            messages.push(Message::assistant_text(text));
        }
        TurnItem::ToolCall(ToolCallItem {
            tool_call_id,
            tool_name,
            input,
        }) => {
            tool_names_by_id.insert(tool_call_id.clone(), tool_name.clone());
            match messages.last_mut() {
                Some(message) if message.role == Role::Assistant => {
                    message.content.push(ContentBlock::ToolUse {
                        id: tool_call_id,
                        name: tool_name,
                        input,
                    });
                }
                _ => {
                    messages.push(Message {
                        role: Role::Assistant,
                        content: vec![ContentBlock::ToolUse {
                            id: tool_call_id,
                            name: tool_name,
                            input,
                        }],
                    });
                }
            }
        }
        TurnItem::ToolResult(ToolResultItem {
            tool_call_id,
            output,
            is_error,
        }) => {
            let content = match output {
                serde_json::Value::String(text) => text,
                other => other.to_string(),
            };
            match messages.last_mut() {
                Some(message)
                    if message.role == Role::User
                        && message
                            .content
                            .iter()
                            .all(|block| matches!(block, ContentBlock::ToolResult { .. })) =>
                {
                    message.content.push(ContentBlock::ToolResult {
                        tool_use_id: tool_call_id,
                        content,
                        is_error,
                    });
                }
                _ => {
                    messages.push(Message {
                        role: Role::User,
                        content: vec![ContentBlock::ToolResult {
                            tool_use_id: tool_call_id,
                            content,
                            is_error,
                        }],
                    });
                }
            }
        }
        TurnItem::Reasoning(TextItem { text })
        | TurnItem::WebSearch(TextItem { text })
        | TurnItem::ImageGeneration(TextItem { text })
        | TurnItem::ContextCompaction(TextItem { text })
        | TurnItem::HookPrompt(TextItem { text }) => {
            messages.push(Message::assistant_text(text));
        }
        // Plan items are replay-visible transcript data, but they are not part of the
        // provider-facing tool-call/result conversation state used for subsequent turns.
        TurnItem::Plan(_) => {}
        TurnItem::ToolProgress(_)
        | TurnItem::ApprovalRequest(_)
        | TurnItem::ApprovalDecision(_) => {}
    }
}

fn replay_history_item_from_turn_item(
    item: &TurnItem,
    tool_names_by_id: &HashMap<String, String>,
) -> Option<crate::SessionHistoryItem> {
    match item {
        TurnItem::ToolResult(ToolResultItem {
            tool_call_id,
            is_error,
            ..
        }) if matches!(
            tool_names_by_id.get(tool_call_id).map(String::as_str),
            Some("update_plan")
        ) && !is_error =>
        {
            None
        }
        other => history_item_from_turn_item(other),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use pretty_assertions::assert_eq;

    use super::ReplayState;
    use clawcr_core::{
        ItemId, ItemLine, ItemRecord, RolloutLine, SessionId, TextItem, ToolCallItem, TurnId,
        TurnItem,
    };

    #[test]
    fn replay_orders_items_by_sequence_before_timestamp() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let earlier = Utc.with_ymd_and_hms(2026, 4, 6, 8, 0, 0).unwrap();
        let later = Utc.with_ymd_and_hms(2026, 4, 6, 8, 0, 1).unwrap();
        let mut replay = ReplayState::default();

        replay
            .apply_line(RolloutLine::Item(ItemLine {
                timestamp: earlier,
                item: ItemRecord {
                    id: ItemId::new(),
                    session_id,
                    turn_id,
                    seq: 2,
                    timestamp: earlier,
                    attempt_placement: None,
                    turn_status: None,
                    sibling_turn_ids: Vec::new(),
                    input_items: Vec::new(),
                    output_items: vec![TurnItem::ToolCall(ToolCallItem {
                        tool_call_id: "call-1".to_string(),
                        tool_name: "bash".to_string(),
                        input: serde_json::json!({"command":"date"}),
                    })],
                    worklog: None,
                    error: None,
                    schema_version: 1,
                },
            }))
            .expect("replay later-seq line");
        replay
            .apply_line(RolloutLine::Item(ItemLine {
                timestamp: later,
                item: ItemRecord {
                    id: ItemId::new(),
                    session_id,
                    turn_id,
                    seq: 1,
                    timestamp: later,
                    attempt_placement: None,
                    turn_status: None,
                    sibling_turn_ids: Vec::new(),
                    output_items: vec![TurnItem::AgentMessage(TextItem {
                        text: "assistant 1".to_string(),
                    })],
                    input_items: Vec::new(),
                    worklog: None,
                    error: None,
                    schema_version: 1,
                },
            }))
            .expect("replay earlier-seq line");

        let mut items = replay.pending_items;
        items.sort_by(|left, right| {
            left.seq
                .cmp(&right.seq)
                .then_with(|| left.timestamp.cmp(&right.timestamp))
                .then_with(|| left.intra_record_order.cmp(&right.intra_record_order))
        });

        let titles = items
            .into_iter()
            .map(|item| match item.turn_item {
                TurnItem::AgentMessage(TextItem { text }) => text,
                TurnItem::ToolCall(ToolCallItem { input, .. }) => {
                    input["command"].as_str().unwrap().to_string()
                }
                other => format!("{other:?}"),
            })
            .collect::<Vec<_>>();

        assert_eq!(titles, vec!["assistant 1", "date"]);
    }
}

fn collect_rollout_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(root).with_context(|| format!("read dir {}", root.display()))? {
        let entry = entry.with_context(|| format!("read entry in {}", root.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("read file type for {}", path.display()))?;
        if file_type.is_dir() {
            collect_rollout_files(&path, files)?;
        } else if file_type.is_file()
            && path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
        {
            files.push(path);
        }
    }
    Ok(())
}

/// Creates one canonical persisted turn record from the transport-facing runtime state.
pub(crate) fn build_turn_record(turn: &TurnSummary) -> TurnRecord {
    TurnRecord {
        id: turn.turn_id,
        session_id: turn.session_id,
        sequence: turn.sequence,
        started_at: turn.started_at,
        completed_at: turn.completed_at,
        status: turn.status.clone(),
        model_slug: turn.model_slug.clone(),
        input_token_estimate: None,
        usage: turn.usage.clone(),
        schema_version: 1,
    }
}

/// Creates one canonical persisted item record from a normalized turn item payload.
pub(crate) fn build_item_record(
    session_id: SessionId,
    turn_id: TurnId,
    item_id: clawcr_core::ItemId,
    seq: u64,
    item: TurnItem,
    turn_status: Option<TurnStatus>,
    worklog: Option<Worklog>,
) -> ItemRecord {
    build_item_record_with_output_items(
        session_id,
        turn_id,
        item_id,
        seq,
        vec![item],
        turn_status,
        worklog,
    )
}

/// Creates one canonical persisted item record from normalized output payloads.
pub(crate) fn build_item_record_with_output_items(
    session_id: SessionId,
    turn_id: TurnId,
    item_id: clawcr_core::ItemId,
    seq: u64,
    output_items: Vec<TurnItem>,
    turn_status: Option<TurnStatus>,
    worklog: Option<Worklog>,
) -> ItemRecord {
    ItemRecord {
        id: item_id,
        session_id,
        turn_id,
        seq,
        timestamp: Utc::now(),
        attempt_placement: None,
        turn_status,
        sibling_turn_ids: Vec::new(),
        input_items: Vec::new(),
        output_items,
        worklog,
        error: None,
        schema_version: 1,
    }
}
