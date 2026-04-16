# AGENTS.md

Coding standards and architectural rules for AI agents working on the `repo-analyzer` codebase.

## Architecture

The application follows a streaming pipeline pattern:

```
Git Walker -> Diff Extractor -> Tree-sitter Parser -> Metric Collectors -> Report Writers
```

1. **Git Walker** (`src/git/walker.rs`) -- Iterates commits in the repository, filtered by time range.
2. **Diff Extractor** (`src/git/diff.rs`) -- Extracts per-file diff records (added/removed lines) from each commit.
3. **Language Registry** (`src/parser/registry.rs`) -- Maps file extensions to tree-sitter grammars and parses diffs into `CodeConstruct` values.
4. **Metric Collectors** (`src/metrics/`) -- Each collector implements `MetricCollector`, receives `ParsedChange` structs one at a time, and produces a `MetricResult` on finalize.
5. **Report Writers** (`src/output/`) -- Each writer implements `ReportWriter` and serializes `MetricResult` slices to a specific format.

The pipeline is orchestrated by `src/pipeline/engine.rs`. Rayon is used for thread pool configuration. Collectors are fed sequentially per commit but the design supports future parallelization.

## Code Style

- **Edition:** Rust 2024 (`edition = "2024"` in Cargo.toml).
- **Formatting:** Always run `cargo fmt` before committing.
- **Linting:** `cargo clippy -- -D warnings` must pass with zero warnings.
- **No `unwrap()` in non-test code.** Use `?` with `anyhow::Result` or return `Option`.
- **Error handling:**
  - `thiserror` for domain-specific error types inside modules.
  - `anyhow` in `main.rs` and at pipeline boundaries.
- **Dependencies:** Justify new crate additions. Prefer the existing stack (serde, clap, comfy-table, indicatif, crossbeam, rayon).

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
2. Implement `MetricCollector` for it:
   - `name()` -- return a static string matching the report kind.
   - `process(&mut self, change: &ParsedChange)` -- incrementally aggregate data. Do not store raw changes.
   - `finalize(&mut self) -> MetricResult` -- return columns and entries.
3. Add a variant to `ReportKind` in `src/types.rs`.
4. Update `ReportKind::all()`, `ReportKind::from_str()`, and `Display` impl in `src/types.rs`.
5. Update the `--only` help text in `src/cli.rs` to include the new name.
6. Add a match arm in `Pipeline::create_collectors()` in `src/pipeline/engine.rs`.
7. Add `pub mod <name>;` to `src/metrics/mod.rs`.
8. Write unit tests in the same file. Write an integration test in `tests/`.

## Adding a New Language

1. Add the `tree-sitter-<lang>` crate to `[dependencies]` in `Cargo.toml`.
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
5. Add tests for construct extraction in the language module.

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
- **Thread pool:** Rayon global thread pool is configured once via `--threads`. Default (`0`) lets Rayon auto-detect.
- **Progress indication:** Use `indicatif` progress bars. Respect `--quiet` by using `ProgressBar::hidden()`.

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
