pub mod age;
pub mod authors;
pub mod bloat;
pub mod churn;
pub mod churn_pareto;
pub mod complexity;
pub mod composition;
pub mod construct_churn;
pub mod construct_ownership;
pub mod coupling;
pub mod fan_in_out;
pub mod half_life;
pub mod hotspots;
pub mod knowledge_silos;
pub mod module_coupling;
pub mod outliers;
pub mod ownership;
pub mod patterns;
pub mod quality;
pub mod succession;

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

#[allow(dead_code)]
pub trait MetricCollector: Send + Sync {
    fn name(&self) -> &str;

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

    /// Optional disk-backed finalization. When `Some(result)` is returned, the
    /// pipeline uses this result and skips the in-memory `finalize()` path.
    /// Collectors that aggregate per-change data should override this and run
    /// their SQL query against the shared [`ChangeStore`] so aggregation state
    /// lives on disk instead of RAM.
    fn finalize_from_db(
        &mut self,
        _store: &ChangeStore,
        _progress: &ProgressReporter,
    ) -> Option<MetricResult> {
        None
    }
}
