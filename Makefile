.PHONY: all validate lint test search help

OKF_BIN ?= $(shell command -v okf 2>/dev/null || echo "bin/okf")
BUNDLE := docs

all: test

## validate: Run strict OKF v0.2 validation on the docs/ bundle
validate:
	@$(OKF_BIN) validate $(BUNDLE) --strict --drift

## lint: Run rustfmt check and clippy with warnings denied across all targets
lint:
	@cargo fmt --check
	@cargo clippy --all-targets -- -D warnings

## test: Run OKF validation, lint (fmt + clippy), and Rust unit + Outer-PTY E2E tests
test: validate lint
	@cargo test

## search: Search project memory (e.g. make search q="auth")
search:
	@$(OKF_BIN) search "$(q)" $(BUNDLE)

help:
	@echo "Project Memory Commands:"
	@echo "  make validate       Validate docs/ bundle"
	@echo "  make search q=\"...\" Search project memory"
