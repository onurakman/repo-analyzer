use comfy_table::{Cell, CellAlignment, Color, Table};

use crate::output::ReportWriter;
use crate::types::{MetricEntry, MetricResult, MetricValue, OutputConfig};

pub struct TerminalWriter;

impl TerminalWriter {
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

impl TerminalWriter {
    fn render_table(entries: &[MetricEntry], columns: &[String], labels: &[String], top_n: usize) {
        let mut table = Table::new();

        // Header row uses human-friendly labels (parallel to `columns`).
        // Falls back to the raw column key if a label is missing.
        let mut header_cells = vec![
            Cell::new("Name")
                .fg(Color::Cyan)
                .set_alignment(CellAlignment::Left),
        ];
        for (i, col) in columns.iter().enumerate() {
            let label = labels.get(i).map(String::as_str).unwrap_or(col);
            header_cells.push(
                Cell::new(label)
                    .fg(Color::Cyan)
                    .set_alignment(CellAlignment::Left),
            );
        }
        table.set_header(header_cells);

        // Data rows — value lookup still uses the snake_case column key.
        let display_count = entries.len().min(top_n);
        for entry in entries.iter().take(display_count) {
            let mut row = vec![entry.key.clone()];
            for col in columns {
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

        // Surface the real total so `--top 20` doesn't look like there are
        // only 20 entries. Only fires when truncation actually hid rows.
        let total = entries.len();
        if total > top_n {
            println!("  ({top_n} of {total} shown; re-run without `--top` to see all)");
        }
    }
}

impl ReportWriter for TerminalWriter {
    fn write(&self, results: &[MetricResult], config: &OutputConfig) -> anyhow::Result<()> {
        let top_n = config.top.unwrap_or(usize::MAX);

        for result in results {
            // Print header
            println!("\n{}", "=".repeat(60));
            let title = if result.display_name.is_empty() {
                &result.name
            } else {
                &result.display_name
            };
            println!("  {title}");
            println!("{}", "=".repeat(60));

            if !result.description.is_empty() {
                println!("  {}\n", result.description);
            }

            let columns = Self::get_columns(result);
            let labels = if result.column_labels.is_empty() {
                columns.clone()
            } else {
                result.column_labels.clone()
            };

            if result.entry_groups.is_empty() {
                if columns.is_empty() && result.entries.is_empty() {
                    println!("  (no data)");
                    continue;
                }
                Self::render_table(&result.entries, &columns, &labels, top_n);
            } else {
                for group in &result.entry_groups {
                    println!(
                        "\n  -- {} ({} entries) --",
                        group.label,
                        group.entries.len()
                    );
                    Self::render_table(&group.entries, &columns, &labels, top_n);
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MetricEntry, OutputFormat};
    use std::collections::HashMap;

    #[test]
    fn test_terminal_writer_no_panic() {
        let result = MetricResult {
            name: "test_metric".to_string(),
            display_name: "Test Metric".to_string(),
            description: "A test metric".to_string(),
            columns: vec!["commits".to_string(), "lines".to_string()],
            column_labels: vec!["Commits".to_string(), "Lines".to_string()],
            entry_groups: vec![],
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
