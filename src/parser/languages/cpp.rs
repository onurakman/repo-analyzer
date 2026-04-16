use tree_sitter::{Language, Node, Query};

use crate::types::CodeConstruct;

/// Returns the tree-sitter Language for C++.
pub fn language() -> Language {
    tree_sitter_cpp::LANGUAGE.into()
}

/// Returns an empty query — we use programmatic AST walking instead of queries.
pub fn query() -> Query {
    Query::new(&language(), "").expect("empty query should always be valid")
}

/// Walks the AST rooted at `root` and extracts code constructs from C++ source.
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
/// `current_class` tracks whether we are inside a class/struct (and its name).
fn walk_node(
    node: &Node,
    source: &str,
    current_class: Option<&str>,
    constructs: &mut Vec<CodeConstruct>,
) {
    match node.kind() {
        "function_definition" => {
            if let Some(declarator) = node.child_by_field_name("declarator") {
                // The declarator could be a function_declarator wrapping the name.
                let name_node = find_function_name(&declarator, source);
                if let Some(name) = name_node {
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
        }
        "class_specifier" => {
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
        "struct_specifier" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(&name_node, source).to_string();
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;
                constructs.push(CodeConstruct::Struct {
                    name,
                    start_line,
                    end_line,
                });
            }
        }
        "enum_specifier" => {
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

    // Walk children (unless we already returned from class_specifier).
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_node(&child, source, current_class, constructs);
    }
}

/// Extracts the function name from a declarator node (handles nested declarators).
fn find_function_name(node: &Node, source: &str) -> Option<String> {
    match node.kind() {
        "function_declarator" => {
            if let Some(declarator) = node.child_by_field_name("declarator") {
                return find_function_name(&declarator, source);
            }
            None
        }
        "identifier" | "field_identifier" | "destructor_name" => {
            Some(node_text(node, source).to_string())
        }
        "qualified_identifier" => {
            // e.g., Foo::bar — take the whole thing or just the last part
            Some(node_text(node, source).to_string())
        }
        _ => {
            // Try to find an identifier child
            node.child_by_field_name("name")
                .map(|name_node| node_text(&name_node, source).to_string())
        }
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
            .expect("Error loading C++ parser");
        let tree = parser.parse(source, None).unwrap();
        map_constructs(&tree.root_node(), source)
    }

    #[test]
    fn test_function() {
        let source = "int greet(const char* name) { return 0; }";
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
class Animal {
public:
    void speak() { }
};
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

    #[test]
    fn test_namespace() {
        let source = r#"
namespace utils {
    int helper() { return 42; }
}
"#;
        let constructs = parse_and_map(source);
        let modules: Vec<_> = constructs
            .iter()
            .filter(|c| c.kind_str() == "module")
            .collect();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].name(), "utils");
    }
}
