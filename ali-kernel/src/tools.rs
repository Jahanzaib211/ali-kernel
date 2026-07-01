use anyhow::Result;
use regex::Regex;
use serde_json::{json, Value};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::time::timeout;

use crate::config::Config;

const TOOL_TIMEOUT_SECS: u64 = 30;

/// Shield: some models emit tool calls as Qwen-style XML in `content` instead of
/// OpenAI `tool_calls`. Detect that and rewrite the response into a proper
/// `tool_calls` array so the rest of the pipeline (and any harness) sees valid
/// structured calls. Returns `true` if a normalization happened.
pub fn normalize_tool_calls(response: &mut Value) -> bool {
    let message = match response
        .get_mut("choices")
        .and_then(|c| c.get_mut(0))
        .and_then(|c| c.get_mut("message"))
    {
        Some(m) => m,
        None => return false,
    };

    // Already has structured tool_calls → nothing to do.
    let has_calls = message
        .get("tool_calls")
        .and_then(|t| t.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if has_calls {
        return false;
    }

    let content = match message.get("content").and_then(|c| c.as_str()) {
        Some(c) if c.contains("<tool_call>") || c.contains("<function=") => c.to_string(),
        _ => return false,
    };

    let calls = parse_xml_tool_calls(&content);
    if calls.is_empty() {
        return false;
    }

    // Strip the XML blocks out of content; null it if nothing else remains.
    let cleaned = function_block_re()
        .replace_all(&tool_call_re().replace_all(&content, ""), "")
        .trim()
        .to_string();
    message["content"] = if cleaned.is_empty() {
        Value::Null
    } else {
        Value::String(cleaned)
    };
    message["tool_calls"] = json!(calls);
    true
}

fn tool_call_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)<tool_call>.*?</tool_call>").unwrap())
}

fn function_block_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)<function=[^>]+>.*?</function>").unwrap())
}

/// Parse `<function=NAME><parameter=KEY>VALUE</parameter>...</function>` blocks
/// into OpenAI tool_call objects (arguments is a JSON-encoded string).
fn parse_xml_tool_calls(content: &str) -> Vec<Value> {
    static FN_RE: OnceLock<Regex> = OnceLock::new();
    static PARAM_RE: OnceLock<Regex> = OnceLock::new();
    let fn_re =
        FN_RE.get_or_init(|| Regex::new(r"(?s)<function=([^>\s]+)>(.*?)</function>").unwrap());
    let param_re =
        PARAM_RE.get_or_init(|| Regex::new(r"(?s)<parameter=([^>\s]+)>(.*?)</parameter>").unwrap());

    let mut calls = Vec::new();
    for (i, cap) in fn_re.captures_iter(content).enumerate() {
        let name = cap[1].trim();
        let mut args = serde_json::Map::new();
        for p in param_re.captures_iter(&cap[2]) {
            args.insert(
                p[1].trim().to_string(),
                Value::String(p[2].trim().to_string()),
            );
        }
        calls.push(json!({
            "id": format!("call_{i}"),
            "type": "function",
            "function": {
                "name": name,
                "arguments": Value::Object(args).to_string(),
            }
        }));
    }
    calls
}

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

        let result = dispatch_single_tool(name, &args, config).await;
        let raw = match result {
            Ok(output) => output,
            Err(e) => format!("Tool error: {e}"),
        };

        // L5.5: structurally compress the observation before it re-enters context.
        let content =
            crate::compress::compress_observation(&raw, crate::compress::DEFAULT_OBS_BUDGET);
        if content.len() < raw.len() {
            tracing::info!("PIPE L5.5 {name} {}→{} chars", raw.len(), content.len());
        }

        results.push(json!({
            "tool_call_id": id,
            "role": "tool",
            "name": name,
            "content": content
        }));

        tracing::info!(
            "PIPE L7 tool_executed name={} chars={}",
            name,
            content.len()
        );
    }

    // Inject tool results into response for caller to handle agentic loop
    let mut enriched = response.clone();
    enriched["_tool_results"] = json!(results);
    enriched
}

pub async fn dispatch_single_tool(name: &str, args: &Value, config: &Config) -> Result<String> {
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
            let raw = arg_path(args).ok_or_else(|| anyhow::anyhow!("missing path"))?;
            let path = config.resolve_tool_path(raw)?;
            Ok(std::fs::read_to_string(&path).unwrap_or_else(|e| format!("read error: {e}")))
        }
        "write_file" => {
            let raw = arg_path(args).ok_or_else(|| anyhow::anyhow!("missing path"))?;
            let path = config.resolve_tool_path(raw)?;
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing content"))?;
            std::fs::write(&path, content)?;
            Ok(format!(
                "written {} bytes to {}",
                content.len(),
                path.display()
            ))
        }
        // Crate-aware Rust engineering: let the model explore the real ecosystem.
        "crate_search" => {
            let q = args
                .get("query")
                .or_else(|| args.get("q"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing query"))?;
            crates_io_search(q).await
        }
        "docs_lookup" | "crate_info" => {
            let name = args
                .get("crate")
                .or_else(|| args.get("name"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing crate name"))?;
            crate_info(name).await
        }
        // Iterative dev loop: scaffold, edit, add crates, build/test/lint.
        "cargo_new" => {
            dev_guard(config)?;
            crate::devloop::cargo_new(&config.workspace_dir, str_arg(args, "project")?).await
        }
        "write_project_file" => {
            dev_guard(config)?;
            let path = arg_path(args).ok_or_else(|| anyhow::anyhow!("missing path"))?;
            crate::devloop::write_project_file(
                &config.workspace_dir,
                str_arg(args, "project")?,
                path,
                str_arg(args, "content")?,
            )
            .await
        }
        "cargo_add" => {
            dev_guard(config)?;
            let krate = args
                .get("crate")
                .or_else(|| args.get("name"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing crate"))?;
            crate::devloop::cargo_add(&config.workspace_dir, str_arg(args, "project")?, krate).await
        }
        "cargo_check" | "cargo_build" | "cargo_test" | "cargo_clippy" => {
            dev_guard(config)?;
            let sub = name.trim_start_matches("cargo_");
            crate::devloop::cargo_run(&config.workspace_dir, str_arg(args, "project")?, sub).await
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

/// Accept common path-arg aliases the model emits (`path`, `filepath`, `file`).
fn arg_path(args: &Value) -> Option<&str> {
    args.get("path")
        .or_else(|| args.get("filepath"))
        .or_else(|| args.get("file"))
        .and_then(|v| v.as_str())
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing {key}"))
}

fn dev_guard(config: &Config) -> Result<()> {
    if !config.dev_loop_enabled {
        anyhow::bail!("dev-loop tools disabled (ORNITH_DEV_LOOP=0)");
    }
    Ok(())
}

fn network_tools_enabled() -> bool {
    std::env::var("ORNITH_NETWORK_TOOLS")
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// Operator opt-in to run bash_exec without bwrap isolation (default: deny).
fn unsandboxed_allowed() -> bool {
    std::env::var("ORNITH_ALLOW_UNSANDBOXED")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Read-only query against the public crates.io registry. No data leaves beyond the query.
async fn crates_io_search(query: &str) -> Result<String> {
    if !network_tools_enabled() {
        return Ok("crate_search disabled (ORNITH_NETWORK_TOOLS=0)".into());
    }
    let url = format!(
        "https://crates.io/api/v1/crates?q={}&per_page=5",
        urlencode(query)
    );
    let v: Value = crates_io_get(&url).await?;
    let mut out = format!("crates.io results for '{query}':\n");
    if let Some(arr) = v.get("crates").and_then(|c| c.as_array()) {
        for c in arr {
            let name = c.get("name").and_then(|x| x.as_str()).unwrap_or("?");
            let ver = c
                .get("max_stable_version")
                .or_else(|| c.get("max_version"))
                .and_then(|x| x.as_str())
                .unwrap_or("?");
            let desc = c
                .get("description")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim();
            out.push_str(&format!(
                "- {name} {ver} — {desc}\n  docs: https://docs.rs/{name}\n"
            ));
        }
    }
    Ok(out)
}

async fn crate_info(name: &str) -> Result<String> {
    if !network_tools_enabled() {
        return Ok("docs_lookup disabled (ORNITH_NETWORK_TOOLS=0)".into());
    }
    let url = format!("https://crates.io/api/v1/crates/{}", urlencode(name));
    let v: Value = crates_io_get(&url).await?;
    let c = v.get("crate").cloned().unwrap_or(json!({}));
    let ver = c
        .get("max_stable_version")
        .or_else(|| c.get("max_version"))
        .and_then(|x| x.as_str())
        .unwrap_or("?");
    let desc = c.get("description").and_then(|x| x.as_str()).unwrap_or("");
    let repo = c.get("repository").and_then(|x| x.as_str()).unwrap_or("");
    Ok(format!(
        "{name} {ver}\n{desc}\ndocs: https://docs.rs/{name}\nrepo: {repo}\nadd: cargo add {name}"
    ))
}

async fn crates_io_get(url: &str) -> Result<Value> {
    let resp = reqwest::Client::new()
        .get(url)
        // crates.io requires a descriptive UA or returns 403.
        .header("User-Agent", "ornith-gateway (crate-aware tool)")
        .timeout(Duration::from_secs(15))
        .send()
        .await?;
    Ok(resp.json::<Value>().await?)
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

async fn sandbox_exec(cmd: &str) -> Result<String> {
    // Use bubblewrap if available, fall back to restricted shell
    let bwrap = std::path::Path::new("/usr/bin/bwrap").exists()
        || std::path::Path::new("/usr/local/bin/bwrap").exists();

    // Fail closed: without bwrap isolation, refuse to execute unless the operator
    // has explicitly accepted a less-isolated fallback via ORNITH_ALLOW_UNSANDBOXED=1.
    if !bwrap && !unsandboxed_allowed() {
        anyhow::bail!(
            "refusing bash_exec: bubblewrap (bwrap) not found. Install bwrap, or set \
             ORNITH_ALLOW_UNSANDBOXED=1 to accept a less-isolated fallback."
        );
    }

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
        tracing::warn!(
            "bwrap missing — running bash_exec WITHOUT sandbox isolation (ORNITH_ALLOW_UNSANDBOXED=1)"
        );
        // Degraded fallback (opt-in only): restricted bash + time limit, NO fs/net isolation.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn xml_response() -> Value {
        json!({"choices":[{"message":{"role":"assistant","content":
            "<tool_call>\n<function=read_file>\n<parameter=filepath>\n/etc/hostname\n</parameter>\n</function>\n</tool_call>"}}]})
    }

    #[test]
    fn normalizes_xml_into_tool_calls() {
        let mut r = xml_response();
        assert!(normalize_tool_calls(&mut r));
        let calls = r["choices"][0]["message"]["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["function"]["name"], "read_file");
        let args: Value =
            serde_json::from_str(calls[0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["filepath"], "/etc/hostname");
        // XML stripped out of content
        assert!(r["choices"][0]["message"]["content"].is_null());
    }

    #[test]
    fn leaves_structured_calls_untouched() {
        let mut r = json!({"choices":[{"message":{"tool_calls":[{"id":"call_0",
            "function":{"name":"x","arguments":"{}"}}]}}]});
        assert!(!normalize_tool_calls(&mut r));
    }

    #[test]
    fn ignores_plain_text() {
        let mut r = json!({"choices":[{"message":{"content":"just a normal answer"}}]});
        assert!(!normalize_tool_calls(&mut r));
    }

    #[test]
    fn filepath_alias_resolves_to_path() {
        let args = json!({"filepath": "/tmp/x"});
        assert_eq!(arg_path(&args), Some("/tmp/x"));
    }
}
