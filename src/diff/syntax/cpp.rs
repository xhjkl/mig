use super::c;
use super::lower::{HighlightQueries, NodeAnnotation};
use super::{LayoutOwnership, ReviewUnit, SiblingMatching, WrapperBoundary};
use ::tree_sitter::Node;

pub static HIGHLIGHT_QUERIES: HighlightQueries = HighlightQueries::new(&[
    tree_sitter_c::HIGHLIGHT_QUERY,
    tree_sitter_cpp::HIGHLIGHT_QUERY,
]);

pub fn annotate(node: Node<'_>, parent_kind: Option<&str>) -> NodeAnnotation {
    let mut annotation = c::annotate(node, parent_kind);
    if !matches!(
        node.kind(),
        "namespace_definition"
            | "namespace_alias_definition"
            | "class_specifier"
            | "struct_specifier"
            | "union_specifier"
            | "enum_specifier"
            | "template_declaration"
            | "alias_declaration"
            | "using_declaration"
            | "concept_definition"
            | "friend_declaration"
            | "function_definition"
            | "declaration"
            | "field_declaration"
            | "type_definition"
    ) {
        return annotation;
    }

    annotation.identity = declaration_identity(node).map(|identity| identity.byte_range());
    annotation.sibling_matching = SiblingMatching::LocalIdentity;
    annotation.wrapper_boundary = WrapperBoundary::Sealed;
    annotation.review = (parent_kind == Some("translation_unit")).then(|| {
        if matches!(
            node.kind(),
            "using_declaration" | "namespace_alias_definition"
        ) {
            ReviewUnit::wiring(LayoutOwnership::None)
        } else {
            ReviewUnit::structural(LayoutOwnership::AdjacentBlankLines)
        }
    });
    annotation
}

pub fn whitespace_is_syntax(parent_kind: &str) -> bool {
    c::whitespace_is_syntax(parent_kind)
        || matches!(parent_kind, "raw_string_literal" | "raw_string_content")
}

fn declaration_identity(mut node: Node<'_>) -> Option<Node<'_>> {
    loop {
        match node.kind() {
            // Keeping the signature distinguishes overloads without consulting their bodies.
            "function_declarator"
            | "operator_cast"
            | "qualified_identifier"
            | "identifier"
            | "field_identifier"
            | "type_identifier" => return Some(node),
            "template_declaration" | "friend_declaration" => {
                node = node.named_children(&mut node.walk()).find(|child| {
                    !matches!(
                        child.kind(),
                        "template_parameter_list" | "requires_clause" | "comment"
                    )
                })?;
            }
            "using_declaration" => return node.named_child(0),
            _ => {
                if let Some(name) = node.child_by_field_name("name") {
                    return Some(name);
                }
                node = node
                    .child_by_field_name("declarator")
                    .or_else(|| node.child_by_field_name("type"))?;
            }
        }
    }
}
