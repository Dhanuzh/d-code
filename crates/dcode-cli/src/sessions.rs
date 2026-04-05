/// Session persistence: save/load conversation sessions to ~/.d-code/sessions/
use std::path::PathBuf;

use dcode_providers::{ContentBlock, Message, Role};
use serde::{Deserialize, Serialize};

/// Maximum number of sessions to keep on disk.
const MAX_SESSIONS: usize = 30;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSession {
    pub id: String,
    /// Human-readable title extracted from the first user message.
    #[serde(default)]
    pub title: String,
    pub provider_model: String,
    pub messages: Vec<Message>,
    pub turn_count: usize,
    pub created_at: String,
    pub updated_at: String,
}

impl SavedSession {
    /// Returns the session title for display; falls back to id.
    pub fn display_title(&self) -> &str {
        if self.title.is_empty() {
            &self.id
        } else {
            &self.title
        }
    }

    /// Returns a short preview of the most recent assistant response.
    pub fn last_reply_preview(&self) -> String {
        for msg in self.messages.iter().rev() {
            if msg.role == Role::Assistant {
                for block in &msg.content {
                    if let ContentBlock::Text { text } = block {
                        let t = text.trim();
                        if !t.is_empty() && !t.starts_with('[') {
                            return truncate_str(t, 80);
                        }
                    }
                }
            }
        }
        String::new()
    }
}

pub fn sessions_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".d-code")
        .join("sessions")
}

pub fn save(provider_model: &str, messages: &[Message], turn_count: usize) -> Option<String> {
    if messages.is_empty() {
        return None;
    }
    let dir = sessions_dir();
    let _ = std::fs::create_dir_all(&dir);

    let now = chrono::Local::now();
    let id = now.format("%Y%m%d-%H%M%S").to_string();
    let title = extract_title(messages);

    let session = SavedSession {
        id: id.clone(),
        title,
        provider_model: provider_model.to_string(),
        messages: messages.to_vec(),
        turn_count,
        created_at: now.to_rfc3339(),
        updated_at: now.to_rfc3339(),
    };
    let path = dir.join(format!("{}.json", id));
    if let Ok(data) = serde_json::to_string(&session) {
        let _ = std::fs::write(path, data);
        // Prune old sessions to keep disk tidy.
        prune_old_sessions();
        Some(id)
    } else {
        None
    }
}

pub fn list() -> Vec<SavedSession> {
    let dir = sessions_dir();
    let mut sessions = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(data) = std::fs::read_to_string(&path) {
                    if let Ok(mut s) = serde_json::from_str::<SavedSession>(&data) {
                        // Back-fill title for sessions saved before title support.
                        if s.title.is_empty() {
                            s.title = extract_title(&s.messages);
                        }
                        sessions.push(s);
                    }
                }
            }
        }
    }
    // Sort newest first.
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    sessions
}

pub fn delete(id: &str) -> bool {
    let path = sessions_dir().join(format!("{}.json", id));
    std::fs::remove_file(path).is_ok()
}

#[allow(dead_code)]
pub fn load(id: &str) -> Option<SavedSession> {
    let path = sessions_dir().join(format!("{}.json", id));
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

pub fn load_latest() -> Option<SavedSession> {
    list().into_iter().next()
}

/// Extract a short title from the first user text message.
fn extract_title(messages: &[Message]) -> String {
    for msg in messages {
        if msg.role == Role::User {
            for block in &msg.content {
                if let ContentBlock::Text { text } = block {
                    let t = text.trim();
                    if !t.is_empty() && !t.starts_with('[') {
                        // Take first line, cap at 60 chars.
                        let first_line = t.lines().next().unwrap_or(t);
                        return truncate_str(first_line, 60);
                    }
                }
            }
        }
    }
    String::new()
}

/// Remove oldest sessions beyond MAX_SESSIONS.
fn prune_old_sessions() {
    let sessions = list();
    if sessions.len() <= MAX_SESSIONS {
        return;
    }
    for old in sessions.iter().skip(MAX_SESSIONS) {
        delete(&old.id);
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        let end: usize = s
            .char_indices()
            .nth(max)
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        format!("{}…", &s[..end])
    }
}
