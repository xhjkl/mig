use super::lower::{HighlightQueries, NodeAnnotation};
use super::{ContentChannel, LayoutOwnership, ReviewUnit, SiblingMatching, WrapperBoundary};
use ::tree_sitter::Node;

pub static HIGHLIGHT_QUERIES: HighlightQueries =
    HighlightQueries::new(&[tree_sitter_c::HIGHLIGHT_QUERY]);

pub fn annotate(node: Node<'_>, parent_kind: Option<&str>) -> NodeAnnotation {
    if node.kind() == "translation_unit" {
        return NodeAnnotation {
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }

    if node.kind() == "comment" {
        let review = (parent_kind == Some("translation_unit"))
            .then(|| ReviewUnit::linewise(LayoutOwnership::None));
        return NodeAnnotation {
            review,
            channel: Some(ContentChannel::Comment),
            prune_children: true,
            ..NodeAnnotation::default()
        };
    }

    if is_preprocessor(node.kind()) {
        let review = (parent_kind == Some("translation_unit")).then(|| {
            if matches!(node.kind(), "preproc_if" | "preproc_ifdef") {
                ReviewUnit::structural(LayoutOwnership::AdjacentBlankLines)
            } else {
                ReviewUnit::wiring(LayoutOwnership::AdjacentBlankLines)
            }
        });
        return NodeAnnotation {
            review,
            identity: preprocessor_identity(node).map(|identity| identity.byte_range()),
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }

    if is_definition(node.kind()) {
        let review = (parent_kind == Some("translation_unit"))
            .then(|| ReviewUnit::structural(LayoutOwnership::AdjacentBlankLines));
        return NodeAnnotation {
            review,
            identity: definition_identity(node).map(|identity| identity.byte_range()),
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }

    if parent_kind == Some("translation_unit") && node.is_named() {
        return NodeAnnotation {
            review: Some(ReviewUnit::linewise(LayoutOwnership::AdjacentBlankLines)),
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }

    let identity = match node.kind() {
        "struct_specifier" | "union_specifier" | "enum_specifier" => {
            node.child_by_field_name("name")
        }
        "field_declaration" | "parameter_declaration" => declaration_identity(node),
        "enumerator" => node.child_by_field_name("name"),
        "call_expression" => call_identity(node),
        "binary_expression" => node.child_by_field_name("operator"),
        _ => None,
    }
    .map(|identity| identity.byte_range());
    let semantic_owner = is_semantic_owner(node.kind());
    NodeAnnotation {
        identity,
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
        "char_literal" | "string_literal" | "system_lib_string"
    )
}

fn is_definition(kind: &str) -> bool {
    matches!(
        kind,
        "function_definition" | "declaration" | "type_definition"
    )
}

fn is_semantic_owner(kind: &str) -> bool {
    is_definition(kind)
        || matches!(
            kind,
            "enum_specifier"
                | "enumerator"
                | "field_declaration"
                | "parameter_declaration"
                | "struct_specifier"
                | "union_specifier"
        )
}

fn is_preprocessor(kind: &str) -> bool {
    matches!(
        kind,
        "preproc_call"
            | "preproc_def"
            | "preproc_elif"
            | "preproc_elifdef"
            | "preproc_else"
            | "preproc_function_def"
            | "preproc_if"
            | "preproc_ifdef"
            | "preproc_include"
    )
}

fn preprocessor_identity<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    node.child_by_field_name("path")
        .or_else(|| node.child_by_field_name("name"))
        .or_else(|| node.child_by_field_name("directive"))
        .or_else(|| node.child_by_field_name("condition"))
}

fn definition_identity<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    declaration_identity(node).or_else(|| {
        node.child_by_field_name("type")
            .and_then(|specifier| specifier.child_by_field_name("name"))
    })
}

fn declaration_identity<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    let declarator = node.child_by_field_name("declarator")?;
    declarator_identity(declarator)
}

fn declarator_identity<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    if matches!(
        node.kind(),
        "field_identifier" | "identifier" | "type_identifier"
    ) {
        return Some(node);
    }
    node.child_by_field_name("declarator")
        .and_then(declarator_identity)
}

fn call_identity<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    let function = node.child_by_field_name("function")?;
    function
        .child_by_field_name("field")
        .or_else(|| matches!(function.kind(), "identifier").then_some(function))
        .or_else(|| first_identifier(function))
}

fn first_identifier(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).find_map(|child| {
        matches!(child.kind(), "field_identifier" | "identifier")
            .then_some(child)
            .or_else(|| first_identifier(child))
    })
}
