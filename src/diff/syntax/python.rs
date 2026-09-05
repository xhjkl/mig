use super::lower::{HighlightQueries, NodeAnnotation};
use super::{ContentChannel, LayoutOwnership, ReviewUnit, SiblingMatching, WrapperBoundary};
use ::tree_sitter::Node;

pub static HIGHLIGHT_QUERIES: HighlightQueries =
    HighlightQueries::new(&[tree_sitter_python::HIGHLIGHTS_QUERY]);

pub fn annotate(node: Node<'_>, parent_kind: Option<&str>) -> NodeAnnotation {
    let kind = node.kind();
    if kind == "module" {
        return NodeAnnotation {
            sibling_matching: SiblingMatching::LocalIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }
    if kind == "comment" {
        return NodeAnnotation {
            review: (parent_kind == Some("module"))
                .then(|| ReviewUnit::linewise(LayoutOwnership::None)),
            channel: Some(ContentChannel::Comment),
            prune_children: true,
            ..NodeAnnotation::default()
        };
    }
    if kind == "line_continuation" {
        return NodeAnnotation {
            channel: Some(ContentChannel::Layout),
            prune_children: true,
            ..NodeAnnotation::default()
        };
    }
    if kind == "interpolation"
        && node
            .children(&mut node.walk())
            .any(|child| child.kind() == "=")
    {
        // Preserving the whole debug expression; Python includes its source spacing in the output.
        return NodeAnnotation {
            prune_children: true,
            ..NodeAnnotation::default()
        };
    }

    let definition = if kind == "decorated_definition" {
        node.child_by_field_name("definition").unwrap_or(node)
    } else {
        node
    };
    let identity = match kind {
        "decorated_definition" | "function_definition" | "class_definition" => {
            definition.child_by_field_name("name")
        }
        "import_from_statement" => node.child_by_field_name("module_name"),
        "import_statement" | "future_import_statement" => node.child_by_field_name("name"),
        "assignment" | "augmented_assignment" | "type_alias_statement" => {
            node.child_by_field_name("left")
        }
        "keyword_argument" => node.child_by_field_name("name"),
        "pair" => node.child_by_field_name("key"),
        "call" => node.child_by_field_name("function"),
        _ => None,
    }
    .map(|node| node.byte_range());
    let wiring = matches!(
        kind,
        "import_statement" | "import_from_statement" | "future_import_statement"
    );
    let owner = matches!(
        kind,
        "decorated_definition"
            | "function_definition"
            | "class_definition"
            | "assignment"
            | "augmented_assignment"
            | "type_alias_statement"
            | "pair"
            | "keyword_argument"
    );
    let review = (parent_kind == Some("module") && node.is_named()).then(|| {
        if wiring {
            ReviewUnit::wiring(LayoutOwnership::None)
        } else {
            ReviewUnit::structural(LayoutOwnership::AdjacentBlankLines)
        }
    });
    NodeAnnotation {
        review,
        identity,
        sibling_matching: if owner || wiring {
            SiblingMatching::LocalIdentity
        } else {
            SiblingMatching::OrderedSyntax
        },
        // Sealing suites preserves scope changes while letting equivalent indentation reflow.
        wrapper_boundary: if owner || wiring || kind == "block" {
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
        "string" | "string_content" | "format_specifier"
    )
}
