use crate::diff::source::Source;
use ::tree_sitter::{Language as TreeSitterLanguage, Node, Parser, Tree};
use anyhow::anyhow;

const MAX_SYNTAX_NODES: usize = 500_000;

/// Tree-sitter grammar selected before lowering into Mig's neutral syntax arena.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ParserLanguage {
    C,
    Rust,
    Html,
    Css,
    TypeScript,
    Tsx,
    JavaScript,
    Jsx,
}

/// Concrete parser tree bound to the exact source revision it describes.
pub(super) struct ParsedFile<'source> {
    pub(super) source: Source<'source>,
    pub(super) language: ParserLanguage,
    pub(super) tree: Tree,
}

/// Source-owned failures fall back; frontend setup defects remain explicit errors.
pub(super) enum SyntaxFailure {
    Fallback,
    Setup(anyhow::Error),
}

/// Parse one source revision with the selected concrete grammar.
pub(super) fn parse(
    source: Source<'_>,
    language: ParserLanguage,
) -> Result<ParsedFile<'_>, SyntaxFailure> {
    parse_with_node_limit(source, language, MAX_SYNTAX_NODES)
}

fn parse_with_node_limit(
    source: Source<'_>,
    parsed_language: ParserLanguage,
    node_limit: usize,
) -> Result<ParsedFile<'_>, SyntaxFailure> {
    let mut parser = Parser::new();
    let language = tree_sitter_language(parsed_language);
    let language_result = parser.set_language(&language);
    if language_result.is_err() {
        let error = language_result.expect_err("checked language setup failure");
        return Err(SyntaxFailure::Setup(anyhow!(error)));
    }

    let tree = parser.parse(source.as_str(), None);
    let Some(tree) = tree else {
        return Err(SyntaxFailure::Setup(anyhow!(
            "tree-sitter cancelled a parse without a cancellation callback"
        )));
    };
    let root = tree.root_node();
    if root.is_missing() || tree_exceeds_node_limit(root, node_limit) {
        return Err(SyntaxFailure::Fallback);
    }

    Ok(ParsedFile {
        source,
        language: parsed_language,
        tree,
    })
}

pub(super) fn tree_sitter_language(language: ParserLanguage) -> TreeSitterLanguage {
    match language {
        ParserLanguage::C => tree_sitter_c::LANGUAGE.into(),
        ParserLanguage::Rust => tree_sitter_rust::LANGUAGE.into(),
        ParserLanguage::Html => tree_sitter_html::LANGUAGE.into(),
        ParserLanguage::Css => tree_sitter_css::LANGUAGE.into(),
        ParserLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        ParserLanguage::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        ParserLanguage::JavaScript | ParserLanguage::Jsx => tree_sitter_javascript::LANGUAGE.into(),
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
        let result = parse_with_node_limit(source, ParserLanguage::Rust, 1);

        assert!(matches!(result, Err(SyntaxFailure::Fallback)));
    }
}
