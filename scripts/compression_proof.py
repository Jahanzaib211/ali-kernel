#!/usr/bin/env python3
"""Prints real evidence of Ali Kernel's L5.5 compression on the Claude proxy path.

Reads logs/gateway.log directly — this is ground truth, unlike asking the
model "is compression working" (the model has no visibility into the proxy
layer; compression happens on the HTTP request before Anthropic ever sees it).
"""
import re
import sys
import urllib.request
from pathlib import Path

GATEWAY_URL = "http://127.0.0.1:9000"
LOG_PATH = Path(__file__).resolve().parent.parent / "logs" / "gateway.log"

EVENT_RE = re.compile(r"compressed: (\d+) . (\d+) chars \(-(\d+)%\) \[claude-proxy\]")
ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")


def main() -> int:
    try:
        urllib.request.urlopen(GATEWAY_URL + "/health", timeout=3)
        print(f"gateway: healthy @ {GATEWAY_URL}")
    except Exception as e:  # noqa: BLE001
        print(f"gateway: UNREACHABLE @ {GATEWAY_URL} ({e})")
        return 1

    if not LOG_PATH.exists():
        print(f"no log at {LOG_PATH} — gateway hasn't handled any request yet")
        return 0

    events = []
    for raw_line in LOG_PATH.read_text(errors="ignore").splitlines():
        if "[claude-proxy]" not in raw_line:
            continue
        line = ANSI_RE.sub("", raw_line)
        m = EVENT_RE.search(line)
        if m:
            ts = line.split()[0] if line else ""
            events.append((ts, int(m.group(1)), int(m.group(2)), int(m.group(3))))

    if not events:
        print("no claude-proxy events logged yet.")
        print("either ANTHROPIC_BASE_URL isn't set for this session, or nothing has")
        print("gone through the gateway yet. Check: env | grep ANTHROPIC_BASE_URL")
        return 0

    total_before = sum(e[1] for e in events)
    total_after = sum(e[2] for e in events)
    saved = total_before - total_after
    pct_saved = round(saved / total_before * 100) if total_before else 0
    squeezed = [e for e in events if e[3] > 0]

    print(f"requests through gateway:     {len(events)}")
    print(f"requests actually compressed: {len(squeezed)} (rest were under budget, -0% is correct)")
    print(f"total bytes:                  {total_before} -> {total_after}  (saved {saved} chars, -{pct_saved}% overall)")
    print()
    print("last 5 events:")
    for ts, before, after, pct in events[-5:]:
        print(f"  {ts}  {before} -> {after} chars  (-{pct}%)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
