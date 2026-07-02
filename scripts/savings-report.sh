#!/usr/bin/env bash
# Weekly savings snapshot — appends one dated row to reports/SAVINGS-LOG.md.
# Run from cron (Mondays) or by hand. Silently exits if the gateway is down.
cd "$(dirname "$0")/.." || exit 1
mkdir -p reports
s=$(curl -sf --max-time 5 http://127.0.0.1:9000/stats) || exit 0

if [ ! -f reports/SAVINGS-LOG.md ]; then
  cat > reports/SAVINGS-LOG.md <<'HDR'
# Ali Kernel — Weekly Savings Log

Appended automatically by `scripts/savings-report.sh`. "API value" prices the
billed tokens at Anthropic list rates; on subscription (OAuth) traffic it is
plan-limit value consumed, not dollars billed. Savings are labeled estimates
(see PROOF.md).

| date (UTC) | requests (all) | compression | 7d api value | 7d est. saved | all-time api value | all-time est. saved |
|---|---|---|---|---|---|---|
HDR
fi

printf '%s' "$s" | jq -r '
  "| \(now | strftime("%Y-%m-%d")) | \(.all_time.requests) | \(.all_time.compression_pct)% | $\(.last_7_days.api_value_usd) | $\(.last_7_days.estimated_savings_usd) | $\(.all_time.api_value_usd) | $\(.all_time.estimated_savings_usd) |"
' >> reports/SAVINGS-LOG.md
