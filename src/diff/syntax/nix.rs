use super::lower::{HighlightQueries, NodeAnnotation};
use super::{ContentChannel, LayoutOwnership, ReviewUnit, SiblingMatching, WrapperBoundary};
use ::tree_sitter::Node;

pub static HIGHLIGHT_QUERIES: HighlightQueries =
    HighlightQueries::new(&[tree_sitter_nix::HIGHLIGHTS_QUERY]);

pub fn annotate(node: Node<'_>) -> NodeAnnotation {
    let kind = node.kind();
    if kind == "comment" {
        return NodeAnnotation {
            channel: Some(ContentChannel::Comment),
            prune_children: true,
            ..NodeAnnotation::default()
        };
    }
    let owner = matches!(
        kind,
        "source_code"
            | "binding_set"
            | "binding"
            | "inherit"
            | "inherit_from"
            | "attrset_expression"
            | "rec_attrset_expression"
            | "let_attrset_expression"
            | "formals"
            | "formal"
    );
    let scope = matches!(
        kind,
        "function_expression"
            | "let_expression"
            | "with_expression"
            | "assert_expression"
            | "if_expression"
            | "list_expression"
    );
    let identity = match kind {
        "binding" => node.child_by_field_name("attrpath"),
        "inherit" | "inherit_from" => node.child_by_field_name("attrs"),
        "formal" => node.child_by_field_name("name"),
        "apply_expression" => node.child_by_field_name("function"),
        _ => None,
    };
    NodeAnnotation {
        review: (kind == "source_code").then(|| ReviewUnit::structural(LayoutOwnership::None)),
        identity: identity.map(|identity| identity.byte_range()),
        sibling_matching: if owner {
            SiblingMatching::LocalIdentity
        } else {
            SiblingMatching::OrderedSyntax
        },
        wrapper_boundary: if owner || scope {
            WrapperBoundary::Sealed
        } else {
            WrapperBoundary::Traversable
        },
        ..NodeAnnotation::default()
    }
}
