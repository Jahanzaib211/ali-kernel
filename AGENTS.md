# ali-kernel — AI Coding Agent Protocol

This file is read by AI coding assistants (Claude Code, Aider, etc.) working
in this repo. The companion to `CLAUDE.md` (which is the broader project
instructions for humans). Read both before making changes.

## Cross-project isolation — HARD RULE

This repo MUST NOT contain literal references to:

- **`nova`** / **`Nova`** — the consumer project (formerly DeerFlow fork)
- **`DeerFlow`** / **`deer-flow`** — the upstream it was forked from
- **`ornith`** — only allowed as the literal model identifier (e.g. a JSON
  field `"model": "ornith"` in a benchmark response); never in comments,
  doc strings, or test names

The `deerflow` Python package namespace is allowed (historical package name
in some consumer projects), but in this Rust codebase there are no such
imports — only the literal consumer-project names are forbidden.

## Why this matters

- **Future AI drift**: AI sessions will see references and assume they are
  load-bearing, then build more coupling on top of them.
- **Deployment decoupling**: ali-kernel is a generic OpenAI-compatible
  gateway. Specific consumers should be wired through deployment config
  (env vars, runtime settings), not source code.
- **Reusability**: keeping the gateway generic means it can serve any
  OpenAI-compatible client, not just one specific one.

## Mechanical enforcement

A test (`scripts/test_no_cross_references.py`) runs as part of the standard
test suite. It fails on any forbidden string. CI (`.github/workflows/cross-ref.yml`)
runs the same test on every push/PR. The test cannot be bypassed without
editing it, which is a visible commit.

Run it locally:
```bash
make cross-ref-check
# or
python3 scripts/test_no_cross_references.py
```

The auto-fix tool (`scripts/fix_cross_references.py`) handles common
patterns automatically:
```bash
python3 scripts/fix_cross_references.py --dry-run   # preview
python3 scripts/fix_cross_references.py             # apply
```

## AI self-test protocol

Before EVERY commit in this repo:

```bash
make check                       # cargo check + cargo test
make cross-ref-check             # the isolation test
git status                       # confirm working tree is sane
```

All three must succeed. If `make cross-ref-check` finds a violation:

1. If the violation is in a comment or doc → just edit the file, remove the
   string, commit.
2. If the violation is in a real code path (variable name, string literal,
   config key) → use `fix_cross_references.py` first; if the fix isn't
   covered, manually rename + refactor.
3. If you genuinely need to keep the string (e.g. `ornith` as a model id
   literal in benchmark JSON), add the file path to the test's allow-list
   AND explain in the commit message why.

## Adding a new feature

1. Write the test first (TDD).
2. Make it generic — would this work for a different consumer?
3. If you find yourself wanting to name a specific consumer, stop. Express
   the integration through env vars or deployment config instead.
4. Run `make check` and `make cross-ref-check`.
5. Commit.

## Before each commit

```bash
make check                       # must pass
make cross-ref-check             # must pass
git status                       # working tree clean
git diff --stat                  # review your changes
git commit -m "..."              # clear message
```

## Operational gotchas (carried over from CLAUDE.md)

- Binary + constitution attestation: `rm -rf /tmp/ali-ram-moat && systemctl
  restart ali-kernel` after every `cargo build --release`.
- Editing `constitutions/*.md` trips the attestation too.
- Never bind to `0.0.0.0` — the bind rule is permanent.

## Git conventions

- Solo-owner repo (`github.com/Jahanzaib211/ali-kernel`) — direct-to-`master`
  commits are the norm, no PR flow.
- **Still confirm with the owner before every `git push`** — this has come
  up repeatedly; the confirmation step is expected even though the answer
  is almost always yes.

## Reference

- `CLAUDE.md` — broader project rules
- `GUIDE.md` — owner-facing plain-English explainer
- `README.md` — install + API reference
- `STATUS.md` — session-by-session status log
- `ROADMAP.md` — phase tracker
