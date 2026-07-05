//! L5 token counter (cl100k_base, exact).
//!
//! Replaces the `chars / 4` heuristic used by `compress.rs` and `context.rs`
//! for budget math. Same input → same token count (per the BPE encoding's
//! deterministic behavior), and crucially, the call to `count_tokens` does
//! not consume time-varying state — so the llama.cpp KV-cache prefix match
//! stays intact when the assembler reuses preambles.
//!
//! Lazy-init: the cl100k_base BPE table is built on first use. Backed by a
//! `OnceLock` so concurrent requests share it.

use std::sync::OnceLock;
use tiktoken_rs::{cl100k_base, CoreBPE};

static TOKENIZER: OnceLock<CoreBPE> = OnceLock::new();

fn tokenizer() -> &'static CoreBPE {
    TOKENIZER.get_or_init(|| {
        // `cl100k_base` matches GPT-4/4o's tokenizer; this is also what
        // tiktoken-rs uses as the canonical gpt-4 reference.
        cl100k_base().expect("cl100k_base BPE table must load (built-in to tiktoken-rs)")
    })
}

/// Exact cl100k_base token count for `text`. UTF-8 safe; safe on empty strings.
pub fn count_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    // `tiktoken_rs::CoreBPE::encode_with_special_tokens` returns the token Vec.
    tokenizer().encode_with_special_tokens(text).len()
}

/// Convert a token budget to a char-coarse upper bound for cheap pre-checks.
/// NOT a precise bound — use `count_tokens` whenever the result determines
/// whether to truncate. Returns `budget * 4` as a deliberately conservative
/// "definitely-enough-chars" estimate.
#[inline]
pub fn token_to_chars_upper(budget: usize) -> usize {
    budget.saturating_mul(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_is_zero_tokens() {
        assert_eq!(count_tokens(""), 0);
    }

    #[test]
    fn ascii_string_matches_known_cl100k_tokens() {
        // "hello world" is 2 cl100k tokens (well-known).
        assert_eq!(count_tokens("hello world"), 2);
    }

    #[test]
    fn code_text_counts_realistic_token_count() {
        let code = r#"fn add(a: i32, b: i32) -> i32 { a + b }"#;
        let n = count_tokens(code);
        // Real cl100k count for this snippet is ~17; assert a sensible range
        // (>= 12, <= 30) so the test survives minor tiktoken-rs updates.
        assert!(n >= 12 && n <= 30, "got {n} tokens for code snippet");
    }

    #[test]
    fn unicode_runs_are_correctly_tokenized() {
        // Chinese text: roughly 1 token per ~1.5 chars (vs the old 4:1
        // heuristic, which wildly under-counts).
        let zh = "今天我们在这里讨论Rust编程";
        let n = count_tokens(zh);
        assert!(n >= 5, "Chinese run under-counted: {n}");
    }

    #[test]
    fn staticity_same_input_same_output() {
        // Two consecutive calls must produce identical token counts — this is
        // a hard requirement for KV-cache prefix match stability.
        let a = count_tokens("fn process_numbers(n: i32) -> Vec<String>");
        let b = count_tokens("fn process_numbers(n: i32) -> Vec<String>");
        assert_eq!(a, b);
    }
}
