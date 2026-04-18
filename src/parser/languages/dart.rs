use tree_sitter::{Language, Node, Query};

use crate::types::CodeConstruct;

/// Returns the tree-sitter Language for Dart.
pub fn language() -> Language {
    tree_sitter_dart::LANGUAGE.into()
}

/// Returns an empty query — we use programmatic AST walking instead of queries.
pub fn query() -> Query {
    Query::new(&language(), "").expect("empty query should always be valid")
}

/// Walks the AST rooted at `root` and extracts code constructs from Dart source.
pub fn map_constructs(root: &Node, source: &str) -> Vec<CodeConstruct> {
    let mut constructs = Vec::new();
    walk_node(root, source, None, &mut constructs);
    constructs
}

fn node_text<'a>(node: &Node, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

/// In Dart, a `function_signature` / `method_signature` covers only the
/// signature line. The enclosing `declaration` (or `class_member`) node
/// covers the body too, which is what we want for span metrics.
fn span_with_body<'a>(node: &Node<'a>) -> Node<'a> {
    node.parent().unwrap_or(*node)
}

fn first_identifier_child(node: &Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return Some(node_text(&child, source).to_string());
        }
    }
    None
}

fn extract_name(node: &Node, source: &str) -> Option<String> {
    node.child_by_field_name("name")
        .map(|n| node_text(&n, source).to_string())
        .or_else(|| first_identifier_child(node, source))
}

fn walk_node(
    node: &Node,
    source: &str,
    current_class: Option<&str>,
    constructs: &mut Vec<CodeConstruct>,
) {
    match node.kind() {
        "class_declaration" | "mixin_application_class" => {
            if let Some(name) = extract_name(node, source) {
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;
                constructs.push(CodeConstruct::Class {
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
        "mixin_declaration" => {
            if let Some(name) = extract_name(node, source) {
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;
                constructs.push(CodeConstruct::Class {
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
        "extension_declaration" => {
            // Dart extensions may be unnamed — skip those.
            if let Some(name) = extract_name(node, source) {
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;
                constructs.push(CodeConstruct::Class {
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
            if let Some(name) = extract_name(node, source) {
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;
                constructs.push(CodeConstruct::Enum {
                    name,
                    start_line,
                    end_line,
                });
            }
        }
        "function_signature" | "getter_signature" | "setter_signature" => {
            if let Some(name) = extract_name(node, source) {
                let span_node = span_with_body(node);
                let start_line = span_node.start_position().row as u32 + 1;
                let end_line = span_node.end_position().row as u32 + 1;
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
        "method_signature" => {
            if let Some(name) = extract_name(node, source) {
                let span_node = span_with_body(node);
                let start_line = span_node.start_position().row as u32 + 1;
                let end_line = span_node.end_position().row as u32 + 1;
                constructs.push(CodeConstruct::Method {
                    name,
                    start_line,
                    end_line,
                    enclosing: current_class.map(|s| s.to_string()),
                });
            }
        }
        "function_expression" | "lambda_expression" => {
            let start_line = node.start_position().row as u32 + 1;
            let end_line = node.end_position().row as u32 + 1;
            constructs.push(CodeConstruct::Closure {
                start_line,
                end_line,
                enclosing: current_class.map(|s| s.to_string()),
            });
        }
        _ => {}
    }

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
            .expect("Error loading Dart parser");
        let tree = parser.parse(source, None).unwrap();
        map_constructs(&tree.root_node(), source)
    }

    #[test]
    fn test_top_level_function() {
        let source = "int square(int x) { return x * x; }\n";
        let constructs = parse_and_map(source);
        let funcs: Vec<_> = constructs
            .iter()
            .filter(|c| c.kind_str() == "function")
            .collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name(), "square");
    }

    #[test]
    fn test_class_with_method() {
        let source = r#"
class Animal {
  String name;
  Animal(this.name);
  String speak() => 'Hello $name';
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
        assert!(!methods.is_empty(), "expected at least one method");
    }

    #[test]
    fn test_enum() {
        let source = "enum Color { red, green, blue }\n";
        let constructs = parse_and_map(source);
        let enums: Vec<_> = constructs
            .iter()
            .filter(|c| c.kind_str() == "enum")
            .collect();
        assert_eq!(enums.len(), 1);
        assert_eq!(enums[0].name(), "Color");
    }
}
