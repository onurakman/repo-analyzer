//! Translation catalog for [`crate::types::LocalizedMessage`].
//!
//! Locale JSON files live in `/locales/<code>.json` and are bundled into the
//! binary with [`include_str!`]. `en.json` is the source of truth. Currently
//! `en` is the only bundled locale (see [`SUPPORTED_LOCALES`]); any other code
//! requested via `--locale` falls back to `en` with a warning on stderr.
//! Additional locales are expected to be translated copies with identical keys.
//!
//! The catalog is a flat map `code → template`, where `template` may contain
//! `{{param_name}}` placeholders that [`Catalog::translate`] substitutes from
//! the message's `params` map. Missing keys render the code verbatim — that
//! keeps failures visible instead of silent.

use std::collections::HashMap;

use crate::types::LocalizedMessage;

const EN_JSON: &str = include_str!("../locales/en.json");

/// Locale codes that ship with a bundled catalog. Any value passed to
/// [`Catalog::load`] that is not listed here falls back to `en`. This is the
/// single source of truth for the set of locales the CLI actually supports, so
/// `--locale` help text and validation can reference it instead of drifting.
pub const SUPPORTED_LOCALES: &[&str] = &["en"];

/// Whether `locale` has a bundled catalog (i.e. is honoured rather than
/// silently falling back to `en`).
pub fn is_supported(locale: &str) -> bool {
    SUPPORTED_LOCALES.contains(&locale)
}

/// Flat `code → template` translation map for one locale.
#[derive(Debug, Clone)]
pub struct Catalog {
    entries: HashMap<String, String>,
}

impl Catalog {
    /// Load a bundled locale by its short code. Unknown codes fall back to
    /// `en` and emit a warning on stderr so the fallback is visible rather than
    /// silent. Returns a catalog even on malformed JSON (empty map) so the CLI
    /// never panics on startup — missing keys still surface via the code
    /// fallback in [`Catalog::translate`].
    ///
    /// The warning is unconditional (no `--quiet` is reachable here); it goes to
    /// stderr only, so it never contaminates stdout output such as JSON or CSV.
    pub fn load(locale: &str) -> Self {
        let raw = if is_supported(locale) {
            match locale {
                "en" => EN_JSON,
                // Every code in `SUPPORTED_LOCALES` must have a matching arm.
                _ => EN_JSON,
            }
        } else {
            eprintln!(
                "warning: unsupported locale {locale:?}; falling back to \"en\" \
                 (supported locales: {})",
                SUPPORTED_LOCALES.join(", ")
            );
            EN_JSON
        };
        let entries: HashMap<String, String> = serde_json::from_str(raw).unwrap_or_default();
        Self { entries }
    }

    /// Translate one message: look up the code, substitute `{{param}}`
    /// placeholders, or return the code itself if missing.
    pub fn translate(&self, msg: &LocalizedMessage) -> String {
        let template = match self.entries.get(&msg.code) {
            Some(s) => s.clone(),
            None => return msg.code.clone(),
        };
        if msg.params.is_empty() {
            return template;
        }
        let mut out = template;
        for (key, value) in &msg.params {
            let placeholder = format!("{{{{{key}}}}}");
            let rendered = render_param(value);
            out = out.replace(&placeholder, &rendered);
        }
        out
    }

    /// Translate a code with no params. Convenience for static labels.
    pub fn translate_code(&self, code: &str) -> String {
        match self.entries.get(code) {
            Some(s) => s.clone(),
            None => code.to_string(),
        }
    }
}

/// Format a `serde_json::Value` for inline display inside a translated string.
/// Strings are unwrapped (no quotes), everything else uses the JSON repr.
fn render_param(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Severity;

    #[test]
    fn missing_code_renders_code_itself() {
        let cat = Catalog {
            entries: HashMap::new(),
        };
        let msg = LocalizedMessage::code("nonexistent.code");
        assert_eq!(cat.translate(&msg), "nonexistent.code");
    }

    #[test]
    fn substitutes_params() {
        let mut entries = HashMap::new();
        entries.insert(
            "test.msg".to_string(),
            "size is {{size_bytes}} bytes".to_string(),
        );
        let cat = Catalog { entries };
        let msg = LocalizedMessage::code("test.msg")
            .with_severity(Severity::Warning)
            .with_param("size_bytes", 1024_u64);
        assert_eq!(cat.translate(&msg), "size is 1024 bytes");
    }

    #[test]
    fn string_params_render_without_quotes() {
        let mut entries = HashMap::new();
        entries.insert("t".into(), "hello {{name}}".into());
        let cat = Catalog { entries };
        let msg = LocalizedMessage::code("t").with_param("name", "world");
        assert_eq!(cat.translate(&msg), "hello world");
    }

    #[test]
    fn loads_bundled_en_catalog() {
        let cat = Catalog::load("en");
        // Catalog should at least parse without panicking.
        let _ = cat.translate_code("anything");
    }

    #[test]
    fn supported_locales_report_en_only() {
        assert!(is_supported("en"));
        assert!(!is_supported("tr"));
        assert!(!is_supported("does-not-exist"));
        // Every advertised locale must actually be loadable.
        assert!(SUPPORTED_LOCALES.contains(&"en"));
    }

    #[test]
    fn unknown_locale_falls_back_to_en_catalog() {
        // Falls back to the en bundle instead of an empty map: the same known
        // code resolves identically for `en` and for an unsupported locale.
        let en = Catalog::load("en");
        let fallback = Catalog::load("xx-unknown");
        let probe = en
            .entries
            .keys()
            .next()
            .expect("bundled en catalog should not be empty");
        assert_eq!(en.translate_code(probe), fallback.translate_code(probe));
    }
}
