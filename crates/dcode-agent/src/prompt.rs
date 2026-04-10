/// System prompt builder with project stack auto-detection and per-project config.
use std::path::Path;

/// Build the system prompt for d-code.
/// Auto-detects the project tech stack and injects build/test commands.
/// Also reads DCODE.md (or .d-code/PROMPT.md) for per-project context.
/// Appends skills loaded from ~/.d-code/skills/ and .d-code/skills/.
pub fn build_system_prompt(cwd: &Path) -> String {
    build_system_prompt_with_skills(cwd, None)
}

/// Same as `build_system_prompt` but accepts pre-loaded skills (avoids re-scanning).
pub fn build_system_prompt_with_skills(
    cwd: &Path,
    skills: Option<&[crate::skills::Skill]>,
) -> String {
    let date = chrono::Local::now().format("%Y-%m-%d");
    let cwd_str = cwd.display();

    // Detect tech stack for smart build/test hints.
    let stack = detect_stack(cwd);
    let stack_hint = if stack.is_empty() {
        String::new()
    } else {
        format!("\nStack: {stack}")
    };

    let core = format!(
        "You are d-code, a precise CLI coding agent. Today: {date}. CWD: {cwd_str}{stack_hint}

EFFICIENCY RULES (critical — follow exactly):
- BATCH tool calls: issue multiple read_file / grep / glob in ONE response. They run in parallel.
  BAD:  read_file A → wait → read_file B → wait → read_file C
  GOOD: read_file A + read_file B + read_file C in one response (all run simultaneously)
- NEVER read the same file twice. Cache what you learned.
- Use grep to find line numbers FIRST, then read only that range with start_line/end_line.
- Tool results are truncated — if output is cut off, use read_file with start_line/end_line or grep to get the specific section.
- edit_file over write_file for any existing file.
- After editing code: run build/test to verify. Fix ALL errors before stopping.
- Output ONLY results — no narration (\"I'll check…\", \"Let me look…\", \"Now I'll…\").
- Be concise: 1-5 lines unless producing code. No summaries of what you just did."
    );

    // Inject per-project context from DCODE.md or .d-code/PROMPT.md.
    let project_ctx = load_project_context(cwd);

    let mut prompt = if project_ctx.is_empty() {
        core
    } else {
        format!("{core}\n\n--- PROJECT CONTEXT ---\n{project_ctx}")
    };

    // Append skills section.
    let loaded_skills;
    let skill_slice: &[crate::skills::Skill] = match skills {
        Some(s) => s,
        None => {
            loaded_skills = crate::skills::load_skills(cwd);
            &loaded_skills
        }
    };
    if !skill_slice.is_empty() {
        prompt.push_str(&crate::skills::format_skills_for_prompt(skill_slice));
    }

    prompt
}

/// Detect the tech stack from files in CWD and return a compact hint string.
/// E.g. "Rust (cargo build / cargo test)"
fn detect_stack(cwd: &Path) -> String {
    let mut stacks: Vec<&'static str> = vec![];

    // Rust
    if cwd.join("Cargo.toml").exists() {
        stacks.push("Rust — build: `cargo build` · test: `cargo test` · lint: `cargo clippy`");
    }
    // Node / npm / bun / yarn / pnpm
    if cwd.join("package.json").exists() {
        let pm = if cwd.join("bun.lockb").exists() || cwd.join("bun.lock").exists() {
            "bun"
        } else if cwd.join("pnpm-lock.yaml").exists() {
            "pnpm"
        } else if cwd.join("yarn.lock").exists() {
            "yarn"
        } else {
            "npm"
        };
        stacks.push(match pm {
            "bun" => "Node/Bun — build: `bun run build` · test: `bun test`",
            "pnpm" => "Node/pnpm — build: `pnpm build` · test: `pnpm test`",
            "yarn" => "Node/Yarn — build: `yarn build` · test: `yarn test`",
            _ => "Node/npm — build: `npm run build` · test: `npm test`",
        });
    }
    // Python
    if cwd.join("pyproject.toml").exists() {
        stacks.push("Python — build: `pip install -e .` · test: `pytest`");
    } else if cwd.join("requirements.txt").exists() {
        stacks.push("Python — install: `pip install -r requirements.txt` · test: `pytest`");
    }
    // Go
    if cwd.join("go.mod").exists() {
        stacks.push("Go — build: `go build ./...` · test: `go test ./...`");
    }
    // Deno
    if cwd.join("deno.json").exists() || cwd.join("deno.jsonc").exists() {
        stacks.push("Deno — run: `deno run` · test: `deno test`");
    }
    // Make
    if cwd.join("Makefile").exists() || cwd.join("makefile").exists() {
        stacks.push("Make — build: `make` · test: `make test`");
    }
    // Docker
    if cwd.join("Dockerfile").exists() || cwd.join("docker-compose.yml").exists() {
        stacks.push("Docker — build: `docker build .` · compose: `docker-compose up`");
    }

    stacks.join(" | ")
}

/// Load per-project instructions. Walks from CWD up to the filesystem root,
/// collecting context from DCODE.md / AGENTS.md / CLAUDE.md in each directory.
/// Closest (most specific) context comes first; global ~/.d-code/AGENTS.md appended last.
fn load_project_context(cwd: &Path) -> String {
    const MAX_TOTAL: usize = 6_000;
    let mut parts: Vec<String> = vec![];

    // Walk up the directory tree.
    let mut dir = Some(cwd);
    while let Some(current) = dir {
        for name in &[
            "DCODE.md",
            "AGENTS.md",
            ".d-code/PROMPT.md",
            ".claude/CLAUDE.md",
        ] {
            let path = current.join(name);
            if let Ok(content) = std::fs::read_to_string(&path) {
                let trimmed = content.trim().to_string();
                if !trimmed.is_empty() {
                    let label = if current == cwd {
                        format!("# {}\n{trimmed}", name)
                    } else {
                        format!("# {} (from {})\n{trimmed}", name, current.display())
                    };
                    parts.push(label);
                    break; // Only one file per directory.
                }
            }
        }
        // Stop at filesystem root.
        dir = current.parent();
    }

    // Append global context from ~/.d-code/AGENTS.md if present.
    if let Some(home) = dirs::home_dir() {
        let global = home.join(".d-code").join("AGENTS.md");
        if let Ok(content) = std::fs::read_to_string(&global) {
            let trimmed = content.trim().to_string();
            if !trimmed.is_empty() {
                parts.push(format!("# AGENTS.md (global)\n{trimmed}"));
            }
        }
    }

    if parts.is_empty() {
        return String::new();
    }

    let combined = parts.join("\n\n");
    if combined.len() > MAX_TOTAL {
        let mut end = MAX_TOTAL;
        while end > 0 && !combined.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}\n[…truncated]", &combined[..end])
    } else {
        combined
    }
}
