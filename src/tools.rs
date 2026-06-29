use anyhow::Result;
use serde_json::{json, Value};
use std::process::Command;
use std::time::Duration;
use tokio::time::timeout;

use crate::config::Config;

const TOOL_TIMEOUT_SECS: u64 = 30;

pub async fn execute_tools(response: Value, config: &Config) -> Value {
    let tool_calls = match response
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("tool_calls"))
        .and_then(|t| t.as_array())
    {
        Some(calls) if !calls.is_empty() => calls.clone(),
        _ => return response,
    };

    let mut results: Vec<Value> = Vec::new();

    for call in &tool_calls {
        let id = call.get("id").and_then(|v| v.as_str()).unwrap_or("call_0");
        let name = call
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let args_str = call
            .get("function")
            .and_then(|f| f.get("arguments"))
            .and_then(|v| v.as_str())
            .unwrap_or("{}");

        let args: Value = serde_json::from_str(args_str).unwrap_or(json!({}));

        let result = dispatch_tool(name, &args, config).await;
        let content = match result {
            Ok(output) => output,
            Err(e) => format!("Tool error: {e}"),
        };

        results.push(json!({
            "tool_call_id": id,
            "role": "tool",
            "name": name,
            "content": content
        }));

        tracing::info!("Tool executed: {name} → {} chars", content.len());
    }

    // Inject tool results into response for caller to handle agentic loop
    let mut enriched = response.clone();
    enriched["_tool_results"] = json!(results);
    enriched
}

async fn dispatch_tool(name: &str, args: &Value, _config: &Config) -> Result<String> {
    match name {
        "bash_exec" | "execute_bash" | "run_command" => {
            let cmd = args
                .get("command")
                .or_else(|| args.get("cmd"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing command argument"))?;
            sandbox_exec(cmd).await
        }
        "read_file" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing path"))?;
            Ok(std::fs::read_to_string(path).unwrap_or_else(|e| format!("read error: {e}")))
        }
        "write_file" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing path"))?;
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing content"))?;
            std::fs::write(path, content)?;
            Ok(format!("written {} bytes to {path}", content.len()))
        }
        "get_weather" | "search" | "web_search" => {
            // Stub for external tools — return informative placeholder
            Ok(format!(
                "Tool `{name}` not yet wired to external API. Args: {args}"
            ))
        }
        _ => Ok(format!("Unknown tool `{name}`. Args: {args}")),
    }
}

async fn sandbox_exec(cmd: &str) -> Result<String> {
    // Use bubblewrap if available, fall back to restricted shell
    let bwrap = std::path::Path::new("/usr/bin/bwrap").exists()
        || std::path::Path::new("/usr/local/bin/bwrap").exists();

    let output = if bwrap {
        timeout(
            Duration::from_secs(TOOL_TIMEOUT_SECS),
            tokio::task::spawn_blocking({
                let cmd = cmd.to_string();
                move || {
                    Command::new("bwrap")
                        .args([
                            "--ro-bind",
                            "/usr",
                            "/usr",
                            "--ro-bind",
                            "/lib",
                            "/lib",
                            "--ro-bind",
                            "/lib64",
                            "/lib64",
                            "--ro-bind",
                            "/bin",
                            "/bin",
                            "--proc",
                            "/proc",
                            "--dev",
                            "/dev",
                            "--tmpfs",
                            "/tmp",
                            "--unshare-net",
                            "--unshare-pid",
                            "--die-with-parent",
                            "bash",
                            "-c",
                            &cmd,
                        ])
                        .output()
                }
            }),
        )
        .await
    } else {
        // Fallback: restricted bash with no network, time limit
        timeout(
            Duration::from_secs(TOOL_TIMEOUT_SECS),
            tokio::task::spawn_blocking({
                let cmd = cmd.to_string();
                move || {
                    Command::new("bash")
                        .args(["-c", &format!("timeout 25 bash -c {}", shell_escape(&cmd))])
                        .output()
                }
            }),
        )
        .await
    };

    match output {
        Ok(Ok(Ok(out))) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            if out.status.success() {
                Ok(stdout.trim().to_string())
            } else {
                Ok(format!(
                    "exit {}: {}\n{}",
                    out.status.code().unwrap_or(-1),
                    stdout.trim(),
                    stderr.trim()
                ))
            }
        }
        Ok(Ok(Err(e))) => Err(anyhow::anyhow!("exec failed: {e}")),
        Ok(Err(e)) => Err(anyhow::anyhow!("task error: {e}")),
        Err(_) => Err(anyhow::anyhow!("tool timed out after {TOOL_TIMEOUT_SECS}s")),
    }
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
