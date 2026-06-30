#!/usr/bin/env python3
"""
Clean the captured RLAIF corpus before fine-tuning.

Drops, from training/sft.jsonl and training/dpo_pairs.jsonl:
  - corrupted lines (not valid JSON — can happen under concurrent appends)
  - near-empty outputs
  - malformed tool-call XML answers (<tool_call>/<function=)
  - self-instruct meta-prompts that leaked in before they were routed to raw :8081
  - exact duplicates

Writes <file>.clean.jsonl next to each input and prints a before/after summary.
Non-destructive: never overwrites the originals.

Usage: python3 scripts/clean_corpus.py
"""
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TRAIN = ROOT / "training"

META_MARKERS = ("one per line", "one item per line", "no numbering")
XML_MARKERS = ("<tool_call>", "<function=")


def is_meta(instruction: str) -> bool:
    lo = instruction.lower()
    return any(m in lo for m in META_MARKERS)


def has_xml(text: str) -> bool:
    return any(m in text for m in XML_MARKERS)


def clean_sft(path: Path) -> tuple[int, int]:
    kept, seen = [], set()
    total = 0
    for line in path.open():
        line = line.strip()
        if not line:
            continue
        total += 1
        try:
            r = json.loads(line)
        except json.JSONDecodeError:
            continue  # corrupted (concurrent-append interleave)
        out = (r.get("output") or "").strip()
        ins = r.get("instruction") or ""
        if len(out) < 5 or has_xml(out) or is_meta(ins):
            continue
        key = (ins, out)
        if key in seen:
            continue
        seen.add(key)
        kept.append(r)
    out_path = path.with_suffix(".clean.jsonl")
    with out_path.open("w") as f:
        for r in kept:
            f.write(json.dumps(r) + "\n")
    return total, len(kept)


def clean_dpo(path: Path) -> tuple[int, int]:
    kept, seen = [], set()
    total = 0
    for line in path.open():
        line = line.strip()
        if not line:
            continue
        total += 1
        try:
            r = json.loads(line)
        except json.JSONDecodeError:
            continue
        chosen = (r.get("chosen") or "").strip()
        rejected = (r.get("rejected") or "").strip()
        if not chosen or not rejected or chosen == rejected or has_xml(chosen):
            continue
        key = (r.get("prompt", ""), chosen, rejected)
        if key in seen:
            continue
        seen.add(key)
        kept.append(r)
    out_path = path.with_suffix(".clean.jsonl")
    with out_path.open("w") as f:
        for r in kept:
            f.write(json.dumps(r) + "\n")
    return total, len(kept)


def main() -> int:
    for name, fn in (("sft.jsonl", clean_sft), ("dpo_pairs.jsonl", clean_dpo)):
        p = TRAIN / name
        if not p.exists():
            print(f"{name}: (none)")
            continue
        total, kept = fn(p)
        print(f"{name}: {total} -> {kept} kept ({total - kept} dropped) -> {p.with_suffix('.clean.jsonl').name}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
