use super::lower::{HighlightQueries, NodeAnnotation, TextTreatment};
use super::{ContentChannel, ReviewRole, SiblingOrder, WrapperBoundary};
use ::tree_sitter::Node;

pub static HIGHLIGHT_QUERIES: HighlightQueries =
    HighlightQueries::new(&[tree_sitter_json::HIGHLIGHTS_QUERY]);

pub fn annotate(node: Node<'_>) -> NodeAnnotation {
    if node.kind() == "comment" {
        return NodeAnnotation {
            text: TextTreatment::Verbatim(ContentChannel::Comment),
            ..NodeAnnotation::default()
        };
    }
    let container = matches!(node.kind(), "document" | "object" | "array" | "pair");
    NodeAnnotation {
        review: (node.kind() == "document").then_some(ReviewRole::Content),
        identity: node.child_by_field_name("key").map(|key| key.byte_range()),
        sibling_order: if matches!(node.kind(), "document" | "object" | "pair") {
            SiblingOrder::ByIdentity
        } else {
            SiblingOrder::SourceOrder
        },
        wrapper_boundary: if container {
            WrapperBoundary::Sealed
        } else {
            WrapperBoundary::Traversable
        },
        ..NodeAnnotation::default()
    }
}
