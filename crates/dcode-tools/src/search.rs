/// Search tools: grep across files, recursive directory listing.
use std::path::Path;

use anyhow::Context;
use regex::Regex;
use walkdir::WalkDir;

const MAX_MATCHES: usize = 50;
const MAX_CONTEXT_LINES: usize = 2;

pub struct GrepArgs {
    pub pattern: String,
    pub path: String,
    /// File glob filter, e.g. "*.rs"
    pub file_glob: Option<String>,
    pub case_insensitive: bool,
    pub context_lines: Option<usize>,
}

pub fn grep_files(args: GrepArgs) -> anyhow::Result<String> {
    let re = if args.case_insensitive {
        Regex::new(&format!("(?i){}", args.pattern))
    } else {
        Regex::new(&args.pattern)
    }
    .context("invalid regex")?;

    let ctx_lines = args.context_lines.unwrap_or(0).min(MAX_CONTEXT_LINES);
    let root = Path::new(&args.path);
    let glob_filter = args.file_glob.as_deref();

    let mut results: Vec<String> = vec![];
    let mut match_count = 0usize;

    'outer: for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        if let Some(glob) = glob_filter {
            let name = entry.file_name().to_string_lossy();
            if !glob_matches(glob, &name) {
                continue;
            }
        }

        // Skip binary / large files.
        if entry.metadata().map(|m| m.len()).unwrap_or(0) > 2_000_000 {
            continue;
        }

        let path = entry.path();
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue, // binary or unreadable
        };

        let lines: Vec<&str> = content.lines().collect();

        for (i, line) in lines.iter().enumerate() {
            if re.is_match(line) {
                match_count += 1;
                if match_count > MAX_MATCHES {
                    results.push(format!(
                        "... ({} more matches, narrow your search)",
                        match_count - MAX_MATCHES
                    ));
                    break 'outer;
                }

                let start = i.saturating_sub(ctx_lines);
                let end = (i + ctx_lines + 1).min(lines.len());
                let rel = path.strip_prefix(root).unwrap_or(path);

                let block: Vec<String> = lines[start..end]
                    .iter()
                    .enumerate()
                    .map(|(j, l)| {
                        let lineno = start + j + 1;
                        let marker = if start + j == i { ">" } else { " " };
                        format!("{marker} {}:{lineno}: {l}", rel.display())
                    })
                    .collect();

                results.push(block.join("\n"));
            }
        }
    }

    if results.is_empty() {
        Ok(format!("No matches for '{}' in {}", args.pattern, args.path))
    } else {
        Ok(results.join("\n---\n"))
    }
}

/// Simple glob matching (only * wildcard).
fn glob_matches(pattern: &str, name: &str) -> bool {
    if let Some(ext) = pattern.strip_prefix("*.") {
        name.ends_with(&format!(".{ext}"))
    } else if pattern.contains('*') {
        // fallback: convert to regex
        let re_pat = regex::escape(pattern).replace("\\*", ".*");
        Regex::new(&format!("^{re_pat}$"))
            .map(|r| r.is_match(name))
            .unwrap_or(false)
    } else {
        name == pattern
    }
}

// ── Directory tree ─────────────────────────────────────────────────────────────

const MAX_TREE_ENTRIES: usize = 300;

pub fn list_directory(path: &str, max_depth: Option<usize>) -> anyhow::Result<String> {
    let root = Path::new(path);
    let depth = max_depth.unwrap_or(3);
    let mut lines = vec![];
    let mut count = 0;

    for entry in WalkDir::new(root)
        .max_depth(depth)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            // Skip hidden dirs and common noise.
            !e.path()
                .components()
                .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
                && !e
                    .path()
                    .components()
                    .any(|c| matches!(c.as_os_str().to_str(), Some("target" | "node_modules")))
        })
    {
        count += 1;
        if count > MAX_TREE_ENTRIES {
            lines.push(format!("... ({} more entries)", count - MAX_TREE_ENTRIES));
            break;
        }
        let depth = entry.depth();
        let indent = "  ".repeat(depth);
        let name = entry.file_name().to_string_lossy();
        let kind = if entry.file_type().is_dir() { "/" } else { "" };
        lines.push(format!("{indent}{name}{kind}"));
    }

    Ok(lines.join("\n"))
}
