use std::sync::Arc;

use indicatif::{HumanDuration, ProgressBar, ProgressStyle};
use rayon::prelude::*;

use crate::git::diff::DiffExtractor;
use crate::git::walker::GitWalker;
use crate::metrics::MetricCollector;
use crate::metrics::age::AgeCollector;
use crate::metrics::authors::AuthorsCollector;
use crate::metrics::bloat::BloatCollector;
use crate::metrics::branches::BranchesCollector;
use crate::metrics::churn::ChurnCollector;
use crate::metrics::coupling::CouplingCollector;
use crate::metrics::hotspots::HotspotsCollector;
use crate::metrics::outliers::OutliersCollector;
use crate::metrics::ownership::OwnershipCollector;
use crate::metrics::patterns::PatternsCollector;
use crate::metrics::quality::QualityCollector;
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

        // 2. Walk all commits
        let walker = GitWalker::new(
            self.config.repo_path.clone(),
            self.config.time_range.clone(),
        );

        let spinner = if self.config.quiet {
            ProgressBar::hidden()
        } else {
            let sp = ProgressBar::new_spinner();
            sp.set_style(
                ProgressStyle::default_spinner()
                    .template("{spinner:.green} {msg}")
                    .unwrap(),
            );
            sp.set_message("Walking commits...");
            sp
        };

        let mut commits = Vec::new();
        walker.walk(|ci| {
            commits.push(ci);
            spinner.tick();
            Ok(())
        })?;
        spinner.finish_and_clear();

        let total = commits.len();
        if total == 0 {
            // No commits to analyze; return empty results from collectors
            let mut collectors = self.create_collectors();
            return Ok(collectors.iter_mut().map(|c| c.finalize()).collect());
        }

        // 3. Set up progress bar (shared across threads — indicatif uses Arc internally)
        let pb = Arc::new(if self.config.quiet {
            ProgressBar::hidden()
        } else {
            let bar = ProgressBar::new(total as u64);
            bar.set_style(
                ProgressStyle::default_bar()
                    .template(
                        "{pos}/{len} commits [{bar:40.cyan/blue}] {elapsed_precise} ETA {eta} {msg}",
                    )
                    .unwrap()
                    .progress_chars("=> "),
            );
            bar
        });

        // 4. Create collectors
        let mut collectors = self.create_collectors();

        // 5. Open gix repo once (thread-safe handle, cheap to clone into workers)
        let thread_safe_repo = Arc::new(gix::ThreadSafeRepository::open(&self.config.repo_path)?);
        let diff_extractor = Arc::new(DiffExtractor::new(thread_safe_repo.clone()));

        // 6. Spawn a collector thread that drains ParsedChanges from a channel.
        //    Parallel workers produce; this thread consumes — collectors stay single-owner.
        let (tx, rx) = crossbeam_channel::bounded::<Vec<ParsedChange>>(256);

        let collector_handle = std::thread::spawn(move || {
            for batch in rx {
                for change in &batch {
                    for c in collectors.iter_mut() {
                        c.process(change);
                    }
                }
            }
            collectors
        });

        // 7. Process commits in parallel
        let quiet = self.config.quiet;
        let registry = &self.registry;

        commits.par_iter().for_each_with(tx.clone(), |tx, commit| {
            let diff_records = match diff_extractor.extract(commit) {
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

            let thread_repo = thread_safe_repo.to_thread_local();

            let mut changes = Vec::with_capacity(diff_records.len());
            for record in diff_records {
                if is_lock_file(&record.file_path) {
                    continue;
                }
                changes.push(parse_diff_record(registry, &thread_repo, &record));
            }

            if !changes.is_empty() {
                let _ = tx.send(changes);
            }
            pb.inc(1);
        });

        // Close the channel by dropping the original sender (workers dropped their clones).
        drop(tx);

        let mut collectors = collector_handle
            .join()
            .map_err(|_| anyhow::anyhow!("collector thread panicked"))?;

        // 8. Repo-level inspection (branches, bloat) — runs after commit walk.
        {
            let repo = thread_safe_repo.to_thread_local();
            for collector in collectors.iter_mut() {
                if let Err(e) = collector.inspect_repo(&repo)
                    && !self.config.quiet
                {
                    pb.println(format!(
                        "Warning: {} inspect_repo failed: {}",
                        collector.name(),
                        e
                    ));
                }
            }
        }

        pb.finish_with_message(format!(
            "Analyzed {} commits in {}",
            total,
            HumanDuration(start.elapsed())
        ));

        // 9. Finalize all collectors
        let results = collectors.iter_mut().map(|c| c.finalize()).collect();

        Ok(results)
    }

    /// Create metric collectors based on configured report kinds.
    fn create_collectors(&self) -> Vec<Box<dyn MetricCollector>> {
        let mut collectors: Vec<Box<dyn MetricCollector>> = Vec::new();

        for kind in &self.config.report_kinds {
            let collector: Box<dyn MetricCollector> = match kind {
                ReportKind::Authors => Box::new(AuthorsCollector::new()),
                ReportKind::Hotspots => Box::new(HotspotsCollector::new()),
                ReportKind::Churn => Box::new(ChurnCollector::new()),
                ReportKind::Ownership => Box::new(OwnershipCollector::new()),
                ReportKind::Coupling => Box::new(CouplingCollector::new()),
                ReportKind::Patterns => Box::new(PatternsCollector::new()),
                ReportKind::Age => Box::new(AgeCollector::new()),
                ReportKind::Branches => Box::new(BranchesCollector::new()),
                ReportKind::Bloat => Box::new(BloatCollector::new()),
                ReportKind::Outliers => Box::new(OutliersCollector::new()),
                ReportKind::Quality => Box::new(QualityCollector::new()),
            };
            collectors.push(collector);
        }

        collectors
    }
}

/// Convert a DiffRecord into a ParsedChange, optionally enriched with tree-sitter constructs.
/// Free function so it can be called from rayon worker threads without borrowing Pipeline.
fn parse_diff_record(
    registry: &LanguageRegistry,
    repo: &gix::Repository,
    record: &DiffRecord,
) -> ParsedChange {
    let constructs = get_file_content_at_commit(repo, &record.commit.oid, &record.file_path)
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
        .unwrap_or_default();

    ParsedChange {
        diff: record.clone(),
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
