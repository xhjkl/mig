use super::tree_sitter::{
    self, Adapter, GapContext, HighlightQueries, NodeAnnotation, NodeContext, ProjectFailure,
};
use super::{
    ContentChannel, Language, LayoutOwnership, Projection, ReviewMode, ReviewUnit,
    SiblingAttachment,
};
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
                .then(|| ReviewUnit::new(ReviewMode::Linewise, LayoutOwnership::None));
            return NodeAnnotation {
                review,
                channel: Some(ContentChannel::Comment),
                descendant_channel: Some(ContentChannel::Comment),
                prune_children: true,
                ..NodeAnnotation::default()
            };
        }

        if node.kind() == "use_declaration" || is_bodyless_module(node) {
            let identity = node
                .child_by_field_name("argument")
                .or_else(|| node.child_by_field_name("name"))
                .map(|identity| identity.byte_range());
            let review = (context.parent_kind == Some("source_file"))
                .then(|| ReviewUnit::new(ReviewMode::Compact, LayoutOwnership::None));
            return NodeAnnotation {
                review,
                identity,
                ..NodeAnnotation::default()
            };
        }

        if is_definition(node.kind()) {
            let identity = identity_node(node).map(|identity| identity.byte_range());
            let review = (context.parent_kind == Some("source_file")).then(|| {
                ReviewUnit::new(ReviewMode::Structural, LayoutOwnership::AdjacentBlankLines)
            });
            return NodeAnnotation {
                review,
                identity,
                ..NodeAnnotation::default()
            };
        }

        let attachment = if node.kind() == "attribute_item" {
            SiblingAttachment::Following
        } else {
            SiblingAttachment::None
        };
        if context.parent_kind == Some("source_file") && node.is_named() {
            return NodeAnnotation {
                review: Some(ReviewUnit::new(
                    ReviewMode::Linewise,
                    LayoutOwnership::AdjacentBlankLines,
                )),
                attachment,
                ..NodeAnnotation::default()
            };
        }

        NodeAnnotation {
            attachment,
            ..NodeAnnotation::default()
        }
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

/// External module declarations are source wiring; inline modules remain definitions.
fn is_bodyless_module(node: ::tree_sitter::Node<'_>) -> bool {
    node.kind() == "mod_item" && node.child_by_field_name("body").is_none()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodyless_modules_are_compact_but_inline_modules_are_structural() {
        let source = Source::new("mod external;\nmod inline { fn payload() {} }\n");
        let projection = project(source);
        let Ok(projection) = projection else {
            panic!("Rust source must project");
        };
        let modules = projection
            .review_units()
            .filter(|(_, node)| node.kind == "mod_item")
            .map(|(_, node)| node.review.as_ref().unwrap().mode)
            .collect::<Vec<_>>();

        assert_eq!(modules, [ReviewMode::Compact, ReviewMode::Structural]);
    }

    #[test]
    fn only_outer_attributes_attach_to_the_following_sibling() {
        let source =
            Source::new("mod tests {\n#![allow(dead_code)]\n#[test]\nfn payload() {}\n}\n");
        let projection = project(source);
        let Ok(projection) = projection else {
            panic!("Rust source must project");
        };
        let outer = projection
            .nodes
            .iter()
            .find(|node| node.kind == "attribute_item")
            .expect("outer attribute");
        let inner = projection
            .nodes
            .iter()
            .find(|node| node.kind == "inner_attribute_item")
            .expect("inner attribute");

        assert_eq!(outer.attachment, SiblingAttachment::Following);
        assert_eq!(inner.attachment, SiblingAttachment::None);
    }
}
