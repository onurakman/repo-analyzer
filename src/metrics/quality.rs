use std::collections::HashMap;

use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{MetricEntry, MetricResult, MetricValue};

pub struct QualityCollector;

impl Default for QualityCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl QualityCollector {
    pub fn new() -> Self {
        Self
    }
}

const MEGA_COMMIT_THRESHOLD: u64 = 1000;
const SHORT_MSG_THRESHOLD: usize = 10;

fn is_low_quality_message(msg: &str) -> bool {
    let lower = msg.trim().to_lowercase();
    matches!(
        lower.as_str(),
        "wip"
            | "fix"
            | "fix."
            | "fixes"
            | "typo"
            | "update"
            | "updates"
            | "changes"
            | "."
            | ".."
            | "..."
            | "tmp"
            | "temp"
            | "minor"
            | "misc"
            | "stuff"
    ) || lower.starts_with("wip ")
        || lower.starts_with("wip:")
        || lower.starts_with("temp ")
        || lower.starts_with("tmp ")
}

fn is_revert_message(msg: &str) -> bool {
    let lower = msg.trim().to_lowercase();
    lower.starts_with("revert ") || lower.starts_with("revert:") || lower.starts_with("revert \"")
}

impl MetricCollector for QualityCollector {
    fn name(&self) -> &str {
        "quality"
    }

    fn finalize(&mut self) -> MetricResult {
        empty_result()
    }

    fn finalize_from_db(
        &mut self,
        store: &ChangeStore,
        _progress: &crate::metrics::ProgressReporter,
    ) -> Option<MetricResult> {
        // One row per distinct commit. Per-commit work stays bounded because
        // we only keep counters, never the full per-commit list.
        let (
            total_commits,
            short_count,
            low_quality_count,
            revert_count,
            mega_count,
            merge_count,
            total_msg_chars,
        ) = store
            .with_conn(
                |conn| -> anyhow::Result<(u64, u64, u64, u64, u64, u64, u64)> {
                    let mut stmt = conn.prepare(
                        "SELECT COALESCE(MAX(message), '') AS msg,
                            SUM(additions + deletions)  AS total_lines,
                            MAX(parent_count)           AS parents
                       FROM changes
                      GROUP BY commit_oid",
                    )?;
                    let mut total_commits: u64 = 0;
                    let mut short: u64 = 0;
                    let mut low_q: u64 = 0;
                    let mut revert: u64 = 0;
                    let mut mega: u64 = 0;
                    let mut merge: u64 = 0;
                    let mut msg_chars: u64 = 0;

                    let rows = stmt.query_map([], |row| {
                        let msg: String = row.get(0)?;
                        let lines: i64 = row.get(1)?;
                        let parents: i64 = row.get(2)?;
                        Ok((msg, lines as u64, parents as u64))
                    })?;
                    for r in rows {
                        let (msg, lines, parents) = r?;
                        total_commits += 1;
                        let first_line = msg.lines().next().unwrap_or("").trim();
                        let len = first_line.chars().count();
                        msg_chars += len as u64;
                        if len < SHORT_MSG_THRESHOLD {
                            short += 1;
                        }
                        if is_low_quality_message(first_line) {
                            low_q += 1;
                        }
                        if is_revert_message(first_line) {
                            revert += 1;
                        }
                        if lines > MEGA_COMMIT_THRESHOLD {
                            mega += 1;
                        }
                        if parents > 1 {
                            merge += 1;
                        }
                    }
                    Ok((total_commits, short, low_q, revert, mega, merge, msg_chars))
                },
            )
            .ok()?
            .ok()?;

        let avg_msg_len = if total_commits > 0 {
            total_msg_chars as f64 / total_commits as f64
        } else {
            0.0
        };

        let pct = |n: u64| -> f64 {
            if total_commits == 0 {
                0.0
            } else {
                (n as f64 / total_commits as f64) * 100.0
            }
        };

        let make_row = |signal: &str, count: u64, pct_val: f64, rec: &str| {
            let mut values = HashMap::new();
            values.insert("commits".into(), MetricValue::Count(count));
            values.insert("percent".into(), MetricValue::Float(pct_val));
            values.insert("recommendation".into(), MetricValue::Text(rec.into()));
            MetricEntry {
                key: signal.into(),
                values,
            }
        };

        let entries = vec![
            make_row("total_commits", total_commits, 100.0, "Baseline"),
            make_row(
                "short_messages",
                short_count,
                pct(short_count),
                if pct(short_count) > 20.0 {
                    "Enforce min message length or conventional commits"
                } else {
                    "OK"
                },
            ),
            make_row(
                "low_quality_messages",
                low_quality_count,
                pct(low_quality_count),
                if pct(low_quality_count) > 5.0 {
                    "Too many wip/fix/typo — squash before merge"
                } else {
                    "OK"
                },
            ),
            make_row(
                "mega_commits",
                mega_count,
                pct(mega_count),
                if pct(mega_count) > 10.0 {
                    "Large commits hurt review — split by feature"
                } else {
                    "OK"
                },
            ),
            make_row(
                "revert_commits",
                revert_count,
                pct(revert_count),
                if pct(revert_count) > 3.0 {
                    "High revert rate — strengthen CI / review gates"
                } else {
                    "OK"
                },
            ),
            make_row(
                "merge_commits",
                merge_count,
                pct(merge_count),
                if pct(merge_count) > 30.0 {
                    "Consider rebase workflow for cleaner history"
                } else {
                    "OK"
                },
            ),
            {
                // This row reports a character-count average, not a commit
                // count. Putting `total_commits` in the `commits` column here
                // makes the value collide with the `total_commits` row and
                // looks like a duplicate — show the rounded average instead.
                let mut values = HashMap::new();
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let avg_u64 = avg_msg_len.round().max(0.0) as u64;
                values.insert("commits".into(), MetricValue::Count(avg_u64));
                values.insert("percent".into(), MetricValue::Float(avg_msg_len));
                values.insert(
                    "recommendation".into(),
                    MetricValue::Text(if avg_msg_len < 30.0 {
                        "Messages too short — require descriptions".into()
                    } else {
                        "OK".into()
                    }),
                );
                MetricEntry {
                    key: "avg_message_length".into(),
                    values,
                }
            },
        ];

        Some(MetricResult {
            name: "quality".into(),
            display_name: "Commit Quality".into(),
            description: "Signals that hint at risky commit habits: very short messages ('wip', 'fix', 'typo'), mega-commits with thousands of lines (hard to review or revert safely), reverts (a previous commit was wrong), and merge mess. Lower numbers across the board mean cleaner history and easier debugging later.".into(),
            entry_groups: vec![],
            columns: vec!["commits".into(), "percent".into(), "recommendation".into()],
            column_labels: vec![],
            entries,
        })
    }
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "quality".into(),
        display_name: "Commit Quality".into(),
        description: String::new(),
        entry_groups: vec![],
        columns: vec![],
        column_labels: vec![],
        entries: vec![],
    }
}
