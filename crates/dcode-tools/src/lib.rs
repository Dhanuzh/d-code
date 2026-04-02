pub mod bash;
pub mod fs;
pub mod search;

use dcode_providers::ToolDef;
use serde_json::json;

/// All built-in tool definitions (sent to the model).
pub fn builtin_tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "read_file".into(),
            description: "Read a file from disk. Use start_line/end_line to read a slice (1-based). Returns line numbers prefixed output.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type":"string","description":"File path to read"},
                    "start_line": {"type":"integer","description":"First line to read (1-based, optional)"},
                    "end_line":   {"type":"integer","description":"Last line to read (1-based, optional)"},
                },
                "required": ["path"],
            }),
        },
        ToolDef {
            name: "write_file".into(),
            description: "Write (overwrite) a file with given content. Creates parent directories as needed.".into(),
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
            description: "Replace an exact string in a file. old_string must match exactly once. Prefer this over write_file for targeted edits.".into(),
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
            description: "Run a shell command. Timeout defaults to 30s. Avoid interactive commands.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command":      {"type":"string","description":"Shell command to execute"},
                    "timeout_secs": {"type":"integer","description":"Timeout in seconds (default 30)"},
                    "working_dir":  {"type":"string","description":"Working directory (default: current)"},
                },
                "required": ["command"],
            }),
        },
        ToolDef {
            name: "grep".into(),
            description: "Search file contents with a regex pattern. Returns matches with file:line context.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern":          {"type":"string","description":"Regex pattern to search"},
                    "path":             {"type":"string","description":"Directory or file to search in"},
                    "file_glob":        {"type":"string","description":"File filter e.g. '*.rs' (optional)"},
                    "case_insensitive": {"type":"boolean","description":"Case insensitive search"},
                    "context_lines":    {"type":"integer","description":"Lines of context around match (0-2)"},
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
            description: "List directory tree (skips hidden dirs, target/, node_modules/).".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path":      {"type":"string","description":"Directory path"},
                    "max_depth": {"type":"integer","description":"Max depth (default 3)"},
                },
                "required": ["path"],
            }),
        },
    ]
}

/// Dispatch a tool call by name with its JSON arguments.
pub async fn dispatch(
    name: &str,
    args: &serde_json::Value,
) -> anyhow::Result<String> {
    match name {
        "read_file" => {
            let path = args["path"].as_str().ok_or_else(|| anyhow::anyhow!("path required"))?.to_string();
            fs::read_file(fs::ReadArgs {
                path,
                start_line: args["start_line"].as_u64().map(|n| n as usize),
                end_line:   args["end_line"].as_u64().map(|n| n as usize),
            })
        }
        "write_file" => {
            let path    = args["path"].as_str().ok_or_else(|| anyhow::anyhow!("path required"))?;
            let content = args["content"].as_str().ok_or_else(|| anyhow::anyhow!("content required"))?;
            fs::write_file(path, content)
        }
        "edit_file" => {
            let path       = args["path"].as_str().ok_or_else(|| anyhow::anyhow!("path required"))?.to_string();
            let old_string = args["old_string"].as_str().ok_or_else(|| anyhow::anyhow!("old_string required"))?.to_string();
            let new_string = args["new_string"].as_str().ok_or_else(|| anyhow::anyhow!("new_string required"))?.to_string();
            fs::edit_file(fs::EditArgs { path, old_string, new_string })
        }
        "bash" => {
            let command = args["command"].as_str().ok_or_else(|| anyhow::anyhow!("command required"))?.to_string();
            bash::bash_exec(bash::BashArgs {
                command,
                timeout_secs: args["timeout_secs"].as_u64(),
                working_dir:  args["working_dir"].as_str().map(str::to_string),
            }).await
        }
        "grep" => {
            let pattern = args["pattern"].as_str().ok_or_else(|| anyhow::anyhow!("pattern required"))?.to_string();
            let path    = args["path"].as_str().ok_or_else(|| anyhow::anyhow!("path required"))?.to_string();
            search::grep_files(search::GrepArgs {
                pattern,
                path,
                file_glob:        args["file_glob"].as_str().map(str::to_string),
                case_insensitive: args["case_insensitive"].as_bool().unwrap_or(false),
                context_lines:    args["context_lines"].as_u64().map(|n| n as usize),
            })
        }
        "glob" => {
            let pattern = args["pattern"].as_str().ok_or_else(|| anyhow::anyhow!("pattern required"))?;
            fs::list_files(pattern)
        }
        "list_dir" => {
            let path = args["path"].as_str().ok_or_else(|| anyhow::anyhow!("path required"))?;
            search::list_directory(path, args["max_depth"].as_u64().map(|n| n as usize))
        }
        other => anyhow::bail!("Unknown tool: {other}"),
    }
}
