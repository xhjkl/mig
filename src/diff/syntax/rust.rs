use super::lower::{DecorationHint, HighlightQueries, NodeAnnotation};
use super::{ContentChannel, LayoutOwnership, ReviewUnit, SiblingMatching, WrapperBoundary};
use ::tree_sitter::Node;

pub static HIGHLIGHT_QUERIES: HighlightQueries =
    HighlightQueries::new(&[tree_sitter_rust::HIGHLIGHTS_QUERY]);

pub fn annotate(node: Node<'_>, parent_kind: Option<&str>, source: &str) -> NodeAnnotation {
    if node.kind() == "source_file" {
        return NodeAnnotation {
            owns_decorations: true,
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }

    if matches!(node.kind(), "line_comment" | "block_comment") {
        let review = (parent_kind == Some("source_file"))
            .then(|| ReviewUnit::linewise(LayoutOwnership::None));
        let text = &source[node.byte_range()];
        return NodeAnnotation {
            review,
            channel: Some(ContentChannel::Comment),
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
        let review =
            (parent_kind == Some("source_file")).then(|| ReviewUnit::wiring(LayoutOwnership::None));
        return NodeAnnotation {
            review,
            identity,
            owns_decorations: true,
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }

    if is_definition(node.kind()) {
        let identity = identity_node(node).map(|identity| identity.byte_range());
        let review = (parent_kind == Some("source_file"))
            .then(|| ReviewUnit::structural(LayoutOwnership::AdjacentBlankLines));
        return NodeAnnotation {
            review,
            identity,
            owns_decorations: true,
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }

    let decoration = match node.kind() {
        "attribute_item" => DecorationHint::FollowingSibling,
        "inner_attribute_item" => DecorationHint::EnclosingOwner,
        _ => DecorationHint::None,
    };
    if parent_kind == Some("source_file") && node.is_named() {
        return NodeAnnotation {
            review: Some(ReviewUnit::linewise(LayoutOwnership::AdjacentBlankLines)),
            decoration,
            owns_decorations: true,
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }

    let identity = match node.kind() {
        "enum_variant" | "field_declaration" => identity_node(node),
        "call_expression" => call_identity(node),
        "binary_expression" => node.child_by_field_name("operator"),
        _ => None,
    }
    .map(|identity| identity.byte_range());
    let semantic_owner = is_semantic_owner(node.kind());
    NodeAnnotation {
        identity,
        decoration,
        owns_decorations: semantic_owner,
        sibling_matching: if semantic_owner {
            SiblingMatching::LocalIdentity
        } else {
            SiblingMatching::OrderedSyntax
        },
        wrapper_boundary: if semantic_owner {
            WrapperBoundary::Sealed
        } else {
            WrapperBoundary::Traversable
        },
        ..NodeAnnotation::default()
    }
}

pub fn whitespace_is_syntax(parent_kind: &str) -> bool {
    matches!(
        parent_kind,
        "char_literal" | "raw_string_literal" | "string_content" | "string_literal"
    )
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

fn call_identity<'tree>(node: ::tree_sitter::Node<'tree>) -> Option<::tree_sitter::Node<'tree>> {
    let function = node.child_by_field_name("function")?;
    function
        .child_by_field_name("field")
        .or_else(|| function.child_by_field_name("name"))
        .or_else(|| matches!(function.kind(), "identifier").then_some(function))
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

/// Identify semantic owners that receive decorations and seal their unmatched descendants.
fn is_semantic_owner(kind: &str) -> bool {
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
