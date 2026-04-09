/// Session state: message history + token tracking.
use dcode_providers::Message;

pub struct Session {
    pub messages: Vec<Message>,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
}

impl Session {
    pub fn new() -> Self {
        Self {
            messages: vec![],
            total_input_tokens: 0,
            total_output_tokens: 0,
        }
    }

    pub fn push(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    pub fn record_usage(&mut self, input: u32, output: u32) {
        self.total_input_tokens += input;
        self.total_output_tokens += output;
    }

    pub fn estimated_tokens(&self) -> usize {
        self.messages.iter().map(|m| m.estimate_tokens()).sum()
    }

    pub fn turn_count(&self) -> usize {
        self.messages
            .iter()
            .filter(|m| matches!(m.role, dcode_providers::Role::User))
            .count()
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}
