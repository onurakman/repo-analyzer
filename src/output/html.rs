use std::fs;

use crate::output::ReportWriter;
use crate::types::{MetricResult, OutputConfig};

const TEMPLATE: &str = include_str!("../../templates/report.html");

pub struct HtmlWriter;

/// Escape special HTML characters so they don't break out of attributes or
/// inline text. The JSON payload goes through `encode_json_for_script` below
/// rather than this function.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Produce a JSON string that is safe to embed inside a
/// `<script type="application/json">…</script>` element. We just have to stop
/// any literal `</script>` sequence in the data from prematurely closing the
/// tag; the rest of JSON escaping is handled by `serde_json`.
fn encode_json_for_script(s: &str) -> String {
    s.replace("</", "<\\/")
}

impl ReportWriter for HtmlWriter {
    fn write(&self, results: &[MetricResult], config: &OutputConfig) -> anyhow::Result<()> {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let raw_json = serde_json::to_string(results)?;
        let safe_json = encode_json_for_script(&raw_json);

        let html = TEMPLATE
            .replace("{{GENERATED_AT}}", &escape_html(&now))
            .replace("{{REPORT_DATA_JSON}}", &safe_json);

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
    use crate::types::{MetricEntry, MetricValue, OutputFormat};
    use std::collections::HashMap;
    use std::fs;
    use tempfile::NamedTempFile;

    #[test]
    fn test_html_output_contains_sections() {
        let result = MetricResult {
            name: "authors".to_string(),
            display_name: "Authors".to_string(),
            description: "Top authors by commits".to_string(),
            columns: vec!["commits".to_string()],
            column_labels: vec!["Commits".to_string()],
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
        // The HTML embeds the report data as JSON inside a <script> tag.
        assert!(
            content.contains("\"display_name\":\"Authors\""),
            "should include display_name in embedded JSON"
        );
        assert!(content.contains("alice"), "should contain entry data");
        assert!(content.contains("bob"), "should contain entry data");
        assert!(content.contains("\"commits\":50"));
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

    #[test]
    fn json_script_close_tags_are_escaped() {
        // Guard against a commit message or file path breaking out of the
        // embedded <script type="application/json"> block.
        let out = encode_json_for_script("{\"x\":\"foo</script>bar\"}");
        assert!(!out.contains("</script"));
        assert!(out.contains("<\\/script"));
    }
}
