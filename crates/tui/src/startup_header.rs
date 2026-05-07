use std::path::Path;
use std::time::Duration;

use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

use crate::exec_command::relativize_to_home;

pub(crate) const STARTUP_HEADER_ANIMATION_INTERVAL: Duration = Duration::from_millis(400);

const STARTUP_HEADER_MAX_TOTAL_WIDTH: usize = 60;
const STARTUP_HEADER_MIN_FULL_WIDTH: usize = 44;
const MASCOT_WIDTH: usize = 11;
const MASCOT_GAP: usize = 3;
const META_GAP: usize = 2;
const MODEL_LABEL: &str = "Model      ";
const REASONING_LABEL: &str = "Reasoning   ";
const DIRECTORY_LABEL: &str = "Directory  ";

pub(crate) struct StartupHeaderData<'a> {
    pub(crate) version: &'static str,
    pub(crate) model: &'a str,
    pub(crate) reasoning: &'a str,
    pub(crate) directory: &'a Path,
    pub(crate) accent_color: Color,
    pub(crate) mascot_frame_index: usize,
}

pub(crate) fn mascot_frame(frame_index: usize) -> [&'static str; 4] {
    const FRAMES: [[&str; 4]; 3] = [
        ["     .----.", "    | >_ |", "    |____|", "     /||\\  "],
        ["     .----.", "    | >_ |", "    |____|", "     \\||/  "],
        ["     .----.", "    | >_ |", "    |____|", "     -||-  "],
    ];
    FRAMES[frame_index % FRAMES.len()]
}

pub(crate) fn build_startup_header(data: StartupHeaderData<'_>, width: u16) -> Vec<Line<'static>> {
    let available_width = usize::from(width);
    if available_width < 4 {
        return Vec::new();
    }

    let total_width = available_width.min(STARTUP_HEADER_MAX_TOTAL_WIDTH);
    let inner_width = total_width.saturating_sub(2);
    if inner_width == 0 {
        return Vec::new();
    }

    if total_width < STARTUP_HEADER_MIN_FULL_WIDTH {
        build_compact_header(data, inner_width)
    } else {
        build_full_header(data, inner_width)
    }
}

fn build_full_header(data: StartupHeaderData<'_>, inner_width: usize) -> Vec<Line<'static>> {
    let border_style = Style::default().dim();
    let muted_style = Style::default().dim();
    let accent_style = Style::default().fg(data.accent_color).bold();
    let mascot_style = Style::default().fg(data.accent_color);
    let mascot = mascot_frame(data.mascot_frame_index);
    let version = format!("v{}", data.version);
    let reasoning = sanitize_reasoning(data.reasoning);

    vec![
        border_line('┏', '━', '┓', inner_width, border_style),
        content_line(
            vec![Span::styled(mascot[0].to_string(), mascot_style)],
            inner_width,
            border_style,
        ),
        content_line(
            build_title_row(
                mascot[1],
                &version,
                inner_width,
                mascot_style,
                accent_style,
                muted_style,
            ),
            inner_width,
            border_style,
        ),
        content_line(
            vec![Span::styled(mascot[2].to_string(), mascot_style)],
            inner_width,
            border_style,
        ),
        content_line(
            vec![Span::styled(mascot[3].to_string(), mascot_style)],
            inner_width,
            border_style,
        ),
        border_line('┣', '━', '┫', inner_width, border_style),
        content_line(
            build_model_reasoning_row(
                data.model,
                &reasoning,
                inner_width,
                muted_style,
                Style::default(),
            ),
            inner_width,
            border_style,
        ),
        content_line(
            build_directory_row(data.directory, inner_width, muted_style),
            inner_width,
            border_style,
        ),
        border_line('┗', '━', '┛', inner_width, border_style),
    ]
}

fn build_compact_header(data: StartupHeaderData<'_>, inner_width: usize) -> Vec<Line<'static>> {
    let border_style = Style::default().dim();
    let muted_style = Style::default().dim();
    let accent_style = Style::default().fg(data.accent_color).bold();
    let version = format!("v{}", data.version);
    let reasoning = sanitize_reasoning(data.reasoning);

    let title = truncate_right(&format!("Devo {version}"), inner_width);
    let model_reasoning = compact_model_reasoning(data.model, &reasoning, inner_width);

    vec![
        border_line('┏', '━', '┓', inner_width, border_style),
        content_line(
            vec![
                Span::styled(title, accent_style),
                Span::styled(String::new(), Style::default()),
            ],
            inner_width,
            border_style,
        ),
        border_line('┣', '━', '┫', inner_width, border_style),
        content_line(
            vec![Span::styled(model_reasoning, Style::default())],
            inner_width,
            border_style,
        ),
        content_line(
            build_directory_row(data.directory, inner_width, muted_style),
            inner_width,
            border_style,
        ),
        border_line('┗', '━', '┛', inner_width, border_style),
    ]
}

fn build_title_row(
    mascot_line: &str,
    version: &str,
    inner_width: usize,
    mascot_style: Style,
    title_style: Style,
    version_style: Style,
) -> Vec<Span<'static>> {
    let secondary_width = inner_width.saturating_sub(MASCOT_WIDTH + MASCOT_GAP);
    let version_width = UnicodeWidthStr::width(version);
    let title = "Devo";
    let title_width = UnicodeWidthStr::width(title);

    let mut spans = vec![Span::styled(mascot_line.to_string(), mascot_style)];
    if secondary_width == 0 {
        return spans;
    }

    push_spaces(&mut spans, MASCOT_GAP);
    if secondary_width > title_width + version_width {
        spans.push(Span::styled(title.to_string(), title_style));
        push_spaces(
            &mut spans,
            secondary_width.saturating_sub(title_width + version_width),
        );
        spans.push(Span::styled(version.to_string(), version_style));
        return spans;
    }

    spans.push(Span::styled(
        truncate_right(&format!("{title} {version}"), secondary_width),
        title_style,
    ));
    spans
}

fn build_model_reasoning_row(
    model: &str,
    reasoning: &str,
    inner_width: usize,
    label_style: Style,
    value_style: Style,
) -> Vec<Span<'static>> {
    let model_label_width = UnicodeWidthStr::width(MODEL_LABEL);
    let reasoning_label_width = UnicodeWidthStr::width(REASONING_LABEL);
    let fixed_width = model_label_width + META_GAP + reasoning_label_width;
    let remaining_width = inner_width.saturating_sub(fixed_width);
    let reasoning_width = UnicodeWidthStr::width(reasoning);
    let mut reasoning_budget = reasoning_width.min(remaining_width.saturating_sub(1).max(1));
    let mut model_budget = remaining_width.saturating_sub(reasoning_budget);

    if model_budget == 0 && remaining_width > 0 {
        model_budget = 1;
        reasoning_budget = remaining_width.saturating_sub(model_budget);
    }

    let model_text = truncate_right(model, model_budget);
    let reasoning_text = truncate_right(reasoning, reasoning_budget);
    let model_padding = model_budget.saturating_sub(UnicodeWidthStr::width(model_text.as_str()));

    let mut spans = vec![
        Span::styled(MODEL_LABEL.to_string(), label_style),
        Span::styled(model_text, value_style),
    ];
    push_spaces(&mut spans, model_padding);
    push_spaces(&mut spans, META_GAP);
    spans.push(Span::styled(REASONING_LABEL.to_string(), label_style));
    spans.push(Span::styled(reasoning_text, value_style));
    spans
}

fn build_directory_row(
    directory: &Path,
    inner_width: usize,
    label_style: Style,
) -> Vec<Span<'static>> {
    let available_path_width = inner_width.saturating_sub(UnicodeWidthStr::width(DIRECTORY_LABEL));
    let path = format_directory(directory, available_path_width);
    vec![
        Span::styled(DIRECTORY_LABEL.to_string(), label_style),
        Span::from(path),
    ]
}

fn compact_model_reasoning(model: &str, reasoning: &str, inner_width: usize) -> String {
    let separator = " / ";
    let separator_width = UnicodeWidthStr::width(separator);
    let reasoning_width = UnicodeWidthStr::width(reasoning);
    if inner_width <= separator_width {
        return truncate_right(reasoning, inner_width);
    }

    let mut reasoning_budget = reasoning_width.min(inner_width.saturating_sub(separator_width + 1));
    let mut model_budget = inner_width.saturating_sub(separator_width + reasoning_budget);
    if model_budget == 0 {
        model_budget = 1.min(inner_width);
        reasoning_budget = inner_width.saturating_sub(separator_width + model_budget);
    }

    let model_text = truncate_right(model, model_budget);
    let reasoning_text = truncate_right(reasoning, reasoning_budget);
    let mut out = model_text;
    if !reasoning_text.is_empty() && UnicodeWidthStr::width(out.as_str()) < inner_width {
        out.push_str(separator);
        out.push_str(&reasoning_text);
    }
    truncate_right(&out, inner_width)
}

fn sanitize_reasoning(reasoning: &str) -> String {
    let trimmed = reasoning.trim();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

fn format_directory(directory: &Path, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }

    let formatted = if let Some(relative) = relativize_to_home(directory) {
        if relative.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~{}{}", std::path::MAIN_SEPARATOR, relative.display())
        }
    } else {
        directory.display().to_string()
    };

    truncate_left(&formatted, max_width)
}

fn border_line(
    left: char,
    horizontal: char,
    right: char,
    inner_width: usize,
    style: Style,
) -> Line<'static> {
    Line::from(Span::styled(
        format!(
            "{left}{}{right}",
            horizontal.to_string().repeat(inner_width)
        ),
        style,
    ))
}

fn content_line(
    spans: Vec<Span<'static>>,
    inner_width: usize,
    border_style: Style,
) -> Line<'static> {
    let used_width = spans_width(&spans);
    let mut row = Vec::with_capacity(spans.len() + 3);
    row.push(Span::styled("┃".to_string(), border_style));
    row.extend(spans);
    if used_width < inner_width {
        row.push(Span::from(" ".repeat(inner_width - used_width)));
    }
    row.push(Span::styled("┃".to_string(), border_style));
    Line::from(row)
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

fn push_spaces(spans: &mut Vec<Span<'static>>, count: usize) {
    if count > 0 {
        spans.push(Span::from(" ".repeat(count)));
    }
}

fn truncate_right(text: &str, max_width: usize) -> String {
    truncate_text(text, max_width, TruncationSide::Right)
}

fn truncate_left(text: &str, max_width: usize) -> String {
    truncate_text(text, max_width, TruncationSide::Left)
}

enum TruncationSide {
    Left,
    Right,
}

fn truncate_text(text: &str, max_width: usize, side: TruncationSide) -> String {
    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let budget = max_width - 1;
    match side {
        TruncationSide::Right => {
            let mut out = String::new();
            let mut used = 0;
            for ch in text.chars() {
                let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
                if used + char_width > budget {
                    break;
                }
                out.push(ch);
                used += char_width;
            }
            out.push('…');
            out
        }
        TruncationSide::Left => {
            let mut kept = Vec::new();
            let mut used = 0;
            for ch in text.chars().rev() {
                let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
                if used + char_width > budget {
                    break;
                }
                kept.push(ch);
                used += char_width;
            }
            kept.reverse();
            let tail = kept.into_iter().collect::<String>();
            format!("…{tail}")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use pretty_assertions::assert_eq;
    use ratatui::style::Color;
    use unicode_width::UnicodeWidthStr;

    use super::StartupHeaderData;
    use super::build_startup_header;
    use super::mascot_frame;

    fn rendered_strings(width: u16, model: &str, reasoning: &str, directory: &Path) -> Vec<String> {
        build_startup_header(
            StartupHeaderData {
                version: "0.1.3",
                model,
                reasoning,
                directory,
                accent_color: Color::Cyan,
                mascot_frame_index: 0,
            },
            width,
        )
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect()
    }

    #[test]
    fn mascot_frames_keep_the_same_width() {
        let widths = (0..3)
            .map(|idx| mascot_frame(idx).map(UnicodeWidthStr::width))
            .collect::<Vec<_>>();
        assert_eq!(widths[0], widths[1]);
        assert_eq!(widths[1], widths[2]);
    }

    #[test]
    fn full_header_renders_at_wide_widths() {
        let rows = rendered_strings(
            80,
            "gpt-5-high",
            "medium",
            Path::new("/Users/tester/Desktop/devo"),
        );
        assert_eq!(9, rows.len());
        assert!(rows[0].starts_with('┏'));
        assert!(rows[1].contains(".----."));
        assert!(rows[2].contains("Devo"));
        assert!(rows[2].contains("v0.1.3"));
        assert!(rows[6].contains("Model"));
        assert!(rows[6].contains("Reasoning"));
        assert!(rows[7].contains("Directory"));
    }

    #[test]
    fn compact_header_renders_at_narrow_widths() {
        let rows = rendered_strings(
            40,
            "gpt-5-high",
            "medium",
            Path::new("/Users/tester/Desktop/devo"),
        );
        assert_eq!(6, rows.len());
        assert!(rows[1].contains("Devo v0.1.3"));
        assert!(rows[3].contains('/'));
    }

    #[test]
    fn very_long_model_and_directory_are_truncated_without_overflow() {
        let rows = rendered_strings(
            60,
            "gpt-5-ultra-long-model-name-with-many-suffixes",
            "medium",
            Path::new("/Users/tester/Desktop/projects/devo/some/really/long/path"),
        );
        assert!(rows[6].contains('…'));
        assert!(rows[7].contains('…'));
        assert!(rows[7].contains("long/path"));
        assert!(
            rows.iter()
                .all(|row| UnicodeWidthStr::width(row.as_str()) <= 60)
        );
    }

    #[test]
    fn unknown_reasoning_and_windows_paths_are_supported() {
        let rows = rendered_strings(
            60,
            "gpt-5-high",
            "",
            Path::new(r"C:\Users\tester\Desktop\devo\long\workspace"),
        );
        assert!(rows[6].contains("unknown"));
        assert!(rows[7].contains("workspace"));
        assert!(
            rows.iter()
                .all(|row| UnicodeWidthStr::width(row.as_str()) <= 60)
        );
    }

    #[test]
    fn header_handles_requested_validation_widths() {
        for width in [120_u16, 80, 60, 40] {
            let rows = rendered_strings(
                width,
                "gpt-5-high",
                "medium",
                Path::new("/Users/tester/Desktop/devo"),
            );
            assert!(!rows.is_empty());
            assert!(
                rows.iter()
                    .all(|row| UnicodeWidthStr::width(row.as_str()) <= usize::from(width))
            );
        }
    }
}
