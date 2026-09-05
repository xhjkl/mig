use super::lower::{HighlightQueries, NodeAnnotation};
use super::{ContentChannel, LayoutOwnership, ReviewUnit, SiblingMatching, WrapperBoundary};
use ::tree_sitter::Node;

pub static HIGHLIGHT_QUERIES: HighlightQueries =
    HighlightQueries::new(&[tree_sitter_json::HIGHLIGHTS_QUERY]);

pub fn annotate(node: Node<'_>) -> NodeAnnotation {
    if node.kind() == "comment" {
        return NodeAnnotation {
            channel: Some(ContentChannel::Comment),
            prune_children: true,
            ..NodeAnnotation::default()
        };
    }
    let container = matches!(node.kind(), "document" | "object" | "array" | "pair");
    NodeAnnotation {
        review: (node.kind() == "document").then(|| ReviewUnit::structural(LayoutOwnership::None)),
        identity: node.child_by_field_name("key").map(|key| key.byte_range()),
        sibling_matching: if matches!(node.kind(), "document" | "object" | "pair") {
            SiblingMatching::LocalIdentity
        } else {
            SiblingMatching::OrderedSyntax
        },
        wrapper_boundary: if container {
            WrapperBoundary::Sealed
        } else {
            WrapperBoundary::Traversable
        },
        ..NodeAnnotation::default()
    }
}
