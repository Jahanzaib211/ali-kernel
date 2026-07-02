//! Usage ledger — the gateway's source of truth for savings claims.
//!
//! The compression log line only measures request chars in/out. Anthropic
//! bills tokens (with cache reads at ~10% of base input price), so dollar
//! claims must come from the `usage` block Anthropic returns. This module
//! records one JSONL line per proxied request combining both views:
//! compression chars (request side) and billed tokens (response side).
//! `GET /stats` aggregates the ledger into spend + estimated savings.

use axum::{extract::State, Json};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use tracing::warn;

use crate::AppState;

/// Non-SSE bodies are buffered for usage extraction up to this cap; a
/// /v1/messages JSON response beyond this is abnormal and just skipped.
const JSON_BODY_CAP: usize = 4 * 1024 * 1024;

/// Rough chars-per-token divisor used only for the *estimated* savings figure.
const CHARS_PER_TOKEN: u64 = 4;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub ts: DateTime<Utc>,
    pub model: String,
    /// "subscription" (OAuth bearer — Claude Code on Pro/Max, draws plan
    /// limits) or "api_key" (x-api-key — pay-per-token). Old records without
    /// the field deserialize to "" and are counted as subscription, since all
    /// pre-field traffic was Claude Code OAuth.
    #[serde(default)]
    pub auth: String,
    pub chars_raw: u64,
    pub chars_compressed: u64,
    pub input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub output_tokens: u64,
}

pub struct UsageLedger {
    path: PathBuf,
    write_lock: Mutex<()>,
    /// Month ("%Y-%m") of the records currently in the active file; when a
    /// record from a newer month arrives the file rotates to
    /// usage-<month>.jsonl so the active file stays small forever.
    cur_month: Mutex<Option<String>>,
    /// /stats result cache — the Claude Code statusline polls on every
    /// render, so re-reading and re-pricing the whole JSONL each time is
    /// wasted work. 2s staleness is invisible to every consumer.
    stats_cache: Mutex<Option<(std::time::Instant, Value)>>,
}

const STATS_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(2);

impl UsageLedger {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            write_lock: Mutex::new(()),
            cur_month: Mutex::new(None),
            stats_cache: Mutex::new(None),
        }
    }

    pub fn append(&self, rec: &UsageRecord) {
        let line = match serde_json::to_string(rec) {
            Ok(l) => l,
            Err(e) => {
                warn!("usage ledger serialize failed: {e}");
                return;
            }
        };
        let _guard = self.write_lock.lock().unwrap_or_else(|p| p.into_inner());
        self.rotate_if_new_month(&rec.ts);
        let res = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = res {
            warn!("usage ledger write failed ({}): {e}", self.path.display());
        }
    }

    /// Rotate the active file to usage-<YYYY-MM>.jsonl when the incoming
    /// record's month is newer than the file's. Caller holds write_lock.
    fn rotate_if_new_month(&self, ts: &DateTime<Utc>) {
        let month = ts.format("%Y-%m").to_string();
        let mut cm = self.cur_month.lock().unwrap_or_else(|p| p.into_inner());
        if cm.is_none() {
            // First append since startup: derive the file's month from its
            // first record so restarts don't skip rotation.
            *cm = std::fs::read_to_string(&self.path)
                .ok()
                .and_then(|c| {
                    c.lines()
                        .find_map(|l| serde_json::from_str::<UsageRecord>(l).ok())
                })
                .map(|r| r.ts.format("%Y-%m").to_string())
                .or_else(|| Some(month.clone()));
        }
        if let Some(m) = cm.as_ref() {
            if *m != month {
                let rotated = self.path.with_file_name(format!("usage-{m}.jsonl"));
                if let Err(e) = std::fs::rename(&self.path, &rotated) {
                    warn!("usage ledger rotation failed: {e}");
                }
            }
        }
        *cm = Some(month);
    }

    /// Read every ledger file (active + rotated) in the log directory.
    fn read_all(&self) -> Vec<UsageRecord> {
        let mut records = Vec::new();
        let dir = self.path.parent().unwrap_or(std::path::Path::new("."));
        let Ok(entries) = std::fs::read_dir(dir) else {
            return records;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !(name.starts_with("usage") && name.ends_with(".jsonl")) {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                records.extend(content.lines().filter_map(|l| serde_json::from_str(l).ok()));
            }
        }
        records
    }

    pub fn stats(&self) -> Value {
        {
            let cache = self.stats_cache.lock().unwrap_or_else(|p| p.into_inner());
            if let Some((at, v)) = cache.as_ref() {
                if at.elapsed() < STATS_CACHE_TTL {
                    return v.clone();
                }
            }
        }
        let computed = self.compute_stats();
        *self.stats_cache.lock().unwrap_or_else(|p| p.into_inner()) =
            Some((std::time::Instant::now(), computed.clone()));
        computed
    }

    fn compute_stats(&self) -> Value {
        let records = self.read_all();
        let now = Utc::now();
        let today: Vec<_> = records
            .iter()
            .filter(|r| r.ts.date_naive() == now.date_naive())
            .collect();
        let week: Vec<_> = records
            .iter()
            .filter(|r| now - r.ts <= Duration::days(7))
            .collect();
        let all: Vec<_> = records.iter().collect();
        serde_json::json!({
            "today": aggregate(&today),
            "last_7_days": aggregate(&week),
            "all_time": aggregate(&all),
            "note": "api_value_usd prices billed tokens at Anthropic list rates. \
                     For subscription (OAuth) traffic it is plan-limit value \
                     consumed, NOT dollars billed — the plan is a flat fee. \
                     For api_key traffic it approximates the actual bill. \
                     estimated_savings_usd assumes ~4 chars/token at each \
                     request's observed cache mix — an estimate.",
        })
    }
}

/// $/MTok (input, output), Anthropic list prices as of July 2026.
/// Cache write = 1.25x input, cache read = 0.1x input.
fn rates_for(model: &str) -> (f64, f64) {
    let m = model.to_lowercase();
    if m.contains("fable") || m.contains("mythos") {
        (10.0, 50.0)
    } else if m.contains("opus") {
        (5.0, 25.0)
    } else if m.contains("haiku") {
        (1.0, 5.0)
    } else {
        // sonnet and unknown models
        (3.0, 15.0)
    }
}

/// Actual dollars billed for this request, from the returned usage block.
fn actual_cost_usd(r: &UsageRecord) -> f64 {
    let (input, output) = rates_for(&r.model);
    (r.input_tokens as f64 * input
        + r.cache_read_input_tokens as f64 * input * 0.1
        + r.cache_creation_input_tokens as f64 * input * 1.25
        + r.output_tokens as f64 * output)
        / 1e6
}

/// Estimated dollars the compression pass avoided: chars removed converted to
/// tokens at ~4 chars/token, priced at this request's observed blend of
/// uncached / cache-read / cache-write input rates.
fn estimated_savings_usd(r: &UsageRecord) -> f64 {
    let removed_tokens = r.chars_raw.saturating_sub(r.chars_compressed) / CHARS_PER_TOKEN;
    if removed_tokens == 0 {
        return 0.0;
    }
    let (input, _) = rates_for(&r.model);
    let billed = r.input_tokens + r.cache_read_input_tokens + r.cache_creation_input_tokens;
    let effective_rate = if billed == 0 {
        input
    } else {
        (r.input_tokens as f64 * input
            + r.cache_read_input_tokens as f64 * input * 0.1
            + r.cache_creation_input_tokens as f64 * input * 1.25)
            / billed as f64
    };
    removed_tokens as f64 * effective_rate / 1e6
}

fn aggregate(records: &[&UsageRecord]) -> Value {
    let sum = |f: fn(&UsageRecord) -> u64| records.iter().map(|r| f(r)).sum::<u64>();
    let chars_raw = sum(|r| r.chars_raw);
    let chars_compressed = sum(|r| r.chars_compressed);
    let compression_pct = if chars_raw > 0 {
        (chars_raw - chars_compressed) as f64 * 100.0 / chars_raw as f64
    } else {
        0.0
    };
    let spend: f64 = records.iter().map(|r| actual_cost_usd(r)).sum();
    let savings: f64 = records.iter().map(|r| estimated_savings_usd(r)).sum();
    // "" = pre-auth-field records; all such traffic was Claude Code OAuth.
    let sub_value: f64 = records
        .iter()
        .filter(|r| r.auth != "api_key")
        .map(|r| actual_cost_usd(r))
        .sum();
    let key_value = spend - sub_value;
    serde_json::json!({
        "requests": records.len(),
        "input_tokens": sum(|r| r.input_tokens),
        "cache_read_input_tokens": sum(|r| r.cache_read_input_tokens),
        "cache_creation_input_tokens": sum(|r| r.cache_creation_input_tokens),
        "output_tokens": sum(|r| r.output_tokens),
        "chars_raw": chars_raw,
        "chars_compressed": chars_compressed,
        "compression_pct": (compression_pct * 10.0).round() / 10.0,
        "api_value_usd": (spend * 10000.0).round() / 10000.0,
        "subscription_value_usd": (sub_value * 10000.0).round() / 10000.0,
        "api_key_value_usd": (key_value * 10000.0).round() / 10000.0,
        "estimated_savings_usd": (savings * 10000.0).round() / 10000.0,
    })
}

pub async fn stats_handler(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let stats = state.ledger.stats();
    if q.get("format").map(|f| f == "prometheus").unwrap_or(false) {
        return (
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; version=0.0.4",
            )],
            prometheus_text(&stats),
        )
            .into_response();
    }
    Json(stats).into_response()
}

/// Prometheus exposition of the all-time counters — one scrape target away
/// from a Grafana dashboard.
fn prometheus_text(stats: &Value) -> String {
    let a = &stats["all_time"];
    let g = |k: &str| a[k].as_f64().unwrap_or(0.0);
    [
        ("ali_requests_total", g("requests")),
        ("ali_input_tokens_total", g("input_tokens")),
        ("ali_cache_read_tokens_total", g("cache_read_input_tokens")),
        (
            "ali_cache_creation_tokens_total",
            g("cache_creation_input_tokens"),
        ),
        ("ali_output_tokens_total", g("output_tokens")),
        ("ali_chars_raw_total", g("chars_raw")),
        ("ali_chars_compressed_total", g("chars_compressed")),
        ("ali_api_value_usd_total", g("api_value_usd")),
        (
            "ali_subscription_value_usd_total",
            g("subscription_value_usd"),
        ),
        ("ali_api_key_value_usd_total", g("api_key_value_usd")),
        (
            "ali_estimated_savings_usd_total",
            g("estimated_savings_usd"),
        ),
    ]
    .iter()
    .map(|(k, v)| format!("{k} {v}\n"))
    .collect()
}

/// Incremental extractor for model + usage from a response body as it streams
/// through the proxy. SSE responses carry usage in `message_start` (input and
/// cache token counts) and the final `message_delta` (output tokens); plain
/// JSON responses carry a top-level `usage` block.
pub struct UsageScanner {
    sse: bool,
    pending: Vec<u8>,
    pub model: Option<String>,
    pub usage: UsageTokens,
    found: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UsageTokens {
    pub input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub output_tokens: u64,
}

impl UsageScanner {
    pub fn new(sse: bool) -> Self {
        Self {
            sse,
            pending: Vec::new(),
            model: None,
            usage: UsageTokens::default(),
            found: false,
        }
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        if self.sse {
            self.pending.extend_from_slice(chunk);
            // Process complete lines; a line is only parseable once its
            // newline has arrived, so split boundaries are safe.
            while let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.pending.drain(..=pos).collect();
                self.process_sse_line(&line[..line.len() - 1]);
            }
            // A well-formed SSE line is small; a newline-free run beyond the
            // cap means garbage upstream — drop it rather than grow unbounded.
            if self.pending.len() > JSON_BODY_CAP {
                self.pending.clear();
            }
        } else if self.pending.len() < JSON_BODY_CAP {
            self.pending.extend_from_slice(chunk);
        }
    }

    pub fn finish(&mut self) {
        if self.sse {
            let rest = std::mem::take(&mut self.pending);
            self.process_sse_line(&rest);
        } else {
            let body = std::mem::take(&mut self.pending);
            if let Ok(v) = serde_json::from_slice::<Value>(&body) {
                if let Some(m) = v.get("model").and_then(|m| m.as_str()) {
                    self.model = Some(m.to_string());
                }
                if let Some(u) = v.get("usage") {
                    self.merge_usage(u);
                }
            }
        }
    }

    pub fn found_usage(&self) -> bool {
        self.found
    }

    fn process_sse_line(&mut self, line: &[u8]) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(payload) = line.strip_prefix(b"data:") else {
            return;
        };
        let Ok(v) = serde_json::from_slice::<Value>(payload) else {
            return;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("message_start") => {
                let msg = &v["message"];
                if let Some(m) = msg.get("model").and_then(|m| m.as_str()) {
                    self.model = Some(m.to_string());
                }
                if let Some(u) = msg.get("usage") {
                    self.merge_usage(u);
                }
            }
            Some("message_delta") => {
                if let Some(u) = v.get("usage") {
                    self.merge_usage(u);
                }
            }
            _ => {}
        }
    }

    fn merge_usage(&mut self, u: &Value) {
        let mut set = |key: &str, slot: fn(&mut UsageTokens) -> &mut u64| {
            if let Some(n) = u.get(key).and_then(|n| n.as_u64()) {
                *slot(&mut self.usage) = n;
                self.found = true;
            }
        };
        set("input_tokens", |t| &mut t.input_tokens);
        set("cache_creation_input_tokens", |t| {
            &mut t.cache_creation_input_tokens
        });
        set("cache_read_input_tokens", |t| {
            &mut t.cache_read_input_tokens
        });
        set("output_tokens", |t| &mut t.output_tokens);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(scanner: &mut UsageScanner, s: &str) {
        // Feed in tiny chunks to exercise line-boundary handling
        for chunk in s.as_bytes().chunks(7) {
            scanner.feed(chunk);
        }
        scanner.finish();
    }

    #[test]
    fn sse_usage_from_message_start_and_delta() {
        let mut sc = UsageScanner::new(true);
        let stream = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-sonnet-4-5\",\
             \"usage\":{\"input_tokens\":1200,\"cache_creation_input_tokens\":300,\
             \"cache_read_input_tokens\":50000,\"output_tokens\":2}}}\n",
            "\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hi\"}}\n",
            "\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":840}}\n",
            "\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n",
        );
        feed_all(&mut sc, stream);
        assert!(sc.found_usage());
        assert_eq!(sc.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(
            sc.usage,
            UsageTokens {
                input_tokens: 1200,
                cache_creation_input_tokens: 300,
                cache_read_input_tokens: 50000,
                output_tokens: 840,
            }
        );
    }

    #[test]
    fn json_body_usage() {
        let mut sc = UsageScanner::new(false);
        feed_all(
            &mut sc,
            "{\"model\":\"claude-opus-4-6\",\"content\":[],\
             \"usage\":{\"input_tokens\":10,\"output_tokens\":20}}",
        );
        assert!(sc.found_usage());
        assert_eq!(sc.model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(sc.usage.input_tokens, 10);
        assert_eq!(sc.usage.output_tokens, 20);
    }

    #[test]
    fn no_usage_found_on_error_body() {
        let mut sc = UsageScanner::new(false);
        feed_all(&mut sc, "{\"error\":{\"type\":\"overloaded_error\"}}");
        assert!(!sc.found_usage());
    }

    #[test]
    fn pricing_uses_cache_rates() {
        let rec = UsageRecord {
            ts: Utc::now(),
            model: "claude-sonnet-4-5".into(),
            auth: String::new(),
            chars_raw: 0,
            chars_compressed: 0,
            input_tokens: 1_000_000,
            cache_creation_input_tokens: 1_000_000,
            cache_read_input_tokens: 1_000_000,
            output_tokens: 1_000_000,
        };
        // 3.00 uncached + 3.75 cache write + 0.30 cache read + 15.00 output
        let cost = actual_cost_usd(&rec);
        assert!((cost - 22.05).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn fable_priced_at_ten_fifty() {
        let rec = UsageRecord {
            ts: Utc::now(),
            model: "claude-fable-5".into(),
            auth: String::new(),
            chars_raw: 0,
            chars_compressed: 0,
            input_tokens: 1_000_000,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 1_000_000,
            output_tokens: 1_000_000,
        };
        // 10.00 uncached + 1.00 cache read + 50.00 output
        let cost = actual_cost_usd(&rec);
        assert!((cost - 61.0).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn savings_priced_at_observed_cache_mix() {
        // All input billed as cache reads → effective rate is 0.1x input
        let rec = UsageRecord {
            ts: Utc::now(),
            model: "claude-sonnet-4-5".into(),
            auth: String::new(),
            chars_raw: 4_000_000,
            chars_compressed: 0,
            input_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 500_000,
            output_tokens: 0,
        };
        // 1M tokens removed × $0.30/MTok
        let savings = estimated_savings_usd(&rec);
        assert!((savings - 0.30).abs() < 1e-9, "got {savings}");
    }
}
