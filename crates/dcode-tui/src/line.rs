//! A single terminal line composed of [`Span`]s.

use crate::Span;

/// A single terminal row: a sequence of styled spans.
///
/// Tracks its own rendered string so the diff engine can compare
/// against the previous frame without re-rendering.
#[derive(Debug, Clone, Default)]
pub struct Line {
    spans: Vec<Span>,
    /// Cached rendered string (ANSI included). Invalidated on mutation.
    cached: Option<String>,
    /// Visible width in columns (no ANSI codes).
    cached_width: Option<usize>,
}

impl Line {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from a vec of spans.
    pub fn from_spans(spans: Vec<Span>) -> Self {
        Self { spans, cached: None, cached_width: None }
    }

    /// Single plain-text line.
    pub fn plain(text: impl Into<String>) -> Self {
        Self::from_spans(vec![Span::plain(text)])
    }

    /// Single styled line.
    pub fn styled(text: impl Into<String>, style: impl Into<String>) -> Self {
        Self::from_spans(vec![Span::styled(text, style)])
    }

    /// Build from a raw ANSI string (already contains escape codes).
    /// Visible width is approximated by stripping ANSI codes.
    pub fn raw(ansi: impl Into<String>) -> Self {
        let s = ansi.into();
        // Store as a single "pre-rendered" span with no additional style.
        // cached is set directly to avoid re-rendering.
        let mut line = Self::new();
        line.cached = Some(s.clone());
        line.cached_width = Some(strip_ansi_width(&s));
        line
    }

    /// Append a span.
    pub fn push(&mut self, span: Span) {
        self.spans.push(span);
        self.cached = None;
        self.cached_width = None;
    }

    /// Render to terminal string (with ANSI codes).
    pub fn render(&mut self) -> &str {
        if self.cached.is_none() {
            self.cached = Some(self.spans.iter().map(|s| s.render()).collect());
        }
        self.cached.as_deref().unwrap()
    }

    /// Visible column width (ANSI codes excluded).
    pub fn width(&mut self) -> usize {
        if self.cached_width.is_none() {
            let rendered = self.render().to_string();
            self.cached_width = Some(strip_ansi_width(&rendered));
        }
        self.cached_width.unwrap()
    }

    /// Whether this line equals another when rendered (used for diff).
    pub fn eq_rendered(&mut self, other: &mut Line) -> bool {
        self.render() == other.render()
    }
}

impl From<&str> for Line {
    fn from(s: &str) -> Self { Line::plain(s) }
}

impl From<String> for Line {
    fn from(s: String) -> Self { Line::plain(s) }
}

/// Approximate visible width of a string containing ANSI escape codes,
/// by stripping all `\x1b[...m` sequences before measuring.
pub fn strip_ansi_width(s: &str) -> usize {
    let stripped = strip_ansi(s);
    unicode_width::UnicodeWidthStr::width(stripped.as_str())
}

/// Remove ANSI escape sequences from a string.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Skip until 'm' (color), 'A'–'H' (cursor), 'J', 'K' (erase)
            while let Some(&next) = chars.peek() {
                chars.next();
                if next.is_ascii_alphabetic() { break; }
            }
        } else {
            out.push(ch);
        }
    }
    out
}
