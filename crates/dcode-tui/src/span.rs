//! A styled text span — the atomic unit of styled terminal output.

/// A piece of text with optional ANSI color/attribute styling.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    /// The text content (no ANSI codes).
    pub text: String,
    /// Optional ANSI styling prefix (e.g. "\x1b[38;2;138;190;183m").
    /// If set, the reset "\x1b[0m" is appended automatically after the text.
    pub style: Option<String>,
}

impl Span {
    pub fn plain(text: impl Into<String>) -> Self {
        Self { text: text.into(), style: None }
    }

    pub fn styled(text: impl Into<String>, style: impl Into<String>) -> Self {
        Self { text: text.into(), style: Some(style.into()) }
    }

    /// RGB foreground color.
    pub fn rgb(text: impl Into<String>, r: u8, g: u8, b: u8) -> Self {
        Self::styled(text, format!("\x1b[38;2;{r};{g};{b}m"))
    }

    /// Render to a terminal string (with reset if styled).
    pub fn render(&self) -> String {
        if let Some(s) = &self.style {
            format!("{}{}\x1b[0m", s, self.text)
        } else {
            self.text.clone()
        }
    }

    /// Visible character width (ignoring ANSI codes).
    pub fn width(&self) -> usize {
        unicode_width::UnicodeWidthStr::width(self.text.as_str())
    }
}
