use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use serde_json::json;
use tokio::fs;
use tracing::debug;

use crate::FunctionToolOutput;

pub(crate) async fn exec_apply_patch(
    cwd: &std::path::Path,
    session_id: &str,
    input: serde_json::Value,
) -> anyhow::Result<FunctionToolOutput> {
    let patch_text = input["patchText"].as_str().unwrap_or("");
    debug!(
        tool = "apply_patch",
        cwd = %cwd.display(),
        session_id = %session_id,
        input = %input,
        patch_text = patch_text,
        patch_text_len = patch_text.len(),
        "received apply_patch request"
    );
    if patch_text.trim().is_empty() {
        debug!("rejecting apply_patch request because patchText is empty");
        return Ok(FunctionToolOutput::error("patchText is required"));
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
            return Ok(FunctionToolOutput::error("patch rejected: empty patch"));
        }
        debug!("rejecting apply_patch request because no hunks were found");
        return Ok(FunctionToolOutput::error(
            "apply_patch verification failed: no hunks found",
        ));
    }

    let mut files = Vec::with_capacity(patch.len());
    let mut summary = Vec::with_capacity(patch.len());
    let mut total_diff = String::new();

    for change in &patch {
        let source_path = resolve_relative(cwd, &change.path)?;
        let target_path = change
            .move_path
            .as_deref()
            .map(|path| resolve_relative(cwd, path))
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
            relative_worktree_path(target_path.as_ref().unwrap_or(&source_path), cwd);
        let kind_name = change.kind.as_str();
        let diff = match change.kind {
            PatchKind::Add => {
                let content = if change.content.ends_with('\n') {
                    change.content.clone()
                } else {
                    format!("{}\n", change.content)
                };
                format!(
                    "diff --git a/{0} b/{0}\nnew file mode 100644\n--- /dev/null\n+++ b/{0}\n@@ -0,0 +1,{1} @@\n{2}",
                    relative_path,
                    additions,
                    content
                        .lines()
                        .map(|line| format!("+{line}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            }
            PatchKind::Delete => {
                let deleted = if old_content.ends_with('\n') {
                    old_content.clone()
                } else {
                    format!("{old_content}\n")
                };
                format!(
                    "diff --git a/{0} b/{0}\ndeleted file mode 100644\n--- a/{0}\n+++ /dev/null\n@@ -1,{1} +0,0 @@\n{2}",
                    relative_path,
                    deletions,
                    deleted
                        .lines()
                        .map(|line| format!("-{line}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            }
            PatchKind::Update | PatchKind::Move => {
                let old_line_count = old_content.lines().count();
                let new_line_count = new_content.lines().count();
                let patch_body = change
                    .hunks
                    .iter()
                    .flat_map(|hunk| {
                        let mut lines = Vec::with_capacity(hunk.lines.len() + 1);
                        lines.push(format!("@@ -1,{old_line_count} +1,{new_line_count} @@"));
                        lines.extend(hunk.lines.iter().map(|line| match line {
                            HunkLine::Context(text) => format!(" {text}"),
                            HunkLine::Remove(text) => format!("-{text}"),
                            HunkLine::Add(text) => format!("+{text}"),
                        }));
                        lines
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "diff --git a/{0} b/{0}\n--- a/{0}\n+++ b/{0}\n{1}\n",
                    relative_path, patch_body
                )
            }
        };

        files.push(json!({
            "path": relative_path,
            "filePath": source_path,
            "relativePath": relative_path,
            "kind": kind_name,
            "type": kind_name,
            "diff": diff,
            "patch": diff,
            "additions": additions,
            "deletions": deletions,
            "movePath": target_path,
        }));
        if !total_diff.is_empty() {
            total_diff.push('\n');
        }
        total_diff.push_str(diff.trim_end());
        total_diff.push('\n');

        summary.push(match change.kind {
            PatchKind::Add => format!("A {}", relative_worktree_path(&source_path, cwd)),
            PatchKind::Delete => {
                format!("D {}", relative_worktree_path(&source_path, cwd))
            }
            PatchKind::Update | PatchKind::Move => {
                format!(
                    "M {}",
                    relative_worktree_path(target_path.as_ref().unwrap_or(&source_path), cwd)
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

        apply_change(cwd, change).await?;
    }

    debug!(
        updated_files = summary.len(),
        summary = ?summary,
        "apply_patch completed successfully"
    );
    Ok(FunctionToolOutput::success_with_metadata(
        format!(
            "Success. Updated the following files:\n{}",
            summary.join("\n")
        ),
        json!({
            "diff": total_diff,
            "files": files,
            "diagnostics": {},
        }),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PatchKind {
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
pub(crate) struct PatchChange {
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
                {
                    best_anchor = Some(candidate);
                }
                if best_anchor.is_none() {
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
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use pretty_assertions::assert_eq;
    use serde_json::json;

    use crate::ToolContent;

    use super::HunkLine;
    use super::PatchHunk;
    use super::PatchKind;
    use super::apply_hunks;
    use super::exec_apply_patch;
    use super::parse_patch;
    use super::resolve_relative;

    fn unique_temp_dir(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("devo-apply-patch-{name}-{nanos}"));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
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

        let output = exec_apply_patch(
            &cwd,
            "test-session",
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
        let text = output.content.text_part().expect("text content");
        assert!(text.contains("Success. Updated the following files:"));
        assert!(text.contains("A add.txt"));
        assert!(text.contains("M update.txt"));
        assert!(text.contains("D delete.txt"));
        assert!(text.contains("M moved/to.txt"));

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

        let ToolContent::Mixed {
            json: Some(metadata),
            ..
        } = &output.content
        else {
            panic!("expected mixed output metadata, got {:?}", output.content);
        };
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

        for file in files {
            if file["kind"] == "update" || file["kind"] == "move" {
                let per_file_diff = file
                    .get("diff")
                    .or_else(|| file.get("patch"))
                    .and_then(serde_json::Value::as_str)
                    .expect("per-file diff");
                let patch = diffy::Patch::from_str(per_file_diff).expect("per-file diff should parse");
                let (added, removed) = patch
                    .hunks()
                    .iter()
                    .flat_map(diffy::Hunk::lines)
                    .fold((0usize, 0usize), |(a, d), line| match line {
                        diffy::Line::Insert(_) => (a + 1, d),
                        diffy::Line::Delete(_) => (a, d + 1),
                        diffy::Line::Context(_) => (a, d),
                    });
                assert_eq!((added, removed), (1, 1));
            }
        }
    }
}
