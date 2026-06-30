//! L5.5 structural context compression (native, pure-Rust, deterministic).
//!
//! Shrinks verbose observations — tool outputs, cargo errors, file reads, crate
//! docs — before they re-enter Ornith's small (~12K) window. v1 is lossless-ish
//! structural compression (no model, no new deps): strip ANSI, collapse blank
//! runs, trim trailing whitespace, and smart-truncate keeping head + tail (errors
//! and results usually live at the end). Semantic compression is a later layer.

use regex::Regex;
use std::sync::OnceLock;

/// Default char budget for a single observation (~2K tokens at ~4 chars/token).
pub const DEFAULT_OBS_BUDGET: usize = 8000;

/// Compress one observation string to roughly `budget` chars.
pub fn compress_observation(s: &str, budget: usize) -> String {
    let stripped = strip_ansi(s);
    let collapsed = collapse_blank_lines(&stripped);
    smart_truncate(&collapsed, budget)
}

fn strip_ansi(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").unwrap())
        .replace_all(s, "")
        .into_owned()
}

/// Collapse runs of >1 blank line into a single blank line; trim trailing spaces.
fn collapse_blank_lines(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let collapsed = RE
        .get_or_init(|| Regex::new(r"\n([ \t]*\n){2,}").unwrap())
        .replace_all(s, "\n\n");
    collapsed
        .lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Keep head (2/3) + tail (1/3) when over budget — the payload (an error, a test
/// result, a final value) is usually at the end.
fn smart_truncate(s: &str, budget: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= budget {
        return s.to_string();
    }
    let head = budget * 2 / 3;
    let tail = budget.saturating_sub(head);
    let omitted = chars.len() - head - tail;
    let h: String = chars[..head].iter().collect();
    let t: String = chars[chars.len() - tail..].iter().collect();
    format!("{h}\n…[{omitted} chars compressed by L5.5]…\n{t}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_escapes() {
        let s = "\x1b[31merror\x1b[0m: boom";
        assert_eq!(strip_ansi(s), "error: boom");
    }

    #[test]
    fn collapses_blank_runs() {
        let s = "a\n\n\n\nb   \nc";
        assert_eq!(collapse_blank_lines(s), "a\n\nb\nc");
    }

    #[test]
    fn truncate_keeps_head_and_tail() {
        let s: String = (0..1000)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let out = compress_observation(&s, 100);
        assert!(out.contains("compressed by L5.5"));
        assert!(out.len() < s.len());
        // tail preserved (where errors live)
        assert!(out.ends_with(&s[s.len() - 10..]));
    }

    #[test]
    fn under_budget_is_untouched() {
        assert_eq!(compress_observation("short output", 8000), "short output");
    }
}
