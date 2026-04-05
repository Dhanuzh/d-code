# d-code project context

## Architecture
Rust workspace with 4 crates:
- `crates/dcode-providers` — LLM provider abstraction (Anthropic, OpenAI, Copilot)
- `crates/dcode-tools`    — Built-in tools (bash, read_file, write_file, edit_file, grep, glob, list_dir)
- `crates/dcode-agent`    — Agentic loop, context compaction, session management
- `crates/dcode-cli`      — CLI binary, REPL, rendering

## Build
```
cargo build          # debug
cargo build --release  # release (optimized, stripped)
cargo clippy         # lint
cargo fmt            # format
```

## Key files
- `crates/dcode-agent/src/lib.rs`      — core run_turn loop
- `crates/dcode-agent/src/compact.rs`  — context compaction logic
- `crates/dcode-agent/src/prompt.rs`   — system prompt builder (reads DCODE.md)
- `crates/dcode-providers/src/anthropic.rs` — Anthropic provider (prompt caching enabled)
- `crates/dcode-cli/src/repl.rs`       — interactive REPL
- `crates/dcode-cli/src/render.rs`     — terminal markdown renderer

## Conventions
- No unwrap() in library code — use anyhow::Result
- Prefer edit_file over write_file for changes to existing files
- All tools have max output limits to protect context window
- System prompt is kept minimal; per-project context lives in DCODE.md

## npm publishing
See `npm/` directory and `.github/workflows/release.yml`.
Push a git tag `vX.Y.Z` to trigger the release pipeline.
