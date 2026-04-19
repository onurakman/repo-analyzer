//! Stable message codes for [`crate::types::LocalizedMessage`].
//!
//! Every human-readable string a collector might emit into a report row goes
//! through this catalog. Callers construct messages by passing one of these
//! constants to [`LocalizedMessage::code`](crate::types::LocalizedMessage::code)
//! and attaching params + severity as needed.
//!
//! Rules:
//! - Codes are stable, dotted, snake_case identifiers grouped by collector.
//! - Never rename a code in place — deprecate + add a new one instead.
//! - Codes are the public contract shared with translation catalogs.
//! - Keep the default-EN catalog in [`crate::output::default_catalog`]
//!   in lockstep when codes are added or removed.

// ---------------------------------------------------------------------------
// age
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// authors
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// bloat
// ---------------------------------------------------------------------------

pub const BLOAT_RECOMMENDATION_OK: &str = "bloat.recommendation.ok";
pub const BLOAT_RECOMMENDATION_LARGE_FILE: &str = "bloat.recommendation.large_file";
pub const BLOAT_RECOMMENDATION_VERY_LARGE_FILE: &str = "bloat.recommendation.very_large_file";
pub const BLOAT_RECOMMENDATION_MINIFIED_BUNDLE: &str = "bloat.recommendation.minified_bundle";
pub const BLOAT_RECOMMENDATION_VENDORED_DEPS: &str = "bloat.recommendation.vendored_deps";
pub const BLOAT_RECOMMENDATION_BUILD_OUTPUT: &str = "bloat.recommendation.build_output";
pub const BLOAT_RECOMMENDATION_RUST_BUILD_OUTPUT: &str = "bloat.recommendation.rust_build_output";
pub const BLOAT_RECOMMENDATION_OS_METADATA: &str = "bloat.recommendation.os_metadata";
pub const BLOAT_RECOMMENDATION_IDE_CONFIG: &str = "bloat.recommendation.ide_config";

// ---------------------------------------------------------------------------
// churn
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// churn_pareto
// ---------------------------------------------------------------------------

pub const CHURN_PARETO_SUMMARY_PCT: &str = "churn_pareto.summary.pct";
pub const CHURN_PARETO_SUMMARY_CUMULATIVE: &str = "churn_pareto.summary.cumulative";

// ---------------------------------------------------------------------------
// complexity
// ---------------------------------------------------------------------------

pub const COMPLEXITY_RECOMMENDATION_SIMPLE: &str = "complexity.recommendation.simple";
pub const COMPLEXITY_RECOMMENDATION_OK: &str = "complexity.recommendation.ok";
pub const COMPLEXITY_RECOMMENDATION_HIGH: &str = "complexity.recommendation.high";
pub const COMPLEXITY_RECOMMENDATION_VERY_HIGH: &str = "complexity.recommendation.very_high";

// ---------------------------------------------------------------------------
// composition
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// construct_churn
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// construct_ownership
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// coupling
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// debt_markers
// ---------------------------------------------------------------------------

pub const DEBT_MARKERS_RECOMMENDATION_AGE_UNKNOWN: &str = "debt_markers.recommendation.age_unknown";
pub const DEBT_MARKERS_RECOMMENDATION_FRESH: &str = "debt_markers.recommendation.fresh";
pub const DEBT_MARKERS_RECOMMENDATION_AGING: &str = "debt_markers.recommendation.aging";
pub const DEBT_MARKERS_RECOMMENDATION_STALE: &str = "debt_markers.recommendation.stale";
pub const DEBT_MARKERS_RECOMMENDATION_ROTTEN: &str = "debt_markers.recommendation.rotten";

// ---------------------------------------------------------------------------
// fan_in_out
// ---------------------------------------------------------------------------

pub const FAN_IN_OUT_ROLE_HUB: &str = "fan_in_out.role.hub";
pub const FAN_IN_OUT_ROLE_ORCHESTRATOR: &str = "fan_in_out.role.orchestrator";
pub const FAN_IN_OUT_ROLE_LEAF: &str = "fan_in_out.role.leaf";
pub const FAN_IN_OUT_ROLE_PURE_DEP: &str = "fan_in_out.role.pure_dep";
pub const FAN_IN_OUT_ROLE_MIXED: &str = "fan_in_out.role.mixed";

// ---------------------------------------------------------------------------
// half_life
// ---------------------------------------------------------------------------

pub const HALF_LIFE_RECOMMENDATION_HOT: &str = "half_life.recommendation.hot";
pub const HALF_LIFE_RECOMMENDATION_AGING: &str = "half_life.recommendation.aging";
pub const HALF_LIFE_RECOMMENDATION_STABLE: &str = "half_life.recommendation.stable";
pub const HALF_LIFE_RECOMMENDATION_CORE: &str = "half_life.recommendation.core";

// ---------------------------------------------------------------------------
// health
// ---------------------------------------------------------------------------

pub const HEALTH_ACTION_CRITICAL: &str = "health.action.critical";
pub const HEALTH_ACTION_WARNING: &str = "health.action.warning";
pub const HEALTH_ACTION_OK: &str = "health.action.ok";
pub const HEALTH_GROUP_OVERALL: &str = "health.group.overall";
pub const HEALTH_GROUP_PILLARS: &str = "health.group.pillars";
pub const HEALTH_GROUP_ACTIONS: &str = "health.group.actions";
pub const HEALTH_GROUP_HYGIENE: &str = "health.group.hygiene";
pub const HEALTH_OVERALL_SCORE: &str = "health.overall.score";
pub const HEALTH_INTERPRETATION_EXCELLENT: &str = "health.interpretation.excellent";
pub const HEALTH_INTERPRETATION_GOOD: &str = "health.interpretation.good";
pub const HEALTH_INTERPRETATION_FAIR: &str = "health.interpretation.fair";
pub const HEALTH_INTERPRETATION_CONCERNING: &str = "health.interpretation.concerning";
pub const HEALTH_INTERPRETATION_POOR: &str = "health.interpretation.poor";
pub const HEALTH_PILLAR_COMMIT_DISCIPLINE: &str = "health.pillar.commit_discipline";
pub const HEALTH_PILLAR_BUS_FACTOR: &str = "health.pillar.bus_factor";
pub const HEALTH_PILLAR_REFACTORING_DEBT: &str = "health.pillar.refactoring_debt";
pub const HEALTH_PILLAR_TIDINESS: &str = "health.pillar.tidiness";
pub const HEALTH_PILLAR_CHANGE_CONCENTRATION: &str = "health.pillar.change_concentration";
pub const HEALTH_PILLAR_SEE_ACTIONS: &str = "health.pillar.see_actions";
pub const HEALTH_SUMMARY_COMMIT_DISCIPLINE: &str = "health.summary.commit_discipline";
pub const HEALTH_SUMMARY_BUS_FACTOR: &str = "health.summary.bus_factor";
pub const HEALTH_SUMMARY_REFACTORING_DEBT: &str = "health.summary.refactoring_debt";
pub const HEALTH_SUMMARY_TIDINESS: &str = "health.summary.tidiness";
pub const HEALTH_SUMMARY_CHANGE_CONCENTRATION: &str = "health.summary.change_concentration";
pub const HEALTH_SUMMARY_NOT_ENOUGH_FILES: &str = "health.summary.not_enough_files";
pub const HEALTH_DETAIL_LOW_QUALITY: &str = "health.detail.low_quality";
pub const HEALTH_DETAIL_MEGA_COMMITS: &str = "health.detail.mega_commits";
pub const HEALTH_DETAIL_HIGH_REVERT: &str = "health.detail.high_revert";
pub const HEALTH_DETAIL_SOLE_OWNER: &str = "health.detail.sole_owner";
pub const HEALTH_DETAIL_HIGH_COMPLEXITY: &str = "health.detail.high_complexity";
pub const HEALTH_DETAIL_GOD_MODULE: &str = "health.detail.god_module";
pub const HEALTH_DETAIL_ROTTEN_MARKER: &str = "health.detail.rotten_marker";
pub const HEALTH_COMMAND_ENFORCE_CONVENTIONAL: &str = "health.command.enforce_conventional";
pub const HEALTH_COMMAND_SPLIT_COMMITS: &str = "health.command.split_commits";
pub const HEALTH_COMMAND_REQUIRE_REVIEW: &str = "health.command.require_review";
pub const HEALTH_COMMAND_PAIR_REVIEW: &str = "health.command.pair_review";
pub const HEALTH_COMMAND_EXTRACT_BRANCHES: &str = "health.command.extract_branches";
pub const HEALTH_COMMAND_SPLIT_MODULE: &str = "health.command.split_module";
pub const HEALTH_COMMAND_RESOLVE_MARKER: &str = "health.command.resolve_marker";
pub const HEALTH_HYGIENE_LARGE_GIT_DIR: &str = "health.hygiene.large_git_dir";
pub const HEALTH_HYGIENE_FRAGMENTED_PACKS: &str = "health.hygiene.fragmented_packs";
pub const HEALTH_HYGIENE_LOOSE_OBJECTS: &str = "health.hygiene.loose_objects";
pub const HEALTH_HYGIENE_BLOAT_FINDING: &str = "health.hygiene.bloat_finding";

// ---------------------------------------------------------------------------
// hotspots
// ---------------------------------------------------------------------------

pub const HOTSPOT_LEVEL_FILE: &str = "hotspot.level.file";
pub const HOTSPOT_LEVEL_CONSTRUCT: &str = "hotspot.level.construct";

// ---------------------------------------------------------------------------
// knowledge_silos
// ---------------------------------------------------------------------------

pub const KNOWLEDGE_SILO_RISK_AT_RISK: &str = "knowledge_silos.risk.at_risk";
pub const KNOWLEDGE_SILO_RISK_SINGLE_OWNER: &str = "knowledge_silos.risk.single_owner";

// ---------------------------------------------------------------------------
// large_sources
// ---------------------------------------------------------------------------

pub const LARGE_SOURCES_RECOMMENDATION_SIZEABLE: &str = "large_sources.recommendation.sizeable";
pub const LARGE_SOURCES_RECOMMENDATION_VERY_LARGE: &str = "large_sources.recommendation.very_large";
pub const LARGE_SOURCES_RECOMMENDATION_ENORMOUS: &str = "large_sources.recommendation.enormous";

// ---------------------------------------------------------------------------
// module_coupling
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// outliers
// ---------------------------------------------------------------------------

pub const OUTLIERS_RECOMMENDATION_GOD_FILE: &str = "outliers.recommendation.god_file";
pub const OUTLIERS_RECOMMENDATION_HIGH_CHURN: &str = "outliers.recommendation.high_churn";
pub const OUTLIERS_RECOMMENDATION_DIFFUSE_OWNERSHIP: &str =
    "outliers.recommendation.diffuse_ownership";
pub const OUTLIERS_RECOMMENDATION_OK: &str = "outliers.recommendation.ok";

// ---------------------------------------------------------------------------
// ownership
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// patterns
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// quality
// ---------------------------------------------------------------------------

pub const QUALITY_RECOMMENDATION_OK: &str = "quality.recommendation.ok";
pub const QUALITY_RECOMMENDATION_BASELINE: &str = "quality.recommendation.baseline";
pub const QUALITY_RECOMMENDATION_ENFORCE_MSG_LENGTH: &str =
    "quality.recommendation.enforce_msg_length";
pub const QUALITY_RECOMMENDATION_SQUASH_WIP: &str = "quality.recommendation.squash_wip";
pub const QUALITY_RECOMMENDATION_SPLIT_MEGA: &str = "quality.recommendation.split_mega";
pub const QUALITY_RECOMMENDATION_STRENGTHEN_REVIEW: &str =
    "quality.recommendation.strengthen_review";
pub const QUALITY_RECOMMENDATION_REBASE_WORKFLOW: &str = "quality.recommendation.rebase_workflow";
pub const QUALITY_RECOMMENDATION_REQUIRE_DESCRIPTIONS: &str =
    "quality.recommendation.require_descriptions";

// ---------------------------------------------------------------------------
// succession
// ---------------------------------------------------------------------------

pub const SUCCESSION_STATUS_HEALTHY: &str = "succession.status.healthy";
pub const SUCCESSION_STATUS_OWNED: &str = "succession.status.owned";
pub const SUCCESSION_STATUS_ORPHANED: &str = "succession.status.orphaned";
pub const SUCCESSION_STATUS_KNOWLEDGE_TRANSFER_NEEDED: &str =
    "succession.status.knowledge_transfer_needed";
pub const SUCCESSION_STATUS_HANDED_OFF: &str = "succession.status.handed_off";

#[cfg(test)]
mod tests {
    use super::*;

    /// All codes must be dotted snake_case and start with a collector namespace.
    #[test]
    fn codes_are_well_formed() {
        for code in all_codes() {
            assert!(code.contains('.'), "code {code:?} missing namespace");
            assert!(
                code.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c == '.'),
                "code {code:?} not snake_case dotted",
            );
        }
    }

    /// Central registry used by the well-formedness test above; also serves as
    /// a quick audit trail when codes are added.
    fn all_codes() -> Vec<&'static str> {
        vec![
            BLOAT_RECOMMENDATION_OK,
            BLOAT_RECOMMENDATION_LARGE_FILE,
            BLOAT_RECOMMENDATION_VERY_LARGE_FILE,
            BLOAT_RECOMMENDATION_MINIFIED_BUNDLE,
            BLOAT_RECOMMENDATION_VENDORED_DEPS,
            BLOAT_RECOMMENDATION_BUILD_OUTPUT,
            BLOAT_RECOMMENDATION_RUST_BUILD_OUTPUT,
            BLOAT_RECOMMENDATION_OS_METADATA,
            BLOAT_RECOMMENDATION_IDE_CONFIG,
            CHURN_PARETO_SUMMARY_PCT,
            CHURN_PARETO_SUMMARY_CUMULATIVE,
            COMPLEXITY_RECOMMENDATION_SIMPLE,
            COMPLEXITY_RECOMMENDATION_OK,
            COMPLEXITY_RECOMMENDATION_HIGH,
            COMPLEXITY_RECOMMENDATION_VERY_HIGH,
            DEBT_MARKERS_RECOMMENDATION_AGE_UNKNOWN,
            DEBT_MARKERS_RECOMMENDATION_FRESH,
            DEBT_MARKERS_RECOMMENDATION_AGING,
            DEBT_MARKERS_RECOMMENDATION_STALE,
            DEBT_MARKERS_RECOMMENDATION_ROTTEN,
            FAN_IN_OUT_ROLE_HUB,
            FAN_IN_OUT_ROLE_ORCHESTRATOR,
            FAN_IN_OUT_ROLE_LEAF,
            FAN_IN_OUT_ROLE_PURE_DEP,
            FAN_IN_OUT_ROLE_MIXED,
            HALF_LIFE_RECOMMENDATION_HOT,
            HALF_LIFE_RECOMMENDATION_AGING,
            HALF_LIFE_RECOMMENDATION_STABLE,
            HALF_LIFE_RECOMMENDATION_CORE,
            HEALTH_ACTION_CRITICAL,
            HEALTH_ACTION_WARNING,
            HEALTH_ACTION_OK,
            HEALTH_GROUP_OVERALL,
            HEALTH_GROUP_PILLARS,
            HEALTH_GROUP_ACTIONS,
            HEALTH_GROUP_HYGIENE,
            HEALTH_OVERALL_SCORE,
            HEALTH_INTERPRETATION_EXCELLENT,
            HEALTH_INTERPRETATION_GOOD,
            HEALTH_INTERPRETATION_FAIR,
            HEALTH_INTERPRETATION_CONCERNING,
            HEALTH_INTERPRETATION_POOR,
            HEALTH_PILLAR_COMMIT_DISCIPLINE,
            HEALTH_PILLAR_BUS_FACTOR,
            HEALTH_PILLAR_REFACTORING_DEBT,
            HEALTH_PILLAR_TIDINESS,
            HEALTH_PILLAR_CHANGE_CONCENTRATION,
            HEALTH_PILLAR_SEE_ACTIONS,
            HEALTH_SUMMARY_COMMIT_DISCIPLINE,
            HEALTH_SUMMARY_BUS_FACTOR,
            HEALTH_SUMMARY_REFACTORING_DEBT,
            HEALTH_SUMMARY_TIDINESS,
            HEALTH_SUMMARY_CHANGE_CONCENTRATION,
            HEALTH_SUMMARY_NOT_ENOUGH_FILES,
            HEALTH_DETAIL_LOW_QUALITY,
            HEALTH_DETAIL_MEGA_COMMITS,
            HEALTH_DETAIL_HIGH_REVERT,
            HEALTH_DETAIL_SOLE_OWNER,
            HEALTH_DETAIL_HIGH_COMPLEXITY,
            HEALTH_DETAIL_GOD_MODULE,
            HEALTH_DETAIL_ROTTEN_MARKER,
            HEALTH_COMMAND_ENFORCE_CONVENTIONAL,
            HEALTH_COMMAND_SPLIT_COMMITS,
            HEALTH_COMMAND_REQUIRE_REVIEW,
            HEALTH_COMMAND_PAIR_REVIEW,
            HEALTH_COMMAND_EXTRACT_BRANCHES,
            HEALTH_COMMAND_SPLIT_MODULE,
            HEALTH_COMMAND_RESOLVE_MARKER,
            HEALTH_HYGIENE_LARGE_GIT_DIR,
            HEALTH_HYGIENE_FRAGMENTED_PACKS,
            HEALTH_HYGIENE_LOOSE_OBJECTS,
            HEALTH_HYGIENE_BLOAT_FINDING,
            HOTSPOT_LEVEL_FILE,
            HOTSPOT_LEVEL_CONSTRUCT,
            KNOWLEDGE_SILO_RISK_AT_RISK,
            KNOWLEDGE_SILO_RISK_SINGLE_OWNER,
            LARGE_SOURCES_RECOMMENDATION_SIZEABLE,
            LARGE_SOURCES_RECOMMENDATION_VERY_LARGE,
            LARGE_SOURCES_RECOMMENDATION_ENORMOUS,
            OUTLIERS_RECOMMENDATION_GOD_FILE,
            OUTLIERS_RECOMMENDATION_HIGH_CHURN,
            OUTLIERS_RECOMMENDATION_DIFFUSE_OWNERSHIP,
            OUTLIERS_RECOMMENDATION_OK,
            QUALITY_RECOMMENDATION_OK,
            QUALITY_RECOMMENDATION_BASELINE,
            QUALITY_RECOMMENDATION_ENFORCE_MSG_LENGTH,
            QUALITY_RECOMMENDATION_SQUASH_WIP,
            QUALITY_RECOMMENDATION_SPLIT_MEGA,
            QUALITY_RECOMMENDATION_STRENGTHEN_REVIEW,
            QUALITY_RECOMMENDATION_REBASE_WORKFLOW,
            QUALITY_RECOMMENDATION_REQUIRE_DESCRIPTIONS,
            SUCCESSION_STATUS_HEALTHY,
            SUCCESSION_STATUS_OWNED,
            SUCCESSION_STATUS_ORPHANED,
            SUCCESSION_STATUS_KNOWLEDGE_TRANSFER_NEEDED,
            SUCCESSION_STATUS_HANDED_OFF,
        ]
    }
}
