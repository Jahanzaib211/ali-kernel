#!/usr/bin/env python3
"""
Ornith RLAIF capture flywheel — runs entirely against the LOCAL gateway.

Pipeline (zero external-API cost; all work on the local GPU):
  1. self-instruct : Ornith generates diverse prompts per domain (thinking off, hot temp).
  2. batch run     : each prompt -> POST /v1/chat/completions. The gateway auto-captures:
                       - training/sft.jsonl  (every call, alpaca format)
                       - training/dpo_pairs.jsonl (when L8 critique rewrites a code answer)
  3. measure       : count new SFT/DPO lines, optionally re-run /eval.

DPO only fires when the answer contains a code block (critique gate), so code/tool/security
prompts produce preference pairs; reasoning prompts produce SFT only.

Usage:
  python3 scripts/flywheel.py --per-domain 25            # ~100 prompts across 4 domains
  python3 scripts/flywheel.py --per-domain 50 --eval     # bigger run + re-eval at the end
  python3 scripts/flywheel.py --seed-file prompts.txt    # skip self-instruct, use a file
"""
import argparse
import json
import sys
import time
import urllib.request
from pathlib import Path

GATEWAY = "http://127.0.0.1:9000"   # full pipeline + SFT/DPO capture (task prompts)
LLAMA = "http://127.0.0.1:8081"     # raw model — for self-instruct, so meta-prompts
                                    # are NOT captured as training data
ROOT = Path(__file__).resolve().parent.parent
SFT = ROOT / "training" / "sft.jsonl"
DPO = ROOT / "training" / "dpo_pairs.jsonl"

DOMAINS = {
    "code": "concrete programming tasks across varied languages (Rust, Python, Go, JavaScript, SQL, bash) and difficulty",
    "tool": "tasks that require calling tools like bash_exec(command), read_file(path), or write_file(path, content)",
    "security": "security, penetration-testing, cryptography, and secure-coding tasks",
    "reasoning": "logic puzzles, math word problems, and step-by-step reasoning challenges",
}


def post(path: str, payload: dict, base: str = GATEWAY, timeout: int = 180) -> dict:
    req = urllib.request.Request(
        base + path,
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read())


def chat(content: str, max_tokens: int = 800, no_think: bool = False,
         base: str = GATEWAY, timeout: int = 180) -> str:
    payload = {
        "model": "ornith",
        "messages": [{"role": "user", "content": content}],
        "max_tokens": max_tokens,
    }
    if no_think:
        payload["chat_template_kwargs"] = {"enable_thinking": False}
    d = post("/v1/chat/completions", payload, base=base, timeout=timeout)
    return (d["choices"][0]["message"].get("content") or "").strip()


def clean(line: str) -> str:
    s = line.strip().lstrip("0123456789.)-* \t").strip()
    if len(s) >= 2 and s[0] in "\"'" and s[-1] in "\"'":
        s = s[1:-1].strip()
    return s


def self_instruct(domain: str, desc: str, n: int) -> list[str]:
    meta = (
        f"List {n} diverse, specific {desc}. "
        f"Write each as a single self-contained imperative sentence on its own line. "
        f"Output ONLY the list, one item per line. No numbering, no preamble, no commentary."
    )
    seen, out = set(), []
    # Run against the RAW model (:8081) so these meta-prompts are not captured as
    # SFT data. Retry with hotter temp if the parse yields too few.
    for attempt in range(2):
        try:
            raw = chat(meta, max_tokens=1600, no_think=True, base=LLAMA, timeout=240)
        except Exception as e:
            print(f"  [self-instruct {domain}] attempt {attempt+1} error: {e}", flush=True)
            continue
        for ln in raw.splitlines():
            c = clean(ln)
            # keep plausible task sentences; drop headers/blank/too-short lines
            if len(c) > 12 and " " in c and c.lower() not in seen:
                seen.add(c.lower())
                out.append(c)
        if len(out) >= max(1, n // 2):
            break
    return out[:n]


def count(path: Path) -> int:
    return sum(1 for _ in path.open()) if path.exists() else 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--per-domain", type=int, default=25)
    ap.add_argument("--seed-file", type=str, default=None)
    ap.add_argument("--eval", action="store_true")
    ap.add_argument("--max-tokens", type=int, default=800)
    args = ap.parse_args()

    # health gate
    try:
        h = post("/health", {}, timeout=5) if False else json.loads(
            urllib.request.urlopen(GATEWAY + "/health", timeout=5).read())
        if h.get("llama_server") != "healthy":
            print(f"gateway up but llama {h.get('llama_server')}; aborting", file=sys.stderr)
            return 1
    except Exception as e:
        print(f"gateway unreachable at {GATEWAY}: {e}", file=sys.stderr)
        return 1

    sft0, dpo0 = count(SFT), count(DPO)
    t0 = time.time()

    # build the prompt set
    prompts: list[tuple[str, str]] = []
    if args.seed_file:
        for ln in Path(args.seed_file).read_text().splitlines():
            c = clean(ln)
            if c:
                prompts.append(("seed", c))
    else:
        for dom, desc in DOMAINS.items():
            print(f"[self-instruct] {dom}: generating {args.per_domain} prompts...", flush=True)
            ps = self_instruct(dom, desc, args.per_domain)
            print(f"[self-instruct] {dom}: got {len(ps)}", flush=True)
            prompts += [(dom, p) for p in ps]

    print(f"\n[run] {len(prompts)} prompts through the gateway (auto SFT+DPO capture)\n", flush=True)
    ok = fail = 0
    for i, (dom, p) in enumerate(prompts, 1):
        try:
            ans = chat(p, max_tokens=args.max_tokens, timeout=200)
            ok += 1
            tag = "code" if "```" in ans else "    "
            print(f"  {i:>4}/{len(prompts)} [{dom:9}] {tag} {p[:64]}", flush=True)
        except Exception as e:
            fail += 1
            print(f"  {i:>4}/{len(prompts)} [{dom:9}] FAIL {type(e).__name__}: {str(e)[:40]}", flush=True)

    sft1, dpo1 = count(SFT), count(DPO)
    dt = time.time() - t0
    print("\n==== flywheel summary ====")
    print(f"  prompts run     : {ok} ok / {fail} fail")
    print(f"  SFT examples    : +{sft1 - sft0}  (total {sft1})")
    print(f"  DPO pairs       : +{dpo1 - dpo0}  (total {dpo1})")
    print(f"  wall time       : {dt:.0f}s  ({ok/dt*60:.1f} prompts/min)" if dt > 0 else "")

    if args.eval:
        print("\n[eval] running /eval ...", flush=True)
        try:
            d = post("/eval", {}, timeout=1800)
            print(f"  EVAL TOTAL: {d['total']}%  ({d['raw_score']}/{d['raw_max']})")
            for k, v in d.get("by_domain", {}).items():
                print(f"    {k:10} {v}%")
        except Exception as e:
            print(f"  eval failed: {e}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
