use super::lower::{HighlightQueries, NodeAnnotation};
use super::{ContentChannel, LayoutOwnership, ReviewUnit, SiblingMatching, WrapperBoundary};
use ::tree_sitter::Node;

pub static HIGHLIGHT_QUERIES: HighlightQueries =
    HighlightQueries::new(&[tree_sitter_go::HIGHLIGHTS_QUERY]);

pub fn annotate(node: Node<'_>, parent_kind: Option<&str>) -> NodeAnnotation {
    let kind = node.kind();
    if kind == "source_file" {
        return NodeAnnotation {
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }
    if kind == "comment" {
        return NodeAnnotation {
            review: (parent_kind == Some("source_file"))
                .then(|| ReviewUnit::linewise(LayoutOwnership::None)),
            channel: Some(ContentChannel::Comment),
            prune_children: true,
            ..NodeAnnotation::default()
        };
    }

    let wiring = matches!(kind, "import_declaration" | "package_clause");
    let owner = wiring
        || matches!(
            kind,
            "function_declaration"
                | "method_declaration"
                | "type_declaration"
                | "const_declaration"
                | "var_declaration"
                | "type_spec"
                | "type_alias"
                | "const_spec"
                | "var_spec"
                | "field_declaration"
                | "method_elem"
                | "import_spec"
                | "parameter_declaration"
                | "variadic_parameter_declaration"
                | "type_parameter_declaration"
                | "keyed_element"
        );
    let identity = if kind == "method_declaration" {
        let name = node.child_by_field_name("name");
        let receiver = node
            .child_by_field_name("receiver")
            .and_then(|receiver| receiver.named_child(0))
            .and_then(|parameter| parameter.child_by_field_name("type"));
        // Including the receiver type distinguishes same-named methods without using their bodies.
        receiver
            .zip(name)
            .map(|(receiver, name)| receiver.start_byte()..name.end_byte())
    } else {
        let identity = match kind {
            "import_spec" => node.child_by_field_name("path"),
            "import_declaration" => node
                .named_child(0)
                .and_then(|spec| spec.child_by_field_name("path")),
            "package_clause" => node.named_child(0),
            "type_declaration" | "const_declaration" | "var_declaration" => node
                .named_child(0)
                .and_then(|spec| spec.child_by_field_name("name")),
            "keyed_element" => node.child_by_field_name("key"),
            "call_expression" => node.child_by_field_name("function"),
            _ if owner => node.child_by_field_name("name"),
            _ => None,
        };
        identity.map(|identity| identity.byte_range())
    };
    NodeAnnotation {
        review: (parent_kind == Some("source_file") && node.is_named()).then(|| {
            if wiring {
                ReviewUnit::wiring(LayoutOwnership::None)
            } else {
                ReviewUnit::structural(LayoutOwnership::AdjacentBlankLines)
            }
        }),
        identity,
        sibling_matching: if owner {
            SiblingMatching::LocalIdentity
        } else {
            SiblingMatching::OrderedSyntax
        },
        wrapper_boundary: if owner {
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
        "raw_string_literal"
            | "raw_string_literal_content"
            | "interpreted_string_literal"
            | "interpreted_string_literal_content"
            | "rune_literal"
    )
}
