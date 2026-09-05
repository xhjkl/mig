use super::lower::{HighlightQueries, NodeAnnotation};
use super::{ContentChannel, LayoutOwnership, ReviewUnit, SiblingMatching, WrapperBoundary};
use ::tree_sitter::Node;

pub static HIGHLIGHT_QUERIES: HighlightQueries =
    HighlightQueries::new(&[tree_sitter_toml_ng::HIGHLIGHTS_QUERY]);

pub fn annotate(node: Node<'_>, parent_kind: Option<&str>) -> NodeAnnotation {
    let kind = node.kind();
    if kind == "comment" {
        return NodeAnnotation {
            review: (parent_kind == Some("document"))
                .then(|| ReviewUnit::linewise(LayoutOwnership::None)),
            channel: Some(ContentChannel::Comment),
            prune_children: true,
            ..NodeAnnotation::default()
        };
    }
    let keyed = matches!(kind, "pair" | "table" | "table_array_element");
    let container = keyed || matches!(kind, "document" | "inline_table" | "array");
    NodeAnnotation {
        review: (parent_kind == Some("document") && keyed)
            .then(|| ReviewUnit::structural(LayoutOwnership::AdjacentBlankLines)),
        identity: if keyed {
            node.named_children(&mut node.walk())
                .find(|child| matches!(child.kind(), "bare_key" | "quoted_key" | "dotted_key"))
                .map(|key| key.byte_range())
        } else {
            None
        },
        sibling_matching: if container && kind != "array" {
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
