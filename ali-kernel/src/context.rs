//! Context Assembly — Cortex: adaptive context builder
//!
//! Nature psychology: The cerebral cortex processes sensory information and builds
//! context. The context assembly engine is the cortex of the agent — it takes
//! raw input (memories, intent, identity, constitutions) and assembles the
//! complete mental context for the model.
//!
//! This is the adaptive context engine. It dynamically pulls from:
//! - Constitutions (soul, agent, owner) — the agent's core identity
//! - Intent state (limbic) — what goal we're pursuing
//! - Identity (prefrontal) — who the user is and what they prefer
//! - Journal entries (hippocampus) — what happened in recent sessions
//! - Memory (procedural) — old interactions relevant to this query
//! - Conversation history — the current session's turns
//!
//! All of these are merged into a single system message with token budget awareness.

use serde_json::{json, Value};
use std::sync::Arc;

use crate::identity;
use crate::intent;
use crate::memory::Memory;
use crate::security::Constitutions;
use crate::session::ToolMode;

const MAX_TOKENS: usize = 12_000;
const CHARS_PER_TOKEN: usize = 4;
const MAX_HISTORY_TURNS: usize = 10;

/// Sentinel markers that delimit the tool-listing paragraphs in the SOUL/AGENT
/// constitutions. In passthrough mode these blocks are spliced out so the model
/// is not pre-informed about gateway-native tool names that the caller's
/// `tools[]` array does not advertise.
const ALI_TOOLS_START: &str = "<!--ALI-TOOLS-START-->";
const ALI_TOOLS_END: &str = "<!--ALI-TOOLS-END-->";

/// Splice out `<!--ALI-TOOLS-START-->...<!--ALI-TOOLS-END-->` blocks per `mode`.
///
/// - `Passthrough`: drop the blocks entirely; append a neutral note that
///   client-supplied tools are authoritative.
/// - `Hybrid`: keep the blocks but append a one-line note that user-supplied
///   tools take precedence on name/schema conflict.
/// - `Native`: identity is unchanged.
pub fn apply_tool_mode_filter(system_content: &str, mode: ToolMode) -> String {
    let block_pattern = |s: &str| -> String {
        let s_start = s.find(ALI_TOOLS_START);
        let s_end = s.find(ALI_TOOLS_END);
        match (s_start, s_end) {
            (Some(a), Some(b)) if b > a => {
                let mut out = String::with_capacity(s.len());
                out.push_str(&s[..a]);
                // collapse the blank lines the markers introduce
                let after = &s[b + ALI_TOOLS_END.len()..];
                let trimmed_after = after.trim_start_matches('\n');
                out.push_str(trimmed_after);
                out
            }
            _ => s.to_string(),
        }
    };

    match mode {
        ToolMode::Passthrough => {
            let mut next = block_pattern(system_content);
            // multiple files may each contain a block; iterate
            while next.contains(ALI_TOOLS_START) {
                let prev = next.clone();
                next = block_pattern(&next);
                if next == prev { break; }
            }
            if next != system_content {
                next.push_str(
                    "\n\n## Tool Mode Note\n\n\
                     The gateway's native tools are suppressed for this session. \
                     Use only the tools listed in the request's `tools[]` array \
                     — those are authoritative. There are no other capabilities.",
                );
            }
            next
        }
        ToolMode::Hybrid => {
            let stripped = block_pattern(system_content);
            if stripped != system_content {
                let mut next = system_content.to_string();
                next.push_str(
                    "\n\n## Tool Mode Note\n\n\
                     User-supplied tools (in the request's `tools[]` array) take \
                     precedence on name or schema conflict. The gateway's \
                     native tools remain available as a fallback.",
                );
                next
            } else {
                system_content.to_string()
            }
        }
        ToolMode::Native => system_content.to_string(),
    }
}

/// Assemble the full context for a model inference call.
/// This is the adaptive cortex — it pulls from all nervous system layers.
pub async fn assemble(
    constitutions: &Constitutions,
    memories: &[Memory],
    system_prompt: &str,
    messages: &[Value],
    intent_store: Arc<intent::Intent>,
    identity_store: Arc<identity::IdentityStore>,
    tool_mode: ToolMode,
) -> Vec<Value> {
    // Phase 1: Build the system content from constitutions
    let raw_system_content = format!(
        "{}\n\n{}\n\n{}\n\n{}",
        constitutions.soul, constitutions.agent, constitutions.owner, system_prompt
    );
    let mut system_content = apply_tool_mode_filter(&raw_system_content, tool_mode);

    // Phase 2: Inject intent context (limbic — what we're working toward)
    if let Some(ctx) = intent_store.get_context().await {
        system_content.push_str("\n\n## Current Goal");
        system_content.push_str(&format!("\nIntent state: {}", ctx.state));
        system_content.push_str(&format!("\nGoal: {}", ctx.goal));
        if ctx.attempt_count > 0 {
            system_content.push_str(&format!(
                "\nAttempts so far: {} (blocked: {})",
                ctx.attempt_count,
                ctx.last_blocked_by.as_deref().unwrap_or("none")
            ));
        }
    }

    // Phase 3: Inject identity context (prefrontal — who you are)
    let identity_context = identity_store.inject_context().await;
    if !identity_context.is_empty() {
        system_content.push_str("\n\n## About You");
        system_content.push_str(&identity_context);
    }

    // Phase 4: Fold retrieved memories (episodic — what happened before)
    if !memories.is_empty() {
        system_content.push_str("\n\n## Relevant Past Sessions");
        for mem in memories {
            system_content.push_str("\n\n---\n");
            system_content.push_str(&mem.content);
        }
    }

    // Phase 5: Build the message array with token budget enforcement
    let mut result: Vec<Value> = vec![json!({
        "role": "system",
        "content": system_content
    })];

    // Keep only last N non-system turns
    let user_turns: Vec<&Value> = messages
        .iter()
        .filter(|m| m.get("role").and_then(|r| r.as_str()) != Some("system"))
        .collect();

    let windowed: Vec<&Value> = if user_turns.len() > MAX_HISTORY_TURNS {
        user_turns[user_turns.len() - MAX_HISTORY_TURNS..].to_vec()
    } else {
        user_turns
    };

    // Token budget enforcement (~4 chars/token)
    let system_chars: usize = result
        .iter()
        .map(|m| {
            m.get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .len()
        })
        .sum();

    let budget_chars = MAX_TOKENS * CHARS_PER_TOKEN;
    let mut used = system_chars;

    let mut trimmed: Vec<Value> = Vec::new();
    for msg in windowed.iter().rev() {
        let msg_chars = msg
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .len();
        if used + msg_chars > budget_chars {
            break;
        }
        used += msg_chars;
        trimmed.push((*msg).clone());
    }
    trimmed.reverse();
    result.extend(trimmed);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_constitutions() -> Constitutions {
        Constitutions {
            soul: "SOUL".into(),
            agent: "AGENT".into(),
            owner: "OWNER".into(),
        }
    }

    fn temp_intent() -> Arc<intent::Intent> {
        let cfg = crate::config::Config {
            ali_dir: std::env::temp_dir().join("ali_ctx_intent_test"),
            ..Default::default()
        };
        Arc::new(intent::Intent::new(&cfg).unwrap())
    }

    fn temp_identity() -> Arc<identity::IdentityStore> {
        let cfg = crate::config::Config {
            ali_dir: std::env::temp_dir().join("ali_ctx_identity_test"),
            ..Default::default()
        };
        Arc::new(identity::IdentityStore::new(&cfg).unwrap())
    }

    #[tokio::test]
    async fn assembles_system_message_from_constitutions() {
        let c = make_constitutions();
        let msgs = vec![json!({"role": "user", "content": "hello"})];
        let result = assemble(&c, &[], "SYS", &msgs, temp_intent(), temp_identity(), ToolMode::Native).await;
        assert_eq!(result[0]["role"], "system");
        let content = result[0]["content"].as_str().unwrap();
        assert!(content.contains("SOUL"));
        assert!(content.contains("AGENT"));
        assert!(content.contains("SYS"));
    }

    #[tokio::test]
    async fn sliding_window_keeps_last_10_turns() {
        let c = make_constitutions();
        let msgs: Vec<Value> = (0..15)
            .map(|i| json!({"role": "user", "content": format!("msg {i}")}))
            .collect();
        let result = assemble(&c, &[], "SYS", &msgs, temp_intent(), temp_identity(), ToolMode::Native).await;
        // system msg + 10 user msgs
        assert_eq!(result.len(), 11);
        assert_eq!(result.last().unwrap()["content"], "msg 14");
    }

    #[tokio::test]
    async fn memory_folded_into_single_system_message() {
        let c = make_constitutions();
        let mems = vec![Memory {
            content: "prev session".into(),
            score: 0.9,
            source: "qdrant".into(),
        }];
        let msgs = vec![json!({"role": "user", "content": "hi"})];
        let result = assemble(&c, &mems, "SYS", &msgs, temp_intent(), temp_identity(), ToolMode::Native).await;
        let system_msgs = result.iter().filter(|m| m["role"] == "system").count();
        assert_eq!(system_msgs, 1);
        assert!(result[0]["content"]
            .as_str()
            .unwrap()
            .contains("prev session"));
    }

    #[tokio::test]
    async fn injects_intent_into_context() {
        let i = temp_intent();
        i.classify("I want to add JWT auth").await;
        let c = make_constitutions();
        let msgs = vec![json!({"role": "user", "content": "add auth"})];
        let result = assemble(&c, &[], "SYS", &msgs, i, temp_identity(), ToolMode::Native).await;
        let content = result[0]["content"].as_str().unwrap();
        assert!(content.contains("Current Goal"));
        assert!(content.contains("JWT auth") || content.contains("add auth"));
    }

    #[test]
    fn apply_tool_mode_passthrough_splices_tool_blocks() {
        let text = format!(
            "Before.\n{start}\nread_file(path)\nbash_exec(command)\n{end}\nAfter.\n",
            start = ALI_TOOLS_START,
            end = ALI_TOOLS_END,
        );
        let out = apply_tool_mode_filter(&text, ToolMode::Passthrough);
        assert!(!out.contains("read_file"));
        assert!(!out.contains("bash_exec"));
        assert!(!out.contains(ALI_TOOLS_START));
        assert!(!out.contains(ALI_TOOLS_END));
        assert!(out.contains("Before"));
        assert!(out.contains("After"));
        assert!(out.contains("Tool Mode Note"));
    }

    #[test]
    fn apply_tool_mode_native_keeps_tool_blocks() {
        let text = format!(
            "Before.\n{start}\nread_file(path)\n{end}\nAfter.\n",
            start = ALI_TOOLS_START,
            end = ALI_TOOLS_END,
        );
        let out = apply_tool_mode_filter(&text, ToolMode::Native);
        assert!(out.contains("read_file"));
        assert!(!out.contains("Tool Mode Note"));
    }

    #[test]
    fn apply_tool_mode_hybrid_keeps_blocks_with_precedence_note() {
        let text = format!(
            "Before.\n{start}\nread_file(path)\n{end}\nAfter.\n",
            start = ALI_TOOLS_START,
            end = ALI_TOOLS_END,
        );
        let out = apply_tool_mode_filter(&text, ToolMode::Hybrid);
        assert!(out.contains("read_file"));
        assert!(out.contains("Tool Mode Note"));
        assert!(out.contains("precedence"));
    }

    #[test]
    fn apply_tool_mode_passthrough_handles_multiple_blocks() {
        let text = format!(
            "A.\n{start}\nread_file(path)\n{end}\nB.\n{start}\nbash_exec(x)\n{end}\nC.\n",
            start = ALI_TOOLS_START,
            end = ALI_TOOLS_END,
        );
        let out = apply_tool_mode_filter(&text, ToolMode::Passthrough);
        assert!(!out.contains("read_file"));
        assert!(!out.contains("bash_exec"));
        assert!(!out.contains(ALI_TOOLS_START));
        assert!(out.contains("A"));
        assert!(out.contains("B"));
        assert!(out.contains("C"));
    }
}
