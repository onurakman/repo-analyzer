# repo-analyzer

Git repository analysis tool with code-construct-level granularity.

`repo-analyzer` walks your Git history, parses diffs with [tree-sitter](https://tree-sitter.github.io/tree-sitter/), and produces reports at the function/class/method level -- not just file level.

## Installation

```bash
# Install to ~/.cargo/bin
cargo install --path .

# Or build a release binary
cargo build --release
# Binary: target/release/repo-analyzer
```

## Usage

```
repo-analyzer [OPTIONS] [PATH]
```

| Argument | Short | Long | Default | Description |
|---|---|---|---|---|
| `PATH` | | | `.` | Path to the git repository to analyze |
| | `-f` | `--format` | `table` | Output format: `table`, `json`, `csv`, `html` |
| | | `--only` | all | Comma-separated report filter (any of: `composition`, `authors`, `hotspots`, `churn`, `ownership`, `coupling`, `patterns`, `age`, `bloat`, `outliers`, `quality`, `complexity`, `construct_churn`, `half_life`, `succession`, `knowledge_silos`, `fan_in_out`, `module_coupling`, `churn_pareto`, `construct_ownership`) |
| | | `--since` | none | Analyze commits since duration (e.g. `6m`, `1y`, `30d`, `2w`) |
| | | `--from` | none | Start date `YYYY-MM-DD` (requires `--to`) |
| | | `--to` | none | End date `YYYY-MM-DD` (requires `--from`) |
| | | `--top` | none | Show only the top N entries per report |
| | `-o` | `--output` | stdout | Write output to file instead of stdout |
| | `-q` | `--quiet` | `false` | Suppress progress indicators |
| | | `--threads` | `0` (auto) | Number of threads for parallel processing |

Duration suffixes: `d` (days), `w` (weeks), `m` (months, ~30 days), `y` (years, ~365 days).

## Examples

```bash
# Analyze current directory, terminal table output
repo-analyzer

# Analyze a specific repo with JSON output
repo-analyzer /path/to/repo -f json

# Last 6 months, top 20 hotspots only, quiet mode
repo-analyzer --since 6m --only hotspots --top 20 -q

# Date range, multiple reports, write HTML to file
repo-analyzer --from 2025-01-01 --to 2025-12-31 --only authors,churn,ownership -f html -o report.html

# Export CSV for external analysis
repo-analyzer -f csv -o metrics.csv --only coupling,patterns

# Full analysis with 4 threads
repo-analyzer /path/to/repo --threads 4
```

## Reports

| Report | `--only` value | Description |
|---|---|---|
| Code Composition | `composition` | Language breakdown at HEAD: real code vs comment vs blank lines, files and bytes per language |
| Authors | `authors` | Commit counts and contribution stats per author |
| Hotspots | `hotspots` | Files and constructs with the most change activity |
| Churn | `churn` | Lines added/removed per file over time |
| Ownership | `ownership` | Code ownership distribution by author per file |
| Coupling | `coupling` | Files that frequently change together |
| Patterns | `patterns` | Commit distribution by hour of day and day of week |
| Age | `age` | Time since last modification per file/construct |
| Bloat | `bloat` | Large files and committed artifacts (minified bundles, build output, vendored deps) |
| Outliers | `outliers` | High-churn + high-author-count files (biggest ownership/risk flags) |
| Quality | `quality` | Commit quality signals: short/low-quality messages, mega-commits, reverts, merges |
| Complexity | `complexity` | Cyclomatic complexity per function (code-only SLOC) |
| Construct Churn | `construct_churn` | Churn at function / class / method level |
| Half-Life | `half_life` | How long lines in each file survive before being rewritten (heavy — opt-in via `--only`) |
| Succession | `succession` | How ownership of files transfers between authors over time |
| Knowledge Silos | `knowledge_silos` | Files known by only one author (bus-factor risk) |
| Fan-In / Fan-Out | `fan_in_out` | Incoming / outgoing dependency counts per file |
| Module Coupling | `module_coupling` | Coupling aggregated at module level |
| Churn Pareto | `churn_pareto` | 80/20 distribution of churn — how concentrated changes are |
| Construct Ownership | `construct_ownership` | Author ownership at function / class level |

By default every report except `half_life` is generated (it's memory-hungry on long histories — opt in via `--only half_life,...`). Use `--only` to select a subset.

## Output Formats

| Format | `--format` value | Description |
|---|---|---|
| Terminal table | `table` | Pretty-printed tables using `comfy-table` (default) |
| JSON | `json` | Structured JSON, suitable for piping to `jq` |
| CSV | `csv` | Comma-separated values for spreadsheet import |
| HTML | `html` | Self-contained HTML report |

## Supported Languages

Construct-level parsing (function / class / method extraction via tree-sitter):

| Language | Extensions | Tree-sitter crate |
|---|---|---|
| Rust | `.rs` | `tree-sitter-rust` |
| TypeScript | `.ts`, `.tsx` | `tree-sitter-typescript` |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` | `tree-sitter-javascript` |
| Python | `.py`, `.pyi` | `tree-sitter-python` |
| Java | `.java` | `tree-sitter-java` |
| Go | `.go` | `tree-sitter-go` |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.h` | `tree-sitter-cpp` |
| C# | `.cs` | `tree-sitter-c-sharp` |
| Kotlin | `.kt`, `.kts` | `tree-sitter-kotlin-ng` |
| Dart | `.dart` | `tree-sitter-dart` |
| PHP | `.php` | `tree-sitter-php` |
| Ruby | `.rb` | `tree-sitter-ruby` |
| Scala | `.scala`, `.sc` | `tree-sitter-scala` |
| Swift | `.swift` | `tree-sitter-swift` |
| Bash | `.sh`, `.bash` | `tree-sitter-bash` |
| HTML | `.html`, `.htm` | `tree-sitter-html` |
| CSS | `.css`, `.scss` | `tree-sitter-css` |

Files with unrecognized extensions are still tracked at the file level; they just lack construct-level detail.

Cyclomatic complexity (`complexity` report) is available for: Rust, TypeScript, JavaScript, Python, Java, Go, Kotlin, Dart.

The `composition` report uses a separate **460+ language** knowledge base (ported from [codestats](https://github.com/trypsynth/codestats), MIT) for language detection and accurate code / comment / blank line classification — the detection isn't limited to the list above.

Lock files (`Cargo.lock`, `package-lock.json`, `yarn.lock`, `bun.lock`, `uv.lock`, `pnpm-lock.yaml`, etc.) are automatically excluded from analysis.

## Development

Requires Rust edition 2024 (nightly or stable 1.85+).

```bash
make setup           # Install rustfmt + clippy + cargo-edit (run once after clone)
make build           # Build debug binary
make release         # Build release binary
make test            # Run all tests
make test-unit       # Run unit tests only
make test-integration # Run integration tests only
make lint            # Run clippy (warnings are errors)
make fmt             # Format code
make fmt-check       # Check formatting without changes
make check           # Fast compile check
make clean           # Clean build artifacts
make install         # Build release + install to ~/.cargo/bin
make run             # Run with default args (current dir, table output)
make pre-commit      # fmt + lint + test (run before committing)
make ci              # fmt-check + lint + test (mirrors GitHub Actions)
make upgrade-check   # Dry-run dep upgrades (dev-deps excluded)
make upgrade         # Apply dep upgrades (dev-deps excluded)
make help            # Show all targets
```

Shortcut targets for common output formats:

```bash
make run-json        # JSON to stdout, quiet mode
make run-html        # HTML to report.html
make run-csv         # CSV to report.csv
```

## CI/CD

The project uses [release-please](https://github.com/googleapis/release-please-action) for automated versioning.

- Every push to `master` runs CI checks (`fmt`, `clippy`, `test`).
- Conventional commits (`feat:`, `fix:`, etc.) trigger automatic version bump PRs.
- Merging a release PR creates a git tag, GitHub Release, and builds binaries for Linux, macOS, and Windows (amd64 + arm64).

## License

See [LICENSE](LICENSE) for details.
