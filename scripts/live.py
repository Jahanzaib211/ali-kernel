#!/usr/bin/env python3
"""
Live terminal view of the Ornith Sovereign Gateway processing a request.

Animated pipeline with status toggles. In a real terminal you get spinners and
in-place updates; piped / non-TTY falls back to clean step-by-step lines.

    python3 scripts/live.py "Write a haiku about Rust"
"""
import json
import sys
import threading
import time
import urllib.request
from pathlib import Path

GW = "http://127.0.0.1:9000"
ROOT = Path(__file__).resolve().parent.parent
TTY = sys.stdout.isatty()
SPIN = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"


def col(s, code):
    return f"\033[{code}m{s}\033[0m" if TTY else str(s)


# Pipeline layers. L6 (brain) is where the real time goes; the rest are <1ms.
LAYERS = [
    ("L0", "Config & env"),
    ("L1", "Binary attest"),
    ("L2", "Constitution"),
    ("L3", "Semantic route"),
    ("L4", "Memory (RAG)"),
    ("L5", "Context assemble"),
    ("L5.5", "Compress"),
    ("L6", "Brain · Ornith"),
    ("L7", "Tools"),
    ("L8", "Critique"),
    ("L9", "Observe"),
]


def get(url, timeout=5):
    return json.loads(urllib.request.urlopen(url, timeout=timeout).read())


def toggle(on):
    return col("● on", "32") if on else col("○ off", "2")


def header(h):
    caps = h.get("capabilities", [])
    bk = h.get("backend", "?")
    print(col("┏━ ORNITH SOVEREIGN GATEWAY ━━━━━━━━━━━━━━━━━━━━━━━━━━━┓", "36"))
    print("  backend   " + col(bk, "32" if bk == "healthy" else "31")
          + f"  · {h.get('model')}  @ {h.get('backend_url')}")
    print(f"  shield {toggle('behavioral_shield' in caps)}   "
          f"tools {toggle('agentic_tools' in caps)}   "
          f"crate-aware {toggle('crate_aware_tools' in caps)}")
    print(f"  compress {toggle(True)}   "
          f"eval {toggle('eval_harness' in caps)}   "
          f"rlaif {toggle('rlaif_capture' in caps)}")
    print(col("┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛", "36"))


def render(states, frame):
    lines = []
    for (tag, name), st in zip(LAYERS, states):
        if st == "done":
            mark = col("✓", "32")
        elif st == "active":
            mark = col(SPIN[frame % len(SPIN)], "33")
        else:
            mark = col("○", "2")
        label = name if st != "pending" else col(name, "2")
        lines.append(f"  {mark} {tag:<5} {label}")
    return "\n".join(lines)


def chat(prompt, box):
    body = {"model": "ornith", "max_tokens": 300,
            "messages": [{"role": "user", "content": prompt}]}
    req = urllib.request.Request(
        GW + "/v1/chat/completions",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    try:
        box["resp"] = json.loads(urllib.request.urlopen(req, timeout=120).read())
    except Exception as e:  # noqa: BLE001
        box["err"] = str(e)


def last_compression():
    log = ROOT / "logs" / "gateway.log"
    if not log.exists():
        return None
    for line in reversed(log.read_text(errors="ignore").splitlines()):
        if "L5.5 compressed" in line:
            return line.split("L5.5 compressed", 1)[1].strip()
    return None


def main():
    prompt = " ".join(sys.argv[1:]) or "Write a haiku about Rust ownership."
    try:
        h = get(GW + "/health")
    except Exception as e:  # noqa: BLE001
        print(f"gateway unreachable at {GW}: {e}")
        return 1

    header(h)
    print(col(f"  ▶ prompt: {prompt}", "1"))
    print()

    n = len(LAYERS)
    states = ["pending"] * n
    box = {}
    t0 = time.time()
    worker = threading.Thread(target=chat, args=(prompt, box))
    worker.start()

    if TTY:
        sys.stdout.write("\033[?25l")  # hide cursor
    print(render(states, 0))

    def redraw(frame):
        if TTY:
            sys.stdout.write(f"\033[{n}A")  # cursor up n lines
            sys.stdout.write(render(states, frame) + "\n")
            sys.stdout.flush()
        # non-TTY: lines are printed as layers complete (below)

    frame = 0
    brain_idx = next(i for i, l in enumerate(LAYERS) if l[0] == "L6")
    # quick sweep L0..L5.5 (these really are sub-ms)
    for i in range(brain_idx):
        states[i] = "active"
        if not TTY:
            print(f"  {col('✓','32')} {LAYERS[i][0]:<5} {LAYERS[i][1]}")
        else:
            redraw(frame)
            time.sleep(0.12)
        states[i] = "done"
    # hold on L6 while inference really runs
    states[brain_idx] = "active"
    if not TTY:
        sys.stdout.write(f"  {col('◐','33')} L6    Brain · Ornith (inferring")
        sys.stdout.flush()
    while worker.is_alive():
        frame += 1
        if TTY:
            redraw(frame)
        else:
            sys.stdout.write(".")
            sys.stdout.flush()
        time.sleep(0.2)
    worker.join()
    states[brain_idx] = "done"
    if not TTY:
        print(")")

    resp = box.get("resp", {})
    tool = bool(resp.get("choices", [{}])[0].get("message", {}).get("tool_calls")) if resp else False
    # remaining layers reflect the real response
    for i in range(brain_idx + 1, n):
        states[i] = "done"
        if not TTY:
            note = ""
            if LAYERS[i][0] == "L7" and not tool:
                note = col(" (no tool call)", "2")
            print(f"  {col('✓','32')} {LAYERS[i][0]:<5} {LAYERS[i][1]}{note}")
    if TTY:
        redraw(frame)
        sys.stdout.write("\033[?25h")  # show cursor

    dt = time.time() - t0
    print()
    if "err" in box:
        print(col(f"  ✗ error: {box['err']}", "31"))
        return 1
    msg = resp["choices"][0]["message"].get("content") or ""
    usage = resp.get("usage", {})
    comp = last_compression()
    print(col("  ── result ──────────────────────────────────────────", "36"))
    for line in msg.strip().splitlines()[:8]:
        print("  " + line)
    print()
    print(f"  {col('tokens','1')}  in {usage.get('prompt_tokens','?')}  ·  "
          f"out {usage.get('completion_tokens','?')}   "
          f"{col('latency','1')} {dt:.1f}s   "
          f"{col('model','1')} {str(resp.get('model','ornith')).split('/')[-1]}")
    if comp:
        print(f"  {col('L5.5','33')}  last compression: {comp}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
