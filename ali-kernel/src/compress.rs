//! L5.5 structural context compression (native, pure-Rust, deterministic).
//!
//! Shrinks verbose observations — tool outputs, cargo errors, file reads, crate
//! docs — before they re-enter Ornith's small (~12K) window. v1 is lossless-ish
//! structural compression (no model, no new deps): strip ANSI, collapse blank
//! runs, trim trailing whitespace, and smart-truncate keeping head + tail (errors
//! and results usually live at the end).
//!
//! v2 adds two quality guards and a data flywheel:
//! - salient-line rescue: error/warning/fail lines from the elided middle are
//!   kept (capped), so truncation stops eating the exact lines a coding agent
//!   needs most;
//! - min-win guard: if compression would shave <10%, the original passes
//!   through untouched — no marker noise for marginal gains;
//! - observation mining: elided middles are appended (deduped, capped) to the
//!   training corpus so a local-LLM semantic compressor can be trained on
//!   exactly the text we currently drop.
//!
//! DETERMINISM IS A CACHE-SAFETY INVARIANT: the same input and budget must
//! always produce the same output, or Anthropic prompt-cache prefixes break
//! and cache-write costs eat the savings. Nothing here may depend on time,
//! randomness, or position in the conversation.

use regex::Regex;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Default char budget for a single observation (~2K tokens at ~4 chars/token).
pub const DEFAULT_OBS_BUDGET: usize = 8000;

/// Salient lines rescued from the elided middle are capped at this fraction
/// of the budget (output may exceed budget by at most this much).
const SALIENT_FRACTION: usize = 8; // budget / 8

/// Mining caps: per-record middle sample and total corpus file size.
const MINE_RECORD_CAP: usize = 16 * 1024;
const MINE_FILE_CAP: u64 = 200 * 1024 * 1024;

/// Char counts through one observation's pipeline, so the ledger can split
/// savings into provably-lossless vs lossy buckets.
#[derive(Debug, Default, Clone, Copy)]
pub struct SqueezeStats {
    /// Original size of blocks the compressor looked at.
    pub raw: u64,
    /// Size after lossless-only passes (ANSI, blank runs, repeated lines).
    pub lossless: u64,
    /// Size actually forwarded (after truncation, if any).
    pub sent: u64,
}

/// Lossless-only pass: strips ANSI escapes, collapses blank-line runs, and
/// collapses runs of identical lines (annotated with the repeat count so no
/// information is lost). Never truncates. Safe on tool outputs of any size.
pub fn compress_lossless(s: &str) -> String {
    let stripped = strip_ansi(s);
    let collapsed = collapse_blank_lines(&stripped);
    collapse_repeated_lines(&collapsed)
}

/// Compress one observation string to roughly `budget` chars.
pub fn compress_observation(s: &str, budget: usize) -> String {
    compress_observation_with(s, budget, &mut SqueezeStats::default())
}

/// Same as `compress_observation`, accumulating pipeline sizes into `stats`
/// so callers can report lossless vs lossy savings separately.
pub fn compress_observation_with(s: &str, budget: usize, stats: &mut SqueezeStats) -> String {
    let lossless = compress_lossless(s);
    let out = smart_truncate(&lossless, budget);
    // Min-win guard: <10% shrink isn't worth injecting a truncation marker.
    let out = if out.len() * 10 > s.len() * 9 {
        s.to_string()
    } else {
        out
    };
    stats.raw += s.len() as u64;
    if out == s {
        // Guard fired — nothing was applied.
        stats.lossless += s.len() as u64;
    } else {
        stats.lossless += lossless.len().min(s.len()) as u64;
    }
    stats.sent += out.len() as u64;
    out
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

/// Collapse runs of >=3 identical lines into the line plus an explicit repeat
/// annotation — build logs and progress spam collapse dramatically, and the
/// count annotation means zero information is lost.
fn collapse_repeated_lines(s: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut run: Option<(&str, usize)> = None;
    let flush = |out: &mut Vec<String>, line: &str, count: usize| {
        if count >= 3 {
            out.push(line.to_string());
            out.push(format!("…[line repeated {} more times]", count - 1));
        } else {
            for _ in 0..count {
                out.push(line.to_string());
            }
        }
    };
    for line in s.lines() {
        match run {
            Some((prev, count)) if prev == line => run = Some((prev, count + 1)),
            Some((prev, count)) => {
                flush(&mut out, prev, count);
                run = Some((line, 1));
            }
            None => run = Some((line, 1)),
        }
    }
    if let Some((prev, count)) = run {
        flush(&mut out, prev, count);
    }
    out.join("\n")
}

/// Keep head (2/3) + tail (1/3) when over budget — the payload (an error, a
/// test result, a final value) is usually at the end. Salient lines from the
/// elided middle (errors, warnings, failures) are rescued up to budget/8.
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
    let middle: String = chars[head..chars.len() - tail].iter().collect();
    mine_middle(&middle);
    let salient = salient_lines(&middle, budget / SALIENT_FRACTION);
    if salient.is_empty() {
        format!("{h}\n…[{omitted} chars compressed by L5.5]…\n{t}")
    } else {
        format!(
            "{h}\n…[{omitted} chars compressed by L5.5; salient lines kept below]…\n{salient}\n…\n{t}"
        )
    }
}

/// Extract error/warning/failure lines from elided text, deduped, up to `cap`
/// chars. Deterministic: lines keep their original order.
fn salient_lines(middle: &str, cap: usize) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)\b(error|warning|fail(ed|ure)?|panic(ked)?|exception|fatal|denied|unwrap|traceback|assert)\b").unwrap()
    });
    let mut out = String::new();
    let mut seen: Vec<&str> = Vec::new();
    for line in middle.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || !re.is_match(trimmed) || seen.contains(&trimmed) {
            continue;
        }
        if out.len() + trimmed.len() + 1 > cap {
            break;
        }
        seen.push(trimmed);
        out.push_str(trimmed);
        out.push('\n');
    }
    out.trim_end().to_string()
}

// --- observation mining (flywheel for the future semantic compressor) ---

static MINE_PATH: OnceLock<PathBuf> = OnceLock::new();
static MINE_SEEN: OnceLock<Mutex<std::collections::HashSet<u64>>> = OnceLock::new();

/// Enable mining of elided middles into `path` (JSONL). Called once at startup.
pub fn set_mining_path(path: PathBuf) {
    let _ = MINE_PATH.set(path);
}

/// Append an elided middle to the training corpus: deduped by content hash
/// (conversations resend the same blocks every turn), sample capped at 16KB,
/// file capped at 200MB. Best-effort — never fails the request path.
fn mine_middle(middle: &str) {
    let Some(path) = MINE_PATH.get() else {
        return;
    };
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    middle.hash(&mut hasher);
    let key = hasher.finish();
    {
        let seen = MINE_SEEN.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
        let mut seen = seen.lock().unwrap_or_else(|p| p.into_inner());
        if !seen.insert(key) {
            return;
        }
    }
    if std::fs::metadata(path)
        .map(|m| m.len() > MINE_FILE_CAP)
        .unwrap_or(false)
    {
        return;
    }
    let sample: String = middle.chars().take(MINE_RECORD_CAP).collect();
    let rec = serde_json::json!({
        "ts": chrono::Utc::now(),
        "total_chars": middle.chars().count(),
        "sample": sample,
    });
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| writeln!(f, "{rec}"));
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

    #[test]
    fn min_win_guard_passes_marginal_input_through() {
        // 1000 chars against a 950 budget: <10% win — keep the original.
        let s = "x".repeat(1000);
        assert_eq!(compress_observation(&s, 950), s);
    }

    #[test]
    fn salient_error_lines_survive_truncation() {
        let mut s = String::new();
        for i in 0..2000 {
            if i == 1000 {
                s.push_str("error[E0308]: mismatched types in the middle\n");
            } else {
                s.push_str(&format!("line {i} ordinary filler content here\n"));
            }
        }
        let out = compress_observation(&s, 2000);
        assert!(out.contains("salient lines kept"));
        assert!(out.contains("error[E0308]: mismatched types in the middle"));
        // and stays near budget (salient adds at most budget/8 + markers)
        assert!(out.len() < 2000 + 2000 / 8 + 200, "len {}", out.len());
    }

    #[test]
    fn repeated_lines_collapse_with_count() {
        let s = "start\nsame line\nsame line\nsame line\nsame line\nend";
        let out = compress_lossless(s);
        assert!(out.contains("same line\n…[line repeated 3 more times]"));
        assert!(out.starts_with("start"));
        assert!(out.ends_with("end"));
        // pairs are left alone — collapsing needs >=3
        let pair = "a\na\nb";
        assert_eq!(compress_lossless(pair), pair);
    }

    #[test]
    fn squeeze_stats_split_lossless_and_lossy() {
        // 6000 chars of unique lines + heavy ANSI noise, budget 2000:
        // ANSI strip is lossless, truncation is lossy — both must be counted.
        let mut s = String::new();
        for i in 0..300 {
            s.push_str(&format!("\x1b[31mline {i} with some content\x1b[0m\n"));
        }
        let mut st = SqueezeStats::default();
        let out = compress_observation_with(&s, 2000, &mut st);
        assert_eq!(st.raw, s.len() as u64);
        assert_eq!(st.sent, out.len() as u64);
        // lossless stage sits strictly between raw and sent
        assert!(st.lossless < st.raw, "ANSI strip must count as lossless");
        assert!(st.sent < st.lossless, "truncation must count as lossy");
    }

    #[test]
    fn deterministic_output_is_cache_safe() {
        let s: String = (0..50_000)
            .map(|i| {
                if i % 97 == 0 {
                    '\n'
                } else {
                    char::from(b'a' + (i % 26) as u8)
                }
            })
            .collect();
        let a = compress_observation(&s, 4000);
        let b = compress_observation(&s, 4000);
        assert_eq!(a, b, "same input+budget must always compress identically");
    }
}
