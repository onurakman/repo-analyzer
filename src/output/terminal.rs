use comfy_table::{Cell, CellAlignment, Color, Table};

use crate::i18n::Catalog;
use crate::output::ReportWriter;
use crate::types::{Column, MetricEntry, MetricResult, MetricValue, OutputConfig, humanize};

pub struct TerminalWriter;

impl TerminalWriter {
    fn format_value(catalog: &Catalog, value: &MetricValue) -> String {
        match value {
            MetricValue::Count(n) => n.to_string(),
            MetricValue::SignedCount(n) => n.to_string(),
            MetricValue::Float(v) => format!("{v:.2}"),
            MetricValue::Text(s) => s.clone(),
            MetricValue::Date(d) => d.to_string(),
            MetricValue::Message(m) => catalog.translate(m),
            MetricValue::List(items) => {
                let parts: Vec<String> = items.iter().map(|i| i.to_string()).collect();
                format!("[{}]", parts.join(", "))
            }
        }
    }

    fn get_columns(result: &MetricResult) -> Vec<Column> {
        if !result.columns.is_empty() {
            return result.columns.clone();
        }
        // Fallback: derive from first entry's value keys (sorted). Label is
        // a generic code since we don't know the owning report; writers then
        // humanise the code when translation is missing.
        if let Some(first) = result.entries.first() {
            let mut keys: Vec<String> = first.values.keys().cloned().collect();
            keys.sort();
            keys.into_iter()
                .map(|k| Column::in_report(&result.name, k))
                .collect()
        } else {
            vec![]
        }
    }
}

impl TerminalWriter {
    fn render_table(catalog: &Catalog, entries: &[MetricEntry], columns: &[Column], top_n: usize) {
        let mut table = Table::new();

        let mut header_cells = vec![
            Cell::new("Name")
                .fg(Color::Cyan)
                .set_alignment(CellAlignment::Left),
        ];
        for col in columns {
            let mut label = catalog.translate(&col.label);
            if label == col.label.code {
                // No catalog hit — fall back to the snake_case value turned
                // into Title Case so terminal output still looks readable.
                label = humanize(&col.value);
            }
            header_cells.push(
                Cell::new(label)
                    .fg(Color::Cyan)
                    .set_alignment(CellAlignment::Left),
            );
        }
        table.set_header(header_cells);

        let display_count = entries.len().min(top_n);
        for entry in entries.iter().take(display_count) {
            let mut row = vec![entry.key.clone()];
            for col in columns {
                let val = entry
                    .values
                    .get(&col.value)
                    .map(|v| Self::format_value(catalog, v))
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
        let catalog = Catalog::load(&config.locale);

        for result in results {
            println!("\n{}", "=".repeat(60));
            let mut title = catalog.translate(&result.display_name);
            if title == result.display_name.code {
                title = result.name.clone();
            }
            println!("  {title}");
            println!("{}", "=".repeat(60));

            let description = catalog.translate(&result.description);
            if description != result.description.code && !description.is_empty() {
                println!("  {description}\n");
            }

            let columns = Self::get_columns(result);

            if result.entry_groups.is_empty() {
                if columns.is_empty() && result.entries.is_empty() {
                    println!("  (no data)");
                    continue;
                }
                Self::render_table(&catalog, &result.entries, &columns, top_n);
            } else {
                for group in &result.entry_groups {
                    println!(
                        "\n  -- {} ({} entries) --",
                        catalog.translate_code(&group.label),
                        group.entries.len()
                    );
                    Self::render_table(&catalog, &group.entries, &columns, top_n);
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
        use crate::types::{report_description, report_display};
        let result = MetricResult {
            name: "test_metric".to_string(),
            display_name: report_display("test_metric"),
            description: report_description("test_metric"),
            columns: vec![
                Column::in_report("test_metric", "commits"),
                Column::in_report("test_metric", "lines"),
            ],
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
            locale: "en".into(),
        };

        let writer = TerminalWriter;
        // Should not panic
        writer.write(&[result], &config).unwrap();
    }
}
