use serde_json::{json, Value};

use crate::memory::Memory;
use crate::security::Constitutions;

const MAX_TOKENS: usize = 12_000;
const CHARS_PER_TOKEN: usize = 4;
const MAX_HISTORY_TURNS: usize = 10;

pub fn assemble(
    constitutions: &Constitutions,
    memories: &[Memory],
    system_prompt: &str,
    messages: &[Value],
) -> Vec<Value> {
    let mut system_content = format!(
        "{}\n\n{}\n\n{}\n\n{}",
        constitutions.soul, constitutions.agent, constitutions.owner, system_prompt
    );

    // Fold retrieved memories into the single leading system message. The chat
    // template requires exactly one system message at the start; emitting each
    // memory as its own system message triggers a 400 from llama-server
    // ("System message must be at the beginning") once memory is populated.
    if !memories.is_empty() {
        system_content.push_str("\n\n## Context from previous sessions");
        for mem in memories {
            system_content.push_str("\n\n");
            system_content.push_str(&mem.content);
        }
    }

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

    #[test]
    fn assembles_system_message_from_constitutions() {
        let c = make_constitutions();
        let msgs = vec![json!({"role": "user", "content": "hello"})];
        let result = assemble(&c, &[], "SYS", &msgs);
        assert_eq!(result[0]["role"], "system");
        let content = result[0]["content"].as_str().unwrap();
        assert!(content.contains("SOUL"));
        assert!(content.contains("AGENT"));
        assert!(content.contains("SYS"));
    }

    #[test]
    fn sliding_window_keeps_last_10_turns() {
        let c = make_constitutions();
        let msgs: Vec<Value> = (0..15)
            .map(|i| json!({"role": "user", "content": format!("msg {i}")}))
            .collect();
        let result = assemble(&c, &[], "SYS", &msgs);
        // system msg + 10 user msgs
        assert_eq!(result.len(), 11);
        assert_eq!(result.last().unwrap()["content"], "msg 14");
    }

    #[test]
    fn memory_folded_into_single_system_message() {
        let c = make_constitutions();
        let mems = vec![Memory {
            content: "prev session".into(),
            score: 0.9,
            source: "qdrant".into(),
        }];
        let msgs = vec![json!({"role": "user", "content": "hi"})];
        let result = assemble(&c, &mems, "SYS", &msgs);
        // Exactly one system message (template requirement); memory lives inside it.
        let system_msgs = result.iter().filter(|m| m["role"] == "system").count();
        assert_eq!(system_msgs, 1);
        assert!(result[0]["content"]
            .as_str()
            .unwrap()
            .contains("prev session"));
    }
}
