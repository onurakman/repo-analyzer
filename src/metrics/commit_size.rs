//! Commit size distribution and anomaly detection.
//!
//! `quality` already flags individual mega-commits (lines > fixed threshold).
//! This report adds the *distribution view*: mean / median / p95 / p99 / max
//! across all commits, plus the specific commits that blow past the
//! repo-relative outlier threshold. Using repo-relative percentiles (not a
//! fixed line count) keeps the signal meaningful across projects of any size.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{
    Column, EntryGroup, MetricEntry, MetricResult, MetricValue, report_description, report_display,
};

/// Cap on how many individual anomaly commits we surface. The summary row
/// always reports the full anomaly count; the per-commit detail list is
/// capped so a runaway CI job doesn't produce a 10k-row table.
const MAX_ANOMALIES: usize = 25;

/// A commit qualifies as an anomaly if its total lines-changed exceeds
/// `max(10 * p95, p99)`. Using the max avoids a pathological `p95 == 0`
/// on repos with many tiny commits (docs-only, lockfile bumps).
const ANOMALY_P95_MULTIPLIER: f64 = 10.0;

pub struct CommitSizeCollector;

impl Default for CommitSizeCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl CommitSizeCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for CommitSizeCollector {
    fn name(&self) -> &str {
        "commit_size"
    }

    fn finalize(&mut self) -> MetricResult {
        empty_result()
    }

    fn finalize_from_db(
        &mut self,
        store: &ChangeStore,
        _progress: &crate::metrics::ProgressReporter,
    ) -> Option<MetricResult> {
        // Aggregate to one row per commit: total lines changed + minimal
        // metadata for the anomaly list. Sort is done in-memory so we can
        // compute percentiles over the full distribution.
        let rows = store
            .with_conn(|conn| -> anyhow::Result<Vec<CommitRow>> {
                let mut stmt = conn.prepare(
                    "SELECT commit_oid,
                                SUM(additions + deletions) AS size,
                                MIN(email)                 AS email,
                                MIN(commit_ts)             AS ts,
                                COALESCE(MIN(message), '') AS message
                           FROM changes
                          GROUP BY commit_oid",
                )?;
                let it = stmt.query_map([], |row| {
                    let oid: String = row.get(0)?;
                    let size: i64 = row.get(1)?;
                    let email: String = row.get(2)?;
                    let ts: i64 = row.get(3)?;
                    let msg: String = row.get(4)?;
                    Ok((oid, size.max(0), email, ts, msg))
                })?;
                let mut out = Vec::new();
                for r in it {
                    out.push(r?);
                }
                Ok(out)
            })
            .ok()?
            .ok()?;

        if rows.is_empty() {
            return None;
        }

        let total_commits = rows.len();
        let mut sizes: Vec<u64> = rows.iter().map(|(_, s, _, _, _)| *s as u64).collect();
        sizes.sort_unstable();

        let stats = Stats::from_sorted(&sizes);
        let threshold = anomaly_threshold(&stats);

        // Filter + rank anomalies. The full `anomaly_count` goes into the
        // summary row; the detail list is capped at `MAX_ANOMALIES`.
        let mut anomaly_rows: Vec<CommitRow> = rows
            .into_iter()
            .filter(|(_, s, _, _, _)| (*s as u64) > threshold)
            .collect();
        anomaly_rows.sort_by_key(|r| std::cmp::Reverse(r.1));
        let anomaly_count = anomaly_rows.len();
        anomaly_rows.truncate(MAX_ANOMALIES);

        let summary_entry = MetricEntry {
            key: "overall".into(),
            values: summary_values(total_commits, &stats, anomaly_count),
        };

        let anomaly_entries: Vec<MetricEntry> =
            anomaly_rows.into_iter().map(to_anomaly_entry).collect();

        Some(MetricResult {
            name: "commit_size".into(),
            display_name: report_display("commit_size"),
            description: report_description("commit_size"),
            columns: vec![
                // Summary-only columns (filled on the single "overall" row).
                Column::in_report("commit_size", "commits"),
                Column::in_report("commit_size", "mean"),
                Column::in_report("commit_size", "median"),
                Column::in_report("commit_size", "p95"),
                Column::in_report("commit_size", "p99"),
                Column::in_report("commit_size", "max"),
                Column::in_report("commit_size", "anomaly_count"),
                // Anomaly-only columns.
                Column::in_report("commit_size", "lines_changed"),
                Column::in_report("commit_size", "author"),
                Column::in_report("commit_size", "date"),
                Column::in_report("commit_size", "message"),
            ],
            entries: vec![],
            entry_groups: vec![
                EntryGroup {
                    name: "summary".into(),
                    label: "commit_size.group.summary".into(),
                    entries: vec![summary_entry],
                },
                EntryGroup {
                    name: "anomalies".into(),
                    label: "commit_size.group.anomalies".into(),
                    entries: anomaly_entries,
                },
            ],
        })
    }
}

struct Stats {
    mean: u64,
    median: u64,
    p95: u64,
    p99: u64,
    max: u64,
}

impl Stats {
    fn from_sorted(sizes: &[u64]) -> Self {
        let n = sizes.len();
        debug_assert!(n > 0);
        let sum: u64 = sizes.iter().sum();
        let mean = (sum as f64 / n as f64).round() as u64;
        let median = sizes[n / 2];
        let p95 = sizes[((n as f64 * 0.95) as usize).min(n - 1)];
        let p99 = sizes[((n as f64 * 0.99) as usize).min(n - 1)];
        let max = *sizes.last().unwrap_or(&0);
        Self {
            mean,
            median,
            p95,
            p99,
            max,
        }
    }
}

fn anomaly_threshold(stats: &Stats) -> u64 {
    ((stats.p95 as f64 * ANOMALY_P95_MULTIPLIER) as u64).max(stats.p99)
}

fn summary_values(
    total_commits: usize,
    stats: &Stats,
    anomaly_count: usize,
) -> HashMap<String, MetricValue> {
    let mut v = HashMap::new();
    v.insert("commits".into(), MetricValue::Count(total_commits as u64));
    v.insert("mean".into(), MetricValue::Count(stats.mean));
    v.insert("median".into(), MetricValue::Count(stats.median));
    v.insert("p95".into(), MetricValue::Count(stats.p95));
    v.insert("p99".into(), MetricValue::Count(stats.p99));
    v.insert("max".into(), MetricValue::Count(stats.max));
    v.insert(
        "anomaly_count".into(),
        MetricValue::Count(anomaly_count as u64),
    );
    v
}

type CommitRow = (String, i64, String, i64, String);

fn to_anomaly_entry(row: CommitRow) -> MetricEntry {
    let (oid, size, email, ts, msg) = row;
    let short_oid: String = oid.chars().take(12).collect();
    let first_line: String = msg.lines().next().unwrap_or("").chars().take(80).collect();
    let mut values = HashMap::new();
    values.insert("lines_changed".into(), MetricValue::Count(size as u64));
    values.insert("author".into(), MetricValue::Text(email));
    values.insert("date".into(), MetricValue::Date(ts_to_date(ts)));
    values.insert("message".into(), MetricValue::Text(first_line));
    MetricEntry {
        key: short_oid,
        values,
    }
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "commit_size".into(),
        display_name: report_display("commit_size"),
        description: report_description("commit_size"),
        columns: vec![],
        entries: vec![],
        entry_groups: vec![],
    }
}

fn ts_to_date(ts: i64) -> NaiveDate {
    let dt: DateTime<Utc> = Utc.timestamp_opt(ts, 0).single().unwrap_or_default();
    dt.date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_collector_produces_named_result() {
        let mut coll = CommitSizeCollector::new();
        let result = coll.finalize();
        assert_eq!(result.name, "commit_size");
    }

    #[test]
    fn stats_from_tiny_distribution() {
        let sizes = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 100];
        let s = Stats::from_sorted(&sizes);
        assert_eq!(s.max, 100);
        // p95 index = floor(10 * 0.95) = 9 → value 100.
        assert_eq!(s.p95, 100);
        // mean of that set ≈ 14.5 → rounds to 15.
        assert_eq!(s.mean, 15);
    }

    #[test]
    fn anomaly_threshold_uses_max_of_p95x10_and_p99() {
        // p95 small but p99 large — threshold should track p99.
        let stats = Stats {
            mean: 10,
            median: 5,
            p95: 0,
            p99: 5_000,
            max: 10_000,
        };
        assert_eq!(anomaly_threshold(&stats), 5_000);
        // p95 large, p99 ~same — threshold should track p95 * 10.
        let stats = Stats {
            mean: 100,
            median: 50,
            p95: 200,
            p99: 250,
            max: 10_000,
        };
        assert_eq!(anomaly_threshold(&stats), 2_000);
    }
}
