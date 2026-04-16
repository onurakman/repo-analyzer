use comfy_table::{Cell, CellAlignment, Color, Table};

use crate::output::ReportWriter;
use crate::types::{MetricResult, MetricValue, OutputConfig};

pub struct TerminalWriter;

impl TerminalWriter {
    fn format_value(value: &MetricValue) -> String {
        match value {
            MetricValue::Count(n) => n.to_string(),
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
        // Fallback: derive from first entry's value keys (sorted)
        if let Some(first) = result.entries.first() {
            let mut cols: Vec<String> = first.values.keys().cloned().collect();
            cols.sort();
            cols
        } else {
            vec![]
        }
    }
}

impl ReportWriter for TerminalWriter {
    fn write(&self, results: &[MetricResult], config: &OutputConfig) -> anyhow::Result<()> {
        let top_n = config.top.unwrap_or(usize::MAX);

        for result in results {
            // Print header
            println!("\n{}", "=".repeat(60));
            println!("  {}", result.name);
            println!("{}", "=".repeat(60));

            if !result.description.is_empty() {
                println!("  {}\n", result.description);
            }

            let columns = Self::get_columns(result);
            if columns.is_empty() && result.entries.is_empty() {
                println!("  (no data)");
                continue;
            }

            let mut table = Table::new();

            // Header row
            let mut header_cells = vec![Cell::new("Name")
                .fg(Color::Cyan)
                .set_alignment(CellAlignment::Left)];
            for col in &columns {
                header_cells.push(
                    Cell::new(col)
                        .fg(Color::Cyan)
                        .set_alignment(CellAlignment::Left),
                );
            }
            table.set_header(header_cells);

            // Data rows
            let display_count = result.entries.len().min(top_n);
            for entry in result.entries.iter().take(display_count) {
                let mut row = vec![entry.key.clone()];
                for col in &columns {
                    let val = entry
                        .values
                        .get(col)
                        .map(Self::format_value)
                        .unwrap_or_default();
                    row.push(val);
                }
                table.add_row(row);
            }

            println!("{table}");

            if result.entries.len() > top_n {
                let remaining = result.entries.len() - top_n;
                println!("  ... and {remaining} more");
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use crate::types::{MetricEntry, OutputFormat};

    #[test]
    fn test_terminal_writer_no_panic() {
        let result = MetricResult {
            name: "Test Metric".to_string(),
            description: "A test metric".to_string(),
            columns: vec!["commits".to_string(), "lines".to_string()],
            entries: vec![MetricEntry {
                key: "alice".to_string(),
                values: HashMap::from([
                    ("commits".to_string(), MetricValue::Count(42)),
                    ("lines".to_string(), MetricValue::Count(1000)),
                ]),
            }],
        };

        let config = OutputConfig {
            format: OutputFormat::Table,
            output_path: None,
            top: Some(10),
            quiet: false,
        };

        let writer = TerminalWriter;
        // Should not panic
        writer.write(&[result], &config).unwrap();
    }
}
