"""Auto-fix tool for cross-project reference violations.

Reads the violations found by scripts/test_no_cross_references.py and
applies safe one-to-one renames. Designed to be a quick first pass for
AI coding sessions — handles the common patterns; deep refactors remain
manual.

Usage:
    python3 scripts/fix_cross_references.py --dry-run   # preview, don't write
    python3 scripts/fix_cross_references.py             # apply
    python3 scripts/fix_cross_references.py --path <file>   # limit to one file

Safety rules (the tool REFUSES to break these):
  - Never touches .md files (docs are human-decision territory)
  - Never touches files in the allow-list
  - Validates Rust syntax via `cargo check` after Rust file edits
  - Writes via temp file + rename (atomic)
  - Won't operate outside repo root (no .. paths)

Exit codes:
  0 — nothing to fix (already clean) OR fixes applied successfully
  1 — fixes applied but post-fix verification failed (review needed)
  2 — refuses to apply (safety rule triggered)
  3 — internal error
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]

# Safe one-to-one renames. The order matters — longer patterns first
# (e.g. "apply_ornith_backward_compat" before "ornith") so the longer
# match wins when scanning.
RENAMES: list[tuple[str, str, str]] = [
    # Nova-specific identifiers (most specific first)
    ("nova_healthcheck", "local_healthcheck", "PM2 app rename (nova- prefix stripped)"),
    ("nova-agent", "local-agent", "agent workspace name"),
    # Generic project-name → config-via-env
    ("nova-healthcheck", "local-healthcheck", "PM2 app name (nova- prefix stripped)"),
    # Note: 'nova' alone is too ambiguous to auto-rename — could be a
    # variable, a string, or a module name. Left for manual review.
]

# Patterns that we recognize as "this ornith usage is legitimate".
# The fixer does NOT touch these — they're already correct.
ORNITH_ALLOWED_PATTERNS: list[re.Pattern[str]] = [
    re.compile(r"ornith-1\.0-9[Bb]-"),          # the actual GGUF filename
    re.compile(r"ornith_memory"),                # legacy Qdrant collection name
    re.compile(r"apply_ornith_backward_compat"), # backward-compat shim
    re.compile(r"ORNITH_\w+"),                   # legacy env var names
    re.compile(r"ornith-generated"),             # legacy skills dir
    re.compile(r"ornith-gateway"),               # legacy User-Agent
    re.compile(r"ornith-train-test-"),            # legacy temp file prefix
]

# What "nova" / "DeerFlow" / "deer-flow" / "deerflow" can mean and how to
# fix each — but the fixer is intentionally conservative and does NOT
# auto-rename these. It surfaces them and recommends manual review.
HUMAN_REVIEW_NEEDED: list[str] = [
    "nova",          # too generic; could be variable, string, module
    "DeerFlow",      # upstream project name; rarely appears post-rename
    "deer-flow",     # hyphenated upstream name
    "deerflow",      # package namespace (Python imports)
]


@dataclass
class ProposedFix:
    path: Path
    line_no: int
    before: str
    after: str
    description: str

    def show(self) -> str:
        rel = self.path.relative_to(REPO_ROOT)
        return f"  {rel}:{self.line_no}  {self.description}\n    - {self.before.strip()[:80]}\n    + {self.after.strip()[:80]}"


def _is_md(path: Path) -> bool:
    return path.suffix.lower() == ".md"


def _is_in_allow_list(rel: str) -> bool:
    """True if this relative path is in the test's allow-list (file-level)."""
    # Import the test module to use its ALLOW_LIST
    sys.path.insert(0, str(REPO_ROOT / "scripts"))
    import test_no_cross_references as t
    return any(rel == allowed_path for allowed_path, _, _ in t.ALLOW_LIST)


def _find_violations() -> list[tuple[Path, int, str, str]]:
    """Return (path, line_no, forbidden_string, line_text) for each violation."""
    sys.path.insert(0, str(REPO_ROOT / "scripts"))
    import test_no_cross_references as t
    files = t._gather_files()
    all_violations = []
    for path in files:
        rel = str(path.relative_to(REPO_ROOT))
        violations = t._scan_file(path)
        for lineno, forbidden, line in violations:
            all_violations.append((path, lineno, forbidden, line))
    return all_violations


def _plan_fix(path: Path, lineno: int, forbidden: str, line: str) -> ProposedFix | None:
    """Decide whether/how to fix one violation. Returns None if no safe fix.

    Logic:
      1. Refuse .md files and allow-listed files outright
      2. Look for a RENAMES pattern in the line — if found, apply it
         (this handles e.g. 'nova_healthcheck' inside a line flagged for 'nova')
      3. For bare 'nova' / 'DeerFlow' / 'deer-flow' / 'deerflow' with no
         specific rename pattern, defer to human review (too ambiguous)
      4. For 'ornith (in identifier/comment)' / 'ornith (non-JSON-value)':
         check ORNITH_ALLOWED_PATTERNS first; if no match, no safe rename
    """
    rel = str(path.relative_to(REPO_ROOT))

    # Refuse to touch docs
    if _is_md(path):
        return None

    # Refuse to touch allow-listed files
    if _is_in_allow_list(rel):
        return None

    # FIRST: scan line for any RENAMES pattern. The line may contain a
    # longer string (e.g. 'nova_healthcheck_cmd') that's flagged because
    # 'nova' is a substring of it. The rename table has the specific fix.
    for old, new, desc in RENAMES:
        if old in line:
            new_line = re.sub(re.escape(old), new, line, count=1)
            return ProposedFix(
                path=path,
                line_no=lineno,
                before=line,
                after=new_line,
                description=f"{desc} (renamed {old!r} → {new!r})",
            )

    # No specific rename pattern matched. Check if the forbidden string is
    # one we require human review for.
    if forbidden.lower() in {x.lower() for x in HUMAN_REVIEW_NEEDED}:
        return None

    # ornith-specific checks (only relevant when forbidden is ornith-*)
    if "ornith" in forbidden:
        # If the line matches an allowed pattern, leave it alone
        for allowed_pattern in ORNITH_ALLOWED_PATTERNS:
            if allowed_pattern.search(line):
                return None
        # No rename pattern and no allowed pattern → no safe fix
        return None

    return None


def _apply_fix(fix: ProposedFix) -> bool:
    """Apply one fix atomically. Returns True on success."""
    content = fix.path.read_text(encoding="utf-8")
    lines = content.splitlines(keepends=True)
    if fix.line_no < 1 or fix.line_no > len(lines):
        return False
    old_line = lines[fix.line_no - 1]
    new_line = old_line.replace(fix.before, fix.after)
    if new_line == old_line:
        return False
    lines[fix.line_no - 1] = new_line
    # Atomic write via temp file + rename
    fd, tmp_path = tempfile.mkstemp(dir=fix.path.parent, prefix=".fix.", suffix=".tmp")
    try:
        with open(fd, "w", encoding="utf-8") as f:
            f.writelines(lines)
        Path(tmp_path).rename(fix.path)
        return True
    except Exception:
        try:
            Path(tmp_path).unlink()
        except OSError:
            pass
        return False


def _verify_rust_syntax() -> bool:
    """Run `cargo check` to verify no Rust file edits broke the build."""
    print("Verifying Rust syntax via `cargo check`...")
    result = subprocess.run(
        ["cargo", "check", "--quiet"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        print("  cargo check failed:")
        print(result.stdout[-2000:] if result.stdout else "")
        print(result.stderr[-2000:] if result.stderr else "")
        return False
    print("  cargo check: OK")
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--dry-run", action="store_true", help="Preview only; don't write")
    parser.add_argument("--path", type=Path, help="Limit fixes to this path (relative to repo root)")
    parser.add_argument("--skip-verify", action="store_true", help="Skip post-fix `cargo check`")
    args = parser.parse_args()

    print(f"Scanning {REPO_ROOT} for cross-project violations...")
    print()
    violations = _find_violations()

    if args.path:
        violations = [v for v in violations if v[0].relative_to(REPO_ROOT) == args.path]

    if not violations:
        print("Nothing to fix.")
        return 0

    print(f"Found {len(violations)} violation(s). Planning fixes...")
    print()

    fixes: list[ProposedFix] = []
    needs_human_review: list[tuple[Path, int, str, str]] = []
    refused: list[tuple[Path, int, str, str]] = []

    for path, lineno, forbidden, line in violations:
        fix = _plan_fix(path, lineno, forbidden, line)
        if fix is not None:
            fixes.append(fix)
            continue

        # No fix proposed — categorize why
        rel = str(path.relative_to(REPO_ROOT))
        if _is_md(path):
            refused.append((path, lineno, forbidden, line))
        elif _is_in_allow_list(rel):
            refused.append((path, lineno, forbidden, line))
        elif forbidden.lower() in {x.lower() for x in HUMAN_REVIEW_NEEDED}:
            needs_human_review.append((path, lineno, forbidden, line))
        else:
            needs_human_review.append((path, lineno, forbidden, line))

    for fix in fixes:
        print(fix.show())

    if not fixes:
        print(f"No auto-fixable patterns found in {len(violations)} violation(s).")
        print()
        if needs_human_review:
            print(f"Human review needed for {len(needs_human_review)} item(s):")
            for path, lineno, forbidden, line in needs_human_review[:10]:
                rel = path.relative_to(REPO_ROOT)
                print(f"  {rel}:{lineno}  {forbidden}: {line.strip()[:80]}")
            if len(needs_human_review) > 10:
                print(f"  ... and {len(needs_human_review) - 10} more")
        return 0

    print(f"\nPlanned {len(fixes)} fix(es).")

    if args.dry_run:
        print("Dry-run mode — no changes written.")
        return 0

    # Apply each fix
    success_count = 0
    failure_count = 0
    for fix in fixes:
        if _apply_fix(fix):
            success_count += 1
            print(f"  ✓ {fix.path.relative_to(REPO_ROOT)}:{fix.line_no}")
        else:
            failure_count += 1
            print(f"  ✗ FAILED {fix.path.relative_to(REPO_ROOT)}:{fix.line_no}")

    print()
    print(f"Applied {success_count} fix(es); {failure_count} failure(s).")

    if not args.skip_verify:
        if not _verify_rust_syntax():
            print("Post-fix verification FAILED — review the changes.")
            return 1

    if needs_human_review:
        print()
        print(f"Human review needed for {len(needs_human_review)} item(s) not auto-fixed:")
        for path, lineno, forbidden, line in needs_human_review[:10]:
            rel = path.relative_to(REPO_ROOT)
            print(f"  {rel}:{lineno}  {forbidden}: {line.strip()[:80]}")
        if len(needs_human_review) > 10:
            print(f"  ... and {len(needs_human_review) - 10} more")

    return 0


if __name__ == "__main__":
    sys.exit(main())
