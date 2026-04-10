pub mod bash;
pub mod fs;
pub mod search;
pub mod truncate;
pub mod web;

use std::path::Path;

use dcode_providers::ToolDef;
use serde_json::json;

/// All built-in tool definitions (sent to the model).
pub fn builtin_tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "read_file".into(),
            description: "Read a file from disk. Returns up to 1000 lines with line numbers. \
                          Use start_line/end_line to read a specific slice of large files (1-based). \
                          For files >1000 lines always use line ranges to avoid truncation.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path":       {"type":"string","description":"File path to read"},
                    "start_line": {"type":"integer","description":"First line to read (1-based, optional)"},
                    "end_line":   {"type":"integer","description":"Last line to read (1-based, optional)"},
                },
                "required": ["path"],
            }),
        },
        ToolDef {
            name: "write_file".into(),
            description: "Write (overwrite) a file with given content. Creates parent directories.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path":    {"type":"string","description":"File path to write"},
                    "content": {"type":"string","description":"Full file content"},
                },
                "required": ["path","content"],
            }),
        },
        ToolDef {
            name: "edit_file".into(),
            description: "Replace an exact string in a file. old_string must match exactly once. \
                          Prefer this over write_file for targeted edits.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path":       {"type":"string","description":"File path to edit"},
                    "old_string": {"type":"string","description":"Exact text to replace (must be unique in file)"},
                    "new_string": {"type":"string","description":"Replacement text"},
                },
                "required": ["path","old_string","new_string"],
            }),
        },
        ToolDef {
            name: "bash".into(),
            description: "Run a shell command. Timeout defaults to 60s. Returns stdout+stderr. \
                          Avoid interactive commands. Use for builds, tests, git ops, package managers.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command":      {"type":"string","description":"Shell command to execute"},
                    "timeout_secs": {"type":"integer","description":"Timeout in seconds (default 60)"},
                    "working_dir":  {"type":"string","description":"Working directory (default: CWD)"},
                },
                "required": ["command"],
            }),
        },
        ToolDef {
            name: "grep".into(),
            description: "Search file contents with a regex pattern. Returns up to 100 matches with file:line context. \
                          Use file_glob to narrow scope in large projects.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern":          {"type":"string","description":"Regex pattern to search"},
                    "path":             {"type":"string","description":"Directory or file to search in"},
                    "file_glob":        {"type":"string","description":"File filter e.g. '*.rs', '*.ts' (optional)"},
                    "case_insensitive": {"type":"boolean","description":"Case insensitive search"},
                    "context_lines":    {"type":"integer","description":"Lines of context around match (0-3)"},
                },
                "required": ["pattern","path"],
            }),
        },
        ToolDef {
            name: "glob".into(),
            description: "Find files matching a glob pattern. Returns sorted file list.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type":"string","description":"Glob pattern e.g. 'src/**/*.rs'"},
                },
                "required": ["pattern"],
            }),
        },
        ToolDef {
            name: "list_dir".into(),
            description: "List directory tree (skips hidden dirs, target/, node_modules/, .git/).".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path":      {"type":"string","description":"Directory path"},
                    "max_depth": {"type":"integer","description":"Max depth (default 3)"},
                },
                "required": ["path"],
            }),
        },
        ToolDef {
            name: "read_image".into(),
            description: "Read a local image file (jpg, png, gif, webp) and show it to the model. \
                          Use when the user references a local image path or screenshot.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type":"string","description":"Absolute or relative path to the image file"},
                },
                "required": ["path"],
            }),
        },
        ToolDef {
            name: "web_fetch".into(),
            description: "Fetch a web page and return its readable text content. \
                          Converts HTML to plain text. Max 2MB response. \
                          Use for reading documentation, articles, or any HTTP URL.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": {"type":"string","description":"Full URL to fetch (must start with http:// or https://)"},
                },
                "required": ["url"],
            }),
        },
        ToolDef {
            name: "web_search".into(),
            description: "Search the web and return relevant results with content. \
                          Use for finding current information, documentation, or anything not in the codebase.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query":       {"type":"string","description":"Search query"},
                    "num_results": {"type":"integer","description":"Number of results to return (default 6, max 10)"},
                },
                "required": ["query"],
            }),
        },
        ToolDef {
            name: "ask_user".into(),
            description: "Pause and ask the user a question. Use when you need clarification, a choice \
                          between options, or confirmation before a risky action. Always prefer asking \
                          over guessing when user intent is ambiguous.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question to ask the user"
                    },
                    "choices": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional list of choices to present (e.g. [\"yes\",\"no\",\"cancel\"])"
                    },
                },
                "required": ["question"],
            }),
        },
    ]
}

/// Dispatch a tool call by name with its JSON arguments.
/// `cwd` is the agent's working directory, used as the default for bash.
pub async fn dispatch(name: &str, args: &serde_json::Value, cwd: &Path) -> anyhow::Result<String> {
    match name {
        "read_file" => {
            let path = args["path"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("path required"))?
                .to_string();
            let path = resolve_path(&path, cwd);
            let result = fs::read_file(fs::ReadArgs {
                path,
                start_line: args["start_line"].as_u64().map(|n| n as usize),
                end_line: args["end_line"].as_u64().map(|n| n as usize),
            })
            .await?;
            // Large file reads: save to disk and return preview with hint.
            Ok(truncate::maybe_offload(result, name, cwd))
        }
        "write_file" => {
            let path = args["path"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("path required"))?;
            let path = resolve_path(path, cwd);
            let content = args["content"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("content required"))?;
            fs::write_file(&path, content)
        }
        "edit_file" => {
            let path = args["path"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("path required"))?
                .to_string();
            let path = resolve_path(&path, cwd);
            let old_string = args["old_string"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("old_string required"))?
                .to_string();
            let new_string = args["new_string"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("new_string required"))?
                .to_string();
            fs::edit_file(fs::EditArgs {
                path,
                old_string,
                new_string,
            })
        }
        "bash" => {
            let command = args["command"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("command required"))?
                .to_string();
            // Default working_dir to agent cwd if not specified.
            let working_dir = args["working_dir"]
                .as_str()
                .map(str::to_string)
                .or_else(|| Some(cwd.to_string_lossy().to_string()));
            let result = bash::bash_exec(bash::BashArgs {
                command,
                timeout_secs: args["timeout_secs"].as_u64(),
                working_dir,
            })
            .await?;
            Ok(truncate::maybe_offload(result, name, cwd))
        }
        "grep" => {
            let pattern = args["pattern"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("pattern required"))?
                .to_string();
            let path = args["path"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("path required"))?
                .to_string();
            let path = resolve_path(&path, cwd);
            let result = search::grep_files(search::GrepArgs {
                pattern,
                path,
                file_glob: args["file_glob"].as_str().map(str::to_string),
                case_insensitive: args["case_insensitive"].as_bool().unwrap_or(false),
                context_lines: args["context_lines"].as_u64().map(|n| n as usize),
            })?;
            Ok(truncate::maybe_offload(result, name, cwd))
        }
        "glob" => {
            let pattern = args["pattern"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("pattern required"))?;
            fs::list_files(pattern)
        }
        "list_dir" => {
            let path = args["path"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("path required"))?;
            let path = resolve_path(path, cwd);
            search::list_directory(&path, args["max_depth"].as_u64().map(|n| n as usize))
        }
        "read_image" => {
            let path = args["path"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("path required"))?;
            let path = resolve_path(path, cwd);
            web::read_image(&path)
        }
        "web_fetch" => {
            let url = args["url"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("url required"))?;
            let result = web::web_fetch(url).await?;
            Ok(truncate::maybe_offload(result, name, cwd))
        }
        "web_search" => {
            let query = args["query"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("query required"))?;
            let num = args["num_results"].as_u64().unwrap_or(6).min(10) as usize;
            let result = web::web_search(query, num).await?;
            Ok(truncate::maybe_offload(result, name, cwd))
        }
        // ask_user is handled at the agent level before dispatch reaches here.
        "ask_user" => Ok("[ask_user handled by agent]".to_string()),
        other => anyhow::bail!("Unknown tool: {other}"),
    }
}

/// Resolve a path relative to cwd if it's not absolute.
fn resolve_path(path: &str, cwd: &Path) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        path.to_string()
    } else {
        cwd.join(p).to_string_lossy().to_string()
    }
}
