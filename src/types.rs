use chrono::{DateTime, FixedOffset, NaiveDate};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Git-level types
// ---------------------------------------------------------------------------

/// Information about a single commit.
///
/// `author` and `email` use `Arc<str>` because they repeat heavily across commits
/// — interning saves substantial memory in large repos.
#[derive(Debug, Clone, Serialize)]
pub struct CommitInfo {
    pub oid: String,
    pub author: Arc<str>,
    pub email: Arc<str>,
    pub timestamp: DateTime<FixedOffset>,
    pub message: String,
    pub parent_ids: Vec<String>,
}

/// Diff-level status of a file within a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

/// A contiguous block of changed lines inside one file.
#[derive(Debug, Clone, Serialize)]
pub struct Hunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
}

/// One file's diff within a commit.
///
/// `commit` is wrapped in `Arc` so all DiffRecords from the same commit share
/// one CommitInfo allocation (especially the long commit message string).
/// `file_path` and `old_path` use `Arc<str>` because the same paths recur
/// across many commits and are stored in many collectors at once.
#[derive(Debug, Clone, Serialize)]
pub struct DiffRecord {
    pub commit: Arc<CommitInfo>,
    pub file_path: Arc<str>,
    pub old_path: Option<Arc<str>>,
    pub status: FileStatus,
    pub hunks: Vec<Hunk>,
    pub additions: u32,
    pub deletions: u32,
}

// ---------------------------------------------------------------------------
// Parser-level types
// ---------------------------------------------------------------------------

/// A code construct identified by the parser inside a changed region.
#[derive(Debug, Clone, Serialize)]
pub enum CodeConstruct {
    Function {
        name: String,
        start_line: u32,
        end_line: u32,
        enclosing: Option<String>,
    },
    Method {
        name: String,
        start_line: u32,
        end_line: u32,
        enclosing: Option<String>,
    },
    Class {
        name: String,
        start_line: u32,
        end_line: u32,
    },
    Struct {
        name: String,
        start_line: u32,
        end_line: u32,
    },
    Enum {
        name: String,
        start_line: u32,
        end_line: u32,
    },
    Trait {
        name: String,
        start_line: u32,
        end_line: u32,
    },
    Interface {
        name: String,
        start_line: u32,
        end_line: u32,
    },
    Impl {
        name: String,
        start_line: u32,
        end_line: u32,
    },
    Closure {
        start_line: u32,
        end_line: u32,
        enclosing: Option<String>,
    },
    Module {
        name: String,
        start_line: u32,
        end_line: u32,
    },
    #[allow(dead_code)]
    Block {
        label: String,
        start_line: u32,
        end_line: u32,
    },
}

impl CodeConstruct {
    /// Returns the human-readable name of the construct.
    pub fn name(&self) -> &str {
        match self {
            Self::Function { name, .. }
            | Self::Method { name, .. }
            | Self::Class { name, .. }
            | Self::Struct { name, .. }
            | Self::Enum { name, .. }
            | Self::Trait { name, .. }
            | Self::Interface { name, .. }
            | Self::Impl { name, .. }
            | Self::Module { name, .. }
            | Self::Block { label: name, .. } => name,
            Self::Closure { start_line, .. } => {
                // Closures don't have names; callers should use qualified_name().
                let _ = start_line;
                "<closure>"
            }
        }
    }

    /// Returns the (start, end) line range (1-based, inclusive).
    pub fn line_range(&self) -> (u32, u32) {
        match self {
            Self::Function {
                start_line,
                end_line,
                ..
            }
            | Self::Method {
                start_line,
                end_line,
                ..
            }
            | Self::Class {
                start_line,
                end_line,
                ..
            }
            | Self::Struct {
                start_line,
                end_line,
                ..
            }
            | Self::Enum {
                start_line,
                end_line,
                ..
            }
            | Self::Trait {
                start_line,
                end_line,
                ..
            }
            | Self::Interface {
                start_line,
                end_line,
                ..
            }
            | Self::Impl {
                start_line,
                end_line,
                ..
            }
            | Self::Closure {
                start_line,
                end_line,
                ..
            }
            | Self::Module {
                start_line,
                end_line,
                ..
            }
            | Self::Block {
                start_line,
                end_line,
                ..
            } => (*start_line, *end_line),
        }
    }

    /// Returns a qualified name like `MyStruct::my_method` or `<closure@42>`.
    pub fn qualified_name(&self) -> String {
        match self {
            Self::Method {
                name,
                enclosing: Some(enc),
                ..
            }
            | Self::Function {
                name,
                enclosing: Some(enc),
                ..
            } => {
                format!("{enc}::{name}")
            }
            Self::Closure {
                start_line,
                enclosing: Some(enc),
                ..
            } => {
                format!("{enc}::<closure@{start_line}>")
            }
            Self::Closure {
                start_line,
                enclosing: None,
                ..
            } => {
                format!("<closure@{start_line}>")
            }
            _ => self.name().to_string(),
        }
    }

    /// Returns a short string identifying the kind of construct.
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Function { .. } => "function",
            Self::Method { .. } => "method",
            Self::Class { .. } => "class",
            Self::Struct { .. } => "struct",
            Self::Enum { .. } => "enum",
            Self::Trait { .. } => "trait",
            Self::Interface { .. } => "interface",
            Self::Impl { .. } => "impl",
            Self::Closure { .. } => "closure",
            Self::Module { .. } => "module",
            Self::Block { .. } => "block",
        }
    }
}

/// A diff record enriched with parsed code constructs.
///
/// `diff` is wrapped in `Arc` so all per-file `ParsedChange` instances within a
/// single commit share the same underlying `DiffRecord` allocation (including its
/// big commit message). For 32k commits × ~5 files this avoids hundreds of MB of
/// duplicated commit-info clones.
#[derive(Debug, Clone, Serialize)]
pub struct ParsedChange {
    pub diff: Arc<DiffRecord>,
    pub constructs: Vec<CodeConstruct>,
}

// ---------------------------------------------------------------------------
// Metric types
// ---------------------------------------------------------------------------

/// A single metric value — polymorphic.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum MetricValue {
    Count(u64),
    SignedCount(i64),
    Float(f64),
    Text(String),
    Date(NaiveDate),
    Message(LocalizedMessage),
    #[allow(dead_code)]
    List(Vec<MetricValue>),
}

impl fmt::Display for MetricValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Count(n) => write!(f, "{n}"),
            Self::SignedCount(n) => write!(f, "{n}"),
            Self::Float(v) => write!(f, "{v:.2}"),
            Self::Text(s) => write!(f, "{s}"),
            Self::Date(d) => write!(f, "{d}"),
            // Default CLI rendering is the stable code — writers that want
            // localized text use their own catalog lookup before formatting.
            Self::Message(m) => write!(f, "{}", m.code),
            Self::List(items) => {
                let parts: Vec<String> = items.iter().map(|i| i.to_string()).collect();
                write!(f, "{}", parts.join(", "))
            }
        }
    }
}

/// One row in a metric report.
#[derive(Debug, Clone, Serialize)]
pub struct MetricEntry {
    pub key: String,
    pub values: HashMap<String, MetricValue>,
}

/// A named group of entries (e.g., "hourly" / "daily" in patterns,
/// "pillars" / "actions" in health). `name` is the stable snake_case id;
/// `label` is the human-friendly heading shown in reports.
#[derive(Debug, Clone, Serialize)]
pub struct EntryGroup {
    pub name: String,
    pub label: String,
    pub entries: Vec<MetricEntry>,
}

/// Severity level attached to a [`LocalizedMessage`]. `None` severity means the
/// message is plain informational data with no badge in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
    Critical,
}

/// A structured, localizable message.
///
/// `code` is a stable snake_case dotted identifier (e.g.
/// `hotspot.recommendation.high_churn`) consumers translate via their own
/// catalog. `params` holds substitution values referenced by the translation.
/// `severity` drives UI badges; `None` means no badge.
#[derive(Debug, Clone)]
pub struct LocalizedMessage {
    pub code: String,
    pub severity: Option<Severity>,
    pub params: BTreeMap<String, serde_json::Value>,
}

impl Serialize for LocalizedMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        // Count fields: type + code + optional severity + optional params
        let len = 2 + usize::from(self.severity.is_some()) + usize::from(!self.params.is_empty());
        let mut map = serializer.serialize_map(Some(len))?;
        map.serialize_entry("type", "i18n")?;
        map.serialize_entry("code", &self.code)?;
        if let Some(ref s) = self.severity {
            map.serialize_entry("severity", s)?;
        }
        if !self.params.is_empty() {
            map.serialize_entry("params", &self.params)?;
        }
        map.end()
    }
}

impl LocalizedMessage {
    /// Bare message with no params and no severity — plain localized text.
    pub fn code(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            severity: None,
            params: BTreeMap::new(),
        }
    }

    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = Some(severity);
        self
    }

    pub fn with_param(
        mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> Self {
        self.params.insert(key.into(), value.into());
        self
    }
}

/// A table column descriptor. `value` is the snake_case key used to look up
/// `MetricEntry.values`; `label` is a [`LocalizedMessage`] that points at a
/// catalog key like `report.bloat.column.size_bytes`.
#[derive(Debug, Clone, Serialize)]
pub struct Column {
    pub value: String,
    pub label: LocalizedMessage,
}

impl Column {
    /// Column whose label code is derived from the owning report + value.
    /// The resulting code is `report.{report}.column.{value}`.
    pub fn in_report(report: &str, value: impl Into<String>) -> Self {
        let value = value.into();
        let code = format!("report.{report}.column.{value}");
        Self {
            value,
            label: LocalizedMessage::code(code),
        }
    }

    /// Column with an explicit label message.
    pub fn labeled(value: impl Into<String>, label: LocalizedMessage) -> Self {
        Self {
            value: value.into(),
            label,
        }
    }
}

/// Localized message code for a report's display name. Consumers translate
/// `report.{name}.display_name` via the catalog.
pub fn report_display(name: &str) -> LocalizedMessage {
    LocalizedMessage::code(format!("report.{name}.display_name"))
}

/// Localized message code for a report's description.
pub fn report_description(name: &str) -> LocalizedMessage {
    LocalizedMessage::code(format!("report.{name}.description"))
}

/// The result of one metric collector's run.
///
/// When `entry_groups` is non-empty, entries are organized into named groups
/// (e.g., "hourly" and "daily" for patterns). Output writers should render
/// each group separately. The flat `entries` field is ignored in this case.
///
/// `name` is the snake_case identifier used in code and CLI flags;
/// `display_name` and `description` are [`LocalizedMessage`]s whose codes
/// follow the `report.{name}.display_name` / `report.{name}.description`
/// convention. Writers that translate consult a [`crate::i18n::Catalog`].
#[derive(Debug, Clone, Serialize)]
pub struct MetricResult {
    pub name: String,
    pub display_name: LocalizedMessage,
    pub description: LocalizedMessage,
    pub columns: Vec<Column>,
    pub entries: Vec<MetricEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub entry_groups: Vec<EntryGroup>,
}

/// Convert a snake_case identifier into a human-friendly Title Case label.
/// "lines_added" → "Lines Added", "fan_in" → "Fan In".
pub fn humanize(s: &str) -> String {
    s.split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// Report / output types
// ---------------------------------------------------------------------------

/// Which reports (metric collectors) to run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub enum ReportKind {
    Health,
    Authors,
    Hotspots,
    Churn,
    Ownership,
    Coupling,
    Patterns,
    Age,
    Bloat,
    Outliers,
    Quality,
    Complexity,
    Composition,
    ConstructChurn,
    DebtMarkers,
    LargeSources,
    HalfLife,
    Succession,
    KnowledgeSilos,
    FanInOut,
    ModuleCoupling,
    ChurnPareto,
    ConstructOwnership,
    CommitVelocity,
    CommitSize,
    DocCoverage,
    DeadCode,
    Clones,
    TestRatio,
}

impl ReportKind {
    /// Returns `true` for reports that are memory-hungry on large repos and
    /// should be opt-in only. Currently: `half_life` (gix blame on long
    /// histories can allocate multi-GB per file).
    pub fn is_heavy(&self) -> bool {
        matches!(self, Self::HalfLife)
    }

    /// Default report set for the CLI when `--only` is not specified.
    /// Excludes [`is_heavy`](Self::is_heavy) reports — users must request them
    /// explicitly via `--only half_life,...`.
    pub fn default_set() -> Vec<ReportKind> {
        Self::all().into_iter().filter(|k| !k.is_heavy()).collect()
    }

    /// All known report kinds.
    pub fn all() -> Vec<ReportKind> {
        vec![
            // Health score first — a one-glance overview with actionable
            // items, synthesised from every other report.
            Self::Health,
            // Language composition second — a snapshot of what the repo is
            // made of is the natural opening frame before any history-driven
            // metric.
            Self::Composition,
            Self::Authors,
            Self::Hotspots,
            Self::Churn,
            Self::Ownership,
            Self::Coupling,
            Self::Patterns,
            Self::Age,
            Self::Bloat,
            Self::Outliers,
            Self::Quality,
            Self::Complexity,
            Self::ConstructChurn,
            Self::DebtMarkers,
            Self::LargeSources,
            Self::HalfLife,
            Self::Succession,
            Self::KnowledgeSilos,
            Self::FanInOut,
            Self::ModuleCoupling,
            Self::ChurnPareto,
            Self::ConstructOwnership,
            Self::CommitVelocity,
            Self::CommitSize,
            Self::DocCoverage,
            Self::DeadCode,
            Self::Clones,
            Self::TestRatio,
        ]
    }

    /// Parse from a string (case-insensitive).
    pub fn parse(s: &str) -> Option<ReportKind> {
        match s.to_lowercase().as_str() {
            "authors" => Some(Self::Authors),
            "hotspots" => Some(Self::Hotspots),
            "churn" => Some(Self::Churn),
            "ownership" => Some(Self::Ownership),
            "coupling" => Some(Self::Coupling),
            "patterns" => Some(Self::Patterns),
            "age" => Some(Self::Age),
            "bloat" => Some(Self::Bloat),
            "outliers" => Some(Self::Outliers),
            "quality" => Some(Self::Quality),
            "complexity" => Some(Self::Complexity),
            "health" => Some(Self::Health),
            "composition" => Some(Self::Composition),
            "construct_churn" | "construct-churn" => Some(Self::ConstructChurn),
            "debt_markers" | "debt-markers" | "todo" | "todos" => Some(Self::DebtMarkers),
            "large_sources" | "large-sources" | "big-files" => Some(Self::LargeSources),
            "half_life" | "half-life" | "halflife" => Some(Self::HalfLife),
            "succession" => Some(Self::Succession),
            "knowledge_silos" | "knowledge-silos" | "silos" => Some(Self::KnowledgeSilos),
            "fan_in_out" | "fan-in-out" | "fanio" | "fan" => Some(Self::FanInOut),
            "module_coupling" | "module-coupling" => Some(Self::ModuleCoupling),
            "churn_pareto" | "churn-pareto" | "pareto" => Some(Self::ChurnPareto),
            "construct_ownership" | "construct-ownership" => Some(Self::ConstructOwnership),
            "commit_velocity" | "commit-velocity" | "velocity" => Some(Self::CommitVelocity),
            "commit_size" | "commit-size" | "commit_anomalies" | "commit-anomalies" => {
                Some(Self::CommitSize)
            }
            "doc_coverage" | "doc-coverage" | "docs" | "documentation" => Some(Self::DocCoverage),
            "dead_code" | "dead-code" | "orphans" | "unused" => Some(Self::DeadCode),
            "clones" | "duplicates" | "duplication" => Some(Self::Clones),
            "test_ratio" | "test-ratio" | "tests" => Some(Self::TestRatio),
            _ => None,
        }
    }
}

impl fmt::Display for ReportKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Authors => "authors",
            Self::Hotspots => "hotspots",
            Self::Churn => "churn",
            Self::Ownership => "ownership",
            Self::Coupling => "coupling",
            Self::Patterns => "patterns",
            Self::Age => "age",
            Self::Bloat => "bloat",
            Self::Outliers => "outliers",
            Self::Quality => "quality",
            Self::Complexity => "complexity",
            Self::Health => "health",
            Self::Composition => "composition",
            Self::ConstructChurn => "construct_churn",
            Self::DebtMarkers => "debt_markers",
            Self::LargeSources => "large_sources",
            Self::HalfLife => "half_life",
            Self::Succession => "succession",
            Self::KnowledgeSilos => "knowledge_silos",
            Self::FanInOut => "fan_in_out",
            Self::ModuleCoupling => "module_coupling",
            Self::ChurnPareto => "churn_pareto",
            Self::ConstructOwnership => "construct_ownership",
            Self::CommitVelocity => "commit_velocity",
            Self::CommitSize => "commit_size",
            Self::DocCoverage => "doc_coverage",
            Self::DeadCode => "dead_code",
            Self::Clones => "clones",
            Self::TestRatio => "test_ratio",
        };
        write!(f, "{s}")
    }
}

/// Time range for filtering commits.
#[derive(Debug, Clone)]
pub enum TimeRange {
    All,
    Since(chrono::Duration),
    Between { from: NaiveDate, to: NaiveDate },
}

/// Output format for reports.
#[derive(Debug, Clone, Default, clap::ValueEnum, Serialize)]
pub enum OutputFormat {
    #[default]
    Table,
    Json,
    Csv,
    Html,
}

/// Configuration for output.
#[derive(Debug, Clone)]
pub struct OutputConfig {
    #[allow(dead_code)]
    pub format: OutputFormat,
    pub output_path: Option<String>,
    pub top: Option<usize>,
    #[allow(dead_code)]
    pub quiet: bool,
    /// Short locale code (`en`, `tr`, ...) used by terminal/CSV/HTML writers.
    /// JSON writer ignores this field and always emits raw message codes.
    pub locale: String,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            format: OutputFormat::default(),
            output_path: None,
            top: None,
            quiet: false,
            locale: "en".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant in `ReportKind::all()` must round-trip through Display + parse.
    /// This catches new variants that forgot to add a `parse()` arm or `Display` arm.
    #[test]
    fn report_kind_round_trip_all_variants() {
        for kind in ReportKind::all() {
            let s = kind.to_string();
            let parsed = ReportKind::parse(&s)
                .unwrap_or_else(|| panic!("ReportKind::parse({s:?}) returned None"));
            assert_eq!(parsed, kind, "round-trip mismatch for {kind:?}");
        }
    }

    #[test]
    fn report_kind_parse_is_case_insensitive() {
        assert_eq!(ReportKind::parse("AUTHORS"), Some(ReportKind::Authors));
        assert_eq!(
            ReportKind::parse("Construct_Churn"),
            Some(ReportKind::ConstructChurn)
        );
        assert_eq!(
            ReportKind::parse("Knowledge-Silos"),
            Some(ReportKind::KnowledgeSilos)
        );
    }

    #[test]
    fn report_kind_parse_unknown_returns_none() {
        assert!(ReportKind::parse("not_a_real_metric").is_none());
        assert!(ReportKind::parse("").is_none());
    }
}
