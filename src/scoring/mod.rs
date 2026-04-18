//! Post-finalize synthesis passes.
//!
//! Unlike `src/metrics`, these don't collect data from commits — they read
//! the finalized [`MetricResult`]s that collectors produce and derive
//! higher-level signals (health score, recommendations, etc.).

pub mod health;
