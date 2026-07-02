#!/usr/bin/env bash
# Claude Code statusLine — live Ali Kernel metrics inside the Claude Code terminal.
# Wired in ~/.claude/settings.json as: "statusLine": {"type":"command","command":"~/ali-kernel/scripts/claude-statusline.sh"}
# Claude Code pipes session JSON on stdin; stdout becomes the status line.
# Must be fast and never hang: gateway probe capped at 400ms, silent fallback.
#
# Budgets for the bars (API-value USD). Anthropic doesn't expose plan quotas,
# so the subscription bar is against a tunable weekly estimate.
SUB_WEEK_BUDGET="${ALI_SUB_WEEK_BUDGET:-300}"
KEY_MONTH_BUDGET="${ALI_KEY_MONTH_BUDGET:-25}"

input=$(cat)
model=$(printf '%s' "$input" | jq -r '.model.display_name // empty' 2>/dev/null)

bar() { # bar <used> <budget> -> [████░░░░░░]
  local pct filled i out=""
  pct=$(awk -v u="$1" -v b="$2" 'BEGIN { if (b <= 0) b = 1; p = u * 100 / b; if (p > 100) p = 100; printf "%d", p }')
  filled=$((pct / 10))
  for ((i = 0; i < 10; i++)); do
    if ((i < filled)); then out+="█"; else out+="░"; fi
  done
  printf '%s %d%%' "$out" "$pct"
}

stats=$(curl -sf --max-time 0.4 http://127.0.0.1:9000/stats 2>/dev/null)
if [ -n "$stats" ]; then
  read -r comp value saved req sub7 key <<<"$(printf '%s' "$stats" | jq -r '
    [.today.compression_pct, .today.api_value_usd, .today.estimated_savings_usd,
     .today.requests, .last_7_days.subscription_value_usd, .all_time.api_key_value_usd]
    | map(tostring) | join(" ")' 2>/dev/null)"
  subbar=$(bar "${sub7:-0}" "$SUB_WEEK_BUDGET")
  keybar=$(bar "${key:-0}" "$KEY_MONTH_BUDGET")
  printf '%s⚡ ali-kernel L5.5 -%s%% · $%.2f value · $%.2f saved · %s req │ sub %s │ credits %s\n' \
    "${model:+$model · }" "${comp:-0}" "${value:-0}" "${saved:-0}" "${req:-0}" "$subbar" "$keybar"
else
  printf '%s○ ali-kernel offline (direct to Anthropic)\n' "${model:+$model · }"
fi
