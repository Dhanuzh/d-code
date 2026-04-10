//! dcode-tui — differential terminal rendering engine.
//!
//! Modeled after pi-mono's packages/tui/src/tui.ts.
//!
//! Architecture:
//! - [`Component`] trait: anything that can render itself to a `Vec<Line>`.
//! - [`Tui`]: owns a list of components, runs differential rendering.
//! - [`Line`]: a single terminal line with optional ANSI styling.
//!
//! Differential rendering algorithm (same as pi-mono):
//! 1. Render all components → new_lines (Vec<String> with ANSI codes).
//! 2. Compare with prev_lines.
//! 3. Find first + last changed index.
//! 4. Move cursor to first changed line; write only the changed range.
//! 5. Clear any extra lines if new render is shorter.
//! 6. Wrap in synchronized-output escape (`\x1b[?2026h...\x1b[?2026l`) to
//!    prevent flicker on responsive terminals.

pub mod component;
pub mod components;
pub mod line;
pub mod span;
pub mod tui;

pub use component::Component;
pub use line::Line;
pub use span::Span;
pub use tui::Tui;

// Re-export all components at crate root for convenience.
pub use components::tool_execution::summarize_input;
pub use components::{AssistantMessage, InputBar, Spinner, StatusBar, ToolExecution, UserMessage};
