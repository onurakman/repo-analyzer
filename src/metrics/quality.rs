use std::collections::{HashMap, HashSet};

use crate::metrics::MetricCollector;
use crate::types::{MetricEntry, MetricResult, MetricValue, ParsedChange};

/// Aggregated quality signals across all commits.
pub struct QualityCollector {
    seen_commits: HashSet<String>,
    commit_lines: HashMap<String, u64>, // oid -> total lines changed
    commit_messages: HashMap<String, String>, // oid -> first line of message
    commit_parents: HashMap<String, usize>, // oid -> parent count
}

impl Default for QualityCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl QualityCollector {
    pub fn new() -> Self {
        Self {
            seen_commits: HashSet::new(),
            commit_lines: HashMap::new(),
            commit_messages: HashMap::new(),
            commit_parents: HashMap::new(),
        }
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

    fn process(&mut self, change: &ParsedChange) {
        let oid = &change.diff.commit.oid;

        if self.seen_commits.insert(oid.clone()) {
            let first_line = change
                .diff
                .commit
                .message
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
            self.commit_messages.insert(oid.clone(), first_line);
            self.commit_parents
                .insert(oid.clone(), change.diff.commit.parent_ids.len());
        }

        let total = change.diff.additions as u64 + change.diff.deletions as u64;
        *self.commit_lines.entry(oid.clone()).or_insert(0) += total;
    }

    fn finalize(&mut self) -> MetricResult {
        let total_commits = self.seen_commits.len() as u64;
        let mut short_count: u64 = 0;
        let mut low_quality_count: u64 = 0;
        let mut revert_count: u64 = 0;
        let mut mega_count: u64 = 0;
        let mut merge_count: u64 = 0;
        let mut total_msg_chars: u64 = 0;

        for (oid, msg) in &self.commit_messages {
            let trimmed = msg.trim();
            total_msg_chars += trimmed.chars().count() as u64;
            if trimmed.chars().count() < SHORT_MSG_THRESHOLD {
                short_count += 1;
            }
            if is_low_quality_message(trimmed) {
                low_quality_count += 1;
            }
            if is_revert_message(trimmed) {
                revert_count += 1;
            }
            if self.commit_parents.get(oid).copied().unwrap_or(0) > 1 {
                merge_count += 1;
            }
        }

        for &lines in self.commit_lines.values() {
            if lines > MEGA_COMMIT_THRESHOLD {
                mega_count += 1;
            }
        }

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
                let mut values = HashMap::new();
                values.insert("commits".into(), MetricValue::Count(total_commits));
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

        MetricResult {
            name: "quality".into(),
            description: "Commit hygiene signals (message quality, size, reverts)".into(),
            entry_groups: vec![],
            columns: vec!["commits".into(), "percent".into(), "recommendation".into()],
            entries,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CommitInfo, DiffRecord, FileStatus};
    use chrono::{FixedOffset, TimeZone};

    fn make_change(oid: &str, msg: &str, additions: u32, parents: usize) -> ParsedChange {
        let ts = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2025, 1, 1, 12, 0, 0)
            .unwrap();
        ParsedChange {
            diff: DiffRecord {
                commit: CommitInfo {
                    oid: oid.into(),
                    author: "a".into(),
                    email: "a@x".into(),
                    timestamp: ts,
                    message: msg.into(),
                    parent_ids: (0..parents).map(|i| format!("p{i}")).collect(),
                },
                file_path: "x.rs".into(),
                old_path: None,
                status: FileStatus::Modified,
                hunks: vec![],
                additions,
                deletions: 0,
            },
            constructs: vec![],
        }
    }

    #[test]
    fn test_quality_signals() {
        let mut c = QualityCollector::new();
        c.process(&make_change("a", "wip", 10, 1));
        c.process(&make_change("b", "Fix typo in README", 5, 1));
        c.process(&make_change("c", "Revert: bad change", 100, 1));
        c.process(&make_change("d", "Regular commit message", 2000, 1));
        c.process(&make_change("e", "Merge branch foo", 1, 2));

        let r = c.finalize();

        // Find rows by key
        let get = |k: &str| -> u64 {
            r.entries
                .iter()
                .find(|e| e.key == k)
                .and_then(|e| match e.values.get("commits") {
                    Some(MetricValue::Count(n)) => Some(*n),
                    _ => None,
                })
                .unwrap_or(0)
        };

        assert_eq!(get("total_commits"), 5);
        assert!(get("low_quality_messages") >= 1); // "wip"
        assert!(get("revert_commits") >= 1);
        assert!(get("mega_commits") >= 1); // d has 2000 additions
        assert_eq!(get("merge_commits"), 1); // e has 2 parents
    }
}
