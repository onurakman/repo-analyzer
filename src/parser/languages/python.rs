use tree_sitter::{Language, Node, Query};

use crate::types::CodeConstruct;

/// Returns the tree-sitter Language for Python.
pub fn language() -> Language {
    tree_sitter_python::LANGUAGE.into()
}

/// Returns an empty query — we use programmatic AST walking instead of queries.
pub fn query() -> Query {
    Query::new(&language(), "").expect("empty query should always be valid")
}

/// Walks the AST rooted at `root` and extracts code constructs from Python source.
pub fn map_constructs(root: &Node, source: &str) -> Vec<CodeConstruct> {
    let mut constructs = Vec::new();
    walk_node(root, source, None, &mut constructs);
    constructs
}

/// Helper to extract the text of a node from source.
fn node_text<'a>(node: &Node, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

/// Recursively walks the AST, extracting constructs.
/// `current_class` tracks whether we are inside a class (and its name).
fn walk_node(
    node: &Node,
    source: &str,
    current_class: Option<&str>,
    constructs: &mut Vec<CodeConstruct>,
) {
    match node.kind() {
        "function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(&name_node, source).to_string();
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;

                if let Some(class_name) = current_class {
                    constructs.push(CodeConstruct::Method {
                        name,
                        start_line,
                        end_line,
                        enclosing: Some(class_name.to_string()),
                    });
                } else {
                    constructs.push(CodeConstruct::Function {
                        name,
                        start_line,
                        end_line,
                        enclosing: None,
                    });
                }
            }

            // Walk children but keep the same class context.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_node(&child, source, current_class, constructs);
            }
            return;
        }
        "class_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(&name_node, source).to_string();
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;
                constructs.push(CodeConstruct::Class {
                    name: name.clone(),
                    start_line,
                    end_line,
                });

                // Walk children with class context.
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    walk_node(&child, source, Some(&name), constructs);
                }
                return;
            }
        }
        "lambda" if node.is_named() => {
            let start_line = node.start_position().row as u32 + 1;
            let end_line = node.end_position().row as u32 + 1;
            constructs.push(CodeConstruct::Closure {
                start_line,
                end_line,
                enclosing: current_class.map(|s| s.to_string()),
            });
            // Return to avoid walking children — the `lambda` keyword token
            // inside the lambda node also has kind "lambda".
            return;
        }
        _ => {}
    }

    // Walk children (unless we already returned).
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_node(&child, source, current_class, constructs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_and_map(source: &str) -> Vec<CodeConstruct> {
        let mut parser = Parser::new();
        parser
            .set_language(&language())
            .expect("Error loading Python parser");
        let tree = parser.parse(source, None).unwrap();
        map_constructs(&tree.root_node(), source)
    }

    #[test]
    fn test_function() {
        let source = "def greet(name):\n    print(name)\n";
        let constructs = parse_and_map(source);
        let funcs: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "function").collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name(), "greet");
    }

    #[test]
    fn test_class_with_methods() {
        let source = r#"
class Animal:
    def __init__(self, name):
        self.name = name

    def speak(self):
        return self.name
"#;
        let constructs = parse_and_map(source);

        let classes: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "class").collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name(), "Animal");

        let methods: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "method").collect();
        assert_eq!(methods.len(), 2);

        let method_names: Vec<String> = methods.iter().map(|m| m.qualified_name()).collect();
        assert!(method_names.contains(&"Animal::__init__".to_string()));
        assert!(method_names.contains(&"Animal::speak".to_string()));
    }

    #[test]
    fn test_lambda() {
        let source = "f = lambda x: x + 1\n";
        let constructs = parse_and_map(source);
        let closures: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "closure").collect();
        assert_eq!(closures.len(), 1);
    }
}
