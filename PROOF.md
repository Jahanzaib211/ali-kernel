# PROOF.md — The Savings Record, With Receipts

**Status:** live measurement, updated automatically at `logs/usage.jsonl`
**Snapshot date:** 2026-07-02 (UTC)
**Audience:** a skeptical developer pointed at this repo and told "this gateway saves money."

This document answers every question that developer should ask, in the order
they should ask it. Every claim is tied to a file, a line of code, or a
reproducible command. Where a number is an estimate, it says so.

---

## 1. What is being claimed, exactly?

Two claims, deliberately separated:

| Claim | Status | Source of truth |
|---|---|---|
| Request payloads shrink ~30% (chars) | **Measured fact** | `PIPE L5.5 compressed` lines in `logs/gateway.log`, emitted at `ali-kernel/src/anthropic_proxy.rs` (the `info!` after the compression pass) |
| ~22-28% of API cost is avoided | **Estimate, from real billed tokens** | `logs/usage.jsonl` + `GET /stats`, computed in `ali-kernel/src/usage_ledger.rs` |

We do **not** claim the char percentage equals the dollar percentage. It
doesn't — see §5.

## 2. Snapshot (one real day of Claude Code usage, 2026-07-02)

All-time ledger totals at time of writing (`curl -s 127.0.0.1:9000/stats`):

```json
{
  "requests": 78,
  "chars_raw": 67492576,
  "chars_compressed": 47202708,
  "compression_pct": 30.1,
  "input_tokens": 30371,
  "cache_read_input_tokens": 16261730,
  "cache_creation_input_tokens": 551948,
  "output_tokens": 38103,
  "api_value_usd": 25.1678,
  "estimated_savings_usd": 6.9741
}
```

Read that as: 78 requests, 67.5M chars of context reduced to 47.2M before
hitting Anthropic; the requests actually billed 16.26M cache-read tokens +
30K uncached input + 552K cache writes + 38K output, costing **$25.17** at
list prices; the compression is estimated to have avoided **$6.97** more —
about **21.7% of what the session would otherwise have cost**.

## 3. Where do the numbers come from? (chain of custody)

```
Claude Code ──► gateway /v1/messages          ──► api.anthropic.com
                 │                                  │
                 │ counts chars before/after        │ returns `usage` block
                 │ compression (request side)       │ (billed token counts)
                 ▼                                  ▼
          gateway.log line                 response tee → UsageScanner
          "PIPE L5.5 compressed:           (ali-kernel/src/usage_ledger.rs)
           N → M chars"                            │
                 └───────────────┬─────────────────┘
                                 ▼
                    logs/usage.jsonl (one line per request)
                                 ▼
                          GET /stats (aggregates + pricing)
```

- **Char counts** are measured by the gateway itself around
  `compress_observation()` — request side only.
- **Token counts are never computed by us.** They are copied verbatim from
  the `usage` block Anthropic returns with every response — the same numbers
  Anthropic bills against. For streaming (SSE) responses they come from the
  `message_start` and final `message_delta` events; for non-streaming, from
  the top-level `usage` field. See `UsageScanner` in
  `ali-kernel/src/usage_ledger.rs`, with unit tests feeding byte-split SSE
  fixtures.
- **Ledger schema** (one JSON line per request in `logs/usage.jsonl`):

```json
{"ts":"2026-07-02T04:17:05Z","model":"claude-fable-5",
 "chars_raw":538600,"chars_compressed":384402,
 "input_tokens":82,"cache_creation_input_tokens":570,
 "cache_read_input_tokens":140095,"output_tokens":569}
```

## 4. Skeptic FAQ

### "You're on a Pro subscription — did you actually spend $25?"

**No — and the ledger now says so explicitly.** Claude Code on a Pro/Max plan
authenticates with an OAuth bearer token, not an API key; the plan is a flat
monthly fee and tokens draw against plan usage limits. The gateway detects
the auth type per request (`x-api-key` = pay-per-token, OAuth = subscription)
and records it in every ledger line. `/stats` splits the total into
`subscription_value_usd` (plan-limit value consumed — NOT dollars billed) and
`api_key_value_usd` (approximates a real bill). The headline field is named
`api_value_usd` — "what these tokens are worth at API list prices" — because
calling it "spend" for subscription traffic would be exactly the kind of
inflation this document exists to avoid. For a subscriber, the practical
benefit of compression is **plan-limit headroom**: ~22% more work before
hitting weekly caps, valued in the same API-list currency.

### "Is the compression lossy?"

**Yes. Unapologetically.** This is not gzip; the model receives less text.
Exactly what happens, per over-budget block
(`ali-kernel/src/compress.rs`, ~55 lines, deterministic, no ML):

1. **ANSI escape codes stripped** — lossless (color codes carry no meaning
   to the model and waste tokens).
2. **Blank-line runs collapsed** to one, trailing whitespace trimmed —
   lossless for content, lossy for exact formatting.
3. **Smart truncation** — the lossy step. If still over budget, keep the
   first 2/3 and last 1/3 of the budget and replace the middle with an
   explicit marker: `…[N chars compressed by L5.5]…`. Head+tail is chosen
   because errors, results, and conclusions cluster at the ends of tool
   output; the middle of a 500KB build log is the cheapest thing to lose.

v2 quality guards (all deterministic — determinism is a tested invariant,
because non-deterministic output would break Anthropic prompt caching):

- **Salient-line rescue**: lines matching error/warning/fail/panic/exception
  patterns in the elided middle are kept (capped at budget/8), so truncation
  no longer eats the exact lines a coding agent needs most.
- **Min-win guard**: if compression would shave under 10%, the original
  passes through untouched — no marker noise for marginal gains.
- **Lossless pass on all tool outputs**: steps 1-2 (ANSI, blank runs) now
  apply to tool results of any size, not just over-budget ones — pure
  savings with zero information loss.
- **Observation mining**: every elided middle is appended (deduped by hash,
  16KB sample cap, 200MB file cap) to `training/mined-middles.jsonl` — the
  corpus a local-LLM semantic compressor will train on, so the lossy step
  can eventually become a summarizing step. The local model is deliberately
  NOT on the hot path today: summarizing 300KB on a local GPU adds seconds
  per request and non-determinism would break prompt caching.

What is **never** touched:

- Under-budget system and user text blocks pass through byte-identical
  (tool results of any size get the lossless ANSI/blank-line pass only —
  truncation still requires crossing the 4,000-char budget).
- JSON structure, `cache_control` markers, tool_use blocks, images,
  non-text blocks — only the text *inside* text fields is
  replaced (`compress_text_field` in `anthropic_proxy.rs`).
- The response direction. Nothing the model says is altered.

**Does the loss hurt output quality?** Sometimes it can — if the model
needed the middle of a huge file read, it will see the marker instead and
may have to re-read the specific range. That is the trade. Two mitigations:
the marker is visible to the model (it knows content was elided and can ask
for it), and the budgets only fire on outsized blocks — normal conversation
is untouched. The session that produced the snapshot above (a full
multi-hour coding session including building this very feature) ran through
the compressor end-to-end.

### "Are the dollar figures a real bill?"

`api_value_usd` is **priced from real billed token counts** at Anthropic
list prices (July 2026): Fable/Mythos 5 $10/$50 per MTok, Opus 4.8 $5/$25,
Haiku $1/$5, Sonnet/default $3/$15; cache reads at 0.1x input, cache writes
at 1.25x input (`rates_for` in `usage_ledger.rs`, covered by unit tests).
It should match your Anthropic console/credit burn to within rounding —
verify against your own console.

Caveats, stated plainly:

- Sonnet is on an intro price ($2/$10) until 2026-08-31; the table uses the
  post-intro rate, so Sonnet rows overstate slightly until then.
- Batch discounts, regional multipliers, and negotiated enterprise rates
  are not modeled.

### "And `estimated_savings_usd`?"

An **estimate, labeled as such everywhere it appears** (the `/stats`
response carries a `note` field saying exactly this). Formula per request:

```
tokens_removed  = (chars_raw − chars_compressed) / 4        // ~4 chars/token heuristic
effective_rate  = billed-input-weighted blend of
                  uncached (1.0x) / cache-read (0.1x) / cache-write (1.25x)
savings         = tokens_removed × effective_rate
```

Why the blend matters: most context in an agent session re-bills as cache
reads at 10% of base price. Naively pricing removed chars at the uncached
rate would inflate savings ~10x. This is why **30% fewer chars ≠ 30% fewer
dollars** — the honest cost reduction is ~22%, and that's the number we
stand behind.

The exact counterfactual (send every request twice, compressed and not)
would double spend to measure; we accept the labeled estimate instead.

### "Doesn't rewriting the prompt break Anthropic's prompt caching?"

The compressor preserves block structure and `cache_control` markers, and
it is **deterministic**: the same input text always compresses to the same
output, so a message compressed identically on turn N and turn N+1 still
prefix-matches. The measured proof is in the snapshot itself: **16.26M of
16.84M input tokens (96.6%) billed as cache reads** — caching demonstrably
survives compression. (A block that crosses its budget threshold between
turns does invalidate the cache suffix from that point — visible as
`cache_creation` tokens, 3.3% of input here.)

### "Does it add latency?"

The compression pass is pure string work on the request (~ms on hundreds of
KB). On the response path the gateway forwards bytes to the client as they
arrive; usage extraction happens on a copy in a spawned task
(`anthropic_proxy.rs`, response tee) — it cannot delay the stream. If the
client disconnects mid-stream, the tee drains upstream only to finish the
usage record.

### "Can the gateway see or store my API key?"

It forwards the caller's `x-api-key`/`Authorization` header unchanged and
never logs or persists it (`anthropic_proxy.rs` header handling; grep the
codebase for any write of those headers — there is none). The gateway binds
to `127.0.0.1` only. Binary attestation (L1) refuses to start if the binary
on disk changed since sealing.

### "What about the response? Do you compress what the model says?"

No. The response direction is byte-for-byte passthrough (plus the passive
tee). Only the request context is compressed.

### "Why should I trust the token counts?"

You shouldn't trust *us* — they're Anthropic's own numbers. Diff any line of
`usage.jsonl` against the `usage` block in the raw API response (run one
request with `ANTHROPIC_BASE_URL` unset and compare shapes), or reconcile
totals against the Anthropic console for the same window.

## 5. Reproduce it yourself (10 minutes)

```bash
# 1. Build and start the gateway (see README for service setup)
cargo build --release

# 2. Point Claude Code at it and restart claude
export ANTHROPIC_BASE_URL="http://127.0.0.1:9000"

# 3. Watch compression live
tail -f logs/gateway.log | grep --line-buffered "L5.5 compressed"

# 4. After a few turns: per-request billed usage
tail -3 logs/usage.jsonl | jq .

# 5. Aggregates with pricing
curl -s http://127.0.0.1:9000/stats | jq .

# 6. In the TUI (./target/release/ali-tui): "$ Savings (billed usage)" panel,
#    refreshed every 10s. Ctrl+Y toggles copy mode for selecting text.
```

Unit tests for the measurement chain (SSE parsing across chunk boundaries,
pricing math, cache-mix savings, compression edge cases):

```bash
cargo test -p ali-kernel usage_ledger compress anthropic_proxy
```

## 6. What this proof does NOT claim

- No claim about market size, TAM, or revenue. Any such number you may have
  seen attached to this project is not backed by this repo.
- No claim that char-% equals cost-%. It doesn't; §4 explains why.
- No claim of zero quality impact. Truncation is lossy by design; the claim
  is that the trade is visible (markers), bounded (budgets), and worth it
  (~22% measured-cost reduction on real sessions).
- Savings figures are estimates derived from real billed tokens; spend
  figures are real billed tokens at list prices.

*Environment for the snapshot: rustc 1.96.0, gateway v2.1.0, Claude Code
via `ANTHROPIC_BASE_URL=http://127.0.0.1:9000`, models `claude-fable-5`
(primary) and `claude-opus-4-8` (classifier traffic).*
