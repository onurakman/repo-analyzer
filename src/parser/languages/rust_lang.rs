use tree_sitter::{Language, Node, Query};

use crate::types::CodeConstruct;

/// Returns the tree-sitter Language for Rust.
pub fn language() -> Language {
    tree_sitter_rust::LANGUAGE.into()
}

/// Returns an empty query — we use programmatic AST walking instead of queries.
pub fn query() -> Query {
    Query::new(&language(), "").expect("empty query should always be valid")
}

/// Walks the AST rooted at `root` and extracts code constructs from Rust source.
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
/// `current_impl` tracks whether we are inside an `impl` block (and its type name).
fn walk_node(
    node: &Node,
    source: &str,
    current_impl: Option<&str>,
    constructs: &mut Vec<CodeConstruct>,
) {
    match node.kind() {
        "function_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(&name_node, source).to_string();
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;

                if let Some(impl_name) = current_impl {
                    constructs.push(CodeConstruct::Method {
                        name,
                        start_line,
                        end_line,
                        enclosing: Some(impl_name.to_string()),
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
        "struct_item" => {
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
        "enum_item" => {
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
        "trait_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(&name_node, source).to_string();
                let start_line = node.start_position().row as u32 + 1;
                let end_line = node.end_position().row as u32 + 1;
                constructs.push(CodeConstruct::Trait {
                    name,
                    start_line,
                    end_line,
                });
            }
        }
        "impl_item" => {
            // Extract the type name for the impl block.
            let impl_name = node
                .child_by_field_name("type")
                .map(|t| node_text(&t, source).to_string());

            let start_line = node.start_position().row as u32 + 1;
            let end_line = node.end_position().row as u32 + 1;

            let name = impl_name.clone().unwrap_or_else(|| "<anonymous>".to_string());
            constructs.push(CodeConstruct::Impl {
                name: name.clone(),
                start_line,
                end_line,
            });

            // Walk children with impl context, then return to avoid double-walking.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_node(&child, source, Some(&name), constructs);
            }
            return;
        }
        "mod_item" => {
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
        "closure_expression" => {
            let start_line = node.start_position().row as u32 + 1;
            let end_line = node.end_position().row as u32 + 1;
            constructs.push(CodeConstruct::Closure {
                start_line,
                end_line,
                enclosing: current_impl.map(|s| s.to_string()),
            });
        }
        _ => {}
    }

    // Walk children (unless we already returned from impl_item).
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_node(&child, source, current_impl, constructs);
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
            .expect("Error loading Rust parser");
        let tree = parser.parse(source, None).unwrap();
        map_constructs(&tree.root_node(), source)
    }

    #[test]
    fn test_function() {
        let source = "fn hello() {}";
        let constructs = parse_and_map(source);
        assert_eq!(constructs.len(), 1);
        assert_eq!(constructs[0].kind_str(), "function");
        assert_eq!(constructs[0].name(), "hello");
    }

    #[test]
    fn test_struct_and_impl() {
        let source = r#"
struct Foo {
    x: i32,
}

impl Foo {
    fn bar(&self) -> i32 {
        self.x
    }

    fn baz(&mut self, v: i32) {
        self.x = v;
    }
}
"#;
        let constructs = parse_and_map(source);

        // Should have: struct Foo, impl Foo, method bar, method baz
        let struct_c: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "struct").collect();
        assert_eq!(struct_c.len(), 1);
        assert_eq!(struct_c[0].name(), "Foo");

        let impl_c: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "impl").collect();
        assert_eq!(impl_c.len(), 1);
        assert_eq!(impl_c[0].name(), "Foo");

        let methods: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "method").collect();
        assert_eq!(methods.len(), 2);

        let method_names: Vec<String> = methods.iter().map(|m| m.qualified_name()).collect();
        assert!(method_names.contains(&"Foo::bar".to_string()));
        assert!(method_names.contains(&"Foo::baz".to_string()));
    }

    #[test]
    fn test_enum() {
        let source = "enum Color { Red, Green, Blue }";
        let constructs = parse_and_map(source);
        let enums: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "enum").collect();
        assert_eq!(enums.len(), 1);
        assert_eq!(enums[0].name(), "Color");
    }

    #[test]
    fn test_trait() {
        let source = "trait Drawable { fn draw(&self); }";
        let constructs = parse_and_map(source);
        let traits: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "trait").collect();
        assert_eq!(traits.len(), 1);
        assert_eq!(traits[0].name(), "Drawable");
    }

    #[test]
    fn test_module() {
        let source = "mod inner { fn foo() {} }";
        let constructs = parse_and_map(source);
        let modules: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "module").collect();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].name(), "inner");
    }

    #[test]
    fn test_closure() {
        let source = "fn main() { let f = |x| x + 1; }";
        let constructs = parse_and_map(source);
        let closures: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "closure").collect();
        assert_eq!(closures.len(), 1);
    }

    #[test]
    fn test_complex_file() {
        let source = r#"
mod utils {
    fn helper() {}
}

struct Point {
    x: f64,
    y: f64,
}

enum Shape {
    Circle(f64),
    Rect(f64, f64),
}

trait Drawable {
    fn draw(&self);
}

impl Drawable for Point {
    fn draw(&self) {
        let scale = |v: f64| v * 2.0;
        println!("{}", scale(self.x));
    }
}

impl Point {
    fn new(x: f64, y: f64) -> Self {
        Point { x, y }
    }
}

fn main() {
    let p = Point::new(1.0, 2.0);
    p.draw();
}
"#;
        let constructs = parse_and_map(source);

        let kinds: Vec<&str> = constructs.iter().map(|c| c.kind_str()).collect();
        assert!(kinds.contains(&"module"), "should find module");
        assert!(kinds.contains(&"struct"), "should find struct");
        assert!(kinds.contains(&"enum"), "should find enum");
        assert!(kinds.contains(&"trait"), "should find trait");
        assert!(kinds.contains(&"impl"), "should find impl");
        assert!(kinds.contains(&"method"), "should find method");
        assert!(kinds.contains(&"closure"), "should find closure");
        assert!(kinds.contains(&"function"), "should find function");
    }
}
