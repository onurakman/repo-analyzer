use std::io::{BufRead, Write};
use std::process::Command;
use std::sync::Arc;

use indicatif::{HumanDuration, ProgressBar, ProgressStyle};
use rayon::prelude::*;

use crate::git::diff::DiffExtractor;
use crate::git::walker::GitWalker;
use crate::metrics::MetricCollector;
use crate::metrics::age::AgeCollector;
use crate::metrics::authors::AuthorsCollector;
use crate::metrics::bloat::BloatCollector;
use crate::metrics::churn::ChurnCollector;
use crate::metrics::churn_pareto::ChurnParetoCollector;
use crate::metrics::clones::ClonesCollector;
use crate::metrics::commit_size::CommitSizeCollector;
use crate::metrics::commit_velocity::CommitVelocityCollector;
use crate::metrics::complexity::ComplexityCollector;
use crate::metrics::composition::CompositionCollector;
use crate::metrics::construct_churn::ConstructChurnCollector;
use crate::metrics::construct_ownership::ConstructOwnershipCollector;
use crate::metrics::coupling::CouplingCollector;
use crate::metrics::dead_code::DeadCodeCollector;
use crate::metrics::debt_markers::DebtMarkersCollector;
use crate::metrics::doc_coverage::DocCoverageCollector;
use crate::metrics::fan_in_out::FanInOutCollector;
use crate::metrics::half_life::HalfLifeCollector;
use crate::metrics::hotspots::HotspotsCollector;
use crate::metrics::knowledge_silos::KnowledgeSilosCollector;
use crate::metrics::large_sources::LargeSourcesCollector;
use crate::metrics::module_coupling::ModuleCouplingCollector;
use crate::metrics::outliers::OutliersCollector;
use crate::metrics::ownership::OwnershipCollector;
use crate::metrics::patterns::PatternsCollector;
use crate::metrics::quality::QualityCollector;
use crate::metrics::succession::SuccessionCollector;
use crate::metrics::test_ratio::TestRatioCollector;
use crate::parser::registry::LanguageRegistry;
use crate::types::{DiffRecord, MetricResult, ParsedChange, ReportKind, TimeRange};

/// Known lock file names that should be excluded from analysis.
const LOCK_FILE_NAMES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
    "uv.lock",
    "pnpm-lock.yaml",
    "Gemfile.lock",
    "poetry.lock",
    "composer.lock",
    "go.sum",
    "Pipfile.lock",
    "flake.lock",
    "packages.lock.json",
    "pubspec.lock",
];

/// Returns `true` if the file path ends with a known lock file name.
fn is_lock_file(path: &str) -> bool {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    LOCK_FILE_NAMES.contains(&file_name)
}

/// Configuration for a pipeline run.
pub struct PipelineConfig {
    pub repo_path: String,
    pub time_range: TimeRange,
    pub report_kinds: Vec<ReportKind>,
    pub quiet: bool,
    pub threads: Option<usize>,
    /// Capacity of the commit and changes channels. Controls peak in-flight
    /// memory: fewer slots = tighter backpressure.
    pub channel_capacity: usize,
    /// Max parsed changes per batch sent to the writer thread.
    pub batch_size: usize,
    /// Per-thread `gix` object cache size, in MiB.
    pub object_cache_mb: usize,
    /// When `true`, a shallow repository is unshallowed automatically without
    /// prompting. Intended for CI pipelines that pair this with `--quiet`.
    pub unshallow: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            repo_path: ".".to_string(),
            time_range: TimeRange::All,
            report_kinds: ReportKind::default_set(),
            quiet: false,
            threads: None,
            channel_capacity: 4,
            batch_size: 64,
            object_cache_mb: 4,
            unshallow: false,
        }
    }
}

/// Orchestrates the full analysis pipeline:
/// walk commits -> extract diffs -> parse with tree-sitter -> feed to metric collectors -> return results.
pub struct Pipeline {
    config: PipelineConfig,
    registry: LanguageRegistry,
}

impl Pipeline {
    /// Create a new pipeline with the given configuration and language registry.
    pub fn new(config: PipelineConfig, registry: LanguageRegistry) -> Self {
        Self { config, registry }
    }

    /// Run the full analysis pipeline and return metric results.
    pub fn run(&self) -> anyhow::Result<Vec<MetricResult>> {
        let start = std::time::Instant::now();

        // 1. Configure rayon thread pool if threads specified
        if let Some(threads) = self.config.threads {
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build_global()
                .ok(); // Ignore error if pool already initialized
        }

        // Shallow repos break every history-based metric. Three paths:
        //   1. `--unshallow` / `-u` → just run it, no prompt. CI path.
        //   2. quiet (no --unshallow) → bail with instructions.
        //   3. interactive           → prompt the user; abort on "no".
        if gix::open(&self.config.repo_path)?.is_shallow() {
            if self.config.unshallow {
                if !self.config.quiet {
                    eprintln!(
                        "note: {} is a shallow clone — unshallowing (--unshallow)",
                        self.config.repo_path
                    );
                }
                run_unshallow(&self.config.repo_path)?;
            } else if self.config.quiet {
                anyhow::bail!(
                    "repository at {} is a shallow clone; all metrics depend on full history. \
                     Run `git fetch --unshallow` first, or re-run with `--unshallow`.",
                    self.config.repo_path
                );
            } else {
                if !prompt_unshallow(&self.config.repo_path)? {
                    anyhow::bail!("aborted: shallow repository, user declined to unshallow");
                }
                run_unshallow(&self.config.repo_path)?;
            }
        }

        // 2. Count commits up front with a cheap walk (just traverses oids).
        //    We do NOT buffer CommitInfo in memory — the second pass below
        //    streams them through a bounded channel so peak RAM stays flat
        //    regardless of history length.
        let interner = Arc::new(crate::interner::Interner::new());

        let spinner = if self.config.quiet {
            ProgressBar::hidden()
        } else {
            let sp = ProgressBar::new_spinner();
            sp.set_style(
                ProgressStyle::default_spinner()
                    .template("{spinner:.green} {msg}")
                    .unwrap(),
            );
            sp.set_message("[1/5] Counting commits...");
            sp
        };

        let count_walker = GitWalker::new(
            self.config.repo_path.clone(),
            self.config.time_range.clone(),
            interner.clone(),
        );
        let mut total: usize = 0;
        count_walker.walk(|_| {
            total += 1;
            spinner.tick();
            Ok(())
        })?;
        spinner.finish_and_clear();

        if total == 0 {
            // No commits to analyze; return empty results from collectors
            let mut collectors = self.create_collectors();
            let empty_results: Vec<MetricResult> =
                collectors.iter_mut().map(|c| c.finalize()).collect();
            return Ok(empty_results);
        }

        // 3. Set up progress bar (shared across threads — indicatif uses Arc internally)
        let pb = Arc::new(if self.config.quiet {
            ProgressBar::hidden()
        } else {
            let bar = ProgressBar::new(total as u64);
            bar.set_style(
                ProgressStyle::default_bar()
                    .template(
                        "{spinner:.green} {pos}/{len} ({percent:>3}%) commits [{bar:40.cyan/blue}] {elapsed_precise} ETA {eta} {msg}",
                    )
                    .unwrap()
                    .progress_chars("=> "),
            );
            bar.set_message("[2/5] walking commits");
            bar.enable_steady_tick(std::time::Duration::from_millis(120));
            bar
        });

        // 4. Create collectors
        let mut collectors = self.create_collectors();

        // 5. Open gix repo once (thread-safe handle, cheap to clone into workers)
        let thread_safe_repo = Arc::new(gix::ThreadSafeRepository::open(&self.config.repo_path)?);
        let diff_extractor = Arc::new(DiffExtractor::new(
            thread_safe_repo.clone(),
            interner.clone(),
        ));

        // 6. Open the disk-backed change store and spawn a writer thread that
        //    drains ParsedChange batches from the channel into SQLite. All
        //    process-based collectors read from the DB at finalize time.
        let store = Arc::new(crate::store::ChangeStore::open_temp()?);
        // Small channel + small per-batch cap so a single giant merge commit
        // cannot queue hundreds of MB of parsed constructs in flight. The
        // sizes are tunable via the `--channel-capacity` and `--batch-size`
        // CLI flags for operators on tight pods.
        let channel_capacity = self.config.channel_capacity.max(1);
        let max_changes_per_batch = self.config.batch_size.max(1);
        let (changes_tx, changes_rx) =
            crossbeam_channel::bounded::<Vec<ParsedChange>>(channel_capacity);

        let writer_store = store.clone();
        let writer_handle = std::thread::spawn(move || -> anyhow::Result<()> {
            for batch in changes_rx {
                writer_store.insert_batch(&batch)?;
            }
            Ok(())
        });

        // 7. Stream commits: a producer thread walks the history and pushes
        //    each CommitInfo into a bounded channel. rayon workers pull from
        //    the channel via `par_bridge` and do the expensive extract + parse
        //    work in parallel. Because nothing is collected into a Vec, peak
        //    RAM stays constant regardless of history length.
        let quiet = self.config.quiet;
        let registry = &self.registry;

        let (commit_tx, commit_rx) =
            crossbeam_channel::bounded::<Arc<crate::types::CommitInfo>>(channel_capacity);
        let producer_interner = interner.clone();
        let producer_repo_path = self.config.repo_path.clone();
        let producer_time_range = self.config.time_range.clone();
        let producer_handle = std::thread::spawn(move || -> anyhow::Result<()> {
            let walker = GitWalker::new(producer_repo_path, producer_time_range, producer_interner);
            walker.walk(|ci| {
                // Send blocks when the channel is full — that's the backpressure
                // that keeps commits from piling up in memory.
                let _ = commit_tx.send(Arc::new(ci));
                Ok(())
            })?;
            drop(commit_tx);
            Ok(())
        });

        let object_cache_bytes = self.config.object_cache_mb.max(1) * 1024 * 1024;

        commit_rx
            .into_iter()
            .par_bridge()
            .for_each_with(changes_tx.clone(), |tx, commit| {
                let diff_records = match diff_extractor.extract(&commit) {
                    Ok(records) => records,
                    Err(e) => {
                        if !quiet {
                            pb.println(format!(
                                "Warning: skipping commit {}: {}",
                                &commit.oid[..8.min(commit.oid.len())],
                                e
                            ));
                        }
                        pb.inc(1);
                        return;
                    }
                };

                // Cap the per-thread object cache. gix's default keeps
                // decompressed blobs in memory — fine for a short-lived CLI
                // call on a small repo, but on a 32k-commit monorepo the
                // cumulative working set balloons into GBs. Tunable via
                // `--object-cache-mb`.
                let mut thread_repo = thread_safe_repo.to_thread_local();
                thread_repo.object_cache_size_if_unset(object_cache_bytes);

                // Split the commit's diff records into capped batches so a
                // single big merge commit doesn't queue hundreds of MBs of
                // parsed constructs in the channel all at once.
                let mut changes: Vec<ParsedChange> = Vec::with_capacity(max_changes_per_batch);
                for record in diff_records {
                    if is_lock_file(&record.file_path) {
                        continue;
                    }
                    changes.push(parse_diff_record(registry, &thread_repo, record));
                    if changes.len() >= max_changes_per_batch {
                        let _ = tx.send(std::mem::take(&mut changes));
                        changes = Vec::with_capacity(max_changes_per_batch);
                    }
                }
                if !changes.is_empty() {
                    let _ = tx.send(changes);
                }
                pb.inc(1);
            });

        // Close the channel by dropping the original sender (workers dropped their clones).
        drop(changes_tx);

        // Commit walking is done. Swap to a spinner *before* we wait on the
        // writer/producer threads so the bar doesn't sit at 100% looking
        // finished while writer drain + index build are still running.
        if !self.config.quiet {
            pb.set_style(
                ProgressStyle::default_spinner()
                    .template("{spinner:.green} [{elapsed_precise}] {msg}")
                    .unwrap(),
            );
            pb.enable_steady_tick(std::time::Duration::from_millis(120));
            pb.set_message("[2/5] draining writer...");
        }

        producer_handle
            .join()
            .map_err(|_| anyhow::anyhow!("commit producer thread panicked"))??;

        writer_handle
            .join()
            .map_err(|_| anyhow::anyhow!("store writer thread panicked"))??;

        // All changes are now in the DB. Build indexes for the query phase.
        pb.set_message("[3/5] Indexing change store...");
        store.finalize_indexes()?;

        // 8. Repo-level inspection (bloat, complexity, …) — runs after commit walk.
        // A single-line status shows which phase is active when memory or
        // time balloons (these phases can be much heavier than commit walking).
        let progress_bar_for_reporter = if self.config.quiet {
            None
        } else {
            Some(pb.as_ref().clone())
        };
        let reporter = crate::metrics::ProgressReporter::new(progress_bar_for_reporter);
        let total_collectors = collectors.len();
        {
            let repo = thread_safe_repo.to_thread_local();
            for (idx, collector) in collectors.iter_mut().enumerate() {
                pb.set_message(format!(
                    "[4/5] Inspecting ({}/{}) {}...",
                    idx + 1,
                    total_collectors,
                    collector.name()
                ));
                if let Err(e) = collector.inspect_repo(&repo, &reporter)
                    && !self.config.quiet
                {
                    // Warnings stay in the scrollback — one-off info the
                    // user still needs to see after the bar is gone.
                    pb.println(format!(
                        "Warning: {} inspect_repo failed: {}",
                        collector.name(),
                        e
                    ));
                }
            }
        }

        // 9. Finalize all collectors. Collectors that aggregate per-change data
        //    override `finalize_from_db` and query the change store; the rest
        //    still compute in-memory in `finalize()`.
        let mut results: Vec<MetricResult> = collectors
            .iter_mut()
            .enumerate()
            .map(|(idx, c)| {
                pb.set_message(format!(
                    "[5/5] Computing ({}/{}) {}...",
                    idx + 1,
                    total_collectors,
                    c.name()
                ));
                c.finalize_from_db(&store, &reporter)
                    .unwrap_or_else(|| c.finalize())
            })
            .collect();

        // Health is a synthesis pass, not a collector — it reads the other
        // finalized reports and derives a score + action list. Prepend it so
        // it appears first in the output.
        if self.config.report_kinds.contains(&ReportKind::Health) {
            pb.set_message("[5/5] Computing health score...");
            if let Some(health) = crate::scoring::health::compute_health(
                &results,
                std::path::Path::new(&self.config.repo_path),
            ) {
                results.insert(0, health);
            }
        }

        if !self.config.quiet {
            pb.disable_steady_tick();
        }
        pb.finish_with_message(format!(
            "Analyzed {} commits in {}",
            total,
            HumanDuration(start.elapsed())
        ));

        Ok(results)
    }

    /// Create metric collectors based on configured report kinds.
    fn create_collectors(&self) -> Vec<Box<dyn MetricCollector>> {
        let mut collectors: Vec<Box<dyn MetricCollector>> = Vec::new();

        for kind in &self.config.report_kinds {
            // Health is synthesised post-finalize from the other reports; it
            // doesn't collect anything per-commit, so skip it here.
            if matches!(kind, ReportKind::Health) {
                continue;
            }
            let collector: Box<dyn MetricCollector> = match kind {
                ReportKind::Health => unreachable!("filtered out above"),
                ReportKind::Authors => Box::new(AuthorsCollector::new()),
                ReportKind::Hotspots => Box::new(HotspotsCollector::new()),
                ReportKind::Churn => Box::new(ChurnCollector::new()),
                ReportKind::Ownership => Box::new(OwnershipCollector::new()),
                ReportKind::Coupling => Box::new(CouplingCollector::new()),
                ReportKind::Patterns => Box::new(PatternsCollector::new()),
                ReportKind::Age => Box::new(AgeCollector::new()),
                ReportKind::Bloat => Box::new(BloatCollector::new()),
                ReportKind::Outliers => Box::new(OutliersCollector::new()),
                ReportKind::Quality => Box::new(QualityCollector::new()),
                ReportKind::Complexity => Box::new(ComplexityCollector::new()),
                ReportKind::Composition => Box::new(CompositionCollector::new()),
                ReportKind::ConstructChurn => Box::new(ConstructChurnCollector::new()),
                ReportKind::DebtMarkers => Box::new(DebtMarkersCollector::new()),
                ReportKind::LargeSources => Box::new(LargeSourcesCollector::new()),
                ReportKind::HalfLife => Box::new(HalfLifeCollector::new()),
                ReportKind::Succession => Box::new(SuccessionCollector::new()),
                ReportKind::KnowledgeSilos => Box::new(KnowledgeSilosCollector::new()),
                ReportKind::FanInOut => Box::new(FanInOutCollector::new()),
                ReportKind::ModuleCoupling => Box::new(ModuleCouplingCollector::new()),
                ReportKind::ChurnPareto => Box::new(ChurnParetoCollector::new()),
                ReportKind::ConstructOwnership => Box::new(ConstructOwnershipCollector::new()),
                ReportKind::CommitVelocity => Box::new(CommitVelocityCollector::new()),
                ReportKind::CommitSize => Box::new(CommitSizeCollector::new()),
                ReportKind::DocCoverage => Box::new(DocCoverageCollector::new()),
                ReportKind::DeadCode => Box::new(DeadCodeCollector::new()),
                ReportKind::Clones => Box::new(ClonesCollector::new()),
                ReportKind::TestRatio => Box::new(TestRatioCollector::new()),
            };
            collectors.push(collector);
        }

        collectors
    }
}

/// Skip parsing source blobs above this size — usually minified bundles or
/// generated code where construct extraction is meaningless and parse trees
/// dominate peak memory.
const MAX_PARSE_BLOB_BYTES: u64 = 512 * 1024;

/// Convert a DiffRecord into a ParsedChange, optionally enriched with tree-sitter constructs.
/// Free function so it can be called from rayon worker threads without borrowing Pipeline.
///
/// Takes the `record` by value and wraps it in `Arc` rather than cloning it,
/// so per-file `ParsedChange`s in a single commit share one allocation.
fn parse_diff_record(
    registry: &LanguageRegistry,
    repo: &gix::Repository,
    record: DiffRecord,
) -> ParsedChange {
    // Short-circuit: if no parser is registered for this file extension, skip
    // loading the blob entirely. This avoids reading binaries (.png, .pdf,
    // generated bundles, etc.) into memory only to discard them.
    let constructs = if registry.get_for_file(&record.file_path).is_some()
        && blob_size_at_commit(repo, &record.commit.oid, &record.file_path)
            .map(|sz| sz <= MAX_PARSE_BLOB_BYTES)
            .unwrap_or(true)
    {
        get_file_content_at_commit(repo, &record.commit.oid, &record.file_path)
            .ok()
            .and_then(|content| {
                let line_ranges: Vec<(u32, u32)> = record
                    .hunks
                    .iter()
                    .map(|h| (h.new_start, h.new_start + h.new_lines.saturating_sub(1)))
                    .filter(|(s, e)| *s > 0 && *e >= *s)
                    .collect();

                if line_ranges.is_empty() {
                    return None;
                }

                registry.parse_constructs_in_ranges(&record.file_path, &content, &line_ranges)
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    ParsedChange {
        diff: Arc::new(record),
        constructs,
    }
}

/// Retrieve file content at a specific commit via gix (no subprocess).
fn get_file_content_at_commit(
    repo: &gix::Repository,
    oid: &str,
    file_path: &str,
) -> anyhow::Result<String> {
    let object_id = gix::ObjectId::from_hex(oid.as_bytes())?;
    let commit = repo.find_object(object_id)?.try_into_commit()?;
    let tree = commit.tree()?;
    let entry = tree
        .lookup_entry_by_path(file_path)?
        .ok_or_else(|| anyhow::anyhow!("path not found in tree: {}", file_path))?;
    let object = entry.object()?;
    Ok(String::from_utf8_lossy(&object.data).to_string())
}

/// Look up a blob's size in the tree at `oid` without loading its contents.
/// Returns `None` if the path or commit can't be resolved (callers default to "parse it").
fn blob_size_at_commit(repo: &gix::Repository, oid: &str, file_path: &str) -> Option<u64> {
    use gix::prelude::HeaderExt;
    let object_id = gix::ObjectId::from_hex(oid.as_bytes()).ok()?;
    let commit = repo.find_object(object_id).ok()?.try_into_commit().ok()?;
    let tree = commit.tree().ok()?;
    let entry = tree.lookup_entry_by_path(file_path).ok()??;
    let header = repo.objects.header(entry.oid()).ok()?;
    Some(header.size())
}

/// Prompt the user (stderr) to confirm unshallowing the repo. Returns true on `y`/`yes`.
fn prompt_unshallow(repo_path: &str) -> anyhow::Result<bool> {
    let mut stderr = std::io::stderr();
    write!(
        stderr,
        "warning: {} is a shallow clone — every metric needs full history.\nFetch full history now (`git fetch --unshallow`)? [y/N] ",
        repo_path
    )?;
    stderr.flush()?;

    let mut input = String::new();
    let stdin = std::io::stdin();
    stdin.lock().read_line(&mut input)?;
    let answer = input.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}

/// Run `git fetch --unshallow` in the repo. Surfaces git's stderr on failure.
fn run_unshallow(repo_path: &str) -> anyhow::Result<()> {
    eprintln!("Running `git fetch --unshallow` in {}...", repo_path);
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["fetch", "--unshallow"])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to invoke git: {e}"))?;

    if !output.status.success() {
        anyhow::bail!(
            "git fetch --unshallow failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    /// Helper: create a temporary git repo with 2 commits.
    fn create_test_repo() -> TempDir {
        let dir = TempDir::new().expect("failed to create temp dir");
        let path = dir.path();

        let run = |args: &[&str]| {
            let output = StdCommand::new("git")
                .args(args)
                .current_dir(path)
                .env("GIT_AUTHOR_NAME", "Test User")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test User")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .output()
                .expect("failed to run git command");
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };

        run(&["init"]);
        run(&["config", "user.name", "Test User"]);
        run(&["config", "user.email", "test@example.com"]);

        // First commit: add a Rust file
        std::fs::write(
            path.join("main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .expect("write failed");
        run(&["add", "main.rs"]);
        run(&["commit", "-m", "Initial commit"]);

        // Small delay to ensure different timestamps
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Second commit: modify the Rust file
        std::fs::write(
            path.join("main.rs"),
            "fn main() {\n    println!(\"hello world\");\n}\n\nfn helper() -> i32 {\n    42\n}\n",
        )
        .expect("write failed");
        run(&["add", "main.rs"]);
        run(&["commit", "-m", "Add helper function"]);

        dir
    }

    #[test]
    fn test_pipeline_runs_end_to_end() {
        let dir = create_test_repo();
        let repo_path = dir.path().to_str().unwrap().to_string();

        let config = PipelineConfig {
            repo_path,
            time_range: TimeRange::All,
            report_kinds: vec![ReportKind::Authors, ReportKind::Churn],
            quiet: true,
            threads: None,
            ..Default::default()
        };

        let registry = LanguageRegistry::build_default();
        let pipeline = Pipeline::new(config, registry);
        let results = pipeline.run().expect("pipeline should succeed");

        assert_eq!(
            results.len(),
            2,
            "should have 2 metric results (authors + churn)"
        );

        // First result should be authors
        assert_eq!(results[0].name, "authors");
        // Should have test@example.com as an author (grouped by email)
        let has_test_user = results[0]
            .entries
            .iter()
            .any(|e| e.key == "test@example.com");
        assert!(
            has_test_user,
            "should have 'test@example.com' in authors report"
        );

        // Second result should be churn
        assert_eq!(results[1].name, "churn");
        // Should have main.rs
        let has_main_rs = results[1].entries.iter().any(|e| e.key == "main.rs");
        assert!(has_main_rs, "should have 'main.rs' in churn report");
    }

    #[test]
    fn test_pipeline_empty_repo() {
        let dir = TempDir::new().expect("failed to create temp dir");
        let path = dir.path();

        let run = |args: &[&str]| {
            let output = StdCommand::new("git")
                .args(args)
                .current_dir(path)
                .env("GIT_AUTHOR_NAME", "Test User")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test User")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .output()
                .expect("failed to run git command");
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };

        run(&["init"]);
        run(&["config", "user.name", "Test User"]);
        run(&["config", "user.email", "test@example.com"]);

        // Single commit with a README
        std::fs::write(path.join("README.md"), "# Test\n").expect("write failed");
        run(&["add", "README.md"]);
        run(&["commit", "-m", "Add README"]);

        let repo_path = path.to_str().unwrap().to_string();

        let config = PipelineConfig {
            repo_path,
            time_range: TimeRange::All,
            report_kinds: vec![ReportKind::Authors],
            quiet: true,
            threads: None,
            ..Default::default()
        };

        let registry = LanguageRegistry::build_default();
        let pipeline = Pipeline::new(config, registry);
        let results = pipeline.run().expect("pipeline should succeed");

        assert_eq!(results.len(), 1, "should have 1 metric result (authors)");
        assert_eq!(results[0].name, "authors");

        // Should have test@example.com (grouped by email)
        let has_test_user = results[0]
            .entries
            .iter()
            .any(|e| e.key == "test@example.com");
        assert!(
            has_test_user,
            "should have 'test@example.com' in authors report"
        );
    }
}
