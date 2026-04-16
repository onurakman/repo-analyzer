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
| | | `--only` | all | Comma-separated report filter: `authors,hotspots,churn,ownership,coupling,patterns,age` |
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
| Authors | `authors` | Commit counts and contribution stats per author |
| Hotspots | `hotspots` | Files and constructs with the most change activity |
| Churn | `churn` | Lines added/removed per file over time |
| Ownership | `ownership` | Code ownership distribution by author per file |
| Coupling | `coupling` | Files that frequently change together |
| Patterns | `patterns` | Recurring change patterns across constructs |
| Age | `age` | Time since last modification per file/construct |

By default all 7 reports are generated. Use `--only` to select a subset.

## Output Formats

| Format | `--format` value | Description |
|---|---|---|
| Terminal table | `table` | Pretty-printed tables using `comfy-table` (default) |
| JSON | `json` | Structured JSON, suitable for piping to `jq` |
| CSV | `csv` | Comma-separated values for spreadsheet import |
| HTML | `html` | Self-contained HTML report |

## Supported Languages

| Language | Extensions | Tree-sitter crate |
|---|---|---|
| Rust | `.rs` | `tree-sitter-rust` |
| TypeScript / JavaScript | `.ts`, `.tsx`, `.js`, `.jsx` | `tree-sitter-typescript` |
| Python | `.py`, `.pyi` | `tree-sitter-python` |
| Java | `.java` | `tree-sitter-java` |
| Go | `.go` | `tree-sitter-go` |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.h` | `tree-sitter-cpp` |
| C# | `.cs` | `tree-sitter-c-sharp` |
| Kotlin | `.kt`, `.kts` | `tree-sitter-kotlin-ng` |

Files with unrecognized extensions are still tracked at the file level; they just lack construct-level detail.

## Development

Requires Rust edition 2024 (nightly or stable 1.85+).

```bash
make help            # Show all targets
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
make ci              # fmt-check + lint + test (CI pipeline)
```

Shortcut targets for common output formats:

```bash
make run-json        # JSON to stdout, quiet mode
make run-html        # HTML to report.html
make run-csv         # CSV to report.csv
```

## License

See [LICENSE](LICENSE) for details.
