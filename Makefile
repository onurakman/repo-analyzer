.PHONY: build release test test-unit test-integration lint fmt check clean install run help

# Default target
help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-20s\033[0m %s\n", $$1, $$2}'

build: ## Build debug binary
	cargo build

release: ## Build release binary
	cargo build --release

test: ## Run all tests
	cargo test

test-unit: ## Run unit tests only
	cargo test --lib

test-integration: ## Run integration tests only
	cargo test --test '*'

lint: ## Run clippy lints
	cargo clippy -- -D warnings

fmt: ## Format code
	cargo fmt

fmt-check: ## Check formatting without changing files
	cargo fmt -- --check

check: ## Run cargo check (fast compile check)
	cargo check

clean: ## Clean build artifacts
	cargo clean

install: release ## Install binary to ~/.cargo/bin
	cargo install --path .

run: build ## Run with default args (current dir, terminal output)
	cargo run

# Example targets for common usage patterns
run-json: build ## Run with JSON output
	cargo run -- -f json -q

run-html: build ## Run with HTML output
	cargo run -- -f html --output report.html

run-csv: build ## Run with CSV output
	cargo run -- -f csv --output report.csv

ci: fmt-check lint test ## Run all CI checks (format + lint + test)
