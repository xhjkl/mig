use super::lower::{HighlightQueries, NodeAnnotation, grammar_symbol};
use super::{
    ContentChannel, Grammar, LayoutOwnership, LeafRole, NodeId, ReviewUnit, SiblingMatching,
    SyntaxNode, WrapperBoundary,
};
use crate::diff::SyntaxClass;
use crate::diff::source::Source;
use ::tree_sitter::Node;

pub static HIGHLIGHT_QUERIES: HighlightQueries =
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

pub fn annotate(node: Node<'_>, source: &str) -> NodeAnnotation {
    if node.kind() == "document" {
        return NodeAnnotation {
            review: Some(ReviewUnit::structural(LayoutOwnership::None)),
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }

    if let Some((name, absorbs_rest)) = opaque_element(node, source) {
        let identity = name.map(|name| name.byte_range());
        return NodeAnnotation {
            channel: Some(ContentChannel::Opaque),
            identity,
            sibling_matching: SiblingMatching::LocalIdentity,
            extent: absorbs_rest.then(|| node.start_byte()..source.len()),
            prune_children: true,
            ..NodeAnnotation::default()
        };
    }

    if matches!(
        node.kind(),
        "element" | "script_element" | "style_element" | "self_closing_tag"
    ) {
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
        let text = &source[node.byte_range()];
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
            prune_children: true,
            ..NodeAnnotation::default()
        };
    }

    NodeAnnotation::default()
}

pub fn whitespace_is_syntax(parent_kind: &str, spelling: &str) -> bool {
    // Same-line whitespace between elements contributes to rendered text.
    matches!(parent_kind, "document" | "element") && !spelling.contains(['\n', '\r'])
        || matches!(parent_kind, "attribute_value" | "quoted_attribute_value")
}

/// Restore explicit paragraph closes displaced by a paragraph-closing block element.
pub fn normalize_recovery(source: &Source<'_>, nodes: &mut [SyntaxNode]) -> bool {
    if nodes.iter().any(|node| node.missing) {
        return false;
    }
    let recoveries = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            (node.kind == grammar_symbol(Grammar::Html, "ERROR", true)
                && node.parent.is_none_or(|parent| {
                    nodes[parent.index()].kind != grammar_symbol(Grammar::Html, "ERROR", true)
                }))
                || node.kind == grammar_symbol(Grammar::Html, "erroneous_end_tag", true)
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
        kind if kind == grammar_symbol(Grammar::Html, "ERROR", true) => {
            grammar_symbol(Grammar::Html, "ERROR", true)
        }
        kind if kind == grammar_symbol(Grammar::Html, "erroneous_end_tag", true) => {
            grammar_symbol(Grammar::Html, "erroneous_end_tag_name", true)
        }
        _ => return false,
    };
    if nodes[open.index()].kind != grammar_symbol(Grammar::Html, "</", false)
        || spelling(*open) != "</"
        || nodes[name.index()].kind != name_kind
        || !spelling(*name).eq_ignore_ascii_case("p")
        || nodes[close.index()].kind != grammar_symbol(Grammar::Html, ">", false)
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
    nodes[error.index()].kind = grammar_symbol(Grammar::Html, "end_tag", true);
    name_leaf.role = LeafRole::Identifier;
    name_leaf.syntax = SyntaxClass::Identifier;
    nodes[name.index()].kind = grammar_symbol(Grammar::Html, "tag_name", true);
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
    if node.kind != grammar_symbol(Grammar::Html, "element", true)
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
        .any(|child| nodes[child.index()].kind == grammar_symbol(Grammar::Html, "end_tag", true))
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
    node.kind == grammar_symbol(Grammar::Html, "element", true)
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

fn opaque_element<'tree>(node: Node<'tree>, source: &str) -> Option<(Option<Node<'tree>>, bool)> {
    let name = tag_name(node);
    if matches!(node.kind(), "script_element" | "style_element") {
        return Some((name, false));
    }
    if node.kind() != "element" {
        return None;
    }

    let name = name?;
    let spelling = &source[name.byte_range()];
    let opaque = OPAQUE_ELEMENTS
        .iter()
        .any(|opaque| spelling.eq_ignore_ascii_case(opaque));
    if !opaque {
        return None;
    }
    Some((Some(name), spelling.eq_ignore_ascii_case("plaintext")))
}

fn tag_name(node: Node<'_>) -> Option<Node<'_>> {
    let tag = if matches!(node.kind(), "start_tag" | "self_closing_tag") {
        node
    } else {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .find(|child| matches!(child.kind(), "start_tag" | "self_closing_tag"))?
    };
    let mut cursor = tag.walk();
    tag.named_children(&mut cursor)
        .find(|child| child.kind() == "tag_name")
}
