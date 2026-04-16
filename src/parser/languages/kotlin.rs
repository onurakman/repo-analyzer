use tree_sitter::{Language, Node, Query};

use crate::types::CodeConstruct;

/// Returns the tree-sitter Language for Kotlin.
pub fn language() -> Language {
    tree_sitter_kotlin_ng::LANGUAGE.into()
}

/// Returns an empty query — we use programmatic AST walking instead of queries.
pub fn query() -> Query {
    Query::new(&language(), "").expect("empty query should always be valid")
}

/// Walks the AST rooted at `root` and extracts code constructs from Kotlin source.
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
        "function_declaration" => {
            // Find the simple_identifier child that is the function name.
            let name = find_name_child(node, source);
            if let Some(name) = name {
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
        }
        "class_declaration" => {
            let name = find_name_child(node, source);
            if let Some(name) = name {
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;

                // Check if it's an enum class.
                let is_enum = has_modifier(node, source, "enum");

                if is_enum {
                    constructs.push(CodeConstruct::Enum {
                        name: name.clone(),
                        start_line,
                        end_line,
                    });
                } else {
                    constructs.push(CodeConstruct::Class {
                        name: name.clone(),
                        start_line,
                        end_line,
                    });
                }

                // Walk children with class context.
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    walk_node(&child, source, Some(&name), constructs);
                }
                return;
            }
        }
        "object_declaration" => {
            let name = find_name_child(node, source);
            if let Some(name) = name {
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
        "interface_declaration" => {
            let name = find_name_child(node, source);
            if let Some(name) = name {
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;
                constructs.push(CodeConstruct::Interface {
                    name,
                    start_line,
                    end_line,
                });
            }
        }
        _ => {}
    }

    // Walk children (unless we already returned).
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_node(&child, source, current_class, constructs);
    }
}

/// Finds the first `simple_identifier` child of a node (used as the name for Kotlin AST nodes).
fn find_name_child(node: &Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "simple_identifier" || child.kind() == "identifier" {
            return Some(node_text(&child, source).to_string());
        }
    }
    None
}

/// Checks if a node has a `modifiers` child that contains the given modifier keyword.
fn has_modifier(node: &Node, source: &str, modifier: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            let text = node_text(&child, source);
            if text.contains(modifier) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_and_map(source: &str) -> Vec<CodeConstruct> {
        let mut parser = Parser::new();
        parser
            .set_language(&language())
            .expect("Error loading Kotlin parser");
        let tree = parser.parse(source, None).unwrap();
        map_constructs(&tree.root_node(), source)
    }

    #[test]
    fn test_function() {
        let source = "fun greet(name: String) { println(name) }";
        let constructs = parse_and_map(source);
        let funcs: Vec<_> = constructs
            .iter()
            .filter(|c| c.kind_str() == "function")
            .collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name(), "greet");
    }

    #[test]
    fn test_class() {
        let source = r#"
class Animal(val name: String) {
    fun speak(): String {
        return name
    }
}
"#;
        let constructs = parse_and_map(source);

        let classes: Vec<_> = constructs
            .iter()
            .filter(|c| c.kind_str() == "class")
            .collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name(), "Animal");

        let methods: Vec<_> = constructs
            .iter()
            .filter(|c| c.kind_str() == "method")
            .collect();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].name(), "speak");
    }
}
