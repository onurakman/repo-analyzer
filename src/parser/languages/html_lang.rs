use tree_sitter::{Language, Node, Query};

use crate::types::CodeConstruct;

/// Returns the tree-sitter Language for HTML.
pub fn language() -> Language {
    tree_sitter_html::LANGUAGE.into()
}

/// Returns an empty query — we use programmatic AST walking instead of queries.
pub fn query() -> Query {
    Query::new(&language(), "").expect("empty query should always be valid")
}

/// HTML does not have traditional code constructs.
/// Files are tracked at file level only; returns an empty vec.
pub fn map_constructs(_root: &Node, _source: &str) -> Vec<CodeConstruct> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_and_map(source: &str) -> Vec<CodeConstruct> {
        let mut parser = Parser::new();
        parser
            .set_language(&language())
            .expect("Error loading HTML parser");
        let tree = parser.parse(source, None).unwrap();
        map_constructs(&tree.root_node(), source)
    }

    #[test]
    fn test_empty_constructs() {
        let source = "<html><body><h1>Hello</h1></body></html>";
        let constructs = parse_and_map(source);
        assert!(constructs.is_empty());
    }

    #[test]
    fn test_language_parses() {
        let mut parser = Parser::new();
        parser
            .set_language(&language())
            .expect("Error loading HTML parser");
        let tree = parser.parse("<div></div>", None);
        assert!(tree.is_some());
    }
}
