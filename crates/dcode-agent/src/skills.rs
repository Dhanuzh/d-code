/// Skills system — load agent skill files from ~/.d-code/skills/ and .d-code/skills/.
///
/// Skill format (pi-mono Agent Skills spec):
///   - Simple: `skill-name.md` in a skills directory
///   - Directory: `skill-name/SKILL.md` (directory name is the skill name)
///   - Frontmatter: `---\ndescription: Short description\n---\n<skill content>`
///
/// Skills are injected into the system prompt as XML so the model knows to
/// read the skill file when a matching task is requested.
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub file_path: PathBuf,
}

/// Load all skills from the standard locations:
/// 1. `~/.d-code/skills/` (global)
/// 2. `.d-code/skills/` relative to `cwd` (project-local)
pub fn load_skills(cwd: &Path) -> Vec<Skill> {
    let mut skills: Vec<Skill> = vec![];
    let mut seen_names = std::collections::HashSet::new();

    // Global skills: ~/.d-code/skills/
    if let Some(home) = dirs::home_dir() {
        let global_dir = home.join(".d-code").join("skills");
        load_skills_from_dir(&global_dir, &mut skills, &mut seen_names);
    }

    // Project-local skills: .d-code/skills/
    let project_dir = cwd.join(".d-code").join("skills");
    load_skills_from_dir(&project_dir, &mut skills, &mut seen_names);

    skills
}

fn load_skills_from_dir(
    dir: &Path,
    skills: &mut Vec<Skill>,
    seen_names: &mut std::collections::HashSet<String>,
) {
    if !dir.exists() {
        return;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if name_str.starts_with('.') {
            continue;
        }

        if path.is_dir() {
            // Directory format: skill-name/SKILL.md
            let skill_file = path.join("SKILL.md");
            if skill_file.exists() {
                let skill_name = name_str.to_string();
                if seen_names.contains(&skill_name) {
                    continue;
                }
                if let Some(skill) = load_skill_file(&skill_file, &skill_name) {
                    seen_names.insert(skill_name);
                    skills.push(skill);
                }
            }
        } else if path.is_file() && name_str.ends_with(".md") {
            // Simple format: skill-name.md
            let skill_name = name_str.trim_end_matches(".md").to_string();
            if seen_names.contains(&skill_name) {
                continue;
            }
            if let Some(skill) = load_skill_file(&path, &skill_name) {
                seen_names.insert(skill_name);
                skills.push(skill);
            }
        }
    }
}

fn load_skill_file(path: &Path, name: &str) -> Option<Skill> {
    let content = std::fs::read_to_string(path).ok()?;
    let description = extract_description(&content)?;

    Some(Skill {
        name: name.to_string(),
        description,
        file_path: path.to_path_buf(),
    })
}

/// Extract `description` from YAML frontmatter.
/// Returns None if no description found.
fn extract_description(content: &str) -> Option<String> {
    let body = content.trim_start();

    if !body.starts_with("---") {
        // No frontmatter — use first non-empty line as description (truncated).
        return body
            .lines()
            .find(|l| !l.trim().is_empty())
            .map(|l| truncate_str(l.trim(), 120));
    }

    // Parse YAML frontmatter between --- delimiters.
    let rest = body.strip_prefix("---")?.trim_start_matches('\n');
    let end = rest.find("\n---")?;
    let frontmatter = &rest[..end];

    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("description:") {
            let val = val.trim().trim_matches('"').trim_matches('\'');
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }

    // Fallback: use first line of body after frontmatter.
    let body_start = end + 4; // skip "\n---"
    if body_start < rest.len() {
        rest[body_start..]
            .lines()
            .find(|l| !l.trim().is_empty())
            .map(|l| truncate_str(l.trim(), 120))
    } else {
        None
    }
}

/// Format skills as XML for injection into the system prompt.
pub fn format_skills_for_prompt(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut out = String::from(
        "\n\nThe following skills provide specialized instructions for specific tasks. \
         Use the read_file tool to load a skill's file when the task matches its description.\n\n\
         <available_skills>\n",
    );

    for skill in skills {
        out.push_str("  <skill>\n");
        out.push_str(&format!("    <name>{}</name>\n", escape_xml(&skill.name)));
        out.push_str(&format!(
            "    <description>{}</description>\n",
            escape_xml(&skill.description)
        ));
        out.push_str(&format!(
            "    <location>{}</location>\n",
            escape_xml(&skill.file_path.display().to_string())
        ));
        out.push_str("  </skill>\n");
    }

    out.push_str("</available_skills>");
    out
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let end = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
        format!("{}…", &s[..end])
    }
}
