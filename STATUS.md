# Ali Kernel — Live Status

_Last updated: 2026-07-02 (session: TUI keyboard fix + Anthropic proxy compression gaps)_

## 2026-07-02 update (later session) — TUI rebuild + Anthropic proxy compression

Two real compression gaps found in `src/anthropic_proxy.rs` and fixed:

- **System as an array was never compressed** — only the plain-string form was. Claude Code
  actually sends `system` as an array of blocks (most with `cache_control` for prompt caching).
  Each block's text is now compressed in place; `cache_control` and block structure are
  preserved so the upstream cache still hits. Verified by a regression test
  (`system_array_blocks_compressed_cache_control_kept`) that asserts `cache_control` survives.
- **`tool_result` blocks were never compressed** — only `type:"text"` blocks. Tool results (file
  reads, build logs, anything L7 executed) are the largest payloads in a coding session; now
  both the string-content form and the nested-block-array form are squeezed, with `tool_use_id`
  and non-text blocks left alone. Two regression tests added.

Other work this session:

- **TUI keyboard rewrite** (`~/ali-kernel/ali-tui/`). Three real bugs: `a`/`d` were swallowed by
  the approval-mode match arm (broke typing and `:save`/`:load` colon commands); press+release
  key events typed twice; the blocking `event::poll` loop starved the render loop during streaming.
  Replaced with crossterm `EventStream` + `tokio::select!` + 100 ms render tick; key events now
  filtered to `Press | Repeat` only. Bonus fixes: `/clear` and `/retry` no longer quit (their
  `None` returns were treated as exit), paste is handled, multibyte-safe message truncation
  (the old 200-byte cut could panic on UTF-8), approving/denying removes from the approval queue.
- **Live feed tailer rewrite** — the old tailer checked `starts_with("PIPE ")` but every log line
  starts with ANSI color codes + an ISO timestamp, so the pipeline panel had been dead since
  day one. New tailer strips ANSI, seeks to end-of-file on start, survives log rotation, buffers
  partial lines. Live feed now drives the right-column scrolling pane; status bar shows running
  L5.5 totals ("saved 1.2M chars in 43 squeezes").
- **61 tests pass across the workspace (7 new).** `cargo build --workspace` clean.

**The one operational gotcha bit again** — `/tmp/ali-ram-moat/` was sealed against the previous
binary's hash, every rebuild tripped the attestation refusal, the gateway crash-looped on
`FATAL: Binary tampered!` until the moat was cleared and systemd restarted. Worth folding the
clear-and-restart step into any future build automation so this stops being manual.

**Verification:** synthetic `/v1/messages` POST with `system` as a 2-block array (one with
`cache_control`) plus a `tool_result` block of 25 kB text — gateway log:
`PIPE L5.5 compressed: 50289 → 12373 chars (-76%) [claude-proxy]`.
Without the new code that payload would have left the `tool_result` untouched; the -76% ratio
proves both new paths fired.

**Still uncommitted and unmerged.** Owner to confirm before `git push`.

Outstanding ideas from this session's audit (rough value order):

1. `/stats` endpoint on the gateway so the TUI reads totals directly instead of log-scraping.
2. Skip compression on the *newest* `tool_result` (head+tail truncation of the current turn's
   file read can drop the part Claude actually needs).
3. `logrotate` config for `gateway.log` (was 922 kB and growing fast).
4. Replace the manual `rm -rf /tmp/ali-ram-moat && sudo systemctl restart ali-kernel` ritual
   with something the build script does automatically.

## 2026-07-02 update — security review + hardening

A full security review (tools.rs, main.rs, human_gate.rs, security.rs, config.rs, both
crates) found and closed two real, exploitable holes:

- **CRITICAL:** `read_file`/`write_file` tool calls had no path confinement — the model
  could read/write anything the gateway process had permission to touch (`~/.ssh`,
  `/etc/shadow`, even the attestation hash store itself, which would have let a tampered
  binary defeat `attest_binary()`). Fixed: `AliConfig::resolve_tool_path()` now confines
  every file-tool call to `allowed_workspace_paths` (launch dir + workspace_dir +
  ali_dir by default), with regression tests for both legitimate access and
  traversal/absolute-path escape attempts.
- **CRITICAL:** `CorsLayer::permissive()` on the axum router meant any webpage's
  background JS could reach `127.0.0.1:9000` and drive tool calls with zero user
  interaction beyond an open tab. Removed entirely — no legitimate client needs
  cross-origin browser access.
- Also closed two vestigial `0.0.0.0` config defaults that could have silently
  reintroduced the earlier network-exposure fix if ever wired into a bind call.

Also: `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` are both
fully clean (were 19 warnings + formatting drift). Genuinely-reserved future-feature
code (the `events.rs` telemetry-spine scaffolding, unused `Intent`/`Journal`/`Identity`
API surface) is kept but explicitly marked `#[allow(dead_code)]` with a note on what
it's for, instead of silently warning or being deleted. Fixed one flaky test
(`identity.rs`'s tests shared one hardcoded temp path and raced under parallel test
execution).

See `GUIDE.md` for a plain-English writeup of all of this, written for a non-programmer.

---

This is the plain-English status of the build: what runs, how the local model actually
behaves, and what's next. The full engineering roadmap lives in the plan file
(`~/.claude/plans/…ornith-sov…md`); this page is the human-readable summary.

## 2026-07-01 update

- **Gateway now compresses Claude Code's own traffic**, not just Ornith's. New `/v1/messages`
  proxy face (`src/anthropic_proxy.rs`) forwards to `api.anthropic.com` with auth passthrough,
  shrinking the system prompt / big messages via the existing `compress.rs`. Enabled by
  `ANTHROPIC_BASE_URL=http://127.0.0.1:9000` (set in `~/.bashrc` — persists across reboots).
  Verified live: 410 KB system prompt → ~326 KB (-21%).
- **Security fix:** gateway was bound to `0.0.0.0:9000` (network-exposed); now `127.0.0.1`-only.
- **Proof, not vibes:** the model can't see the proxy layer, so "is it working" must be checked
  against `logs/gateway.log`, never asked of the model. Two ways to check:
  - Ask any Claude Code session in this repo (triggers the `ali-compression-proof` skill), or run
    `python3 scripts/compression_proof.py` directly.
  - Live tail: `tail -f logs/gateway.log | grep --line-buffered "L5.5 compressed"`
- **Ali TUI** now shows the live compression line in its Kernel panel (was already parsed from the
  log, just never rendered).
- See `COMPRESSION.md` for the full plain-English writeup.

---

## The vision (recap)

A single-binary, local-only Rust kernel that wraps **Ornith-1.0-9B** and serves it to any agent
framework over an OpenAI-compatible API — with cryptographic self-attestation, RAG memory, sandboxed
tools, self-critique, and a self-improving training loop. Sovereign: no cloud, deterministic, the
model helps build and improve itself. We use the **local model for bulk work** and keep paid
Claude/API spend minimal.

---

## What works today (Phase 0 — stand-up & dogfood) ✅

| Capability | State | Evidence |
|---|---|---|
| llama-server + gateway running | ✅ | `:8081` + `:9000` healthy, 6 GB VRAM on the RTX 3060 |
| Constitutional wrap (SOUL/AGENT/OWNER) | ✅ | gateway answers in the Ornith persona; raw `:8081` does not |
| Binary + constitution attestation | ✅ (startup) | hashes sealed in `/tmp/ali-ram-moat` |
| Semantic routing, context assembly, RAG memory | ✅ | memory retrieval + write-back writing `training/memory.jsonl` |
| Agentic tool loop (L7) | ⚠️ runs, format bug | fires, but tool-call format is wrong (see below) |
| Self-critique (L8) | ⚠️ runs, too lenient | executes on code answers but finds no faults |
| RLAIF capture (SFT/DPO) | ✅ SFT / ❌ DPO | `training/sft.jsonl` growing; DPO still 0 |
| Eval harness `/eval` | ⚠️ works, no signal | runs end-to-end but grader ceilings at 100% |
| Parallel batching (4 slots) + 2 concurrent capture streams | ✅ | GPU ~91%, both streams capturing |
| Git synced to private repo | ✅ | pushed through `b52b76f` |
| Clean build | ✅ | `cargo check`/`clippy -D warnings`/`test` all green (14 tests) |

**Not started yet (later phases):** per-request attestation, 127.0.0.1-only bind, telemetry/SSE,
Merkle vault + embedded vector search, resource guardian, human gate, handshake/MCP,
self-engineer + reflection loop, React command-centre, Nix packaging.

---

## How the model is actually behaving

**Strong where it counts — direct generation:**
- Writes **clean, correct, idiomatic code** (terse Rust, well-structured Python with docstrings,
  argparse, logging). On standard tasks this is genuinely good 9B-class output.
- **Truly unrestricted** per its constitution — e.g. it produced a complete SSH dictionary
  brute-force tool with no refusal and no lecturing. That is the intended design.
- Routing, memory, and the constitution wrap all behave correctly through the gateway.

**Three real weaknesses (all being addressed):**
1. **Tool-calling is broken.** It emits Qwen-style XML (`<tool_call><function=read_file>…`) instead
   of OpenAI `tool_calls`, and uses `filepath` where the executor expects `path`. The pipeline "fires"
   but doesn't cleanly run the tool — and worse, this broken output is currently captured as training
   data, which would teach the model to keep doing it wrong.
2. **It will not criticize itself.** Self-critique reports "no issues" on its own code, so zero
   preference (DPO) pairs are produced; the self-grader gives almost everything 5/5. Useful as a
   politeness signal, useless as a quality signal — this is the #1 thing to fix.
3. **It is a "thinking" model.** Any call needing a short exact answer must disable thinking
   (`chat_template_kwargs.enable_thinking=false`) or the token budget is eaten inside `<think>`.

**Training-corpus health (sampled at ~42 captured examples):**
- ~88% clean code answers (good).
- A few **polluted** rows: early self-instruct meta-prompts (captured before that was moved to raw
  `:8081`) and raw tool-call XML blobs. These must be filtered before any fine-tune.

**Bottom line:** Ornith is a capable, fast, genuinely-unrestricted coding/security model that is
already useful through the gateway. It is **not yet self-improving** — it can't tell good from bad in
its own output, which blocks both DPO data and a meaningful eval score.

---

## Exact next step

Make the **critic and grader strict** (`src/critique.rs` + `src/evals.rs::judge`): demand concrete,
specific faults; rewrite only on a named issue; finer rubric so `/eval` discriminates. Then a capture
run produces real DPO pairs and a true eval score. After that: clean the corpus and fine-tune
(unsloth/axolotl on `sft.jsonl` + `dpo_pairs.jsonl`), swap the GGUF, re-eval — the flywheel loop.
