# Ali Kernel — Roadmap & Crate Arsenal

_Last updated: 2026-07-05._ Companion to `STATUS.md` (live status) and the engineering plan.

## Vision

A single-binary, local-only Rust kernel that wraps **Ornith-1.0-9B** and serves it to any agent
framework over an OpenAI-compatible API — with cryptographic self-attestation, RAG memory, sandboxed
+ crate-aware tools, self-critique, and a self-improving training loop. **Sovereign**: no cloud,
deterministic, the model helps build and improve itself. Lean on the local model; spend paid API tokens
sparingly.

---

## Status

| Phase | What | State |
|---|---|---|
| **0** | Stand up + dogfood + version control | ✅ done |
| **0.5** | Behavioral shield (set-and-forget) | ✅ done — eval discriminates at **94%** |
| **0.75** | Ali TUI + human gate | ✅ done — elite approval UI with SSE streaming |
| 1 | Sovereign invariants (per-request attestation, 127.0.0.1, determinism) | next options |
| 2 | Telemetry spine (broadcast + SSE) | planned |
| 3 | Memory integrity + single-binary vector (Merkle vault, kill Qdrant) | planned |
| 4 | Runtime resilience (guardian + human gate) | planned |
| 5 | Harness layer (handshake/JWT + meta-learner + MCP) | planned |
| 6 | Autonomy (self-engineer + reflection loop) | planned |
| 7 | Command Centre (React + rust-embed) | planned |
| 8 | Reproducible deploy (Nix flake) | planned |

**Recommended next:** close the **training loop** — capture (now clean SFT + real DPO) → `clean_corpus.py`
→ first LoRA fine-tune (unsloth) → swap GGUF → re-eval to move the 94%. This is the payoff of the shield.

---

## Crate Arsenal (curated — not the kitchen sink)

A 200-crate "enterprise mesh" was proposed. Engineering reality: **each dependency is build time + attack
surface**, and our own rules say *minimize deps, run `cargo audit`, stay sovereign*. So this is the curated
~20 that actually serve the vision. AI/LLM crates are called out first.

### AI / ML / LLM
| Concern | Use now | Why / where |
|---|---|---|
| Inference | **llama.cpp via HTTP** (current) | keep the sidecar — gives watchdog, restart, circuit-breaker. (FFI `llama-cpp-2`/`candle` embedding rejected: ~30% slower, kills the watchdog.) |
| Local embeddings | **`candle-core` + `candle-transformers`** | in-process embeddings (<2ms) for RAG → replaces the `/v1/embeddings` round-trip (Phase 3, L4) |
| Tokenization | **`tiktoken-rs`** (OpenAI cl100k_base port) | exact token budget in `context.rs`/`compress.rs` instead of the 4-chars/token estimate. Replaces the earlier `tokenizers` (HF) pick — tiktoken-rs is smaller and gives us OpenAI-compatible token accounting out of the box (L5). Added 2026-07-05. |
| Vector search | **`sqlite-vec`** (via `rusqlite`) or **`hnsw_rs`** | single-binary ANN → **kills the Qdrant sidecar** (Phase 3) |
| Fine-tuning | **unsloth / axolotl** (offline, Python) | LoRA on our `sft.jsonl` + `dpo_pairs.jsonl`; swap GGUF, re-eval |
| _Consider later_ | `burn` (in-Rust training), `ort`/`tract` (ONNX small models) | only if we ever train/serve in-process |
| _Skip_ | `mistral-rs`, `wonnx`, `ratchet`, `faiss-rs`, `milvus-sdk`, `qdrant-client`, `surrealdb` | redundant with llama.cpp + sqlite-vec; sovereignty/bloat |

### Infrastructure (per phase)
| Concern | Crate | Phase |
|---|---|---|
| Web/runtime | `axum`, `tokio`, `tower` (have) | core |
| Integrity / Merkle | `sha2` (have), `rs_merkle` | 3 (vault) |
| Auth handshake | `jsonwebtoken`, `hmac` | 5 |
| Resilience | `governor` (rate-limit), `backon` (retry → replace hand-rolled), `sysinfo` (CPU), `nvml-wrapper` (VRAM) | 4 |
| Telemetry/SSE | `tokio::sync::broadcast` + axum SSE (built-in) | 2 |
| Checkpoints | `rkyv` or `bincode` (fast flight-recorder snapshots) | 6 |
| UI embed | `rust-embed` | 7 |
| Hygiene | `cargo audit`, `cargo deny` (CI) | cross-cutting |
| _Skip_ | `kube`, `etcd`, `consul`, `aws-sdk-*`, `libp2p`, `quinn`, `tonic` | a single-owner local gateway needs none of these |

**Principle:** add a crate when the need is real (DRY/YAGNI), audit it, prefer pure-Rust + sovereign.
The crate-aware `crate_search`/`docs_lookup` tools (shipped in Phase 0.5) let **Ornith itself** justify
each pick from crates.io/docs.rs — and that usage becomes the fine-tune signal toward "a Rust engineer
who masters crates."

---

## Context Compression layer (mined from headroom)

Ref: `github.com/headroomlabs-ai/headroom` — a context-compression layer that shrinks LLM input 60–95%.
High value here because **Ornith's window is small (~12K)** and we obsess over tokens. Mine the concepts,
implement lean in-gateway (sovereignty / single-binary), do **not** adopt the full stack.

- **L5.5 Compression** (new stage, between `context::assemble` and `brain::infer`): squeeze tool outputs
  (cargo errors, file reads, crate docs), retrieved memory, and long history before the small window.
  Strategies: AST-light code squeeze + JSON key-pruning + smart truncation (headroom's CodeCompressor /
  SmartCrusher, lean Rust impl).
- **Cache alignment**: keep the system prefix byte-stable to maximize llama.cpp prompt-cache hits
  (we already see `cached_tokens` from the constitution prefix — formalize it).
- **Reversible (CCR)**: store full content in scratchpad/vault, inject compressed, expand on demand.
- **Failure mining** (`headroom learn` analogue): miner over `pipeline.jsonl` + low `/eval` scores →
  correction notes feeding the RLAIF/DPO loop.
- _Optional later_: headroom's Rust core for the trained prose compressor; or run headroom as a Claude
  Code wrapper to cut the orchestrator's own token spend (their tool supports Claude Code directly).
- **Gateway-as-compressing-proxy (the unification) — ✅ done 2026-07-01:** `/v1/messages` proxy face
  (`src/anthropic_proxy.rs`) forwards to `api.anthropic.com` with auth passthrough, sharing
  `compress.rs`. Claude Code routes through the gateway via `ANTHROPIC_BASE_URL=http://127.0.0.1:9000`
  (set in `~/.bashrc`, survives reboot). Verified live: -21% on a 410 KB system prompt. **One sovereign
  gateway now compresses both Ornith and Claude simultaneously**; headroom is no longer needed. See
  `BUILD.md` and `COMPRESSION.md`. Proof/check: `ali-compression-proof` skill or
  `python3 scripts/compression_proof.py`.
