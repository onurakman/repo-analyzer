use clap::Parser;

use crate::types::{OutputFormat, ReportKind, TimeRange};

/// Git repository analysis tool with code-construct-level granularity.
#[derive(Parser, Debug)]
#[command(name = "repo-analyzer", version, about)]
pub struct Cli {
    /// Path to the git repository to analyze
    #[arg(default_value = ".")]
    pub path: String,

    /// Output format
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Table)]
    pub format: OutputFormat,

    /// Run only these reports (comma-separated: health,authors,hotspots,churn,ownership,coupling,patterns,age,bloat,outliers,quality,complexity,composition,construct_churn,debt_markers,large_sources,half_life,succession,knowledge_silos,fan_in_out,module_coupling,churn_pareto,construct_ownership)
    #[arg(long)]
    pub only: Option<String>,

    /// Analyze commits since this duration (e.g. "6m", "1y", "30d")
    #[arg(long)]
    pub since: Option<String>,

    /// Start date for analysis (YYYY-MM-DD). Requires --to.
    #[arg(long)]
    pub from: Option<String>,

    /// End date for analysis (YYYY-MM-DD). Requires --from.
    #[arg(long)]
    pub to: Option<String>,

    /// Show only the top N entries per report
    #[arg(long)]
    pub top: Option<usize>,

    /// Write output to file instead of stdout
    #[arg(short, long)]
    pub output: Option<String>,

    /// Suppress progress indicators
    #[arg(short, long)]
    pub quiet: bool,

    /// Number of threads for parallel processing (0 = auto)
    #[arg(long, default_value_t = 0)]
    pub threads: usize,

    /// In-flight channel capacity between commit producer, parse workers, and
    /// the SQLite writer. Lower it (1–2) to shrink peak RAM on small pods;
    /// raise it (8–32) on fast disks to reduce stalls. Default 4.
    #[arg(long, default_value_t = 4)]
    pub channel_capacity: usize,

    /// Max parsed changes per batch flushed into the channel. Smaller batches
    /// cut in-flight memory for huge merge commits; larger batches amortize
    /// transaction overhead on the SQLite writer. Default 64.
    #[arg(long, default_value_t = 64)]
    pub batch_size: usize,

    /// Per-thread `gix` object cache size in MiB. The cache speeds up blob
    /// reuse but grows with repo activity; drop to 1 on tight pods. Default 4.
    #[arg(long, default_value_t = 4)]
    pub object_cache_mb: usize,

    /// Automatically run `git fetch --unshallow` when the repository is a
    /// shallow clone. Without this flag, shallow repos either prompt the user
    /// (interactive) or abort (`--quiet`). Pair with `--quiet` in CI.
    #[arg(short = 'u', long)]
    pub unshallow: bool,

    /// Fast filesystem-only language composition. Skips git entirely and
    /// prints `[{"language", "percentage", ...}, ...]` JSON to stdout (or
    /// `--output` file). All other report flags are ignored when this is set.
    #[arg(long)]
    pub quick_composition: bool,
}

impl Cli {
    /// Parse the time range from CLI arguments.
    ///
    /// Returns an error if both `--since` and `--from/--to` are provided,
    /// or if `--from` is given without `--to` (or vice-versa).
    pub fn parse_time_range(&self) -> anyhow::Result<TimeRange> {
        match (&self.since, &self.from, &self.to) {
            (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
                anyhow::bail!("Cannot combine --since with --from/--to");
            }
            (Some(dur_str), None, None) => {
                let duration = parse_duration(dur_str)?;
                Ok(TimeRange::Since(duration))
            }
            (None, Some(from_str), Some(to_str)) => {
                let from = chrono::NaiveDate::parse_from_str(from_str, "%Y-%m-%d")
                    .map_err(|e| anyhow::anyhow!("Invalid --from date: {e}"))?;
                let to = chrono::NaiveDate::parse_from_str(to_str, "%Y-%m-%d")
                    .map_err(|e| anyhow::anyhow!("Invalid --to date: {e}"))?;
                if from > to {
                    anyhow::bail!("--from date must be before --to date");
                }
                Ok(TimeRange::Between { from, to })
            }
            (None, Some(_), None) => {
                anyhow::bail!("--from requires --to");
            }
            (None, None, Some(_)) => {
                anyhow::bail!("--to requires --from");
            }
            (None, None, None) => Ok(TimeRange::All),
        }
    }

    /// Parse the `--only` flag into a list of `ReportKind`.
    ///
    /// If `--only` is not provided, returns the default (non-heavy) report set.
    /// Heavy reports (e.g. `half_life`) must be requested explicitly via `--only`.
    pub fn parse_report_kinds(&self) -> anyhow::Result<Vec<ReportKind>> {
        match &self.only {
            None => Ok(ReportKind::default_set()),
            Some(s) => {
                let mut kinds = Vec::new();
                for part in s.split(',') {
                    let trimmed = part.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match ReportKind::parse(trimmed) {
                        Some(k) => kinds.push(k),
                        None => anyhow::bail!("Unknown report kind: '{trimmed}'"),
                    }
                }
                if kinds.is_empty() {
                    anyhow::bail!("--only requires at least one report kind");
                }
                Ok(kinds)
            }
        }
    }
}

/// Parse a human-friendly duration string into a `chrono::Duration`.
///
/// Supported suffixes: `d` (days), `w` (weeks), `m` (months ≈ 30 days), `y` (years ≈ 365 days).
pub fn parse_duration(s: &str) -> anyhow::Result<chrono::Duration> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("Empty duration string");
    }

    let (num_str, suffix) = s.split_at(s.len() - 1);
    let num: i64 = num_str
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid duration number: '{num_str}'"))?;

    if num <= 0 {
        anyhow::bail!("Duration must be positive, got {num}");
    }

    let days = match suffix {
        "d" => num,
        "w" => num * 7,
        "m" => num * 30,
        "y" => num * 365,
        _ => anyhow::bail!("Unknown duration suffix: '{suffix}'. Use d, w, m, or y."),
    };

    Ok(chrono::Duration::days(days))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration_days() {
        let d = parse_duration("30d").unwrap();
        assert_eq!(d.num_days(), 30);
    }

    #[test]
    fn test_parse_duration_weeks() {
        let d = parse_duration("2w").unwrap();
        assert_eq!(d.num_days(), 14);
    }

    #[test]
    fn test_parse_duration_months() {
        let d = parse_duration("6m").unwrap();
        assert_eq!(d.num_days(), 180);
    }

    #[test]
    fn test_parse_duration_years() {
        let d = parse_duration("1y").unwrap();
        assert_eq!(d.num_days(), 365);
    }

    #[test]
    fn test_parse_duration_invalid_suffix() {
        assert!(parse_duration("10x").is_err());
    }

    #[test]
    fn test_parse_report_kinds_single() {
        let cli = Cli {
            path: ".".to_string(),
            format: OutputFormat::Table,
            only: Some("hotspots".to_string()),
            since: None,
            from: None,
            to: None,
            top: None,
            output: None,
            quiet: false,
            threads: 0,
            channel_capacity: 4,
            batch_size: 64,
            object_cache_mb: 4,
            unshallow: false,
            quick_composition: false,
        };
        let kinds = cli.parse_report_kinds().unwrap();
        assert_eq!(kinds, vec![ReportKind::Hotspots]);
    }

    #[test]
    fn test_parse_report_kinds_multiple() {
        let cli = Cli {
            path: ".".to_string(),
            format: OutputFormat::Table,
            only: Some("authors,churn,age".to_string()),
            since: None,
            from: None,
            to: None,
            top: None,
            output: None,
            quiet: false,
            threads: 0,
            channel_capacity: 4,
            batch_size: 64,
            object_cache_mb: 4,
            unshallow: false,
            quick_composition: false,
        };
        let kinds = cli.parse_report_kinds().unwrap();
        assert_eq!(
            kinds,
            vec![ReportKind::Authors, ReportKind::Churn, ReportKind::Age]
        );
    }
}
