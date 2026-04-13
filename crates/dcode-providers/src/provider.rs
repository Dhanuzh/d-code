use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::types::{Message, StreamEvent, ThinkingLevel, ToolDef};

/// Unified interface for all AI providers.
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    fn context_window(&self) -> u32;

    /// Fetch available model IDs from the provider. Falls back to static list on error.
    async fn list_models(&self) -> Vec<String>;

    /// Stream a chat completion response.
    async fn chat_stream(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        max_tokens: u32,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>>>;

    /// Set extended thinking budget. Default no-op (only Anthropic supports it).
    fn set_thinking_level(&mut self, _level: ThinkingLevel) {}
    fn thinking_level(&self) -> ThinkingLevel {
        ThinkingLevel::Off
    }
}

/// Boxed provider alias.
pub type BoxProvider = Box<dyn Provider>;
