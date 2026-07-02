#!/usr/bin/env bash
# One-liner for AI self-test: fail if any consumer-specific name appears.
#
# Usage:
#   bash scripts/check_no_cross_references.sh
#
# Exit codes:
#   0 — clean
#   1 — violations found
#
# This is a thin wrapper around scripts/test_no_cross_references.py. It
# exists so AI sessions can run a single grep-style check without needing
# to know the python script's path.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

exec python3 "$REPO_ROOT/scripts/test_no_cross_references.py" "$@"
