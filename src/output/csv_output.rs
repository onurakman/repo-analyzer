use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use crate::output::ReportWriter;
use crate::types::{MetricResult, MetricValue, OutputConfig};

pub struct CsvWriter;

impl CsvWriter {
    fn format_value(value: &MetricValue) -> String {
        match value {
            MetricValue::Count(n) => n.to_string(),
            MetricValue::SignedCount(n) => n.to_string(),
            MetricValue::Float(v) => format!("{v:.2}"),
            MetricValue::Text(s) => s.clone(),
            MetricValue::Date(d) => d.to_string(),
            MetricValue::List(items) => {
                let parts: Vec<String> = items.iter().map(|i| i.to_string()).collect();
                format!("[{}]", parts.join(", "))
            }
        }
    }

    fn get_columns(result: &MetricResult) -> Vec<String> {
        if !result.columns.is_empty() {
            return result.columns.clone();
        }
        if let Some(first) = result.entries.first() {
            let mut cols: Vec<String> = first.values.keys().cloned().collect();
            cols.sort();
            cols
        } else {
            vec![]
        }
    }

    fn write_result_to_writer<W: io::Write>(
        result: &MetricResult,
        writer: W,
    ) -> anyhow::Result<()> {
        let columns = Self::get_columns(result);
        let mut csv_writer = csv::Writer::from_writer(writer);

        // Header: "name" + column names
        let mut header = vec!["name".to_string()];
        header.extend(columns.iter().cloned());
        csv_writer.write_record(&header)?;

        // Data rows
        for entry in &result.entries {
            let mut row = vec![entry.key.clone()];
            for col in &columns {
                let val = entry
                    .values
                    .get(col)
                    .map(Self::format_value)
                    .unwrap_or_default();
                row.push(val);
            }
            csv_writer.write_record(&row)?;
        }

        csv_writer.flush()?;
        Ok(())
    }

    /// Compute the output path for a specific report when there are multiple reports.
    /// E.g., base = "report.csv", name = "authors" -> "report_authors.csv"
    fn multi_report_path(base: &str, report_name: &str) -> PathBuf {
        let path = Path::new(base);
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_else(|| "csv".to_string());
        let sanitized_name = report_name.to_lowercase().replace(' ', "_");
        let new_name = format!("{stem}_{sanitized_name}.{ext}");
        path.with_file_name(new_name)
    }
}

impl ReportWriter for CsvWriter {
    fn write(&self, results: &[MetricResult], config: &OutputConfig) -> anyhow::Result<()> {
        match (&config.output_path, results.len()) {
            // Single report to file
            (Some(path), 1) => {
                let file = File::create(path)?;
                Self::write_result_to_writer(&results[0], file)?;
            }
            // Multiple reports to separate files
            (Some(path), _) => {
                for result in results {
                    let file_path = Self::multi_report_path(path, &result.name);
                    let file = File::create(&file_path)?;
                    Self::write_result_to_writer(result, file)?;
                }
            }
            // Single report to stdout
            (None, 1) => {
                let stdout = io::stdout();
                Self::write_result_to_writer(&results[0], stdout.lock())?;
            }
            // Multiple reports to stdout with separators
            (None, _) => {
                for result in results {
                    let name = result.name.to_lowercase().replace(' ', "_");
                    println!("--- {name} ---");
                    let stdout = io::stdout();
                    Self::write_result_to_writer(result, stdout.lock())?;
                    println!();
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MetricEntry, MetricValue, OutputFormat};
    use std::collections::HashMap;
    use std::fs;
    use tempfile::TempDir;

    fn sample_result(name: &str) -> MetricResult {
        MetricResult {
            name: name.to_string(),
            description: format!("{name} description"),
            columns: vec!["commits".to_string(), "lines".to_string()],
            entries: vec![
                MetricEntry {
                    key: "alice".to_string(),
                    values: HashMap::from([
                        ("commits".to_string(), MetricValue::Count(50)),
                        ("lines".to_string(), MetricValue::Count(1200)),
                    ]),
                },
                MetricEntry {
                    key: "bob".to_string(),
                    values: HashMap::from([
                        ("commits".to_string(), MetricValue::Count(30)),
                        ("lines".to_string(), MetricValue::Count(800)),
                    ]),
                },
            ],
        }
    }

    #[test]
    fn test_csv_output() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("report.csv");
        let path_str = path.to_str().unwrap().to_string();

        let config = OutputConfig {
            format: OutputFormat::Csv,
            output_path: Some(path_str),
            top: None,
            quiet: false,
        };

        let result = sample_result("Authors");
        let writer = CsvWriter;
        writer.write(&[result], &config).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("name,commits,lines"));
        assert!(content.contains("alice,50,1200"));
        assert!(content.contains("bob,30,800"));
    }

    #[test]
    fn test_csv_multiple_reports_creates_separate_files() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("report.csv");
        let path_str = path.to_str().unwrap().to_string();

        let config = OutputConfig {
            format: OutputFormat::Csv,
            output_path: Some(path_str),
            top: None,
            quiet: false,
        };

        let results = vec![sample_result("Authors"), sample_result("Churn")];
        let writer = CsvWriter;
        writer.write(&results, &config).unwrap();

        let authors_path = tmp.path().join("report_authors.csv");
        let churn_path = tmp.path().join("report_churn.csv");

        assert!(authors_path.exists(), "authors file should exist");
        assert!(churn_path.exists(), "churn file should exist");

        let authors_content = fs::read_to_string(&authors_path).unwrap();
        assert!(authors_content.contains("name,commits,lines"));
        assert!(authors_content.contains("alice"));

        let churn_content = fs::read_to_string(&churn_path).unwrap();
        assert!(churn_content.contains("name,commits,lines"));
        assert!(churn_content.contains("bob"));
    }
}
