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

use crate::messages;
use crate::types::{
    Column, EntryGroup, LocalizedMessage, MetricEntry, MetricResult, MetricValue, Severity,
    report_description, report_display,
};

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
    let insights = collect_insights(&by_name);
    let supplementary: Vec<SupplementaryScore> = [
        score_architecture(&by_name),
        score_team_health(&by_name),
        score_activity(&by_name),
    ]
    .into_iter()
    .flatten()
    .collect();

    Some(build_result(
        overall_score,
        pillars,
        hygiene,
        insights,
        supplementary,
    ))
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
    summary: LocalizedMessage,
    actions: Vec<Action>,
}

/// A concrete, file- or function-level action the user can take to improve
/// one pillar. Consolidated across pillars and capped at [`MAX_ACTIONS`] in
/// the final output.
struct Action {
    pillar: &'static str,
    level: LocalizedMessage,
    target: String,
    detail: LocalizedMessage,
    command: LocalizedMessage,
}

impl Action {
    fn new(
        pillar: &'static str,
        level: LocalizedMessage,
        target: impl Into<String>,
        detail: LocalizedMessage,
        command: LocalizedMessage,
    ) -> Self {
        Self {
            pillar,
            level,
            target: target.into(),
            detail,
            command,
        }
    }
}

fn critical() -> LocalizedMessage {
    LocalizedMessage::code(messages::HEALTH_ACTION_CRITICAL).with_severity(Severity::Critical)
}

fn warning() -> LocalizedMessage {
    LocalizedMessage::code(messages::HEALTH_ACTION_WARNING).with_severity(Severity::Warning)
}

fn info() -> LocalizedMessage {
    LocalizedMessage::code(messages::HEALTH_ACTION_INFO).with_severity(Severity::Info)
}

fn level_for_score(score: f64) -> LocalizedMessage {
    if score >= 80.0 {
        LocalizedMessage::code(messages::HEALTH_ACTION_OK)
    } else if score >= 60.0 {
        warning()
    } else {
        critical()
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
            warning(),
            "commit messages",
            LocalizedMessage::code(messages::HEALTH_DETAIL_LOW_QUALITY)
                .with_param("pct", format!("{:.1}", low_q)),
            LocalizedMessage::code(messages::HEALTH_COMMAND_ENFORCE_CONVENTIONAL),
        ));
    }
    if mega > 10.0 {
        actions.push(Action::new(
            "commit_discipline",
            warning(),
            "mega commits",
            LocalizedMessage::code(messages::HEALTH_DETAIL_MEGA_COMMITS)
                .with_param("pct", format!("{:.1}", mega)),
            LocalizedMessage::code(messages::HEALTH_COMMAND_SPLIT_COMMITS),
        ));
    }
    if revert > 3.0 {
        actions.push(Action::new(
            "commit_discipline",
            critical(),
            "revert rate",
            LocalizedMessage::code(messages::HEALTH_DETAIL_HIGH_REVERT)
                .with_param("pct", format!("{:.1}", revert)),
            LocalizedMessage::code(messages::HEALTH_COMMAND_REQUIRE_REVIEW),
        ));
    }

    let summary = LocalizedMessage::code(messages::HEALTH_SUMMARY_COMMIT_DISCIPLINE)
        .with_param("short", format!("{:.0}", short))
        .with_param("low_q", format!("{:.0}", low_q))
        .with_param("mega", format!("{:.0}", mega))
        .with_param("revert", format!("{:.0}", revert))
        .with_param("merge", format!("{:.0}", merge));

    Pillar {
        key: "commit_discipline",
        display: messages::HEALTH_PILLAR_COMMIT_DISCIPLINE,
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
        .filter(|e| {
            matches!(
                e.values.get("risk"),
                Some(MetricValue::Message(m))
                    if m.code == messages::KNOWLEDGE_SILO_RISK_AT_RISK
            )
        })
        .count();

    let stale = succ
        .iter()
        .filter(|e| {
            matches!(
                e.values.get("status"),
                Some(MetricValue::Message(m))
                    if m.code == messages::SUCCESSION_STATUS_ORPHANED
                        || m.code == messages::SUCCESSION_STATUS_KNOWLEDGE_TRANSFER_NEEDED
            )
        })
        .count();

    // Up to 50 points off for heavy silo load, up to 30 for stale succession.
    let silo_penalty = (high_risk as f64 * 2.0).min(50.0);
    let stale_penalty = (stale as f64 * 1.5).min(30.0);

    // Positive: well-distributed ownership partially offsets silo risk.
    let ownership_bonus = by_name
        .get("ownership")
        .map(|o| {
            let total = o.entries.len();
            if total == 0 {
                return 0.0;
            }
            let well_owned = o
                .entries
                .iter()
                .filter(|e| {
                    matches!(e.values.get("bus_factor"), Some(MetricValue::Count(n)) if *n >= 3)
                })
                .count();
            let pct = (well_owned * 100) as f64 / total as f64;
            if pct > 60.0 {
                ((pct - 60.0) * 0.125).min(5.0)
            } else {
                0.0
            }
        })
        .unwrap_or(0.0);

    let score =
        (100.0 - silo_penalty - stale_penalty + ownership_bonus).clamp(MIN_PILLAR_SCORE, 100.0);

    let mut actions: Vec<Action> = Vec::new();
    for e in silos.iter().take(3) {
        if matches!(
            e.values.get("risk"),
            Some(MetricValue::Message(m)) if m.code == messages::KNOWLEDGE_SILO_RISK_AT_RISK
        ) {
            let owner = match e.values.get("owner") {
                Some(MetricValue::Text(s)) => s.clone(),
                _ => "<unknown>".into(),
            };
            actions.push(Action::new(
                "bus_factor",
                critical(),
                e.key.clone(),
                LocalizedMessage::code(messages::HEALTH_DETAIL_SOLE_OWNER)
                    .with_param("owner", owner),
                LocalizedMessage::code(messages::HEALTH_COMMAND_PAIR_REVIEW),
            ));
        }
    }

    Pillar {
        key: "bus_factor",
        display: messages::HEALTH_PILLAR_BUS_FACTOR,
        score,
        summary: LocalizedMessage::code(messages::HEALTH_SUMMARY_BUS_FACTOR)
            .with_param("high_risk", high_risk as u64)
            .with_param("stale", stale as u64),
        actions,
    }
}

/// Pillar 3: refactoring debt from `complexity`, `outliers`, and stale
/// debt markers (TODO/FIXME/HACK/XXX comments older than 6 months).
fn score_refactoring_debt(by_name: &HashMap<&str, &MetricResult>) -> Pillar {
    let complex = by_name
        .get("complexity")
        .map(|r| r.entries.as_slice())
        .unwrap_or(&[]);
    let outliers = by_name
        .get("outliers")
        .map(|r| r.entries.as_slice())
        .unwrap_or(&[]);
    let markers = by_name
        .get("debt_markers")
        .map(|r| r.entries.as_slice())
        .unwrap_or(&[]);
    let large = by_name
        .get("large_sources")
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

    // Only markers older than 180 days count. Fresh TODOs are normal backlog,
    // not debt. Rotten (>365d) TODOs hurt a bit more than merely-stale ones.
    let stale_markers = markers
        .iter()
        .filter(|e| match e.values.get("age_days") {
            Some(MetricValue::Count(d)) => *d >= 180 && *d < 365,
            _ => false,
        })
        .count();
    let rotten_markers = markers
        .iter()
        .filter(|e| match e.values.get("age_days") {
            Some(MetricValue::Count(d)) => *d >= 365,
            _ => false,
        })
        .count();

    // Large-source files are split candidates. A single >5k file ("god
    // module") is painful; a handful of 1.5k-5k files less so.
    let very_large_sources = large
        .iter()
        .filter(|e| match e.values.get("code_lines") {
            Some(MetricValue::Count(n)) => *n >= 5000,
            _ => false,
        })
        .count();
    let sizeable_sources = large
        .iter()
        .filter(|e| match e.values.get("code_lines") {
            Some(MetricValue::Count(n)) => *n >= 1500 && *n < 5000,
            _ => false,
        })
        .count();

    // CC≥20 is twice as painful as CC 11-19. Outliers add a small kicker.
    // Rotten markers and god-modules add a steady trickle — no single one
    // tanks the score, but a backlog of them will.
    let penalty = (very_high as f64 * 4.0).min(50.0)
        + (high as f64 * 1.0).min(20.0)
        + (outlier_count as f64 * 0.5).min(20.0)
        + (stale_markers as f64 * 0.3).min(10.0)
        + (rotten_markers as f64 * 0.6).min(20.0)
        + (very_large_sources as f64 * 3.0).min(15.0)
        + (sizeable_sources as f64 * 0.5).min(10.0);
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
                critical(),
                e.key.clone(),
                LocalizedMessage::code(messages::HEALTH_DETAIL_HIGH_COMPLEXITY)
                    .with_param("cc", *c)
                    .with_param("lines", lines),
                LocalizedMessage::code(messages::HEALTH_COMMAND_EXTRACT_BRANCHES),
            ));
        }
    }
    // Biggest source file — surface the top-1 as a concrete split target.
    if let Some(biggest) = large.first()
        && let Some(MetricValue::Count(lines)) = biggest.values.get("code_lines")
        && *lines >= 1500
    {
        actions.push(Action::new(
            "refactoring_debt",
            warning(),
            biggest.key.clone(),
            LocalizedMessage::code(messages::HEALTH_DETAIL_GOD_MODULE).with_param("lines", *lines),
            LocalizedMessage::code(messages::HEALTH_COMMAND_SPLIT_MODULE),
        ));
    }

    // Oldest rotten marker as a concrete candidate to resolve or delete.
    if let Some(oldest) = markers
        .iter()
        .find(|e| matches!(e.values.get("age_days"), Some(MetricValue::Count(d)) if *d >= 365))
    {
        let age = match oldest.values.get("age_days") {
            Some(MetricValue::Count(d)) => *d,
            _ => 0,
        };
        let marker = match oldest.values.get("marker") {
            Some(MetricValue::Text(s)) => s.clone(),
            _ => "TODO".into(),
        };
        actions.push(Action::new(
            "refactoring_debt",
            warning(),
            oldest.key.clone(),
            LocalizedMessage::code(messages::HEALTH_DETAIL_ROTTEN_MARKER)
                .with_param("marker", marker)
                .with_param("days", age),
            LocalizedMessage::code(messages::HEALTH_COMMAND_RESOLVE_MARKER),
        ));
    }

    Pillar {
        key: "refactoring_debt",
        display: messages::HEALTH_PILLAR_REFACTORING_DEBT,
        score,
        summary: LocalizedMessage::code(messages::HEALTH_SUMMARY_REFACTORING_DEBT)
            .with_param("very_high", very_high as u64)
            .with_param("high", high as u64)
            .with_param("outliers", outlier_count as u64)
            .with_param("stale", stale_markers as u64)
            .with_param("rotten", rotten_markers as u64)
            .with_param("huge", very_large_sources as u64)
            .with_param("sizeable", sizeable_sources as u64),
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
        .filter(|e| {
            !matches!(
                e.values.get("recommendation"),
                Some(MetricValue::Message(m)) if m.code == messages::BLOAT_RECOMMENDATION_OK
            )
        })
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

    // Positive: active cleanup (high deletion ratio) suggests good housekeeping.
    let cleanup_bonus = by_name
        .get("churn")
        .map(|c| {
            let (added, deleted) = c.entries.iter().fold((0u64, 0u64), |(a, d), e| {
                let a2 = match e.values.get("lines_added") {
                    Some(MetricValue::Count(n)) => *n,
                    _ => 0,
                };
                let d2 = match e.values.get("lines_deleted") {
                    Some(MetricValue::Count(n)) => *n,
                    _ => 0,
                };
                (a + a2, d + d2)
            });
            if added == 0 {
                return 0.0;
            }
            let ratio = deleted as f64 / added as f64;
            if ratio > 0.8 {
                ((ratio - 0.8) * 25.0).min(5.0)
            } else {
                0.0
            }
        })
        .unwrap_or(0.0);

    let score = (100.0 - penalty + cleanup_bonus).clamp(MIN_PILLAR_SCORE, 100.0);

    let mut actions: Vec<Action> = Vec::new();
    for e in bloat.iter().take(3) {
        let Some(MetricValue::Message(m)) = e.values.get("recommendation") else {
            continue;
        };
        if m.code == messages::BLOAT_RECOMMENDATION_OK {
            continue;
        }
        let cmd = bloat_command_for(&e.key);
        actions.push(Action::new(
            "tidiness",
            warning(),
            e.key.clone(),
            // Detail carries the bloat code so the localised UI can render the
            // reason alongside the suggested shell command.
            LocalizedMessage::code(m.code.clone()),
            LocalizedMessage::code(cmd),
        ));
    }

    Pillar {
        key: "tidiness",
        display: messages::HEALTH_PILLAR_TIDINESS,
        score,
        summary: LocalizedMessage::code(messages::HEALTH_SUMMARY_TIDINESS)
            .with_param("bloat", bloat_findings as u64)
            .with_param("binary", binary_files),
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
            display: messages::HEALTH_PILLAR_CHANGE_CONCENTRATION,
            score: 100.0,
            summary: LocalizedMessage::code(messages::HEALTH_SUMMARY_NOT_ENOUGH_FILES),
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

    let summary = LocalizedMessage::code(messages::HEALTH_SUMMARY_CHANGE_CONCENTRATION)
        .with_param("pct", format!("{:.0}", cumulative_at_top20));

    Pillar {
        key: "change_concentration",
        display: messages::HEALTH_PILLAR_CHANGE_CONCENTRATION,
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
            detail: LocalizedMessage::code(messages::HEALTH_HYGIENE_LARGE_GIT_DIR)
                .with_param("mb", mb),
            command: "# https://git-lfs.com/ — migrate existing large blobs with `git lfs migrate import`".into(),
            history_rewrite: false,
        });
    }

    if let Some(packs) = count_pack_files(&git_dir)
        && packs > 50
    {
        out.push(HygieneFinding {
            detail: LocalizedMessage::code(messages::HEALTH_HYGIENE_FRAGMENTED_PACKS)
                .with_param("packs", packs as u64),
            command: "git repack -a -d --depth=250 --window=250".into(),
            history_rewrite: false,
        });
    }

    if let Some(loose) = count_loose_objects(&git_dir)
        && loose > 10_000
    {
        out.push(HygieneFinding {
            detail: LocalizedMessage::code(messages::HEALTH_HYGIENE_LOOSE_OBJECTS)
                .with_param("loose", loose as u64),
            command: "git gc --aggressive --prune=now".into(),
            history_rewrite: false,
        });
    }

    // Cross-ref: if bloat surfaced a vendored directory, suggest removing it
    // from tracking. If it found a large binary, suggest filter-repo (heavy).
    if let Some(bloat) = by_name.get("bloat") {
        for e in bloat.entries.iter().take(5) {
            let Some(MetricValue::Message(m)) = e.values.get("recommendation") else {
                continue;
            };
            let code = m.code.as_str();
            let is_cached = matches!(
                code,
                c if c == messages::BLOAT_RECOMMENDATION_VENDORED_DEPS
                    || c == messages::BLOAT_RECOMMENDATION_BUILD_OUTPUT
                    || c == messages::BLOAT_RECOMMENDATION_RUST_BUILD_OUTPUT
                    || c == messages::BLOAT_RECOMMENDATION_IDE_CONFIG
            );
            let is_history_rewrite = matches!(
                code,
                c if c == messages::BLOAT_RECOMMENDATION_LARGE_FILE
                    || c == messages::BLOAT_RECOMMENDATION_VERY_LARGE_FILE
            );
            if is_cached {
                out.push(HygieneFinding {
                    detail: LocalizedMessage::code(messages::HEALTH_HYGIENE_BLOAT_FINDING)
                        .with_param("path", e.key.clone())
                        .with_param("reason", code),
                    command: format!(
                        "git rm -rf --cached '{}' && echo '{}' >> .gitignore",
                        e.key, e.key
                    ),
                    history_rewrite: false,
                });
            } else if is_history_rewrite {
                out.push(HygieneFinding {
                    detail: LocalizedMessage::code(messages::HEALTH_HYGIENE_BLOAT_FINDING)
                        .with_param("path", e.key.clone())
                        .with_param("reason", code),
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
    detail: LocalizedMessage,
    command: String,
    history_rewrite: bool,
}

/// An informational finding that enriches the health report without affecting
/// the score. Surfaced in the "Insights" group.
struct Insight {
    key: String,
    detail: LocalizedMessage,
    note: LocalizedMessage,
}

/// A supplementary 0-100 score that provides an additional dimension of
/// information without affecting the overall health score.
struct SupplementaryScore {
    key: &'static str,
    display: &'static str,
    score: f64,
    summary: LocalizedMessage,
}

/// Architecture score (0-100): how modular and decoupled is the codebase?
/// Derived from coupling, module_coupling, and fan_in_out data.
fn score_architecture(by_name: &HashMap<&str, &MetricResult>) -> Option<SupplementaryScore> {
    let coupling = by_name.get("coupling");
    let module_coupling = by_name.get("module_coupling");
    let fio = by_name.get("fan_in_out");

    // Need at least one source of data.
    if coupling.is_none() && module_coupling.is_none() && fio.is_none() {
        return None;
    }

    let tight_couples = coupling
        .map(|c| {
            c.entries
                .iter()
                .filter(
                    |e| matches!(e.values.get("score"), Some(MetricValue::Float(s)) if *s > 0.7),
                )
                .count()
        })
        .unwrap_or(0);

    let (hubs, high_instability) = fio
        .map(|f| {
            let h = f
                .entries
                .iter()
                .filter(|e| {
                    matches!(
                        e.values.get("role"),
                        Some(MetricValue::Message(m)) if m.code == messages::FAN_IN_OUT_ROLE_HUB
                    )
                })
                .count();
            let hi = f
                .entries
                .iter()
                .filter(
                    |e| matches!(e.values.get("instability_pct"), Some(MetricValue::Count(p)) if *p > 80),
                )
                .count();
            (h, hi)
        })
        .unwrap_or((0, 0));

    let mc_pairs = module_coupling.map(|m| m.entries.len()).unwrap_or(0);

    let penalty = (tight_couples as f64 * 4.0).min(30.0)
        + (hubs as f64 * 3.0).min(20.0)
        + (high_instability as f64 * 2.0).min(15.0)
        + (mc_pairs as f64 * 2.0).min(15.0);
    let score = (100.0 - penalty).max(MIN_PILLAR_SCORE);

    Some(SupplementaryScore {
        key: "architecture",
        display: messages::HEALTH_SCORE_ARCHITECTURE,
        score,
        summary: LocalizedMessage::code(messages::HEALTH_SUMMARY_ARCHITECTURE)
            .with_param("tight_couples", tight_couples as u64)
            .with_param("hubs", hubs as u64)
            .with_param("high_instability", high_instability as u64),
    })
}

/// Team health score (0-100): how well-distributed is knowledge and ownership?
/// Derived from authors and ownership data.
fn score_team_health(by_name: &HashMap<&str, &MetricResult>) -> Option<SupplementaryScore> {
    let authors = by_name.get("authors");
    let ownership = by_name.get("ownership");

    if authors.is_none() && ownership.is_none() {
        return None;
    }

    let total_authors = authors.map(|a| a.entries.len()).unwrap_or(0);

    let (well_owned_pct, single_owner_pct) = ownership
        .map(|o| {
            let total = o.entries.len();
            if total == 0 {
                return (0usize, 0usize);
            }
            let well = o
                .entries
                .iter()
                .filter(|e| {
                    matches!(e.values.get("bus_factor"), Some(MetricValue::Count(n)) if *n >= 3)
                })
                .count();
            let single = o
                .entries
                .iter()
                .filter(|e| {
                    matches!(e.values.get("bus_factor"), Some(MetricValue::Count(n)) if *n <= 1)
                })
                .count();
            (well * 100 / total, single * 100 / total)
        })
        .unwrap_or((0, 0));

    // Start at 70 (neutral). Good signals push up, bad push down.
    let mut score = 70.0;

    // Positive: many contributors
    if total_authors >= 10 {
        score += 15.0;
    } else if total_authors >= 5 {
        score += 10.0;
    } else if total_authors >= 3 {
        score += 5.0;
    }

    // Positive: well-distributed ownership
    score += (well_owned_pct as f64 * 0.15).min(10.0);

    // Negative: too many single-owner files
    if single_owner_pct > 50 {
        score -= ((single_owner_pct - 50) as f64 * 0.5).min(25.0);
    }

    let score = score.clamp(MIN_PILLAR_SCORE, 100.0);

    Some(SupplementaryScore {
        key: "team_health",
        display: messages::HEALTH_SCORE_TEAM,
        score,
        summary: LocalizedMessage::code(messages::HEALTH_SUMMARY_TEAM)
            .with_param("total_authors", total_authors as u64)
            .with_param("well_owned_pct", well_owned_pct as u64)
            .with_param("single_owner_pct", single_owner_pct as u64),
    })
}

/// Activity score (0-100): how actively and healthily is the codebase evolving?
/// Derived from churn and hotspots data.
fn score_activity(by_name: &HashMap<&str, &MetricResult>) -> Option<SupplementaryScore> {
    let churn = by_name.get("churn");
    let hotspots = by_name.get("hotspots");

    if churn.is_none() && hotspots.is_none() {
        return None;
    }

    let (total_added, total_deleted) = churn
        .map(|c| {
            c.entries.iter().fold((0u64, 0u64), |(a, d), e| {
                let a2 = match e.values.get("lines_added") {
                    Some(MetricValue::Count(n)) => *n,
                    _ => 0,
                };
                let d2 = match e.values.get("lines_deleted") {
                    Some(MetricValue::Count(n)) => *n,
                    _ => 0,
                };
                (a + a2, d + d2)
            })
        })
        .unwrap_or((0, 0));

    let hot_count = hotspots
        .map(|h| {
            h.entries
                .iter()
                .filter(|e| {
                    matches!(
                        e.values.get("level"),
                        Some(MetricValue::Message(m)) if m.code == messages::HOTSPOT_LEVEL_FILE
                    ) && matches!(
                        e.values.get("score"),
                        Some(MetricValue::Count(s)) if *s >= 100
                    )
                })
                .count()
        })
        .unwrap_or(0);

    // Start at 75 (neutral — some activity is expected).
    let mut score = 75.0;

    // Positive: active cleanup (deletion ratio > 0.5)
    if total_added > 0 {
        let ratio = total_deleted as f64 / total_added as f64;
        score += (ratio * 10.0).min(15.0);
    }

    // Negative: many hotspots mean churn is concentrated and risky
    score -= (hot_count as f64 * 1.5).min(20.0);

    // Positive: some activity at all
    if total_added + total_deleted > 1000 {
        score += 5.0;
    }

    let score = score.clamp(MIN_PILLAR_SCORE, 100.0);

    Some(SupplementaryScore {
        key: "activity",
        display: messages::HEALTH_SCORE_ACTIVITY,
        score,
        summary: LocalizedMessage::code(messages::HEALTH_SUMMARY_ACTIVITY)
            .with_param("added", total_added)
            .with_param("deleted", total_deleted)
            .with_param("hotspots", hot_count as u64),
    })
}

/// Gather informational findings from collectors that don't feed into the
/// score but provide useful context (team shape, architecture signals, trends).
fn collect_insights(by_name: &HashMap<&str, &MetricResult>) -> Vec<Insight> {
    let mut out = Vec::new();

    // -- Team size (from authors) --
    if let Some(authors) = by_name.get("authors") {
        let total = authors.entries.len();
        if total > 0 {
            out.push(Insight {
                key: "team".into(),
                detail: LocalizedMessage::code(messages::HEALTH_INSIGHT_TEAM)
                    .with_param("total", total as u64),
                note: LocalizedMessage::code(messages::HEALTH_INSIGHT_TEAM_NOTE),
            });
        }
    }

    // -- Ownership distribution (from ownership) --
    if let Some(ownership) = by_name.get("ownership") {
        let total_files = ownership.entries.len();
        let well_owned = ownership
            .entries
            .iter()
            .filter(
                |e| matches!(e.values.get("bus_factor"), Some(MetricValue::Count(n)) if *n >= 3),
            )
            .count();
        if let Some(pct) = (well_owned * 100).checked_div(total_files) {
            out.push(Insight {
                key: "ownership".into(),
                detail: LocalizedMessage::code(messages::HEALTH_INSIGHT_OWNERSHIP)
                    .with_param("well_owned_pct", pct as u64)
                    .with_param("well_owned", well_owned as u64)
                    .with_param("total_files", total_files as u64),
                note: LocalizedMessage::code(messages::HEALTH_INSIGHT_OWNERSHIP_NOTE),
            });
        }
    }

    // -- Top coupling pairs (from coupling) --
    if let Some(coupling) = by_name.get("coupling") {
        for e in coupling.entries.iter().take(3) {
            let (Some(MetricValue::Text(file_a)), Some(MetricValue::Text(file_b))) =
                (e.values.get("file_a"), e.values.get("file_b"))
            else {
                continue;
            };
            let Some(MetricValue::Count(co_changes)) = e.values.get("co_changes") else {
                continue;
            };
            let Some(MetricValue::Float(score)) = e.values.get("score") else {
                continue;
            };
            out.push(Insight {
                key: e.key.clone(),
                detail: LocalizedMessage::code(messages::HEALTH_INSIGHT_COUPLING)
                    .with_param("file_a", file_a.clone())
                    .with_param("file_b", file_b.clone())
                    .with_param("co_changes", *co_changes)
                    .with_param("score", format!("{score:.2}")),
                note: LocalizedMessage::code(messages::HEALTH_INSIGHT_COUPLING_NOTE),
            });
        }
    }

    // -- Top module coupling (from module_coupling) --
    if let Some(mc) = by_name.get("module_coupling") {
        for e in mc.entries.iter().take(2) {
            let (Some(MetricValue::Text(module_a)), Some(MetricValue::Text(module_b))) =
                (e.values.get("module_a"), e.values.get("module_b"))
            else {
                continue;
            };
            let Some(MetricValue::Count(co_changes)) = e.values.get("co_changes") else {
                continue;
            };
            let Some(MetricValue::Float(score)) = e.values.get("score") else {
                continue;
            };
            out.push(Insight {
                key: e.key.clone(),
                detail: LocalizedMessage::code(messages::HEALTH_INSIGHT_MODULE_COUPLING)
                    .with_param("module_a", module_a.clone())
                    .with_param("module_b", module_b.clone())
                    .with_param("co_changes", *co_changes)
                    .with_param("score", format!("{score:.2}")),
                note: LocalizedMessage::code(messages::HEALTH_INSIGHT_MODULE_COUPLING_NOTE),
            });
        }
    }

    // -- Hub files (from fan_in_out) --
    if let Some(fio) = by_name.get("fan_in_out") {
        let mut hub_count = 0u32;
        for e in &fio.entries {
            let is_hub = matches!(
                e.values.get("role"),
                Some(MetricValue::Message(m)) if m.code == messages::FAN_IN_OUT_ROLE_HUB
            );
            if !is_hub {
                continue;
            }
            hub_count += 1;
            if hub_count > 3 {
                continue;
            }
            let fan_in = match e.values.get("fan_in") {
                Some(MetricValue::Count(n)) => *n,
                _ => continue,
            };
            out.push(Insight {
                key: e.key.clone(),
                detail: LocalizedMessage::code(messages::HEALTH_INSIGHT_HUB)
                    .with_param("fan_in", fan_in),
                note: LocalizedMessage::code(messages::HEALTH_INSIGHT_HUB_NOTE),
            });
        }
    }

    // -- Churn trend (from churn) --
    if let Some(churn) = by_name.get("churn") {
        let (total_added, total_deleted) = churn.entries.iter().fold((0u64, 0u64), |(a, d), e| {
            let a2 = match e.values.get("lines_added") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            let d2 = match e.values.get("lines_deleted") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            (a + a2, d + d2)
        });
        if total_added > 0 || total_deleted > 0 {
            let net = total_added as i64 - total_deleted as i64;
            let note_code = if net < 0 {
                messages::HEALTH_INSIGHT_ACTIVE_CLEANUP
            } else {
                messages::HEALTH_INSIGHT_GROWING_CODEBASE
            };
            out.push(Insight {
                key: "churn_trend".into(),
                detail: LocalizedMessage::code(messages::HEALTH_INSIGHT_CHURN_TREND)
                    .with_param("added", total_added)
                    .with_param("deleted", total_deleted)
                    .with_param("net", net),
                note: LocalizedMessage::code(note_code),
            });
        }
    }

    // -- Hotspot density (from hotspots) --
    if let Some(hotspots) = by_name.get("hotspots") {
        let hot_count = hotspots
            .entries
            .iter()
            .filter(|e| {
                matches!(
                    e.values.get("level"),
                    Some(MetricValue::Message(m)) if m.code == messages::HOTSPOT_LEVEL_FILE
                ) && matches!(
                    e.values.get("score"),
                    Some(MetricValue::Count(s)) if *s >= 100
                )
            })
            .count();
        if hot_count > 0 {
            out.push(Insight {
                key: "hotspots".into(),
                detail: LocalizedMessage::code(messages::HEALTH_INSIGHT_HOTSPOTS)
                    .with_param("count", hot_count as u64),
                note: LocalizedMessage::code(messages::HEALTH_INSIGHT_HOTSPOTS_NOTE),
            });
        }
    }

    out
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

fn build_result(
    overall: u64,
    pillars: Vec<Pillar>,
    hygiene: Vec<HygieneFinding>,
    insights: Vec<Insight>,
    supplementary: Vec<SupplementaryScore>,
) -> MetricResult {
    let columns = vec![
        Column::in_report("health", "score"),
        Column::labeled(
            "level",
            LocalizedMessage::code("report.health.column.level"),
        ),
        Column::in_report("health", "details"),
        Column::in_report("health", "action"),
    ];

    let overall_entry = MetricEntry {
        key: "overall".into(),
        values: values(
            Some(overall),
            MetricValue::Message(
                LocalizedMessage::code(messages::HEALTH_OVERALL_SCORE).with_param("score", overall),
            ),
            MetricValue::Message(interpretation(overall)),
        ),
    };

    let pillar_entries: Vec<MetricEntry> = pillars
        .iter()
        .map(|p| {
            let mut v = values(
                Some(p.score.round() as u64),
                MetricValue::Message(p.summary.clone()),
                MetricValue::Message(
                    LocalizedMessage::code(messages::HEALTH_PILLAR_SEE_ACTIONS)
                        .with_param("pillar", p.display),
                ),
            );
            v.insert(
                "level".into(),
                MetricValue::Message(level_for_score(p.score)),
            );
            MetricEntry {
                key: p.key.to_string(),
                values: v,
            }
        })
        .collect();

    // Gather all actions, take top MAX_ACTIONS in pillar-priority order.
    let mut all_actions: Vec<&Action> = pillars.iter().flat_map(|p| p.actions.iter()).collect();
    // Stable order: preserve pillar insertion order by using `enumerate` index.
    all_actions.truncate(MAX_ACTIONS);

    let action_entries: Vec<MetricEntry> = all_actions
        .into_iter()
        .map(|a| {
            let mut v = values(
                None,
                MetricValue::Message(a.detail.clone()),
                MetricValue::Message(a.command.clone()),
            );
            v.insert("level".into(), MetricValue::Message(a.level.clone()));
            MetricEntry {
                key: format!("{}: {}", a.pillar, a.target),
                values: v,
            }
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
                values: values(
                    None,
                    MetricValue::Message(h.detail),
                    MetricValue::Text(h.command),
                ),
            }
        })
        .collect();

    let mut entry_groups: Vec<EntryGroup> = Vec::new();
    entry_groups.push(EntryGroup {
        name: "overall".into(),
        label: messages::HEALTH_GROUP_OVERALL.into(),
        entries: vec![overall_entry],
    });
    entry_groups.push(EntryGroup {
        name: "pillars".into(),
        label: messages::HEALTH_GROUP_PILLARS.into(),
        entries: pillar_entries,
    });
    if !supplementary.is_empty() {
        let score_entries: Vec<MetricEntry> = supplementary
            .iter()
            .map(|s| {
                let mut v = values(
                    Some(s.score.round() as u64),
                    MetricValue::Message(s.summary.clone()),
                    MetricValue::Message(LocalizedMessage::code(s.display)),
                );
                v.insert(
                    "level".into(),
                    MetricValue::Message(level_for_score(s.score)),
                );
                MetricEntry {
                    key: s.key.to_string(),
                    values: v,
                }
            })
            .collect();
        entry_groups.push(EntryGroup {
            name: "scores".into(),
            label: messages::HEALTH_GROUP_SCORES.into(),
            entries: score_entries,
        });
    }
    if !action_entries.is_empty() {
        entry_groups.push(EntryGroup {
            name: "actions".into(),
            label: messages::HEALTH_GROUP_ACTIONS.into(),
            entries: action_entries,
        });
    }
    if !hygiene_entries.is_empty() {
        entry_groups.push(EntryGroup {
            name: "hygiene".into(),
            label: messages::HEALTH_GROUP_HYGIENE.into(),
            entries: hygiene_entries,
        });
    }

    if !insights.is_empty() {
        let insight_entries: Vec<MetricEntry> = insights
            .into_iter()
            .map(|i| {
                let mut v = values(
                    None,
                    MetricValue::Message(i.detail),
                    MetricValue::Message(i.note),
                );
                v.insert("level".into(), MetricValue::Message(info()));
                MetricEntry {
                    key: i.key,
                    values: v,
                }
            })
            .collect();
        entry_groups.push(EntryGroup {
            name: "insights".into(),
            label: messages::HEALTH_GROUP_INSIGHTS.into(),
            entries: insight_entries,
        });
    }

    MetricResult {
        name: "health".into(),
        display_name: report_display("health"),
        description: report_description("health"),
        columns,
        entries: vec![],
        entry_groups,
    }
}

fn values(
    score: Option<u64>,
    details: MetricValue,
    action: MetricValue,
) -> HashMap<String, MetricValue> {
    let mut m = HashMap::new();
    match score {
        Some(s) => m.insert("score".into(), MetricValue::Count(s)),
        None => m.insert("score".into(), MetricValue::Text("—".into())),
    };
    m.insert("details".into(), details);
    m.insert("action".into(), action);
    m
}

fn interpretation(score: u64) -> LocalizedMessage {
    let code = match score {
        90..=100 => messages::HEALTH_INTERPRETATION_EXCELLENT,
        80..=89 => messages::HEALTH_INTERPRETATION_GOOD,
        70..=79 => messages::HEALTH_INTERPRETATION_FAIR,
        60..=69 => messages::HEALTH_INTERPRETATION_CONCERNING,
        _ => messages::HEALTH_INTERPRETATION_POOR,
    };
    LocalizedMessage::code(code)
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
            display_name: report_display(name),
            description: report_description(name),
            columns: vec![],
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
        assert_eq!(
            interpretation(95).code,
            messages::HEALTH_INTERPRETATION_EXCELLENT
        );
        assert_eq!(
            interpretation(85).code,
            messages::HEALTH_INTERPRETATION_GOOD
        );
        assert_eq!(
            interpretation(75).code,
            messages::HEALTH_INTERPRETATION_FAIR
        );
        assert_eq!(
            interpretation(65).code,
            messages::HEALTH_INTERPRETATION_CONCERNING
        );
        assert_eq!(
            interpretation(40).code,
            messages::HEALTH_INTERPRETATION_POOR
        );
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
    fn refactoring_debt_penalises_rotten_todos() {
        let markers = mk_result(
            "debt_markers",
            vec![
                mk_entry("a.rs:10", &[("age_days", MetricValue::Count(50))]), // fresh, ignored
                mk_entry("a.rs:20", &[("age_days", MetricValue::Count(200))]), // stale
                mk_entry("a.rs:30", &[("age_days", MetricValue::Count(400))]), // rotten
                mk_entry("a.rs:40", &[("age_days", MetricValue::Count(500))]), // rotten
            ],
        );
        let mut by_name = HashMap::new();
        by_name.insert("debt_markers", &markers);
        let p = score_refactoring_debt(&by_name);
        assert!(
            p.score < 100.0,
            "stale + rotten markers should deduct points, got {}",
            p.score
        );
        // Summary params should reflect the split.
        assert_eq!(
            p.summary.params.get("stale"),
            Some(&serde_json::json!(1_u64))
        );
        assert_eq!(
            p.summary.params.get("rotten"),
            Some(&serde_json::json!(2_u64))
        );
        // An action for the oldest rotten marker.
        assert!(
            p.actions
                .iter()
                .any(|a| a.target.contains("a.rs:40") || a.target.contains("a.rs:30")),
            "should surface a rotten marker as action"
        );
    }

    #[test]
    fn refactoring_debt_penalises_large_source_files() {
        let large = mk_result(
            "large_sources",
            vec![
                mk_entry("huge.java", &[("code_lines", MetricValue::Count(8000))]), // god module
                mk_entry("big.java", &[("code_lines", MetricValue::Count(2500))]),  // sizeable
                mk_entry("small.java", &[("code_lines", MetricValue::Count(600))]), // ignored
            ],
        );
        let mut by_name = HashMap::new();
        by_name.insert("large_sources", &large);
        let p = score_refactoring_debt(&by_name);
        assert!(
            p.score < 100.0,
            "huge files should deduct points, got {}",
            p.score
        );
        assert_eq!(
            p.summary.params.get("huge"),
            Some(&serde_json::json!(1_u64))
        );
        assert_eq!(
            p.summary.params.get("sizeable"),
            Some(&serde_json::json!(1_u64))
        );
        // Should surface the biggest one as an action.
        assert!(
            p.actions.iter().any(|a| a.target.contains("huge.java")),
            "should surface huge.java as action"
        );
    }

    #[test]
    fn refactoring_debt_ignores_fresh_todos() {
        let markers = mk_result(
            "debt_markers",
            vec![
                mk_entry("a.rs:1", &[("age_days", MetricValue::Count(10))]),
                mk_entry("a.rs:2", &[("age_days", MetricValue::Count(100))]),
            ],
        );
        let mut by_name = HashMap::new();
        by_name.insert("debt_markers", &markers);
        let p = score_refactoring_debt(&by_name);
        assert_eq!(
            p.score as u64, 100,
            "fresh markers (<180d) should not penalise"
        );
    }

    #[test]
    fn tidiness_rewards_clean_bloat_report() {
        use crate::types::LocalizedMessage;
        let bloat = mk_result(
            "bloat",
            vec![mk_entry(
                "src/main.rs",
                &[(
                    "recommendation",
                    MetricValue::Message(LocalizedMessage::code(messages::BLOAT_RECOMMENDATION_OK)),
                )],
            )],
        );
        let mut by_name = HashMap::new();
        by_name.insert("bloat", &bloat);
        let p = score_tidiness(&by_name);
        assert_eq!(p.score as u64, 100);
    }

    #[test]
    fn tidiness_punishes_vendored_content() {
        use crate::types::LocalizedMessage;
        let bloat = mk_result(
            "bloat",
            vec![mk_entry(
                "node_modules/foo",
                &[(
                    "recommendation",
                    MetricValue::Message(LocalizedMessage::code(
                        messages::BLOAT_RECOMMENDATION_VENDORED_DEPS,
                    )),
                )],
            )],
        );
        let mut by_name = HashMap::new();
        by_name.insert("bloat", &bloat);
        let p = score_tidiness(&by_name);
        assert!(p.score < 100.0);
        assert!(!p.actions.is_empty());
        assert!(p.actions[0].command.code.contains("git rm"));
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
        let overall = result.entry_groups[0].entries[0]
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
