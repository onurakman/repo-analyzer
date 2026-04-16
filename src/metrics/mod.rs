pub mod age;
pub mod authors;
pub mod churn;
pub mod coupling;
pub mod hotspots;
pub mod ownership;
pub mod patterns;

use crate::types::{MetricResult, ParsedChange};

#[allow(dead_code)]
pub trait MetricCollector: Send + Sync {
    fn name(&self) -> &str;
    fn process(&mut self, change: &ParsedChange);
    fn finalize(&mut self) -> MetricResult;
}
