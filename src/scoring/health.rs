//! Overall repository health score.
//!
//! Reads the finalized [`MetricResult`]s from other collectors and a live
//! [`gix::Repository`] handle, then produces a synthesized [`MetricResult`]
//! with five pillar scores (0-100, equally weighted), a combined overall
//! score, top actionable items, and repo-hygiene cleanup commands.
//!
//! The scoring is **rule-based and opinionated** — thresholds live as `const`s
//! here so they're easy to audit. A team with different priorities (e.g. more
//! tolerant of solo ownership in a small project) should tune them.

use std::collections::HashMap;
use std::path::Path;

use crate::types::{MetricEntry, MetricResult, MetricValue};

/// Weight of each pillar in the overall score. Equal weighting keeps the
/// surface transparent — one pillar can't dominate silently.
const PILLAR_WEIGHT: f64 = 1.0 / 5.0;

/// How many concrete action items to surface in the "Actions" group across
/// all pillars. Keeps the output skimmable.
const MAX_ACTIONS: usize = 10;

/// Minimum score any pillar can drop to. Prevents a single outlier from
/// driving a pillar to 0 and dragging the overall score down irrecoverably.
const MIN_PILLAR_SCORE: f64 = 20.0;

/// Compute the health result, or `None` if we can't produce anything useful
/// (e.g. an empty repo with no collectors run).
pub fn compute_health(results: &[MetricResult], repo_path: &Path) -> Option<MetricResult> {
    let by_name: HashMap<&str, &MetricResult> =
        results.iter().map(|r| (r.name.as_str(), r)).collect();

    let pillars: Vec<Pillar> = vec![
        score_commit_discipline(&by_name),
        score_bus_factor(&by_name),
        score_refactoring_debt(&by_name),
        score_tidiness(&by_name),
        score_change_concentration(&by_name),
    ];

    let overall_score = {
        let sum: f64 = pillars.iter().map(|p| p.score * PILLAR_WEIGHT).sum();
        sum.round().clamp(0.0, 100.0) as u64
    };

    let hygiene = repo_hygiene_findings(repo_path, &by_name);

    Some(build_result(overall_score, pillars, hygiene))
}

// ---------------------------------------------------------------------------
// Pillar scoring
// ---------------------------------------------------------------------------

/// One pillar's contribution to the overall score plus the concrete actions
/// it wants to surface.
struct Pillar {
    key: &'static str,
    display: &'static str,
    score: f64,
    summary: String,
    actions: Vec<Action>,
}

/// A concrete, file- or function-level action the user can take to improve
/// one pillar. Consolidated across pillars and capped at [`MAX_ACTIONS`] in
/// the final output.
struct Action {
    pillar: &'static str,
    target: String,
    detail: String,
    command: String,
}

impl Action {
    fn new(
        pillar: &'static str,
        target: impl Into<String>,
        detail: impl Into<String>,
        command: impl Into<String>,
    ) -> Self {
        Self {
            pillar,
            target: target.into(),
            detail: detail.into(),
            command: command.into(),
        }
    }
}

/// Pillar 1: commit hygiene signals from the `quality` report.
fn score_commit_discipline(by_name: &HashMap<&str, &MetricResult>) -> Pillar {
    let quality = by_name.get("quality");
    let pct = |key: &str| -> f64 {
        quality
            .and_then(|r| r.entries.iter().find(|e| e.key == key))
            .and_then(|e| match e.values.get("percent") {
                Some(MetricValue::Float(f)) => Some(*f),
                _ => None,
            })
            .unwrap_or(0.0)
    };

    let short = pct("short_messages");
    let low_q = pct("low_quality_messages");
    let mega = pct("mega_commits");
    let revert = pct("revert_commits");
    let merge = pct("merge_commits");

    // Each dimension has its own threshold; overrun is multiplied by a weight
    // that reflects how much the signal matters (reverts hurt more than
    // merge-heavy histories, for example).
    let penalty = over(short, 20.0) * 1.0
        + over(low_q, 5.0) * 2.0
        + over(mega, 10.0) * 1.5
        + over(revert, 3.0) * 3.0
        + over(merge, 30.0) * 0.5;
    let score = (100.0 - penalty).max(MIN_PILLAR_SCORE);

    let mut actions: Vec<Action> = Vec::new();
    if low_q > 5.0 {
        actions.push(Action::new(
            "commit_discipline",
            "commit messages",
            format!(
                "{:.1}% of commits have low-quality messages (wip/fix/typo/...)",
                low_q
            ),
            "Enforce conventional commits or a `commitlint` hook in CI",
        ));
    }
    if mega > 10.0 {
        actions.push(Action::new(
            "commit_discipline",
            "mega commits",
            format!(
                "{:.1}% of commits touch 1000+ lines — hard to review or revert",
                mega
            ),
            "Split large commits by feature/file before merging",
        ));
    }
    if revert > 3.0 {
        actions.push(Action::new(
            "commit_discipline",
            "revert rate",
            format!(
                "{:.1}% of commits are reverts — CI/review gates are leaky",
                revert
            ),
            "Require green CI + peer review before merge",
        ));
    }

    let summary = format!(
        "short={:.0}% low-q={:.0}% mega={:.0}% revert={:.0}% merge={:.0}%",
        short, low_q, mega, revert, merge
    );

    Pillar {
        key: "commit_discipline",
        display: "Commit Discipline",
        score,
        summary,
        actions,
    }
}

/// Pillar 2: bus-factor risk from `knowledge_silos` and `succession`.
fn score_bus_factor(by_name: &HashMap<&str, &MetricResult>) -> Pillar {
    let silos = by_name
        .get("knowledge_silos")
        .map(|r| r.entries.as_slice())
        .unwrap_or(&[]);
    let succ = by_name
        .get("succession")
        .map(|r| r.entries.as_slice())
        .unwrap_or(&[]);

    let high_risk = silos
        .iter()
        .filter(
            |e| matches!(e.values.get("risk"), Some(MetricValue::Text(s)) if s.contains("High")),
        )
        .count();

    let stale = succ
        .iter()
        .filter(|e| {
            matches!(e.values.get("status"), Some(MetricValue::Text(s)) if s.to_lowercase().contains("stale"))
        })
        .count();

    // Up to 50 points off for heavy silo load, up to 30 for stale succession.
    let silo_penalty = (high_risk as f64 * 2.0).min(50.0);
    let stale_penalty = (stale as f64 * 1.5).min(30.0);
    let score = (100.0 - silo_penalty - stale_penalty).max(MIN_PILLAR_SCORE);

    let mut actions: Vec<Action> = Vec::new();
    for e in silos.iter().take(3) {
        if matches!(e.values.get("risk"), Some(MetricValue::Text(s)) if s.contains("High")) {
            let owner = match e.values.get("owner") {
                Some(MetricValue::Text(s)) => s.clone(),
                _ => "<unknown>".into(),
            };
            actions.push(Action::new(
                "bus_factor",
                e.key.clone(),
                format!("sole owner: {}", owner),
                "Pair-review next change to this file with a second author",
            ));
        }
    }

    Pillar {
        key: "bus_factor",
        display: "Bus Factor",
        score,
        summary: format!("{} high-risk silos, {} stale owners", high_risk, stale),
        actions,
    }
}

/// Pillar 3: refactoring debt from `complexity` and `outliers`.
fn score_refactoring_debt(by_name: &HashMap<&str, &MetricResult>) -> Pillar {
    let complex = by_name
        .get("complexity")
        .map(|r| r.entries.as_slice())
        .unwrap_or(&[]);
    let outliers = by_name
        .get("outliers")
        .map(|r| r.entries.as_slice())
        .unwrap_or(&[]);

    let very_high = complex
        .iter()
        .filter(|e| match e.values.get("cyclomatic") {
            Some(MetricValue::Count(c)) => *c >= 20,
            _ => false,
        })
        .count();
    let high = complex
        .iter()
        .filter(|e| match e.values.get("cyclomatic") {
            Some(MetricValue::Count(c)) => *c >= 11 && *c < 20,
            _ => false,
        })
        .count();

    let outlier_count = outliers.len();

    // CC≥20 is twice as painful as CC 11-19. Outliers add a small kicker.
    let penalty = (very_high as f64 * 4.0).min(50.0)
        + (high as f64 * 1.0).min(20.0)
        + (outlier_count as f64 * 0.5).min(20.0);
    let score = (100.0 - penalty).max(MIN_PILLAR_SCORE);

    let mut actions: Vec<Action> = Vec::new();
    for e in complex.iter().take(3) {
        if let Some(MetricValue::Count(c)) = e.values.get("cyclomatic")
            && *c >= 20
        {
            let lines = match e.values.get("lines") {
                Some(MetricValue::Count(l)) => *l,
                _ => 0,
            };
            actions.push(Action::new(
                "refactoring_debt",
                e.key.clone(),
                format!("cyclomatic {} ({} code lines)", c, lines),
                "Extract branches into helper fns; add a dedicated test first",
            ));
        }
    }

    Pillar {
        key: "refactoring_debt",
        display: "Refactoring Debt",
        score,
        summary: format!(
            "{} functions CC≥20, {} CC 11-19, {} outlier files",
            very_high, high, outlier_count
        ),
        actions,
    }
}

/// Pillar 4: repo tidiness from `bloat` and `composition`.
fn score_tidiness(by_name: &HashMap<&str, &MetricResult>) -> Pillar {
    let bloat = by_name
        .get("bloat")
        .map(|r| r.entries.as_slice())
        .unwrap_or(&[]);
    let composition = by_name.get("composition");

    // Every bloat finding that isn't "OK" in the recommendation column counts.
    let bloat_findings = bloat
        .iter()
        .filter(
            |e| !matches!(e.values.get("recommendation"), Some(MetricValue::Text(s)) if s == "OK"),
        )
        .count();

    // Composition may report `(binary/skipped)` and `(unknown)` buckets. Those
    // aren't direct hygiene problems but they hint at vendored or generated
    // content that probably shouldn't be in git.
    let binary_files = composition
        .and_then(|r| r.entries.iter().find(|e| e.key == "(binary/skipped)"))
        .and_then(|e| match e.values.get("files") {
            Some(MetricValue::Count(n)) => Some(*n),
            _ => None,
        })
        .unwrap_or(0);

    let penalty = (bloat_findings as f64 * 3.0).min(60.0) + (binary_files as f64 * 1.0).min(20.0);
    let score = (100.0 - penalty).max(MIN_PILLAR_SCORE);

    let mut actions: Vec<Action> = Vec::new();
    for e in bloat.iter().take(3) {
        let rec = match e.values.get("recommendation") {
            Some(MetricValue::Text(s)) => s.clone(),
            _ => continue,
        };
        if rec == "OK" {
            continue;
        }
        let cmd = bloat_command_for(&e.key);
        actions.push(Action::new("tidiness", e.key.clone(), rec, cmd));
    }

    Pillar {
        key: "tidiness",
        display: "Repo Tidiness",
        score,
        summary: format!(
            "{} bloat findings, {} binary/skipped files",
            bloat_findings, binary_files
        ),
        actions,
    }
}

/// Pillar 5: churn concentration from `churn_pareto`.
fn score_change_concentration(by_name: &HashMap<&str, &MetricResult>) -> Pillar {
    // `churn_pareto` emits per-file entries sorted by churn desc, with
    // `cumulative_pct` telling us how much of total churn the top N files
    // account for. A healthy repo spreads churn; an unhealthy one has
    // everything in a handful of files (high Gini coefficient).
    let pareto = by_name
        .get("churn_pareto")
        .map(|r| r.entries.as_slice())
        .unwrap_or(&[]);

    let total_files = pareto
        .iter()
        .filter(|e| e.key != "summary" && !e.key.is_empty())
        .count();
    if total_files < 5 {
        // Too small to draw a line.
        return Pillar {
            key: "change_concentration",
            display: "Change Concentration",
            score: 100.0,
            summary: "not enough files to measure".into(),
            actions: vec![],
        };
    }

    // How many of the top 20% of files are needed to cover 80% of churn?
    // Healthier: curve is flatter, more files share churn.
    let top_20_count = (total_files / 5).max(1);
    let cumulative_at_top20 = pareto
        .iter()
        .filter(|e| e.key != "summary")
        .nth(top_20_count.saturating_sub(1))
        .and_then(|e| match e.values.get("cumulative_pct") {
            Some(MetricValue::Count(n)) => Some(*n as f64),
            _ => None,
        })
        .unwrap_or(80.0);

    // 80% is textbook Pareto — treat that as neutral. Heavier concentration
    // (cumulative > 80%) is penalised linearly; lighter is rewarded.
    let deviation = cumulative_at_top20 - 80.0;
    let score = (100.0 - deviation.max(0.0) * 2.0).max(MIN_PILLAR_SCORE);

    let summary = format!(
        "top 20% of files carry {:.0}% of churn (80% is textbook Pareto)",
        cumulative_at_top20
    );

    Pillar {
        key: "change_concentration",
        display: "Change Concentration",
        score,
        summary,
        actions: vec![],
    }
}

// ---------------------------------------------------------------------------
// Repo hygiene
// ---------------------------------------------------------------------------

/// Inspect the git directory and cross-reference with bloat findings to
/// produce concrete cleanup commands. Never rewrites history automatically —
/// each command is for the user to run after reading the warning.
fn repo_hygiene_findings(
    repo_path: &Path,
    by_name: &HashMap<&str, &MetricResult>,
) -> Vec<HygieneFinding> {
    let mut out: Vec<HygieneFinding> = Vec::new();
    let git_dir = repo_path.join(".git");

    if let Some(size) = dir_size_bytes(&git_dir)
        && let mb = size / (1024 * 1024)
        && mb >= 1024
    {
        out.push(HygieneFinding {
            detail: format!(".git is {} MB — consider Git LFS for large binaries", mb),
            command: "# https://git-lfs.com/ — migrate existing large blobs with `git lfs migrate import`".into(),
            history_rewrite: false,
        });
    }

    if let Some(packs) = count_pack_files(&git_dir)
        && packs > 50
    {
        out.push(HygieneFinding {
            detail: format!("{} pack files — repo is fragmented", packs),
            command: "git repack -a -d --depth=250 --window=250".into(),
            history_rewrite: false,
        });
    }

    if let Some(loose) = count_loose_objects(&git_dir)
        && loose > 10_000
    {
        out.push(HygieneFinding {
            detail: format!("{} loose objects — housekeeping overdue", loose),
            command: "git gc --aggressive --prune=now".into(),
            history_rewrite: false,
        });
    }

    // Cross-ref: if bloat surfaced a vendored directory, suggest removing it
    // from tracking. If it found a large binary, suggest filter-repo (heavy).
    if let Some(bloat) = by_name.get("bloat") {
        for e in bloat.entries.iter().take(5) {
            let rec = match e.values.get("recommendation") {
                Some(MetricValue::Text(s)) => s.clone(),
                _ => continue,
            };
            if rec.contains("Vendored") || rec.contains("Build output") || rec.contains("IDE") {
                out.push(HygieneFinding {
                    detail: format!("{}: {}", e.key, rec),
                    command: format!(
                        "git rm -rf --cached '{}' && echo '{}' >> .gitignore",
                        e.key, e.key
                    ),
                    history_rewrite: false,
                });
            } else if rec.contains("Very large") || rec.contains("Large") {
                out.push(HygieneFinding {
                    detail: format!("{}: {}", e.key, rec),
                    command: format!(
                        "git filter-repo --path '{}' --invert-paths  # rewrites history",
                        e.key
                    ),
                    history_rewrite: true,
                });
            }
        }
    }

    out
}

struct HygieneFinding {
    detail: String,
    command: String,
    history_rewrite: bool,
}

fn dir_size_bytes(path: &Path) -> Option<u64> {
    let mut total: u64 = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&p) else {
            continue;
        };
        for entry in read.flatten() {
            let Ok(md) = entry.metadata() else { continue };
            if md.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(md.len());
            }
        }
    }
    if total == 0 { None } else { Some(total) }
}

fn count_pack_files(git_dir: &Path) -> Option<usize> {
    let pack_dir = git_dir.join("objects").join("pack");
    let read = std::fs::read_dir(pack_dir).ok()?;
    let n = read
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("pack"))
                .unwrap_or(false)
        })
        .count();
    Some(n)
}

fn count_loose_objects(git_dir: &Path) -> Option<usize> {
    let objects = git_dir.join("objects");
    let read = std::fs::read_dir(&objects).ok()?;
    let mut n: usize = 0;
    for entry in read.flatten() {
        let name = entry.file_name();
        let Some(s) = name.to_str() else { continue };
        // Loose object subdirs are hex: "00".."ff".
        if s.len() != 2 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        if let Ok(sub) = std::fs::read_dir(entry.path()) {
            n = n.saturating_add(sub.flatten().count());
        }
    }
    Some(n)
}

// ---------------------------------------------------------------------------
// Result assembly
// ---------------------------------------------------------------------------

fn build_result(overall: u64, pillars: Vec<Pillar>, hygiene: Vec<HygieneFinding>) -> MetricResult {
    let columns = vec![
        "score".to_string(),
        "details".to_string(),
        "action".to_string(),
    ];

    let overall_entry = MetricEntry {
        key: "overall".into(),
        values: values(
            Some(overall),
            format!("{}/100", overall),
            interpretation(overall),
        ),
    };

    let pillar_entries: Vec<MetricEntry> = pillars
        .iter()
        .map(|p| MetricEntry {
            key: p.key.to_string(),
            values: values(
                Some(p.score.round() as u64),
                p.summary.clone(),
                format!("{} — see actions below", p.display),
            ),
        })
        .collect();

    // Gather all actions, take top MAX_ACTIONS in pillar-priority order.
    let mut all_actions: Vec<&Action> = pillars.iter().flat_map(|p| p.actions.iter()).collect();
    // Stable order: preserve pillar insertion order by using `enumerate` index.
    all_actions.truncate(MAX_ACTIONS);

    let action_entries: Vec<MetricEntry> = all_actions
        .into_iter()
        .map(|a| MetricEntry {
            key: format!("{}: {}", a.pillar, a.target),
            values: values(None, a.detail.clone(), a.command.clone()),
        })
        .collect();

    let hygiene_entries: Vec<MetricEntry> = hygiene
        .into_iter()
        .enumerate()
        .map(|(i, h)| {
            let key = if h.history_rewrite {
                format!("#{} ⚠ history-rewrite", i + 1)
            } else {
                format!("#{} safe", i + 1)
            };
            MetricEntry {
                key,
                values: values(None, h.detail, h.command),
            }
        })
        .collect();

    let mut entry_groups: Vec<(String, Vec<MetricEntry>)> = Vec::new();
    entry_groups.push(("Overall".into(), vec![overall_entry]));
    entry_groups.push(("Pillars (0-100 each, equal weight)".into(), pillar_entries));
    if !action_entries.is_empty() {
        entry_groups.push(("Start here — actions".into(), action_entries));
    }
    if !hygiene_entries.is_empty() {
        entry_groups.push(("Repo hygiene — run these".into(), hygiene_entries));
    }

    MetricResult {
        name: "health".into(),
        display_name: "Health Score".into(),
        description: "Overall repository health on a 0-100 scale, built from five equally-weighted pillars: commit discipline, bus factor, refactoring debt, repo tidiness, and change concentration. Thresholds are opinionated — see scoring/health.rs for the rules. The 'Start here' list picks the highest-impact concrete actions from each pillar; 'Repo hygiene' lists cleanup commands (never run automatically).".into(),
        columns,
        column_labels: vec![],
        entries: vec![],
        entry_groups,
    }
}

fn values(
    score: Option<u64>,
    details: impl Into<String>,
    action: impl Into<String>,
) -> HashMap<String, MetricValue> {
    let mut m = HashMap::new();
    match score {
        Some(s) => m.insert("score".into(), MetricValue::Count(s)),
        None => m.insert("score".into(), MetricValue::Text("—".into())),
    };
    m.insert("details".into(), MetricValue::Text(details.into()));
    m.insert("action".into(), MetricValue::Text(action.into()));
    m
}

fn interpretation(score: u64) -> &'static str {
    match score {
        90..=100 => "excellent — keep current practices",
        80..=89 => "good — minor improvements available",
        70..=79 => "fair — address top pillar actions",
        60..=69 => "concerning — meaningful cleanup needed",
        _ => "poor — prioritise refactoring and hygiene",
    }
}

fn over(value: f64, threshold: f64) -> f64 {
    (value - threshold).max(0.0)
}

fn bloat_command_for(path: &str) -> String {
    if path.contains("node_modules/")
        || path.starts_with("dist/")
        || path.starts_with("build/")
        || path.starts_with("target/")
        || path.starts_with("vendor/")
    {
        format!(
            "git rm -rf --cached '{}' && echo '{}/' >> .gitignore",
            path,
            path.trim_end_matches('/')
        )
    } else if path.ends_with(".min.js") || path.ends_with(".min.css") {
        format!(
            "# Build artifact — remove from tracking:\ngit rm --cached '{}' && echo 'dist/*.min.*' >> .gitignore",
            path
        )
    } else {
        format!(
            "git filter-repo --path '{}' --invert-paths  # rewrites history",
            path
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_result(name: &str, entries: Vec<MetricEntry>) -> MetricResult {
        MetricResult {
            name: name.into(),
            display_name: name.into(),
            description: String::new(),
            columns: vec![],
            column_labels: vec![],
            entries,
            entry_groups: vec![],
        }
    }

    fn mk_entry(key: &str, values: &[(&str, MetricValue)]) -> MetricEntry {
        MetricEntry {
            key: key.into(),
            values: values
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }

    #[test]
    fn over_returns_zero_when_below_threshold() {
        assert_eq!(over(3.0, 5.0), 0.0);
        assert_eq!(over(7.0, 5.0), 2.0);
    }

    #[test]
    fn interpretation_covers_all_bands() {
        assert!(interpretation(95).starts_with("excellent"));
        assert!(interpretation(85).starts_with("good"));
        assert!(interpretation(75).starts_with("fair"));
        assert!(interpretation(65).starts_with("concerning"));
        assert!(interpretation(40).starts_with("poor"));
    }

    #[test]
    fn commit_discipline_perfect_when_quality_missing() {
        let by_name = HashMap::new();
        let p = score_commit_discipline(&by_name);
        assert_eq!(p.score as u64, 100);
    }

    #[test]
    fn commit_discipline_penalised_for_bad_signals() {
        let quality = mk_result(
            "quality",
            vec![
                mk_entry(
                    "low_quality_messages",
                    &[("percent", MetricValue::Float(25.0))],
                ),
                mk_entry("mega_commits", &[("percent", MetricValue::Float(30.0))]),
                mk_entry("revert_commits", &[("percent", MetricValue::Float(10.0))]),
            ],
        );
        let mut by_name = HashMap::new();
        by_name.insert("quality", &quality);
        let p = score_commit_discipline(&by_name);
        assert!(p.score < 100.0, "expected penalty, got score {}", p.score);
        assert!(!p.actions.is_empty(), "should surface actions");
    }

    #[test]
    fn refactoring_debt_counts_very_high_cc() {
        let complex = mk_result(
            "complexity",
            vec![
                mk_entry(
                    "a.rs::foo:1",
                    &[
                        ("cyclomatic", MetricValue::Count(25)),
                        ("lines", MetricValue::Count(120)),
                    ],
                ),
                mk_entry(
                    "a.rs::bar:10",
                    &[
                        ("cyclomatic", MetricValue::Count(15)),
                        ("lines", MetricValue::Count(40)),
                    ],
                ),
                mk_entry(
                    "a.rs::baz:50",
                    &[
                        ("cyclomatic", MetricValue::Count(5)),
                        ("lines", MetricValue::Count(10)),
                    ],
                ),
            ],
        );
        let mut by_name = HashMap::new();
        by_name.insert("complexity", &complex);
        let p = score_refactoring_debt(&by_name);
        assert!(p.score < 100.0);
        // One action for the CC=25 entry.
        assert_eq!(p.actions.len(), 1);
        assert!(p.actions[0].target.contains("foo"));
    }

    #[test]
    fn tidiness_rewards_clean_bloat_report() {
        let bloat = mk_result(
            "bloat",
            vec![mk_entry(
                "src/main.rs",
                &[("recommendation", MetricValue::Text("OK".into()))],
            )],
        );
        let mut by_name = HashMap::new();
        by_name.insert("bloat", &bloat);
        let p = score_tidiness(&by_name);
        assert_eq!(p.score as u64, 100);
    }

    #[test]
    fn tidiness_punishes_vendored_content() {
        let bloat = mk_result(
            "bloat",
            vec![mk_entry(
                "node_modules/foo",
                &[(
                    "recommendation",
                    MetricValue::Text("Vendored dependencies — add to .gitignore".into()),
                )],
            )],
        );
        let mut by_name = HashMap::new();
        by_name.insert("bloat", &bloat);
        let p = score_tidiness(&by_name);
        assert!(p.score < 100.0);
        assert!(!p.actions.is_empty());
        assert!(p.actions[0].command.contains("git rm"));
    }

    #[test]
    fn bloat_command_handles_vendored_paths() {
        let cmd = bloat_command_for("node_modules/react");
        assert!(cmd.contains("git rm"));
        assert!(cmd.contains("gitignore"));
    }

    #[test]
    fn bloat_command_suggests_filter_repo_for_large_random_path() {
        let cmd = bloat_command_for("archive/big_blob.bin");
        assert!(cmd.contains("filter-repo"));
    }

    #[test]
    fn compute_health_with_no_inputs_still_returns_result() {
        let tmp = std::env::temp_dir();
        let result = compute_health(&[], &tmp).expect("should produce result");
        assert_eq!(result.name, "health");
        // Without any collector data, every pillar is effectively 100.
        let overall = result.entry_groups[0].1[0]
            .values
            .get("score")
            .and_then(|v| match v {
                MetricValue::Count(n) => Some(*n),
                _ => None,
            })
            .unwrap();
        assert_eq!(overall, 100);
    }
}
