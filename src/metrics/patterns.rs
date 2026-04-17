use std::collections::HashMap;

use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{MetricEntry, MetricResult, MetricValue};

pub struct PatternsCollector;

impl Default for PatternsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl PatternsCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for PatternsCollector {
    fn name(&self) -> &str {
        "patterns"
    }

    fn finalize(&mut self) -> MetricResult {
        empty_result()
    }

    fn finalize_from_db(
        &mut self,
        store: &ChangeStore,
        _progress: &crate::metrics::ProgressReporter,
    ) -> Option<MetricResult> {
        // SQLite %w is 0=Sun..6=Sat. Remap to 0=Mon..6=Sun so Monday is at index 0,
        // matching the original `%u` (1=Mon..7=Sun) / day_names layout.
        let (hourly, daily) = store
            .with_conn(|conn| -> anyhow::Result<([u64; 24], [u64; 7])> {
                let mut hourly = [0u64; 24];
                let mut daily = [0u64; 7];

                let mut stmt = conn.prepare(
                    "SELECT
                        CAST(strftime('%H', datetime(commit_ts, 'unixepoch')) AS INTEGER) AS hour,
                        CAST(strftime('%w', datetime(commit_ts, 'unixepoch')) AS INTEGER) AS dow,
                        COUNT(*) AS commits
                       FROM (
                          SELECT commit_oid, MIN(commit_ts) AS commit_ts
                            FROM changes GROUP BY commit_oid
                       )
                      GROUP BY hour, dow",
                )?;
                let rows = stmt.query_map([], |row| {
                    let hour: i64 = row.get(0)?;
                    let dow: i64 = row.get(1)?;
                    let cnt: i64 = row.get(2)?;
                    Ok((hour, dow, cnt as u64))
                })?;
                for r in rows {
                    let (hour, dow, cnt) = r?;
                    let h = hour.clamp(0, 23) as usize;
                    hourly[h] += cnt;
                    // SQLite 0=Sun..6=Sat. Our output slots 0=Mon..6=Sun.
                    let day_idx = match dow {
                        0 => 6,                // Sun → slot 6
                        n => (n - 1) as usize, // Mon..Sat → 0..5
                    };
                    daily[day_idx] += cnt;
                }
                Ok((hourly, daily))
            })
            .ok()?
            .ok()?;

        let hourly_entries: Vec<MetricEntry> = (0..24)
            .map(|h| {
                let key = format!("{:02}:00", h);
                let mut values = HashMap::new();
                values.insert("order".into(), MetricValue::Count(h as u64));
                values.insert("commits".into(), MetricValue::Count(hourly[h]));
                MetricEntry { key, values }
            })
            .collect();

        let day_names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        let daily_entries: Vec<MetricEntry> = day_names
            .iter()
            .enumerate()
            .map(|(i, &name)| {
                let mut values = HashMap::new();
                values.insert("order".into(), MetricValue::Count((i + 1) as u64));
                values.insert("commits".into(), MetricValue::Count(daily[i]));
                MetricEntry {
                    key: name.into(),
                    values,
                }
            })
            .collect();

        Some(MetricResult {
            name: "patterns".into(),
            display_name: "Commit Patterns".into(),
            description: "When commits happen — broken down by hour of day and day of week. Reveals team work patterns. Lots of late-night or weekend commits may indicate burnout, deadline pressure, or single-person bottlenecks. A healthy team usually shows a clear weekday daytime pattern.".into(),
            columns: vec!["order".into(), "commits".into()],
            column_labels: vec![],
            entries: vec![],
            entry_groups: vec![
                ("hourly".into(), hourly_entries),
                ("daily".into(), daily_entries),
            ],
        })
    }
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "patterns".into(),
        display_name: "Commit Patterns".into(),
        description: String::new(),
        columns: vec![],
        column_labels: vec![],
        entries: vec![],
        entry_groups: vec![],
    }
}
