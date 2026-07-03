# Benchmarks

Real, dated, reproducible captures of this binary running locally.
Numbers in these reports come from `harness.py` with no human fudging.

## Layout

| file | purpose |
|---|---|
| `harness.py` | single-file self-contained runner: 14 tasks across 6 languages, 6 roles, 6 prompt styles; per-task wall-time, token counts, pass-rate |
| `YYYY-MM-DD-<slug>.md` | dated report comparing endpoints (gateway vs direct llama-server) |

## Latest

[`2026-07-03-gateway-vs-direct.md`](./2026-07-03-gateway-vs-direct.md) — gateway `:9000`
vs direct `:8081`, 14 tasks, raw + per-language + per-role + per-style
breakdowns.

## Reproducing

```bash
# 1. Make sure both endpoints are up
curl -sf http://127.0.0.1:8081/health >/dev/null && echo "llama-server OK"
curl -sf http://127.0.0.1:9000/health >/dev/null && echo "gateway OK"

# 2. Warmup (prompt cache needs to be hot on the gateway)
python3 harness.py "gateway :9000" >/dev/null
python3 harness.py "direct :8081"  >/dev/null

# 3. Reported run
python3 harness.py "gateway :9000" | tee gateway.out
python3 harness.py "direct :8081"  | tee direct.out
```

## File-naming

`YYYY-MM-DD-<slug>.md` where `<slug>` is a short kebab-cased phrase naming
the comparison (e.g. `gateway-vs-direct`, `gateway-passthrough-native`).
