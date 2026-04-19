use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};

use serde_json::{Map, Value, json};

use crate::output::ReportWriter;
use crate::types::{MetricEntry, MetricResult, MetricValue, OutputConfig};

pub struct JsonWriter;

impl JsonWriter {
    fn metric_value_to_json(value: &MetricValue) -> Value {
        match value {
            MetricValue::Count(n) => json!(*n),
            MetricValue::SignedCount(n) => json!(*n),
            MetricValue::Float(v) => json!(*v),
            MetricValue::Text(s) => json!(s),
            MetricValue::Date(d) => json!(d.to_string()),
            MetricValue::Message(m) => serde_json::to_value(m).unwrap_or(Value::Null),
            MetricValue::List(items) => {
                Value::Array(items.iter().map(Self::metric_value_to_json).collect())
            }
        }
    }

    fn entries_to_json(entries: &[MetricEntry]) -> Vec<Value> {
        entries
            .iter()
            .map(|entry| {
                let mut map = Map::new();
                map.insert("name".to_string(), json!(&entry.key));
                // Use sorted keys for deterministic output
                let sorted_values: BTreeMap<&String, &MetricValue> = entry.values.iter().collect();
                for (k, v) in &sorted_values {
                    map.insert((*k).clone(), Self::metric_value_to_json(v));
                }
                Value::Object(map)
            })
            .collect()
    }

    /// Serialize one report, applying `--top` to the flat entries list and
    /// attaching `total_entries` / `shown_entries` so DB-sink consumers can
    /// show "N of TOTAL" without a second round trip. `entry_groups` are
    /// fixed-dimension buckets (e.g. hourly/daily) and not truncated.
    fn result_to_json(result: &MetricResult, top: Option<usize>) -> Value {
        let mut obj = Map::new();
        obj.insert("name".to_string(), json!(result.name));
        obj.insert("display_name".to_string(), json!(result.display_name));
        obj.insert("description".to_string(), json!(result.description));
        obj.insert("columns".to_string(), json!(result.columns));

        if result.entry_groups.is_empty() {
            let total = result.entries.len();
            let slice: &[MetricEntry] = match top {
                Some(n) if n < total => &result.entries[..n],
                _ => &result.entries[..],
            };
            obj.insert("total_entries".to_string(), json!(total));
            obj.insert("shown_entries".to_string(), json!(slice.len()));
            obj.insert("entries".to_string(), json!(Self::entries_to_json(slice)));
        } else {
            let total: usize = result.entry_groups.iter().map(|g| g.entries.len()).sum();
            obj.insert("total_entries".to_string(), json!(total));
            obj.insert("shown_entries".to_string(), json!(total));
            let groups: Vec<Value> = result
                .entry_groups
                .iter()
                .map(|g| {
                    let mut gm = Map::new();
                    gm.insert("name".to_string(), json!(&g.name));
                    gm.insert("label".to_string(), json!(&g.label));
                    gm.insert(
                        "entries".to_string(),
                        json!(Self::entries_to_json(&g.entries)),
                    );
                    Value::Object(gm)
                })
                .collect();
            obj.insert("entry_groups".to_string(), Value::Array(groups));
        }

        Value::Object(obj)
    }
}

impl ReportWriter for JsonWriter {
    fn write(&self, results: &[MetricResult], config: &OutputConfig) -> anyhow::Result<()> {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let mut reports = Map::new();
        for result in results {
            let key = result.name.to_lowercase().replace(' ', "_");
            reports.insert(key, Self::result_to_json(result, config.top));
        }

        let output = json!({
            "generated_at": now,
            "reports": reports
        });

        // Stream serialization straight into the sink (file or stdout) so we
        // never materialise the full JSON document in memory — critical on
        // memory-constrained pods where reports can be tens of MB. Compact
        // output: downstream consumers pretty-print if they want to.
        if let Some(path) = &config.output_path {
            let mut writer = BufWriter::new(File::create(path)?);
            serde_json::to_writer(&mut writer, &output)?;
            writer.flush()?;
        } else {
            let stdout = std::io::stdout();
            let mut writer = BufWriter::new(stdout.lock());
            serde_json::to_writer(&mut writer, &output)?;
            writeln!(writer)?;
            writer.flush()?;
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
    fn test_json_output_valid() {
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
            format: OutputFormat::Json,
            output_path: Some(path.clone()),
            top: None,
            quiet: false,
            locale: "en".into(),
        };

        let writer = JsonWriter;
        writer.write(&[result], &config).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();

        // Verify structure
        assert!(parsed.get("generated_at").is_some());
        assert!(parsed.get("reports").is_some());

        let reports = parsed.get("reports").unwrap().as_object().unwrap();
        assert!(reports.contains_key("authors"));

        let authors = reports.get("authors").unwrap();
        assert_eq!(authors.get("name").unwrap().as_str().unwrap(), "authors");
        let display_name = authors.get("display_name").unwrap();
        assert_eq!(
            display_name.get("code").unwrap().as_str().unwrap(),
            "report.authors.display_name"
        );
        let columns = authors.get("columns").unwrap().as_array().unwrap();
        assert_eq!(
            columns[0].get("value").unwrap().as_str().unwrap(),
            "commits"
        );
        assert_eq!(
            columns[0]
                .get("label")
                .unwrap()
                .get("code")
                .unwrap()
                .as_str()
                .unwrap(),
            "report.authors.column.commits"
        );

        let entries = authors.get("entries").unwrap().as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].get("name").unwrap().as_str().unwrap(), "alice");
        assert_eq!(entries[0].get("commits").unwrap().as_u64().unwrap(), 50);
    }
}
