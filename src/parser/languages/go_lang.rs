use tree_sitter::{Language, Node, Query};

use crate::types::CodeConstruct;

/// Returns the tree-sitter Language for Go.
pub fn language() -> Language {
    tree_sitter_go::LANGUAGE.into()
}

/// Returns an empty query — we use programmatic AST walking instead of queries.
pub fn query() -> Query {
    Query::new(&language(), "").expect("empty query should always be valid")
}

/// Walks the AST rooted at `root` and extracts code constructs from Go source.
pub fn map_constructs(root: &Node, source: &str) -> Vec<CodeConstruct> {
    let mut constructs = Vec::new();
    walk_node(root, source, &mut constructs);
    constructs
}

/// Helper to extract the text of a node from source.
fn node_text<'a>(node: &Node, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

/// Recursively walks the AST, extracting constructs.
fn walk_node(node: &Node, source: &str, constructs: &mut Vec<CodeConstruct>) {
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
        "method_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(&name_node, source).to_string();
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;

                // Extract receiver type name.
                let receiver = node.child_by_field_name("receiver").and_then(|params| {
                    // The receiver is a parameter_list; find the type inside.
                    let mut cursor = params.walk();
                    for child in params.children(&mut cursor) {
                        if child.kind() == "parameter_declaration"
                            && let Some(type_node) = child.child_by_field_name("type")
                        {
                            // Could be a pointer_type like *Foo
                            let type_text = node_text(&type_node, source);
                            let clean = type_text.trim_start_matches('*');
                            return Some(clean.to_string());
                        }
                    }
                    None
                });

                constructs.push(CodeConstruct::Method {
                    name,
                    start_line,
                    end_line,
                    enclosing: receiver,
                });
            }
        }
        "type_declaration" => {
            // type_declaration contains type_spec children.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "type_spec"
                    && let Some(name_node) = child.child_by_field_name("name")
                {
                    let name = node_text(&name_node, source).to_string();
                    let start_line = node.start_position().row as u32 + 1;
                    let end_line = node.end_position().row as u32 + 1;

                    if let Some(type_node) = child.child_by_field_name("type") {
                        match type_node.kind() {
                            "struct_type" => {
                                constructs.push(CodeConstruct::Struct {
                                    name,
                                    start_line,
                                    end_line,
                                });
                            }
                            "interface_type" => {
                                constructs.push(CodeConstruct::Interface {
                                    name,
                                    start_line,
                                    end_line,
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
            return; // Already walked children.
        }
        _ => {}
    }

    // Walk children.
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
            .expect("Error loading Go parser");
        let tree = parser.parse(source, None).unwrap();
        map_constructs(&tree.root_node(), source)
    }

    #[test]
    fn test_function() {
        let source = r#"
package main

func greet(name string) {
    fmt.Println(name)
}
"#;
        let constructs = parse_and_map(source);
        let funcs: Vec<_> = constructs
            .iter()
            .filter(|c| c.kind_str() == "function")
            .collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name(), "greet");
    }

    #[test]
    fn test_struct() {
        let source = r#"
package main

type Point struct {
    X float64
    Y float64
}
"#;
        let constructs = parse_and_map(source);
        let structs: Vec<_> = constructs
            .iter()
            .filter(|c| c.kind_str() == "struct")
            .collect();
        assert_eq!(structs.len(), 1);
        assert_eq!(structs[0].name(), "Point");
    }

    #[test]
    fn test_method_with_receiver() {
        let source = r#"
package main

type Point struct {
    X float64
    Y float64
}

func (p *Point) Move(dx float64, dy float64) {
    p.X += dx
    p.Y += dy
}
"#;
        let constructs = parse_and_map(source);

        let methods: Vec<_> = constructs
            .iter()
            .filter(|c| c.kind_str() == "method")
            .collect();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].name(), "Move");
        assert_eq!(methods[0].qualified_name(), "Point::Move");
    }
}
