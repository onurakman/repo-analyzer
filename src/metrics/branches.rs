//! Branch inventory: one row per branch in the repo, with age, author, and
//! a recommendation for what to do with it (merge, delete, review).
//!
//! Enumerates refs under `refs/remotes/origin/*` first — these are populated
//! by a normal fetch and cover every branch on the remote. Falls back to
//! `refs/heads/*` for repos that only have local branches (e.g. a workspace
//! that hasn't pushed yet).
//!
//! A shallow or `--single-branch` clone will surface fewer than 2 branches;
//! in that case we emit an empty result rather than render a useless single
//! row that says "this repo has one branch".

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use crate::messages;
use crate::metrics::MetricCollector;
use crate::types::{
    Column, EntryGroup, LocalizedMessage, MetricEntry, MetricResult, MetricValue, ParsedChange,
    Severity, report_description, report_display,
};

/// Minimum branch count needed to produce a useful report. One branch is just
/// HEAD — no signal worth showing.
pub(crate) const MIN_BRANCHES: usize = 2;

/// Days since last commit above which an unmerged branch is flagged as stale.
const STALE_DAYS: i64 = 60;

/// Days since last commit above which an unmerged branch is flagged as
/// abandoned — much stronger signal than stale.
const ABANDONED_DAYS: i64 = 180;

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

    fn process(&mut self, _change: &ParsedChange) {}

    fn inspect_repo(
        &mut self,
        repo: &gix::Repository,
        progress: &crate::metrics::ProgressReporter,
    ) -> anyhow::Result<()> {
        let head_commit = match repo.head_commit() {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        let head_id = head_commit.id;

        let head_short = repo.head_name().ok().flatten().map(|n| {
            let full = n.as_bstr().to_string();
            full.strip_prefix("refs/heads/")
                .unwrap_or(&full)
                .to_string()
        });

        // Walk HEAD's ancestry once and stash the set of reachable commits.
        // `merge_base(head, branch_tip)` per branch is O(N) each and becomes
        // pathological on repos with many branches; set membership is O(1).
        progress.status("branches: walking HEAD ancestry");
        let head_ancestors = collect_ancestors(repo, head_id);

        let now = Utc::now().date_naive();
        progress.status("branches: scanning refs");

        // Prefer remote-tracking branches since a typical full clone has all
        // branches under refs/remotes/origin/* but only one under refs/heads/.
        // If no remotes exist, fall back to local branches.
        self.scan_remote_branches(repo, head_id, &head_ancestors, head_short.as_deref(), now)?;
        if self.branches.is_empty() {
            self.scan_local_branches(repo, head_id, &head_ancestors, head_short.as_deref(), now)?;
        }

        Ok(())
    }

    fn finalize(&mut self) -> MetricResult {
        if self.branches.len() < MIN_BRANCHES {
            self.branches.clear();
            return empty_result();
        }

        // Bucket branches by status so the report leads with delete/review
        // candidates and buries healthy ones. Groups keep the visual scan
        // fast — each header carries its own count via the template.
        let mut buckets: HashMap<BranchStatus, Vec<MetricEntry>> = HashMap::new();
        for b in self.branches.drain(..) {
            let status = classify(&b);
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
            values.insert(
                "recommendation".into(),
                MetricValue::Message(recommendation),
            );
            buckets.entry(status).or_default().push(MetricEntry {
                key: b.name,
                values,
            });
        }

        // Each group sorted by days_since desc — oldest first within a bucket.
        for entries in buckets.values_mut() {
            entries.sort_by(|a, b| {
                let ad = days_of(a);
                let bd = days_of(b);
                bd.cmp(&ad)
            });
        }

        // Fixed group order — worst-first so readers act on the top rows.
        let order = [
            BranchStatus::Abandoned,
            BranchStatus::Stale,
            BranchStatus::MergedStale,
            BranchStatus::MergedRecent,
            BranchStatus::Active,
            BranchStatus::Head,
        ];
        let mut entry_groups: Vec<EntryGroup> = Vec::new();
        for status in order {
            if let Some(entries) = buckets.remove(&status) {
                if entries.is_empty() {
                    continue;
                }
                entry_groups.push(EntryGroup {
                    name: status.name().into(),
                    label: status.label_code().into(),
                    entries,
                });
            }
        }

        MetricResult {
            name: "branches".into(),
            display_name: report_display("branches"),
            description: report_description("branches"),
            columns: vec![
                Column::in_report("branches", "last_commit"),
                Column::in_report("branches", "days_since"),
                Column::in_report("branches", "author"),
                Column::in_report("branches", "merged"),
                Column::in_report("branches", "is_head"),
                Column::in_report("branches", "recommendation"),
            ],
            entries: vec![],
            entry_groups,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
enum BranchStatus {
    Head,
    Active,
    Stale,
    Abandoned,
    MergedRecent,
    MergedStale,
}

impl BranchStatus {
    fn name(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Active => "active",
            Self::Stale => "stale",
            Self::Abandoned => "abandoned",
            Self::MergedRecent => "merged_recent",
            Self::MergedStale => "merged_stale",
        }
    }

    fn label_code(self) -> &'static str {
        match self {
            Self::Head => "branches.group.head",
            Self::Active => "branches.group.active",
            Self::Stale => "branches.group.stale",
            Self::Abandoned => "branches.group.abandoned",
            Self::MergedRecent => "branches.group.merged_recent",
            Self::MergedStale => "branches.group.merged_stale",
        }
    }
}

fn classify(b: &BranchInfo) -> BranchStatus {
    if b.is_head {
        return BranchStatus::Head;
    }
    if b.merged {
        if b.days_since >= STALE_DAYS {
            BranchStatus::MergedStale
        } else {
            BranchStatus::MergedRecent
        }
    } else if b.days_since >= ABANDONED_DAYS {
        BranchStatus::Abandoned
    } else if b.days_since >= STALE_DAYS {
        BranchStatus::Stale
    } else {
        BranchStatus::Active
    }
}

fn days_of(e: &MetricEntry) -> u64 {
    match e.values.get("days_since") {
        Some(MetricValue::Count(n)) => *n,
        _ => 0,
    }
}

impl BranchesCollector {
    fn scan_remote_branches(
        &mut self,
        repo: &gix::Repository,
        head_id: gix::ObjectId,
        head_ancestors: &HashSet<gix::ObjectId>,
        head_short: Option<&str>,
        now: chrono::NaiveDate,
    ) -> anyhow::Result<()> {
        let refs = repo.references()?;
        let iter = match refs.prefixed("refs/remotes/") {
            Ok(i) => i,
            Err(_) => return Ok(()),
        };
        for r in iter {
            let mut reference = match r {
                Ok(x) => x,
                Err(_) => continue,
            };
            let full = reference.name().as_bstr().to_string();
            // Skip the pseudo-ref `refs/remotes/<remote>/HEAD` that points at
            // the default branch — it duplicates another row.
            if full.ends_with("/HEAD") {
                continue;
            }
            let stripped = match full.strip_prefix("refs/remotes/") {
                Some(s) => s,
                None => continue,
            };
            // stripped looks like "origin/feature/x"; drop the remote prefix.
            let short = match stripped.split_once('/') {
                Some((_remote, rest)) => rest.to_string(),
                None => stripped.to_string(),
            };
            if let Some(info) = resolve_branch(
                &mut reference,
                &short,
                head_id,
                head_ancestors,
                head_short,
                now,
            ) {
                self.branches.push(info);
            }
        }
        Ok(())
    }

    fn scan_local_branches(
        &mut self,
        repo: &gix::Repository,
        head_id: gix::ObjectId,
        head_ancestors: &HashSet<gix::ObjectId>,
        head_short: Option<&str>,
        now: chrono::NaiveDate,
    ) -> anyhow::Result<()> {
        let refs = repo.references()?;
        let iter = match refs.local_branches() {
            Ok(i) => i,
            Err(_) => return Ok(()),
        };
        for r in iter {
            let mut reference = match r {
                Ok(x) => x,
                Err(_) => continue,
            };
            let full = reference.name().as_bstr().to_string();
            let short = full
                .strip_prefix("refs/heads/")
                .unwrap_or(&full)
                .to_string();
            if let Some(info) = resolve_branch(
                &mut reference,
                &short,
                head_id,
                head_ancestors,
                head_short,
                now,
            ) {
                self.branches.push(info);
            }
        }
        Ok(())
    }
}

/// Walk every ancestor of HEAD and return the flat set of commit IDs. Errors
/// while traversing (missing objects in a shallow clone, corrupt refs) stop
/// the walk rather than abort — a partial set still lets us classify most
/// branches correctly, and shallow holes would only cause false "unmerged"
/// readings on commits below the clone's depth cutoff.
fn collect_ancestors(repo: &gix::Repository, head_id: gix::ObjectId) -> HashSet<gix::ObjectId> {
    let mut set = HashSet::new();
    let walk = match repo.rev_walk(Some(head_id)).all() {
        Ok(w) => w,
        Err(_) => {
            set.insert(head_id);
            return set;
        }
    };
    for info in walk {
        match info {
            Ok(info) => {
                set.insert(info.id);
            }
            Err(_) => break,
        }
    }
    set.insert(head_id);
    set
}

fn resolve_branch(
    reference: &mut gix::Reference<'_>,
    short: &str,
    head_id: gix::ObjectId,
    head_ancestors: &HashSet<gix::ObjectId>,
    head_short: Option<&str>,
    now: chrono::NaiveDate,
) -> Option<BranchInfo> {
    let commit = reference.peel_to_commit().ok()?;
    let commit_id = commit.id;

    let (author_email, time_secs) = match commit.author() {
        Ok(sig) => (
            sig.email.to_string(),
            sig.time().map(|t| t.seconds).unwrap_or(0),
        ),
        Err(_) => ("<unknown>".into(), 0),
    };
    let last_dt = DateTime::<Utc>::from_timestamp(time_secs, 0).unwrap_or_default();
    let last_commit_date = last_dt.date_naive();
    let days_since = (now - last_commit_date).num_days().max(0);

    // A branch is "merged" iff its tip is an ancestor of HEAD.
    let merged = commit_id == head_id || head_ancestors.contains(&commit_id);

    let is_head = head_short == Some(short);

    Some(BranchInfo {
        name: short.to_string(),
        last_commit_date,
        days_since,
        author: author_email,
        merged,
        is_head,
    })
}

fn build_recommendation(merged: bool, days_since: i64, is_head: bool) -> LocalizedMessage {
    if is_head {
        return LocalizedMessage::code(messages::BRANCHES_RECOMMENDATION_HEAD);
    }
    if merged {
        if days_since >= STALE_DAYS {
            LocalizedMessage::code(messages::BRANCHES_RECOMMENDATION_MERGED_STALE)
                .with_severity(Severity::Warning)
                .with_param("days", days_since as u64)
        } else {
            LocalizedMessage::code(messages::BRANCHES_RECOMMENDATION_MERGED_RECENT)
                .with_severity(Severity::Info)
        }
    } else if days_since >= ABANDONED_DAYS {
        LocalizedMessage::code(messages::BRANCHES_RECOMMENDATION_ABANDONED)
            .with_severity(Severity::Critical)
            .with_param("days", days_since as u64)
    } else if days_since >= STALE_DAYS {
        LocalizedMessage::code(messages::BRANCHES_RECOMMENDATION_STALE)
            .with_severity(Severity::Warning)
            .with_param("days", days_since as u64)
    } else {
        LocalizedMessage::code(messages::BRANCHES_RECOMMENDATION_ACTIVE)
    }
}

fn empty_result() -> MetricResult {
    MetricResult {
        name: "branches".into(),
        display_name: report_display("branches"),
        description: report_description("branches"),
        columns: vec![],
        entries: vec![],
        entry_groups: vec![],
    }
}
