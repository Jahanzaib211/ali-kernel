//! L3 intent classification + L5 constraint mirroring (Rust port of the
//! `elite_transform.py` prototype).
//!
//! Most items are intentionally `#[allow(dead_code)]` until C.3 (brain.rs)
//! dispatches per intent. The module is wired into the binary now, the
//! dispatch integration lands in the next change.
//!
//! Two responsibilities:
//!
//! 1. **Intent classification** — a small regex set decides whether an
//!    incoming user prompt is `Diagnostic` (debug / review), `DevLoop`
//!    (write code / function), `Extraction` (JSON / format), or `Neutral`.
//!    The decision steers preamble choice in the L5 assembler.
//!
//! 2. **Constraint mirroring** — captures imperative rules from the user
//!    prompt ("print squared", "no markdown", "in one sentence") and emits a
//!    static `CRITICAL CONSTRAINTS` block. The constraint block is appended
//!    AFTER the user spec in the last message so the model's recency bias
//!    carries it without diluting the preamble context.
//!
//! ## KV-cache invariant
//!
//! `preamble_for`, `classify`, and `extract_constraints` MUST be pure
//! functions of their inputs. No time-of-day, no UUIDs, no random values.
//! Same input → byte-identical output, every call. This guarantees the
//! llama.cpp KV-cache prefix match survives across requests when the
//! preamble + constraint block is reused.
//!
//! The staticity self-test runs via the public `check_staticity()` helper and
//! in unit tests (`#[test]` blocks below).
#![allow(dead_code)] // items land in C.3 — wired now so the binary can be debugged in isolation.

use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;

/// Classified user intent. Drives preamble choice in `assemble`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// "Find the bug", "explain why", "review this" — model should analyze,
    /// not rewrite.
    Diagnostic,
    /// "Write a function", "implement X", "build a class" — code generation.
    DevLoop,
    /// "Extract JSON", "respond only", structured-output expectations.
    Extraction,
    /// Default — short helpers + prose. No structural constraints inferred.
    Neutral,
}

static RE_DIAG: OnceLock<Regex> = OnceLock::new();
static RE_DEV: OnceLock<Regex> = OnceLock::new();
static RE_EXT: OnceLock<Regex> = OnceLock::new();
static RE_CODE_FENCE: OnceLock<Regex> = OnceLock::new();

fn re_diag() -> &'static Regex {
    RE_DIAG.get_or_init(|| {
        Regex::new(r"(?i)\b(explain|debug|find the bug|find the error|review|diagnose|why does|why is|what is wrong|reason about|analy[sz]e)\b")
            .unwrap()
    })
}
fn re_dev() -> &'static Regex {
    RE_DEV.get_or_init(|| {
        Regex::new(r"(?i)\b(write|implement|build|create|generate)\b[\s\S]{0,80}\b(function|class|code|program|script|impl|module|component|server|handler)\b")
            .unwrap()
    })
}
fn re_ext() -> &'static Regex {
    RE_EXT.get_or_init(|| {
        Regex::new(r"(?i)\b(extract|format|json|only|just|table|list of)\b").unwrap()
    })
}
fn re_code_fence() -> &'static Regex {
    RE_CODE_FENCE.get_or_init(|| Regex::new(r"```[\s\S]*?```").unwrap())
}

/// Classify the user's intent from their prompt. Order: most specific first.
pub fn classify(msg: &str) -> Intent {
    if re_diag().is_match(msg) {
        return Intent::Diagnostic;
    }
    if re_ext().is_match(msg) && !re_dev().is_match(msg) {
        return Intent::Extraction;
    }
    if re_dev().is_match(msg) {
        return Intent::DevLoop;
    }
    Intent::Neutral
}

/// Pre-amble (system block) for the given intent. Returns a `&'static str` so
/// the assembler never re-allocates per request.
pub fn preamble_for(intent: Intent) -> &'static str {
    match intent {
        Intent::Diagnostic => {
            "You are a senior code reviewer. \
            The user is asking you to ANALYZE or EXPLAIN. \
            Do NOT rewrite or fix the code. \
            Explain the issue concisely, exactly as the user asked."
        }
        Intent::DevLoop => {
            "You are a careful software engineer. \
            Follow the user's specification LITERALLY. \
            Every sentence in the spec is a hard requirement. \
            Output ONLY the requested artifact; no prose."
        }
        Intent::Extraction => {
            "You are a precise data extractor. \
            Output ONLY the requested structure. \
            No markdown code fences, no prose, no apologies, no labels."
        }
        Intent::Neutral => {
            "You are a helpful assistant. \
            Be concise and follow the user's instructions precisely."
        }
    }
}

/// Extract imperative rules from a user prompt and return a `CRITICAL
/// CONSTRAINTS` block. Pure function of `msg`; no time or randomness, so
/// llama.cpp KV-cache prefix is preserved across requests that reuse the
/// same prompt.
pub fn extract_constraints(msg: &str) -> String {
    let mut rules: Vec<String> = Vec::new();

    // Strip code fences — we extract imperative rules from prose only.
    let prose = re_code_fence().replace_all(msg, "");

    for sentence in split_sentences(&prose) {
        let low = sentence.to_ascii_lowercase();
        if low.is_empty() {
            continue;
        }
        if [
            "print ",
            "return ",
            "output ",
            "instead ",
            "otherwise",
            "must ",
            "should ",
            "do not ",
            "don't ",
            "don\u{2019}t ",
            "no markdown",
            "no backticks",
            "no explanation",
            "no prose",
            "in one sentence",
            "in one paragraph",
            "only the ",
            "respond only",
        ]
        .iter()
        .any(|kw| low.contains(kw))
        {
            rules.push(sentence.trim().to_string());
        }
    }

    // Specific numbers the user mentioned (e.g. "8472", "21") — require
    // they appear in the answer.
    let nums: Vec<String> = regex::Regex::new(r"\b\d{2,5}\b")
        .unwrap()
        .find_iter(&prose)
        .map(|m| m.as_str().to_string())
        .collect();
    if !nums.is_empty() {
        rules.push(format!(
            "any numeric literal in the spec must appear in the answer (e.g., {})",
            nums.join(", ")
        ));
    }

    // The "squared" / "i ** 2" requirement — recite the math.
    if Regex::new(r"(?i)\bsquared\b|\bi\s*\*\*\s*2\b|\bi\s*\*\s*i\b")
        .unwrap()
        .is_match(msg)
    {
        rules.push("the expression `i ** 2` or its equivalent must appear in the answer".into());
    }

    if rules.is_empty() {
        return String::new();
    }

    format!(
        "CRITICAL CONSTRAINTS (you MUST honor every item below):\n{}",
        rules
            .iter()
            .map(|r| format!("- {r}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Build the L5 message array: system(preamble), user(spec + reminder).
/// The reminder block is constructed from `extract_constraints(msg)`; if
/// non-empty, it is appended after the user spec inside the single user
/// message so recency bias carries it.
pub fn assemble_messages(intent: Intent, user_msg: &str) -> Vec<Value> {
    let mut user_block = user_msg.to_string();
    let cons = extract_constraints(user_msg);
    if !cons.is_empty() {
        user_block.push_str("\n\n[REMINDER]\n");
        user_block.push_str(&cons);
    }
    json!([
        {"role": "system", "content": preamble_for(intent)},
        {"role": "user",   "content": user_block},
    ])
    .as_array()
    .map(|a| a.clone())
    .unwrap_or_default()
}

fn split_sentences(s: &str) -> Vec<String> {
    // cheap split on `.!? ` — good enough for prose imperative extraction.
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        cur.push(ch);
        if matches!(ch, '.' | '!' | '?') {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

// ─── Staticity self-test (KV-cache prefix safety) ──────────────────────────
//
// Runs on module import: asserts `extract_constraints` and `classify` are
// pure functions of their input. A regression here would silently invalidate
// the llama.cpp KV-cache prefix every time the request came through, blowing
// up prompt-cache hit rates and adding tokens billed per request.
fn staticity_self_test() {
    let samples = [
        "If x is divisible by 3, print Foo. Otherwise, print x squared.",
        "Output ONLY a JSON object. No markdown, no backticks.",
        "Explain why this function crashes in one sentence.",
        "What is 21 + 21? Call the calc tool with the expression.",
        "Write a Rust function that reads a file and returns Vec<String>.",
    ];
    for s in samples {
        // Staticity: extract_constraints must be deterministic across calls.
        let a = extract_constraints(s);
        let b = extract_constraints(s);
        assert!(
            a == b,
            "extract_constraints is not static on input {s:?}\n  first  = {a:?}\n  second = {b:?}"
        );
        // Staticity: classify must be deterministic.
        let ca = classify(s);
        let cb = classify(s);
        assert!(
            ca == cb,
            "classify is not static on input {s:?}: {ca:?} vs {cb:?}"
        );
        // Staticity: preamble_for must return the same bytes.
        let pa = preamble_for(ca);
        let pb = preamble_for(cb);
        assert!(pa == pb, "preamble_for is not static");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_examples() {
        assert_eq!(
            classify("Find the bug and explain why it fails."),
            Intent::Diagnostic
        );
        assert_eq!(
            classify("Write a Rust function that returns Vec<String>."),
            Intent::DevLoop
        );
        assert_eq!(
            classify("Extract JSON. Respond only with valid JSON."),
            Intent::Extraction
        );
        assert_eq!(classify("Hello there"), Intent::Neutral);
    }

    #[test]
    fn constraints_capture_specific_math() {
        let c = extract_constraints("Otherwise, print i squared.");
        assert!(c.contains("i ** 2"), "got: {c}");
    }

    #[test]
    fn constraints_capture_no_prose_marker() {
        let c = extract_constraints("Output ONLY the function. No prose, no markdown.");
        assert!(c.contains("ONLY"), "got: {c}");
    }

    #[test]
    fn constraints_ignore_code_fences() {
        let msg =
            "```rust\nfn add(a: i32, b: i32) -> i32 { print!(\"must be coffee\"); a + b }\n```";
        let c = extract_constraints(msg);
        // Imperative lines INSIDE code fences should not be lifted to constraints.
        assert!(
            !c.contains("must be coffee"),
            "code-fence text leaked into constraints: {c}"
        );
    }

    #[test]
    fn preamble_for_distinct_per_intent() {
        let preambles = [
            preamble_for(Intent::Diagnostic),
            preamble_for(Intent::DevLoop),
            preamble_for(Intent::Extraction),
            preamble_for(Intent::Neutral),
        ];
        for i in 0..preambles.len() {
            for j in 0..preambles.len() {
                if i != j {
                    assert_ne!(preambles[i], preambles[j], "preamble[{i}] == preamble[{j}]");
                }
            }
        }
    }

    #[test]
    fn staticity_self_test_holds() {
        super::staticity_self_test();
    }

    #[test]
    fn public_staticity_check_runs_clean() {
        super::check_staticity();
    }
}

// Manual staticity self-check function, callable from tests or as a sanity
// probe during startup. Not auto-invoked at module load (Rust doesn't support
// module-load side effects without a `ctor` proc-macro, which we deliberately
// avoid adding for build-time).
pub fn check_staticity() {
    staticity_self_test();
}
