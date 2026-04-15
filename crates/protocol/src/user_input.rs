/// The implementation refernce to codex: `codex-rs\protocol\src\user_input.rs`.
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// Conservative cap so one user message cannot monopolize a large context window.
/// Upper bound for a single text submission.
pub const INPUT_TEXT_LIMIT: usize = 1 << 20;

/// User input
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserInput {
    Text {
        text: String,
        /// UI-defined spans within `text` that should be treated as special elements.
        /// These are byte ranges into the UTF-8 `text` buffer and are used to render
        /// or persist rich input markers (e.g., image placeholders) across history
        /// and resume without mutating the literal text.
        #[serde(default)]
        text_elements: Vec<TextElement>,
    },
    /// Pre‑encoded data: URI image.
    Image { image_url: String },

    /// Local image path provided by the user.  This will be converted to an
    /// `Image` variant (base64 data URL) during request serialization.
    LocalImage { path: std::path::PathBuf },

    /// Skill selected by the user (name + path to SKILL.md).
    Skill {
        name: String,
        path: std::path::PathBuf,
    },
    /// Explicit structured mention selected by the user.
    ///
    /// `path` identifies the exact mention target, for example
    /// `app://<connector-id>` or `plugin://<plugin-name>@<marketplace-name>`.
    Mention { name: String, path: String },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct TextElement {
    /// Byte offsets inside the parent text buffer.
    pub byte_range: Utf8ByteSpan,
    /// Optional human-readable placeholder for the element, displayed in the UI.
    placeholder: Option<String>,
}

impl TextElement {
    pub fn new(byte_range: Utf8ByteSpan, placeholder: Option<String>) -> Self {
        Self {
            byte_range,
            placeholder,
        }
    }

    /// Returns a copy of this element with a remapped byte range.
    ///
    /// The placeholder is preserved as-is; callers must ensure the new range
    /// still refers to the same logical element (and same placeholder)
    /// within the new text.
    pub fn map_range<F>(&self, map: F) -> Self
    where
        F: FnOnce(Utf8ByteSpan) -> Utf8ByteSpan,
    {
        Self {
            byte_range: map(self.byte_range),
            placeholder: self.placeholder.clone(),
        }
    }

    pub fn set_placeholder(&mut self, placeholder: Option<String>) {
        self.placeholder = placeholder;
    }

    /// Returns the stored placeholder without falling back to the text buffer.
    ///
    /// This must only be used inside `From<TextElement>` implementations on equivalent
    /// protocol types where the source text is unavailable. Prefer `placeholder(text)`
    /// everywhere else.
    #[doc(hidden)]
    pub fn _placeholder_for_conversion_only(&self) -> Option<&str> {
        self.placeholder.as_deref()
    }

    pub fn placeholder<'a>(&'a self, text: &'a str) -> Option<&'a str> {
        self.placeholder
            .as_deref()
            .or_else(|| text.get(self.byte_range.start..self.byte_range.end))
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema)]
pub struct Utf8ByteSpan {
    /// Start byte offset (inclusive) within the UTF-8 text buffer.
    pub start: usize,
    /// End byte offset (exclusive) within the UTF-8 text buffer.
    pub end: usize,
}

impl From<std::ops::Range<usize>> for Utf8ByteSpan {
    fn from(range: std::ops::Range<usize>) -> Self {
        Self {
            start: range.start,
            end: range.end,
        }
    }
}
