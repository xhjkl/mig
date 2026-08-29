use super::lower::{GapContext, HighlightQueries, NodeAnnotation, NodeContext};
use super::{ContentChannel, LayoutOwnership, ReviewUnit, SiblingMatching, WrapperBoundary};
use ::tree_sitter::Node;

pub(super) static TYPESCRIPT_HIGHLIGHTS: HighlightQueries = HighlightQueries::new(&[
    tree_sitter_javascript::HIGHLIGHT_QUERY,
    tree_sitter_typescript::HIGHLIGHTS_QUERY,
]);
pub(super) static TSX_HIGHLIGHTS: HighlightQueries = HighlightQueries::new(&[
    tree_sitter_javascript::HIGHLIGHT_QUERY,
    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
    tree_sitter_typescript::HIGHLIGHTS_QUERY,
]);
pub(super) static JAVASCRIPT_HIGHLIGHTS: HighlightQueries =
    HighlightQueries::new(&[tree_sitter_javascript::HIGHLIGHT_QUERY]);
pub(super) static JSX_HIGHLIGHTS: HighlightQueries = HighlightQueries::new(&[
    tree_sitter_javascript::HIGHLIGHT_QUERY,
    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
]);

pub(super) fn annotate(context: NodeContext<'_, '_>) -> NodeAnnotation {
    let node = context.node;
    if node.kind() == "program" {
        return NodeAnnotation {
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }

    if matches!(node.kind(), "comment" | "html_comment") {
        let review = (context.parent_kind == Some("program"))
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
        let identity = node
            .child_by_field_name("source")
            .map(|source| source.byte_range());
        let review = (context.parent_kind == Some("program"))
            .then(|| ReviewUnit::wiring(LayoutOwnership::None));
        return NodeAnnotation {
            review,
            identity,
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }

    if is_jsx_element(node.kind()) {
        let identity = jsx_name(node).map(|name| name.byte_range());
        return NodeAnnotation {
            identity,
            sibling_matching: SiblingMatching::LocalIdentity,
            ..NodeAnnotation::default()
        };
    }

    if is_declaration(node.kind()) {
        let identity = declaration_identity(node).map(|name| name.byte_range());
        let review = (context.parent_kind == Some("program"))
            .then(|| ReviewUnit::structural(LayoutOwnership::AdjacentBlankLines));
        return NodeAnnotation {
            review,
            identity,
            sibling_matching: if is_declaration_owner(node.kind()) {
                SiblingMatching::LocalIdentity
            } else {
                SiblingMatching::OrderedSyntax
            },
            wrapper_boundary: if is_declaration_owner(node.kind()) {
                WrapperBoundary::Sealed
            } else {
                WrapperBoundary::Traversable
            },
            ..NodeAnnotation::default()
        };
    }

    if context.parent_kind == Some("program") && node.is_named() {
        return NodeAnnotation {
            review: Some(ReviewUnit::linewise(LayoutOwnership::AdjacentBlankLines)),
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }

    let identity = if is_member(node.kind()) {
        direct_identity(node)
    } else if matches!(node.kind(), "call_expression" | "new_expression") {
        call_identity(node)
    } else if node.kind() == "binary_expression" {
        node.child_by_field_name("operator")
    } else {
        None
    }
    .map(|identity| identity.byte_range());
    let member = is_member(node.kind());
    NodeAnnotation {
        identity,
        sibling_matching: if member || is_local_container(node.kind()) {
            SiblingMatching::LocalIdentity
        } else {
            SiblingMatching::OrderedSyntax
        },
        wrapper_boundary: if member {
            WrapperBoundary::Sealed
        } else {
            WrapperBoundary::Traversable
        },
        ..NodeAnnotation::default()
    }
}

pub(super) fn gap_channel(context: GapContext<'_, '_>) -> ContentChannel {
    if context.is_whitespace()
        && matches!(
            context.parent.kind(),
            "string"
                | "string_fragment"
                | "template_literal_type"
                | "template_string"
                | "template_type"
        )
    {
        return ContentChannel::Syntax;
    }
    context.default_channel()
}

fn declaration_identity<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    let identity = direct_identity(node);
    if identity.is_some() {
        return identity;
    }

    let declaration = node.child_by_field_name("declaration");
    if let Some(declaration) = declaration {
        let identity = descendant_identity(declaration);
        if identity.is_some() {
            return identity;
        }
    }

    let source = node.child_by_field_name("source");
    if source.is_some() {
        return source;
    }
    descendant_identity(node)
}

fn descendant_identity<'tree>(root: Node<'tree>) -> Option<Node<'tree>> {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        let identity = direct_identity(node);
        if identity.is_some() {
            return identity;
        }

        let mut cursor = node.walk();
        let children = node.named_children(&mut cursor).collect::<Vec<_>>();
        pending.extend(children.into_iter().rev());
    }
    None
}

fn direct_identity(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("name")
        .or_else(|| node.child_by_field_name("property"))
        .or_else(|| node.child_by_field_name("key"))
}

fn call_identity(node: Node<'_>) -> Option<Node<'_>> {
    let function = node
        .child_by_field_name("function")
        .or_else(|| node.child_by_field_name("constructor"))?;
    function
        .child_by_field_name("property")
        .or_else(|| function.child_by_field_name("name"))
        .or_else(|| matches!(function.kind(), "identifier").then_some(function))
}

fn jsx_name<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    let opening = node.child_by_field_name("open_tag");
    let opening = opening.unwrap_or(node);
    opening.child_by_field_name("name")
}

fn is_jsx_element(kind: &str) -> bool {
    matches!(kind, "jsx_element" | "jsx_self_closing_element")
}

fn is_declaration(kind: &str) -> bool {
    matches!(
        kind,
        "abstract_class_declaration"
            | "ambient_declaration"
            | "class_declaration"
            | "enum_declaration"
            | "export_statement"
            | "function_declaration"
            | "function_signature"
            | "generator_function_declaration"
            | "interface_declaration"
            | "internal_module"
            | "lexical_declaration"
            | "method_definition"
            | "method_signature"
            | "module"
            | "type_alias_declaration"
            | "using_declaration"
            | "variable_declaration"
    )
}

fn is_declaration_owner(kind: &str) -> bool {
    is_declaration(kind) && !matches!(kind, "ambient_declaration" | "export_statement")
}

fn is_member(kind: &str) -> bool {
    matches!(
        kind,
        "pair" | "property_signature" | "public_field_definition"
    )
}

/// Anonymous structures whose child payload must not choose a sibling occurrence.
fn is_local_container(kind: &str) -> bool {
    matches!(kind, "object" | "object_pattern" | "object_type")
}
