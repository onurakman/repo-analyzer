pub mod age;
pub mod authors;
pub mod bloat;
pub mod branches;
pub mod churn;
pub mod coupling;
pub mod hotspots;
pub mod outliers;
pub mod ownership;
pub mod patterns;
pub mod quality;

use crate::types::{MetricResult, ParsedChange};

#[allow(dead_code)]
pub trait MetricCollector: Send + Sync {
    fn name(&self) -> &str;
    fn process(&mut self, change: &ParsedChange);
    fn finalize(&mut self) -> MetricResult;

    /// Optional hook invoked after commit walk completes, before `finalize()`.
    /// Collectors that need repo-level state (refs, object db) override this.
    /// Default: no-op.
    fn inspect_repo(&mut self, _repo: &gix::Repository) -> anyhow::Result<()> {
        Ok(())
    }
}
