# d-code session  —  copilot/gpt-5.2

**You:** hello

Hello. What do you want to do in this repo—fix a bug, add a feature, or run something (build/tests/CLI)?

**You:** what is the current project explain me

*Tool: `read_file({"path":"DCODE.md"})`*

*Tool: `glob({"pattern":"crates/**/Cargo.toml"})`*

*Tool: `list_dir({"max_depth":3,"path":"crates"})`*

`d-code` is a Rust workspace for building/running an “agentic” coding assistant CLI (and a TUI) that can talk to multiple LLM providers and safely use local tools (read files, grep, run commands, etc.).

### What’s in the project (crates)
- `crates/dcode-providers`  
  Abstraction over LLM backends (Anthropic, OpenAI, Copilot, Gemini, OpenRouter, etc.). Contains provider implementations and shared types.
- `crates/dcode-tools`  
  Built-in “tool” implementations the agent can call: `bash`, filesystem ops (`read_file`, `write_file`, `edit_file`), search (`grep`, `glob`), and web helpers.
- `crates/dcode-agent`  
  The core agent loop: runs “turns”, manages session state, does context compaction/summarization to stay within model limits, and builds prompts.
- `crates/dcode-cli`  
  The command-line program: REPL, command parsing, login/auth flows, rendering output to terminal (markdown-ish).
- `crates/dcode-tui`  
  A terminal UI layer (components, rendering primitives, TUI app wiring).

### How it works (high level)
1. CLI/TUI collects user input (and loads/stores sessions).
2. Agent builds a system prompt (includes `DCODE.md`) + conversation context.
3. Agent calls a selected provider (Anthropic/OpenAI/etc.).
4. Model can request tool calls (bash, read/search files, etc.) via `dcode-tools`.
5. Tool results are fed back into the agent loop until it produces a final answer.

### Useful commands
- `cargo build` (debug build)
- `cargo test`
- `cargo clippy`
- `cargo fmt`

If you tell me whether you care more about “how to run it” or “how the agent loop is implemented”, I can point to the exact entry points and main flow.

