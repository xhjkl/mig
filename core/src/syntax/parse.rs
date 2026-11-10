use super::{Grammar, LineFallback};
use crate::source::Source;
use ::tree_sitter::{Language as TreeSitterLanguage, Node, Parser, Tree};
use anyhow::{Result, anyhow};

/// Resource budget before highlighting, lowering, or recursive matching starts.
pub struct Budget {
    pub nodes: usize,
    pub depth: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            nodes: 500_000,
            depth: 128,
        }
    }
}

pub enum Outcome<'source> {
    Ready(ParsedFile<'source>),
    Fallback(LineFallback),
}

/// Parser tree kept with the exact source and grammar needed to interpret its nodes.
pub struct ParsedFile<'source> {
    pub source: Source<'source>,
    pub grammar: Grammar,
    pub tree: Tree,
}

/// Parse a revision within the resource budget.
/// Recovery inside the tree is left for the language lowerer to assess.
pub fn parse(source: Source<'_>, grammar: Grammar, budget: Budget) -> Result<Outcome<'_>> {
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
    if root.is_missing() {
        return Ok(Outcome::Fallback(LineFallback::ParseErrors));
    }
    if let Err(reason) = check_budget(root, budget) {
        return Ok(Outcome::Fallback(reason));
    }

    Ok(Outcome::Ready(ParsedFile {
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

fn check_budget(root: Node<'_>, budget: Budget) -> Result<(), LineFallback> {
    let mut cursor = root.walk();
    let mut nodes = 0;
    let mut depth = 0;
    loop {
        nodes += 1;
        if nodes > budget.nodes {
            return Err(LineFallback::NodeLimit);
        }
        if cursor.goto_first_child() {
            depth += 1;
            if depth > budget.depth {
                return Err(LineFallback::DepthLimit);
            }
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return Ok(());
            }
            depth -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax_node_budget_rejects_before_lowering() {
        let source = Source::new("fn {");
        let result = parse(
            source,
            Grammar::Rust,
            Budget {
                nodes: 1,
                ..Budget::default()
            },
        );

        assert!(matches!(
            result,
            Ok(Outcome::Fallback(LineFallback::NodeLimit))
        ));
    }
}
