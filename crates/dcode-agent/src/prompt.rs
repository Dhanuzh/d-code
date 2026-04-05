/// System prompt builder with project stack auto-detection and per-project config.
use std::path::Path;

/// Build the system prompt for d-code.
/// Auto-detects the project tech stack and injects build/test commands.
/// Also reads DCODE.md (or .d-code/PROMPT.md) for per-project context.
pub fn build_system_prompt(cwd: &Path) -> String {
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

RULES:
- Never output tool call XML — tools run internally.
- Be concise: answer in 1-5 lines unless producing code/files.
- Output ONLY results — no narration like \"I'll run X\" or \"Let me check Y\".
- For large files: read in line-range slices (start_line/end_line), never read the whole file.
- Prefer edit_file over write_file for targeted changes.
- Use grep/glob to locate files before reading in large projects.
- After writing/editing code: run the build/test command to verify.
- When a task spans multiple files: use list_dir or glob first to map structure.
- Chain tool calls efficiently — plan before acting on big refactors."
    );

    // Inject per-project context from DCODE.md or .d-code/PROMPT.md.
    let project_ctx = load_project_context(cwd);

    if project_ctx.is_empty() {
        core
    } else {
        format!("{core}\n\n--- PROJECT CONTEXT ---\n{project_ctx}")
    }
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
            "bun"  => "Node/Bun — build: `bun run build` · test: `bun test`",
            "pnpm" => "Node/pnpm — build: `pnpm build` · test: `pnpm test`",
            "yarn" => "Node/Yarn — build: `yarn build` · test: `yarn test`",
            _      => "Node/npm — build: `npm run build` · test: `npm test`",
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
        for name in &["DCODE.md", "AGENTS.md", ".d-code/PROMPT.md", ".claude/CLAUDE.md"] {
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
