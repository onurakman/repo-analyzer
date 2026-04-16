use tree_sitter::{Language, Node, Query};

use crate::types::CodeConstruct;

/// Returns the tree-sitter Language for TypeScript.
pub fn language() -> Language {
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}

/// Returns an empty query — we use programmatic AST walking instead of queries.
pub fn query() -> Query {
    Query::new(&language(), "").expect("empty query should always be valid")
}

/// Walks the AST rooted at `root` and extracts code constructs from TypeScript source.
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
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(&name_node, source).to_string();
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;
                constructs.push(CodeConstruct::Function {
                    name,
                    start_line,
                    end_line,
                    enclosing: current_class.map(|s| s.to_string()),
                });
            }
        }
        "method_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(&name_node, source).to_string();
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;
                if current_class.is_some() {
                    constructs.push(CodeConstruct::Method {
                        name,
                        start_line,
                        end_line,
                        enclosing: current_class.map(|s| s.to_string()),
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
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(&name_node, source).to_string();
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;
                constructs.push(CodeConstruct::Class {
                    name: name.clone(),
                    start_line,
                    end_line,
                });

                // Walk children with class context, then return to avoid double-walking.
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
                    name,
                    start_line,
                    end_line,
                });
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
        "arrow_function" | "function_expression" => {
            let start_line = node.start_position().row as u32 + 1;
            let end_line = node.end_position().row as u32 + 1;
            constructs.push(CodeConstruct::Closure {
                start_line,
                end_line,
                enclosing: current_class.map(|s| s.to_string()),
            });
        }
        "module" => {
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

    // Walk children (unless we already returned from class_declaration).
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
            .expect("Error loading TypeScript parser");
        let tree = parser.parse(source, None).unwrap();
        map_constructs(&tree.root_node(), source)
    }

    #[test]
    fn test_function() {
        let source = "function greet(name: string): void { console.log(name); }";
        let constructs = parse_and_map(source);
        let funcs: Vec<_> = constructs
            .iter()
            .filter(|c| c.kind_str() == "function")
            .collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name(), "greet");
    }

    #[test]
    fn test_class_with_methods() {
        let source = r#"
class Animal {
    constructor(name: string) {
        this.name = name;
    }
    speak() {
        return this.name;
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
        assert!(methods.len() >= 2, "should find constructor and speak");
    }

    #[test]
    fn test_interface() {
        let source = "interface Shape { area(): number; perimeter(): number; }";
        let constructs = parse_and_map(source);
        let interfaces: Vec<_> = constructs
            .iter()
            .filter(|c| c.kind_str() == "interface")
            .collect();
        assert_eq!(interfaces.len(), 1);
        assert_eq!(interfaces[0].name(), "Shape");
    }

    #[test]
    fn test_enum() {
        let source = "enum Direction { Up, Down, Left, Right }";
        let constructs = parse_and_map(source);
        let enums: Vec<_> = constructs
            .iter()
            .filter(|c| c.kind_str() == "enum")
            .collect();
        assert_eq!(enums.len(), 1);
        assert_eq!(enums[0].name(), "Direction");
    }

    #[test]
    fn test_arrow_function() {
        let source = "const add = (a: number, b: number) => a + b;";
        let constructs = parse_and_map(source);
        let closures: Vec<_> = constructs
            .iter()
            .filter(|c| c.kind_str() == "closure")
            .collect();
        assert_eq!(closures.len(), 1);
    }
}
