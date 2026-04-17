use chrono::{DateTime, FixedOffset, NaiveDate};
use serde::Serialize;
use std::collections::HashMap;
use std::fmt;

// ---------------------------------------------------------------------------
// Git-level types
// ---------------------------------------------------------------------------

/// Information about a single commit.
#[derive(Debug, Clone, Serialize)]
pub struct CommitInfo {
    pub oid: String,
    pub author: String,
    pub email: String,
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
    pub content: String,
}

/// One file's diff within a commit.
#[derive(Debug, Clone, Serialize)]
pub struct DiffRecord {
    pub commit: CommitInfo,
    pub file_path: String,
    pub old_path: Option<String>,
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
#[derive(Debug, Clone, Serialize)]
pub struct ParsedChange {
    pub diff: DiffRecord,
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

/// The result of one metric collector's run.
///
/// When `entry_groups` is non-empty, entries are organized into named groups
/// (e.g., "hourly" and "daily" for patterns). Output writers should render
/// each group separately. The flat `entries` field is ignored in this case.
#[derive(Debug, Clone, Serialize)]
pub struct MetricResult {
    pub name: String,
    pub description: String,
    pub columns: Vec<String>,
    pub entries: Vec<MetricEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub entry_groups: Vec<(String, Vec<MetricEntry>)>,
}

// ---------------------------------------------------------------------------
// Report / output types
// ---------------------------------------------------------------------------

/// Which reports (metric collectors) to run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub enum ReportKind {
    Authors,
    Hotspots,
    Churn,
    Ownership,
    Coupling,
    Patterns,
    Age,
    Branches,
    Bloat,
    Outliers,
    Quality,
}

impl ReportKind {
    /// All known report kinds.
    pub fn all() -> Vec<ReportKind> {
        vec![
            Self::Authors,
            Self::Hotspots,
            Self::Churn,
            Self::Ownership,
            Self::Coupling,
            Self::Patterns,
            Self::Age,
            Self::Branches,
            Self::Bloat,
            Self::Outliers,
            Self::Quality,
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
            "branches" => Some(Self::Branches),
            "bloat" => Some(Self::Bloat),
            "outliers" => Some(Self::Outliers),
            "quality" => Some(Self::Quality),
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
            Self::Branches => "branches",
            Self::Bloat => "bloat",
            Self::Outliers => "outliers",
            Self::Quality => "quality",
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
}
