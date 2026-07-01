---
name: ali-compression-proof
description: Check whether the Ali Kernel gateway is actively compressing this Claude Code session's traffic and report real before/after byte counts as proof. Use whenever the user asks "is compression working", "what's the proof", "how much are we saving", or anything about Ali Kernel / gateway token savings.
---

# Ali Kernel — Compression Proof

The Ali Kernel gateway can sit between Claude Code and `api.anthropic.com`
(active when `ANTHROPIC_BASE_URL=http://127.0.0.1:9000` is set for this
session) and shrinks oversized system prompts / messages before forwarding.

**Hard rule: never answer "is it working" from self-report or vibes.** The
model has zero visibility into the proxy layer — compression happens on the
HTTP request before Anthropic ever sees it, so introspection is not a valid
source of evidence here. Only the gateway's own log is ground truth.

## What to do when invoked

1. Run the bundled proof script and report its output, numbers included —
   don't paraphrase away the actual byte counts:
   ```bash
   python3 /home/jahanzaib/ali-kernel/scripts/compression_proof.py
   ```

2. If it prints `gateway: UNREACHABLE` — the gateway isn't running. Say so
   plainly; don't guess or fabricate stats. Suggest checking
   `curl http://127.0.0.1:9000/health`.

3. If it prints "no claude-proxy events logged yet" — either
   `ANTHROPIC_BASE_URL` isn't set for the current session, or nothing has
   gone through the gateway yet. Check with
   `env | grep ANTHROPIC_BASE_URL` (safe — only reveals the URL, never an
   API key) and suggest restarting `claude` with it exported.

4. Present the real before/after byte counts and % saved from the script
   output. Requests showing `-0%` are correct, not a failure — they were
   simply under the compression budget (system prompt < ~8000 chars,
   message < ~4000 chars) and passed through untouched by design.

5. For a live, continuous view instead of a one-shot check, tell the user
   about:
   ```bash
   tail -f /home/jahanzaib/ali-kernel/logs/gateway.log | grep --line-buffered "L5.5 compressed"
   ```
