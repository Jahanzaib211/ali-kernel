# Watching Ali Kernel Compress Your Tokens (plain-English guide)

## TL;DR

- The gateway now sits **between Claude Code and Anthropic**.
- It shrinks big chunks of text (your CLAUDE.md, long tool output, big messages) *before* forwarding to `api.anthropic.com`.
- Your API key is never touched or stored by the gateway — it just passes through.
- One command shows you every squeeze, live, as it happens:

```bash
tail -f ~/ali-kernel/logs/gateway.log | grep --line-buffered "L5.5 compressed"
```

Leave that running in a spare terminal while you work. Every line means "the gateway just saved you tokens."

---

## What's actually happening

```
Claude Code  ──────────►  Ali Kernel (127.0.0.1:9000)  ──────────►  api.anthropic.com
                              │
                          shrinks big text here
                          (system prompt, tool
                           output, long messages)
```

Two separate doors on the same gateway:

| Door | Used by | Talks to |
|---|---|---|
| `/v1/chat/completions` | your local Ornith model / TUI | `llama-server` on your GPU |
| `/v1/messages` | **Claude Code (this tool)** | real Anthropic API |

Both doors share the same squeezing logic (`compress.rs`). One gateway, two models, one compressor.

## What gets squeezed

Only the big stuff — small requests pass through untouched (0% saved is normal and fine):

- **System prompt** — if it's over ~8,000 characters (your CLAUDE.md rule files, project instructions), it gets compressed.
- **Message content / tool results** — if any single message is over ~4,000 characters (a big file read, a long command output), it gets compressed.

The squeeze itself: strips ANSI color codes, collapses repeated blank lines, then smart-truncates to fit the budget while keeping the important parts (start + end, not just a hard cutoff).

## How to see it live

1. Make sure the gateway is running:
   ```bash
   curl http://127.0.0.1:9000/health
   ```
2. In a spare terminal, tail the log filtered to just compression events:
   ```bash
   tail -f ~/ali-kernel/logs/gateway.log | grep --line-buffered "L5.5 compressed"
   ```
3. Use Claude Code normally in another terminal. Every time a big prompt or tool result goes through, you'll see a line like:
   ```
   PIPE L5.5 compressed: 51200 → 8000 chars (-84%) [claude-proxy]
   ```
   That's before → after size, and the percentage saved.

If you want the full animated per-layer view (routing, memory, compress, tools, critique) for the **local Ornith model** specifically, there's already a dashboard for that:
```bash
python3 scripts/live.py "your prompt here"
```

## Turning it on / off

Claude Code reads `ANTHROPIC_BASE_URL` once, at startup — it can't be flipped mid-session.

- **On** (already set in `~/.bashrc`):
  ```bash
  export ANTHROPIC_BASE_URL="http://127.0.0.1:9000"
  ```
  Restart `claude` for a new session to pick it up.
- **Off** — comment out that line in `~/.bashrc`, open a new terminal, restart `claude`. It'll talk to Anthropic directly again.

## Why this is safe

- The gateway is bound to `127.0.0.1` only (loopback) — nothing on your network can reach it.
- Auth passthrough: whatever `x-api-key` / `Authorization` header Claude Code sends, the gateway forwards unchanged. It never logs or persists it.
- Binary attestation (L1): if the gateway binary changes on disk, it refuses to run until you clear `/tmp/ali-ram-moat` and let it reseal — so it can't be silently swapped out from under you.

## When you rebuild the gateway

Rebuilding changes the binary's hash, which trips the attestation check on next start:

```bash
cargo build --release
rm -rf /tmp/ali-ram-moat   # let it reseal against the new binary
# then restart the gateway
```
