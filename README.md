# d-code

A lightweight, fast CLI coding agent powered by Claude, GPT-4, and GitHub Copilot.
Runs entirely in your terminal — no browser, no Electron, no cloud sync.

```
 anthropic/claude-sonnet-4-5 ▸ fix the bug in src/auth.rs

  ✓ read_file  src/auth.rs
  ✏ edit_file  src/auth.rs
  ⚡ bash       cargo test

  All 12 tests pass.
```

---

## Install

**npm (recommended)**
```bash
npm install -g d-code
```

**Cargo**
```bash
cargo install --git https://github.com/ddhanush1/d-code
```

**Download binary** — grab the latest release from [GitHub Releases](https://github.com/ddhanush1/d-code/releases).

---

## Quick start

```bash
# 1. Login (choose one)
d-code login anthropic   # Claude — OAuth PKCE, no API key needed
d-code login copilot     # GitHub Copilot — device-code flow
d-code login openai      # OpenAI — paste your API key

# 2. Start coding
d-code                   # interactive REPL
d-code -p "explain this repo"  # one-shot mode
```

---

## Providers & Models

| Provider | Default model | Auth |
|----------|--------------|------|
| `anthropic` | claude-sonnet-4-5 | OAuth PKCE |
| `copilot` | gpt-4o-mini | GitHub device-code |
| `openai` | gpt-4.1-mini | API key |

**Switch model in the REPL:**
```
/model                            # interactive picker
/model anthropic/claude-opus-4-5  # direct switch
/model copilot/claude-sonnet-4.5  # Copilot routing Claude
```

**Available models:**
- Anthropic: `claude-sonnet-4-6`, `claude-sonnet-4-5`, `claude-opus-4-5`, `claude-opus-4-1`, `claude-haiku-4-5-20251001`
- OpenAI: `gpt-4.1`, `gpt-4.1-mini`, `gpt-4.1-nano`, `gpt-4o`, `o3`, `o4-mini`
- Copilot: `gpt-4o`, `gpt-4.1`, `claude-sonnet-4`, `gemini-2.5-pro`

---

## Built-in tools

The agent uses these tools automatically based on your request:

| Tool | What it does |
|------|-------------|
| `read_file` | Read files with optional line ranges (up to 1000 lines) |
| `write_file` | Create new files |
| `edit_file` | Replace exact strings in existing files |
| `bash` | Run shell commands (build, test, git, etc.) — 60s timeout |
| `grep` | Regex search across files with context lines |
| `glob` | Find files matching a pattern |
| `list_dir` | Recursive directory tree |

---

## REPL commands

```
/help           Show all commands
/status         Token usage, context %, estimated cost
/compact        Force context compaction (saves tokens)
/undo           Remove the last turn from context
/clear          Clear conversation (no save)
/new            Save session and start fresh
/sessions       Browse and resume saved sessions
/resume         Resume the latest session
/model          Switch model (interactive or /model provider/name)
/login [p]      Login to a provider
/logout [p]     Logout from a provider
/quit           Exit
```

**Keyboard shortcuts:**
- `Shift+Enter` — newline (multi-line input)
- `Ctrl+U` — clear current line
- `Ctrl+C` — cancel input / exit on empty line
- `↑/↓` — history navigation
- `Tab` — complete slash commands

---

## One-shot mode

```bash
d-code -p "what does this repo do"
d-code -p "add error handling to src/main.rs" -P anthropic
d-code -p "run tests and fix failures" -C /path/to/project
```

---

## Per-project config (DCODE.md)

Drop a `DCODE.md` in your project root to give d-code project-specific context:

```markdown
# My API server

## Stack
Node.js, Express, PostgreSQL, Jest

## Commands
- Build: `npm run build`
- Test: `npm test`
- Start: `npm start`

## Conventions
- Use async/await, never callbacks
- All DB queries go through src/db/index.ts
- Tests live alongside source files (*.test.ts)
```

d-code auto-injects this into the system prompt on every turn. No manual setup.

---

## Token usage & cost

d-code is designed to minimize token usage:

- **Prompt caching** (Anthropic) — system prompt + tools cached after first call; ~60-90% cheaper on cache hits
- **Smart compaction** — context summarized when approaching the limit
- **Tool result sizing** — large outputs (>16 KB) saved to `~/.d-code/tmp/` and referenced by path
- **Doom loop detection** — stops if the same tool call repeats 3× with identical args

Check your usage anytime with `/status`.

---

## Sessions

Conversations are auto-saved to `~/.d-code/sessions/`. Resume them with:

```
/sessions    # browse all sessions
/resume      # jump back into the last one
```

Sessions include full message history with tool calls and results.

---

## CLI flags

```
d-code [OPTIONS] [COMMAND]

Options:
  -p, --prompt <PROMPT>      One-shot prompt
  -P, --provider <PROVIDER>  Provider: anthropic, copilot, openai
  -C, --dir <DIR>            Working directory (default: current)

Commands:
  login [PROVIDER]   Login to a provider
  logout <PROVIDER>  Logout (or 'all')
  status             Show login status
```

---

## Building from source

**Requirements:** Rust 1.80+

```bash
git clone https://github.com/ddhanush1/d-code
cd d-code
cargo build --release
# Binary at: target/release/d-code
```

---

## Publishing a new release

Push a version tag to trigger the GitHub Actions release pipeline:

```bash
git tag v0.2.0
git push --tags
# → Builds Linux/macOS/Windows binaries → GitHub Release → npm publish
```

Requires `NPM_TOKEN` secret in GitHub repo settings.

---

## License

MIT
