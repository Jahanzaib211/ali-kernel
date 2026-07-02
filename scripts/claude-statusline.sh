#!/usr/bin/env bash
# Claude Code statusLine — live Ali Kernel savings inside the Claude Code terminal.
# Wired in ~/.claude/settings.json as: "statusLine": {"type":"command","command":"~/ali-kernel/scripts/claude-statusline.sh"}
# Claude Code pipes session JSON on stdin; stdout becomes the status line.
# Must be fast and never hang: gateway probe capped at 400ms, silent fallback.

input=$(cat)
model=$(printf '%s' "$input" | jq -r '.model.display_name // empty' 2>/dev/null)

stats=$(curl -sf --max-time 0.4 http://127.0.0.1:9000/stats 2>/dev/null)
if [ -n "$stats" ]; then
  line=$(printf '%s' "$stats" | jq -r '
    def h: if . >= 1000000 then "\((. / 100000 | round) / 10)M"
           elif . >= 1000 then "\((. / 100 | round) / 10)K"
           else "\(.)" end;
    .today |
    "L5.5 -\(.compression_pct)%"
    + " · $\((.actual_spend_usd * 100 | round) / 100) spent"
    + " · $\((.estimated_savings_usd * 100 | round) / 100) saved"
    + " · in \(.input_tokens | h)/cached \(.cache_read_input_tokens | h)/out \(.output_tokens | h)"
    + " · \(.requests) req"' 2>/dev/null)
  printf '%s⚡ ali-kernel %s\n' "${model:+$model · }" "${line:-stats parse error}"
else
  printf '%s○ ali-kernel offline (direct to Anthropic)\n' "${model:+$model · }"
fi
