use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::metrics::MetricCollector;
use crate::types::{MetricEntry, MetricResult, MetricValue, ParsedChange};

struct BranchInfo {
    name: String,
    last_commit_date: chrono::NaiveDate,
    days_since: i64,
    author: String,
    merged: bool,
    is_head: bool,
}

pub struct BranchesCollector {
    branches: Vec<BranchInfo>,
}

impl Default for BranchesCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl BranchesCollector {
    pub fn new() -> Self {
        Self { branches: vec![] }
    }
}

impl MetricCollector for BranchesCollector {
    fn name(&self) -> &str {
        "branches"
    }

    fn process(&mut self, _change: &ParsedChange) {
        // No per-commit work — we operate on repo state via inspect_repo.
    }

    fn inspect_repo(&mut self, repo: &gix::Repository) -> anyhow::Result<()> {
        let head_commit = match repo.head_commit() {
            Ok(c) => c,
            Err(_) => return Ok(()), // empty repo or detached HEAD without commit
        };
        let head_id = head_commit.id;

        let head_ref_short = repo.head_name().ok().flatten().map(|n| {
            let full = n.as_bstr().to_string();
            full.strip_prefix("refs/heads/")
                .unwrap_or(&full)
                .to_string()
        });

        let now = Utc::now();

        let refs = repo.references()?;
        let iter = match refs.local_branches() {
            Ok(i) => i,
            Err(_) => return Ok(()),
        };

        for branch_res in iter {
            let branch = match branch_res {
                Ok(b) => b,
                Err(_) => continue,
            };
            let full_name = branch.name().as_bstr().to_string();
            let short_name = full_name
                .strip_prefix("refs/heads/")
                .unwrap_or(&full_name)
                .to_string();

            let mut branch_mut = branch;
            let commit = match branch_mut.peel_to_commit() {
                Ok(c) => c,
                Err(_) => continue,
            };
            let commit_id = commit.id;

            let (author_name, time_secs) = match commit.author() {
                Ok(sig) => (
                    sig.name.to_string(),
                    sig.time().map(|t| t.seconds).unwrap_or(0),
                ),
                Err(_) => ("<unknown>".into(), 0),
            };

            let last_dt = DateTime::<Utc>::from_timestamp(time_secs, 0).unwrap_or_default();
            let last_commit_date = last_dt.date_naive();
            let days_since = (now.date_naive() - last_commit_date).num_days().max(0);

            let merged = if commit_id == head_id {
                true
            } else {
                match repo.merge_base(head_id, commit_id) {
                    Ok(mb) => mb.detach() == commit_id,
                    Err(_) => false,
                }
            };

            let is_head = head_ref_short.as_deref() == Some(short_name.as_str());

            self.branches.push(BranchInfo {
                name: short_name,
                last_commit_date,
                days_since,
                author: author_name,
                merged,
                is_head,
            });
        }

        Ok(())
    }

    fn finalize(&mut self) -> MetricResult {
        let mut entries: Vec<MetricEntry> = self
            .branches
            .drain(..)
            .map(|b| {
                let recommendation = build_recommendation(b.merged, b.days_since, b.is_head);

                let mut values = HashMap::new();
                values.insert("last_commit".into(), MetricValue::Date(b.last_commit_date));
                values.insert("days_since".into(), MetricValue::Count(b.days_since as u64));
                values.insert("author".into(), MetricValue::Text(b.author));
                values.insert(
                    "merged".into(),
                    MetricValue::Text(if b.merged { "yes".into() } else { "no".into() }),
                );
                values.insert(
                    "is_head".into(),
                    MetricValue::Text(if b.is_head { "yes".into() } else { "no".into() }),
                );
                values.insert("recommendation".into(), MetricValue::Text(recommendation));

                MetricEntry {
                    key: b.name,
                    values,
                }
            })
            .collect();

        // Sort: merged+stale first (delete candidates), then by days_since desc
        entries.sort_by(|a, b| {
            let ka = sort_key(a);
            let kb = sort_key(b);
            kb.cmp(&ka)
        });

        MetricResult {
            name: "branches".into(),
            description: "Branch hygiene — merged/stale/active".into(),
            entry_groups: vec![],
            columns: vec![
                "last_commit".into(),
                "days_since".into(),
                "author".into(),
                "merged".into(),
                "is_head".into(),
                "recommendation".into(),
            ],
            entries,
        }
    }
}

fn sort_key(e: &MetricEntry) -> i64 {
    let days = match e.values.get("days_since") {
        Some(MetricValue::Count(n)) => *n as i64,
        _ => 0,
    };
    let merged = matches!(e.values.get("merged"), Some(MetricValue::Text(s)) if s == "yes");
    // Merged branches with long idle come first (highest priority for deletion)
    if merged { days + 100_000 } else { days }
}

fn build_recommendation(merged: bool, days: i64, is_head: bool) -> String {
    if is_head {
        return "Current HEAD".into();
    }
    match (merged, days) {
        (true, d) if d >= 30 => "Safe to delete — merged & idle".into(),
        (true, _) => "Recently merged — delete after verification".into(),
        (false, d) if d >= 180 => "Stale & unmerged — investigate before deleting".into(),
        (false, d) if d >= 30 => "Unmerged — check if still active".into(),
        (false, _) => "Active".into(),
    }
}
