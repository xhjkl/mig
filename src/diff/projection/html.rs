use super::tree_sitter::{
    self, Adapter, GapContext, HighlightQueries, NodeAnnotation, NodeContext, ProjectFailure,
};
use super::{ContentChannel, Language, LayoutOwnership, Projection, ReviewTreatment, ReviewUnit};
use crate::diff::source::Source;
use ::tree_sitter::{Language as TreeSitterLanguage, Node};

static HIGHLIGHT_QUERIES: HighlightQueries =
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

pub(super) fn project<'source>(
    source: Source<'source>,
) -> Result<Projection<'source>, ProjectFailure<'source>> {
    tree_sitter::project(source, &HtmlAdapter)
}

struct HtmlAdapter;

impl Adapter for HtmlAdapter {
    fn language(&self) -> TreeSitterLanguage {
        tree_sitter_html::LANGUAGE.into()
    }

    fn projected_language(&self) -> Language {
        Language::Html
    }

    fn highlight_queries(&self) -> &'static HighlightQueries {
        &HIGHLIGHT_QUERIES
    }

    fn annotate(&self, context: NodeContext<'_, '_>) -> NodeAnnotation {
        let node = context.node;
        if node.kind() == "document" {
            return NodeAnnotation {
                review: Some(ReviewUnit::stationary(
                    ReviewTreatment::Linewise,
                    LayoutOwnership::None,
                )),
                ..NodeAnnotation::default()
            };
        }

        if is_plaintext_element(node, context.source) {
            let identity = tag_name(node).map(|name| name.byte_range());
            return NodeAnnotation {
                channel: Some(ContentChannel::Opaque),
                descendant_channel: Some(ContentChannel::Opaque),
                identity,
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
                prune_children: true,
                ..NodeAnnotation::default()
            };
        }

        if is_element(node.kind()) || matches!(node.kind(), "start_tag" | "self_closing_tag") {
            let identity = tag_name(node).map(|name| name.byte_range());
            return NodeAnnotation {
                identity,
                ..NodeAnnotation::default()
            };
        }

        if node.kind() == "text" {
            let text = &context.source[node.byte_range()];
            if text.chars().all(char::is_whitespace) && (text.contains('\n') || text.contains('\r'))
            {
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

    fn gap_channel(&self, context: GapContext<'_, '_>) -> ContentChannel {
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
