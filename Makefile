# ali-kernel Makefile
#
# Targets:
#   make check            — cargo check (fast type-check)
#   make build            — cargo build --release (full build)
#   make test             — cargo test (full test suite)
#   make cross-ref-check  — fail if any consumer-specific name appears in source
#   make lint             — rustfmt check + clippy with -D warnings
#   make format           — rustfmt on all .rs files
#   make ci               — check + test + cross-ref-check + lint (run before pushing)
#   make fix-cross-refs   — apply the auto-fix tool (use --dry-run first)

CARGO ?= cargo
PYTHON ?= python3
UV ?= uv

.PHONY: help check build test cross-ref-check lint format ci clean fix-cross-refs

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
	  awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}'

check: ## cargo check (fast type-check, no codegen)
	$(CARGO) check --all-targets

build: ## cargo build --release
	$(CARGO) build --release

test: ## cargo test (full test suite)
	$(CARGO) test --workspace

cross-ref-check: ## AI self-test: fail if any consumer-specific name appears
	bash scripts/check_no_cross_references.sh

lint: ## rustfmt check + clippy
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --all-targets -- -D warnings

format: ## rustfmt on all .rs files
	$(CARGO) fmt --all

ci: ## Full CI: check + test + cross-ref-check (run before pushing)
	$(MAKE) check
	$(MAKE) test
	$(MAKE) cross-ref-check

fix-cross-refs: ## Apply the auto-fix tool to all detected violations
	$(PYTHON) scripts/fix_cross_references.py

fix-cross-refs-dry-run: ## Preview what the auto-fix would do
	$(PYTHON) scripts/fix_cross_references.py --dry-run

clean: ## cargo clean
	$(CARGO) clean
