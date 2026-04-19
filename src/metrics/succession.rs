use std::collections::HashMap;

use chrono::{Duration, Utc};

use crate::analysis::source_filter::is_source_file;
use crate::messages;
use crate::metrics::MetricCollector;
use crate::store::ChangeStore;
use crate::types::{
    Column, LocalizedMessage, MetricEntry, MetricResult, MetricValue, Severity, report_description,
    report_display,
};

const INACTIVE_DAYS: i64 = 180;

/// Cap the per-file per-author detail pass to the top-N files by total commits.
/// Beyond this the report entry is truncated anyway, and the tail dominates memory.
const TOP_FILES: i64 = 500;

pub struct SuccessionCollector;

impl Default for SuccessionCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl SuccessionCollector {
    pub fn new() -> Self {
        Self
    }
}

impl MetricCollector for SuccessionCollector {
    fn name(&self) -> &str {
        "succession"
    }

    fn finalize(&mut self) -> MetricResult {
        empty_result()
    }

    fn finalize_from_db(
        &mut self,
        store: &ChangeStore,
        progress: &crate::metrics::ProgressReporter,
    ) -> Option<MetricResult> {
        struct AuthorRow {
            commits: u64,
            last_ts: i64,
        }
        struct FileAcc {
            authors: HashMap<String, AuthorRow>,
            original_email: String,
            original_ts: i64,
        }
        type Files = HashMap<String, FileAcc>;
        type GlobalLast = HashMap<String, i64>;

        let (files, global_last) = store
            .with_conn(|conn| -> anyhow::Result<(Files, GlobalLast)> {
                progress.status(&format!(
                    "  succession: picking top {TOP_FILES} active files..."
                ));
                // Top-N files by commit activity. Everything else is dropped.
                conn.execute_batch(&format!(
                    "DROP TABLE IF EXISTS __top_files;
                     CREATE TEMP TABLE __top_files AS
                       SELECT file_path AS file
                         FROM changes
                        GROUP BY file_path
                        ORDER BY COUNT(DISTINCT commit_oid) DESC
                        LIMIT {TOP_FILES};"
                ))?;

                progress.status("  succession: per-file per-author detail...");
                let mut files: Files = HashMap::new();
                {
                    let mut stmt = conn.prepare(
                        "SELECT ch.file_path,
                                ch.email,
                                COUNT(DISTINCT ch.commit_oid) AS commits,
                                MIN(ch.commit_ts)             AS first_ts,
                                MAX(ch.commit_ts)             AS last_ts
                           FROM changes ch
                           JOIN __top_files t ON t.file = ch.file_path
                          GROUP BY ch.file_path, ch.email",
                    )?;
                    let rows = stmt.query_map([], |row| {
                        let file: String = row.get(0)?;
                        let email: String = row.get(1)?;
                        let commits: i64 = row.get(2)?;
                        let first_ts: i64 = row.get(3)?;
                        let last_ts: i64 = row.get(4)?;
                        Ok((file, email, commits as u64, first_ts, last_ts))
                    })?;
                    for r in rows {
                        let (file, email, commits, first_ts, last_ts) = r?;
                        if !is_source_file(&file) {
                            continue;
                        }
                        let acc = files.entry(file).or_insert_with(|| FileAcc {
                            authors: HashMap::new(),
                            original_email: email.clone(),
                            original_ts: first_ts,
                        });
                        if first_ts < acc.original_ts {
                            acc.original_ts = first_ts;
                            acc.original_email = email.clone();
                        }
                        acc.authors.insert(email, AuthorRow { commits, last_ts });
                    }
                }

                let mut global_last: GlobalLast = HashMap::new();
                {
                    let mut stmt2 =
                        conn.prepare("SELECT email, MAX(commit_ts) FROM changes GROUP BY email")?;
                    let rows2 = stmt2.query_map([], |row| {
                        let email: String = row.get(0)?;
                        let last: i64 = row.get(1)?;
                        Ok((email, last))
                    })?;
                    for r in rows2 {
                        let (email, last) = r?;
                        global_last.insert(email, last);
                    }
                }

                conn.execute("DROP TABLE IF EXISTS __top_files", [])?;
                Ok((files, global_last))
            })
            .ok()?
            .ok()?;

        let now = Utc::now();
        let active_cutoff = (now - Duration::days(INACTIVE_DAYS)).timestamp();

        let mut entries: Vec<MetricEntry> = files
            .into_iter()
            .map(|(path, fs)| {
                let original_active = global_last
                    .get(&fs.original_email)
                    .map(|t| *t >= active_cutoff)
                    .unwrap_or(false);
                let total_authors = fs.authors.len() as u64;
                let successor_count = fs
                    .authors
                    .iter()
                    .filter(|(email, _)| **email != fs.original_email)
                    .count() as u64;
                let current_top: String = fs
                    .authors
                    .iter()
                    .max_by_key(|(_, t)| (t.commits, t.last_ts))
                    .map(|(e, _)| e.clone())
                    .unwrap_or_else(|| "<unknown>".into());

                let status = classify(original_active, successor_count, total_authors);

                let mut values = HashMap::new();
                values.insert(
                    "original_author".into(),
                    MetricValue::Text(fs.original_email),
                );
                values.insert(
                    "original_active".into(),
                    MetricValue::Count(u64::from(original_active)),
                );
                values.insert("current_top".into(), MetricValue::Text(current_top));
                values.insert("total_authors".into(), MetricValue::Count(total_authors));
                values.insert("successors".into(), MetricValue::Count(successor_count));
                values.insert("status".into(), MetricValue::Message(status));

                MetricEntry { key: path, values }
            })
            .collect();

        entries.sort_by_key(|e| std::cmp::Reverse(status_rank(e)));
        entries.truncate(200);

        Some(MetricResult {
            name: "succession".into(),
            display_name: report_display("succession"),
            description: report_description("succession"),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("succession", "original_author"),
                Column::in_report("succession", "original_active"),
                Column::in_report("succession", "current_top"),
                Column::in_report("succession", "total_authors"),
                Column::in_report("succession", "successors"),
                Column::in_report("succession", "status"),
            ],
            entries,
        })
    }
}

fn classify(original_active: bool, successors: u64, total_authors: u64) -> LocalizedMessage {
    let (code, severity) = if original_active {
        if total_authors >= 3 {
            (messages::SUCCESSION_STATUS_HEALTHY, None)
        } else {
            (messages::SUCCESSION_STATUS_OWNED, None)
        }
    } else if successors == 0 {
        (messages::SUCCESSION_STATUS_ORPHANED, Some(Severity::Error))
    } else if successors == 1 {
        (
            messages::SUCCESSION_STATUS_KNOWLEDGE_TRANSFER_NEEDED,
            Some(Severity::Warning),
        )
    } else {
        (messages::SUCCESSION_STATUS_HANDED_OFF, None)
    };
    let mut msg = LocalizedMessage::code(code)
        .with_param("successors", successors)
        .with_param("total_authors", total_authors);
    if let Some(s) = severity {
        msg = msg.with_severity(s);
    }
    msg
}

fn status_rank(entry: &MetricEntry) -> u8 {
    match entry.values.get("status") {
        Some(MetricValue::Message(m)) => match m.code.as_str() {
            c if c == messages::SUCCESSION_STATUS_ORPHANED => 4,
            c if c == messages::SUCCESSION_STATUS_KNOWLEDGE_TRANSFER_NEEDED => 3,
            c if c == messages::SUCCESSION_STATUS_HANDED_OFF => 2,
            c if c == messages::SUCCESSION_STATUS_OWNED => 1,
            _ => 0,
        },
        _ => 0,
    }
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "succession".into(),
        display_name: report_display("succession"),
        description: report_description("succession"),
        entry_groups: vec![],
        columns: vec![],
        entries: vec![],
    }
}
