use serde_json::json;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::path::Path;

use crate::ToolOutput;

pub(crate) fn read_directory(
    path: &Path,
    limit: usize,
    offset: usize,
) -> anyhow::Result<ToolOutput> {
    let mut items = std::fs::read_dir(path)?
        .flatten()
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if is_dir { format!("{name}/") } else { name }
        })
        .collect::<Vec<_>>();
    items.sort_unstable_by_key(|a| a.to_lowercase());

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
            format!(
                "\n(Showing {} of {} entries. Use 'offset' parameter to read beyond entry {})",
                sliced.len(),
                items.len(),
                offset + sliced.len()
            )
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

pub(crate) fn read_file(path: &Path, limit: usize, offset: usize) -> anyhow::Result<ToolOutput> {
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
        if line.len() > 2000 {
            line.truncate(2000);
            line.push_str("... (line truncated to 2000 chars)");
        }
        let size = line.len() + if raw.is_empty() { 0 } else { 1 };
        if bytes + size > 50 * 1024 {
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
            "\n(Output capped at 50 KB. Showing lines {}-{}. Use offset={} to continue.)",
            offset, last, next
        ));
    } else if more {
        output.push_str(&format!(
            "\n(Showing lines {}-{} of {}. Use offset={} to continue.)",
            offset, last, count, next
        ))
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

pub(crate) fn is_binary_file(path: &Path) -> anyhow::Result<bool> {
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

pub(crate) fn missing_file_message(filepath: &str) -> String {
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
    use std::env;
    use std::fs::File;
    use std::fs::{self};
    use std::io::Write;
    use std::path::Path;
    use std::path::PathBuf;
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    fn create_temp_dir(prefix: &str) -> PathBuf {
        let mut path = env::temp_dir();
        let ticks = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!("devo-tools-read-{prefix}-{ticks}"));
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
        fs::write(&path, [0u8, 1, 2]).unwrap();

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
