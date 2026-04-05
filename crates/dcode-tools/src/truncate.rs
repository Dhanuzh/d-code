/// Large-output-to-disk offloader.
///
/// Inspired by opencode's truncation pattern:
/// when a tool produces output larger than the context threshold,
/// the full output is saved to a temp file on disk and the model
/// receives a preview + a hint on how to access the rest.
///
/// This keeps context tokens low while still giving the model access
/// to large outputs via targeted grep/read_file calls.
use std::path::Path;

/// Inline limit: outputs below this size go directly into context (≈4 000 tokens).
const INLINE_LIMIT_BYTES: usize = 16_000;

/// Preview size returned when output is offloaded (≈500 tokens).
const PREVIEW_BYTES: usize = 2_000;

/// Check whether `output` exceeds the inline limit.
/// If it does: save full content to `~/.d-code/tmp/<tool>-<hash>.txt`
/// and return a compact preview + access hint.
/// If it fits: return the output unchanged.
pub fn maybe_offload(output: String, tool_name: &str, _cwd: &Path) -> String {
    if output.len() <= INLINE_LIMIT_BYTES {
        return output;
    }

    // Try to save to disk; if that fails just truncate gracefully.
    match save_to_disk(&output, tool_name) {
        Ok(path) => {
            let preview = preview(&output, PREVIEW_BYTES);
            let total_lines = output.lines().count();
            format!(
                "{preview}\n\
                 \n\
                 [Output truncated — {total_lines} lines total ({} bytes)]\n\
                 Full output saved to: {path}\n\
                 Use read_file with start_line/end_line to read specific sections,\n\
                 or grep to search within it.",
                output.len(),
            )
        }
        Err(_) => {
            // Fallback: just truncate in-place.
            let preview = preview(&output, INLINE_LIMIT_BYTES);
            let removed = output.len() - INLINE_LIMIT_BYTES;
            format!(
                "{preview}\n[… +{removed} bytes truncated — use grep or read_file with line ranges to view more]"
            )
        }
    }
}

fn save_to_disk(content: &str, tool_name: &str) -> anyhow::Result<String> {
    let tmp_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".d-code")
        .join("tmp");
    std::fs::create_dir_all(&tmp_dir)?;

    // Use a simple hash of the content for a stable, collision-resistant filename.
    let hash = simple_hash(content);
    let filename = format!("{tool_name}-{hash:x}.txt");
    let path = tmp_dir.join(&filename);

    std::fs::write(&path, content)?;

    Ok(path.to_string_lossy().to_string())
}

/// Return the first `max_bytes` of `s`, cut on a newline boundary if possible.
fn preview(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let slice = &s[..max_bytes];
    // Try to cut on a newline so the preview ends cleanly.
    match slice.rfind('\n') {
        Some(pos) if pos > max_bytes / 2 => &s[..pos],
        _ => slice,
    }
}

/// Cheap, non-cryptographic hash for generating stable filenames.
fn simple_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325; // FNV-1a offset basis
    for byte in s.bytes().take(4096) {
        h ^= byte as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Cleanup tmp files older than 7 days.
/// Call this on startup or from a background task.
pub fn cleanup_old_tmp() {
    let tmp_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".d-code")
        .join("tmp");

    let threshold = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(7 * 24 * 3600))
        .unwrap_or(std::time::UNIX_EPOCH);

    if let Ok(entries) = std::fs::read_dir(&tmp_dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    if modified < threshold {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
}
