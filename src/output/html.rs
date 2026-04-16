use std::fs;

use crate::output::ReportWriter;
use crate::types::{MetricEntry, MetricResult, MetricValue, OutputConfig};

const TEMPLATE: &str = include_str!("../../templates/report.html");

pub struct HtmlWriter;

/// Escape special HTML characters to prevent XSS.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

impl HtmlWriter {
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

    /// Find the first numeric column name (for bar chart widths).
    fn first_numeric_column(result: &MetricResult) -> Option<String> {
        let columns = Self::get_columns(result);
        let first_entry = result.entries.first()?;
        for col in &columns {
            if let Some(
                MetricValue::Count(_) | MetricValue::SignedCount(_) | MetricValue::Float(_),
            ) = first_entry.values.get(col)
            {
                return Some(col.clone());
            }
        }
        None
    }

    /// Extract numeric value for bar chart sizing.
    fn numeric_value(value: &MetricValue) -> f64 {
        match value {
            MetricValue::Count(n) => *n as f64,
            MetricValue::SignedCount(n) => *n as f64,
            MetricValue::Float(v) => *v,
            _ => 0.0,
        }
    }

    /// Number of entries shown by default before requiring expand.
    const TOP_N: usize = 10;

    fn render_entries_block(
        entries: &[MetricEntry],
        columns: &[String],
        bar_col: &Option<String>,
    ) -> String {
        let mut html = String::new();
        let total = entries.len();
        let has_extra = total > Self::TOP_N;

        // Find max value for bar chart scaling
        let max_val = bar_col
            .as_ref()
            .map(|col| {
                entries
                    .iter()
                    .filter_map(|e| e.values.get(col))
                    .map(Self::numeric_value)
                    .fold(0.0f64, f64::max)
            })
            .unwrap_or(0.0);

        // Bar chart (if numeric column exists)
        if let Some(bar_col_name) = bar_col {
            html.push_str("    <div style=\"margin-bottom: 1rem;\">\n");
            for (i, entry) in entries.iter().enumerate() {
                let val = entry
                    .values
                    .get(bar_col_name)
                    .map(Self::numeric_value)
                    .unwrap_or(0.0);
                let width_pct = if max_val > 0.0 {
                    (val / max_val * 100.0).round() as u32
                } else {
                    0
                };
                let extra_class = if i >= Self::TOP_N {
                    " bar-row extra-row"
                } else {
                    ""
                };
                html.push_str(&format!(
                    "      <div class=\"bar-row{extra_class}\" style=\"margin: 2px 0;\"><span class=\"bar\" style=\"width: {width_pct}%;\"></span><span>{} ({})</span></div>\n",
                    escape_html(&entry.key),
                    escape_html(&Self::format_value(
                        entry.values.get(bar_col_name).unwrap_or(&MetricValue::Count(0))
                    ))
                ));
            }
            html.push_str("    </div>\n");
        }

        // Data table
        html.push_str("    <table>\n");
        html.push_str("      <thead><tr><th>Name</th>");
        for col in columns {
            html.push_str(&format!("<th>{}</th>", escape_html(col)));
        }
        html.push_str("</tr></thead>\n");
        html.push_str("      <tbody>\n");

        for (i, entry) in entries.iter().enumerate() {
            let extra_class = if i >= Self::TOP_N {
                " class=\"extra-row\""
            } else {
                ""
            };
            html.push_str(&format!("        <tr{extra_class}>"));
            html.push_str(&format!("<td>{}</td>", escape_html(&entry.key)));
            for col in columns {
                let val = entry
                    .values
                    .get(col)
                    .map(Self::format_value)
                    .unwrap_or_default();
                html.push_str(&format!("<td>{}</td>", escape_html(&val)));
            }
            html.push_str("</tr>\n");
        }

        html.push_str("      </tbody>\n");
        html.push_str("    </table>\n");

        // Show more button (only if there are extra rows)
        if has_extra {
            let remaining = total - Self::TOP_N;
            let show_text = format!("Show all {} entries", total);
            let hide_text = format!("Show top {} only", Self::TOP_N);
            html.push_str(&format!(
                "    <span class=\"show-more-btn\" data-expanded=\"false\" data-show-text=\"{}\" data-hide-text=\"{}\">{}</span>\n",
                escape_html(&show_text),
                escape_html(&hide_text),
                escape_html(&format!("Show {} more entries", remaining))
            ));
        }

        html
    }

    fn render_section(result: &MetricResult) -> String {
        let columns = Self::get_columns(result);
        let bar_col = Self::first_numeric_column(result);

        let total: usize = if result.entry_groups.is_empty() {
            result.entries.len()
        } else {
            result
                .entry_groups
                .iter()
                .map(|(_, entries)| entries.len())
                .sum()
        };

        let mut html = String::new();

        // Section wrapper
        html.push_str("<div class=\"report-section\">\n");
        html.push_str(&format!(
            "  <div class=\"section-header\"><h2>{}<span class=\"entry-count\">({} entries)</span></h2><span class=\"toggle\">\u{25be}</span></div>\n",
            escape_html(&result.name),
            total
        ));
        html.push_str("  <div class=\"section-body\">\n");

        // Description
        if !result.description.is_empty() {
            html.push_str(&format!(
                "    <p class=\"section-desc\">{}</p>\n",
                escape_html(&result.description)
            ));
        }

        if result.entry_groups.is_empty() {
            html.push_str(&Self::render_entries_block(
                &result.entries,
                &columns,
                &bar_col,
            ));
        } else {
            for (group_name, group_entries) in &result.entry_groups {
                html.push_str(&format!(
                    "    <h3 style=\"color: #58a6ff; margin: 1.2rem 0 0.5rem; font-size: 0.95rem; text-transform: capitalize;\">{} <span style=\"color: #8b949e; font-size: 0.8rem;\">({} entries)</span></h3>\n",
                    escape_html(group_name),
                    group_entries.len()
                ));
                html.push_str(&Self::render_entries_block(
                    group_entries,
                    &columns,
                    &bar_col,
                ));
            }
        }

        html.push_str("  </div>\n");
        html.push_str("</div>\n");

        html
    }
}

impl ReportWriter for HtmlWriter {
    fn write(&self, results: &[MetricResult], config: &OutputConfig) -> anyhow::Result<()> {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let sections: String = results.iter().map(Self::render_section).collect();

        let html = TEMPLATE
            .replace("{{GENERATED_AT}}", &escape_html(&now))
            .replace("{{REPORT_SECTIONS}}", &sections);

        if let Some(path) = &config.output_path {
            fs::write(path, &html)?;
        } else {
            println!("{html}");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MetricEntry, OutputFormat};
    use std::collections::HashMap;
    use std::fs;
    use tempfile::NamedTempFile;

    #[test]
    fn test_html_output_contains_sections() {
        let result = MetricResult {
            name: "Authors".to_string(),
            description: "Top authors by commits".to_string(),
            columns: vec!["commits".to_string()],
            entry_groups: vec![],
            entries: vec![
                MetricEntry {
                    key: "alice".to_string(),
                    values: HashMap::from([("commits".to_string(), MetricValue::Count(50))]),
                },
                MetricEntry {
                    key: "bob".to_string(),
                    values: HashMap::from([("commits".to_string(), MetricValue::Count(30))]),
                },
            ],
        };

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let config = OutputConfig {
            format: OutputFormat::Html,
            output_path: Some(path.clone()),
            top: None,
            quiet: false,
        };

        let writer = HtmlWriter;
        writer.write(&[result], &config).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("Authors"), "should contain report name");
        assert!(content.contains("alice"), "should contain entry data");
        assert!(content.contains("bob"), "should contain entry data");
        assert!(content.contains("50"), "should contain metric value");
        assert!(
            content.contains("Generated:"),
            "should contain generated timestamp"
        );
    }

    #[test]
    fn test_html_escapes_special_chars() {
        assert_eq!(escape_html("<script>"), "&lt;script&gt;");
        assert_eq!(escape_html("a & b"), "a &amp; b");
        assert_eq!(escape_html("\"hello\""), "&quot;hello&quot;");
        assert_eq!(escape_html("it's"), "it&#x27;s");
    }
}
