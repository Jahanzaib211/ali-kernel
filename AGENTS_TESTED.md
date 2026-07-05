# Agent CLI Compatibility (gateway smoke-tested)

_Last updated: 2026-07-05._

This file lists the lightweight coding-agent CLIs that have been smoke-tested
against the gateway on `127.0.0.1:9000`. Each entry shows the install path
and the wiring needed to point the agent at the gateway instead of its
default cloud provider.

The gateway already exposes `/v1/chat/completions`, `/v1/messages`, `/v1/models`,
and `/health`, so most OpenAI- or Anthropic-compatible clients need only a
single base-URL swap.

## Aider ✓ smoke PASS

- **Install**: `uv tool install aider-chat` (venv-isolated, one-shot)
- **Wiring**: `OPENAI_API_BASE=http://127.0.0.1:9000/v1 OPENAI_API_KEY=sk-noauth aider --model openai/<model-id> --no-git --no-auto-commits --message "..."` (replace `<model-id>` with whatever the gateway advertises in `/v1/models`)
- **Notes**: Aider is a litellm-fronted wrapper. The provider prefix `openai/`
  in the model id tells litellm to use the OpenAI request shape; the gateway
  sees a vanilla `/v1/chat/completions` call. Bonus: Aider carries its own
  in-process git-checkpoint loop, so it's a clean testbed for multi-turn
  coding-agent workflows.
- **Caveat**: the gateway's `/health` endpoint is OpenAI-incompatible to
  litellm; Aider will warn about a "down provider" before settling on its
  first real call. Suppress with `--no-show-model-warnings`.

## OpenCode CLI ⚠ partial

- **Install**: already at `/snap/bin/opencode` (system snap)
- **Wiring**: the default `minimax-coding-plan` provider works out of the box
  but routes to a cloud model, NOT the gateway. To redirect, add a custom
  `provider` block to `~/.config/opencode/opencode.jsonc`:
  ```jsonc
  {
    "provider": {
      "ali-kernel-gateway": {
        "npm": "@ai-sdk/openai-compatible",
        "options": {
          "baseURL": "http://127.0.0.1:9000/v1"
        },
        "models": {
          "ornith": {}
        }
      }
    }
  }
  ```
  Then invoke with `opencode run -m ali-kernel-gateway/<model-id> "your prompt"`.
- **Caveat**: requires a config-file edit, not just env vars.

## Continue CLI ⚠ partial

- **Install**: `bunx @continuedev/cli` (resolves the `cn` binary)
- **Wiring**: needs `~/.continue/config.json` with an OpenAI-compatible
  provider entry pointing at `http://127.0.0.1:9000/v1` and `model: <model-id>`
  (whatever the gateway advertises in `/v1/models`).
  The `-p` flag accepts a prompt for non-interactive use.
- **Caveat**: bunx-cached binary lives in `/tmp/bunx-*`; not on `$PATH` by
  default. Env-var base-URL redirection NOT supported — config-file edit is
  mandatory.

## Hermes CLI ⚠ partial

- **Install**: already at `/home/jahanzaib/.local/bin/hermes` (wrapper to a
  venv-installed Python CLI)
- **Wiring**: `~/.hermes/config.yaml` has an empty `providers:` block. Needs
  a populated block for an OpenAI-compatible provider. The `proxy` subcommand
  is for OAuth providers and can't be redirected at the gateway.
- **Caveat**: the CLI defaults to `anthropic`; without a `providers:` entry
  for the gateway, all chat invocations hit the cloud. Single-pass
  compatibility is fine; two-pass routing requires restarting with the
  refreshed config.

## OpenHands (SDK only) ⚠ no CLI

- **Status**: `uvx openhands` does not ship an executable. OpenHands is
  exposed as a Python SDK (`openhands.sdk`) plus an `agent-server` HTTP
  frontend. To drive it against the gateway, write a small Python script
  using `LLM(base_url="http://127.0.0.1:9000/v1", model="ornith", api_key="sk-noauth")`
  and the SDK's event loop.
- **Caveat**: heavy agent framework (full Conversation/Action loop). Best
  choice for long-running autonomous workflows, not for one-shot
  benchmarking.

## Notes for new agents

- The gateway **does not require** an API key on `/v1/chat/completions` or
  `/v1/messages`. Any bearer token (including `sk-noauth`) is forwarded
  unchanged and never logged.
- Loopback only: `127.0.0.1` binding is a hard invariant per CLAUDE.md.
  Never bind the gateway to `0.0.0.0`.
- The model id literal in the gateway's `/v1/models` response is the
  identifier chosen at deploy time. The tool-calling layer toggles between
  `native-tools`, `openai-passthrough`, and `hybrid` per the request header
  `X-Ali-Tool-Mode` (see `handshake.rs` for the manifest). Most coding
  agents work best with `openai-passthrough`.
- Two-pass think-then-answer dispatch (`src/passes.rs`) is wired as Rust
  primitives; main.rs call-site integration is v3.1 — gating env var will
  be `ALI_INTENT_ROUTING=1`.
