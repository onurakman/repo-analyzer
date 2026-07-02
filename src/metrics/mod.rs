pub mod age;
pub mod authors;
pub mod bloat;
pub mod branches;
pub mod churn;
pub mod churn_pareto;
pub mod clones;
pub mod commit_size;
pub mod commit_velocity;
pub mod complexity;
pub mod composition;
pub mod construct_churn;
pub mod construct_ownership;
pub mod coupling;
pub mod dead_code;
pub mod debt_markers;
pub mod doc_coverage;
pub mod fan_in_out;
pub mod half_life;
pub mod hotspots;
pub mod knowledge_silos;
pub mod large_sources;
pub mod module_coupling;
pub mod outliers;
pub mod ownership;
pub mod patterns;
pub mod quality;
pub mod succession;
pub mod test_ratio;

use indicatif::ProgressBar;

use crate::store::ChangeStore;
use crate::types::{MetricResult, ParsedChange};

/// Lightweight reporter handed to collectors so they can print sub-phase
/// updates through the same indicatif bar the pipeline owns. Keeps the output
/// consistent (no interleaved eprintln noise above the bar) and lets collectors
/// stay ignorant of whether the bar is hidden in `--quiet` mode.
#[derive(Clone)]
pub struct ProgressReporter {
    bar: Option<ProgressBar>,
}

impl ProgressReporter {
    pub fn new(bar: Option<ProgressBar>) -> Self {
        Self { bar }
    }

    /// Update the bar's message in place. No-op in quiet mode. Replaces any
    /// previous sub-status, so callers should publish a short, self-contained
    /// phase label ("parsed N files", "pass 2/2"), not a scrolling log.
    pub fn status(&self, msg: &str) {
        if let Some(bar) = &self.bar {
            bar.set_message(msg.to_string());
        }
    }
}

/// A collector that analyzes each source file's parsed syntax tree at HEAD.
///
/// The pipeline walks the HEAD tree ONCE, parses each source blob ONCE with the
/// tree-sitter grammar for its extension, and dispatches the shared tree to
/// every scanner — replacing N independent tree walks + re-parses (one per
/// collector) with a single shared scan. (audit finding #23)
pub trait SourceScanner {
    /// Called once per source file at HEAD. `tree` is parsed with the grammar
    /// for the file's extension; the scanner re-derives its own per-language
    /// spec by path and runs its analysis on the shared tree.
    fn scan_file(&mut self, path: &str, source: &str, tree: &tree_sitter::Tree);
}

#[allow(dead_code)]
pub trait MetricCollector: Send + Sync {
    fn name(&self) -> &str;

    /// If this collector analyzes parsed source trees, return itself as a
    /// [`SourceScanner`] so the pipeline drives it via the single shared HEAD
    /// scan instead of a private per-collector walk. Default: not a scanner.
    /// Scanners must leave [`inspect_repo`](Self::inspect_repo) a no-op — the
    /// shared scan replaces it. (audit finding #23)
    fn as_source_scanner(&mut self) -> Option<&mut dyn SourceScanner> {
        None
    }

    /// Default in-memory processing path. Called by the pipeline for every
    /// parsed change when the collector does not override `finalize_from_db`.
    /// Collectors that derive their results from the SQLite change store
    /// leave this as a no-op.
    fn process(&mut self, _change: &ParsedChange) {}

    fn finalize(&mut self) -> MetricResult;

    /// Optional hook invoked after the commit walk completes, before `finalize()`.
    /// Collectors that need repo-level state (refs, object db) override this.
    /// Default: no-op.
    fn inspect_repo(
        &mut self,
        _repo: &gix::Repository,
        _progress: &ProgressReporter,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Optional disk-backed finalization. The three-way return disambiguates:
    /// - `None` — this collector doesn't use the DB path; the pipeline falls
    ///   back to the in-memory [`finalize`](Self::finalize).
    /// - `Some(Ok(result))` — use this result and skip `finalize()`.
    /// - `Some(Err(e))` — the collector uses the DB path but its query failed;
    ///   the pipeline warns and falls back to `finalize()` rather than silently
    ///   emitting an empty report. (audit finding #5)
    ///
    /// Collectors that aggregate per-change data should override this and run
    /// their SQL query against the shared [`ChangeStore`] so aggregation state
    /// lives on disk instead of RAM.
    fn finalize_from_db(
        &mut self,
        _store: &ChangeStore,
        _progress: &ProgressReporter,
    ) -> Option<anyhow::Result<MetricResult>> {
        None
    }
}
