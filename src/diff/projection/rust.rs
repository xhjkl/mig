use super::tree_sitter::{
    self, Adapter, DecorationHint, GapContext, HighlightQueries, NodeAnnotation, NodeContext,
    ProjectFailure,
};
use super::{ContentChannel, Language, LayoutOwnership, Projection, ReviewMode, ReviewUnit};
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
            return NodeAnnotation {
                owns_decorations: true,
                ..NodeAnnotation::default()
            };
        }

        if is_comment(node.kind()) {
            let review = (context.parent_kind == Some("source_file"))
                .then(|| ReviewUnit::new(ReviewMode::Linewise, LayoutOwnership::None));
            let text = &context.source[node.byte_range()];
            return NodeAnnotation {
                review,
                channel: Some(ContentChannel::Comment),
                descendant_channel: Some(ContentChannel::Comment),
                decoration: doc_comment_decoration(node.kind(), text),
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
                owns_decorations: true,
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
                owns_decorations: true,
                ..NodeAnnotation::default()
            };
        }

        let decoration = match node.kind() {
            "attribute_item" => DecorationHint::FollowingSibling,
            "inner_attribute_item" => DecorationHint::EnclosingOwner,
            _ => DecorationHint::None,
        };
        if context.parent_kind == Some("source_file") && node.is_named() {
            return NodeAnnotation {
                review: Some(ReviewUnit::new(
                    ReviewMode::Linewise,
                    LayoutOwnership::AdjacentBlankLines,
                )),
                decoration,
                owns_decorations: true,
                ..NodeAnnotation::default()
            };
        }

        NodeAnnotation {
            decoration,
            owns_decorations: is_nested_decoration_owner(node.kind()),
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

fn doc_comment_decoration(kind: &str, text: &str) -> DecorationHint {
    match kind {
        "line_comment" if text.starts_with("//!") => DecorationHint::EnclosingOwner,
        "line_comment" if text.starts_with("///") && !text.starts_with("////") => {
            DecorationHint::FollowingSibling
        }
        "block_comment" if text.starts_with("/*!") => DecorationHint::EnclosingOwner,
        "block_comment" if text.starts_with("/**") && !text.starts_with("/***") => {
            DecorationHint::FollowingSibling
        }
        "line_comment" | "block_comment" => DecorationHint::None,
        _ => unreachable!("doc-comment policy receives only Rust comments"),
    }
}

/// Nested semantic boundaries that may receive an outer attribute or documentation comment.
fn is_nested_decoration_owner(kind: &str) -> bool {
    is_definition(kind)
        || matches!(
            kind,
            "associated_type"
                | "enum_variant"
                | "field_declaration"
                | "function_signature_item"
                | "let_declaration"
                | "macro_invocation"
                | "match_arm"
        )
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
    use crate::diff::projection::NodeId;

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
        let module = projection
            .nodes
            .iter()
            .position(|node| node.kind == "mod_item")
            .map(NodeId::new)
            .expect("module owner");
        let function = projection
            .nodes
            .iter()
            .position(|node| node.kind == "function_item")
            .map(NodeId::new)
            .expect("function owner");

        assert_eq!(outer.decoration_owner, Some(function));
        assert_eq!(inner.decoration_owner, Some(module));
    }

    #[test]
    fn outer_decorations_cross_independent_comments_to_their_declared_owner() {
        let source = Source::new(concat!(
            "#[derive(Clone)]\n",
            "// explanatory comment\n",
            "/// Alpha documentation.\n",
            "struct Alpha { value: u8 }\n",
            "\n",
            "#[derive(Clone)]\n",
            "/// Beta documentation.\n",
            "struct Beta { value: u16 }\n",
        ));
        let projection = project(source);
        let Ok(projection) = projection else {
            panic!("Rust source must project");
        };
        let node_on_line = |kind: &str, line: usize| {
            projection
                .nodes
                .iter()
                .position(|node| node.kind == kind && node.lines.start == line)
                .map(NodeId::new)
                .expect("expected syntax node")
        };
        let alpha = node_on_line("struct_item", 4);
        let beta = node_on_line("struct_item", 8);
        let alpha_attribute = node_on_line("attribute_item", 1);
        let ordinary_comment = node_on_line("line_comment", 2);
        let alpha_docs = node_on_line("line_comment", 3);
        let beta_attribute = node_on_line("attribute_item", 6);
        let beta_docs = node_on_line("line_comment", 7);

        assert_eq!(
            projection.node(alpha_attribute).decoration_owner,
            Some(alpha)
        );
        assert_eq!(projection.node(alpha_docs).decoration_owner, Some(alpha));
        assert_eq!(projection.node(ordinary_comment).decoration_owner, None);
        assert_eq!(projection.node(beta_attribute).decoration_owner, Some(beta));
        assert_eq!(projection.node(beta_docs).decoration_owner, Some(beta));
    }

    #[test]
    fn nested_outer_and_inner_docs_choose_following_and_enclosing_owners() {
        let source = Source::new(concat!(
            "mod tests {\n",
            "#![allow(dead_code)]\n",
            "//! Module line documentation.\n",
            "/*! Module block documentation. */\n",
            "#[test]\n",
            "/// Alpha documentation.\n",
            "fn alpha() {}\n",
            "}\n",
        ));
        let projection = project(source);
        let Ok(projection) = projection else {
            panic!("Rust source must project");
        };
        let node_on_line = |kind: &str, line: usize| {
            projection
                .nodes
                .iter()
                .position(|node| node.kind == kind && node.lines.start == line)
                .map(NodeId::new)
                .expect("expected syntax node")
        };
        let module = node_on_line("mod_item", 1);
        let inner_attribute = node_on_line("inner_attribute_item", 2);
        let inner_line_docs = node_on_line("line_comment", 3);
        let inner_block_docs = node_on_line("block_comment", 4);
        let outer_attribute = node_on_line("attribute_item", 5);
        let outer_docs = node_on_line("line_comment", 6);
        let function = node_on_line("function_item", 7);

        for decoration in [inner_attribute, inner_line_docs, inner_block_docs] {
            assert_eq!(projection.node(decoration).decoration_owner, Some(module));
        }
        for decoration in [outer_attribute, outer_docs] {
            assert_eq!(projection.node(decoration).decoration_owner, Some(function));
        }
    }
}
