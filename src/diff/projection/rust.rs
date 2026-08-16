use super::tree_sitter::{
    self, Adapter, GapContext, HighlightQueries, NodeAnnotation, NodeContext, ProjectFailure,
};
use super::{ContentChannel, Language, LayoutOwnership, Projection, ReviewTreatment, ReviewUnit};
use crate::diff::source::Source;
use ::tree_sitter::Language as TreeSitterLanguage;

static HIGHLIGHT_QUERIES: HighlightQueries =
    HighlightQueries::new(&[tree_sitter_rust::HIGHLIGHTS_QUERY]);

pub(super) fn project<'source>(
    source: Source<'source>,
) -> Result<Projection<'source>, ProjectFailure<'source>> {
    tree_sitter::project(source, &RustAdapter)
}

struct RustAdapter;

impl Adapter for RustAdapter {
    fn language(&self) -> TreeSitterLanguage {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn projected_language(&self) -> Language {
        Language::Rust
    }

    fn highlight_queries(&self) -> &'static HighlightQueries {
        &HIGHLIGHT_QUERIES
    }

    fn annotate(&self, context: NodeContext<'_, '_>) -> NodeAnnotation {
        let node = context.node;
        if node.kind() == "source_file" {
            return NodeAnnotation::default();
        }

        if is_comment(node.kind()) {
            let review = (context.parent_kind == Some("source_file"))
                .then(|| ReviewUnit::stationary(ReviewTreatment::Linewise, LayoutOwnership::None));
            return NodeAnnotation {
                review,
                channel: Some(ContentChannel::Comment),
                descendant_channel: Some(ContentChannel::Comment),
                prune_children: true,
                ..NodeAnnotation::default()
            };
        }

        if node.kind() == "use_declaration" {
            let identity = node
                .child_by_field_name("argument")
                .map(|argument| argument.byte_range());
            let review = (context.parent_kind == Some("source_file"))
                .then(|| ReviewUnit::stationary(ReviewTreatment::Compact, LayoutOwnership::None));
            return NodeAnnotation {
                review,
                identity,
                ..NodeAnnotation::default()
            };
        }

        if is_definition(node.kind()) {
            let identity = identity_node(node).map(|identity| identity.byte_range());
            let review = (context.parent_kind == Some("source_file")).then(|| {
                ReviewUnit::movable(ReviewTreatment::Inline, LayoutOwnership::AdjacentBlankLines)
            });
            return NodeAnnotation {
                review,
                identity,
                ..NodeAnnotation::default()
            };
        }

        if context.parent_kind == Some("source_file") && node.is_named() {
            return NodeAnnotation {
                review: Some(ReviewUnit::stationary(
                    ReviewTreatment::Linewise,
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
                "char_literal" | "raw_string_literal" | "string_content" | "string_literal"
            )
        {
            return ContentChannel::Syntax;
        }
        context.default_channel()
    }
}

fn identity_node<'tree>(node: ::tree_sitter::Node<'tree>) -> Option<::tree_sitter::Node<'tree>> {
    let identity = node.child_by_field_name("name");
    if identity.is_some() {
        return identity;
    }
    if node.kind() != "impl_item" {
        return None;
    }
    node.child_by_field_name("type")
}

fn is_comment(kind: &str) -> bool {
    matches!(kind, "line_comment" | "block_comment")
}

fn is_definition(kind: &str) -> bool {
    matches!(
        kind,
        "const_item"
            | "enum_item"
            | "extern_crate_declaration"
            | "foreign_mod_item"
            | "function_item"
            | "impl_item"
            | "macro_definition"
            | "mod_item"
            | "static_item"
            | "struct_item"
            | "trait_item"
            | "type_item"
            | "union_item"
    )
}
