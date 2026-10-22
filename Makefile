.PHONY: all validate test search help

OKF_BIN ?= $(shell command -v okf 2>/dev/null || echo "bin/okf")
BUNDLE := docs

all: validate

## validate: Run strict OKF v0.2 validation on the docs/ bundle
validate:
	@$(OKF_BIN) validate $(BUNDLE) --strict --drift

## test: Run Rust unit + Outer-PTY E2E tests and OKF bundle validation
test: validate
	@cargo test

## search: Search project memory (e.g. make search q="auth")
search:
	@$(OKF_BIN) search "$(q)" $(BUNDLE)

help:
	@echo "Project Memory Commands:"
	@echo "  make validate       Validate docs/ bundle"
	@echo "  make search q=\"...\" Search project memory"
