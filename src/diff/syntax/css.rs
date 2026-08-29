use super::lower::{GapContext, HighlightQueries, NodeAnnotation, NodeContext};
use super::{ContentChannel, LayoutOwnership, ReviewUnit, SiblingMatching, WrapperBoundary};
use ::tree_sitter::Node;

pub(super) static HIGHLIGHT_QUERIES: HighlightQueries =
    HighlightQueries::new(&[tree_sitter_css::HIGHLIGHTS_QUERY]);

pub(super) fn annotate(context: NodeContext<'_, '_>) -> NodeAnnotation {
    let node = context.node;
    if node.kind() == "stylesheet" {
        return NodeAnnotation {
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }

    if matches!(node.kind(), "comment" | "js_comment") {
        let review = (context.parent_kind == Some("stylesheet"))
            .then(|| ReviewUnit::linewise(LayoutOwnership::None));
        return NodeAnnotation {
            review,
            channel: Some(ContentChannel::Comment),
            descendant_channel: Some(ContentChannel::Comment),
            prune_children: true,
            ..NodeAnnotation::default()
        };
    }

    if node.kind() == "import_statement" {
        let identity = first_named_child(node).map(|source| source.byte_range());
        let review = (context.parent_kind == Some("stylesheet"))
            .then(|| ReviewUnit::wiring(LayoutOwnership::None));
        return NodeAnnotation {
            review,
            identity,
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }

    if is_inline_statement(node.kind()) {
        let identity = statement_identity(node, context.source);
        let review = (context.parent_kind == Some("stylesheet"))
            .then(|| ReviewUnit::structural(LayoutOwnership::AdjacentBlankLines));
        return NodeAnnotation {
            review,
            identity,
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }

    if node.kind() == "declaration" {
        let identity = first_named_child(node).map(|child| child.byte_range());
        return NodeAnnotation {
            identity,
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }

    if context.parent_kind == Some("stylesheet") && node.is_named() {
        return NodeAnnotation {
            review: Some(ReviewUnit::linewise(LayoutOwnership::AdjacentBlankLines)),
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }

    NodeAnnotation::default()
}

pub(super) fn gap_channel(context: GapContext<'_, '_>) -> ContentChannel {
    if context.is_whitespace()
        && matches!(
            context.parent.kind(),
            "descendant_selector" | "string_content" | "string_value"
        )
    {
        return ContentChannel::Syntax;
    }
    context.default_channel()
}

fn first_named_child(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).next()
}

fn is_inline_statement(kind: &str) -> bool {
    matches!(
        kind,
        "at_rule"
            | "keyframes_statement"
            | "media_statement"
            | "rule_set"
            | "scope_statement"
            | "supports_statement"
    )
}

fn statement_identity(node: Node<'_>, source: &str) -> Option<std::ops::Range<usize>> {
    if node.kind() == "rule_set" {
        return first_named_child(node).map(|child| child.byte_range());
    }
    if node.kind() == "keyframes_statement" {
        return named_child_of_kind(node, "keyframes_name").map(|name| name.byte_range());
    }

    let block = named_child_of_kind(node, "block");
    let end = block.map_or(node.end_byte(), |block| block.start_byte());
    let mut end = end;
    while end > node.start_byte() && source.as_bytes()[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    Some(node.start_byte()..end)
}

fn named_child_of_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == kind)
}
