# Ali Kernel

> A single-binary Rust runtime that exposes an OpenAI-compatible API and **augments the model with access to the Rust ecosystem** — discovering libraries, reading documentation, executing sandboxed tools, and integrating dependencies — on top of binary attestation, constitution integrity, semantic routing, RAG memory, self-critique, and structured observability.

**The gateway is the product; the model backend is pluggable.** It ships pointed at a local
`llama.cpp` server, but `BACKEND_URL` switches it to any OpenAI-compatible provider without touching
memory, tools, routing, or the behavioral shield. The real power is combining ecosystem access with an
iterative develop → build → test loop — every capability benefits whichever model is behind it.

[![Rust](https://img.shields.io/badge/rust-1.78%2B-orange)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT-blue)](#license)
[![Model](https://img.shields.io/badge/model-Ornith--1.0--9B--Q4__K__M-purple)](#)

---

## Architecture

```
Client (any OpenAI SDK)
        │
         ▼ :9000
┌───────────────────────────────────────────────────────────┐
│                       Ali Kernel                           │
│                                                           │
│  L0  Config & Environment       (.env validation)         │
│  L1  Binary Attestation         (SHA-256 /proc/self/exe)  │
│  L2  Integrity Vault            (Constitution SHA-256)    │
│  L3  Semantic Router            (regex classification)    │
│  L4  Memory                     (Qdrant + local fallback) │
│  L5  Context Assembler          (sliding window, tokens)  │
│  L5.5 Compression               (shrinks oversized text)   │
│  L6  Brain Proxy                (SSE + retry forwarding)  │
│  L7  Tool Executor              (bubblewrap-sandboxed)     │
│  L8  Critique Loop              (code review + fix)       │
│  L9  Observability              (JSONL pipeline log)      │
└───────────────────────────────────────────────────────────┘
        │
        ▼ :8081
   llama-server (llama.cpp)
    ornith-1.0-9B Q4_K_M
```

---

## Launcher

Type `./ali` in the repo root to launch the Ali Kernel TUI (add it to your `PATH` to
just type `ali` from anywhere). See the `ali` script for what it does: starts
`llama-server` and the gateway if they aren't already running, then launches the TUI.

---

## Hardware Requirements

| Component | Minimum | Verified |
|-----------|---------|---------|
| GPU | RTX 3060 8GB | RTX 3060 12GB |
| RAM | 16GB | 32GB |
| Disk | 20GB free | 281GB free |
| CUDA | 11.8+ | 13.2 |
| OS | Ubuntu 20.04+ | Ubuntu 24.04 |

---

## Benchmark Results

| Metric | Result |
|--------|--------|
| Generation speed | **48 tok/s** |
| Prompt processing | **1,400+ tok/s** |
| Tool calling accuracy | **10/10 (100%)** |
| Max usable context | **~12K tokens** |
| Gateway VRAM overhead | **0 MB** (pure Rust) |
| Binary size | ~8MB |

---

## Prerequisites

### 1. llama-server (must be running before gateway starts)

```bash
~/llama.cpp/build/bin/llama-server \
  --port 8081 \
  -ngl 99 \
  --jinja \
  -c 16384 \
  --cache-ram 4096 \
  --host 127.0.0.1 \
  -m ~/ornith-1.0-9b-Q4_K_M.gguf
```

`--host 127.0.0.1` matters — binding to `0.0.0.0` exposes the raw, unauthenticated model
API to your network. See `GUIDE.md` for why this was tightened.

Verify: `curl http://localhost:8081/health` → `{"status":"ok"}`

### 2. Rust toolchain

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source $HOME/.cargo/env
```

---

## Installation

```bash
git clone https://github.com/Jahanzaib211/ali-kernel
cd ali-kernel

# Configure environment
cp .env.example .env
# Edit .env with your paths

# Build
cargo build --release

# Initialise integrity hashes (first run)
mkdir -p /tmp/ali-ram-moat
sha256sum target/release/ali-kernel | awk '{print $1}' > /tmp/ali-ram-moat/golden_binary.hash
sha256sum constitutions/SOUL.md | awk '{print $1}' > /tmp/ali-ram-moat/soul_golden_hash.txt
```

---

## Running

```bash
# Foreground
./target/release/ali-kernel

# systemd (production) — installs + enables both llama-server and the gateway,
# so they auto-start on boot instead of needing ./ali run by hand
sudo bash install-service.sh
sudo journalctl -fu ali-kernel -fu llama-server
```

---

## API Reference

All endpoints are OpenAI SDK-compatible.

### `GET /health`

```json
{
  "status": "alive",
  "version": "2.1.0",
  "capabilities": ["rag_memory", "agentic_tools", "crate_aware_tools",
                   "behavioral_shield", "eval_harness", "rlaif_capture"],
  "backend": "healthy",
  "backend_url": "http://127.0.0.1:8081/v1",
  "backend_local": true,
  "model": "ornith"
}
```

### `GET /v1/models`

```json
{
  "object": "list",
  "data": [{ "id": "ornith", "object": "model", "owned_by": "ali-kernel", "created": 1700000000 }]
}
```

### `POST /v1/messages`

Anthropic Messages API proxy — point Claude Code at the gateway
(`ANTHROPIC_BASE_URL=http://127.0.0.1:9000`) to compress its own context
before forwarding to `api.anthropic.com`. Auth passthrough: your `x-api-key`
is forwarded unchanged, never stored or logged. See `COMPRESSION.md`.

### `POST /v1/chat/completions`

Fully compatible with the OpenAI chat completions API.

```bash
curl -X POST http://localhost:9000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "ornith",
    "messages": [{"role": "user", "content": "Write a binary search in Rust"}],
    "temperature": 0.6,
    "max_tokens": 2048
  }'
```

**Streaming (SSE):**

```bash
curl -X POST http://localhost:9000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"ornith","messages":[{"role":"user","content":"Hello"}],"stream":true}'
```

**Tool calling:**

```bash
curl -X POST http://localhost:9000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "ornith",
    "messages": [{"role": "user", "content": "Get weather in NYC"}],
    "tools": [{
      "type": "function",
      "function": {
        "name": "get_weather",
        "parameters": {
          "type": "object",
          "properties": {"city": {"type": "string"}},
          "required": ["city"]
        }
      }
    }],
    "temperature": 0.6,
    "max_tokens": 512
  }'
```

**Constraints:**
- `max_tokens` is hard-capped at 12,000
- Empty `messages` → HTTP 400
- `reasoning_content` is passed through transparently

### Human Gate — `POST /v1/approvals`, `GET /v1/approvals/:request_id`

Pass `"human_gate": true` in a `/v1/chat/completions` request (or set
`human_gate_enabled = true` in config) to pause before any tool call executes
and wait for your approval:

```bash
# Check what's pending
curl http://localhost:9000/v1/approvals/<request_id>

# Approve (optionally rewriting the tool's args) or deny
curl -X POST http://localhost:9000/v1/approvals \
  -H "Content-Type: application/json" \
  -d '{"request_id": "<request_id>", "action": "approve", "tool_index": 0}'
```

Unresolved approvals time out after 5 minutes. Note: these endpoints are not
authenticated beyond the (unguessable) `request_id` — see `GUIDE.md` for the
current threat model.

### `GET /benchmark`

Runs 5 internal test prompts and returns timing stats:

```json
{
  "benchmark": [
    {"prompt": "Write a binary search in Rust", "latency_ms": 4200, "ok": true},
    ...
  ],
  "model": "ornith",
  "gateway_version": "2.1.0"
}
```

---

## Security Architecture

### Binary Attestation (L1)

On first run, the gateway hashes itself (`/proc/self/exe`, SHA-256) and writes the hash to `/tmp/ali-ram-moat/golden_binary.hash`. Every subsequent run verifies this hash. A mismatch triggers an immediate `exit(1)`.

```
First run:  hash(binary) → /tmp/ali-ram-moat/golden_binary.hash
Every run:  hash(binary) == stored_hash  →  proceed
                                         →  exit(1) if mismatch
```

### Constitution Integrity (L2)

`constitutions/SOUL.md` is SHA-256 hashed and locked on first run. Tampering with the constitution causes a `403 Constitution tampered` response.

### Semantic Router (L3)

Queries are classified before forwarding:

| Pattern | Route | Temperature |
|---------|-------|-------------|
| `^(hello\|hi\|hey\|...)` | Quick | 0.0 |
| `(scan\|exploit\|hack\|...)` | Full | 0.6 |
| `(latest\|news\|current\|...)` | Search→Full | 0.6 |
| default | Full | 0.6 |

---

## Observability

Every request is logged to `logs/pipeline.jsonl`:

```json
{
  "timestamp": "2026-06-29T18:30:00Z",
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "model": "ornith",
  "prompt_tokens": 142,
  "completion_tokens": 387,
  "latency_ms": 3820,
  "route": "full",
  "tool_called": false,
  "critique_applied": true
}
```

---

## Pipeline Layers

| Layer | Module | Status |
|-------|--------|--------|
| L0 Config | `config.rs` | Production |
| L1 Binary Attestation | `security.rs` | Production |
| L2 Constitution Integrity | `security.rs` | Production |
| L3 Semantic Router | `router.rs` | Production |
| L4 Memory | `memory.rs` | Production (Qdrant when `QDRANT_URL` set, local keyword fallback otherwise) |
| L5 Context Assembly | `context.rs` | Production |
| L5.5 Compression | `compress.rs`, `anthropic_proxy.rs` | Production (shared by the Ornith and Claude-proxy paths) |
| L6 Brain Proxy | `brain.rs`, `anthropic_proxy.rs` | Production (pluggable backend + `/v1/messages` Anthropic face) |
| L7 Tool Executor | `tools.rs` | Production (bubblewrap-sandboxed `bash_exec`; file tools confined to `allowed_workspace_paths`) |
| L8 Critique Loop | `critique.rs` | Production |
| L9 Observability | `logging.rs` | Production |
| Human Gate | `human_gate.rs` | Production — optional approval pause before tool execution, see below |

---

## Configuration

All config via `.env` (see `.env.example`):

| Variable | Description | Default |
|----------|-------------|---------|
| `LLAMA_BIN` | Path to llama-server binary | required |
| `MODEL_PATH` | Path to GGUF model file | required |
| `LLAMA_PORT` | llama-server port | `8081` |
| `GATEWAY_PORT` | Gateway listen port | `9000` |
| `SYSTEM_PROMPT` | System-level instruction | see `.env.example` |
| `RAM_MOAT` | Path for integrity hashes | `/tmp/ali-ram-moat` |
| `LOG_DIR` | Pipeline log directory | `logs/` |
| `BACKEND_URL` | Swap the model backend to any OpenAI-compatible provider | local llama.cpp |
| `ANTHROPIC_BASE_URL` | Set to `http://127.0.0.1:9000` to route Claude Code itself through the gateway's `/v1/messages` compressing proxy | unset |

See `.env.example` for the full set (dev-loop workspace, network-tool toggles, etc.) with inline comments.

---

## systemd Service

```ini
[Unit]
Description=Ali Kernel
After=network.target

[Service]
Type=simple
User=jahanzaib
ExecStart=/home/jahanzaib/ali-kernel/target/release/ali-kernel
Restart=on-failure
RestartSec=5
Environment=HOME=/home/jahanzaib
WorkingDirectory=/home/jahanzaib/ali-kernel
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
```

---

## Testing

```bash
# Unit tests (router, context — no services needed)
cargo test

# E2E tests (requires gateway + llama-server running)
cargo test --test integration -- --ignored
```

Expected unit test output:
```
test router::tests::quick_route_on_greeting ... ok
test router::tests::full_route_on_security_query ... ok
test router::tests::full_route_on_default ... ok
test router::tests::search_route_on_news_query ... ok
test context::tests::assembles_system_message_from_constitutions ... ok
test context::tests::sliding_window_keeps_last_10_turns ... ok
test context::tests::memory_injected_after_system ... ok
```

---

## Roadmap

| Phase | Feature | Status |
|-------|---------|--------|
| 1 | Core gateway (L0–L9) | **Done** |
| 2 | Streaming SSE proxy | **Done** |
| 3 | Qdrant vector memory (L4) | **Done** (optional — local keyword fallback when unset) |
| 4 | bubblewrap tool sandboxing (L7) | **Done** |
| 5 | Multi-model routing | **Done** (pluggable `BACKEND_URL` + Anthropic `/v1/messages` proxy) |

Full current phase-by-phase status (telemetry spine, memory-integrity vault, runtime
resilience, autonomy, etc.) lives in `ROADMAP.md` — this table is just the high-level
summary and won't be kept in perfect lockstep with it.

---

## License

MIT — use freely, ship responsibly.

---

## Work with me

Built by Jahanzaib Ali, Founder & Systems Engineer at [AliLabsX](https://alilabsx.com). I don't rent, I build: I run my own hardware and keep my own data.

[Jahanzaib@alilabsx.com](mailto:Jahanzaib@alilabsx.com) · [alilabsx.com](https://alilabsx.com) · [LinkedIn](https://www.linkedin.com/in/jahanzaib-ali-labsx)
