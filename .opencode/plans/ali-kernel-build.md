# Ali Kernel — Build Plan

## Context

Ornith Sovereign Gateway (ornith-stack) is a single-binary Rust HTTP proxy that wraps a local llama.cpp model, exposing OpenAI-compatible API with semantic routing, RAG memory, context compression, sandboxed tool execution, self-critique, and JSONL observability. It runs on jahanzaib's RTX 3060 12GB at home.

**User wants:**
1. Rename everything: folder → `ali-kernel`, binary → `ali-kernel`, all refs → "Ali Kernel"
2. Type `ali` to launch a full TUI
3. Elite-level human gate: rich diff/preview approval UI before any tool executes
4. Use case: security code review

**Architecture decision (user confirmed):** Rust + ratatui for TUI, separate binary from gateway, production-grade.

---

## Phase 1: Rename (Ornith → Ali Kernel)

### 1.1 Files to rename / update content

| File | Changes |
|------|---------|
| `~/ornith-stack/` folder | → `~/ali-kernel/` |
| `Cargo.toml` | `name = "ali-kernel"`, binary name |
| `README.md` | All "Ornith Sovereign Gateway" → "Ali Kernel" |
| `STATUS.md`, `ROADMAP.md`, `BUILD.md` | Same replacements |
| `.env.example` + `.env` | `ORNITH_` → `ALI_` prefixes |
| `install-service.sh` | Binary path, service name |
| `ornith-gateway.service` | → `ali-kernel.service` |
| `src/config.rs` | Support `ALI_` + `ORNITH_` (backward compat) |
| `constitutions/AGENT.md` | "You are Ornith" → "You are Ali" |
| `constitutions/OWNER.md` | Ali Kernel references |
| `scripts/live.py` | Port via env |

### 1.2 Env var migration

Add `ALI_GATEWAY_PORT`, `ALI_LLAMA_PORT`, `ALI_LOG_DIR`, `ALI_RAM_MOAT`, `ALI_WORKSPACE_DIR`, `ALI_SYSTEM_PROMPT`, `ALI_NETWORK_TOOLS`, `ALI_ALLOW_UNSANDBOXED`. Keep `ORNITH_*` as aliases.

### 1.3 Binary + service

- Binary: `target/release/ali-kernel`
- Service: `ali-kernel.service`

---

## Phase 2: Gateway — Human Gate API

### 2.1 Current flow

`execute_tools()` in `tools.rs` auto-executes all tool calls synchronously. No pause, no approval.

### 2.2 New flow

```
User → gateway → LLM → tool_calls detected →
  STREAM: tool_call_pending SSE event →
  PAUSE (do NOT execute) →
  WAIT for /v1/approvals →
  APPROVE: execute → continue
  DENY: error to LLM → continue without
```

### 2.3 New endpoints

**`POST /v1/chat/completions`** (modify SSE mode):
- SSE events: `tool_call_pending`, `tool_result`, `human_decision`
- Pause agentic loop when tools detected

**`POST /v1/approvals`** (new):
```json
{ "request_id": "uuid", "tool_index": 0, "action": "approve|deny|modify", "modification": {...} }
```

**`GET /v1/approvals/:request_id`** (new): check status

### 2.4 Risk classification (tools.rs)

| Risk | Tools | Badge |
|------|-------|-------|
| LOW | `read_file`, `crate_search`, `docs_lookup` | green |
| MEDIUM | `bash_exec` (sandboxed), `write_file` | yellow |
| HIGH | `cargo_add`, `cargo_new`, network tools | red |

### 2.5 State

Add `PendingApprovals: Arc<Mutex<HashMap<request_id, PendingState>>>` to `AppState`.

---

## Phase 3: Ali TUI (ratatui)

### 3.1 Cargo workspace

```
ali-kernel/
  Cargo.toml          # workspace: ali-kernel + ali-tui
  src/                # gateway
  ali-tui/
    src/
      main.rs
      ui/
        app.rs        # App + ratatui state
        chat.rs       # Scrollable chat
        input.rs      # Input area
        approval.rs   # Approval cards
        pipeline.rs   # L3-L9 live pane
      gateway.rs      # SSE client
    Cargo.toml
```

### 3.2 Workspace deps

```toml
[workspace.dependencies]
ratatui = "0.28"
crossterm = "0.27"
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["json", "stream"] }
```

### 3.3 Layout

```
┌─────────────────────────────────────────────┐
│  Ali Kernel                    [status bar] │
├──────────────────────────┬──────────────────┤
│   Chat messages          │  Pipeline pane  │
│   (scrollable)          │  L3/L4/L5/L7... │
├──────────────────────────┴──────────────────┤
│  [Approval Card - when tool pending]        │
│  🔴 bash_exec  risk=HIGH  sandbox=bubblewrap│
│  command: rm -rf /tmp/test                  │
│  [a] Approve  [d] Deny  [m] Modify          │
├─────────────────────────────────────────────┤
│  > user input here                    [⏎]  │
└─────────────────────────────────────────────┘
```

### 3.4 Keybindings

| Key | Action |
|-----|--------|
| `Enter` | Send |
| `Ctrl+C` | Quit |
| `a` | Approve once |
| `A` | Approve always (session) |
| `d` | Deny |
| `m` | Modify args |
| `Esc` | Cancel |
| `Ctrl+L` | Toggle pipeline pane |
| `Ctrl+K` | Clear chat |

### 3.5 `ali` launch script

```bash
#!/usr/bin/env bash
set -euo pipefail

# Start llama-server if needed
if ! curl -s http://localhost:8081/health > /dev/null 2>&1; then
  nohup ~/llama.cpp/build/bin/llama-server \
    --port 8081 -ngl 99 --jinja -c 16384 \
    --cache-ram 4096 --host 0.0.0.0 \
    -m ~/ornith-1.0-9b-Q4_K_M.gguf \
    > ~/ali-kernel/logs/llama.log 2>&1 &
  sleep 3
fi

# Start gateway if needed
if ! curl -s http://localhost:9000/health > /dev/null 2>&1; then
  nohup ~/ali-kernel/target/release/ali-kernel \
    > ~/ali-kernel/logs/gateway.log 2>&1 &
  sleep 2
fi

exec ~/ali-kernel/target/release/ali-tui "$@"
```

`~/.bashrc`: `alias ali=/path/to/ali.sh`

---

## Phase 4: Strip Trailing XML (quick win)

`normalize_tool_calls` leaves XML fragment after extracting tool calls. Fix: strip the full `<tool_call>...</tool_call>` block. Also fix `strip_think` for all `<think>` variants. Test: `python3 scripts/live.py "Read Cargo.lock and count the crates"` — should end cleanly.

---

## Implementation Order

```
1. Phase 1: Rename (folder, Cargo.toml, all content, env vars)
   → verify: cargo build --release

2. Phase 2: Gateway human gate API
   → /v1/approvals endpoint, pause on tool_calls, SSE events
   → verify: curl test

3. Phase 3: TUI scaffold
   → Cargo workspace, ratatui layout, chat + pipeline panes
   → verify: renders, connects to /health

4. Phase 3: TUI SSE + approval
   → SSE client, stream tokens, approval cards
   → approve/deny/modify flow, POST to /v1/approvals

5. Phase 3: ali launch script + alias

6. Phase 4: trailing XML fix in tools.rs
```

---

## Open Questions

1. **Tool result display:** Compressed summary only, or full raw with expand option?
2. **Modify flow:** Inline JSON editor in TUI for tool args, or text input?
3. **LLM continue after deny:** Error as tool result? Or system message?
4. **Multiple tool calls at once:** Pause all, show all cards, approve/deny each independently?
5. **Git history:** `git mv` to preserve history, or clean break?
