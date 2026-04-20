use std::fs::File;
use std::io::{self, BufWriter, Write};

use serde_json::{Map, Value, json};

use crate::i18n::Catalog;
use crate::output::ReportWriter;
use crate::types::{MetricEntry, MetricResult, MetricValue, OutputConfig};

const TEMPLATE: &str = include_str!("../../templates/report.html");
const GENERATED_AT_MARKER: &str = "{{GENERATED_AT}}";
const REPORT_DATA_MARKER: &str = "{{REPORT_DATA_JSON}}";

pub struct HtmlWriter;

/// Escape special HTML characters so they don't break out of attributes or
/// inline text. The JSON payload goes through [`ScriptEscapeWriter`] below
/// rather than this function.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// A `Write` wrapper that rewrites any `</` byte sequence to `<\/` on the fly.
/// Lets us stream serde_json directly into the HTML template without buffering
/// the full JSON payload, while still preventing a literal `</script>` in the
/// data from closing the surrounding `<script type="application/json">` block.
///
/// Byte-oriented so it works correctly even if `<` lands on a buffer boundary.
struct ScriptEscapeWriter<W: Write> {
    inner: W,
    pending_lt: bool,
}

impl<W: Write> ScriptEscapeWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            pending_lt: false,
        }
    }

    fn finish(mut self) -> io::Result<W> {
        if self.pending_lt {
            self.inner.write_all(b"<")?;
            self.pending_lt = false;
        }
        Ok(self.inner)
    }
}

impl<W: Write> Write for ScriptEscapeWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut i = 0;
        while i < buf.len() {
            if self.pending_lt {
                if buf[i] == b'/' {
                    self.inner.write_all(b"<\\/")?;
                    i += 1;
                } else {
                    self.inner.write_all(b"<")?;
                }
                self.pending_lt = false;
                continue;
            }
            let rest = &buf[i..];
            match memchr::memchr(b'<', rest) {
                Some(pos) => {
                    if pos > 0 {
                        self.inner.write_all(&rest[..pos])?;
                    }
                    i += pos + 1;
                    if i == buf.len() {
                        self.pending_lt = true;
                    } else if buf[i] == b'/' {
                        self.inner.write_all(b"<\\/")?;
                        i += 1;
                    } else {
                        self.inner.write_all(b"<")?;
                    }
                }
                None => {
                    self.inner.write_all(rest)?;
                    i = buf.len();
                }
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// One segment of the pre-split template: either a literal chunk of HTML or
/// a placeholder marker that the streamer substitutes at write time.
enum Segment {
    Literal(&'static str),
    GeneratedAt,
    ReportData,
}

/// Split the template once at program start into a flat list of literal
/// chunks and markers. The template has `{{GENERATED_AT}}` appearing twice
/// and `{{REPORT_DATA_JSON}}` once, in arbitrary order — so we scan linearly
/// and replace each occurrence in place. Returns an Arc-less static-lived
/// vector because `TEMPLATE` is `'static`.
fn split_template() -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut rest = TEMPLATE;
    while !rest.is_empty() {
        let gen_idx = rest.find(GENERATED_AT_MARKER);
        let data_idx = rest.find(REPORT_DATA_MARKER);
        let next = match (gen_idx, data_idx) {
            (Some(g), Some(d)) if g < d => Some((g, GENERATED_AT_MARKER, Segment::GeneratedAt)),
            (Some(_), Some(d)) => Some((d, REPORT_DATA_MARKER, Segment::ReportData)),
            (Some(g), None) => Some((g, GENERATED_AT_MARKER, Segment::GeneratedAt)),
            (None, Some(d)) => Some((d, REPORT_DATA_MARKER, Segment::ReportData)),
            (None, None) => None,
        };
        match next {
            Some((idx, marker, seg)) => {
                if idx > 0 {
                    segments.push(Segment::Literal(&rest[..idx]));
                }
                segments.push(seg);
                rest = &rest[idx + marker.len()..];
            }
            None => {
                segments.push(Segment::Literal(rest));
                break;
            }
        }
    }
    segments
}

/// Build a JSON tree where every [`crate::types::LocalizedMessage`] has been
/// resolved to its translated string via the supplied catalog. The HTML
/// template consumes plain strings for `display_name`, `description`, column
/// labels, and message-valued cells — so we flatten everything here rather
/// than teaching the frontend to understand the i18n envelope.
fn build_report_json(results: &[MetricResult], catalog: &Catalog) -> Value {
    Value::Array(
        results
            .iter()
            .map(|r| build_one_report(r, catalog))
            .collect(),
    )
}

fn build_one_report(r: &MetricResult, catalog: &Catalog) -> Value {
    let mut obj = Map::new();
    obj.insert("name".into(), json!(r.name));
    obj.insert(
        "display_name".into(),
        json!(catalog.translate(&r.display_name)),
    );
    obj.insert(
        "description".into(),
        json!(catalog.translate(&r.description)),
    );

    let column_keys: Vec<&String> = r.columns.iter().map(|c| &c.value).collect();
    let column_labels: Vec<String> = r
        .columns
        .iter()
        .map(|c| catalog.translate(&c.label))
        .collect();
    obj.insert("columns".into(), json!(column_keys));
    obj.insert("column_labels".into(), json!(column_labels));

    if r.entry_groups.is_empty() {
        obj.insert("entries".into(), build_entries(&r.entries, catalog));
    } else {
        let groups: Vec<Value> = r
            .entry_groups
            .iter()
            .map(|g| {
                json!({
                    "name": g.name,
                    "label": catalog.translate_code(&g.label),
                    "entries": build_entries(&g.entries, catalog),
                })
            })
            .collect();
        obj.insert("entry_groups".into(), Value::Array(groups));
    }

    Value::Object(obj)
}

fn build_entries(entries: &[MetricEntry], catalog: &Catalog) -> Value {
    let arr: Vec<Value> = entries
        .iter()
        .map(|e| {
            let mut values = Map::new();
            for (k, v) in &e.values {
                values.insert(k.clone(), translate_metric_value(v, catalog));
            }
            json!({ "key": e.key, "values": Value::Object(values) })
        })
        .collect();
    Value::Array(arr)
}

fn translate_metric_value(v: &MetricValue, catalog: &Catalog) -> Value {
    match v {
        MetricValue::Message(m) => Value::String(catalog.translate(m)),
        MetricValue::List(items) => Value::Array(
            items
                .iter()
                .map(|i| translate_metric_value(i, catalog))
                .collect(),
        ),
        other => serde_json::to_value(other).unwrap_or(Value::Null),
    }
}

fn stream_html<W: Write>(
    writer: &mut W,
    results: &[MetricResult],
    locale: &str,
) -> anyhow::Result<()> {
    let segments = split_template();
    let now_escaped = escape_html(&chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());
    let catalog = Catalog::load(locale);
    let data = build_report_json(results, &catalog);

    for seg in segments {
        match seg {
            Segment::Literal(s) => writer.write_all(s.as_bytes())?,
            Segment::GeneratedAt => writer.write_all(now_escaped.as_bytes())?,
            Segment::ReportData => {
                let mut script_writer = ScriptEscapeWriter::new(&mut *writer);
                serde_json::to_writer(&mut script_writer, &data)?;
                script_writer.finish()?;
            }
        }
    }
    Ok(())
}

impl ReportWriter for HtmlWriter {
    fn write(&self, results: &[MetricResult], config: &OutputConfig) -> anyhow::Result<()> {
        if let Some(path) = &config.output_path {
            let mut writer = BufWriter::new(File::create(path)?);
            stream_html(&mut writer, results, &config.locale)?;
            writer.flush()?;
        } else {
            let stdout = std::io::stdout();
            let mut writer = BufWriter::new(stdout.lock());
            stream_html(&mut writer, results, &config.locale)?;
            writer.flush()?;
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
        use crate::types::{Column, report_description, report_display};
        let result = MetricResult {
            name: "authors".to_string(),
            display_name: report_display("authors"),
            description: report_description("authors"),
            columns: vec![Column::in_report("authors", "commits")],
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
            locale: "en".into(),
        };

        let writer = HtmlWriter;
        writer.write(&[result], &config).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        // The HTML embeds the report data as JSON inside a <script> tag.
        // display_name / description / column labels are resolved to strings
        // via the bundled `en` catalog before being serialized, so the
        // LocalizedMessage envelope never reaches the frontend.
        assert!(
            content.contains("\"display_name\":\"Authors\""),
            "should include translated display_name in embedded JSON"
        );
        assert!(
            !content.contains("\"code\":\"report.authors.display_name\""),
            "raw LocalizedMessage envelope must not leak into HTML"
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
    fn script_escape_writer_rewrites_close_tags() {
        // Guard against a commit message or file path breaking out of the
        // embedded <script type="application/json"> block.
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut w = ScriptEscapeWriter::new(&mut buf);
            w.write_all(b"{\"x\":\"foo</script>bar\"}").unwrap();
            w.finish().unwrap();
        }
        let out = String::from_utf8(buf).unwrap();
        assert!(!out.contains("</script"));
        assert!(out.contains("<\\/script"));
    }

    #[test]
    fn script_escape_writer_handles_boundary_split() {
        // `<` at end of one write, `/` at start of next — must still rewrite.
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut w = ScriptEscapeWriter::new(&mut buf);
            w.write_all(b"foo<").unwrap();
            w.write_all(b"/bar").unwrap();
            w.finish().unwrap();
        }
        assert_eq!(String::from_utf8(buf).unwrap(), "foo<\\/bar");
    }

    #[test]
    fn script_escape_writer_preserves_lone_lt() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut w = ScriptEscapeWriter::new(&mut buf);
            w.write_all(b"a < b").unwrap();
            w.finish().unwrap();
        }
        assert_eq!(String::from_utf8(buf).unwrap(), "a < b");
    }
}
