use std::collections::HashMap;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use anyhow::Context;
use anyhow::Result;
use chrono::Datelike;
use chrono::SecondsFormat;
use chrono::Utc;
use tokio::sync::Mutex;

use devo_core::CompactionSnapshotLine;
use devo_core::ContentBlock;
use devo_core::ItemId;
use devo_core::ItemLine;
use devo_core::ItemRecord;
use devo_core::Message;
use devo_core::Role;
use devo_core::RolloutLine;
use devo_core::SessionId;
use devo_core::SessionMetaLine;
use devo_core::SessionRecord;
use devo_core::SessionTitleFinalSource;
use devo_core::SessionTitleState;
use devo_core::SessionTitleUpdatedLine;
use devo_core::TextItem;
use devo_core::ToolCallItem;
use devo_core::ToolResultItem;
use devo_core::TurnId;
use devo_core::TurnItem;
use devo_core::TurnLine;
use devo_core::TurnRecord;
use devo_core::TurnStatus;
use devo_core::Worklog;

use crate::execution::PersistedTurnItem;
use crate::execution::RuntimeSession;
use crate::execution::ServerRuntimeDependencies;
use crate::projection::history_item_from_turn_item;
use crate::session::SessionMetadata;
use crate::session::SessionRuntimeStatus;
use crate::turn::TurnMetadata;

/// Owns canonical append-only rollout persistence rooted at the server data directory.
pub(crate) struct RolloutStore {
    /// Root data directory that contains the `sessions/` hierarchy.
    data_root: PathBuf,
    /// Per-file locks that serialise concurrent writes to the same rollout file,
    /// preventing interleaved JSON lines.
    file_locks: Arc<StdMutex<HashMap<PathBuf, Arc<StdMutex<()>>>>>,
}

impl std::fmt::Debug for RolloutStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RolloutStore")
            .field("data_root", &self.data_root)
            .finish()
    }
}

impl Clone for RolloutStore {
    fn clone(&self) -> Self {
        Self {
            data_root: self.data_root.clone(),
            file_locks: Arc::clone(&self.file_locks),
        }
    }
}

impl RolloutStore {
    /// Creates a rollout store rooted at the supplied server home directory.
    pub(crate) fn new(data_root: PathBuf) -> Self {
        Self {
            data_root,
            file_locks: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    /// Constructs a canonical durable session record for a newly created session.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_session_record(
        &self,
        id: SessionId,
        created_at: chrono::DateTime<Utc>,
        cwd: PathBuf,
        title: Option<String>,
        model: Option<String>,
        thinking: Option<String>,
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
            thinking,
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
            session_context: None,
            latest_turn_context: None,
            schema_version: 2,
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
            &RolloutLine::Turn(Box::new(TurnLine {
                timestamp: Utc::now(),
                turn,
            })),
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

    /// Appends one compaction snapshot line to the durable rollout journal.
    pub(crate) fn append_compaction_snapshot(
        &self,
        record: &SessionRecord,
        snapshot: CompactionSnapshotLine,
    ) -> Result<()> {
        self.append_line(
            &record.rollout_path,
            &RolloutLine::CompactionSnapshot(Box::new(snapshot)),
        )
    }

    /// Loads every durable session that can be rebuilt from canonical rollout files.
    pub(crate) fn load_sessions(
        &self,
        deps: &ServerRuntimeDependencies,
    ) -> Result<HashMap<SessionId, std::sync::Arc<Mutex<RuntimeSession>>>> {
        let mut sessions = HashMap::new();
        for rollout_path in self.rollout_paths()? {
            match self
                .load_session_from_rollout(&rollout_path, deps)
                .with_context(|| format!("replay rollout {}", rollout_path.display()))
            {
                Ok(recovered) => {
                    sessions.insert(recovered.summary.session_id, recovered.shared());
                }
                Err(error) => {
                    tracing::warn!(
                        rollout_path = %rollout_path.display(),
                        error = %error,
                        "failed to replay rollout; skipping persisted session"
                    );
                }
            }
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
        let mut lines = reader.lines().enumerate().peekable();

        while let Some((line_index, line)) = lines.next() {
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
                    tracing::warn!(
                        rollout_path = %rollout_path.display(),
                        line_number = line_index + 1,
                        error = %error,
                        "skipping corrupt rollout line"
                    );
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
        // Acquire a per-file lock so concurrent writes to the same rollout file
        // do not interleave their JSON payloads.
        let file_lock = {
            let mut locks = self
                .file_locks
                .lock()
                .expect("rollout file-locks table poisoned");
            locks
                .entry(rollout_path.to_path_buf())
                .or_insert_with(|| Arc::new(StdMutex::new(())))
                .clone()
        };
        let _guard = file_lock.lock().expect("rollout per-file lock poisoned");
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
    latest_turn_metadata: Option<TurnMetadata>,
    loaded_item_count: u64,
    next_item_seq: u64,
    turns_seen: u32,
    total_input_tokens: usize,
    total_output_tokens: usize,
    total_cache_creation_tokens: usize,
    total_cache_read_tokens: usize,
    last_input_tokens: usize,
    session_context: Option<devo_core::SessionContext>,
    latest_turn_context: Option<devo_core::TurnContext>,
    messages: Vec<Message>,
    history_items: Vec<crate::SessionHistoryItem>,
    pending_items: Vec<ReplayHistoryItem>,
    latest_compaction_snapshot: Option<CompactionSnapshotLine>,
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
                self.latest_turn_metadata = Some(TurnMetadata {
                    turn_id: line.turn.id,
                    session_id: line.turn.session_id,
                    sequence: line.turn.sequence,
                    status: line.turn.status.clone(),
                    kind: line.turn.kind.clone(),
                    model: line.turn.model.clone(),
                    thinking: line.turn.thinking.clone(),
                    request_model: line.turn.request_model.clone(),
                    request_thinking: line.turn.request_thinking.clone(),
                    started_at: line.turn.started_at,
                    completed_at: line.turn.completed_at,
                    usage: line.turn.usage.clone(),
                });
                if let Some(session_context) = line.turn.session_context.clone() {
                    self.session_context = Some(session_context);
                }
                if let Some(turn_context) = line.turn.turn_context.clone() {
                    self.latest_turn_context = Some(turn_context);
                }
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
            RolloutLine::CompactionSnapshot(line) => {
                self.latest_compaction_snapshot = Some(*line);
            }
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
        let mut replayed_persisted_turn_items = Vec::with_capacity(ordered_items.len());
        let mut tool_names_by_id = HashMap::new();
        for pending_item in ordered_items {
            apply_turn_item(
                &mut replayed_messages,
                &mut replayed_history_items,
                &mut tool_names_by_id,
                pending_item.turn_item.clone(),
            );
            replayed_persisted_turn_items.push(PersistedTurnItem {
                item_id: pending_item.item_id,
                turn_item: pending_item.turn_item,
            });
        }

        core_session.messages = replayed_messages;
        core_session.prompt_messages =
            self.latest_compaction_snapshot
                .as_ref()
                .and_then(|snapshot| {
                    build_prompt_messages_from_snapshot(&replayed_persisted_turn_items, snapshot)
                });
        core_session.session_context = self
            .session_context
            .or_else(|| record.session_context.clone());
        core_session.latest_turn_context = self
            .latest_turn_context
            .or_else(|| record.latest_turn_context.clone());
        core_session.turn_count = self.turns_seen as usize;
        core_session.total_input_tokens = self.total_input_tokens;
        core_session.total_output_tokens = self.total_output_tokens;
        core_session.total_cache_creation_tokens = self.total_cache_creation_tokens;
        core_session.total_cache_read_tokens = self.total_cache_read_tokens;
        core_session.last_input_tokens = self.last_input_tokens;
        core_session.prompt_token_estimate = core_session
            .prompt_source_messages()
            .iter()
            .map(|message| serde_json::to_string(message).map_or(0, |json| json.len()))
            .sum::<usize>()
            .div_ceil(4);

        let summary = SessionMetadata {
            session_id: record.id,
            cwd: record.cwd.clone(),
            created_at: record.created_at,
            updated_at: record.updated_at,
            title: record.title.clone(),
            title_state: record.title_state.clone(),
            ephemeral: false,
            model: record.model.clone(),
            thinking: record.thinking.clone(),
            total_input_tokens: self.total_input_tokens,
            total_output_tokens: self.total_output_tokens,
            prompt_token_estimate: core_session.prompt_token_estimate,
            status: SessionRuntimeStatus::Idle,
        };

        Ok(RuntimeSession {
            record: Some(record),
            summary,
            core_session: std::sync::Arc::new(Mutex::new(core_session)),
            active_turn: None,
            latest_turn: self.latest_turn_metadata,
            loaded_item_count: self.loaded_item_count,
            history_items: replayed_history_items,
            persisted_turn_items: replayed_persisted_turn_items,
            latest_compaction_snapshot: self.latest_compaction_snapshot,
            steering_queue: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::VecDeque::new(),
            )),
            active_task: None,
            next_item_seq: self.next_item_seq.max(1),
        })
    }

    fn collect_item_line(&mut self, item: ItemRecord) {
        let item_id = item.id;
        let record_timestamp = item.timestamp;
        let line_timestamp = record_timestamp;
        let seq = item.seq;
        let mut intra_record_order = 0usize;

        for turn_item in item.output_items {
            self.pending_items.push(ReplayHistoryItem {
                item_id,
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
                item_id,
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

#[derive(Debug, Clone)]
struct ReplayHistoryItem {
    item_id: ItemId,
    seq: u64,
    timestamp: chrono::DateTime<Utc>,
    record_timestamp: chrono::DateTime<Utc>,
    line_timestamp: chrono::DateTime<Utc>,
    bucket_priority: u8,
    intra_record_order: usize,
    turn_item: TurnItem,
}

fn build_prompt_messages_from_snapshot(
    persisted_turn_items: &[PersistedTurnItem],
    snapshot: &CompactionSnapshotLine,
) -> Option<Vec<Message>> {
    let ordered_items = persisted_turn_items
        .iter()
        .filter(|item| prompt_visible_turn_item(&item.turn_item))
        .collect::<Vec<_>>();
    let summary_index = ordered_items
        .iter()
        .position(|item| item.item_id == snapshot.summary_item_id)?;

    let mut by_item_id: HashMap<ItemId, PersistedTurnItem> = ordered_items
        .iter()
        .cloned()
        .map(|item| (item.item_id, item.clone()))
        .collect();

    let mut rebuilt = Vec::new();
    if let Some(summary_item) = by_item_id.remove(&snapshot.summary_item_id) {
        rebuilt.push(summary_item);
    }

    for preserved_id in &snapshot.preserved_item_ids {
        if let Some(item) = by_item_id.remove(preserved_id) {
            rebuilt.push(item);
        }
    }

    rebuilt.extend(
        ordered_items
            .iter()
            .skip(summary_index + 1)
            .filter(|item| item.item_id != snapshot.summary_item_id)
            .filter(|item| !snapshot.preserved_item_ids.contains(&item.item_id))
            .map(|item| (*item).clone()),
    );

    let mut messages = Vec::new();
    let mut tool_names_by_id = HashMap::new();
    for item in rebuilt {
        apply_prompt_turn_item(&mut messages, &mut tool_names_by_id, item.turn_item.clone());
    }
    Some(messages)
}

fn prompt_visible_turn_item(item: &TurnItem) -> bool {
    matches!(
        item,
        TurnItem::ContextCompaction(_)
            | TurnItem::UserMessage(_)
            | TurnItem::SteerInput(_)
            | TurnItem::AgentMessage(_)
            | TurnItem::Reasoning(_)
            | TurnItem::ToolCall(_)
            | TurnItem::ToolResult(_)
            | TurnItem::Plan(_)
            | TurnItem::WebSearch(_)
            | TurnItem::ImageGeneration(_)
            | TurnItem::HookPrompt(_)
    )
}

fn apply_turn_item(
    messages: &mut Vec<Message>,
    history_items: &mut Vec<crate::SessionHistoryItem>,
    tool_names_by_id: &mut HashMap<String, String>,
    item: TurnItem,
) {
    let item = match item {
        TurnItem::ToolCall(ToolCallItem {
            tool_call_id,
            tool_name,
            input,
        }) => {
            tool_names_by_id.insert(tool_call_id.clone(), tool_name.clone());
            TurnItem::ToolCall(ToolCallItem {
                tool_call_id,
                tool_name,
                input,
            })
        }
        TurnItem::ToolResult(ToolResultItem {
            tool_call_id,
            tool_name,
            output,
            is_error,
        }) => TurnItem::ToolResult(ToolResultItem {
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.or_else(|| tool_names_by_id.get(&tool_call_id).cloned()),
            output,
            is_error,
        }),
        other => other,
    };

    if let Some(history_item) = history_item_from_turn_item(&item) {
        history_items.push(history_item);
    }
    match item {
        TurnItem::UserMessage(TextItem { text }) | TurnItem::SteerInput(TextItem { text }) => {
            messages.push(Message::user(text));
        }
        TurnItem::AgentMessage(TextItem { text }) => match messages.last_mut() {
            Some(message) if message.role == Role::Assistant => {
                message.content.push(ContentBlock::Text { text });
            }
            _ => {
                messages.push(Message::assistant_text(text));
            }
        },
        TurnItem::ToolCall(ToolCallItem {
            tool_call_id,
            tool_name,
            input,
        }) => match messages.last_mut() {
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
        },
        TurnItem::ToolResult(ToolResultItem {
            tool_call_id,
            tool_name: _,
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
        TurnItem::Plan(TextItem { text })
        | TurnItem::WebSearch(TextItem { text })
        | TurnItem::ImageGeneration(TextItem { text })
        | TurnItem::HookPrompt(TextItem { text }) => {
            messages.push(Message::assistant_text(text));
        }
        TurnItem::ContextCompaction(TextItem { .. }) => {}
        TurnItem::Reasoning(TextItem { text }) => match messages.last_mut() {
            Some(message) if message.role == Role::Assistant => {
                message.content.push(ContentBlock::Reasoning { text });
            }
            _ => {
                messages.push(Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Reasoning { text }],
                });
            }
        },
        TurnItem::ToolProgress(_)
        | TurnItem::ApprovalRequest(_)
        | TurnItem::ApprovalDecision(_) => {}
    }
}

fn apply_prompt_turn_item(
    messages: &mut Vec<Message>,
    tool_names_by_id: &mut HashMap<String, String>,
    item: TurnItem,
) {
    let item = match item {
        TurnItem::ToolCall(ToolCallItem {
            tool_call_id,
            tool_name,
            input,
        }) => {
            tool_names_by_id.insert(tool_call_id.clone(), tool_name.clone());
            TurnItem::ToolCall(ToolCallItem {
                tool_call_id,
                tool_name,
                input,
            })
        }
        TurnItem::ToolResult(ToolResultItem {
            tool_call_id,
            tool_name,
            output,
            is_error,
        }) => TurnItem::ToolResult(ToolResultItem {
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.or_else(|| tool_names_by_id.get(&tool_call_id).cloned()),
            output,
            is_error,
        }),
        other => other,
    };

    match item {
        TurnItem::UserMessage(TextItem { text }) | TurnItem::SteerInput(TextItem { text }) => {
            messages.push(Message::user(text));
        }
        TurnItem::AgentMessage(TextItem { text })
        | TurnItem::Plan(TextItem { text })
        | TurnItem::WebSearch(TextItem { text })
        | TurnItem::ImageGeneration(TextItem { text })
        | TurnItem::ContextCompaction(TextItem { text })
        | TurnItem::HookPrompt(TextItem { text }) => {
            messages.push(Message::assistant_text(text));
        }
        TurnItem::ToolCall(ToolCallItem {
            tool_call_id,
            tool_name,
            input,
        }) => match messages.last_mut() {
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
        },
        TurnItem::ToolResult(ToolResultItem {
            tool_call_id,
            tool_name: _,
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
        TurnItem::Reasoning(TextItem { text }) => match messages.last_mut() {
            Some(message) if message.role == Role::Assistant => {
                message.content.push(ContentBlock::Reasoning { text });
            }
            _ => {
                messages.push(Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Reasoning { text }],
                });
            }
        },
        TurnItem::ToolProgress(_)
        | TurnItem::ApprovalRequest(_)
        | TurnItem::ApprovalDecision(_) => {}
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
pub(crate) fn build_turn_record(
    turn: &TurnMetadata,
    session_context: Option<devo_core::SessionContext>,
    turn_context: Option<devo_core::TurnContext>,
) -> TurnRecord {
    TurnRecord {
        id: turn.turn_id,
        session_id: turn.session_id,
        sequence: turn.sequence,
        started_at: turn.started_at,
        completed_at: turn.completed_at,
        status: turn.status.clone(),
        kind: turn.kind.clone(),
        model: turn.model.clone(),
        thinking: turn.thinking.clone(),
        request_model: turn.request_model.clone(),
        request_thinking: turn.request_thinking.clone(),
        input_token_estimate: None,
        usage: turn.usage.clone(),
        session_context,
        turn_context,
        schema_version: 2,
    }
}

/// Creates one canonical persisted item record from a normalized turn item payload.
pub(crate) fn build_item_record(
    session_id: SessionId,
    turn_id: TurnId,
    item_id: devo_core::ItemId,
    seq: u64,
    item: TurnItem,
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
        output_items: vec![item],
        worklog,
        error: None,
        schema_version: 1,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use chrono::TimeZone;
    use chrono::Utc;
    use pretty_assertions::assert_eq;

    use super::ReplayState;
    use super::build_prompt_messages_from_snapshot;
    use crate::execution::PersistedTurnItem;
    use crate::persistence::apply_turn_item;
    use devo_core::CompactionSnapshotLine;
    use devo_core::EnvironmentContext;
    use devo_core::ItemId;
    use devo_core::ItemLine;
    use devo_core::ItemRecord;
    use devo_core::Message;
    use devo_core::Model;
    use devo_core::Persona;
    use devo_core::RolloutLine;
    use devo_core::SessionContext;
    use devo_core::SessionId;
    use devo_core::SessionMetaLine;
    use devo_core::SessionRecord;
    use devo_core::SessionTitleState;
    use devo_core::TextItem;
    use devo_core::ToolCallItem;
    use devo_core::ToolResultItem;
    use devo_core::TurnContext;
    use devo_core::TurnId;
    use devo_core::TurnItem;
    use devo_core::TurnLine;
    use devo_core::TurnRecord;
    use devo_core::TurnStatus;

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

    #[test]
    fn replay_backfills_tool_result_name_from_prior_tool_call() {
        let mut messages = Vec::new();
        let mut history_items = Vec::new();
        let mut tool_names_by_id = HashMap::new();

        apply_turn_item(
            &mut messages,
            &mut history_items,
            &mut tool_names_by_id,
            TurnItem::ToolCall(ToolCallItem {
                tool_call_id: "call-1".to_string(),
                tool_name: "read".to_string(),
                input: serde_json::json!({"filePath":"/tmp/test.txt"}),
            }),
        );
        apply_turn_item(
            &mut messages,
            &mut history_items,
            &mut tool_names_by_id,
            TurnItem::ToolResult(ToolResultItem {
                tool_call_id: "call-1".to_string(),
                tool_name: None,
                output: serde_json::Value::String("hello".to_string()),
                is_error: false,
            }),
        );

        assert_eq!(history_items.len(), 2);
        assert_eq!(history_items[0].title, "Ran read");
        assert_eq!(history_items[1].title, "read output");
    }

    #[test]
    fn prompt_messages_rebuild_from_compaction_snapshot_without_trimming_transcript() {
        let summary_item_id = ItemId::new();
        let preserved_item_id = ItemId::new();
        let later_item_id = ItemId::new();

        let persisted_turn_items = vec![
            PersistedTurnItem {
                item_id: ItemId::new(),
                turn_item: TurnItem::UserMessage(TextItem {
                    text: "older user".to_string(),
                }),
            },
            PersistedTurnItem {
                item_id: summary_item_id,
                turn_item: TurnItem::ContextCompaction(TextItem {
                    text: "<compaction_summary>summary</compaction_summary>".to_string(),
                }),
            },
            PersistedTurnItem {
                item_id: preserved_item_id,
                turn_item: TurnItem::UserMessage(TextItem {
                    text: "latest user".to_string(),
                }),
            },
            PersistedTurnItem {
                item_id: later_item_id,
                turn_item: TurnItem::AgentMessage(TextItem {
                    text: "latest assistant".to_string(),
                }),
            },
        ];

        let prompt_messages = build_prompt_messages_from_snapshot(
            &persisted_turn_items,
            &CompactionSnapshotLine {
                timestamp: Utc::now(),
                session_id: SessionId::new(),
                turn_id: TurnId::new(),
                summary_item_id,
                preserved_item_ids: vec![preserved_item_id],
            },
        )
        .expect("prompt messages");

        assert_eq!(
            prompt_messages,
            vec![
                Message::assistant_text("<compaction_summary>summary</compaction_summary>"),
                Message::user("latest user"),
                Message::assistant_text("latest assistant"),
            ]
        );
    }

    #[test]
    fn replay_restores_context_snapshots_from_turn_records() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let now = Utc.with_ymd_and_hms(2026, 4, 27, 8, 0, 0).unwrap();
        let session_context = SessionContext {
            base_instructions: "base".into(),
            workspace_instructions: Some("workspace".into()),
            locked_agents_snapshot: None,
            environment: EnvironmentContext {
                cwd: PathBuf::from("/tmp/root"),
                shell: "bash".into(),
                current_date: "2026-04-27".into(),
                timezone: "UTC".into(),
            },
            persona: Persona::Default,
            model: Model {
                slug: "model-a".into(),
                ..Model::default()
            },
            thinking_selection: None,
            reasoning_effort: None,
        };
        let turn_context = TurnContext {
            environment: EnvironmentContext {
                cwd: PathBuf::from("/tmp/next"),
                shell: "bash".into(),
                current_date: "2026-04-28".into(),
                timezone: "UTC".into(),
            },
            persona: Persona::Default,
            model: Model {
                slug: "model-b".into(),
                ..Model::default()
            },
            thinking_selection: Some("enabled".into()),
            reasoning_effort: None,
            observed_agents_snapshot: None,
        };
        let mut replay = ReplayState::default();

        replay
            .apply_line(RolloutLine::SessionMeta(Box::new(SessionMetaLine {
                timestamp: now,
                session: SessionRecord {
                    id: session_id,
                    rollout_path: PathBuf::from("rollout.jsonl"),
                    created_at: now,
                    updated_at: now,
                    source: "cli".into(),
                    agent_nickname: None,
                    agent_role: None,
                    agent_path: None,
                    model_provider: "test".into(),
                    model: Some("model-a".into()),
                    thinking: None,
                    cwd: PathBuf::from("/tmp/root"),
                    cli_version: "0.1.0".into(),
                    title: None,
                    title_state: SessionTitleState::Unset,
                    sandbox_policy: "workspace-write".into(),
                    approval_mode: "on-request".into(),
                    tokens_used: 0,
                    first_user_message: None,
                    archived_at: None,
                    git_sha: None,
                    git_branch: None,
                    git_origin_url: None,
                    parent_session_id: None,
                    session_context: None,
                    latest_turn_context: None,
                    schema_version: 2,
                },
            })))
            .expect("apply session meta");
        replay
            .apply_line(RolloutLine::Turn(Box::new(TurnLine {
                timestamp: now,
                turn: TurnRecord {
                    id: turn_id,
                    session_id,
                    sequence: 1,
                    started_at: now,
                    completed_at: Some(now),
                    status: TurnStatus::Completed,
                    kind: devo_core::TurnKind::Regular,
                    model: "model-b".into(),
                    thinking: Some("enabled".into()),
                    request_model: "model-b".into(),
                    request_thinking: Some("enabled".into()),
                    input_token_estimate: None,
                    usage: None,
                    session_context: Some(session_context.clone()),
                    turn_context: Some(turn_context.clone()),
                    schema_version: 2,
                },
            })))
            .expect("apply turn line");

        assert_eq!(replay.session_context, Some(session_context));
        assert_eq!(replay.latest_turn_context, Some(turn_context));
    }
}
