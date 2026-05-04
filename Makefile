.PHONY: build release test test-unit test-integration lint fmt fmt-check check clean install run help ci setup pre-commit upgrade-check upgrade coverage coverage-html coverage-open coverage-clean coverage-install

# Coverage output paths. JSON is the AI-readable artifact; LCOV is for
# CI integrations (Codecov / Coveralls); HTML is for human browsing.
COVERAGE_DIR := coverage
COVERAGE_JSON := $(COVERAGE_DIR)/coverage-summary.json
COVERAGE_LCOV := $(COVERAGE_DIR)/coverage.lcov
COVERAGE_HTML_DIR := $(COVERAGE_DIR)/html

# Default target
help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-20s\033[0m %s\n", $$1, $$2}'

setup: ## Install required toolchain components (rustfmt, clippy, llvm-tools) + cargo-edit + cargo-llvm-cov
	rustup component add rustfmt clippy llvm-tools-preview
	cargo install cargo-edit --locked
	cargo install cargo-llvm-cov --locked

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

ci: fmt-check lint test ## Run all CI checks (mirrors GitHub Actions)

pre-commit: fmt lint test ## Format, lint, and test before committing

coverage-install: ## Install cargo-llvm-cov + llvm-tools-preview rustup component
	rustup component add llvm-tools-preview
	cargo install cargo-llvm-cov --locked

coverage: ## Run tests with coverage; emit AI-readable JSON summary, LCOV, and terminal table
	@mkdir -p $(COVERAGE_DIR)
	cargo llvm-cov clean --workspace
	cargo llvm-cov --no-report --workspace
	cargo llvm-cov report --summary-only --json --output-path $(COVERAGE_JSON)
	cargo llvm-cov report --lcov --output-path $(COVERAGE_LCOV)
	@echo
	cargo llvm-cov report
	@echo
	@echo "Coverage artifacts:"
	@echo "  AI-readable JSON summary : $(COVERAGE_JSON)"
	@echo "  LCOV (Codecov/Coveralls) : $(COVERAGE_LCOV)"
	@echo "  HTML report (optional)   : run \`make coverage-html\`"

coverage-html: ## Generate HTML coverage report (uses cached coverage data when available)
	@mkdir -p $(COVERAGE_DIR)
	cargo llvm-cov report --html --output-dir $(COVERAGE_DIR)
	@echo "HTML report: $(COVERAGE_HTML_DIR)/index.html"

coverage-open: ## Generate HTML coverage report and open in browser
	@mkdir -p $(COVERAGE_DIR)
	cargo llvm-cov report --html --open --output-dir $(COVERAGE_DIR)

coverage-clean: ## Remove coverage artifacts and reset coverage data
	cargo llvm-cov clean --workspace
	rm -rf $(COVERAGE_DIR)

upgrade-check: ## Show dep upgrades available (incl. major bumps), dev-deps excluded
	cargo upgrade --incompatible --dry-run --exclude tempfile --exclude assert_cmd --exclude predicates

upgrade: ## Apply dep upgrades (incl. major bumps), dev-deps left untouched
	cargo upgrade --incompatible --exclude tempfile --exclude assert_cmd --exclude predicates
	cargo update
