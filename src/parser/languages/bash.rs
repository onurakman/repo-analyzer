use tree_sitter::{Language, Node, Query};

use crate::types::CodeConstruct;

/// Returns the tree-sitter Language for Bash.
pub fn language() -> Language {
    tree_sitter_bash::LANGUAGE.into()
}

/// Returns an empty query — we use programmatic AST walking instead of queries.
pub fn query() -> Query {
    Query::new(&language(), "").expect("empty query should always be valid")
}

/// Walks the AST rooted at `root` and extracts code constructs from Bash source.
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
fn walk_node(
    node: &Node,
    source: &str,
    constructs: &mut Vec<CodeConstruct>,
) {
    if node.kind() == "function_definition" {
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
            .expect("Error loading Bash parser");
        let tree = parser.parse(source, None).unwrap();
        map_constructs(&tree.root_node(), source)
    }

    #[test]
    fn test_function() {
        let source = "hello() {\n  echo \"hi\"\n}";
        let constructs = parse_and_map(source);
        let funcs: Vec<_> = constructs.iter().filter(|c| c.kind_str() == "function").collect();
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].name(), "hello");
    }

    #[test]
    fn test_no_constructs_in_plain_script() {
        let source = "echo 'hello world'\nls -la";
        let constructs = parse_and_map(source);
        assert!(constructs.is_empty());
    }
}
