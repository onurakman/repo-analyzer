use std::collections::HashMap;

use crate::messages;
use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{
    Column, LocalizedMessage, MetricEntry, MetricResult, MetricValue, Severity, report_description,
    report_display,
};

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

        let make_row = |signal: &str, count: u64, pct_val: f64, rec: LocalizedMessage| {
            let mut values = HashMap::new();
            values.insert("commits".into(), MetricValue::Count(count));
            values.insert("percent".into(), MetricValue::Float(pct_val));
            values.insert("recommendation".into(), MetricValue::Message(rec));
            MetricEntry {
                key: signal.into(),
                values,
            }
        };

        let ok = || LocalizedMessage::code(messages::QUALITY_RECOMMENDATION_OK);
        let warn =
            |code: &str| LocalizedMessage::code(code.to_string()).with_severity(Severity::Warning);

        let entries = vec![
            make_row(
                "total_commits",
                total_commits,
                100.0,
                LocalizedMessage::code(messages::QUALITY_RECOMMENDATION_BASELINE),
            ),
            make_row(
                "short_messages",
                short_count,
                pct(short_count),
                if pct(short_count) > 20.0 {
                    warn(messages::QUALITY_RECOMMENDATION_ENFORCE_MSG_LENGTH)
                } else {
                    ok()
                },
            ),
            make_row(
                "low_quality_messages",
                low_quality_count,
                pct(low_quality_count),
                if pct(low_quality_count) > 5.0 {
                    warn(messages::QUALITY_RECOMMENDATION_SQUASH_WIP)
                } else {
                    ok()
                },
            ),
            make_row(
                "mega_commits",
                mega_count,
                pct(mega_count),
                if pct(mega_count) > 10.0 {
                    warn(messages::QUALITY_RECOMMENDATION_SPLIT_MEGA)
                } else {
                    ok()
                },
            ),
            make_row(
                "revert_commits",
                revert_count,
                pct(revert_count),
                if pct(revert_count) > 3.0 {
                    warn(messages::QUALITY_RECOMMENDATION_STRENGTHEN_REVIEW)
                } else {
                    ok()
                },
            ),
            make_row(
                "merge_commits",
                merge_count,
                pct(merge_count),
                if pct(merge_count) > 30.0 {
                    warn(messages::QUALITY_RECOMMENDATION_REBASE_WORKFLOW)
                } else {
                    ok()
                },
            ),
            {
                let mut values = HashMap::new();
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let avg_u64 = avg_msg_len.round().max(0.0) as u64;
                values.insert("commits".into(), MetricValue::Count(avg_u64));
                values.insert("percent".into(), MetricValue::Float(avg_msg_len));
                values.insert(
                    "recommendation".into(),
                    MetricValue::Message(if avg_msg_len < 30.0 {
                        warn(messages::QUALITY_RECOMMENDATION_REQUIRE_DESCRIPTIONS)
                    } else {
                        ok()
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
            display_name: report_display("quality"),
            description: report_description("quality"),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("quality", "commits"),
                Column::in_report("quality", "percent"),
                Column::in_report("quality", "recommendation"),
            ],
            entries,
        })
    }
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "quality".into(),
        display_name: report_display("quality"),
        description: report_description("quality"),
        entry_groups: vec![],
        columns: vec![],
        entries: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CommitInfo, DiffRecord, FileStatus, ParsedChange};
    use chrono::{FixedOffset, TimeZone};
    use std::sync::Arc;

    fn make_change(
        oid: &str,
        message: &str,
        additions: u32,
        deletions: u32,
        parent_ids: Vec<String>,
    ) -> ParsedChange {
        let ts = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2025, 1, 15, 12, 0, 0)
            .unwrap();
        ParsedChange {
            diff: Arc::new(DiffRecord {
                commit: Arc::new(CommitInfo {
                    oid: oid.into(),
                    author: "dev".into(),
                    email: "dev@x".into(),
                    timestamp: ts,
                    message: message.into(),
                    parent_ids,
                }),
                file_path: format!("file_{oid}.rs").into(),
                old_path: None,
                status: FileStatus::Modified,
                hunks: vec![],
                additions,
                deletions,
            }),
            constructs: vec![],
        }
    }

    fn store_with(changes: &[ParsedChange]) -> ChangeStore {
        let store = ChangeStore::open_temp().expect("open store");
        store.insert_batch(changes).expect("insert");
        store.finalize_indexes().expect("index");
        store
    }

    fn entry<'a>(r: &'a MetricResult, key: &str) -> &'a MetricEntry {
        r.entries
            .iter()
            .find(|e| e.key == key)
            .unwrap_or_else(|| panic!("missing key {key}"))
    }

    fn count_of(e: &MetricEntry, key: &str) -> u64 {
        match e.values.get(key) {
            Some(MetricValue::Count(n)) => *n,
            other => panic!("expected Count for {key}, got {other:?}"),
        }
    }

    fn rec_code(e: &MetricEntry) -> String {
        match e.values.get("recommendation") {
            Some(MetricValue::Message(m)) => m.code.clone(),
            other => panic!("expected recommendation Message, got {other:?}"),
        }
    }

    #[test]
    fn empty_collector_returns_named_result() {
        let mut coll = QualityCollector::new();
        let r = coll.finalize();
        assert_eq!(r.name, "quality");
        assert!(r.entries.is_empty());
    }

    #[test]
    fn low_quality_message_detection_covers_common_lazy_phrases() {
        for s in ["wip", "WIP", "fix", "typo", "update", "...", "tmp", "stuff"] {
            assert!(
                is_low_quality_message(s),
                "{s:?} should be marked low-quality"
            );
        }
        assert!(is_low_quality_message("WIP debug"));
        assert!(is_low_quality_message("wip: refactor"));
        assert!(is_low_quality_message("temp foo"));
        assert!(is_low_quality_message("tmp bar"));
    }

    #[test]
    fn low_quality_message_passes_real_messages() {
        assert!(!is_low_quality_message("Add feature X"));
        assert!(!is_low_quality_message("Refactor module Y for clarity"));
        assert!(!is_low_quality_message("Fix bug in parser"));
    }

    #[test]
    fn revert_message_detection_supports_common_forms() {
        assert!(is_revert_message("Revert \"feat: x\""));
        assert!(is_revert_message("revert: foo"));
        assert!(is_revert_message("Revert this"));
    }

    #[test]
    fn revert_message_negatives() {
        assert!(!is_revert_message("revertedly broken"));
        assert!(!is_revert_message("Add reverter helper"));
        assert!(!is_revert_message(""));
    }

    #[test]
    fn finalize_from_db_emits_seven_signal_rows() {
        let store = store_with(&[make_change("c1", "Add feature X with care", 10, 5, vec![])]);
        let mut coll = QualityCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        let keys: Vec<String> = r.entries.iter().map(|e| e.key.clone()).collect();
        for k in [
            "total_commits",
            "short_messages",
            "low_quality_messages",
            "mega_commits",
            "revert_commits",
            "merge_commits",
            "avg_message_length",
        ] {
            assert!(keys.contains(&k.to_string()), "missing signal row {k}");
        }
        // Single commit, 100% baseline.
        assert_eq!(count_of(entry(&r, "total_commits"), "commits"), 1);
    }

    #[test]
    fn finalize_from_db_flags_short_messages_when_majority_short() {
        // 3 short, 1 long → 75% short > 20% triggers the warning.
        let store = store_with(&[
            make_change("c1", "x", 1, 0, vec![]),
            make_change("c2", "y", 1, 0, vec![]),
            make_change("c3", "z", 1, 0, vec![]),
            make_change(
                "c4",
                "A reasonably detailed commit message about feature work",
                1,
                0,
                vec![],
            ),
        ]);
        let mut coll = QualityCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert_eq!(
            rec_code(entry(&r, "short_messages")),
            messages::QUALITY_RECOMMENDATION_ENFORCE_MSG_LENGTH
        );
    }

    #[test]
    fn finalize_from_db_flags_low_quality_when_above_5pct() {
        // 2 of 10 commits are 'wip' = 20% > 5% → squash_wip recommendation.
        let mut commits: Vec<ParsedChange> = (1..=8)
            .map(|i| {
                make_change(
                    &format!("c{i}"),
                    "Reasonable commit message describing work",
                    1,
                    0,
                    vec![],
                )
            })
            .collect();
        commits.push(make_change("clow1", "wip", 1, 0, vec![]));
        commits.push(make_change("clow2", "WIP", 1, 0, vec![]));
        let store = store_with(&commits);
        let mut coll = QualityCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert_eq!(
            rec_code(entry(&r, "low_quality_messages")),
            messages::QUALITY_RECOMMENDATION_SQUASH_WIP
        );
    }

    #[test]
    fn finalize_from_db_flags_mega_commit_when_above_threshold() {
        // 2 of 10 commits exceed 1000 lines (20% > 10%).
        let mut commits: Vec<ParsedChange> = (1..=8)
            .map(|i| make_change(&format!("c{i}"), "Reasonable message", 5, 5, vec![]))
            .collect();
        commits.push(make_change("cbig1", "Reasonable message", 600, 500, vec![]));
        commits.push(make_change("cbig2", "Reasonable message", 700, 700, vec![]));
        let store = store_with(&commits);
        let mut coll = QualityCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert_eq!(
            rec_code(entry(&r, "mega_commits")),
            messages::QUALITY_RECOMMENDATION_SPLIT_MEGA
        );
    }

    #[test]
    fn finalize_from_db_flags_revert_when_above_3pct() {
        // 1 revert in 20 commits = 5% > 3%.
        let mut commits: Vec<ParsedChange> = (1..=19)
            .map(|i| make_change(&format!("c{i}"), "Reasonable message", 5, 5, vec![]))
            .collect();
        commits.push(make_change("crev", "Revert \"x\"", 5, 5, vec![]));
        let store = store_with(&commits);
        let mut coll = QualityCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert_eq!(
            rec_code(entry(&r, "revert_commits")),
            messages::QUALITY_RECOMMENDATION_STRENGTHEN_REVIEW
        );
    }

    #[test]
    fn finalize_from_db_flags_merge_commits_when_above_30pct() {
        // 2 of 4 commits are merges (50% > 30%).
        let store = store_with(&[
            make_change("c1", "Reasonable message", 1, 0, vec![]),
            make_change("c2", "Reasonable message", 1, 0, vec![]),
            make_change(
                "cm1",
                "Merge branch foo",
                1,
                0,
                vec!["p1".into(), "p2".into()],
            ),
            make_change(
                "cm2",
                "Merge branch bar",
                1,
                0,
                vec!["p1".into(), "p2".into()],
            ),
        ]);
        let mut coll = QualityCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert_eq!(
            rec_code(entry(&r, "merge_commits")),
            messages::QUALITY_RECOMMENDATION_REBASE_WORKFLOW
        );
    }

    #[test]
    fn finalize_from_db_flags_avg_message_length_below_30() {
        let store = store_with(&[make_change("c1", "Short msg", 1, 0, vec![])]);
        let mut coll = QualityCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        assert_eq!(
            rec_code(entry(&r, "avg_message_length")),
            messages::QUALITY_RECOMMENDATION_REQUIRE_DESCRIPTIONS
        );
    }

    #[test]
    fn finalize_from_db_total_commits_baseline_has_no_severity() {
        let store = store_with(&[make_change("c1", "msg ok", 1, 0, vec![])]);
        let mut coll = QualityCollector::new();
        let r = coll
            .finalize_from_db(&store, &crate::metrics::ProgressReporter::new(None))
            .expect("db result");
        let baseline = entry(&r, "total_commits");
        match baseline.values.get("recommendation") {
            Some(MetricValue::Message(m)) => {
                assert_eq!(m.code, messages::QUALITY_RECOMMENDATION_BASELINE);
                assert!(m.severity.is_none());
            }
            other => panic!("expected baseline Message, got {other:?}"),
        }
    }
}
