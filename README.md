# repo-analyzer

Git repository analysis tool with code-construct-level granularity.

`repo-analyzer` walks your Git history, parses diffs with [tree-sitter](https://tree-sitter.github.io/tree-sitter/), and produces reports at the function/class/method level -- not just file level.

## Installation

### Pre-built binary (recommended)

Pulls the latest release for your platform and installs it into `/usr/local/bin`.

```bash
curl -sfL https://raw.githubusercontent.com/onurakman/repo-analyzer/master/contrib/install.sh | sh -s -- -b /usr/local/bin
```

Pass a specific version as the last argument (e.g. `v0.1.5`). Use `-b "$HOME/.local/bin"` if you don't want to use sudo. Linux builds are statically linked against musl, so they run on any distro (Alpine, Debian, RHEL, slim Docker images — no glibc dance).

### From source

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
| | | `--only` | all | Comma-separated report filter (see [Reports](#reports) for valid names) |
| | | `--since` | none | Analyze commits since duration (e.g. `6m`, `1y`, `30d`, `2w`) |
| | | `--from` | none | Start date `YYYY-MM-DD` (requires `--to`) |
| | | `--to` | none | End date `YYYY-MM-DD` (requires `--from`) |
| | | `--top` | none | Truncate each report to the top N entries (terminal/JSON/CSV only; totals are still surfaced so `top 20 of 240` is visible) |
| | `-o` | `--output` | stdout | Write output to file instead of stdout |
| | `-q` | `--quiet` | `false` | Suppress progress indicators |
| | `-u` | `--unshallow` | `false` | Auto-run `git fetch --unshallow` on shallow clones instead of prompting or aborting. Pair with `--quiet` for CI. |
| | | `--threads` | `0` (auto) | Number of threads for parallel processing |
| | | `--channel-capacity` | `4` | Bounded-channel slots between producer, workers, and SQLite writer. Lower (1–2) to tighten RAM on small pods; raise (8–32) on fast disks. |
| | | `--batch-size` | `64` | Max parsed changes per batch flushed to the store. Smaller cuts in-flight memory on huge merge commits; larger amortizes SQLite transaction overhead. |
| | | `--object-cache-mb` | `4` | Per-thread `gix` object cache size in MiB. Drop to `1` on tight pods; raise for very repo-heavy runs. |
| | | `--quick-composition` | `false` | Fast filesystem-only language breakdown. Skips git entirely and prints a flat `[{"language","percentage",...}]` JSON array. All other report flags are ignored. |

Duration suffixes: `d` (days), `w` (weeks), `m` (months, ~30 days), `y` (years, ~365 days).

Short flags may be combined: `-qu` = `--quiet --unshallow`, `-quf json` = `--quiet --unshallow --format json`.

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

# CI: shallow clone, quiet, auto-unshallow, JSON to file
repo-analyzer . -qu -f json -o report.json

# Tight memory pod: smaller batch + smaller gix cache
repo-analyzer . --batch-size 16 --object-cache-mb 1 --channel-capacity 2

# Fast language composition (no git, pure filesystem walk, ~tens of ms)
repo-analyzer . --quick-composition
repo-analyzer /path/to/repo --quick-composition --top 5 -o comp.json
```

### Quick composition (no git)

`--quick-composition` is a shortcut for the common "what is this repo written in?" question. It bypasses the whole commit-history pipeline and walks the working tree directly, classifying each file with the same 460-language knowledge base used by the `composition` report. Typical runs finish in tens of milliseconds even on multi-thousand-file repos.

Output is a flat JSON array, sorted descending by share of real code lines:

```json
[
  { "language": "Rust",  "percentage": 76.02, "code_lines": 12795, "files": 72 },
  { "language": "JSON5", "percentage": 15.61, "code_lines": 2627,  "files": 1  }
]
```

Notes:

- Skips `.git`, `node_modules`, `target`, `dist`, `build`, `venv`, `__pycache__`, `vendor`, and common IDE dirs.
- Skips lockfiles (`package-lock.json`, `yarn.lock`, `Cargo.lock`, `go.sum`, …), manifests (`package.json`, `pom.xml`, `requirements.txt`, …), docs (`README`, `LICENSE`, `CHANGELOG`), and pure data/markup dialects (JSON, JSON5, YAML, TOML, XML, Markdown, INI, …) — same code-only filter the history-based metrics use.
- Skips binaries (NUL-byte heuristic), empty files, and files above 2 MiB (usually minified/generated).
- Percentages are computed from **code** lines — comments and blanks are excluded — and rounded to 2 decimals.
- `--top` and `--output` are honored; other report flags (`--since`, `--from`/`--to`, `--only`, `--format`) are ignored.

## Reports

| Report | `--only` value | Description |
|---|---|---|
| Health Score | `health` | Single 0–100 score synthesised from every other report, plus a prioritised action list |
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
| Debt Markers | `debt_markers` | `TODO` / `FIXME` / `HACK` / `XXX` comments, enriched with git-blame (author + age) |
| Large Sources | `large_sources` | Source files past a size threshold — likely refactor candidates |
| Half-Life | `half_life` | How long lines in each file survive before being rewritten (heavy — opt-in via `--only`) |
| Succession | `succession` | How ownership of files transfers between authors over time |
| Knowledge Silos | `knowledge_silos` | Files known by only one author (bus-factor risk) |
| Fan-In / Fan-Out | `fan_in_out` | Incoming / outgoing dependency counts per file |
| Module Coupling | `module_coupling` | Coupling aggregated at module level |
| Churn Pareto | `churn_pareto` | 80/20 distribution of churn — how concentrated changes are |
| Construct Ownership | `construct_ownership` | Author ownership at function / class level |

By default every report except `half_life` is generated (it's memory-hungry on long histories — opt in via `--only half_life,...`). Use `--only` to select a subset.

Most metrics skip non-code paths (lockfiles, generated bundles, docs, assets) via the shared `source_filter` so coupling and outlier signals aren't drowned out by noise.

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

Language detection is delegated to [drshade/linguist](https://github.com/drshade/linguist) (GitHub Linguist data, MIT), so the `composition` / `quick-composition` / `debt_markers` / `large_sources` reports are not limited to the list above. Line classification (real code vs comment vs blank, nested block comments, shebangs) still uses the codestats-derived knowledge base ([trypsynth/codestats](https://github.com/trypsynth/codestats), MIT) — 460+ languages with their comment markers.

Code-focused reports (coupling, module coupling, outliers, silos, hotspots, …) route every file through a shared source filter that excludes:

- Lock files (`Cargo.lock`, `package-lock.json`, `yarn.lock`, `bun.lock`, `uv.lock`, `pnpm-lock.yaml`, `go.sum`, …) and manifests (`package.json`, `pom.xml`, `requirements.txt`, …).
- Docs (`README`, `LICENSE`, `CHANGELOG`) and markup / data dialects (JSON, YAML, TOML, XML, Markdown, INI, …).
- Vendored / generated paths via Linguist's curated `vendor.yml`: `node_modules/`, `vendor/`, `bower_components/`, `*.min.js`, generated protobuf, test fixtures, Gradle/Cocoapods caches, and hundreds more patterns.

This keeps architectural signals from being drowned out by bundled dependencies and generated output.

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

## Shallow clones

History-based metrics need the full commit log. If the repo is a shallow clone:

- **Interactive:** you're prompted to unshallow (`git fetch --unshallow`).
- **`--quiet`:** the run aborts with instructions.
- **`--unshallow` / `-u`:** the fetch runs automatically. Typical CI use: `-qu`.

## CI/CD

The project uses [release-please](https://github.com/googleapis/release-please-action) for automated versioning.

- Every push to `master` runs CI checks (`fmt`, `clippy`, `test`).
- Conventional commits (`feat:`, `fix:`, etc.) trigger automatic version bump PRs.
- Merging a release PR creates a git tag, GitHub Release, and builds binaries for Linux (musl, static), macOS, and Windows (amd64 + arm64). Linux targets use [`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild) so the resulting binaries have no glibc version dependency and run on any distro.

## License

See [LICENSE](LICENSE) for details.
