//! Iterative develop → build → test loop tools.
//!
//! Gives the model a confined Rust workspace where it can scaffold a project,
//! write files, add crates, and run `cargo {check,build,test,clippy}` — reading
//! real compiler/test output back so it can fix and iterate. All paths are
//! confined to the workspace root (no escapes). Unlike `bash_exec`, cargo needs
//! network to fetch crates, so these run outside the network-isolated sandbox;
//! they are gated by `ORNITH_DEV_LOOP` and the workspace confinement.

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

const CARGO_TIMEOUT_SECS: u64 = 240;
const MAX_OUTPUT: usize = 6000;

/// Resolve `<workspace>/<project>` after validating the project name (no escapes).
pub fn project_dir(workspace: &Path, project: &str) -> Result<PathBuf> {
    if project.is_empty()
        || !project
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(anyhow!("invalid project name '{project}'"));
    }
    Ok(workspace.join(project))
}

/// Join a relative path under `base`, rejecting absolute paths and `..` traversal.
fn safe_join(base: &Path, rel: &str) -> Result<PathBuf> {
    if rel.starts_with('/') || rel.split(['/', '\\']).any(|c| c == "..") {
        return Err(anyhow!("unsafe path '{rel}'"));
    }
    Ok(base.join(rel))
}

pub async fn cargo_new(workspace: &Path, project: &str) -> Result<String> {
    let dir = project_dir(workspace, project)?;
    tokio::fs::create_dir_all(workspace).await.ok();
    if dir.exists() {
        return Ok(format!("project '{project}' already exists"));
    }
    run(
        "cargo",
        &["new", "--bin", &dir.to_string_lossy()],
        workspace,
    )
    .await
}

pub async fn write_project_file(
    workspace: &Path,
    project: &str,
    rel: &str,
    content: &str,
) -> Result<String> {
    let dir = project_dir(workspace, project)?;
    let path = safe_join(&dir, rel)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, content).await?;
    Ok(format!("wrote {} bytes to {project}/{rel}", content.len()))
}

pub async fn cargo_add(workspace: &Path, project: &str, krate: &str) -> Result<String> {
    if !valid_crate_spec(krate) {
        return Err(anyhow!("invalid crate spec '{krate}'"));
    }
    let dir = project_dir(workspace, project)?;
    let manifest = manifest(&dir);
    // `--` ends option parsing so a name like `-foo` can never be read as a flag.
    run(
        "cargo",
        &["add", "--manifest-path", &manifest, "--", krate],
        &dir,
    )
    .await
}

/// Accept only `name` or `name@version` from the crate grammar — no leading `-`,
/// no `=`/`:`/`/`/whitespace — to block argv flag smuggling into `cargo add`.
fn valid_crate_spec(krate: &str) -> bool {
    if krate.is_empty() || krate.starts_with('-') {
        return false;
    }
    let (name, version) = match krate.split_once('@') {
        Some((n, v)) => (n, Some(v)),
        None => (krate, None),
    };
    let name_ok = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let version_ok = version.is_none_or(|v| {
        !v.is_empty()
            && v.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'))
    });
    name_ok && version_ok
}

/// Run `cargo {sub}` (check|build|test|clippy) and return combined output.
pub async fn cargo_run(workspace: &Path, project: &str, sub: &str) -> Result<String> {
    let dir = project_dir(workspace, project)?;
    if !dir.exists() {
        return Err(anyhow!(
            "project '{project}' does not exist — run cargo_new first"
        ));
    }
    let manifest = manifest(&dir);
    run("cargo", &[sub, "--manifest-path", &manifest], &dir).await
}

fn manifest(dir: &Path) -> String {
    dir.join("Cargo.toml").to_string_lossy().into_owned()
}

async fn run(bin: &str, args: &[&str], cwd: &Path) -> Result<String> {
    let out = timeout(
        Duration::from_secs(CARGO_TIMEOUT_SECS),
        Command::new(bin)
            .args(args)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .map_err(|_| anyhow!("`{bin} {}` timed out", args.join(" ")))??;

    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&out.stdout));
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    let status = if out.status.success() { "OK" } else { "FAILED" };
    Ok(format!(
        "[{status}] cargo {}\n{}",
        args.join(" "),
        truncate(&combined, MAX_OUTPUT)
    ))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}\n…(output truncated)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn rejects_bad_project_names() {
        let ws = Path::new("/tmp/ws");
        assert!(project_dir(ws, "../etc").is_err());
        assert!(project_dir(ws, "a b").is_err());
        assert!(project_dir(ws, "").is_err());
        assert!(project_dir(ws, "my-proj_1").is_ok());
    }

    #[test]
    fn safe_join_blocks_escape() {
        let base = Path::new("/tmp/ws/proj");
        assert!(safe_join(base, "../../etc/passwd").is_err());
        assert!(safe_join(base, "/etc/passwd").is_err());
        assert!(safe_join(base, "src/main.rs").is_ok());
    }

    #[test]
    fn crate_spec_blocks_flag_smuggling() {
        assert!(valid_crate_spec("serde"));
        assert!(valid_crate_spec("tokio@1.40"));
        assert!(valid_crate_spec("async-sqlite@0.6.0"));
        assert!(!valid_crate_spec("--config=evil")); // flag smuggling
        assert!(!valid_crate_spec("-Z"));
        assert!(!valid_crate_spec("serde --features=foo")); // whitespace
        assert!(!valid_crate_spec("../etc/passwd")); // path chars
        assert!(!valid_crate_spec(""));
    }
}
