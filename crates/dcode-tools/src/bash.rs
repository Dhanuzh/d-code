/// Shell execution tool with timeout and output truncation.
use std::time::Duration;

use anyhow::Context;

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_OUTPUT_BYTES: usize = 8_192; // 8 KiB per output stream

pub struct BashArgs {
    pub command: String,
    pub timeout_secs: Option<u64>,
    pub working_dir: Option<String>,
}

pub async fn bash_exec(args: BashArgs) -> anyhow::Result<String> {
    let timeout = Duration::from_secs(args.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));

    let mut cmd = tokio::process::Command::new("bash");
    cmd.arg("-c").arg(&args.command);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    if let Some(dir) = &args.working_dir {
        cmd.current_dir(dir);
    }

    let result = tokio::time::timeout(timeout, async {
        let output = cmd.output().await.context("spawn bash")?;
        Ok::<_, anyhow::Error>(output)
    })
    .await;

    match result {
        Err(_) => Ok(format!(
            "[timeout after {}s]\n$ {}",
            timeout.as_secs(),
            args.command
        )),
        Ok(Err(e)) => Err(e),
        Ok(Ok(output)) => {
            let stdout = truncate_output(&output.stdout, MAX_OUTPUT_BYTES);
            let stderr = truncate_output(&output.stderr, MAX_OUTPUT_BYTES);
            let status = output.status.code().unwrap_or(-1);

            let mut parts = vec![];
            if !stdout.is_empty() {
                parts.push(stdout);
            }
            if !stderr.is_empty() {
                parts.push(format!("[stderr]\n{stderr}"));
            }
            if status != 0 {
                parts.push(format!("[exit {status}]"));
            }

            if parts.is_empty() {
                Ok(format!("[exit {status}]"))
            } else {
                Ok(parts.join("\n"))
            }
        }
    }
}

fn truncate_output(bytes: &[u8], max: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    if s.len() <= max {
        s.trim_end().to_string()
    } else {
        let truncated = &s[..max];
        // Try to cut on a newline boundary.
        let cut = truncated.rfind('\n').unwrap_or(max);
        format!(
            "{}\n... [{} bytes truncated]",
            truncated[..cut].trim_end(),
            s.len() - cut
        )
    }
}
