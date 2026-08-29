use super::lower::{GapContext, HighlightQueries, NodeAnnotation, NodeContext};
use super::{
    ContentChannel, GrammarSymbol, LayoutOwnership, LeafRole, NodeId, ReviewUnit, SiblingMatching,
    SyntaxKind, SyntaxNode, WrapperBoundary,
};
use crate::diff::SyntaxClass;
use crate::diff::source::Source;
use ::tree_sitter::Node;

pub(super) static HIGHLIGHT_QUERIES: HighlightQueries =
    HighlightQueries::new(&[tree_sitter_html::HIGHLIGHTS_QUERY]);
const OPAQUE_ELEMENTS: &[&str] = &[
    "iframe",
    "listing",
    "noembed",
    "noframes",
    "noscript",
    "plaintext",
    "pre",
    "script",
    "style",
    "textarea",
    "title",
    "xmp",
];
const PARAGRAPH_CLOSING_ELEMENTS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "details",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "main",
    "menu",
    "nav",
    "ol",
    "pre",
    "search",
    "section",
    "summary",
    "table",
    "ul",
];

pub(super) fn normalize_recovery(source: &Source<'_>, nodes: &mut [SyntaxNode]) -> bool {
    normalize_source_authored_paragraph_end_tags(source, nodes)
}

pub(super) fn annotate(context: NodeContext<'_, '_>) -> NodeAnnotation {
    let node = context.node;
    if node.kind() == "document" {
        return NodeAnnotation {
            review: Some(ReviewUnit::structural(LayoutOwnership::None)),
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }

    if is_plaintext_element(node, context.source) {
        let identity = tag_name(node).map(|name| name.byte_range());
        return NodeAnnotation {
            channel: Some(ContentChannel::Opaque),
            descendant_channel: Some(ContentChannel::Opaque),
            identity,
            sibling_matching: SiblingMatching::LocalIdentity,
            extent: Some(node.start_byte()..context.source.len()),
            prune_children: true,
            ..NodeAnnotation::default()
        };
    }

    if is_opaque_element(node, context.source) {
        let identity = tag_name(node).map(|name| name.byte_range());
        return NodeAnnotation {
            channel: Some(ContentChannel::Opaque),
            descendant_channel: Some(ContentChannel::Opaque),
            identity,
            sibling_matching: SiblingMatching::LocalIdentity,
            prune_children: true,
            ..NodeAnnotation::default()
        };
    }

    if is_element(node.kind()) || node.kind() == "self_closing_tag" {
        let identity = tag_name(node).map(|name| name.byte_range());
        return NodeAnnotation {
            identity,
            sibling_matching: SiblingMatching::LocalIdentity,
            ..NodeAnnotation::default()
        };
    }

    if node.kind() == "start_tag" {
        let identity = tag_name(node).map(|name| name.byte_range());
        return NodeAnnotation {
            identity,
            ..NodeAnnotation::default()
        };
    }

    if node.kind() == "text" {
        let text = &context.source[node.byte_range()];
        if text.chars().all(char::is_whitespace) && (text.contains('\n') || text.contains('\r')) {
            return NodeAnnotation {
                channel: Some(ContentChannel::Layout),
                ..NodeAnnotation::default()
            };
        }
    }

    if node.kind() == "comment" {
        return NodeAnnotation {
            channel: Some(ContentChannel::Comment),
            descendant_channel: Some(ContentChannel::Comment),
            prune_children: true,
            ..NodeAnnotation::default()
        };
    }

    NodeAnnotation::default()
}

pub(super) fn gap_channel(context: GapContext<'_, '_>) -> ContentChannel {
    if context.is_whitespace()
        && matches!(context.parent.kind(), "document" | "element")
        && !context.source[context.bytes.clone()].contains(['\n', '\r'])
    {
        // Same-line whitespace between elements contributes to rendered text.
        return ContentChannel::Syntax;
    }
    if context.is_whitespace()
        && matches!(
            context.parent.kind(),
            "attribute_value" | "quoted_attribute_value"
        )
    {
        return ContentChannel::Syntax;
    }
    context.default_channel()
}

/// Restore explicit paragraph closes displaced by a paragraph-closing block element.
fn normalize_source_authored_paragraph_end_tags(
    source: &Source<'_>,
    nodes: &mut [SyntaxNode],
) -> bool {
    if nodes.iter().any(|node| node.missing) {
        return false;
    }
    let recoveries = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            (node.kind == grammar_kind("ERROR", true)
                && node
                    .parent
                    .is_none_or(|parent| nodes[parent.index()].kind != grammar_kind("ERROR", true)))
                || node.kind == grammar_kind("erroneous_end_tag", true)
        })
        .map(|(index, _)| NodeId::new(index))
        .collect::<Vec<_>>();
    if recoveries.is_empty() {
        return true;
    }

    for error in recoveries.iter().copied() {
        if !canonicalize_recovered_paragraph_end_tag(source, nodes, error) {
            return false;
        }
        if !restore_source_authored_paragraph(source, nodes, &recoveries, error) {
            return false;
        }
    }
    true
}

fn canonicalize_recovered_paragraph_end_tag(
    source: &Source<'_>,
    nodes: &mut [SyntaxNode],
    error: NodeId,
) -> bool {
    let children = nodes[error.index()].children.clone();
    let [open, name, close] = children.as_slice() else {
        return false;
    };
    let spelling = |node: NodeId| {
        source
            .slice(nodes[node.index()].bytes.clone())
            .unwrap_or_default()
    };
    let name_kind = match nodes[error.index()].kind {
        kind if kind == grammar_kind("ERROR", true) => grammar_kind("ERROR", true),
        kind if kind == grammar_kind("erroneous_end_tag", true) => {
            grammar_kind("erroneous_end_tag_name", true)
        }
        _ => return false,
    };
    if nodes[open.index()].kind != grammar_kind("</", false)
        || spelling(*open) != "</"
        || nodes[name.index()].kind != name_kind
        || !spelling(*name).eq_ignore_ascii_case("p")
        || nodes[close.index()].kind != grammar_kind(">", false)
        || spelling(*close) != ">"
        || nodes[open.index()].bytes.start != nodes[error.index()].bytes.start
        || nodes[open.index()].bytes.end != nodes[name.index()].bytes.start
        || nodes[name.index()].bytes.end != nodes[close.index()].bytes.start
        || nodes[close.index()].bytes.end != nodes[error.index()].bytes.end
    {
        return false;
    }
    let Some(name_leaf) = nodes[name.index()].leaf.as_mut() else {
        return false;
    };
    nodes[error.index()].kind = grammar_kind("end_tag", true);
    name_leaf.role = LeafRole::Identifier;
    name_leaf.syntax = SyntaxClass::Identifier;
    nodes[name.index()].kind = grammar_kind("tag_name", true);
    true
}

fn restore_source_authored_paragraph(
    source: &Source<'_>,
    nodes: &mut [SyntaxNode],
    recoveries: &[NodeId],
    error: NodeId,
) -> bool {
    let Some(parent) = nodes[error.index()].parent else {
        return false;
    };
    let siblings = nodes[parent.index()].children.clone();
    let Some(error_index) = siblings.iter().position(|candidate| *candidate == error) else {
        return false;
    };
    let candidates = siblings[..error_index]
        .iter()
        .enumerate()
        .filter(|(_, candidate)| is_unclosed_paragraph(source, nodes, **candidate))
        .collect::<Vec<_>>();
    let [(candidate_index, candidate)] = candidates.as_slice() else {
        return false;
    };
    let candidate_index = *candidate_index;
    let candidate = **candidate;
    let displaced = &siblings[candidate_index + 1..error_index];
    let Some(first_content) = displaced
        .iter()
        .find(|child| !is_layout(nodes, **child))
        .copied()
    else {
        return false;
    };
    if !is_paragraph_closing_element(source, nodes, first_content) {
        return false;
    }
    let adopted_bytes = nodes[candidate.index()].bytes.end..nodes[error.index()].bytes.start;
    if recoveries.iter().copied().any(|recovery| {
        recovery != error
            && nodes[recovery.index()].bytes.start < adopted_bytes.end
            && adopted_bytes.start < nodes[recovery.index()].bytes.end
    }) {
        return false;
    }

    let adopted = siblings[candidate_index + 1..=error_index].to_vec();
    let mut cursor = nodes[candidate.index()].bytes.end;
    for child in &adopted {
        let child = &nodes[child.index()];
        if child.bytes.start != cursor {
            return false;
        }
        cursor = child.bytes.end;
    }

    let bytes = nodes[candidate.index()].bytes.start..cursor;
    let Some(lines) = source.line_coverage(bytes.clone()) else {
        return false;
    };
    nodes[candidate.index()].bytes = bytes;
    nodes[candidate.index()].source_envelope = nodes[candidate.index()].bytes.clone();
    nodes[candidate.index()].lines = lines;
    nodes[candidate.index()].children.extend(adopted.iter());
    for child in adopted {
        nodes[child.index()].parent = Some(candidate);
    }
    nodes[parent.index()]
        .children
        .drain(candidate_index + 1..=error_index);
    true
}

fn is_unclosed_paragraph(source: &Source<'_>, nodes: &[SyntaxNode], candidate: NodeId) -> bool {
    let node = &nodes[candidate.index()];
    if node.kind != grammar_kind("element", true)
        || node
            .identity
            .as_ref()
            .and_then(|bytes| source.slice(bytes.clone()))
            .is_none_or(|identity| !identity.eq_ignore_ascii_case("p"))
    {
        return false;
    }
    !node
        .children
        .iter()
        .any(|child| nodes[child.index()].kind == grammar_kind("end_tag", true))
}

fn is_layout(nodes: &[SyntaxNode], node: NodeId) -> bool {
    nodes[node.index()]
        .leaf
        .is_some_and(|leaf| leaf.channel == ContentChannel::Layout)
}

fn is_paragraph_closing_element(
    source: &Source<'_>,
    nodes: &[SyntaxNode],
    candidate: NodeId,
) -> bool {
    let node = &nodes[candidate.index()];
    node.kind == grammar_kind("element", true)
        && node
            .identity
            .as_ref()
            .and_then(|bytes| source.slice(bytes.clone()))
            .is_some_and(|identity| {
                PARAGRAPH_CLOSING_ELEMENTS
                    .iter()
                    .any(|element| identity.eq_ignore_ascii_case(element))
            })
}

fn grammar_kind(kind: &str, named: bool) -> SyntaxKind {
    let language: ::tree_sitter::Language = tree_sitter_html::LANGUAGE.into();
    SyntaxKind::Grammar(GrammarSymbol::new(language.id_for_node_kind(kind, named)))
}

fn is_element(kind: &str) -> bool {
    matches!(kind, "element" | "script_element" | "style_element")
}

fn is_opaque_element(node: Node<'_>, source: &str) -> bool {
    if matches!(node.kind(), "script_element" | "style_element") {
        return true;
    }
    if node.kind() != "element" {
        return false;
    }

    let name = tag_name(node);
    let Some(name) = name else {
        return false;
    };
    let name = &source[name.byte_range()];
    OPAQUE_ELEMENTS
        .iter()
        .any(|opaque| name.eq_ignore_ascii_case(opaque))
}

fn is_plaintext_element(node: Node<'_>, source: &str) -> bool {
    if node.kind() != "element" {
        return false;
    }
    let name = tag_name(node);
    let Some(name) = name else {
        return false;
    };
    source[name.byte_range()].eq_ignore_ascii_case("plaintext")
}

fn tag_name(node: Node<'_>) -> Option<Node<'_>> {
    if matches!(node.kind(), "start_tag" | "self_closing_tag") {
        return named_child_of_kind(node, "tag_name");
    }

    let mut cursor = node.walk();
    let tag = node
        .named_children(&mut cursor)
        .find(|child| matches!(child.kind(), "start_tag" | "self_closing_tag"))?;
    named_child_of_kind(tag, "tag_name")
}

fn named_child_of_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == kind)
}
