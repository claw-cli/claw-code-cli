use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Mutable composer buffer used by the interactive terminal UI.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct InputBuffer {
    /// Full text currently present in the composer.
    text: String,
    /// Cursor position measured in character indices.
    cursor_chars: usize,
}

impl InputBuffer {
    /// Creates an empty composer buffer.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Returns the full composer text.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Returns whether the composer is empty or whitespace-only.
    pub(crate) fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Clears the composer and resets the cursor to the start.
    pub(crate) fn clear(&mut self) {
        self.text.clear();
        self.cursor_chars = 0;
    }

    /// Inserts one character at the current cursor position.
    pub(crate) fn insert_char(&mut self, ch: char) {
        let byte_index = self.byte_index_for_char(self.cursor_chars);
        self.text.insert(byte_index, ch);
        self.cursor_chars += 1;
    }

    /// Inserts a pasted string at the current cursor position.
    pub(crate) fn insert_str(&mut self, value: &str) {
        let byte_index = self.byte_index_for_char(self.cursor_chars);
        self.text.insert_str(byte_index, value);
        self.cursor_chars += value.chars().count();
    }

    /// Deletes the character immediately before the cursor.
    pub(crate) fn backspace(&mut self) {
        if self.cursor_chars == 0 {
            return;
        }
        let end = self.byte_index_for_char(self.cursor_chars);
        let start = self.byte_index_for_char(self.cursor_chars - 1);
        self.text.replace_range(start..end, "");
        self.cursor_chars -= 1;
    }

    /// Deletes the character currently under the cursor.
    pub(crate) fn delete(&mut self) {
        if self.cursor_chars >= self.char_len() {
            return;
        }
        let start = self.byte_index_for_char(self.cursor_chars);
        let end = self.byte_index_for_char(self.cursor_chars + 1);
        self.text.replace_range(start..end, "");
    }

    /// Moves the cursor one character to the left.
    pub(crate) fn move_left(&mut self) {
        self.cursor_chars = self.cursor_chars.saturating_sub(1);
    }

    /// Moves the cursor one character to the right.
    pub(crate) fn move_right(&mut self) {
        self.cursor_chars = (self.cursor_chars + 1).min(self.char_len());
    }

    /// Moves the cursor to the start of the buffer.
    pub(crate) fn move_home(&mut self) {
        self.cursor_chars = 0;
    }

    /// Moves the cursor to the end of the buffer.
    pub(crate) fn move_end(&mut self) {
        self.cursor_chars = self.char_len();
    }

    /// Removes the current text and returns it.
    pub(crate) fn take(&mut self) -> String {
        let value = self.text.clone();
        self.clear();
        value
    }

    /// Replaces the full composer text and moves the cursor to the end.
    pub(crate) fn replace(&mut self, value: impl Into<String>) {
        self.text = value.into();
        self.cursor_chars = self.char_len();
    }

    /// Returns the rendered cursor position for a wrapped text area.
    pub(crate) fn visual_cursor(&self, full_width: u16) -> (u16, u16) {
        self.layout(full_width).cursor
    }

    /// Returns the rendered cursor position for a wrapped text area with a prompt prefix.
    pub(crate) fn visual_cursor_with_prompt(
        &self,
        full_width: u16,
        prompt: Option<&str>,
    ) -> (u16, u16) {
        self.layout_with_prompt(full_width, prompt).cursor
    }

    /// Returns the fully wrapped visual lines used by the composer renderer.
    pub(crate) fn rendered_lines(&self, full_width: u16) -> Vec<String> {
        self.layout(full_width).lines
    }

    /// Returns the fully wrapped visual lines for a prompt-prefixed composer.
    pub(crate) fn rendered_lines_with_prompt(
        &self,
        full_width: u16,
        prompt: Option<&str>,
    ) -> Vec<String> {
        self.layout_with_prompt(full_width, prompt).lines
    }

    /// Returns a horizontally scrolled single-line prompt view for inline mode.
    pub(crate) fn inline_line(&self, full_width: u16, prefix: &str) -> InlineInputLine {
        let width_limit = usize::from(full_width.max(1));
        let prefix_width = UnicodeWidthStr::width(prefix).min(width_limit);
        let available_width = width_limit.saturating_sub(prefix_width);
        let mut rendered = prefix.to_string();

        if available_width == 0 {
            return InlineInputLine {
                text: rendered,
                cursor_x: prefix_width.saturating_sub(1) as u16,
            };
        }

        let flattened = self
            .text
            .chars()
            .map(|ch| if ch == '\n' { ' ' } else { ch })
            .collect::<Vec<_>>();
        let widths = flattened
            .iter()
            .map(|ch| UnicodeWidthChar::width(*ch).unwrap_or(1).max(1))
            .collect::<Vec<_>>();
        let cursor_chars = self.cursor_chars.min(flattened.len());
        let mut start = cursor_chars;
        let mut width_before_cursor = 0usize;

        while start > 0 {
            let next_width = widths[start - 1];
            if width_before_cursor + next_width > available_width {
                break;
            }
            width_before_cursor += next_width;
            start -= 1;
        }

        let mut consumed_width = 0usize;
        for (index, ch) in flattened.iter().enumerate().skip(start) {
            let ch_width = widths[index];
            if consumed_width + ch_width > available_width {
                break;
            }
            rendered.push(*ch);
            consumed_width += ch_width;
        }

        let cursor_width = widths[start..cursor_chars].iter().sum::<usize>();
        let cursor_x = (prefix_width + cursor_width).min(width_limit.saturating_sub(1));

        InlineInputLine {
            text: rendered,
            cursor_x: cursor_x as u16,
        }
    }

    fn char_len(&self) -> usize {
        self.text.chars().count()
    }

    fn byte_index_for_char(&self, char_index: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_index)
            .map(|(byte_index, _)| byte_index)
            .unwrap_or(self.text.len())
    }

    fn layout(&self, full_width: u16) -> ComposerLayout {
        self.layout_with_prompt(full_width, None)
    }

    fn layout_with_prompt(&self, full_width: u16, prompt: Option<&str>) -> ComposerLayout {
        let width_limit = usize::from(full_width.max(1));
        let prompt_prefix = prompt.map(|value| format!("{value}> "));
        let prompt_width = prompt_prefix
            .as_deref()
            .map(unicode_width::UnicodeWidthStr::width)
            .unwrap_or(2);
        let mut lines = Vec::new();
        let mut current = prompt_prefix.clone().unwrap_or_else(|| String::from("> "));
        let mut x = prompt_width;
        let mut y = 0usize;
        let mut chars_seen = 0usize;
        let mut cursor = (prompt_width as u16, 0u16);

        if self.cursor_chars == 0 {
            cursor = (prompt_width as u16, 0);
        }

        for ch in self.text.chars() {
            if chars_seen == self.cursor_chars {
                cursor = (x as u16, y as u16);
            }

            if ch == '\n' {
                lines.push(std::mem::take(&mut current));
                current = String::from("  ");
                x = 2;
                y += 1;
                chars_seen += 1;
                continue;
            }

            let width = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
            if x + width > width_limit && !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                x = 0;
                y += 1;
            }

            current.push(ch);
            x += width;
            chars_seen += 1;

            if x >= width_limit {
                lines.push(std::mem::take(&mut current));
                x = 0;
                y += 1;
            }
        }

        if chars_seen == self.cursor_chars {
            cursor = (x as u16, y as u16);
        }

        if current.is_empty() {
            current = String::new();
        }
        lines.push(current);

        ComposerLayout { lines, cursor }
    }
}

/// Precomputed wrapped composer content and cursor position.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ComposerLayout {
    /// Fully wrapped visual lines exactly as the composer renders them.
    lines: Vec<String>,
    /// Cursor position within the wrapped visual lines.
    cursor: (u16, u16),
}

/// Single-line inline input representation with horizontal scrolling applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InlineInputLine {
    /// Rendered prompt prefix and visible input text.
    pub(crate) text: String,
    /// Cursor offset within the rendered line.
    pub(crate) cursor_x: u16,
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::InputBuffer;

    #[test]
    fn insert_and_backspace_follow_cursor_position() {
        let mut buffer = InputBuffer::new();
        buffer.insert_str("helo");
        buffer.move_left();
        buffer.move_left();
        buffer.insert_char('l');
        buffer.move_end();
        buffer.backspace();

        assert_eq!(buffer.text(), "hell");
    }

    #[test]
    fn delete_removes_character_under_cursor() {
        let mut buffer = InputBuffer::new();
        buffer.insert_str("abc");
        buffer.move_left();
        buffer.delete();

        assert_eq!(buffer.text(), "ab");
    }

    #[test]
    fn visual_cursor_wraps_long_lines() {
        let mut buffer = InputBuffer::new();
        buffer.insert_str("abcdef");

        assert_eq!(buffer.rendered_lines(4), vec!["> ab", "cdef", ""]);
        assert_eq!(buffer.visual_cursor(4), (0, 2));
        assert_eq!(buffer.rendered_lines(4).len(), 3);
    }

    #[test]
    fn visual_cursor_handles_newlines() {
        let mut buffer = InputBuffer::new();
        buffer.insert_str("a\nbc");

        assert_eq!(buffer.rendered_lines(10), vec!["> a", "  bc"]);
        assert_eq!(buffer.visual_cursor(10), (4, 1));
        assert_eq!(buffer.rendered_lines(10).len(), 2);
    }

    #[test]
    fn rendered_lines_keep_explicit_multiline_prefixes() {
        let mut buffer = InputBuffer::new();
        buffer.insert_str("hello\nworld");

        assert_eq!(buffer.rendered_lines(20), vec!["> hello", "  world"]);
    }

    #[test]
    fn blank_detection_ignores_whitespace() {
        let mut buffer = InputBuffer::new();
        buffer.insert_str(" \n ");

        assert!(buffer.is_blank());
    }

    #[test]
    fn inline_line_keeps_cursor_visible_when_text_overflows() {
        let mut buffer = InputBuffer::new();
        buffer.insert_str("abcdefghijklmnopqrstuvwxyz");

        let line = buffer.inline_line(12, "› ");

        assert_eq!(line.text, "› qrstuvwxyz");
        assert_eq!(line.cursor_x, 11);
    }

    #[test]
    fn inline_line_flattens_newlines() {
        let mut buffer = InputBuffer::new();
        buffer.insert_str("hi\nthere");

        let line = buffer.inline_line(20, "label> ");

        assert_eq!(line.text, "label> hi there");
        assert_eq!(line.cursor_x, 15);
    }
}
