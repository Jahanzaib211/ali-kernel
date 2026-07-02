# How This Is Being Built — Parties, Roles & Method

_A living record of who builds the Ali Kernel and how. Persistent by design._
_Last updated: 2026-07-02._

---

## The parties and their roles

| Party | Role | What they own |
|---|---|---|
| **Jahanzaib Ali ("Max")** | **Owner / Product Lead / Vision** | Direction, the north stars (absolute sovereignty, token economy, deterministic local kernel), and the **final call on consequential changes** (anything that reroutes auth, modifies startup config, or spends real money). Generates the big architectural ideas (gateway-as-compressor, distill-a-compressor, crate-mastery). |
| **Claude (this agent)** | **Engineering Lead / Implementer** | Turns vision into shipped code: makes technical calls, writes/tests/reviews, surfaces trade-offs, **enforces discipline** (security findings closed before features; tests + clippy green on every commit). Orchestrates the local model; keeps its own (paid) token spend low. |
| **Ornith-1.0-9B** (local model) | **The Engine / Apprentice → Co-builder** | The model the gateway serves. Does generation, self-critique, crate exploration, and now real dev-loop coding (write → build → test). Subject of the training loop; trending toward self-improvement. |
| **The Gateway** (`ali-kernel`) | **The Product / Kernel** | The artifact being built. Provides the capabilities; the model behind it is interchangeable. Endgame: the single compressing proxy in front of *every* model. |
| **headroom** | **External Teacher / Reference** | Bootstraps the compression concepts (mined into native L5.5) and can wrap Claude Code to cut the orchestrator's tokens. **Not required at gateway runtime.** |

---

## How we build (method)

- **Plan → execute → verify → commit, in small increments.** Each capability lands as its own
  reviewed, tested, pushed commit. Nothing half-wired.
- **Discipline before features.** A known security finding on a powerful surface is closed before the
  next feature (e.g. fail-closed sandbox + argv-injection fix shipped before the compression layer).
- **Token economy (3 tiers).** Ornith (local, $0) does bulk work; Haiku-tier handles cheap orchestration;
  Opus (Claude) is reserved for architecture, review, and high-signal teacher feedback.
- **Dogfooding.** The gateway's own tools are used on its own build — e.g. `crate_search` picks the
  crates for new features; the dev loop will eventually extend the gateway itself.
- **Sovereignty + minimize-deps.** Prefer pure-Rust, single-binary, audited crates; mine concepts rather
  than adopt heavy stacks (native L5.5 instead of running headroom at runtime).
- **Persistence everywhere.** Plan file, `STATUS.md`, `ROADMAP.md`, `BUILD.md`, dated session files in
  `~/.claude/session-data/`, and a file-based memory — so any session (human or agent) can resume cold.

---

## The unification: one gateway compresses everyone

The gateway already has the two pieces (pluggable backend + native L5.5 compression). Joined, it becomes
a **sovereign compressing proxy for any model — including the orchestrator itself.**

```
  Ornith (local)  ◄───────────┐
                              │  OpenAI face  : /v1/chat/completions
  Any OpenAI provider ◄───────┤  (compress observations, route to backend)
                              │
        ┌──────────────  ALI KERNEL  ──────────────┐
        │            shared engine: compress.rs         │
        └──────────────────────┬────────────────────────┘
                              │  Anthropic face : /v1/messages  (LIVE)
  Claude Code  ───────────────┘  (compress request, forward to api.anthropic.com, stream back)
```

- **Done (2026-07-01):** `src/anthropic_proxy.rs` implements the Anthropic-Messages proxy face.
  Claude Code routes through the gateway via `ANTHROPIC_BASE_URL=http://127.0.0.1:9000`; auth
  passthrough (the caller's `x-api-key`/`Authorization` forwarded unchanged, never stored/logged);
  system prompt + oversized messages compressed via the same `compress.rs` used for Ornith. Verified
  live: a 410 KB system prompt (CLAUDE.md + tool schemas, sent every turn) compresses to ~326 KB
  (-21%) before it reaches `api.anthropic.com`. Proof lives in `logs/gateway.log` (grep
  `[claude-proxy]`) — see `COMPRESSION.md` and the `ali-compression-proof` skill/`scripts/compression_proof.py`.
  **One gateway now compresses Ornith and Claude simultaneously**; headroom is no longer needed.
- **Also done:** gateway bind hardened to `127.0.0.1`-only (was `0.0.0.0`, network-exposed) to match
  the sovereign/local-only invariant. Ali TUI's Kernel panel now surfaces the L5.5 compression line
  live (it was already parsing it from the log, just never rendering it).
- **Earlier (tool observations):** L5.5 compresses tool observations before they re-enter the local
  model (verified ~84% reduction on a 51 KB file → fit in the ~12K window).

This is the literal expression of the project thesis: *the gateway is the product; every model — local or
frontier, engine or orchestrator — sits behind the same sovereign capabilities.*
