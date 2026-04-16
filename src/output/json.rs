use std::collections::BTreeMap;
use std::fs;

use serde_json::{json, Map, Value};

use crate::output::ReportWriter;
use crate::types::{MetricResult, MetricValue, OutputConfig};

pub struct JsonWriter;

impl JsonWriter {
    fn metric_value_to_json(value: &MetricValue) -> Value {
        match value {
            MetricValue::Count(n) => json!(*n),
            MetricValue::Float(v) => json!(*v),
            MetricValue::Text(s) => json!(s),
            MetricValue::Date(d) => json!(d.to_string()),
            MetricValue::List(items) => {
                Value::Array(items.iter().map(Self::metric_value_to_json).collect())
            }
        }
    }

    fn result_to_json(result: &MetricResult) -> Value {
        let entries: Vec<Value> = result
            .entries
            .iter()
            .map(|entry| {
                let mut map = Map::new();
                map.insert("name".to_string(), json!(&entry.key));
                // Use sorted keys for deterministic output
                let sorted_values: BTreeMap<&String, &MetricValue> =
                    entry.values.iter().collect();
                for (k, v) in &sorted_values {
                    map.insert((*k).clone(), Self::metric_value_to_json(v));
                }
                Value::Object(map)
            })
            .collect();

        json!({
            "name": result.name,
            "description": result.description,
            "columns": result.columns,
            "entries": entries
        })
    }
}

impl ReportWriter for JsonWriter {
    fn write(&self, results: &[MetricResult], config: &OutputConfig) -> anyhow::Result<()> {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let mut reports = Map::new();
        for result in results {
            let key = result.name.to_lowercase().replace(' ', "_");
            reports.insert(key, Self::result_to_json(result));
        }

        let output = json!({
            "generated_at": now,
            "reports": reports
        });

        let pretty = serde_json::to_string_pretty(&output)?;

        if let Some(path) = &config.output_path {
            fs::write(path, &pretty)?;
        } else {
            println!("{pretty}");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use crate::types::{MetricEntry, OutputFormat};
    use tempfile::NamedTempFile;

    #[test]
    fn test_json_output_valid() {
        let result = MetricResult {
            name: "Authors".to_string(),
            description: "Top authors".to_string(),
            columns: vec!["commits".to_string()],
            entries: vec![
                MetricEntry {
                    key: "alice".to_string(),
                    values: HashMap::from([
                        ("commits".to_string(), MetricValue::Count(50)),
                    ]),
                },
                MetricEntry {
                    key: "bob".to_string(),
                    values: HashMap::from([
                        ("commits".to_string(), MetricValue::Count(30)),
                    ]),
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
        assert_eq!(
            authors.get("name").unwrap().as_str().unwrap(),
            "Authors"
        );

        let entries = authors.get("entries").unwrap().as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].get("name").unwrap().as_str().unwrap(), "alice");
        assert_eq!(entries[0].get("commits").unwrap().as_u64().unwrap(), 50);
    }
}
