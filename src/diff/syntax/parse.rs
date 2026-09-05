use super::Grammar;
use crate::diff::source::Source;
use ::tree_sitter::{Language as TreeSitterLanguage, Node, Parser, Tree};
use anyhow::{Result, anyhow};

const MAX_SYNTAX_NODES: usize = 500_000;

/// Parser tree kept with the exact source and grammar needed to interpret its nodes.
pub struct ParsedFile<'source> {
    pub source: Source<'source>,
    pub grammar: Grammar,
    pub tree: Tree,
}

/// Parse a revision, returning `None` if its root is missing or its tree exceeds the node budget.
/// Recovery inside the tree is left for the language lowerer to assess.
pub fn parse(
    source: Source<'_>,
    grammar: Grammar,
    node_limit: Option<usize>,
) -> Result<Option<ParsedFile<'_>>> {
    let node_limit = node_limit.unwrap_or(MAX_SYNTAX_NODES);
    let mut parser = Parser::new();
    let language = tree_sitter_language(grammar);
    parser.set_language(&language)?;

    let tree = parser.parse(source.as_str(), None);
    let Some(tree) = tree else {
        return Err(anyhow!(
            "tree-sitter cancelled a parse without a cancellation callback"
        ));
    };
    let root = tree.root_node();
    if root.is_missing() || tree_exceeds_node_limit(root, node_limit) {
        return Ok(None);
    }

    Ok(Some(ParsedFile {
        source,
        grammar,
        tree,
    }))
}

pub fn tree_sitter_language(grammar: Grammar) -> TreeSitterLanguage {
    match grammar {
        Grammar::C => tree_sitter_c::LANGUAGE.into(),
        Grammar::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        Grammar::Rust => tree_sitter_rust::LANGUAGE.into(),
        Grammar::Python => tree_sitter_python::LANGUAGE.into(),
        Grammar::Go => tree_sitter_go::LANGUAGE.into(),
        Grammar::Json => tree_sitter_json::LANGUAGE.into(),
        Grammar::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
        Grammar::Nix => tree_sitter_nix::LANGUAGE.into(),
        Grammar::Html => tree_sitter_html::LANGUAGE.into(),
        Grammar::Css => tree_sitter_css::LANGUAGE.into(),
        Grammar::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Grammar::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Grammar::JavaScript | Grammar::Jsx => tree_sitter_javascript::LANGUAGE.into(),
    }
}

fn tree_exceeds_node_limit(root: Node<'_>, limit: usize) -> bool {
    let mut cursor = root.walk();
    let mut nodes = 0;
    loop {
        nodes += 1;
        if nodes > limit {
            return true;
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax_node_budget_rejects_before_lowering() {
        let source = Source::new("fn {");
        let result = parse(source, Grammar::Rust, Some(1));

        assert!(matches!(result, Ok(None)));
    }
}
