use super::tree_sitter::{
    self, Adapter, GapContext, HighlightQueries, NodeAnnotation, NodeContext, ProjectFailure,
};
use super::{ContentChannel, Language, LayoutOwnership, Projection, ReviewMode, ReviewUnit};
use crate::diff::source::Source;
use ::tree_sitter::{Language as TreeSitterLanguage, Node};

static TYPESCRIPT_HIGHLIGHTS: HighlightQueries = HighlightQueries::new(&[
    tree_sitter_javascript::HIGHLIGHT_QUERY,
    tree_sitter_typescript::HIGHLIGHTS_QUERY,
]);
static TSX_HIGHLIGHTS: HighlightQueries = HighlightQueries::new(&[
    tree_sitter_javascript::HIGHLIGHT_QUERY,
    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
    tree_sitter_typescript::HIGHLIGHTS_QUERY,
]);
static JAVASCRIPT_HIGHLIGHTS: HighlightQueries =
    HighlightQueries::new(&[tree_sitter_javascript::HIGHLIGHT_QUERY]);
static JSX_HIGHLIGHTS: HighlightQueries = HighlightQueries::new(&[
    tree_sitter_javascript::HIGHLIGHT_QUERY,
    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
]);

pub(super) fn project_typescript<'source>(
    source: Source<'source>,
) -> Result<Projection<'source>, ProjectFailure<'source>> {
    tree_sitter::project(
        source,
        &TypeScriptAdapter {
            dialect: ScriptDialect::TypeScript,
        },
    )
}

pub(super) fn project_tsx<'source>(
    source: Source<'source>,
) -> Result<Projection<'source>, ProjectFailure<'source>> {
    tree_sitter::project(
        source,
        &TypeScriptAdapter {
            dialect: ScriptDialect::Tsx,
        },
    )
}

pub(super) fn project_javascript<'source>(
    source: Source<'source>,
) -> Result<Projection<'source>, ProjectFailure<'source>> {
    tree_sitter::project(
        source,
        &TypeScriptAdapter {
            dialect: ScriptDialect::JavaScript,
        },
    )
}

pub(super) fn project_jsx<'source>(
    source: Source<'source>,
) -> Result<Projection<'source>, ProjectFailure<'source>> {
    tree_sitter::project(
        source,
        &TypeScriptAdapter {
            dialect: ScriptDialect::Jsx,
        },
    )
}

#[derive(Clone, Copy)]
enum ScriptDialect {
    TypeScript,
    Tsx,
    JavaScript,
    Jsx,
}

struct TypeScriptAdapter {
    dialect: ScriptDialect,
}

impl Adapter for TypeScriptAdapter {
    fn language(&self) -> TreeSitterLanguage {
        match self.dialect {
            ScriptDialect::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            ScriptDialect::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            ScriptDialect::JavaScript | ScriptDialect::Jsx => {
                tree_sitter_javascript::LANGUAGE.into()
            }
        }
    }

    fn projected_language(&self) -> Language {
        match self.dialect {
            ScriptDialect::TypeScript => Language::TypeScript,
            ScriptDialect::Tsx => Language::Tsx,
            ScriptDialect::JavaScript => Language::JavaScript,
            ScriptDialect::Jsx => Language::Jsx,
        }
    }

    fn highlight_queries(&self) -> &'static HighlightQueries {
        match self.dialect {
            ScriptDialect::TypeScript => &TYPESCRIPT_HIGHLIGHTS,
            ScriptDialect::Tsx => &TSX_HIGHLIGHTS,
            ScriptDialect::JavaScript => &JAVASCRIPT_HIGHLIGHTS,
            ScriptDialect::Jsx => &JSX_HIGHLIGHTS,
        }
    }

    fn annotate(&self, context: NodeContext<'_, '_>) -> NodeAnnotation {
        let node = context.node;
        if node.kind() == "program" {
            return NodeAnnotation::default();
        }

        if matches!(node.kind(), "comment" | "html_comment") {
            let review = (context.parent_kind == Some("program"))
                .then(|| ReviewUnit::new(ReviewMode::Linewise, LayoutOwnership::None));
            return NodeAnnotation {
                review,
                channel: Some(ContentChannel::Comment),
                descendant_channel: Some(ContentChannel::Comment),
                prune_children: true,
                ..NodeAnnotation::default()
            };
        }

        if node.kind() == "import_statement" {
            let identity = node
                .child_by_field_name("source")
                .map(|source| source.byte_range());
            let review = (context.parent_kind == Some("program"))
                .then(|| ReviewUnit::new(ReviewMode::Compact, LayoutOwnership::None));
            return NodeAnnotation {
                review,
                identity,
                ..NodeAnnotation::default()
            };
        }

        if is_jsx_element(node.kind()) {
            let identity = jsx_name(node).map(|name| name.byte_range());
            return NodeAnnotation {
                identity,
                ..NodeAnnotation::default()
            };
        }

        if is_declaration(node.kind()) {
            let identity = declaration_identity(node).map(|name| name.byte_range());
            let review = (context.parent_kind == Some("program")).then(|| {
                ReviewUnit::new(ReviewMode::Structural, LayoutOwnership::AdjacentBlankLines)
            });
            return NodeAnnotation {
                review,
                identity,
                ..NodeAnnotation::default()
            };
        }

        if context.parent_kind == Some("program") && node.is_named() {
            return NodeAnnotation {
                review: Some(ReviewUnit::new(
                    ReviewMode::Linewise,
                    LayoutOwnership::AdjacentBlankLines,
                )),
                ..NodeAnnotation::default()
            };
        }

        NodeAnnotation::default()
    }

    fn gap_channel(&self, context: GapContext<'_, '_>) -> ContentChannel {
        if context.is_whitespace()
            && matches!(
                context.parent.kind(),
                "string"
                    | "string_fragment"
                    | "template_literal_type"
                    | "template_string"
                    | "template_type"
            )
        {
            return ContentChannel::Syntax;
        }
        context.default_channel()
    }
}

fn declaration_identity<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    let identity = direct_identity(node);
    if identity.is_some() {
        return identity;
    }

    let declaration = node.child_by_field_name("declaration");
    if let Some(declaration) = declaration {
        let identity = descendant_identity(declaration);
        if identity.is_some() {
            return identity;
        }
    }

    let source = node.child_by_field_name("source");
    if source.is_some() {
        return source;
    }
    descendant_identity(node)
}

fn descendant_identity<'tree>(root: Node<'tree>) -> Option<Node<'tree>> {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        let identity = direct_identity(node);
        if identity.is_some() {
            return identity;
        }

        let mut cursor = node.walk();
        let children = node.named_children(&mut cursor).collect::<Vec<_>>();
        pending.extend(children.into_iter().rev());
    }
    None
}

fn direct_identity(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("name")
        .or_else(|| node.child_by_field_name("property"))
}

fn jsx_name<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    let opening = node.child_by_field_name("open_tag");
    let opening = opening.unwrap_or(node);
    opening.child_by_field_name("name")
}

fn is_jsx_element(kind: &str) -> bool {
    matches!(kind, "jsx_element" | "jsx_self_closing_element")
}

fn is_declaration(kind: &str) -> bool {
    matches!(
        kind,
        "abstract_class_declaration"
            | "ambient_declaration"
            | "class_declaration"
            | "enum_declaration"
            | "export_statement"
            | "function_declaration"
            | "function_signature"
            | "generator_function_declaration"
            | "interface_declaration"
            | "internal_module"
            | "lexical_declaration"
            | "method_definition"
            | "method_signature"
            | "module"
            | "type_alias_declaration"
            | "using_declaration"
            | "variable_declaration"
    )
}
