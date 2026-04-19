use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use regex::Regex;

use crate::analysis::source_filter::is_source_file;
use crate::messages;
use crate::metrics::MetricCollector;
use crate::types::{
    Column, LocalizedMessage, MetricEntry, MetricResult, MetricValue, ParsedChange,
    report_description, report_display,
};

/// Skip files larger than this when scanning for imports.
const MAX_BLOB_BYTES: u64 = 200 * 1024;

#[derive(Default, Clone, Copy)]
struct Counts {
    fan_in: u64,
    fan_out: u64,
}

pub struct FanInOutCollector {
    counts: HashMap<String, Counts>,
}

impl Default for FanInOutCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl FanInOutCollector {
    pub fn new() -> Self {
        Self {
            counts: HashMap::new(),
        }
    }
}

impl MetricCollector for FanInOutCollector {
    fn name(&self) -> &str {
        "fan_in_out"
    }

    fn process(&mut self, _change: &ParsedChange) {}

    fn inspect_repo(
        &mut self,
        repo: &gix::Repository,
        progress: &crate::metrics::ProgressReporter,
    ) -> anyhow::Result<()> {
        let head_commit = match repo.head_commit() {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        let tree = head_commit.tree()?;

        progress.status("  fan_in_out: pass 1/2 collecting source paths...");
        // Pass 1: collect all candidate source paths in the repo.
        let mut all_paths: Vec<(String, gix::ObjectId, u64)> = vec![];
        collect_blobs(repo, &tree, "", &mut all_paths);
        let path_set: HashSet<String> = all_paths.iter().map(|(p, _, _)| p.clone()).collect();

        // Pass 2: for each source file, extract imports and resolve them against the path set.
        let total = all_paths.len();
        for (idx, (path, oid, size)) in all_paths.iter().enumerate() {
            if idx.is_multiple_of(200) {
                progress.status(&format!(
                    "  fan_in_out: pass 2/2 {}/{total} files...",
                    idx + 1
                ));
            }
            if !is_source_file(path) {
                continue;
            }
            if *size > MAX_BLOB_BYTES {
                continue;
            }
            let Some(lang) = detect_lang(path) else {
                continue;
            };
            let Ok(object) = repo.find_object(*oid) else {
                continue;
            };
            let Ok(blob) = object.try_into_blob() else {
                continue;
            };
            let Ok(source) = std::str::from_utf8(&blob.data) else {
                continue;
            };

            let imports = extract_imports(lang, source);
            for raw in imports {
                if let Some(target) = resolve_import(lang, &raw, path, &path_set) {
                    if target == *path {
                        continue; // self-import shouldn't happen, but guard
                    }
                    self.counts.entry(target).or_default().fan_in += 1;
                    self.counts.entry(path.clone()).or_default().fan_out += 1;
                }
            }
            // Make sure every source file appears even if it imports nothing or is unimported.
            self.counts.entry(path.clone()).or_default();
        }

        Ok(())
    }

    fn finalize(&mut self) -> MetricResult {
        let mut entries: Vec<MetricEntry> = self
            .counts
            .drain()
            .filter(|(_, c)| c.fan_in + c.fan_out > 0)
            .map(|(path, c)| {
                let total = c.fan_in + c.fan_out;
                let instability = c
                    .fan_out
                    .saturating_mul(100)
                    .checked_div(total)
                    .unwrap_or(0);
                let role = classify(c.fan_in, c.fan_out);
                let mut values = HashMap::new();
                values.insert("fan_in".into(), MetricValue::Count(c.fan_in));
                values.insert("fan_out".into(), MetricValue::Count(c.fan_out));
                values.insert("instability_pct".into(), MetricValue::Count(instability));
                values.insert("role".into(), MetricValue::Message(role));
                MetricEntry { key: path, values }
            })
            .collect();

        // Sort by fan_in desc — critical files first.
        entries.sort_by(|a, b| {
            let ia = match a.values.get("fan_in") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            let ib = match b.values.get("fan_in") {
                Some(MetricValue::Count(n)) => *n,
                _ => 0,
            };
            ib.cmp(&ia)
        });
        entries.truncate(150);

        MetricResult {
            name: "fan_in_out".into(),
            display_name: report_display("fan_in_out"),
            description: report_description("fan_in_out"),
            entry_groups: vec![],
            columns: vec![
                Column::in_report("fan_in_out", "fan_in"),
                Column::in_report("fan_in_out", "fan_out"),
                Column::in_report("fan_in_out", "instability_pct"),
                Column::in_report("fan_in_out", "role"),
            ],
            entries,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lang {
    Rust,
    Python,
    TypeScript,
}

pub(crate) fn detect_lang(path: &str) -> Option<Lang> {
    let ext = path.rsplit('.').next()?;
    match ext {
        "rs" => Some(Lang::Rust),
        "py" | "pyi" => Some(Lang::Python),
        "ts" | "tsx" | "js" | "jsx" => Some(Lang::TypeScript),
        _ => None,
    }
}

fn classify(fan_in: u64, fan_out: u64) -> LocalizedMessage {
    let code = if fan_in >= 5 && fan_out <= 2 {
        messages::FAN_IN_OUT_ROLE_HUB
    } else if fan_out >= 5 && fan_in <= 1 {
        messages::FAN_IN_OUT_ROLE_ORCHESTRATOR
    } else if fan_in == 0 && fan_out > 0 {
        messages::FAN_IN_OUT_ROLE_LEAF
    } else if fan_out == 0 && fan_in > 0 {
        messages::FAN_IN_OUT_ROLE_PURE_DEP
    } else {
        messages::FAN_IN_OUT_ROLE_MIXED
    };
    LocalizedMessage::code(code)
        .with_param("fan_in", fan_in)
        .with_param("fan_out", fan_out)
}

pub(crate) fn collect_blobs(
    repo: &gix::Repository,
    tree: &gix::Tree,
    prefix: &str,
    out: &mut Vec<(String, gix::ObjectId, u64)>,
) {
    use gix::prelude::HeaderExt;
    for entry_res in tree.iter() {
        let entry = match entry_res {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.filename().to_string();
        let full_path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let id = entry.oid();
        let mode = entry.mode();
        if mode.is_tree() {
            if let Ok(subobj) = repo.find_object(id)
                && let Ok(subtree) = subobj.try_into_tree()
            {
                collect_blobs(repo, &subtree, &full_path, out);
            }
        } else if mode.is_blob() && detect_lang(&full_path).is_some() {
            let size = repo.objects.header(id).map(|h| h.size()).unwrap_or(0);
            out.push((full_path, id.into(), size));
        }
    }
}

// --- Import extraction (regex) ----------------------------------------------------

fn rust_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // captures the path inside `use ...;` — best-effort, ignores braces
        Regex::new(r"^[ \t]*(?:pub\s+)?use\s+([A-Za-z_][\w:]*)").unwrap()
    })
}

fn rust_mod_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^[ \t]*(?:pub\s+)?mod\s+([A-Za-z_]\w*)\s*;").unwrap())
}

fn python_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^[ \t]*(?:from\s+([\w.]+)\s+import|import\s+([\w.]+))").unwrap())
}

fn ts_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?:from|require\()\s*['"]([^'"]+)['"]"#).unwrap())
}

pub(crate) fn extract_imports(lang: Lang, source: &str) -> Vec<String> {
    let mut out = vec![];
    match lang {
        Lang::Rust => {
            for line in source.lines() {
                if let Some(c) = rust_re().captures(line)
                    && let Some(m) = c.get(1)
                {
                    out.push(m.as_str().to_string());
                }
                if let Some(c) = rust_mod_re().captures(line)
                    && let Some(m) = c.get(1)
                {
                    // record as a "mod foo" hint with marker prefix "mod:"
                    out.push(format!("mod:{}", m.as_str()));
                }
            }
        }
        Lang::Python => {
            for line in source.lines() {
                if let Some(c) = python_re().captures(line) {
                    if let Some(m) = c.get(1) {
                        out.push(m.as_str().to_string());
                    } else if let Some(m) = c.get(2) {
                        out.push(m.as_str().to_string());
                    }
                }
            }
        }
        Lang::TypeScript => {
            for c in ts_re().captures_iter(source) {
                if let Some(m) = c.get(1) {
                    out.push(m.as_str().to_string());
                }
            }
        }
    }
    out
}

// --- Import resolution ------------------------------------------------------------

pub(crate) fn resolve_import(
    lang: Lang,
    raw: &str,
    importer: &str,
    paths: &HashSet<String>,
) -> Option<String> {
    match lang {
        Lang::Rust => resolve_rust(raw, importer, paths),
        Lang::Python => resolve_python(raw, paths),
        Lang::TypeScript => resolve_ts(raw, importer, paths),
    }
}

/// Try to resolve a Rust import path like `crate::foo::bar` or `mod:foo` to a file in the repo.
fn resolve_rust(raw: &str, importer: &str, paths: &HashSet<String>) -> Option<String> {
    if let Some(name) = raw.strip_prefix("mod:") {
        // `mod foo;` — sibling file in importer's directory
        let dir = importer.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        for cand in candidate_rust_paths(dir, name) {
            if paths.contains(&cand) {
                return Some(cand);
            }
        }
        return None;
    }
    // Strip leading `crate::`, `self::`, `super::` — best effort
    let trimmed = raw
        .strip_prefix("crate::")
        .or_else(|| raw.strip_prefix("self::"))
        .or_else(|| raw.strip_prefix("super::"))
        .unwrap_or(raw);

    // External crate? Skip.
    if !raw.starts_with("crate::") && !raw.starts_with("self::") && !raw.starts_with("super::") {
        return None;
    }

    let parts: Vec<&str> = trimmed.split("::").collect();
    if parts.is_empty() {
        return None;
    }
    // Walk longest prefix down to shortest, trying to resolve to src/<parts>.rs or .../mod.rs
    for take in (1..=parts.len()).rev() {
        let joined = parts[..take].join("/");
        let cands = [format!("src/{joined}.rs"), format!("src/{joined}/mod.rs")];
        for c in cands {
            if paths.contains(&c) {
                return Some(c);
            }
        }
    }
    None
}

fn candidate_rust_paths(dir: &str, name: &str) -> Vec<String> {
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    vec![
        format!("{prefix}{name}.rs"),
        format!("{prefix}{name}/mod.rs"),
    ]
}

fn resolve_python(raw: &str, paths: &HashSet<String>) -> Option<String> {
    let dotted = raw.trim_start_matches('.');
    if dotted.is_empty() {
        return None;
    }
    let slashed = dotted.replace('.', "/");
    let candidates = [
        format!("{slashed}.py"),
        format!("{slashed}/__init__.py"),
        format!("src/{slashed}.py"),
        format!("src/{slashed}/__init__.py"),
    ];
    candidates.into_iter().find(|c| paths.contains(c))
}

fn resolve_ts(raw: &str, importer: &str, paths: &HashSet<String>) -> Option<String> {
    if !raw.starts_with('.') {
        return None; // bare/external module
    }
    let importer_dir = importer.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let combined = if importer_dir.is_empty() {
        raw.to_string()
    } else {
        format!("{importer_dir}/{raw}")
    };
    let normalized = normalize_path(&combined);

    let exts = ["ts", "tsx", "js", "jsx"];
    for ext in &exts {
        let cand = format!("{normalized}.{ext}");
        if paths.contains(&cand) {
            return Some(cand);
        }
    }
    for ext in &exts {
        let cand = format!("{normalized}/index.{ext}");
        if paths.contains(&cand) {
            return Some(cand);
        }
    }
    if paths.contains(&normalized) {
        return Some(normalized);
    }
    None
}

/// Collapse `./` and `../` segments in a slash-separated path.
fn normalize_path(p: &str) -> String {
    let mut out: Vec<&str> = vec![];
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rust_use_crate_resolves() {
        let p = paths(&["src/foo/bar.rs", "src/lib.rs"]);
        let r = resolve_rust("crate::foo::bar::Thing", "src/lib.rs", &p);
        assert_eq!(r.as_deref(), Some("src/foo/bar.rs"));
    }

    #[test]
    fn rust_external_use_skipped() {
        let p = paths(&["src/lib.rs"]);
        assert!(resolve_rust("anyhow::Result", "src/lib.rs", &p).is_none());
    }

    #[test]
    fn rust_mod_decl_resolves_sibling() {
        let p = paths(&["src/parser/registry.rs", "src/parser/mod.rs"]);
        let r = resolve_rust("mod:registry", "src/parser/mod.rs", &p);
        assert_eq!(r.as_deref(), Some("src/parser/registry.rs"));
    }

    #[test]
    fn python_dotted_resolves() {
        let p = paths(&["a/b.py", "a/__init__.py"]);
        assert_eq!(resolve_python("a.b", &p).as_deref(), Some("a/b.py"));
    }

    #[test]
    fn ts_relative_resolves_with_extension() {
        let p = paths(&["src/lib/foo.ts", "src/index.ts"]);
        let r = resolve_ts("./lib/foo", "src/index.ts", &p);
        assert_eq!(r.as_deref(), Some("src/lib/foo.ts"));
    }

    #[test]
    fn ts_external_skipped() {
        let p = paths(&["src/index.ts"]);
        assert!(resolve_ts("react", "src/index.ts", &p).is_none());
    }

    #[test]
    fn extract_rust_uses() {
        let src = "use crate::a::b::C;\nuse std::collections::HashMap;\nmod foo;\n";
        let imps = extract_imports(Lang::Rust, src);
        assert!(imps.iter().any(|s| s == "crate::a::b::C"));
        assert!(imps.iter().any(|s| s == "std::collections::HashMap"));
        assert!(imps.iter().any(|s| s == "mod:foo"));
    }

    #[test]
    fn classify_hub_vs_orchestrator() {
        assert_eq!(classify(10, 1).code, messages::FAN_IN_OUT_ROLE_HUB);
        assert_eq!(classify(0, 8).code, messages::FAN_IN_OUT_ROLE_ORCHESTRATOR);
        assert_eq!(classify(0, 0).code, messages::FAN_IN_OUT_ROLE_MIXED);
    }

    #[test]
    fn python_plain_import_resolves() {
        let p = paths(&["pkg/mod.py", "pkg/__init__.py"]);
        assert_eq!(resolve_python("pkg.mod", &p).as_deref(), Some("pkg/mod.py"));
    }

    #[test]
    fn python_init_module_resolves() {
        let p = paths(&["pkg/__init__.py"]);
        assert_eq!(
            resolve_python("pkg", &p).as_deref(),
            Some("pkg/__init__.py")
        );
    }

    #[test]
    fn ts_deep_relative_resolves() {
        // importer src/a/b/x.ts; import "../../c/leaf" → src/c/leaf.ts
        let p = paths(&["src/c/leaf.ts", "src/a/b/x.ts"]);
        assert_eq!(
            resolve_ts("../../c/leaf", "src/a/b/x.ts", &p).as_deref(),
            Some("src/c/leaf.ts")
        );
    }

    #[test]
    fn ts_index_file_resolves() {
        let p = paths(&["lib/utils/index.ts"]);
        assert_eq!(
            resolve_ts("./utils", "lib/main.ts", &p).as_deref(),
            Some("lib/utils/index.ts")
        );
    }

    #[test]
    fn normalize_path_collapses_dots() {
        assert_eq!(normalize_path("a/./b"), "a/b");
        assert_eq!(normalize_path("a/b/../c"), "a/c");
        assert_eq!(normalize_path("./a"), "a");
    }

    #[test]
    fn extract_python_imports_both_forms() {
        let src = "from a.b import x\nimport c.d\n# import not_a_real comment\n";
        let imps = extract_imports(Lang::Python, src);
        assert!(imps.iter().any(|s| s == "a.b"));
        assert!(imps.iter().any(|s| s == "c.d"));
    }

    #[test]
    fn extract_ts_handles_require_and_import() {
        let src = "import x from './foo';\nconst y = require('./bar');\n";
        let imps = extract_imports(Lang::TypeScript, src);
        assert!(imps.iter().any(|s| s == "./foo"));
        assert!(imps.iter().any(|s| s == "./bar"));
    }

    #[test]
    fn rust_use_picks_longest_resolvable_prefix() {
        // `crate::a::b::Symbol` should resolve to src/a/b.rs even though Symbol isn't a file
        let p = paths(&["src/a/b.rs", "src/a.rs"]);
        let r = resolve_rust("crate::a::b::Symbol", "src/lib.rs", &p);
        assert_eq!(r.as_deref(), Some("src/a/b.rs"));
    }
}
