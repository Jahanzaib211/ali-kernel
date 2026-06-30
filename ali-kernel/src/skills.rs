use anyhow::Result;
use reqwest::Client;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

use crate::brain::{self, BrainResponse};
use crate::config::Config;
use crate::security::Constitutions;

const SKILLS_DIR: &str = "ornith-generated";

pub async fn synthesize_skill(
    client: &Client,
    config: &Config,
    constitutions: &Constitutions,
    task_description: &str,
    task_response: &str,
) -> Result<String> {
    let prompt = format!(
        r#"You solved this task:
TASK: {task_description}

SOLUTION SUMMARY: {task_response}

Now write a reusable Claude Code skill (SKILL.md) so this type of task can be automated in future sessions.

Format:
---
name: <kebab-case-name>
description: <one line — what this skill does and when to invoke it>
---

# <Skill Title>

## When to use
<1-2 sentences>

## Approach
<numbered steps Claude should follow>

## Key patterns
<bullet points of gotchas, constraints, or reusable code snippets>

Output the complete SKILL.md content only."#
    );

    let messages = vec![
        json!({
            "role": "system",
            "content": format!("{}\n\n{}", constitutions.soul, constitutions.agent)
        }),
        json!({ "role": "user", "content": prompt }),
    ];

    let result = brain::infer(
        client, config, messages, 0.3, 1024, false, json!({}),
    ).await?;

    let content = match result {
        BrainResponse::Complete(v) => brain::extract_content(&v)
            .unwrap_or("# Skill generation failed")
            .to_string(),
        BrainResponse::Streaming(_) => return Err(anyhow::anyhow!("unexpected stream")),
    };

    save_skill(&content, task_description)?;
    Ok(content)
}

fn save_skill(content: &str, task_description: &str) -> Result<()> {
    let skills_base = Path::new("skills").join(SKILLS_DIR);
    fs::create_dir_all(&skills_base)?;

    // Extract name from frontmatter if present
    let skill_name = content
        .lines()
        .find(|l| l.starts_with("name:"))
        .and_then(|l| l.strip_prefix("name:"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| slugify(task_description));

    let skill_dir = skills_base.join(&skill_name);
    fs::create_dir_all(&skill_dir)?;
    fs::write(skill_dir.join("SKILL.md"), content)?;

    tracing::info!("Skill synthesized: skills/{SKILLS_DIR}/{skill_name}/SKILL.md");
    Ok(())
}

fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(40)
        .collect()
}
