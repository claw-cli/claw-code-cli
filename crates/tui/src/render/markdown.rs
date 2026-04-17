use std::{
    path::Path,
    sync::{LazyLock, OnceLock},
};

use dirs::home_dir;
use pulldown_cmark::{CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use regex::Regex;
use syntect::{
    easy::HighlightLines,
    highlighting::Theme,
    parsing::{SyntaxReference, SyntaxSet},
    util::LinesWithEndings,
};
use two_face::theme::EmbeddedThemeName;
use url::Url;

static COLON_LOCATION_SUFFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r":\d+(?::\d+)?(?:[-–]\d+(?::\d+)?)?$")
        .unwrap_or_else(|error| panic!("invalid location suffix regex: {error}"))
});
static HASH_LOCATION_SUFFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^L\d+(?:C\d+)?(?:-L\d+(?:C\d+)?)?$")
        .unwrap_or_else(|error| panic!("invalid hash location regex: {error}"))
});
static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static SYNTAX_THEME: OnceLock<Theme> = OnceLock::new();

#[derive(Clone, Copy)]
struct MarkdownStyles {
    base: Style,
    emphasis: Style,
    strong: Style,
    strikethrough: Style,
    inline_code: Style,
    link: Style,
    blockquote: Style,
    h1: Style,
    h2: Style,
    h3: Style,
    h4: Style,
    h5: Style,
    h6: Style,
}

impl MarkdownStyles {
    fn new(base: Style) -> Self {
        Self {
            base,
            emphasis: base.add_modifier(Modifier::ITALIC),
            strong: base.add_modifier(Modifier::BOLD),
            strikethrough: base.add_modifier(Modifier::CROSSED_OUT),
            inline_code: base.fg(Color::Cyan),
            link: base.fg(Color::Cyan).add_modifier(Modifier::UNDERLINED),
            blockquote: base.fg(Color::Green),
            h1: base
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::UNDERLINED),
            h2: base.add_modifier(Modifier::BOLD),
            h3: base
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::ITALIC),
            h4: base.add_modifier(Modifier::ITALIC),
            h5: base.add_modifier(Modifier::ITALIC),
            h6: base.add_modifier(Modifier::ITALIC),
        }
    }
}

#[derive(Clone, Debug)]
struct LinkState {
    rendered_target: Option<String>,
    rendered_label: String,
    suppress_label: bool,
}

#[derive(Clone, Debug)]
struct ListState {
    next_index: Option<u64>,
}

pub(super) fn render_markdown_lines(
    input: &str,
    base_style: Style,
    cwd: Option<&Path>,
) -> Vec<Line<'static>> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(input, options);
    let mut writer = MarkdownWriter::new(base_style, cwd);
    for event in parser {
        writer.handle_event(event);
    }
    writer.finish()
}

struct MarkdownWriter<'a> {
    styles: MarkdownStyles,
    cwd: Option<&'a Path>,
    lines: Vec<Line<'static>>,
    current_line: Vec<Span<'static>>,
    inline_styles: Vec<Style>,
    list_stack: Vec<ListState>,
    blockquote_depth: usize,
    pending_item_prefix: Option<String>,
    code_block_language: Option<String>,
    in_code_block: bool,
    code_block_buffer: String,
    link: Option<LinkState>,
}

impl<'a> MarkdownWriter<'a> {
    fn new(base_style: Style, cwd: Option<&'a Path>) -> Self {
        Self {
            styles: MarkdownStyles::new(base_style),
            cwd,
            lines: Vec::new(),
            current_line: Vec::new(),
            inline_styles: vec![base_style],
            list_stack: Vec::new(),
            blockquote_depth: 0,
            pending_item_prefix: None,
            code_block_language: None,
            in_code_block: false,
            code_block_buffer: String::new(),
            link: None,
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush_current_line();
        while self.lines.first().is_some_and(|line| line.spans.is_empty()) {
            self.lines.remove(0);
        }
        while self.lines.last().is_some_and(|line| line.spans.is_empty()) {
            self.lines.pop();
        }
        if self.lines.is_empty() {
            self.lines
                .push(Line::from(vec![Span::styled("", self.styles.base)]));
        }
        self.lines
    }

    fn handle_event(&mut self, event: Event<'a>) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.push_text(text.as_ref()),
            Event::Code(code) => self.push_span(code.into_string(), self.styles.inline_code),
            Event::SoftBreak | Event::HardBreak => self.new_line(),
            Event::Rule => {
                self.ensure_blank_line();
                self.lines
                    .push(Line::from(vec![Span::styled("———", self.styles.base)]));
                self.lines.push(Line::default());
            }
            Event::Html(html) | Event::InlineHtml(html) => self.push_text(html.as_ref()),
            Event::InlineMath(text) | Event::DisplayMath(text) => self.push_text(text.as_ref()),
            Event::FootnoteReference(_) | Event::TaskListMarker(_) => {}
        }
    }

    fn start_tag(&mut self, tag: Tag<'a>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.ensure_blank_line();
                let style = match level {
                    pulldown_cmark::HeadingLevel::H1 => self.styles.h1,
                    pulldown_cmark::HeadingLevel::H2 => self.styles.h2,
                    pulldown_cmark::HeadingLevel::H3 => self.styles.h3,
                    pulldown_cmark::HeadingLevel::H4 => self.styles.h4,
                    pulldown_cmark::HeadingLevel::H5 => self.styles.h5,
                    pulldown_cmark::HeadingLevel::H6 => self.styles.h6,
                };
                self.push_span(format!("{} ", "#".repeat(level as usize)), style);
                self.inline_styles.push(style);
            }
            Tag::BlockQuote(_) => {
                self.ensure_blank_line();
                self.blockquote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.ensure_blank_line();
                self.in_code_block = true;
                self.code_block_language = match kind {
                    CodeBlockKind::Fenced(lang) => Some(first_info_token(lang)),
                    CodeBlockKind::Indented => None,
                };
                self.code_block_buffer.clear();
            }
            Tag::List(start) => self.list_stack.push(ListState { next_index: start }),
            Tag::Item => {
                if !self.current_line.is_empty() {
                    self.flush_current_line();
                }
                self.pending_item_prefix = Some(self.next_item_prefix());
            }
            Tag::Emphasis => self.inline_styles.push(self.styles.emphasis),
            Tag::Strong => self.inline_styles.push(self.styles.strong),
            Tag::Strikethrough => self.inline_styles.push(self.styles.strikethrough),
            Tag::Link { dest_url, .. } => {
                let destination = dest_url.to_string();
                let rendered_target = render_local_link_target(&destination, self.cwd);
                self.link = Some(LinkState {
                    suppress_label: rendered_target.is_some(),
                    rendered_target,
                    rendered_label: String::new(),
                });
                self.inline_styles.push(self.styles.link);
            }
            Tag::HtmlBlock
            | Tag::FootnoteDefinition(_)
            | Tag::Table(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::Image { .. }
            | Tag::MetadataBlock(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                if self.list_stack.is_empty() {
                    self.new_line();
                } else {
                    self.flush_current_line();
                }
            }
            TagEnd::Heading(_) => {
                self.inline_styles.pop();
                self.new_line();
                self.new_line();
            }
            TagEnd::BlockQuote(_) => {
                self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
                self.new_line();
            }
            TagEnd::CodeBlock => self.finish_code_block(),
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.new_line();
            }
            TagEnd::Item => self.flush_current_line(),
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.inline_styles.pop();
            }
            TagEnd::Link => {
                self.inline_styles.pop();
                if let Some(link) = self.link.take() {
                    if let Some(rendered_target) = link.rendered_target {
                        self.push_span(rendered_target, self.styles.link);
                    } else if !link.rendered_label.is_empty() {
                        self.push_span(link.rendered_label, self.styles.link);
                    }
                }
            }
            TagEnd::HtmlBlock
            | TagEnd::FootnoteDefinition
            | TagEnd::Table
            | TagEnd::TableHead
            | TagEnd::TableRow
            | TagEnd::TableCell
            | TagEnd::Image
            | TagEnd::MetadataBlock(_)
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Superscript
            | TagEnd::Subscript => {}
        }
    }

    fn push_text(&mut self, text: &str) {
        if self.in_code_block {
            self.code_block_buffer.push_str(text);
            return;
        }

        if let Some(link) = self.link.as_mut() {
            if !link.suppress_label {
                link.rendered_label.push_str(text);
            }
            return;
        }

        for (index, segment) in text.split('\n').enumerate() {
            if index > 0 {
                self.new_line();
            }
            if !segment.is_empty() {
                self.push_span(segment.to_string(), self.current_style());
            }
        }
    }

    fn push_span(&mut self, text: String, style: Style) {
        if text.is_empty() {
            return;
        }
        self.ensure_line_prefix();
        self.current_line.push(Span::styled(text, style));
    }

    fn current_style(&self) -> Style {
        self.inline_styles
            .last()
            .copied()
            .unwrap_or(self.styles.base)
    }

    fn ensure_line_prefix(&mut self) {
        if !self.current_line.is_empty() {
            return;
        }

        if self.blockquote_depth > 0 {
            self.current_line.push(Span::styled(
                "> ".repeat(self.blockquote_depth),
                self.styles.blockquote,
            ));
        }

        if let Some(prefix) = self.pending_item_prefix.take() {
            self.current_line
                .push(Span::styled(prefix, self.current_style()));
        } else if !self.list_stack.is_empty() {
            let indent = "  ".repeat(self.list_stack.len());
            if !indent.is_empty() {
                self.current_line
                    .push(Span::styled(indent, self.styles.base));
            }
        }
    }

    fn next_item_prefix(&mut self) -> String {
        let depth = self.list_stack.len().saturating_sub(1);
        let indent = "  ".repeat(depth);
        let marker = match self
            .list_stack
            .last_mut()
            .and_then(|state| state.next_index.as_mut())
        {
            Some(index) => {
                let rendered = format!("{index}. ");
                *index += 1;
                rendered
            }
            None => "- ".to_string(),
        };
        format!("{indent}{marker}")
    }

    fn new_line(&mut self) {
        self.flush_current_line();
    }

    fn flush_current_line(&mut self) {
        if self.current_line.is_empty() {
            if self.lines.last().is_none_or(|line| !line.spans.is_empty()) {
                self.lines.push(Line::default());
            }
            return;
        }

        self.lines
            .push(Line::from(std::mem::take(&mut self.current_line)));
    }

    fn ensure_blank_line(&mut self) {
        self.flush_current_line();
        if self.lines.last().is_some_and(|line| !line.spans.is_empty()) {
            self.lines.push(Line::default());
        }
    }

    fn finish_code_block(&mut self) {
        self.in_code_block = false;
        let lang = self.code_block_language.take();
        let code = std::mem::take(&mut self.code_block_buffer);
        let highlighted = match lang {
            Some(lang) if !lang.is_empty() => highlight_code_to_lines(&code, &lang),
            _ => plain_code_to_lines(&code, self.styles.base),
        };
        for line in highlighted {
            self.lines.push(line);
        }
        self.lines.push(Line::default());
    }
}

fn first_info_token(lang: CowStr<'_>) -> String {
    lang.split([',', ' ', '\t'])
        .next()
        .unwrap_or_default()
        .to_string()
}

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(two_face::syntax::extra_newlines)
}

fn syntax_theme() -> &'static Theme {
    SYNTAX_THEME.get_or_init(|| {
        two_face::theme::extra()
            .get(EmbeddedThemeName::CatppuccinMocha)
            .clone()
    })
}

fn find_syntax(lang: &str) -> Option<&'static SyntaxReference> {
    let patched = match lang {
        "csharp" | "c-sharp" => "c#",
        "golang" => "go",
        "python3" => "python",
        "shell" => "bash",
        _ => lang,
    };

    let syntax_set = syntax_set();
    if let Some(syntax) = syntax_set.find_syntax_by_token(patched) {
        return Some(syntax);
    }
    if let Some(syntax) = syntax_set.find_syntax_by_name(patched) {
        return Some(syntax);
    }
    if let Some(syntax) = syntax_set.find_syntax_by_extension(lang) {
        return Some(syntax);
    }

    let lowercase = patched.to_ascii_lowercase();
    syntax_set
        .syntaxes()
        .iter()
        .find(|syntax| syntax.name.to_ascii_lowercase() == lowercase)
}

fn highlight_code_to_lines(code: &str, lang: &str) -> Vec<Line<'static>> {
    let Some(syntax) = find_syntax(lang) else {
        return plain_code_to_lines(code, Style::default());
    };

    let mut highlighter = HighlightLines::new(syntax, syntax_theme());
    let mut lines = Vec::new();
    for line in LinesWithEndings::from(code) {
        let Ok(ranges) = highlighter.highlight_line(line, syntax_set()) else {
            return plain_code_to_lines(code, Style::default());
        };
        let mut spans = Vec::new();
        for (style, text) in ranges {
            let text = text.trim_end_matches(['\n', '\r']);
            if text.is_empty() {
                continue;
            }
            spans.push(Span::styled(
                text.to_string(),
                syntect_style_to_ratatui(style),
            ));
        }
        if spans.is_empty() {
            spans.push(Span::raw(String::new()));
        }
        lines.push(Line::from(spans));
    }
    if lines.is_empty() {
        lines.push(Line::default());
    }
    lines
}

fn syntect_style_to_ratatui(style: syntect::highlighting::Style) -> Style {
    let mut converted = Style::new().fg(Color::Rgb(
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
    ));
    if style
        .font_style
        .contains(syntect::highlighting::FontStyle::BOLD)
    {
        converted = converted.add_modifier(Modifier::BOLD);
    }
    if style
        .font_style
        .contains(syntect::highlighting::FontStyle::ITALIC)
    {
        converted = converted.add_modifier(Modifier::ITALIC);
    }
    if style
        .font_style
        .contains(syntect::highlighting::FontStyle::UNDERLINE)
    {
        converted = converted.add_modifier(Modifier::UNDERLINED);
    }
    converted
}

fn plain_code_to_lines(code: &str, style: Style) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = code
        .lines()
        .map(|line| Line::from(vec![Span::styled(line.to_string(), style)]))
        .collect();
    if lines.is_empty() {
        lines.push(Line::default());
    }
    lines
}

fn render_local_link_target(dest_url: &str, cwd: Option<&Path>) -> Option<String> {
    let (path_text, location_suffix) = parse_local_link_target(dest_url)?;
    let mut rendered = display_local_link_path(&path_text, cwd);
    if let Some(location_suffix) = location_suffix {
        rendered.push_str(&location_suffix);
    }
    Some(rendered)
}

fn parse_local_link_target(dest_url: &str) -> Option<(String, Option<String>)> {
    if !is_local_path_like_link(dest_url) {
        return None;
    }

    if dest_url.starts_with("file://") {
        let url = Url::parse(dest_url).ok()?;
        let path_text = file_url_to_local_path_text(&url)?;
        let location_suffix = url
            .fragment()
            .and_then(normalize_hash_location_suffix_fragment);
        return Some((path_text, location_suffix));
    }

    let mut path_text = dest_url;
    let mut location_suffix = None;
    if let Some((candidate_path, fragment)) = dest_url.rsplit_once('#')
        && let Some(normalized) = normalize_hash_location_suffix_fragment(fragment)
    {
        path_text = candidate_path;
        location_suffix = Some(normalized);
    }
    if location_suffix.is_none()
        && let Some(suffix) = extract_colon_location_suffix(path_text)
    {
        let path_len = path_text.len().saturating_sub(suffix.len());
        path_text = &path_text[..path_len];
        location_suffix = Some(suffix);
    }

    let decoded_path_text =
        urlencoding::decode(path_text).unwrap_or(std::borrow::Cow::Borrowed(path_text));
    Some((expand_local_link_path(&decoded_path_text), location_suffix))
}

fn is_local_path_like_link(dest_url: &str) -> bool {
    dest_url.starts_with("file://")
        || dest_url.starts_with('/')
        || dest_url.starts_with("~/")
        || dest_url.starts_with("./")
        || dest_url.starts_with("../")
        || dest_url.starts_with("\\\\")
        || matches!(
            dest_url.as_bytes(),
            [drive, b':', separator, ..]
                if drive.is_ascii_alphabetic() && matches!(separator, b'/' | b'\\')
        )
}

fn normalize_hash_location_suffix_fragment(fragment: &str) -> Option<String> {
    HASH_LOCATION_SUFFIX_RE
        .is_match(fragment)
        .then(|| format!("#{fragment}"))
}

fn extract_colon_location_suffix(path_text: &str) -> Option<String> {
    COLON_LOCATION_SUFFIX_RE
        .find(path_text)
        .filter(|matched| matched.end() == path_text.len())
        .map(|matched| matched.as_str().to_string())
}

fn expand_local_link_path(path_text: &str) -> String {
    if let Some(rest) = path_text.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return normalize_local_link_path_text(&home.join(rest).to_string_lossy());
    }

    normalize_local_link_path_text(path_text)
}

fn file_url_to_local_path_text(url: &Url) -> Option<String> {
    if let Ok(path) = url.to_file_path() {
        return Some(normalize_local_link_path_text(&path.to_string_lossy()));
    }

    let mut path_text = url.path().to_string();
    if let Some(host) = url.host_str()
        && !host.is_empty()
        && host != "localhost"
    {
        path_text = format!("//{host}{path_text}");
    } else if matches!(
        path_text.as_bytes(),
        [b'/', drive, b':', b'/', ..] if drive.is_ascii_alphabetic()
    ) {
        path_text.remove(0);
    }

    Some(normalize_local_link_path_text(&path_text))
}

fn normalize_local_link_path_text(path_text: &str) -> String {
    if let Some(rest) = path_text.strip_prefix("\\\\") {
        format!("//{}", rest.replace('\\', "/").trim_start_matches('/'))
    } else {
        path_text.replace('\\', "/")
    }
}

fn is_absolute_local_link_path(path_text: &str) -> bool {
    path_text.starts_with('/')
        || path_text.starts_with("//")
        || matches!(
            path_text.as_bytes(),
            [drive, b':', b'/', ..] if drive.is_ascii_alphabetic()
        )
}

fn trim_trailing_local_path_separator(path_text: &str) -> &str {
    if path_text == "/" || path_text == "//" {
        return path_text;
    }
    if matches!(path_text.as_bytes(), [drive, b':', b'/'] if drive.is_ascii_alphabetic()) {
        return path_text;
    }
    path_text.trim_end_matches('/')
}

fn strip_local_path_prefix<'a>(path_text: &'a str, cwd_text: &str) -> Option<&'a str> {
    let path_text = trim_trailing_local_path_separator(path_text);
    let cwd_text = trim_trailing_local_path_separator(cwd_text);
    if path_text == cwd_text {
        return None;
    }
    if cwd_text == "/" || cwd_text == "//" {
        return path_text.strip_prefix('/');
    }

    path_text
        .strip_prefix(cwd_text)
        .and_then(|rest| rest.strip_prefix('/'))
}

fn display_local_link_path(path_text: &str, cwd: Option<&Path>) -> String {
    let path_text = normalize_local_link_path_text(path_text);
    if !is_absolute_local_link_path(&path_text) {
        return path_text;
    }

    if let Some(cwd) = cwd {
        let cwd_text = normalize_local_link_path_text(&cwd.to_string_lossy());
        if let Some(stripped) = strip_local_path_prefix(&path_text, &cwd_text) {
            return stripped.to_string();
        }
    }

    path_text
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn rendered_strings(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn renders_markdown_list_items() {
        let rendered = render_markdown_lines("- item one\n- item two", Style::default(), None);
        assert_eq!(
            rendered_strings(&rendered),
            vec!["- item one".to_string(), "- item two".to_string()]
        );
    }

    #[test]
    fn highlights_fenced_code_blocks() {
        let rendered = render_markdown_lines("```rust\nfn main() {}\n```", Style::default(), None);

        let has_rgb_span = rendered.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| matches!(span.style.fg, Some(Color::Rgb(..))))
        });
        assert!(has_rgb_span, "expected syntax-highlighted spans");
    }

    #[test]
    fn shortens_local_file_links_against_cwd() {
        let rendered = render_markdown_lines(
            "[label](file:///workspace/src/main.rs#L10)",
            Style::default(),
            Some(Path::new("/workspace")),
        );
        assert_eq!(
            rendered_strings(&rendered),
            vec!["src/main.rs#L10".to_string()]
        );
    }

    #[cfg(windows)]
    #[test]
    fn shortens_windows_local_file_links_against_cwd() {
        let rendered = render_markdown_lines(
            "[label](file:///C:/workspace/src/main.rs#L10C2)",
            Style::default(),
            Some(Path::new("C:\\workspace")),
        );
        assert_eq!(
            rendered_strings(&rendered),
            vec!["src/main.rs#L10C2".to_string()]
        );
    }

    #[test]
    fn keeps_web_link_label_text() {
        let rendered =
            render_markdown_lines("[docs](https://example.com/docs)", Style::default(), None);
        assert_eq!(rendered_strings(&rendered), vec!["docs".to_string()]);
    }
}
