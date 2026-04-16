use std::collections::HashMap;
use tree_sitter::{Language, Node, Parser, Query};

use crate::types::CodeConstruct;

/// Configuration for a supported language.
pub struct LanguageConfig {
    #[allow(dead_code)]
    pub name: &'static str,
    pub language: Language,
    #[allow(dead_code)]
    pub query: Query,
    pub construct_mapper: fn(&Node, &str) -> Vec<CodeConstruct>,
}

/// Registry that maps file extensions to language configurations.
pub struct LanguageRegistry {
    by_extension: HashMap<String, usize>, // ext → index into configs
    configs: Vec<LanguageConfig>,
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self {
            by_extension: HashMap::new(),
            configs: Vec::new(),
        }
    }

    /// Registers a language configuration for the given file extensions.
    pub fn register(&mut self, extensions: &[&str], config: LanguageConfig) {
        let index = self.configs.len();
        self.configs.push(config);
        for ext in extensions {
            self.by_extension.insert(ext.to_string(), index);
        }
    }

    /// Looks up the language config for a file by its extension.
    pub fn get_for_file(&self, file_path: &str) -> Option<&LanguageConfig> {
        let ext = file_path.rsplit('.').next()?;
        let index = self.by_extension.get(ext)?;
        self.configs.get(*index)
    }

    /// Parses the source and extracts code constructs for the given file.
    pub fn parse_constructs(&self, file_path: &str, source: &str) -> Option<Vec<CodeConstruct>> {
        let config = self.get_for_file(file_path)?;
        let mut parser = Parser::new();
        parser.set_language(&config.language).ok()?;
        let tree = parser.parse(source, None)?;
        let root = tree.root_node();
        Some((config.construct_mapper)(&root, source))
    }

    /// Parses the source and extracts only constructs that overlap with the given line ranges.
    /// Line ranges are 1-based inclusive: (start_line, end_line).
    pub fn parse_constructs_in_ranges(
        &self,
        file_path: &str,
        source: &str,
        line_ranges: &[(u32, u32)],
    ) -> Option<Vec<CodeConstruct>> {
        let all = self.parse_constructs(file_path, source)?;
        let filtered = all
            .into_iter()
            .filter(|c| {
                let (cs, ce) = c.line_range();
                line_ranges.iter().any(|&(rs, re)| cs <= re && rs <= ce)
            })
            .collect();
        Some(filtered)
    }

    /// Returns the number of registered languages.
    #[allow(dead_code)]
    pub fn language_count(&self) -> usize {
        self.configs.len()
    }

    /// Returns all registered file extensions.
    #[allow(dead_code)]
    pub fn extensions(&self) -> Vec<&str> {
        self.by_extension.keys().map(|s| s.as_str()).collect()
    }

    /// Builds the default registry with all supported languages.
    pub fn build_default() -> Self {
        use super::languages::bash;
        use super::languages::cpp;
        use super::languages::csharp;
        use super::languages::css;
        use super::languages::go_lang;
        use super::languages::html_lang;
        use super::languages::java;
        use super::languages::kotlin;
        use super::languages::php;
        use super::languages::python;
        use super::languages::ruby;
        use super::languages::rust_lang;
        use super::languages::scala;
        use super::languages::swift;
        use super::languages::typescript;

        let mut registry = Self::new();

        registry.register(
            &["rs"],
            LanguageConfig {
                name: "Rust",
                language: rust_lang::language(),
                query: rust_lang::query(),
                construct_mapper: rust_lang::map_constructs,
            },
        );

        registry.register(
            &["ts", "tsx", "js", "jsx"],
            LanguageConfig {
                name: "TypeScript",
                language: typescript::language(),
                query: typescript::query(),
                construct_mapper: typescript::map_constructs,
            },
        );

        registry.register(
            &["py", "pyi"],
            LanguageConfig {
                name: "Python",
                language: python::language(),
                query: python::query(),
                construct_mapper: python::map_constructs,
            },
        );

        registry.register(
            &["java"],
            LanguageConfig {
                name: "Java",
                language: java::language(),
                query: java::query(),
                construct_mapper: java::map_constructs,
            },
        );

        registry.register(
            &["go"],
            LanguageConfig {
                name: "Go",
                language: go_lang::language(),
                query: go_lang::query(),
                construct_mapper: go_lang::map_constructs,
            },
        );

        registry.register(
            &["cpp", "cc", "cxx", "hpp", "h"],
            LanguageConfig {
                name: "C++",
                language: cpp::language(),
                query: cpp::query(),
                construct_mapper: cpp::map_constructs,
            },
        );

        registry.register(
            &["cs"],
            LanguageConfig {
                name: "C#",
                language: csharp::language(),
                query: csharp::query(),
                construct_mapper: csharp::map_constructs,
            },
        );

        registry.register(
            &["kt", "kts"],
            LanguageConfig {
                name: "Kotlin",
                language: kotlin::language(),
                query: kotlin::query(),
                construct_mapper: kotlin::map_constructs,
            },
        );

        registry.register(
            &["php"],
            LanguageConfig {
                name: "PHP",
                language: php::language(),
                query: php::query(),
                construct_mapper: php::map_constructs,
            },
        );

        registry.register(
            &["rb"],
            LanguageConfig {
                name: "Ruby",
                language: ruby::language(),
                query: ruby::query(),
                construct_mapper: ruby::map_constructs,
            },
        );

        registry.register(
            &["html", "htm"],
            LanguageConfig {
                name: "HTML",
                language: html_lang::language(),
                query: html_lang::query(),
                construct_mapper: html_lang::map_constructs,
            },
        );

        registry.register(
            &["css", "scss"],
            LanguageConfig {
                name: "CSS",
                language: css::language(),
                query: css::query(),
                construct_mapper: css::map_constructs,
            },
        );

        registry.register(
            &["sh", "bash"],
            LanguageConfig {
                name: "Bash",
                language: bash::language(),
                query: bash::query(),
                construct_mapper: bash::map_constructs,
            },
        );

        registry.register(
            &["scala", "sc"],
            LanguageConfig {
                name: "Scala",
                language: scala::language(),
                query: scala::query(),
                construct_mapper: scala::map_constructs,
            },
        );

        registry.register(
            &["swift"],
            LanguageConfig {
                name: "Swift",
                language: swift::language(),
                query: swift::query(),
                construct_mapper: swift::map_constructs,
            },
        );

        registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::languages::rust_lang;

    #[test]
    fn test_register_and_lookup() {
        let mut registry = LanguageRegistry::new();
        registry.register(
            &["rs"],
            LanguageConfig {
                name: "Rust",
                language: rust_lang::language(),
                query: rust_lang::query(),
                construct_mapper: rust_lang::map_constructs,
            },
        );

        assert!(registry.get_for_file("src/main.rs").is_some());
        assert_eq!(registry.get_for_file("src/main.rs").unwrap().name, "Rust");
        assert!(registry.get_for_file("src/main.py").is_none());
    }

    #[test]
    fn test_parse_constructs() {
        let mut registry = LanguageRegistry::new();
        registry.register(
            &["rs"],
            LanguageConfig {
                name: "Rust",
                language: rust_lang::language(),
                query: rust_lang::query(),
                construct_mapper: rust_lang::map_constructs,
            },
        );

        let source = "fn main() {}";
        let constructs = registry.parse_constructs("test.rs", source);
        assert!(constructs.is_some());
        let constructs = constructs.unwrap();
        assert_eq!(constructs.len(), 1);
        assert_eq!(constructs[0].kind_str(), "function");
        assert_eq!(constructs[0].name(), "main");
    }

    #[test]
    fn test_unknown_extension_returns_none() {
        let registry = LanguageRegistry::new();
        assert!(registry.get_for_file("unknown.xyz").is_none());
        assert!(
            registry
                .parse_constructs("unknown.xyz", "some code")
                .is_none()
        );
    }
}
