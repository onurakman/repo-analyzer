use tree_sitter::{Language, Node, Query};

use crate::types::CodeConstruct;

/// Returns the tree-sitter Language for Swift.
pub fn language() -> Language {
    tree_sitter_swift::LANGUAGE.into()
}

/// Returns an empty query — we use programmatic AST walking instead of queries.
pub fn query() -> Query {
    Query::new(&language(), "").expect("empty query should always be valid")
}

/// Walks the AST rooted at `root` and extracts code constructs from Swift source.
pub fn map_constructs(root: &Node, source: &str) -> Vec<CodeConstruct> {
    let mut constructs = Vec::new();
    walk_node(root, source, &mut constructs);
    constructs
}

/// Helper to extract the text of a node from source.
fn node_text<'a>(node: &Node, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

/// Finds the first child with kind `type_identifier` and returns its text.
fn find_type_name<'a>(node: &Node, source: &'a str) -> Option<&'a str> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "type_identifier" {
            return Some(node_text(&child, source));
        }
    }
    None
}

/// Checks if the node has a direct child whose kind matches the given keyword.
fn has_keyword_child(node: &Node, keyword: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == keyword {
            return true;
        }
    }
    false
}

/// Recursively walks the AST, extracting constructs.
///
/// In tree-sitter-swift, `class`, `struct`, and `enum` all use `class_declaration`
/// as the node kind. The distinguishing keyword is a child node.
/// The type name is in a `type_identifier` child, not a `name` field.
/// Functions use `function_declaration` with a `name` field of kind `simple_identifier`.
/// Protocols use `protocol_declaration`.
fn walk_node(
    node: &Node,
    source: &str,
    constructs: &mut Vec<CodeConstruct>,
) {
    match node.kind() {
        "function_declaration" => {
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
        "class_declaration" => {
            if let Some(name) = find_type_name(node, source) {
                let name = name.to_string();
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;

                if has_keyword_child(node, "enum") {
                    constructs.push(CodeConstruct::Enum {
                        name,
                        start_line,
                        end_line,
                    });
                } else if has_keyword_child(node, "struct") {
                    constructs.push(CodeConstruct::Struct {
                        name,
                        start_line,
                        end_line,
                    });
                } else {
                    // "class" keyword (the default for class_declaration)
                    constructs.push(CodeConstruct::Class {
                        name,
                        start_line,
                        end_line,
                    });
                }
            }
        }
        "protocol_declaration" => {
            if let Some(name) = find_type_name(node, source) {
                let name = name.to_string();
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

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_node(&child, source, constructs);
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
            .expect("Error loading Swift parser");
        let tree = parser.parse(source, None).unwrap();
        map_constructs(&tree.root_node(), source)
    }

    #[test]
    fn test_function() {
        let source = "func hello() { print(\"hi\") }";
        let constructs = parse_and_map(source);
        let funcs: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "function").collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name(), "hello");
    }

    #[test]
    fn test_class_and_struct() {
        let source = r#"
class Dog {
    var name: String
    init(name: String) { self.name = name }
}

struct Point {
    var x: Double
    var y: Double
}
"#;
        let constructs = parse_and_map(source);
        let classes: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "class").collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name(), "Dog");

        let structs: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "struct").collect();
        assert_eq!(structs.len(), 1);
        assert_eq!(structs[0].name(), "Point");
    }

    #[test]
    fn test_enum() {
        let source = "enum Direction { case north, south, east, west }";
        let constructs = parse_and_map(source);
        let enums: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "enum").collect();
        assert_eq!(enums.len(), 1);
        assert_eq!(enums[0].name(), "Direction");
    }

}
