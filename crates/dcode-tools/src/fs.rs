/// File-system tools: read, write, edit, list.
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};

const MAX_READ_LINES: usize = 500;
const MAX_WRITE_BYTES: usize = 1_024 * 1_024; // 1 MiB guard

// ── Read file ──────────────────────────────────────────────────────────────────

pub struct ReadArgs {
    pub path: String,
    /// 1-based start line (inclusive, optional).
    pub start_line: Option<usize>,
    /// 1-based end line (inclusive, optional).
    pub end_line: Option<usize>,
}

pub fn read_file(args: ReadArgs) -> anyhow::Result<String> {
    let path = PathBuf::from(&args.path);
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", args.path))?;

    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    let start = args.start_line.map(|n| n.saturating_sub(1)).unwrap_or(0);
    let end = args
        .end_line
        .map(|n| n.min(total))
        .unwrap_or_else(|| (start + MAX_READ_LINES).min(total));

    if start >= total && total > 0 {
        bail!("start_line {start} exceeds file length {total}");
    }

    let selected = &lines[start..end];
    let header = if total > MAX_READ_LINES {
        format!(
            "// Lines {}-{} of {} total ({})\n",
            start + 1,
            end,
            total,
            args.path
        )
    } else {
        String::new()
    };

    Ok(format!(
        "{header}{}",
        selected
            .iter()
            .enumerate()
            .map(|(i, l)| format!("{:>4} | {l}", start + i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

// ── Write file ─────────────────────────────────────────────────────────────────

pub fn write_file(path: &str, content: &str) -> anyhow::Result<String> {
    if content.len() > MAX_WRITE_BYTES {
        bail!("content too large ({} bytes)", content.len());
    }
    let p = Path::new(path);
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dirs for {path}"))?;
        }
    }
    std::fs::write(p, content).with_context(|| format!("write {path}"))?;
    Ok(format!("Written {path} ({} bytes)", content.len()))
}

// ── Edit file ──────────────────────────────────────────────────────────────────

pub struct EditArgs {
    pub path: String,
    pub old_string: String,
    pub new_string: String,
}

pub fn edit_file(args: EditArgs) -> anyhow::Result<String> {
    let content = std::fs::read_to_string(&args.path)
        .with_context(|| format!("read {}", args.path))?;

    // Count occurrences to catch ambiguous edits.
    let count = content.matches(&args.old_string).count();
    match count {
        0 => bail!(
            "old_string not found in {}. Check whitespace/indentation.",
            args.path
        ),
        n if n > 1 => bail!(
            "old_string matches {n} locations in {}. Provide more context to make it unique.",
            args.path
        ),
        _ => {}
    }

    let new_content = content.replacen(&args.old_string, &args.new_string, 1);
    std::fs::write(&args.path, &new_content)
        .with_context(|| format!("write {}", args.path))?;

    // Produce a short diff summary.
    let old_lines = args.old_string.lines().count();
    let new_lines = args.new_string.lines().count();
    Ok(format!(
        "Edited {} (-{old_lines} +{new_lines} lines)",
        args.path
    ))
}

// ── List / glob ────────────────────────────────────────────────────────────────

const MAX_GLOB_RESULTS: usize = 200;

pub fn list_files(pattern: &str) -> anyhow::Result<String> {
    let paths: Vec<_> = glob::glob(pattern)
        .context("invalid glob pattern")?
        .filter_map(|r| r.ok())
        .filter(|p| p.is_file())
        .take(MAX_GLOB_RESULTS)
        .collect();

    if paths.is_empty() {
        return Ok(format!("No files matched: {pattern}"));
    }

    let mut lines: Vec<String> = paths
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    lines.sort();

    let truncated = if paths.len() == MAX_GLOB_RESULTS {
        format!("\n(showing first {MAX_GLOB_RESULTS})")
    } else {
        String::new()
    };

    Ok(format!("{}{truncated}", lines.join("\n")))
}
