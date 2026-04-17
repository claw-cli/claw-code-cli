use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use serde_json::json;
use tokio::fs;
use tracing::debug;

use crate::{Tool, ToolContext, ToolOutput};

const DESCRIPTION: &str = include_str!("apply_patch.txt");

pub struct ApplyPatchTool;

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "patchText": {
                    "type": "string",
                    "description": "The full patch text that describes all changes to be made"
                }
            },
            "required": ["patchText"]
        })
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        input: serde_json::Value,
    ) -> anyhow::Result<ToolOutput> {
        let patch_text = input["patchText"].as_str().unwrap_or("");
        debug!(
            tool = self.name(),
            cwd = %ctx.cwd.display(),
            session_id = %ctx.session_id,
            input = %input,
            patch_text = patch_text,
            patch_text_len = patch_text.len(),
            "received apply_patch request"
        );
        if patch_text.trim().is_empty() {
            debug!("rejecting apply_patch request because patchText is empty");
            return Ok(ToolOutput::error("patchText is required"));
        }

        let patch = parse_patch(patch_text)?;
        debug!(change_count = patch.len(), "parsed apply_patch request");
        if patch.is_empty() {
            let normalized = patch_text
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .trim()
                .to_string();
            if normalized == "*** Begin Patch\n*** End Patch" {
                debug!("rejecting apply_patch request because patch contained no changes");
                return Ok(ToolOutput::error("patch rejected: empty patch"));
            }
            debug!("rejecting apply_patch request because no hunks were found");
            return Ok(ToolOutput::error(
                "apply_patch verification failed: no hunks found",
            ));
        }

        let mut files = Vec::with_capacity(patch.len());
        let mut summary = Vec::with_capacity(patch.len());
        let mut total_diff = String::new();

        for change in &patch {
            let source_path = resolve_relative(&ctx.cwd, &change.path)?;
            let target_path = change
                .move_path
                .as_deref()
                .map(|path| resolve_relative(&ctx.cwd, path))
                .transpose()?;
            debug!(
                kind = %change.kind.as_str(),
                source_path = %source_path.display(),
                target_path = ?target_path.as_ref().map(|path| path.display().to_string()),
                content_len = change.content.len(),
                "prepared apply_patch change"
            );

            let old_content = match change.kind {
                PatchKind::Add => String::new(),
                _ => read_file(&source_path).await?,
            };
            let new_content = match change.kind {
                PatchKind::Add => change.content.clone(),
                PatchKind::Update | PatchKind::Move => apply_hunks(&old_content, &change.hunks)?,
                PatchKind::Delete => String::new(),
            };

            let additions = new_content.lines().count();
            let deletions = old_content.lines().count();
            let relative_path =
                relative_worktree_path(target_path.as_ref().unwrap_or(&source_path), &ctx.cwd);
            let kind_name = change.kind.as_str();
            let diff = format!("--- {}\n+++ {}\n", relative_path, relative_path);

            files.push(json!({
                "filePath": source_path,
                "relativePath": relative_path,
                "type": kind_name,
                "patch": diff,
                "additions": additions,
                "deletions": deletions,
                "movePath": target_path,
            }));
            total_diff.push_str(&diff);
            total_diff.push('\n');

            summary.push(match change.kind {
                PatchKind::Add => format!("A {}", relative_worktree_path(&source_path, &ctx.cwd)),
                PatchKind::Delete => {
                    format!("D {}", relative_worktree_path(&source_path, &ctx.cwd))
                }
                PatchKind::Update | PatchKind::Move => {
                    format!(
                        "M {}",
                        relative_worktree_path(
                            target_path.as_ref().unwrap_or(&source_path),
                            &ctx.cwd
                        )
                    )
                }
            });
        }

        for change in &patch {
            debug!(
                kind = %change.kind.as_str(),
                path = %change.path,
                move_path = ?change.move_path,
                "applying patch change"
            );

            apply_change(&ctx.cwd, change).await?;
        }

        debug!(
            updated_files = summary.len(),
            summary = ?summary,
            "apply_patch completed successfully"
        );
        Ok(ToolOutput {
            content: format!(
                "Success. Updated the following files:\n{}",
                summary.join("\n")
            ),
            is_error: false,
            metadata: Some(json!({
                "diff": total_diff,
                "files": files,
                "diagnostics": {},
            })),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatchKind {
    Add,
    Update,
    Delete,
    Move,
}

impl PatchKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Move => "move",
        }
    }
}

#[derive(Debug, Clone)]
struct PatchChange {
    path: String,
    move_path: Option<String>,
    content: String,
    hunks: Vec<PatchHunk>,
    kind: PatchKind,
}

#[derive(Debug, Clone)]
struct PatchHunk {
    lines: Vec<HunkLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HunkLine {
    Context(String),
    Add(String),
    Remove(String),
}

fn is_file_header_line(line: &str) -> bool {
    line.starts_with("*** Add File: ")
        || line.starts_with("*** Delete File: ")
        || line.starts_with("*** Update File: ")
}

fn parse_patch(patch_text: &str) -> anyhow::Result<Vec<PatchChange>> {
    let normalized = patch_text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = normalized.lines().peekable();

    let Some(first_line) = lines.peek().copied() else {
        return Ok(Vec::new());
    };

    let mut wrapped = false;
    if first_line == "*** Begin Patch" {
        wrapped = true;
        lines.next();
    } else if !is_file_header_line(first_line) {
        return Err(anyhow::anyhow!(
            "patch must start with *** Begin Patch or a file operation header"
        ));
    }

    let mut changes = Vec::new();
    let mut saw_end_patch = false;

    while let Some(line) = lines.next() {
        if line == "*** End Patch" {
            saw_end_patch = true;
            break;
        }

        if line == "*** End of File" {
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Add File: ") {
            let contents = collect_plus_block(&mut lines)?;
            changes.push(PatchChange {
                path: path.to_string(),
                move_path: None,
                content: contents,
                hunks: Vec::new(),
                kind: PatchKind::Add,
            });
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            changes.push(PatchChange {
                path: path.to_string(),
                move_path: None,
                content: String::new(),
                hunks: Vec::new(),
                kind: PatchKind::Delete,
            });
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Update File: ") {
            let mut move_path = None;
            if matches!(lines.peek(), Some(next) if next.starts_with("*** Move to: ")) {
                let next = lines.next().unwrap_or_default();
                move_path = Some(next.trim_start_matches("*** Move to: ").to_string());
            }
            let hunks = collect_hunk_block(&mut lines)?;
            let kind = if move_path.is_some() {
                PatchKind::Move
            } else {
                PatchKind::Update
            };
            changes.push(PatchChange {
                path: path.to_string(),
                move_path,
                content: String::new(),
                hunks,
                kind,
            });
            continue;
        }

        return Err(anyhow::anyhow!(
            "expected file operation header, got: {line}"
        ));
    }

    if changes.is_empty() {
        return Err(anyhow::anyhow!("no patch operations found"));
    }

    if wrapped && !saw_end_patch {
        return Err(anyhow::anyhow!("patch must end with *** End Patch"));
    }

    Ok(changes)
}

fn is_hunk_header_line(line: &str) -> bool {
    line == "@@" || line.starts_with("@@ ")
}

fn is_git_diff_metadata_line(line: &str) -> bool {
    line.starts_with("diff --git ")
        || line.starts_with("index ")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
}

fn collect_plus_block(
    lines: &mut std::iter::Peekable<std::str::Lines<'_>>,
) -> anyhow::Result<String> {
    let mut content = String::new();
    while let Some(next) = lines.peek() {
        if next.starts_with("*** ") {
            break;
        }
        let line = lines.next().unwrap_or_default();
        if let Some(rest) = line.strip_prefix('+') {
            content.push_str(rest);
            content.push('\n');
        } else {
            return Err(anyhow::anyhow!(
                "add file lines must start with +, got: {line}"
            ));
        }
    }
    Ok(content)
}

fn collect_hunk_block(
    lines: &mut std::iter::Peekable<std::str::Lines<'_>>,
) -> anyhow::Result<Vec<PatchHunk>> {
    let mut hunks = Vec::new();
    let mut current_hunk: Option<PatchHunk> = None;
    let mut saw_hunk = false;

    while let Some(next) = lines.peek() {
        if next.starts_with("*** ") && !next.starts_with("*** End of File") {
            break;
        }
        let line = lines.next().unwrap_or_default();
        if line == "*** End of File" {
            break;
        }
        if current_hunk.is_none() && is_git_diff_metadata_line(line) {
            continue;
        }
        if is_hunk_header_line(line) {
            saw_hunk = true;
            if let Some(hunk) = current_hunk.take() {
                hunks.push(hunk);
            }
            current_hunk = Some(PatchHunk { lines: Vec::new() });
            continue;
        }
        let Some(hunk) = current_hunk.as_mut() else {
            return Err(anyhow::anyhow!(
                "encountered patch lines before a hunk header"
            ));
        };
        match line.chars().next() {
            Some('+') => hunk.lines.push(HunkLine::Add(line[1..].to_string())),
            Some(' ') => hunk.lines.push(HunkLine::Context(line[1..].to_string())),
            Some('-') => {
                saw_hunk = true;
                hunk.lines.push(HunkLine::Remove(line[1..].to_string()));
            }
            None => {
                hunk.lines.push(HunkLine::Context(String::new()));
            }
            _ => return Err(anyhow::anyhow!("unsupported hunk line: {line}")),
        };
    }

    if let Some(hunk) = current_hunk.take() {
        hunks.push(hunk);
    }

    if !saw_hunk && hunks.iter().all(|hunk| hunk.lines.is_empty()) {
        return Err(anyhow::anyhow!("no hunks found"));
    }

    Ok(hunks)
}

fn resolve_relative(base: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    let candidate = Path::new(rel);
    if candidate.is_absolute() {
        return Err(anyhow::anyhow!(
            "file references can only be relative, NEVER ABSOLUTE."
        ));
    }

    let mut out = base.to_path_buf();
    for component in candidate.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => out.push(part),
            Component::ParentDir => out.push(".."),
            Component::Prefix(_) | Component::RootDir => {
                return Err(anyhow::anyhow!(
                    "file references can only be relative, NEVER ABSOLUTE."
                ));
            }
        }
    }
    Ok(out)
}

fn relative_worktree_path(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

async fn read_file(path: &Path) -> anyhow::Result<String> {
    Ok(fs::read_to_string(path).await?)
}

async fn apply_change(base: &Path, change: &PatchChange) -> anyhow::Result<()> {
    let source = resolve_relative(base, &change.path)?;
    match change.kind {
        PatchKind::Add => {
            if let Some(parent) = source.parent() {
                fs::create_dir_all(parent).await?;
            }
            fs::write(&source, &change.content).await?;
        }
        PatchKind::Update => {
            let old_content = read_file(&source).await?;
            let new_content = apply_hunks(&old_content, &change.hunks)?;
            fs::write(&source, &new_content).await?;
        }
        PatchKind::Delete => {
            let _ = fs::remove_file(&source).await;
        }
        PatchKind::Move => {
            if let Some(dest) = &change.move_path {
                let dest = resolve_relative(base, dest)?;
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).await?;
                }
                let old_content = read_file(&source).await?;
                let new_content = apply_hunks(&old_content, &change.hunks)?;
                fs::write(&dest, &new_content).await?;
                let _ = fs::remove_file(&source).await;
            }
        }
    }
    Ok(())
}

fn apply_hunks(old_content: &str, hunks: &[PatchHunk]) -> anyhow::Result<String> {
    let old_lines = normalized_lines(old_content);
    let mut output = Vec::new();
    let mut cursor = 0usize;

    for hunk in hunks {
        let matched_hunk = find_hunk_start(&old_lines, cursor, hunk)?;
        let start = matched_hunk.start;
        output.extend_from_slice(&old_lines[cursor..start]);
        let mut position = start;
        for line in &hunk.lines {
            match line {
                HunkLine::Context(expected) => {
                    let actual = old_lines.get(position).ok_or_else(|| {
                        anyhow::anyhow!("context line beyond end of file: {expected}")
                    })?;
                    if !lines_match_mode(expected, actual, matched_hunk.mode) {
                        return Err(anyhow::anyhow!(
                            "context mismatch while applying patch: expected {expected:?}, got {actual:?}"
                        ));
                    }
                    output.push(actual.clone());
                    position += 1;
                }
                HunkLine::Remove(expected) => {
                    let actual = old_lines.get(position).ok_or_else(|| {
                        anyhow::anyhow!("removed line beyond end of file: {expected}")
                    })?;
                    if !lines_match_mode(expected, actual, matched_hunk.mode) {
                        return Err(anyhow::anyhow!(
                            "remove mismatch while applying patch: expected {expected:?}, got {actual:?}"
                        ));
                    }
                    position += 1;
                }
                HunkLine::Add(line) => output.push(line.clone()),
            }
        }
        cursor = position;
    }

    output.extend_from_slice(&old_lines[cursor..]);
    Ok(if output.is_empty() {
        String::new()
    } else {
        format!("{}\n", output.join("\n"))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchMode {
    Exact,
    Trimmed,
    NormalizedWhitespace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HunkMatch {
    start: usize,
    mode: MatchMode,
}

fn normalize_whitespace(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn lines_match_mode(expected: &str, actual: &str, mode: MatchMode) -> bool {
    match mode {
        MatchMode::Exact => expected == actual,
        MatchMode::Trimmed => expected.trim() == actual.trim(),
        MatchMode::NormalizedWhitespace => {
            normalize_whitespace(expected) == normalize_whitespace(actual)
        }
    }
}

fn find_hunk_start(
    old_lines: &[String],
    cursor: usize,
    hunk: &PatchHunk,
) -> anyhow::Result<HunkMatch> {
    let expected = hunk
        .lines
        .iter()
        .filter_map(|line| match line {
            HunkLine::Context(text) | HunkLine::Remove(text) => Some(text),
            HunkLine::Add(_) => None,
        })
        .collect::<Vec<_>>();

    if expected.is_empty() {
        return Ok(HunkMatch {
            start: cursor,
            mode: MatchMode::Exact,
        });
    }

    for mode in [
        MatchMode::Exact,
        MatchMode::Trimmed,
        MatchMode::NormalizedWhitespace,
    ] {
        if let Some(start) = try_find_hunk_start(old_lines, cursor, &expected, mode) {
            return Ok(HunkMatch { start, mode });
        }
    }

    if let Some(anchor) = select_hunk_anchor(hunk) {
        for mode in [
            MatchMode::Exact,
            MatchMode::Trimmed,
            MatchMode::NormalizedWhitespace,
        ] {
            if let Some(start) =
                try_find_hunk_start_from_anchor(old_lines, cursor, &expected, anchor, mode)
            {
                return Ok(HunkMatch { start, mode });
            }
        }
    }

    let (best_start, best_prefix, best_mode) =
        best_hunk_partial_match(old_lines, cursor, &expected).unwrap_or((0, 0, MatchMode::Exact));

    if best_prefix > 0 {
        let mismatch_at = best_prefix;
        let actual = old_lines
            .get(best_start + mismatch_at)
            .map(String::as_str)
            .unwrap_or("<EOF>");
        let expected_line = expected
            .get(mismatch_at)
            .map(|s| s.as_str())
            .unwrap_or("<none>");

        return Err(anyhow::anyhow!(
            "failed to locate hunk context; closest {:?} match started at old_lines[{}], mismatch at hunk line {}: expected {:?}, got {:?}",
            best_mode,
            best_start,
            mismatch_at,
            expected_line,
            actual,
        ));
    }

    Err(anyhow::anyhow!(
        "failed to locate hunk context in source file; no partial match found"
    ))
}

fn try_find_hunk_start(
    old_lines: &[String],
    cursor: usize,
    expected: &[&String],
    mode: MatchMode,
) -> Option<usize> {
    let max_start = old_lines.len().saturating_sub(expected.len());

    (cursor..=max_start).find(|&start| {
        expected.iter().enumerate().all(|(offset, line)| {
            old_lines
                .get(start + offset)
                .map(|actual| lines_match_mode(line, actual, mode))
                .unwrap_or(false)
        })
    })
}

fn select_hunk_anchor(hunk: &PatchHunk) -> Option<(usize, &str)> {
    let mut sequence_index = 0usize;
    let mut best_anchor = None;

    for line in &hunk.lines {
        match line {
            HunkLine::Context(text) => {
                let candidate = (sequence_index, text.as_str());
                if !text.trim().is_empty()
                    && best_anchor
                        .map(|(_, best_text): (usize, &str)| text.len() > best_text.len())
                        .unwrap_or(true)
                    || best_anchor.is_none()
                {
                    best_anchor = Some(candidate);
                }
                sequence_index += 1;
            }
            HunkLine::Remove(_) => sequence_index += 1,
            HunkLine::Add(_) => {}
        }
    }

    best_anchor
}

fn try_find_hunk_start_from_anchor(
    old_lines: &[String],
    cursor: usize,
    expected: &[&String],
    anchor: (usize, &str),
    mode: MatchMode,
) -> Option<usize> {
    let (anchor_index, anchor_text) = anchor;
    let max_start = old_lines.len().saturating_sub(expected.len());

    (cursor..=max_start).find(|&start| {
        old_lines
            .get(start + anchor_index)
            .map(|actual| lines_match_mode(anchor_text, actual, mode))
            .unwrap_or(false)
            && expected.iter().enumerate().all(|(offset, line)| {
                old_lines
                    .get(start + offset)
                    .map(|actual| lines_match_mode(line, actual, mode))
                    .unwrap_or(false)
            })
    })
}

fn best_hunk_partial_match(
    old_lines: &[String],
    cursor: usize,
    expected: &[&String],
) -> Option<(usize, usize, MatchMode)> {
    let mut best_start = None;
    let mut best_prefix = 0usize;
    let mut best_mode = MatchMode::Exact;
    let max_start = old_lines.len().saturating_sub(expected.len());

    for mode in [
        MatchMode::Exact,
        MatchMode::Trimmed,
        MatchMode::NormalizedWhitespace,
    ] {
        for start in cursor..=max_start {
            let mut matched = 0usize;

            for (offset, expected_line) in expected.iter().enumerate() {
                let actual = old_lines
                    .get(start + offset)
                    .map(String::as_str)
                    .unwrap_or("<EOF>");
                if lines_match_mode(expected_line, actual, mode) {
                    matched += 1;
                } else {
                    break;
                }
            }

            if matched > best_prefix {
                best_prefix = matched;
                best_start = Some(start);
                best_mode = mode;
            }
        }
    }

    best_start.map(|start| (start, best_prefix, best_mode))
}

fn normalized_lines(content: &str) -> Vec<String> {
    content
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use clawcr_safety::legacy_permissions::{PermissionMode, RuleBasedPolicy};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::{
        ApplyPatchTool, HunkLine, PatchHunk, PatchKind, apply_hunks, parse_patch, resolve_relative,
    };
    use crate::{Tool, ToolContext};

    fn unique_temp_dir(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("clawcr-apply-patch-{name}-{nanos}"));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn make_ctx(cwd: std::path::PathBuf) -> ToolContext {
        ToolContext {
            cwd,
            permissions: Arc::new(RuleBasedPolicy::new(PermissionMode::AutoApprove)),
            session_id: "test-session".into(),
        }
    }

    #[test]
    fn parse_patch_supports_all_change_kinds() {
        let patch = parse_patch(
            "*** Begin Patch
*** Add File: add.txt
+hello
*** Update File: update.txt
@@
-old
+new
*** Delete File: delete.txt
*** Update File: from.txt
*** Move to: to.txt
@@
-before
+after
*** End Patch",
        )
        .expect("parse patch");

        assert_eq!(patch.len(), 4);
        assert_eq!(patch[0].path, "add.txt");
        assert_eq!(patch[0].kind, PatchKind::Add);
        assert_eq!(patch[0].content, "hello\n");

        assert_eq!(patch[1].path, "update.txt");
        assert_eq!(patch[1].kind, PatchKind::Update);
        assert!(patch[1].content.is_empty());
        assert_eq!(patch[1].hunks.len(), 1);
        assert_eq!(
            patch[1].hunks[0].lines,
            vec![
                HunkLine::Remove("old".to_string()),
                HunkLine::Add("new".to_string())
            ]
        );

        assert_eq!(patch[2].path, "delete.txt");
        assert_eq!(patch[2].kind, PatchKind::Delete);

        assert_eq!(patch[3].path, "from.txt");
        assert_eq!(patch[3].move_path.as_deref(), Some("to.txt"));
        assert_eq!(patch[3].kind, PatchKind::Move);
        assert!(patch[3].content.is_empty());
        assert_eq!(patch[3].hunks.len(), 1);
        assert_eq!(
            patch[3].hunks[0].lines,
            vec![
                HunkLine::Remove("before".to_string()),
                HunkLine::Add("after".to_string())
            ]
        );
    }

    #[test]
    fn parse_patch_tolerates_git_diff_headers_before_hunk() {
        let patch = parse_patch(
            "*** Begin Patch
*** Update File: read.rs
diff --git a/read.rs b/read.rs
index 1234567..89abcde 100644
--- a/read.rs
+++ b/read.rs
@@ -10,11 +10,6 @@ use serde_json::json;
 use crate::{Tool, ToolContext, ToolOutput};
 
 const DESCRIPTION: &str = include_str!(\"read.txt\");
-const MAX_LINE_LENGTH: usize = 2000;
+const MAX_BYTES: usize = 50 * 1024;
*** End Patch",
        )
        .expect("parse patch with git diff headers");

        assert_eq!(patch.len(), 1);
        assert_eq!(patch[0].path, "read.rs");
        assert_eq!(patch[0].kind, PatchKind::Update);
        assert_eq!(patch[0].hunks.len(), 1);
        assert_eq!(
            patch[0].hunks[0].lines,
            vec![
                HunkLine::Context("use crate::{Tool, ToolContext, ToolOutput};".to_string()),
                HunkLine::Context(String::new()),
                HunkLine::Context(
                    "const DESCRIPTION: &str = include_str!(\"read.txt\");".to_string()
                ),
                HunkLine::Remove("const MAX_LINE_LENGTH: usize = 2000;".to_string()),
                HunkLine::Add("const MAX_BYTES: usize = 50 * 1024;".to_string()),
            ]
        );
    }

    #[test]
    fn parse_patch_requires_end_marker() {
        let error = parse_patch(
            "*** Begin Patch
*** Update File: README.md
@@
 **If you find this project useful, please consider giving it a ⭐**
+Bye",
        )
        .expect_err("patch without end marker should fail");

        assert!(error.to_string().contains("*** End Patch"));
    }

    #[test]
    fn parse_patch_rejects_surrounding_log_text() {
        let error = parse_patch(
            "request tool=\"apply_patch\"\ninput={...}\n*** Begin Patch
*** Update File: README.md
@@
 **If you find this project useful, please consider giving it a ⭐**
+Bye
*** End Patch",
        )
        .expect_err("surrounding log text should fail");

        assert!(error.to_string().contains("*** Begin Patch"));
    }

    #[test]
    fn parse_patch_rejects_non_prefixed_add_file_content() {
        let error = parse_patch(
            "*** Begin Patch
*** Add File: hello.txt
hello
*** End Patch",
        )
        .expect_err("non-prefixed add content should fail");

        assert!(error.to_string().contains("must start with +"));
    }

    #[test]
    fn apply_hunks_matches_trimmed_lines_without_rewriting_context_whitespace() {
        let old_content = "start\n  keep me  \nold\nend\n";
        let hunks = vec![PatchHunk {
            lines: vec![
                HunkLine::Context("start".to_string()),
                HunkLine::Context("keep me".to_string()),
                HunkLine::Remove("old".to_string()),
                HunkLine::Add("new".to_string()),
                HunkLine::Context("end".to_string()),
            ],
        }];

        let new_content = apply_hunks(old_content, &hunks).expect("apply hunks");

        assert_eq!(new_content, "start\n  keep me  \nnew\nend\n");
    }

    #[test]
    fn apply_hunks_matches_lines_with_normalized_whitespace() {
        let old_content = "alpha   beta\nold value\nomega\n";
        let hunks = vec![PatchHunk {
            lines: vec![
                HunkLine::Context("alpha beta".to_string()),
                HunkLine::Remove("old value".to_string()),
                HunkLine::Add("new value".to_string()),
                HunkLine::Context("omega".to_string()),
            ],
        }];

        let new_content = apply_hunks(old_content, &hunks).expect("apply hunks");

        assert_eq!(new_content, "alpha   beta\nnew value\nomega\n");
    }

    #[test]
    fn resolve_relative_rejects_absolute_paths() {
        let base = std::path::Path::new("C:\\workspace");

        #[cfg(windows)]
        let path = "C:\\absolute\\file.txt";
        #[cfg(unix)]
        let path = "/absolute/file.txt";

        let error = resolve_relative(base, path).expect_err("absolute path should fail");
        assert!(error.to_string().contains("NEVER ABSOLUTE"));
    }

    #[tokio::test]
    async fn execute_applies_changes_and_returns_summary() {
        let cwd = unique_temp_dir("execute");
        std::fs::write(cwd.join("update.txt"), "old\n").expect("write update file");
        std::fs::write(cwd.join("from.txt"), "before\n").expect("write move source");
        std::fs::write(cwd.join("delete.txt"), "remove me\n").expect("write delete source");
        let ctx = make_ctx(cwd.clone());

        let output = ApplyPatchTool
            .execute(
                &ctx,
                json!({
                    "patchText": "*** Begin Patch
*** Add File: add.txt
+hello
*** Update File: update.txt
@@
-old
+new
*** Delete File: delete.txt
*** Update File: from.txt
*** Move to: moved/to.txt
@@
-before
+after
*** End Patch"
                }),
            )
            .await
            .expect("execute apply_patch");

        assert!(!output.is_error);
        assert!(
            output
                .content
                .contains("Success. Updated the following files:")
        );
        assert!(output.content.contains("A add.txt"));
        assert!(output.content.contains("M update.txt"));
        assert!(output.content.contains("D delete.txt"));
        assert!(output.content.contains("M moved/to.txt"));

        assert_eq!(
            std::fs::read_to_string(cwd.join("add.txt")).expect("read added file"),
            "hello\n"
        );
        assert_eq!(
            std::fs::read_to_string(cwd.join("update.txt")).expect("read updated file"),
            "new\n"
        );
        assert!(!cwd.join("delete.txt").exists());
        assert!(!cwd.join("from.txt").exists());
        assert_eq!(
            std::fs::read_to_string(cwd.join("moved").join("to.txt")).expect("read moved file"),
            "after\n"
        );

        let metadata = output.metadata.expect("metadata");
        let files = metadata["files"].as_array().expect("files metadata");
        assert_eq!(files.len(), 4);
        assert_eq!(files[0]["additions"], 1);
        assert_eq!(files[0]["deletions"], 0);
        assert_eq!(files[1]["additions"], 1);
        assert_eq!(files[1]["deletions"], 1);
        assert_eq!(files[2]["additions"], 0);
        assert_eq!(files[2]["deletions"], 1);
        assert_eq!(files[3]["additions"], 1);
        assert_eq!(files[3]["deletions"], 1);
    }

    #[tokio::test]
    async fn execute_given_patch() {
        let content = r#"use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use serde_json::json;

use crate::{Tool, ToolContext, ToolOutput};

const DESCRIPTION: &str = include_str!("read.txt");
const MAX_LINE_LENGTH: usize = 2000;
const MAX_LINE_SUFFIX: &str = "... (line truncated to 2000 chars)";
const MAX_BYTES: usize = 50 * 1024;
const MAX_BYTES_LABEL: &str = "50 KB";

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "filePath": {
                    "type": "string",
                    "description": "The absolute path to the file or directory to read"
                },
                "offset": {
                    "type": "integer",
                    "description": "The line number to start reading from (1-indexed, default 1)"
                },
                "limit": {
                    "type": "integer",
                    "description": "The maximum number of lines to read (no limit by default)"
                }
            },
            "required": ["filePath"]
        })
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        input: serde_json::Value,
    ) -> anyhow::Result<ToolOutput> {
        let mut filepath = input["filePath"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'filePath' field"))?
            .to_string();
        let offset = input["offset"].as_u64().map(|value| value as usize);
        let limit = input["limit"].as_u64().map(|value| value as usize);

        if let Some(offset) = offset {
            if offset < 1 {
                return Ok(ToolOutput::error(
                    "offset must be greater than or equal to 1",
                ));
            }
        }

        if !Path::new(&filepath).is_absolute() {
            filepath = ctx.cwd.join(&filepath).to_string_lossy().to_string();
        }

        let path = PathBuf::from(&filepath);
        if !path.exists() {
            return Ok(ToolOutput::error(missing_file_message(&filepath)));
        }

        if path.is_dir() {
            return read_directory(
                &path, limit.unwrap_or(usize::MAX),
                offset.unwrap_or(1),
            );
        }

        if is_binary_file(&path)? {
            return Ok(ToolOutput::error(format!(
                "Cannot read binary file: {}",
                path.display()
            )));
        }

        read_file(
            &path,
            limit.unwrap_or(usize::MAX),
            offset.unwrap_or(1),
        )
    }
}

fn read_directory(path: &Path, limit: usize, offset: usize) -> anyhow::Result<ToolOutput> {
    let mut items = std::fs::read_dir(path)?
        .flatten()
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if is_dir { format!("{name}/") } else { name }
        })
        .collect::<Vec<_>>();
    items.sort_unstable_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));

    let start = offset.saturating_sub(1);
    let sliced = items
        .iter()
        .skip(start)
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    let truncated = start + sliced.len() < items.len();
    let preview = sliced
        .iter()
        .take(20)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");

    let output = [
        format!("<path>{}</path>", path.display()),
        "<type>directory</type>".to_string(),
        "<entries>".to_string(),
        sliced.join("\n"),
        if truncated {
            format!("\n(Showing {} of {} entries. Use 'offset' parameter to read beyond entry {})", sliced.len(), items.len(), offset + sliced.len())
        } else {
            format!("\n({} entries)", items.len())
        },
        "</entries>".to_string(),
    ]
    .join("\n");

    Ok(ToolOutput {
        content: output,
        is_error: false,
        metadata: Some(json!({
            "preview": preview,
            "truncated": truncated,
            "loaded": []
        })),
    })
}

fn read_file(path: &Path, limit: usize, offset: usize) -> anyhow::Result<ToolOutput> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let start = offset.saturating_sub(1);
    let mut raw = Vec::new();
    let mut bytes = 0usize;
    let mut count = 0usize;
    let mut cut = false;
    let mut more = false;

    for line in reader.lines() {
        let mut line = line?;
        count += 1;
        if count <= start {
            continue;
        }
        if raw.len() >= limit {
            more = true;
            continue;
        }
        if line.len() > MAX_LINE_LENGTH {
            line.truncate(MAX_LINE_LENGTH);
            line.push_str(MAX_LINE_SUFFIX);
        }
        let size = line.len() + if raw.is_empty() { 0 } else { 1 };
        if bytes + size > MAX_BYTES {
            cut = true;
            more = true;
            break;
        }
        raw.push(line);
        bytes += size;
    }

    if count < offset && !(count == 0 && offset == 1) {
        return Ok(ToolOutput::error(format!(
            "Offset {} is out of range for this file ({} lines)",
            offset, count
        )));
    }

    let mut output = format!(
        "<path>{}</path>\n<type>file</type>\n<content>\n",
        path.display()
    );
    for (index, line) in raw.iter().enumerate() {
        output.push_str(&format!("{}: {}\n", offset + index, line));
    }

    let last = offset + raw.len().saturating_sub(1);
    let next = last + 1;
    if cut {
        output.push_str(&format!(
            "\n(Output capped at {}. Showing lines {}-{}. Use offset={} to continue.)",
            MAX_BYTES_LABEL, offset, last, next
        ));
    } else if more {
        output.push_str(&format!("\n(Showing lines {}-{} of {}. Use offset={} to continue.)", offset, last, count, next))
    } else {
        output.push_str(&format!("\n(End of file - total {} lines)", count))
    }
    output.push_str("\n</content>");

    Ok(ToolOutput {
        content: output,
        is_error: false,
        metadata: Some(json!({
            "preview": raw.iter().take(20).cloned().collect::<Vec<_>>().join("\n"),
            "truncated": cut || more,
            "loaded": []
        })),
    })
}

fn is_binary_file(path: &Path) -> anyhow::Result<bool> {
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(
        ext.as_str(),
        "zip"
            | "tar"
            | "gz"
            | "exe"
            | "dll"
            | "so"
            | "class"
            | "jar"
            | "war"
            | "7z"
            | "doc"
            | "docx"
            | "xls"
            | "xlsx"
            | "ppt"
            | "pptx"
            | "odt"
            | "ods"
            | "odp"
            | "bin"
            | "dat"
            | "obj"
            | "o"
            | "a"
            | "lib"
            | "wasm"
            | "pyc"
            | "pyo"
    ) {
        return Ok(true);
    }

    let mut file = File::open(path)?;
    let size = file.metadata()?.len() as usize;
    if size == 0 {
        return Ok(false);
    }

    let sample_size = size.min(4096);
    let mut bytes = vec![0u8; sample_size];
    let read = file.read(&mut bytes)?;
    if read == 0 {
        return Ok(false);
    }

    let mut non_printable = 0usize;
    for byte in bytes.iter().take(read) {
        if *byte == 0 {
            return Ok(true);
        }
        if *byte < 9 || (*byte > 13 && *byte < 32) {
            non_printable += 1;
        }
    }

    Ok((non_printable as f64) / (read as f64) > 0.3)
}

fn missing_file_message(filepath: &str) -> String {
    let path = Path::new(filepath);
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let base = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(filepath);

    let suggestions = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| {
                    name.to_lowercase().contains(&base.to_lowercase())
                        || base.to_lowercase().contains(&name.to_lowercase())
                })
                .take(3)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if suggestions.is_empty() {
        format!("File not found: {filepath}")
    } else {
        format!(
            "File not found: {filepath}\n\nDid you mean one of these?\n{}",
            suggestions
                .into_iter()
                .map(|item| dir.join(item).to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env,
        fs::{self, File},
        io::Write,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    fn create_temp_dir(prefix: &str) -> PathBuf {
        let mut path = env::temp_dir();
        let ticks = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!("clawcr-tools-read-{prefix}-{ticks}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_lines(path: &Path, lines: &[&str]) {
        let mut file = File::create(path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    #[test]
    fn read_directory_sorts_entries_and_reports_truncation() {
        let dir = create_temp_dir("dir");
        File::create(dir.join("b.txt")).unwrap();
        File::create(dir.join("a.txt")).unwrap();
        fs::create_dir_all(dir.join("subdir")).unwrap();

        let output = read_directory(&dir, 1, 2).unwrap();
        assert!(output.content.contains("<type>directory</type>"));
        assert!(output.content.contains("b.txt"));
        assert!(
            output.content.contains(
                "(Showing 1 of 3 entries. Use 'offset' parameter to read beyond entry 3)"
            )
        );

        let metadata = output.metadata.unwrap();
        assert!(metadata.get("truncated").and_then(|value| value.as_bool()) == Some(true));
    }

    #[test]
    fn read_file_applies_limit_and_reports_more() {
        let dir = create_temp_dir("file");
        let path = dir.join("sample.txt");
        write_lines(&path, &["line1", "line2", "line3", "line4", "line5"]);

        let output = read_file(&path, 2, 2).unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("2: line2"));
        assert!(output.content.contains("3: line3"));
        assert!(
            output
                .content
                .contains("(Showing lines 2-3 of 5. Use offset=4 to continue.)")
        );

        let metadata = output.metadata.unwrap();
        assert!(metadata.get("truncated").and_then(|value| value.as_bool()) == Some(true));
    }

    #[test]
    fn read_file_reports_offset_out_of_range() {
        let dir = create_temp_dir("error");
        let path = dir.join("short.txt");
        write_lines(&path, &["hello", "world"]);

        let output = read_file(&path, 10, 5).unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("Offset 5 is out of range"));
    }

    #[test]
    fn is_binary_file_detects_null_bytes() {
        let dir = create_temp_dir("binary");
        let path = dir.join("payload.bin");
        fs::write(&path, &[0u8, 1, 2]).unwrap();

        assert!(is_binary_file(&path).unwrap());
    }

    #[test]
    fn missing_file_message_includes_suggestions() {
        let dir = create_temp_dir("missing");
        let target = dir.join("example.txt");
        write_lines(&target, &["content"]);

        let missing = dir.join("example");
        let message = missing_file_message(&missing.to_string_lossy());
        assert!(message.contains("Did you mean"));
        assert!(message.contains("example.txt"));
    }
}
"#;
        let cwd = unique_temp_dir("execute");
        std::fs::write(cwd.join("read.rs"), content).expect("write update file");

        let ctx = make_ctx(cwd);

        let patch = r#"*** Begin Patch
*** Update File: read.rs
@@ use std::{
     fs::File,
     io::{BufRead, BufReader, Read},
     path::{Path, PathBuf},
 };
 
 use async_trait::async_trait;
 use serde_json::json;
 
 use crate::{Tool, ToolContext, ToolOutput};
 
 const DESCRIPTION: &str = include_str!("read.txt");
-const MAX_LINE_LENGTH: usize = 2000;
-const MAX_LINE_SUFFIX: &str = "... (line truncated to 2000 chars)";
-const MAX_BYTES: usize = 50 * 1024;
-const MAX_BYTES_LABEL: &str = "50 KB";
 
 pub struct ReadTool;
*** End Patch"#;

        let output = ApplyPatchTool
            .execute(
                &ctx,
                json!({
                    "patchText": patch
                }),
            )
            .await
            .expect("execute apply_patch");

        assert_eq!(output.is_error, false);
    }
}
