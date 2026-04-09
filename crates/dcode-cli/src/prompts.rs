/// Prompt templates — load `.md` files from ~/.d-code/prompts/ and .d-code/prompts/.
///
/// Usage:
///   `/template-name [arg1] [arg2]` expands the template with positional args.
///
/// Template frontmatter (optional):
///   ```
///   ---
///   description: Short description of what this template does
///   ---
///   Template content with $1, $2, $@ placeholders.
///   ```
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct PromptTemplate {
    pub name: String,
    pub description: String,
    pub content: String,
    pub file_path: PathBuf,
}

/// Load all prompt templates from standard locations.
pub fn load_templates(cwd: &Path) -> Vec<PromptTemplate> {
    let mut templates: Vec<PromptTemplate> = vec![];
    let mut seen = std::collections::HashSet::new();

    // Global: ~/.d-code/prompts/
    if let Some(home) = dirs::home_dir() {
        let global_dir = home.join(".d-code").join("prompts");
        load_from_dir(&global_dir, &mut templates, &mut seen);
    }

    // Project-local: .d-code/prompts/ (relative to cwd)
    let project_dir = cwd.join(".d-code").join("prompts");
    load_from_dir(&project_dir, &mut templates, &mut seen);

    templates
}

fn load_from_dir(
    dir: &Path,
    templates: &mut Vec<PromptTemplate>,
    seen: &mut std::collections::HashSet<String>,
) {
    if !dir.exists() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.ends_with(".md") || name_str.starts_with('.') {
            continue;
        }
        let template_name = name_str.trim_end_matches(".md").to_string();
        if seen.contains(&template_name) {
            continue;
        }
        if let Some(t) = load_template_file(&path, &template_name) {
            seen.insert(template_name);
            templates.push(t);
        }
    }
}

fn load_template_file(path: &Path, name: &str) -> Option<PromptTemplate> {
    let raw = std::fs::read_to_string(path).ok()?;
    let (description, body) = parse_frontmatter(&raw);
    Some(PromptTemplate {
        name: name.to_string(),
        description,
        content: body,
        file_path: path.to_path_buf(),
    })
}

/// Parse optional YAML frontmatter; return (description, body).
fn parse_frontmatter(raw: &str) -> (String, String) {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("---") {
        // No frontmatter — use first line as description.
        let desc = trimmed
            .lines()
            .find(|l| !l.trim().is_empty())
            .map(|l| truncate(l.trim(), 80))
            .unwrap_or_default();
        return (desc, raw.to_string());
    }

    let rest = trimmed.strip_prefix("---").unwrap().trim_start_matches('\n');
    let Some(end) = rest.find("\n---") else {
        return (String::new(), raw.to_string());
    };
    let frontmatter = &rest[..end];
    let body_start = end + 4; // skip "\n---"
    let body = if body_start < rest.len() {
        rest[body_start..].trim_start_matches('\n').to_string()
    } else {
        String::new()
    };

    let mut description = String::new();
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("description:") {
            description = val.trim().trim_matches('"').trim_matches('\'').to_string();
            break;
        }
    }

    if description.is_empty() {
        // Fallback to first body line.
        description = body
            .lines()
            .find(|l| !l.trim().is_empty())
            .map(|l| truncate(l.trim(), 80))
            .unwrap_or_default();
    }

    (description, body)
}

/// Expand a `/template-name [args…]` input using loaded templates.
/// Returns None if no matching template is found.
pub fn expand(input: &str, templates: &[PromptTemplate]) -> Option<String> {
    let input = input.trim();
    if !input.starts_with('/') {
        return None;
    }
    let rest = &input[1..]; // strip leading '/'
    let (cmd, args_str) = rest.split_once(' ').unwrap_or((rest, ""));
    let template = templates.iter().find(|t| t.name == cmd)?;
    let args = parse_args(args_str);
    Some(substitute_args(&template.content, &args))
}

/// Parse bash-style quoted arguments.
fn parse_args(s: &str) -> Vec<String> {
    let mut args = vec![];
    let mut current = String::new();
    let mut in_quote: Option<char> = None;

    for ch in s.chars() {
        if let Some(q) = in_quote {
            if ch == q {
                in_quote = None;
            } else {
                current.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            in_quote = Some(ch);
        } else if ch == ' ' || ch == '\t' {
            if !current.is_empty() {
                args.push(current.clone());
                current.clear();
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

/// Substitute $1, $2, $@, $ARGUMENTS in template content.
fn substitute_args(content: &str, args: &[String]) -> String {
    let all_args = args.join(" ");

    // Replace $1, $2, … first to avoid re-substituting values.
    let mut result = String::with_capacity(content.len());
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            // Check for ${@:N} or ${@:N:L} slice syntax.
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                if let Some(end) = content[i..].find('}') {
                    let expr = &content[i + 2..i + end]; // content inside ${ }
                    if let Some(rest) = expr.strip_prefix("@:") {
                        let parts: Vec<&str> = rest.splitn(2, ':').collect();
                        let start = parts[0].parse::<usize>().unwrap_or(1).saturating_sub(1);
                        let slice = if parts.len() == 2 {
                            let len = parts[1].parse::<usize>().unwrap_or(0);
                            args.get(start..start + len)
                                .unwrap_or(&[])
                                .join(" ")
                        } else {
                            args.get(start..).unwrap_or(&[]).join(" ")
                        };
                        result.push_str(&slice);
                        i += end + 1;
                        continue;
                    }
                }
            }
            // Positional: $N
            if i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                let n: usize = content[i + 1..j].parse().unwrap_or(0);
                result.push_str(args.get(n.saturating_sub(1)).map(|s| s.as_str()).unwrap_or(""));
                i = j;
                continue;
            }
            result.push(bytes[i] as char);
        } else {
            result.push(bytes[i] as char);
        }
        i += 1;
    }

    // Replace $ARGUMENTS and $@.
    result
        .replace("$ARGUMENTS", &all_args)
        .replace("$@", &all_args)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let end = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
        format!("{}…", &s[..end])
    }
}
