#!/usr/bin/env python3
"""
Live terminal view of the Ali Kernel — shows the REAL per-layer
activity as it happens, by tailing the gateway's structured log during a request.

It sends a tool-capable prompt so you see routing, memory, L5.5 compression
(with before→after byte counts), tool execution, critique, and tokens — live.

    python3 scripts/live.py "Read Cargo.lock and count the crates"

Real terminal → animated spinners + in-place updates. Piped → step-by-step lines.
"""
import json
import os
import queue
import re
import sys
import threading
import time
import urllib.request
from pathlib import Path

GW = os.environ.get("ALI_GATEWAY_URL", "http://127.0.0.1:9000")
ROOT = Path(__file__).resolve().parent.parent
LOG = ROOT / "logs" / "gateway.log"
TTY = sys.stdout.isatty()
SPIN = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"

LAYERS = [
    ("L0", "Config & env"), ("L1", "Binary attest"), ("L2", "Constitution"),
    ("L3", "Semantic route"), ("L4", "Memory (RAG)"), ("L5", "Context assemble"),
    ("L5.5", "Compress"), ("L6", "Brain · Ornith"), ("L7", "Tools"),
    ("L8", "Critique"), ("L9", "Observe"),
]
IDX = {tag: i for i, (tag, _) in enumerate(LAYERS)}

TOOLS = [
    {"type": "function", "function": {"name": "read_file", "parameters": {
        "type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}}},
    {"type": "function", "function": {"name": "bash_exec", "parameters": {
        "type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}}},
    {"type": "function", "function": {"name": "crate_search", "parameters": {
        "type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}}},
]


def col(s, code):
    return f"\033[{code}m{s}\033[0m" if TTY else str(s)


def get(url, timeout=5):
    return json.loads(urllib.request.urlopen(url, timeout=timeout).read())


def parse(line):
    """Map a gateway log line to (layer_tag, detail) or None."""
    def after(marker):
        return line.split(marker, 1)[1].strip()
    if "PIPE L3 " in line:
        return "L3", after("PIPE L3 ")
    if "PIPE L4 " in line:
        return "L4", after("PIPE L4 ")
    if "PIPE L5 " in line:
        return "L5", after("PIPE L5 ")
    if "L5.5 compressed" in line:
        seg = after("L5.5 compressed")
        nums = re.findall(r"\d+", seg)
        if len(nums) >= 2:
            a, b = int(nums[0]), int(nums[1])
            pct = 100 - round(b / a * 100) if a else 0
            who = seg.split(" output")[0]
            return "L5.5", f"{who}: {a} → {b} chars  (-{pct}%)"
        return "L5.5", seg
    if "Tool executed:" in line:
        return "L7", after("Tool executed:")
    if "PIPE L8 " in line:
        return "L8", after("PIPE L8 ")
    if "PIPE L9 " in line:
        return "L9", after("PIPE L9 ")
    return None


def tailer(start_pos, q, stop):
    try:
        f = open(LOG, "r", errors="ignore")
    except FileNotFoundError:
        return
    f.seek(start_pos)
    while not stop.is_set():
        line = f.readline()
        if line:
            ev = parse(line)
            if ev:
                q.put(ev)
        else:
            time.sleep(0.04)
    for line in f:  # final drain
        ev = parse(line)
        if ev:
            q.put(ev)


def chat(prompt, box):
    body = {"model": "ornith", "max_tokens": 400, "tools": TOOLS,
            "messages": [{"role": "user", "content": prompt}]}
    req = urllib.request.Request(GW + "/v1/chat/completions",
                                 data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    try:
        box["resp"] = json.loads(urllib.request.urlopen(req, timeout=180).read())
    except Exception as e:  # noqa: BLE001
        box["err"] = str(e)


def header(h):
    caps = h.get("capabilities", [])
    tog = lambda on: col("● on", "32") if on else col("○ off", "2")  # noqa: E731
    print(col("┏━ ALI KERNEL ━ live pipeline ━━━━━━━━━━━━━━━━━━━━━━━━━━┓", "36"))
    print("  backend  " + col(h.get("backend", "?"), "32") + f"  · {h.get('model')}  @ {h.get('backend_url')}")
    print(f"  shield {tog('behavioral_shield' in caps)}  tools {tog('agentic_tools' in caps)}  "
          f"crate {tog('crate_aware_tools' in caps)}  compress {tog(True)}  rlaif {tog('rlaif_capture' in caps)}")
    print(col("┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛", "36"))


def line_for(i, states, details, frame):
    tag, name = LAYERS[i]
    st = states[i]
    mark = {"done": col("✓", "32"), "active": col(SPIN[frame % len(SPIN)], "33")}.get(st, col("○", "2"))
    det = col("  " + details[tag], "2") if details.get(tag) else ""
    label = name if st != "pending" else col(name, "2")
    return f"  {mark} {tag:<5} {label}{det}"


def main():
    prompt = " ".join(sys.argv[1:]) or \
        "Read the file /home/jahanzaib/ali-kernel/Cargo.lock and tell me how many crates are listed."
    try:
        h = get(GW + "/health")
    except Exception as e:  # noqa: BLE001
        print(f"gateway unreachable at {GW}: {e}")
        return 1

    header(h)
    print(col(f"  ▶ {prompt}", "1"))
    print()

    n = len(LAYERS)
    states = ["pending"] * n
    details = {}
    start_pos = LOG.stat().st_size if LOG.exists() else 0
    q, stop, box = queue.Queue(), threading.Event(), {}
    threading.Thread(target=tailer, args=(start_pos, q, stop), daemon=True).start()
    worker = threading.Thread(target=chat, args=(prompt, box))
    worker.start()

    # L0-L2 are cached / startup — genuinely instant per request.
    for i in range(IDX["L3"]):
        states[i] = "done"

    if TTY:
        sys.stdout.write("\033[?25l")
        print("\n".join(line_for(i, states, details, 0) for i in range(n)))

    printed = set()
    frame = 0
    while worker.is_alive() or not q.empty():
        drained = False
        try:
            while True:
                tag, det = q.get_nowait()
                details[tag] = det
                states[IDX[tag]] = "done"
                drained = True
        except queue.Empty:
            pass
        # frontier = first non-done layer → active
        frontier = next((i for i in range(n) if states[i] != "done"), None)
        if frontier is not None and worker.is_alive():
            states[frontier] = "active"
        frame += 1
        if TTY:
            sys.stdout.write(f"\033[{n}A" + "\n".join(line_for(i, states, details, frame) for i in range(n)) + "\n")
            sys.stdout.flush()
        else:
            for i in range(n):
                if states[i] == "done" and i not in printed:
                    print(line_for(i, states, details, frame))
                    printed.add(i)
        time.sleep(0.1 if TTY else 0.02)
    worker.join()
    stop.set()
    time.sleep(0.1)
    # final drain
    try:
        while True:
            tag, det = q.get_nowait()
            details[tag] = det
            states[IDX[tag]] = "done"
    except queue.Empty:
        pass
    for i in range(n):
        states[i] = "done"
    if TTY:
        sys.stdout.write(f"\033[{n}A" + "\n".join(line_for(i, states, details, frame) for i in range(n)) + "\n")
        sys.stdout.write("\033[?25h")
    else:
        for i in range(n):
            if i not in printed:
                print(line_for(i, states, details, frame))

    print()
    if "err" in box:
        print(col(f"  ✗ error: {box['err']}", "31"))
        return 1
    resp = box["resp"]
    if "choices" not in resp:
        print(col("  ⚠ backend returned no completion:", "33"))
        print("  " + json.dumps(resp.get("error", resp))[:280])
        return 0
    msg = resp["choices"][0]["message"].get("content") or ""
    usage = resp.get("usage", {})
    print(col("  ── result ──────────────────────────────────────────────", "36"))
    for line in msg.strip().splitlines()[:8]:
        print("  " + line)
    print()
    print(f"  {col('tokens','1')} in {usage.get('prompt_tokens','?')} · out {usage.get('completion_tokens','?')}   "
          f"{col('model','1')} {str(resp.get('model','ornith')).split('/')[-1]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
