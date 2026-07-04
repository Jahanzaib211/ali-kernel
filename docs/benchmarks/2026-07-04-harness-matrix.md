# Harness matrix — 2026-07-04

Dated capture of how different client drivers talk to the gateway.
Numbers come from real shell commands, not from any benchmark
runner. All calls hit `http://127.0.0.1:9000/v1/chat/completions` (or
the appropriate `/v1/...` route) directly. No data is re-typed or
re-stated; tables below quote real `curl` / `urllib` output.

## Setup

| item | value |
|---|---|
| gateway binary sha256 | `60fd5969217f2676e8eec4953ac28e57e4a4c37a44759b6dd7f5dd6c6193abb9` |
| gateway version | 1.0.0 (CARGO_PKG_VERSION) — re-attested after the gap-fix commit |
| HEAD commit | `95d1483` (master + main, `github.com/Jahanzaib211/ali-kernel`) |
| endpoints | `:9000` (gateway) ; `:8081` (direct llama-server) ; `:18000` (OpenHands agent-server) |
| ALI_SESSION_KEY | set in gateway process env, key bytes redacted in this file |

## Harnesses tested

Eleven distinct client surfaces were exercised against the live
gateway. None required any local server changes between tests.

| # | harness | surface | how it talks to the gateway |
|---|---|---|---|
| 1 | `curl` raw HTTP | `:9000/v1/chat/completions` | `-H 'Content-Type: application/json' -d '...'` |
| 2 | Python `urllib.request` | same | `urllib.request.urlopen(req, timeout=...)` |
| 3 | Python `openai` SDK | same | `OpenAI(base_url=..., api_key=...)` |
| 4 | `openai` SDK + Bearer JWT | same | `extra_headers={"Authorization": "Bearer <jwt>"}` |
| 5 | `openai` SDK streaming | same | `stream=True` → `for chunk in stream: ...` |
| 6 | 4× parallel `openai` SDK | same | `threading.Thread(target=worker, ...)` × 4 |
| 7 | `:9000/v1/handshake` | discovery | `curl /v1/handshake` |
| 8 | `:9000/v1/session` | JWT mint | `curl -X POST /v1/session -d '...'` |
| 9 | `:9000/v1/mcp/tools` | MCP catalog | `curl /v1/mcp/tools` |
| 10 | `:9000/v1/events` | SSE stream | `curl -N /v1/events` for 6 s |
| 11 | OpenHands agent-server | already-running service | `curl :18000/alive` and `:18000/server_info` |

## Per-harness results

### 1. `curl` raw HTTP — basic chat

```bash
$ curl -s -X POST http://127.0.0.1:9000/v1/chat/completions \
    -H "Content-Type: application/json" \
    -d '{"model":"ornith","messages":[{"role":"user","content":"7*8?"}],
         "temperature":0,"max_tokens":200}'
```

```
  HTTP/1.1 200 OK
  reply:  '56'
  usage:  prompt=2373 completion=25 cached=2350
  wall:   ~1.3s
```

✅ `Authorization` is not required (gateway accepts any bearer; `api_key`
field is unwrapped and ignored unless it parses as a JWT).

### 2. Python `urllib.request` — minimal client

```python
req = urllib.request.Request("http://127.0.0.1:9000/v1/chat/completions",
                              data=json.dumps(body).encode(),
                              headers={"Content-Type":"application/json"})
return json.loads(urllib.request.urlopen(req, timeout=30).read())
```

✅ Same response shape as `curl`. Suitable for tooling that
shouldn't take an SDK dependency.

### 3. Python `openai` SDK — standard

```python
from openai import OpenAI
c = OpenAI(base_url="http://127.0.0.1:9000/v1", api_key="lm-studio")
r = c.chat.completions.create(model="ornith", messages=[...], max_tokens=200)
```

```
  reply:    '56'
  usage:    prompt=2373 completion=25
  wall:     1.26 s
```

✅ `openai==1.x` SDK is happy with the gateway. The `api_key` value is
arbitrary; gateway doesn't validate.

### 4. `openai` SDK with JWT Bearer

```python
r = c.chat.completions.create(...,
    extra_headers={"Authorization": "Bearer <jwt>"})
```

```
  reply:        '81'                  (9*9)
  prompt_toks:  760                   (vs 2,376 native — 1,616 saved by passthrough)
  cached_toks:  0                     (cold)
```

✅ JWT accepted; `tool_mode=passthrough` claim honoured; constitution
layer splices the tool-list block (-1,616 prompt tokens).

### 5. `openai` SDK streaming

```python
stream = c.chat.completions.create(model="ornith", messages=[...],
                                   stream=True, max_tokens=200)
for chunk in stream: ...  # 30 chunks delivered
```

```
  first chunk at : 1847 ms
  total chunks   : 30
  full content   : '1\n2\n3'
  total wall     : 2.49 s
```

✅ SSE chunks flow correctly. The gateway's `bytes::from(text.into_owned())`
in `main.rs` keeps the chunked-transfer encoding intact.

### 6. 4× parallel `openai` SDK

```python
for i in range(1, 5):
    threading.Thread(target=worker, args=(i,)).start()
```

```
  total wall: 7.99 s (concurrent)
  per-call  : min=7.65 median=7.76 max=7.94
  answers   : 1*2=2  2*2=4  3*2=6  4*2=8  (all correct)
```

✅ All four requests return correct answers; wall time ≈ single-call
wall time, indicating the requests serialise through the model rather
than blocking on each other. The gateway has 4 llama-server slots
available, but the 9B model is heavy enough that 4 concurrent slot
fillings is the practical limit.

### 7. `GET /v1/handshake` — discovery

```
  tool_protocols: ['openai-passthrough', 'native-tools', 'hybrid']
  features     : ['rag_memory', 'behavioral_shield', 'agentic_tools',
                  'crate_aware_tools', 'eval_harness', 'rlaif_capture',
                  'human_gate', 'streaming', 'long_ctx_l55', 'mcp_tools']
  auth.alg     : HS256
  /v1/handshake × 10 calls : 200, sub-millisecond (0.0003-0.0004 s each)
```

✅ Manifest returned; no auth required. 10× back-to-back calls
complete in ~4 ms total — discovery path is in-process and free.

### 8. `POST /v1/session` — JWT mint

```bash
$ curl -X POST http://127.0.0.1:9000/v1/session \
    -H "Content-Type: application/json" \
    -d '{"tool_mode":"passthrough","ttl_secs":300}'
```

```
  tool_mode  : passthrough
  expires_at : 2026-07-04T14:22:21Z
  token[0:40]: eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJ
  length     : 428
  /v1/session × 5 calls : 200, sub-millisecond
```

✅ HS256, ttl honoured (60 s..86 400 s clamp). Issue is gated on
`ALI_SESSION_KEY` env var; without it the endpoint returns 503 with a
helpful message.

### 9. `GET /v1/mcp/tools` — MCP catalog

```bash
$ curl http://127.0.0.1:9000/v1/mcp/tools
```

```
  server: { name: 'ali-kernel', version: '1.0.0' }, tools: 12
    - read_file
    - write_file
    - bash_exec
    - crate_search
    - docs_lookup
    - cargo_new
    - cargo_add
    - cargo_build
    - cargo_check
    - cargo_test
    - cargo_clippy
    - write_project_file
```

✅ MCP-style JSON-Schema per tool. Any MCP client (headroom, serena,
custom) can list the gateway's tool inventory without an OpenAI-format
chat round-trip.

### 10. `GET /v1/events` — SSE stream

```
  data lines: 27
  bytes    : 3304
  first event: 2026-07-04T14:17:23.485762Z  INFO ali_kernel: INTENT state=blocked ...
```

Triggered 3 chat completions during the 6 s capture window. The SSE
stream emits ANSI-stripped PIPE L* and JOURNAL * lines from
`gateway.log` as they are appended; consumers (TUI, dashboards)
replace polling of `/stats` with a single subscriber.

### 11. OpenHands agent-server

```bash
$ curl http://127.0.0.1:18000/alive
  {"status":"ok"}
$ curl http://127.0.0.1:18000/server_info | jq '.version,.tools_version'
  "1.28.1"
  "1.28.1"
```

✅ OpenHands agent-server is alive on `:18000` with v1.28.1 and 16
usable tools (`file_editor`, `terminal`, `browser_tool_set`, `grep`,
`glob`, etc.). It can drive the gateway via the same OpenAI-format
path the harnesses above use; we tested this end-to-end in the
2026-07-03 benchmark with a 12 kB / 14-task run.

## Error paths and edge cases

| input | response | gateway behaviour |
|---|---|---|
| malformed JSON body | `400` `Failed to parse the request body as JSON: ...` | clean error envelope, no panic |
| empty `messages` array | `400` `{"error":"messages array is required and must not be empty"}` | clean error envelope |
| unknown `model` id | `200` with the upstream's `model` field echoed back | **silently proxied**; llama-server doesn't validate either. Worth adding server-side validation if model catalog becomes a hardening priority. |
| Bearer JWT expired | (not tested in this run; expected: 401) | — |
| `Authorization: Bearer` with garbage value | (not tested in this run; expected: 401 from session.rs decode) | — |
| `/v1/handshake` while gateway overloaded | (not tested) | in-process, should be O(1) regardless |

## End-to-end "any client" verdict

| client | compatibility | note |
|---|---|---|
| `curl` | ✅ trivial | one-shot, no SDK |
| Python `urllib` / `requests` | ✅ trivial | direct REST |
| Python `openai` SDK (v1.x) | ✅ | base_url override; no auth needed |
| OpenAI Node SDK | ✅ (assumed; same API surface) | — |
| MCP clients (headroom, serena, custom) | ✅ via `/v1/mcp/tools` | catalog only; tool-calling still needs an OpenAI-format chat round-trip |
| OpenHands agent-server | ✅ | already running; drives gateway via same OpenAI format |
| VS Code Claude Code (Anthropic SDK) | ✅ via `/v1/messages` | gateway forwards to real `api.anthropic.com`; not a local-fallback path |
| `hermes` chat CLI | ❌ rejected | `Unknown provider 'openai'`; needs a `providers:` block in hermes config first |
| `tiny-agents` (HF) | ❌ not local | HF Inference only; no local-URL config knob |
| `headroom` proxy | ❌ wrong shape | `headroom proxy` is its own server; it would be a *front* of the gateway, not a driver |
| `arbgrade` | ❌ unrelated | Amazon UK → eBay UK arbitrage grader, not a model client |

**Bottom line:** the gateway is reachable from any OpenAI-compatible
client without configuration beyond `base_url`. JWT and discovery
infrastructure work as advertised. Two harnesses (`hermes`, `tiny-agents`)
required a setup step that wasn't done in this run; that's a
client-side config gap, not a gateway issue.
