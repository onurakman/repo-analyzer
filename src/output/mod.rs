pub mod csv_output;
pub mod html;
pub mod json;
pub mod terminal;

use crate::types::{MetricResult, OutputConfig};

pub trait ReportWriter {
    fn write(&self, results: &[MetricResult], config: &OutputConfig) -> anyhow::Result<()>;
}
