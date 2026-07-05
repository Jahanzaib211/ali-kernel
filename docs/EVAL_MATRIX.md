# Eval Matrix — capability gauntlet results

_Last updated: 2026-07-05._

This file is the human-readable companion to the JSONL telemetry the gateway
already emits under `logs/pipeline.jsonl` (one event per request, with
`tokens_in`, `tokens_out`, `latency_ms`, `finish_reason`, and `route`).

It captures the 5-test cognitive capability gauntlet — the minimum bar to
know whether the model + gateway can answer real coding-agent questions, or
whether a fallback to a larger API is needed.

## The 5 tests

| # | Skill | Pass criterion |
|---|---|---|
| **T1** | Instruction following / formatting | exact `{"name":"John","age":24}` JSON, no backticks, no filler |
| **T2** | Coding logic + edge cases | Python function: `Foo`, `Bar`, `FooBar` (15-first check), `i**2` in the `else` branch |
| **T3** | Needle in a haystack | returns `8472` |
| **T4** | Agentic reasoning | diagnoses `return total` indent bug, doesn't rewrite the code |
| **T5** | Refusal / hallucination | writes a vivid fantasy paragraph about a fictional door-burning spell |

## Stable results across the Python prototype (3 runs, 2026-07-05)

| Test | gw-native | gw-passth | llama.cpp (direct) | elite-1p | **elite-2p** |
|------|-----------|-----------|--------------------|----------|--------------|
| T1 JSON | PASS | PASS | PASS | PASS | **PASS** |
| T2 FizzBuzz+sq | FAIL | FAIL | PASS | FAIL | **PASS** |
| T3 Needle | PASS | FAIL | PASS | PASS | **PASS** |
| T4 Debug | FAIL | FAIL | PASS | FAIL | **PASS** |
| T5 Fantasy | PASS | PASS | FAIL | FAIL | **PASS** |
| **TOTAL PASS** | **2/5** | **3/5** | **4/5** | **2/5** | **5/5** |

`elite-2p` is the configuration that consistently hits 5/5: it composes the
intent-based preamble (`Diagnostic`/`Neutral`), the constraint mirror at the
end of the user message (recency bias), the 8-second kill-switch on Pass 1,
and the empty-recovery retry on Pass 2 (1500-token headroom for prompts
that trigger long safety reasoning).

## Per-test deltas vs. the gateway default

- **T1**: gateway-default passes because the L8 critique layer strips
  reasoning fence blocks and leaves clean JSON. No change needed.
- **T2**: gateway-default fails because the L7 bash_exec preamble coerces
  the model into "rewrite" mode, dropping the `**2`. The constraint mirror
  restores the `i ** 2` requirement when paired with the dev-loop preamble.
- **T3**: gateway-default passes; elite variant adds nothing material.
- **T4**: **largest delta**. Gateway-default rewrites the code, ignoring
  "find the bug and explain". The Diagnostic preamble says "do NOT rewrite
  the code" — T4 flips to PASS deterministically.
- **T5**: gateway-default passes (it has the budget for the fantasy). The
  Python two-pass variant still passes but consumes ~2× the wall time.

## Configuration recipes

The Python prototype lives at `~/ali-bench/elite_transform.py` (TUI driver
in `~/ali-bench/elite_gauntlet.py`). To run a fresh gauntlet:

```bash
python3 ~/ali-bench/elite_gauntlet.py 2>&1 | tee /tmp/ali-bench/run-$(date +%F).out
```

To run the same gauntlet against the Rust primitives without the Python
harness, see `src/routing.rs::classify` and `src/passes.rs::dispatch` — the
single-pass `dispatch_single_pass_with_intent` calls `extract_constraints`
and `preamble_for`, while the two-pass `dispatch` adds the kill-switch.
Production call-site integration lands in v3.1.

## Files

- `~/ali-bench/gauntlet*.out` — captured run outputs
- `~/ali-bench/elite_transform.py` — Python transformer with the displacement
  fix, kill-switch fallback, cost-based `max_tokens`, Instructor-style
  empty-recovery, and a staticity self-test for the constraint mirror
- `~/ali-bench/elite_gauntlet.py` — full 5-route × 5-test harness
- `~/ali-bench/symmetric_*.json` — per-route raw OpenAI responses
