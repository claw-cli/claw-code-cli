const HEAD_LIMIT: usize = 512 * 1024;
const TAIL_LIMIT: usize = 512 * 1024;

pub struct HeadTailBuffer {
    head: Vec<u8>,
    tail: Vec<u8>,
    total: usize,
    dropped: bool,
    head_limit: usize,
    tail_limit: usize,
}

impl HeadTailBuffer {
    pub fn new() -> Self {
        HeadTailBuffer {
            head: Vec::new(),
            tail: Vec::new(),
            total: 0,
            dropped: false,
            head_limit: HEAD_LIMIT,
            tail_limit: TAIL_LIMIT,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.total += bytes.len();

        if self.head.len() < self.head_limit {
            let space = self.head_limit - self.head.len();
            let take = bytes.len().min(space);
            self.head.extend_from_slice(&bytes[..take]);

            if take < bytes.len() {
                self.dropped = true;
                let remaining = &bytes[take..];

                if remaining.len() > self.tail_limit {
                    self.tail = remaining[remaining.len() - self.tail_limit..].to_vec();
                } else {
                    self.tail.extend_from_slice(remaining);
                    if self.tail.len() > self.tail_limit {
                        let excess = self.tail.len() - self.tail_limit;
                        self.tail.drain(0..excess);
                    }
                }
            }
        } else {
            self.dropped = true;
            if bytes.len() > self.tail_limit {
                self.tail = bytes[bytes.len() - self.tail_limit..].to_vec();
            } else {
                self.tail.extend_from_slice(bytes);
                if self.tail.len() > self.tail_limit {
                    let excess = self.tail.len() - self.tail_limit;
                    self.tail.drain(0..excess);
                }
            }
        }
    }

    pub fn collect(&self) -> String {
        let mut result = String::with_capacity(self.head.len() + self.tail.len() + 100);

        // SAFETY: head bytes are from PTY output, lossy conversion is acceptable
        let head_str = String::from_utf8_lossy(&self.head);
        result.push_str(&head_str);

        let tail_str = String::from_utf8_lossy(&self.tail);
        result.push_str(&tail_str);

        result
    }

    /// Collect raw bytes (for when callers need `Vec<u8>` directly)
    pub fn collect_bytes(&self) -> Vec<u8> {
        let mut result = Vec::with_capacity(self.head.len() + self.tail.len());
        result.extend_from_slice(&self.head);
        result.extend_from_slice(&self.tail);
        result
    }

    pub fn drain_collect_bytes(&mut self) -> Vec<u8> {
        let result = self.collect_bytes();
        self.head.clear();
        self.tail.clear();
        self.total = 0;
        self.dropped = false;
        result
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn truncated(&self) -> bool {
        self.dropped
    }
}

impl Default for HeadTailBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_keeps_small_content() {
        let mut buf = HeadTailBuffer::new();
        buf.push(b"hello world");
        let result = buf.collect();
        assert_eq!(result, "hello world");
        assert!(!buf.truncated());
    }

    #[test]
    fn buffer_keeps_head_and_tail_when_overflow() {
        let mut buf = HeadTailBuffer::new();
        buf.head_limit = 10;
        buf.tail_limit = 10;

        let data = b"0123456789ABCDEFGHIJ";
        buf.push(data);
        let result = buf.collect();
        assert!(result.starts_with("0123456789"));
        assert!(result.contains("GHIJ"));
        assert!(buf.truncated());
        assert_eq!(buf.total(), 20);
    }

    #[test]
    fn buffer_preserves_tail_across_multiple_pushes() {
        let mut buf = HeadTailBuffer::new();
        buf.head_limit = 5;
        buf.tail_limit = 5;

        buf.push(b"AAAAA");
        buf.push(b"BBBBB");
        buf.push(b"CCCCC");
        let result = buf.collect();
        assert!(result.starts_with("AAAAA"));
        assert!(result.contains("CCCCC"));
        assert!(buf.truncated());
    }

    #[test]
    fn empty_buffer_returns_empty() {
        let buf = HeadTailBuffer::new();
        assert!(buf.collect().is_empty());
    }

    #[test]
    fn buffer_utf8_multibyte_boundary() {
        let mut buf = HeadTailBuffer::new();
        buf.head_limit = 3;
        buf.tail_limit = 3;

        // 3-byte UTF-8 character "€" = [0xE2, 0x82, 0xAC]
        // Push data so that head limit is hit in the middle of the character
        buf.push(b"ab"); // 2 bytes
        buf.push(&[0xE2, 0x82]); // 2 bytes of a 3-byte char → head overflow
        buf.tail.extend_from_slice(&[0xAC]); // the last byte ends up in tail
        buf.dropped = true;

        let result = buf.collect();
        // collect() uses from_utf8_lossy which handles broken UTF-8 gracefully
        // The important thing is it doesn't panic
        assert!(result.contains('\u{FFFD}') || result.contains("ab"));
    }

    #[test]
    fn buffer_multiple_pushes_no_drop() {
        let mut buf = HeadTailBuffer::new();
        for i in 0..10 {
            buf.push(format!("line {i}\n").as_bytes());
        }
        assert!(!buf.truncated());
        let result = buf.collect();
        assert!(result.contains("line 0"));
        assert!(result.contains("line 9"));
    }

    #[test]
    fn buffer_zero_byte_push() {
        let mut buf = HeadTailBuffer::new();
        buf.push(b"");
        assert!(buf.collect().is_empty());
    }

    #[test]
    fn buffer_total_tracks_bytes() {
        let mut buf = HeadTailBuffer::new();
        buf.push(b"abc");
        buf.push(b"def");
        assert_eq!(buf.total(), 6);
    }

    #[test]
    fn buffer_collect_bytes_matches() {
        let mut buf = HeadTailBuffer::new();
        buf.push(b"hello");
        assert_eq!(&buf.collect_bytes(), b"hello");
    }

    #[test]
    fn buffer_drain_collect_bytes_clears_buffer() {
        let mut buf = HeadTailBuffer::new();
        buf.push(b"hello");

        assert_eq!(&buf.drain_collect_bytes(), b"hello");
        assert_eq!(&buf.collect_bytes(), b"");
        assert_eq!(buf.total(), 0);
        assert!(!buf.truncated());
    }

    #[test]
    fn buffer_truncation_preserves_tail() {
        let mut buf = HeadTailBuffer::new();
        buf.head_limit = 20;
        buf.tail_limit = 20;

        let data = "A".repeat(100);
        buf.push(data.as_bytes());
        assert!(buf.truncated());

        let result = buf.collect();
        // Should have head and tail without inserting a second truncation marker.
        assert!(result.starts_with("AAAAAAAAAA"));
        assert_eq!(result.len(), 40);
    }

    #[test]
    fn buffer_single_push_exactly_fits_head() {
        let mut buf = HeadTailBuffer::new();
        buf.head_limit = 10;

        let data = b"1234567890";
        buf.push(data);
        assert!(!buf.truncated());
        assert_eq!(buf.collect(), "1234567890");
    }

    #[test]
    fn buffer_collect_empty_on_new() {
        let buf = HeadTailBuffer::new();
        assert_eq!(buf.collect(), "");
        assert_eq!(buf.collect_bytes(), b"");
        assert_eq!(buf.total(), 0);
        assert!(!buf.truncated());
    }
}
