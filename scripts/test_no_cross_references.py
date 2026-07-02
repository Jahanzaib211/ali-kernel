"""Mechanical enforcement: ali-kernel must not reference consumer projects.

Run via pytest (`pytest scripts/test_no_cross_references.py`) or directly
(`python3 scripts/test_no_cross_references.py`).

The forbidden strings are nova / DeerFlow. The `ornith` literal is allowed
only as a model identifier (string value in JSON responses) — not in
comments, doc strings, function names, or test names.

Run from repo root:
    python3 scripts/test_no_cross_references.py
Or via Makefile:
    make cross-ref-check
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]

# Files to scan (relative to REPO_ROOT)
SCAN_PATTERNS = [
    "ali-kernel/**/*.rs",
    "ali-tui/**/*.rs",
    "scripts/**/*",
    "*.toml",
    "*.md",
    "AGENTS.md",
    "CLAUDE.md",
]

# Files to skip entirely (they contain the rule definitions themselves)
SKIP_FILES = {
    "AGENTS.md",
    "CLAUDE.md",
    "scripts/test_no_cross_references.py",  # contains "nova" / "DeerFlow" in docstrings
    "scripts/fix_cross_references.py",  # contains forbidden strings as rename-table data
}

# Allow-list for known historical references that are NOT project-coupling.
# Each entry is (exact_relative_path, line_pattern, reason). Adding to this
# list requires explicit justification; future AI sessions should NOT extend
# it without good cause — prefer renaming to adding a new allow-list entry.
#
# Format: list of (relative_path, regex, reason). The path must match
# exactly (relative to repo root) — substring matches were tried and caused
# false-negatives (e.g. "ali" matches "ali-kernel/...").
ALLOW_LIST: list[tuple[str, re.Pattern[str], str]] = [
    # The project was renamed from `ornith-sovereign-gateway` to `ali-kernel`
    # on 2026-07-02. Historical documents still reference the old name.
    (
        "README.md",
        re.compile(r"ornith"),
        "README.md is historical install doc; mentions old project name. "
        "Will be cleaned in a follow-up.",
    ),
    (
        "STATUS.md",
        re.compile(r"ornith"),
        "STATUS.md is a session log; references old project name in dated "
        "entries. Don't rewrite history — add new entries going forward.",
    ),
    # Operational config: the model file genuinely is named
    # ornith-1.0-9B-Q4_K_M.gguf on disk. Renaming the file would require
    # re-downloading a 5GB model — not worth it for the symbol-purity goal.
    (
        "ali",
        re.compile(r"ornith"),
        "Shell launcher; references the actual model file path.",
    ),
    (
        "llama-server.service",
        re.compile(r"ornith"),
        "systemd unit file; references the actual model file path.",
    ),
    # Legacy identifier shims from the rename — these are deliberate
    # backward-compat shims. Will be removed once the old `ORNITH_*` env
    # vars and `apply_ornith_backward_compat` function are no longer needed.
    # Also covers the actual model file path (legitimate runtime value).
    (
        "ali-kernel/src/config.rs",
        re.compile(r"(ornith_memory|ornith_backward_compat|ORNITH_|ornith-1\.0-9[Bb])"),
        "Backward-compat shim for pre-rename env vars AND the actual "
        "model file path (legitimate runtime value); tracked for "
        "removal in a follow-up.",
    ),
    # Skills/Tools/memory internals use "ornith" as a namespace prefix from
    # the old codebase. These are deep refactors; tracked for cleanup.
    (
        "ali-kernel/src/memory.rs",
        re.compile(r"ornith"),
        "Qdrant collection name from the old codebase; deep refactor "
        "needed if renamed (would require data migration).",
    ),
    (
        "ali-kernel/src/skills.rs",
        re.compile(r"ornith"),
        "Skills directory name from old codebase; tracked for rename.",
    ),
    (
        "ali-kernel/src/tools.rs",
        re.compile(r"ornith"),
        "User-Agent header from old codebase; cosmetic rename.",
    ),
    (
        "ali-kernel/src/training.rs",
        re.compile(r"ornith"),
        "Temp-dir prefix from old codebase; cosmetic rename.",
    ),
    (
        "ali-tui/src/main.rs",
        re.compile(r"ornith"),
        "TUI status display includes model name; cosmetic.",
    ),
    (
        "tests/integration.rs",
        re.compile(r"ornith"),
        "Test name references old model id; cosmetic rename.",
    ),
]

# Strings that absolutely cannot appear in this repo.
# Matching is case-insensitive — any case variation is flagged.
# Listed explicitly to cover all common separator/capitalization variants.
FORBIDDEN = [
    "nova",          # consumer project name
    "deerflow",      # upstream project name + package namespace
    "deer flow",     # upstream with space
    "deer-flow",     # upstream hyphenated
    "deer_flow",     # upstream underscored
]

# Context-specific allowances: `ornith` is the model id; allowed as a JSON value
# but never in comments, doc strings, or identifiers.
# We allow it only as a quoted JSON string: `"ornith"` or `'ornith'`.
ORITH_PATTERN = re.compile(r"""(['"])ornith\1""")
ORITH_FORBIDDEN_CONTEXTS = [
    re.compile(r"^\s*//.*ornith"),  # line comment
    re.compile(r"///\s*.*ornith"),  # rust doc comment
    re.compile(r"#\s*ornith"),       # python/shell comment
    re.compile(r"\bdef\s+\w*ornith\w*"),  # python function name
    re.compile(r"\b(ornith_\w+|\w+_ornith)\b"),  # snake_case identifier with ornith
    re.compile(r"\b(ornith[A-Z]\w*|[A-Z]\w*Ornith)\b"),  # CamelCase identifier with ornith
]


def _gather_files() -> list[Path]:
    """Collect all files to scan via `git ls-files` (respects .gitignore)."""
    result = subprocess.run(
        ["git", "ls-files"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    files = []
    for line in result.stdout.splitlines():
        if not line:
            continue
        path = REPO_ROOT / line
        if not path.is_file():
            continue
        if any(part.startswith(".") for part in path.parts):
            continue  # skip .git, .claude, etc.
        rel = path.relative_to(REPO_ROOT)
        if str(rel) in SKIP_FILES:
            continue
        files.append(path)
    return files


def _is_allowed(rel_str: str, line_text: str) -> bool:
    """True if (file, line) is in the allow-list."""
    for allowed_path, pattern, _reason in ALLOW_LIST:
        if allowed_path == rel_str and pattern.search(line_text):
            return True
    return False


def _scan_file(path: Path) -> list[tuple[int, str, str]]:
    """Return list of (line_no, forbidden_string, line_text) for each violation."""
    violations: list[tuple[int, str, str]] = []
    try:
        content = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return violations

    rel = path.relative_to(REPO_ROOT)
    rel_str = str(rel)

    for lineno, line in enumerate(content.splitlines(), start=1):
        if _is_allowed(rel_str, line):
            continue

        # Check forbidden project names (case-insensitive)
        line_lower = line.lower()
        for forbidden in FORBIDDEN:
            if forbidden.lower() in line_lower:
                violations.append((lineno, forbidden, line.strip()))

        # Check ornith in forbidden contexts (comments, identifiers)
        if "ornith" in line:
            for ctx_pattern in ORITH_FORBIDDEN_CONTEXTS:
                if ctx_pattern.search(line):
                    violations.append((lineno, "ornith (in identifier/comment)", line.strip()))
                    break
            else:
                # ornith appears in a line but NOT in a forbidden context.
                # Allowed only if it's a quoted JSON string value.
                if not ORITH_PATTERN.search(line):
                    if '"ornith"' not in line and "'ornith'" not in line:
                        violations.append((lineno, "ornith (non-JSON-value usage)", line.strip()))

    return violations


def main() -> int:
    print(f"Scanning {REPO_ROOT} for cross-project references...")
    print()

    files = _gather_files()
    total_violations = 0
    files_with_violations: dict[Path, list[tuple[int, str, str]]] = {}

    for path in files:
        violations = _scan_file(path)
        if violations:
            files_with_violations[path] = violations
            total_violations += len(violations)

    if not files_with_violations:
        print("OK: no cross-project references found.")
        print(f"  Scanned {len(files)} files.")
        return 0

    print(f"FAIL: {total_violations} cross-project reference(s) in {len(files_with_violations)} file(s):")
    print()
    for path, violations in files_with_violations.items():
        rel = path.relative_to(REPO_ROOT)
        for lineno, forbidden, line in violations:
            print(f"  {rel}:{lineno}: {forbidden}")
            print(f"    {line[:100]}{'...' if len(line) > 100 else ''}")
        print()

    print("Remediation:")
    print("  - For nova/DeerFlow: rename variable/function/module to something generic")
    print("  - For ornith in comments/identifiers: rewrite or move to config")
    print("  - For allowed ornith-as-model-id: ensure it's only a JSON string value")
    print()
    print("Auto-fix: python3 scripts/fix_cross_references.py --dry-run")
    print("Allow-list: edit SKIP_FILES / ORITH_PATTERN in scripts/test_no_cross_references.py")

    return 1


if __name__ == "__main__":
    sys.exit(main())
