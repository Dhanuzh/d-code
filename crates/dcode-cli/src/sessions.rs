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
    /// User-defined display name (set via /name command).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub provider_model: String,
    pub messages: Vec<Message>,
    pub turn_count: usize,
    pub created_at: String,
    pub updated_at: String,
    /// Parent session ID for forked sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

impl SavedSession {
    /// Returns the session title for display; prefers display_name, then title, then id.
    pub fn display_title(&self) -> &str {
        if let Some(name) = &self.display_name {
            if !name.is_empty() {
                return name.as_str();
            }
        }
        if !self.title.is_empty() {
            return &self.title;
        }
        &self.id
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

#[allow(dead_code)]
pub fn save(provider_model: &str, messages: &[Message], turn_count: usize) -> Option<String> {
    save_with_opts(provider_model, messages, turn_count, None, None, None)
}

/// Save with optional existing id (update), display name, and parent id.
pub fn save_with_opts(
    provider_model: &str,
    messages: &[Message],
    turn_count: usize,
    existing_id: Option<&str>,
    display_name: Option<&str>,
    parent_id: Option<&str>,
) -> Option<String> {
    if messages.is_empty() {
        return None;
    }
    let dir = sessions_dir();
    let _ = std::fs::create_dir_all(&dir);

    let now = chrono::Local::now();
    let id = existing_id
        .map(|s| s.to_string())
        .unwrap_or_else(|| now.format("%Y%m%d-%H%M%S").to_string());
    let title = extract_title(messages);

    // Load existing session to preserve created_at and display_name if updating.
    let (created_at, existing_display_name, existing_parent_id) = if let Some(eid) = existing_id {
        if let Some(old) = load(eid) {
            (old.created_at, old.display_name, old.parent_id)
        } else {
            (now.to_rfc3339(), None, None)
        }
    } else {
        (now.to_rfc3339(), None, None)
    };

    let session = SavedSession {
        id: id.clone(),
        title,
        display_name: display_name
            .map(|s| s.to_string())
            .or(existing_display_name),
        provider_model: provider_model.to_string(),
        messages: messages.to_vec(),
        turn_count,
        created_at,
        updated_at: now.to_rfc3339(),
        parent_id: parent_id.map(|s| s.to_string()).or(existing_parent_id),
    };
    let path = dir.join(format!("{}.json", id));
    if let Ok(data) = serde_json::to_string(&session) {
        let _ = std::fs::write(path, data);
        prune_old_sessions();
        Some(id)
    } else {
        None
    }
}

/// Update only the display_name field of an existing session.
pub fn set_display_name(id: &str, name: &str) -> bool {
    let path = sessions_dir().join(format!("{}.json", id));
    let Ok(data) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(mut session) = serde_json::from_str::<SavedSession>(&data) else {
        return false;
    };
    session.display_name = if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    };
    let now = chrono::Local::now();
    session.updated_at = now.to_rfc3339();
    if let Ok(new_data) = serde_json::to_string(&session) {
        std::fs::write(path, new_data).is_ok()
    } else {
        false
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
        let end: usize = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
        format!("{}…", &s[..end])
    }
}
