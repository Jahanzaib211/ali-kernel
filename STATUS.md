# Ali Kernel — Live Status

_Last updated: 2026-06-30 (session: stand-up + dogfood + RLAIF flywheel)_

This is the plain-English status of the build: what runs, how the local model actually
behaves, and what's next. The full engineering roadmap lives in the plan file
(`~/.claude/plans/…ornith-sov…md`); this page is the human-readable summary.

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
| Binary + constitution attestation | ✅ (startup) | hashes sealed in `/tmp/ornith-ram-moat` |
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
