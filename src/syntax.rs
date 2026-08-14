use anyhow::{Context, Result};
use std::ops::Range;
use tree_sitter::{Node, Parser, Tree};

/// Coarse language syntax category understood by the terminal palette.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxClass {
    Plain,
    Keyword,
    Identifier,
    Type,
    Literal,
    String,
    Comment,
    Punctuation,
}

/// Parsed Rust source and the syntax projections used by correspondence.
pub(crate) struct SyntaxFile<'source> {
    pub(crate) source: &'source str,
    pub(crate) tree: Tree,
    pub(crate) lines: Vec<SourceLine>,
    pub(crate) definitions: Vec<Definition<'source>>,
    pub(crate) imports: Vec<Import<'source>>,
}

/// One top-level named Rust definition, preserved as a source occurrence.
pub(crate) struct Definition<'source> {
    pub(crate) kind: &'static str,
    pub(crate) name: &'source str,
    pub(crate) bytes: Range<usize>,
    pub(crate) lines: Range<usize>,
    pub(crate) tokens: Vec<Token<'source>>,
    pub(crate) comments: Vec<Comment<'source>>,
    pub(crate) has_error: bool,
    pub(crate) code_fingerprint: StructuralFingerprint<'source>,
    pub(crate) full_fingerprint: StructuralFingerprint<'source>,
}

/// One top-level Rust import retained for the compact import treatment.
pub(crate) struct Import<'source> {
    pub(crate) text: &'source str,
    pub(crate) line: usize,
}

/// One concrete leaf occurrence, including missing zero-width syntax.
pub(crate) struct Token<'source> {
    pub(crate) text: &'source str,
    pub(crate) kind: &'static str,
    pub(crate) field: Option<&'static str>,
    pub(crate) bytes: Range<usize>,
    pub(crate) is_comment: bool,
}

impl Token<'_> {
    /// Rust-specific projection into Mig's deliberately small syntax palette.
    pub(crate) fn syntax_class(&self) -> SyntaxClass {
        let kind = self.kind;
        if self.is_comment {
            return SyntaxClass::Comment;
        }
        if kind.contains("string") || kind.contains("char_literal") {
            return SyntaxClass::String;
        }
        if kind.contains("literal")
            || kind.contains("integer")
            || kind.contains("float")
            || matches!(kind, "true" | "false")
        {
            return SyntaxClass::Literal;
        }
        if kind.contains("type_identifier") || kind == "primitive_type" {
            return SyntaxClass::Type;
        }
        if kind == "identifier" {
            return SyntaxClass::Identifier;
        }
        if is_rust_keyword(kind) {
            return SyntaxClass::Keyword;
        }
        if kind
            .chars()
            .any(|character| character.is_ascii_punctuation())
        {
            return SyntaxClass::Punctuation;
        }
        SyntaxClass::Plain
    }
}

/// One comment occurrence owned by its surrounding definition.
pub(crate) struct Comment<'source> {
    pub(crate) indent: &'source str,
    pub(crate) text: &'source str,
    pub(crate) line: usize,
}

/// One source row mapped back to its exact non-newline bytes.
pub(crate) struct SourceLine {
    pub(crate) number: usize,
    pub(crate) bytes: Range<usize>,
}

/// Collision-free preorder encoding of syntax shape and leaf payloads.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct StructuralFingerprint<'source> {
    nodes: Vec<StructuralNode<'source>>,
}

/// Child counts make the preorder stream retain exact ordered containment.
#[derive(Clone, Debug, Eq, PartialEq)]
struct StructuralNode<'source> {
    kind: &'static str,
    field: Option<&'static str>,
    child_count: usize,
    payload: Option<&'source str>,
    named: bool,
    extra: bool,
    missing: bool,
}

#[derive(Default)]
struct DefinitionProjection<'source> {
    tokens: Vec<Token<'source>>,
    comments: Vec<Comment<'source>>,
    code: Vec<StructuralNode<'source>>,
    full: Vec<StructuralNode<'source>>,
}

/// Parse Rust without imposing any filesystem or workspace I/O policy.
pub(crate) fn parse_rust(source: &str) -> Result<SyntaxFile<'_>> {
    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE.into();
    let language_result = parser.set_language(&language);
    language_result.context("failed to load the Rust tree-sitter grammar")?;

    let tree = parser.parse(source, None);
    let tree = tree.context("tree-sitter cancelled the Rust parse")?;
    let lines = source_lines(source);
    let (definitions, imports) = top_level_occurrences(&tree, source);

    Ok(SyntaxFile {
        source,
        tree,
        lines,
        definitions,
        imports,
    })
}

/// Named definitions and imports in original source order.
fn top_level_occurrences<'source>(
    tree: &Tree,
    source: &'source str,
) -> (Vec<Definition<'source>>, Vec<Import<'source>>) {
    let root = tree.root_node();
    let mut definitions = Vec::new();
    let mut imports = Vec::new();
    let mut cursor = root.walk();
    for node in root.named_children(&mut cursor) {
        if node.kind() == "use_declaration" {
            imports.push(import(node, source));
            continue;
        }
        if is_comment_kind(node.kind()) {
            continue;
        }

        let name = definition_name(node, source);
        let Some(name) = name else {
            continue;
        };
        definitions.push(definition(node, name, source));
    }
    (definitions, imports)
}

fn import<'source>(node: Node<'_>, source: &'source str) -> Import<'source> {
    let bytes = node.byte_range();
    Import {
        text: source[bytes.clone()].trim(),
        line: node.start_position().row + 1,
    }
}

fn definition<'source>(
    node: Node<'_>,
    name: &'source str,
    source: &'source str,
) -> Definition<'source> {
    let mut projection = DefinitionProjection::default();
    visit_node(node, None, source, &mut projection);

    Definition {
        kind: node.kind(),
        name,
        bytes: node.byte_range(),
        lines: node_line_range(node),
        tokens: projection.tokens,
        comments: projection.comments,
        has_error: node.has_error(),
        code_fingerprint: StructuralFingerprint {
            nodes: projection.code,
        },
        full_fingerprint: StructuralFingerprint {
            nodes: projection.full,
        },
    }
}

/// Project tokens, comments, and both fingerprints during the same tree walk.
fn visit_node<'source>(
    node: Node<'_>,
    field: Option<&'static str>,
    source: &'source str,
    projection: &mut DefinitionProjection<'source>,
) -> bool {
    let starts_comment = is_comment_kind(node.kind());
    if starts_comment {
        projection.comments.push(comment(node, source));
    }

    // A Tree-sitter comment owns its body while its children may only cover delimiters.
    let is_leaf = starts_comment || node.child_count() == 0;
    let payload = if is_leaf {
        let bytes = node.byte_range();
        Some(&source[bytes])
    } else {
        None
    };
    let fingerprint_node = StructuralNode {
        kind: node.kind(),
        field,
        child_count: 0,
        payload,
        named: node.is_named(),
        extra: node.is_extra(),
        missing: node.is_missing(),
    };
    let full_index = projection.full.len();
    projection.full.push(fingerprint_node.clone());
    let code_index = if starts_comment {
        None
    } else {
        let index = projection.code.len();
        projection.code.push(fingerprint_node);
        Some(index)
    };

    if is_leaf {
        let bytes = node.byte_range();
        projection.tokens.push(Token {
            text: &source[bytes.clone()],
            kind: node.kind(),
            field,
            bytes,
            is_comment: starts_comment,
        });
        return code_index.is_some();
    }

    let mut full_children = 0;
    let mut code_children = 0;
    let mut cursor = node.walk();
    for (index, child) in node.children(&mut cursor).enumerate() {
        let field = node.field_name_for_child(index as u32);
        let included_in_code = visit_node(child, field, source, projection);
        full_children += 1;
        if included_in_code {
            code_children += 1;
        }
    }
    projection.full[full_index].child_count = full_children;
    if let Some(code_index) = code_index {
        projection.code[code_index].child_count = code_children;
    }
    code_index.is_some()
}

fn comment<'source>(node: Node<'_>, source: &'source str) -> Comment<'source> {
    let bytes = node.byte_range();
    let line_start = source[..bytes.start]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    Comment {
        indent: &source[line_start..bytes.start],
        text: &source[bytes.clone()],
        line: node.start_position().row + 1,
    }
}

fn definition_name<'source>(node: Node<'_>, source: &'source str) -> Option<&'source str> {
    let name = node.child_by_field_name("name");
    if let Some(name) = name {
        return Some(&source[name.byte_range()]);
    }
    if node.kind() != "impl_item" {
        return None;
    }

    let name = node.child_by_field_name("type")?;
    Some(&source[name.byte_range()])
}

fn node_line_range(node: Node<'_>) -> Range<usize> {
    let start = node.start_position();
    let end = node.end_position();
    let start_line = start.row + 1;
    let end_line = if end.column == 0 && end.row > start.row {
        end.row + 1
    } else {
        end.row + 2
    };
    start_line..end_line
}

fn is_comment_kind(kind: &str) -> bool {
    matches!(kind, "line_comment" | "block_comment")
}

fn is_rust_keyword(kind: &str) -> bool {
    matches!(
        kind,
        "as" | "async"
            | "await"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
    )
}

fn source_lines(source: &str) -> Vec<SourceLine> {
    if source.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in source.bytes().enumerate() {
        if byte != b'\n' {
            continue;
        }
        let mut end = index;
        if end > start && source.as_bytes()[end - 1] == b'\r' {
            end -= 1;
        }
        lines.push(SourceLine {
            number: lines.len() + 1,
            bytes: start..end,
        });
        start = index + 1;
    }
    if start < source.len() {
        lines.push(SourceLine {
            number: lines.len() + 1,
            bytes: start..source.len(),
        });
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_frontend_retains_duplicate_definition_occurrences() {
        let source = "use crate::Thing;\n\nimpl Thing {}\nimpl Thing {}\n";
        let parsed = parse_rust(source).expect("Rust source must parse");

        assert_eq!(parsed.tree.root_node().kind(), "source_file");
        assert_eq!(parsed.source, source);
        assert_eq!(parsed.lines.len(), 4);
        assert_eq!(parsed.lines[1].bytes, 18..18);
        assert_eq!(parsed.imports.len(), 1);
        assert_eq!(parsed.definitions.len(), 2);
        assert_eq!(parsed.definitions[0].kind, "impl_item");
        assert_eq!(parsed.definitions[0].name, "Thing");
        assert_eq!(parsed.definitions[0].lines, 3..4);
        assert_eq!(parsed.definitions[1].lines, 4..5);
    }

    #[test]
    fn whitespace_changes_only_concrete_source() {
        let before = parse_rust("fn run() { work(); }\n").expect("before source must parse");
        let after = parse_rust("fn run() {\n    work();\n}\n").expect("after source must parse");
        let before = &before.definitions[0];
        let after = &after.definitions[0];

        assert_eq!(before.code_fingerprint, after.code_fingerprint);
        assert_eq!(before.full_fingerprint, after.full_fingerprint);
        assert_ne!(before.bytes.len(), after.bytes.len());
    }

    #[test]
    fn comments_are_content_but_not_code_structure() {
        let before = parse_rust("fn run() {\n    // old\n    work();\n}\n")
            .expect("before source must parse");
        let after = parse_rust("fn run() {\n    // new\n    work();\n}\n")
            .expect("after source must parse");
        let before = &before.definitions[0];
        let after = &after.definitions[0];

        assert_eq!(before.code_fingerprint, after.code_fingerprint);
        assert_ne!(before.full_fingerprint, after.full_fingerprint);
        assert_eq!(before.comments[0].indent, "    ");
        assert_eq!(before.comments[0].text, "// old");
    }

    #[test]
    fn recovered_syntax_remains_visible_to_correspondence() {
        let parsed = parse_rust("fn broken(value: u32 {}\n").expect("parser must recover");
        let definition = &parsed.definitions[0];

        assert!(definition.has_error);
        assert!(definition.tokens.iter().any(|token| token.bytes.is_empty()));
    }
}
