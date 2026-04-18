# AGENTS.md

Coding standards and architectural rules for AI agents working on the `repo-analyzer` codebase.

## Architecture

The application follows a streaming pipeline pattern with a disk-backed aggregation store:

```
Git Walker -> Diff Extractor -> Tree-sitter Parser -> ChangeStore (SQLite)
                                                     \
                                                      -> Metric Collectors -> Health Synthesis -> Report Writers
```

1. **Git Walker** (`src/git/walker.rs`) -- Iterates commits in the repository, filtered by time range.
2. **Diff Extractor** (`src/git/diff.rs`) -- Extracts per-file diff records (added/removed lines) from each commit. Interns repeated paths/authors via `Interner` (a `DashSet<Arc<str>>` so rayon workers don't fight a global mutex).
3. **Language Registry** (`src/parser/registry.rs`) -- Maps file extensions to tree-sitter grammars and parses diffs into `CodeConstruct` values. Parsers are pooled per-thread via `thread_local!` so they aren't reallocated per file.
4. **Change Store** (`src/store.rs`) -- Parsed changes stream into a temp SQLite database via a dedicated writer thread. Keeps peak RAM bounded on huge histories; collectors that aggregate per-change data query the store at finalize time via `finalize_from_db()` instead of buffering in memory. The temp DB file is unlinked on `Drop`.
5. **Metric Collectors** (`src/metrics/`) -- Each collector implements `MetricCollector`. In-memory collectors receive `ParsedChange` via `process()` and return a `MetricResult` from `finalize()`. DB-backed collectors skip `process()` and run SQL queries in `finalize_from_db()`. Repo-snapshot collectors (`bloat`, `complexity`, `composition`, `debt_markers`, `large_sources`) use `inspect_repo()` which walks the HEAD tree after commits are processed.
6. **Health Synthesis** (`src/scoring/health.rs`) -- Runs *after* every collector finalizes. Reads the other `MetricResult`s and derives a 0–100 score plus a prioritized action list. Prepended to the results so it appears first in every output.
7. **Report Writers** (`src/output/`) -- Each writer implements `ReportWriter` and serializes `MetricResult` slices to a specific format. `--top` is applied inside each writer (not upstream) so the real total can still be surfaced alongside the truncated list.

Auxiliary subsystems:

- **`src/langs/`** + **`src/analysis/line_classifier.rs`** -- 460+ language detection and code/comment/blank line classification. Ported from [codestats](https://github.com/trypsynth/codestats) (MIT). Data lives in `languages.json5`; `build.rs` codegens a static `LANGUAGES` table at build time.
- **`src/analysis/source_filter.rs`** -- Classifies paths as real source vs. non-code (lockfiles, generated bundles, docs, vendored assets). Most metrics (coupling, module coupling, outliers, silos, etc.) gate their work through `is_source_file()` so noise doesn't drown out real signals.
- **`src/interner.rs`** -- `DashSet<Arc<str>>` based string interner. Used for file paths and author emails that repeat heavily across commits. Must remain lock-free in the hot path.

The pipeline is orchestrated by `src/pipeline/engine.rs`. Rayon is used for commit-parallel diff extraction; two bounded channels (`--channel-capacity`) feed producer → workers → SQLite writer so memory stays flat regardless of history length. Batch size and per-thread gix object cache size are also CLI-tunable (`--batch-size`, `--object-cache-mb`) for memory-constrained environments.

## Mandatory Verification

After **every** code change — no exceptions — run:

```bash
cargo clippy --all-targets -- -D warnings
```

Do not consider any task complete until clippy passes with zero warnings. This applies to one-line edits, refactors, new features, test additions, and shallow "comment-only" changes alike. If clippy fails, fix the lints before reporting the work as done or moving on to the next task.

## Code Style

- **Edition:** Rust 2024 (`edition = "2024"` in Cargo.toml).
- **Formatting:** Always run `cargo fmt` before committing.
- **Linting:** `cargo clippy --all-targets -- -D warnings` must pass with zero warnings (see Mandatory Verification above).
- **No `unwrap()` in non-test code.** Use `?` with `anyhow::Result` or return `Option`.
- **Error handling:**
  - `thiserror` for domain-specific error types inside modules.
  - `anyhow` in `main.rs` and at pipeline boundaries.
- **Dependencies:** Justify new crate additions. Prefer the existing stack (serde, clap, comfy-table, indicatif, crossbeam, rayon, dashmap, rusqlite, aho-corasick, memchr, globset).

## Naming Conventions

| Element | Convention | Example |
|---|---|---|
| Modules | `snake_case` | `go_lang`, `csv_output` |
| Types / Traits | `PascalCase` | `MetricCollector`, `ReportKind` |
| Functions / Methods | `snake_case` | `build_default`, `parse_constructs` |
| Constants | `SCREAMING_SNAKE_CASE` | |
| Enum variants | `PascalCase` | `ReportKind::Hotspots` |
| File names | `snake_case.rs` | `rust_lang.rs`, `csv_output.rs` |

Language module files use the pattern `<language>.rs` (e.g., `rust_lang.rs`, `go_lang.rs`). The `_lang` suffix avoids Rust keyword conflicts.

## Adding a New Metric

1. Create `src/metrics/<name>.rs` with a struct (e.g., `FooCollector`).
2. Implement `MetricCollector` for it. Pick the right finalize path:
   - `process(&mut self, change: &ParsedChange)` + `finalize(&mut self) -> MetricResult` -- for in-memory collectors. Incrementally aggregate; do not store raw changes.
   - `finalize_from_db(&mut self, store: &ChangeStore, ...) -> Option<MetricResult>` -- for aggregation queries. Skip `process()`. Push filters / `GROUP BY` / `ORDER BY` / `LIMIT` into SQL wherever possible instead of pulling all rows into Rust. The pipeline uses `finalize_from_db` when it returns `Some`, otherwise falls back to `finalize()`.
   - `inspect_repo(&mut self, repo: &gix::Repository, ...)` -- for HEAD-tree snapshots (bloat, complexity, composition, debt_markers, large_sources). Runs once after the commit walk.
   - `name()` -- return a static string matching the report kind.
3. Add a variant to `ReportKind` in `src/types.rs`.
4. Update `ReportKind::all()`, `ReportKind::parse()`, and `Display` impl in `src/types.rs`. `all()` also dictates the output order in the terminal/JSON/CSV writers; place the variant where it should appear.
5. If the metric is memory-hungry on long histories, override `ReportKind::is_heavy()` to `true` so it's excluded from the default set and only runs when explicitly requested via `--only`.
6. Update the `--only` help text in `src/cli.rs` to include the new name.
7. Add a match arm in `Pipeline::create_collectors()` in `src/pipeline/engine.rs`.
8. Add `pub mod <name>;` to `src/metrics/mod.rs`.
9. Implement `Default` for the collector (delegates to `new()`).
10. Gate per-file logic through `analysis::source_filter::is_source_file()` unless the metric is explicitly meant for non-code paths. This keeps lockfiles, vendored bundles, and docs from polluting the signal.
11. Sort entries descending by the primary metric in `finalize()`.
12. If Health Score should take this metric into account, wire a contribution into `src/scoring/health.rs`.
13. Add the report to `REPORT_ORDER` and pick an icon in `ICON_MAP` in `templates/report.html` (otherwise the HTML output falls back to the generic `file-text` icon and trailing position).
14. Write unit tests in the same file. Write an integration test in `tests/`.

## Adding a New Language

1. Add the `tree-sitter-<lang>` crate to `[dependencies]` in `Cargo.toml`. Confirm it compiles against our pinned `tree-sitter` version — some grammars (older `tree-sitter-kotlin`, `tree-sitter-dart` 0.0.x) depend on an older `tree-sitter` and will fail the `links = "tree-sitter"` uniqueness check.
2. Create `src/parser/languages/<name>.rs` exporting:
   - `pub fn language() -> Language`
   - `pub fn query() -> Query`
   - `pub fn map_constructs(node: &Node, source: &str) -> Vec<CodeConstruct>`
3. Add `pub mod <name>;` to `src/parser/languages/mod.rs`.
4. Register in `LanguageRegistry::build_default()` in `src/parser/registry.rs`:
   ```rust
   registry.register(
       &["ext1", "ext2"],
       LanguageConfig {
           name: "Language Name",
           language: <name>::language(),
           query: <name>::query(),
           construct_mapper: <name>::map_constructs,
       },
   );
   ```
5. To also support cyclomatic complexity for the language, add a `LangSpec` (function-kinds + decision-kinds) in `src/metrics/complexity.rs` and wire it into `SUPPORTED` and the `spec_for_path` extension map.
6. Add tests for construct extraction in the language module.

**Note:** The `composition` report does *not* need changes here — it uses the `languages.json5` knowledge base for 460+ languages independently of tree-sitter grammars.

## Adding a New Output Format

1. Add a variant to `OutputFormat` in `src/types.rs`.
2. Create `src/output/<name>.rs` with a struct implementing `ReportWriter`.
3. Add `pub mod <name>;` to `src/output/mod.rs`.
4. Add a match arm in `main.rs` where the writer is selected.
5. If the format needs a new serialization crate, add it to `Cargo.toml`.

## Testing

### Unit tests

Place unit tests in a `#[cfg(test)] mod tests` block at the bottom of each module file.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_something() {
        // Arrange, Act, Assert
    }
}
```

### Integration tests

Place integration tests in the `tests/` directory. Use `assert_cmd` and `predicates` for CLI testing. Use `tempfile::TempDir` for test repositories.

Pattern for creating a test git repo:
1. Create a `TempDir`.
2. Run `git init`, `git config user.name/email`.
3. Write source files, `git add`, `git commit`.
4. Construct a `PipelineConfig` and run the pipeline.

### Running tests

```bash
cargo test              # All tests
cargo test --lib        # Unit tests only
cargo test --test '*'   # Integration tests only
```

## Error Handling

- Modules define errors with `thiserror::Error` when they have distinct failure modes.
- The pipeline and `main.rs` use `anyhow::Result` for ergonomic error propagation.
- Return `Option<T>` for lookups that may legitimately find nothing (e.g., `LanguageRegistry::get_for_file`).
- Never panic in library code. Reserve `unwrap()` / `expect()` for test code only.

## Performance

- **Streaming processing:** Collectors receive one `ParsedChange` at a time. Do not buffer all changes in memory.
- **Incremental aggregation:** Collectors maintain running counters/maps, not raw data vectors.
- **Disk-backed store:** Per-change data is written to a temp SQLite DB via `ChangeStore`. Collectors that need cross-change aggregation implement `finalize_from_db()` and run SQL there — this keeps peak RAM bounded on huge histories. See `src/store.rs`.
- **Thread pool:** Rayon global thread pool is configured once via `--threads`. Default (`0`) lets Rayon auto-detect.
- **Progress indication:** Use `indicatif` progress bars. Respect `--quiet` by using `ProgressBar::hidden()`. Sub-phase status is published via `ProgressReporter::status()` which updates the bar message in place (never scrolls new lines).

## Pipeline Rules

- **Lock file exclusion:** The pipeline automatically skips known lock files (`Cargo.lock`, `package-lock.json`, `yarn.lock`, `bun.lock`, `uv.lock`, `pnpm-lock.yaml`, etc.). New lock files are added to `LOCK_FILE_NAMES` in `src/pipeline/engine.rs`.
- **Source-only signal:** Most collectors gate per-file work through `analysis::source_filter::is_source_file()`. Non-code paths (docs, assets, generated bundles) are excluded so architectural signals like coupling aren't diluted. New rules go in `src/analysis/source_filter.rs`.
- **Author grouping:** Authors are grouped by **email**, not by name. This handles name variations across commits.
- **Signed values:** Use `MetricValue::SignedCount(i64)` for values that can be negative (e.g., `net_change`). Never cast a signed value to `MetricValue::Count(u64)`.
- **Default impls:** Every collector struct with a `pub fn new()` must also have a `Default` impl.
- **Shallow repos:** The pipeline rejects shallow clones by default because every history-based metric needs the full log. Behaviour branches:
  - Interactive TTY → prompt to run `git fetch --unshallow`.
  - `--quiet` alone → abort with a clear error.
  - `--unshallow` / `-u` → auto-unshallow. Pair with `--quiet` in CI.
- **Memory tuning knobs:** `--channel-capacity`, `--batch-size`, `--object-cache-mb` expose pipeline bounds. Defaults (4 / 64 / 4 MB) are tuned for mid-range pods; drop everything for < 256 MB limits. Never hard-code these values inside new collectors.
- **`--top` semantics:** Truncation happens inside writers (terminal/JSON/CSV), not upstream. Always keep the full `MetricResult.entries` available to downstream synthesis (especially Health) so totals remain visible (`top 20 of 240`).

## CI/CD & Release

### Workflows (`.github/workflows/`)

- **`ci.yml`** -- Runs on every push and PR to `master`: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`. Uses `dtolnay/rust-toolchain@stable`, so new clippy lints can land locally before they land in CI — keep the local toolchain current with `rustup update stable` or pin it in `rust-toolchain.toml` if drift becomes painful.
- **`release-please.yml`** -- Runs on push to `master`. Uses [release-please](https://github.com/googleapis/release-please-action) to automate versioning. Tag format is plain `vX.Y.Z` (configured via `include-component-in-tag: false`); legacy tags from before that change retain the `repo-analyzer-vX.Y.Z` prefix and `contrib/install.sh` tries both formats.

### Release flow

1. Use conventional commits (`feat:`, `fix:`, etc.) and push to `master`.
2. Release-please automatically opens a PR with version bump (`Cargo.toml`), `CHANGELOG.md`, and manifest update.
3. Merge the release PR → git tag + GitHub Release + 6 binary builds (linux/macos/windows × amd64/arm64).

### Build targets

Linux targets use `cargo-zigbuild` with `*-unknown-linux-musl` so the resulting binaries are **statically linked** and run on any distro (Alpine, Debian 11, RHEL 8, minimal containers — no glibc version dependency). macOS and Windows targets build natively with the stable Rust toolchain. New Linux-only system deps must either be vendored into the crate or compile cleanly under musl; glibc-only shared libraries will break the release build.

### Version bump rules

| Commit prefix | Bump | Example |
|---|---|---|
| `fix:` | patch (0.1.0 → 0.1.1) | `fix: handle empty diff` |
| `feat:` | minor (0.1.0 → 0.2.0) | `feat: add Kotlin support` |
| `feat!:` / `BREAKING CHANGE:` | major* | `feat!: new config format` |

\* While < 1.0.0, breaking changes bump minor (controlled by `bump-minor-pre-major` in `release-please-config.json`).

### Makefile targets

| Target | What it does | When to use |
|---|---|---|
| `make setup` | Installs `rustfmt` + `clippy` + `cargo-edit` | After first clone |
| `make pre-commit` | `fmt` + `clippy` + `test` | Before every commit |
| `make ci` | `fmt-check` + `clippy` + `test` | To mirror GitHub Actions locally |
| `make upgrade-check` | `cargo upgrade --incompatible --dry-run` (dev-deps excluded) | To preview dep upgrades |
| `make upgrade` | Apply dep upgrades (dev-deps left untouched) + `cargo update` | When bumping deps |

## Commit Messages

Follow conventional commit format:

```
type: short description

Optional longer body explaining why, not what.
```

Types: `feat`, `fix`, `refactor`, `test`, `docs`, `chore`, `perf`, `ci`.

Examples:
- `feat: add Kotlin language support`
- `fix: handle empty commit diff gracefully`
- `refactor: extract progress bar setup into helper`
- `test: add integration test for HTML output`
