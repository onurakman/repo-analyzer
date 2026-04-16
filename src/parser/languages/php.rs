use tree_sitter::{Language, Node, Query};

use crate::types::CodeConstruct;

/// Returns the tree-sitter Language for PHP.
pub fn language() -> Language {
    tree_sitter_php::LANGUAGE_PHP.into()
}

/// Returns an empty query — we use programmatic AST walking instead of queries.
pub fn query() -> Query {
    Query::new(&language(), "").expect("empty query should always be valid")
}

/// Walks the AST rooted at `root` and extracts code constructs from PHP source.
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
/// `current_class` tracks whether we are inside a class/interface/trait.
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
                constructs.push(CodeConstruct::Function {
                    name,
                    start_line,
                    end_line,
                    enclosing: None,
                });
            }
        }
        "method_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(&name_node, source).to_string();
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;
                constructs.push(CodeConstruct::Method {
                    name,
                    start_line,
                    end_line,
                    enclosing: current_class.map(|s| s.to_string()),
                });
            }
        }
        "class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(&name_node, source).to_string();
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;
                constructs.push(CodeConstruct::Class {
                    name: name.clone(),
                    start_line,
                    end_line,
                });
                // Walk children with class context
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    walk_node(&child, source, Some(&name), constructs);
                }
                return;
            }
        }
        "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(&name_node, source).to_string();
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;
                constructs.push(CodeConstruct::Interface {
                    name: name.clone(),
                    start_line,
                    end_line,
                });
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    walk_node(&child, source, Some(&name), constructs);
                }
                return;
            }
        }
        "trait_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(&name_node, source).to_string();
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;
                constructs.push(CodeConstruct::Trait {
                    name: name.clone(),
                    start_line,
                    end_line,
                });
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    walk_node(&child, source, Some(&name), constructs);
                }
                return;
            }
        }
        "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(&name_node, source).to_string();
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;
                constructs.push(CodeConstruct::Enum {
                    name,
                    start_line,
                    end_line,
                });
            }
        }
        "namespace_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(&name_node, source).to_string();
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;
                constructs.push(CodeConstruct::Module {
                    name,
                    start_line,
                    end_line,
                });
            }
        }
        _ => {}
    }

    // Walk children (unless we already returned from class/interface/trait).
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
            .expect("Error loading PHP parser");
        let tree = parser.parse(source, None).unwrap();
        map_constructs(&tree.root_node(), source)
    }

    #[test]
    fn test_function() {
        let source = "<?php\nfunction hello() {}";
        let constructs = parse_and_map(source);
        let funcs: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "function").collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name(), "hello");
    }

    #[test]
    fn test_class_with_method() {
        let source = r#"<?php
class Greeter {
    public function greet() {
        echo "Hello";
    }
}
"#;
        let constructs = parse_and_map(source);
        let classes: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "class").collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name(), "Greeter");

        let methods: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "method").collect();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].qualified_name(), "Greeter::greet");
    }

    #[test]
    fn test_interface() {
        let source = "<?php\ninterface Loggable { public function log(); }";
        let constructs = parse_and_map(source);
        let ifaces: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "interface").collect();
        assert_eq!(ifaces.len(), 1);
        assert_eq!(ifaces[0].name(), "Loggable");
    }
}
