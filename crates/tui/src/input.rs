use unicode_width::UnicodeWidthChar;

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

    /// Returns the rendered cursor position for a wrapped text area.
    pub(crate) fn visual_cursor(&self, inner_width: u16) -> (u16, u16) {
        if inner_width == 0 {
            return (0, 0);
        }

        let width_limit = usize::from(inner_width);
        let mut x = 0usize;
        let mut y = 0usize;
        for ch in self.text.chars().take(self.cursor_chars) {
            if ch == '\n' {
                x = 0;
                y += 1;
                continue;
            }

            let width = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
            if x + width > width_limit {
                x = 0;
                y += 1;
            }
            x += width;
            if x >= width_limit {
                x = 0;
                y += 1;
            }
        }

        (x as u16, y as u16)
    }

    /// Returns the number of visual lines needed to render the composer.
    pub(crate) fn visual_line_count(&self, inner_width: u16) -> u16 {
        let (_, y) = self.visual_cursor(inner_width);
        let current_line = self
            .text
            .chars()
            .last()
            .is_some_and(|last| last == '\n')
            .then_some(1)
            .unwrap_or(0);
        y.saturating_add(1 + current_line)
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

        assert_eq!(buffer.visual_cursor(4), (2, 1));
        assert_eq!(buffer.visual_line_count(4), 2);
    }

    #[test]
    fn visual_cursor_handles_newlines() {
        let mut buffer = InputBuffer::new();
        buffer.insert_str("a\nbc");

        assert_eq!(buffer.visual_cursor(10), (2, 1));
        assert_eq!(buffer.visual_line_count(10), 2);
    }

    #[test]
    fn blank_detection_ignores_whitespace() {
        let mut buffer = InputBuffer::new();
        buffer.insert_str(" \n ");

        assert!(buffer.is_blank());
    }
}
