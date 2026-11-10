use super::lower::{HighlightQueries, NodeAnnotation, TextTreatment};
use super::{ContentChannel, Declares, ReviewRole, SiblingOrder, WrapperBoundary};
use ::tree_sitter::Node;

pub static HIGHLIGHT_QUERIES: HighlightQueries =
    HighlightQueries::new(&[tree_sitter_python::HIGHLIGHTS_QUERY]);

pub fn annotate(node: Node<'_>, parent_kind: Option<&str>) -> NodeAnnotation {
    let kind = node.kind();
    if kind == "module" {
        return NodeAnnotation {
            sibling_order: SiblingOrder::ByIdentity,
            wrapper_boundary: WrapperBoundary::Sealed,
            ..NodeAnnotation::default()
        };
    }
    if kind == "comment" {
        return NodeAnnotation {
            review: (parent_kind == Some("module")).then_some(ReviewRole::Content),
            text: TextTreatment::Verbatim(ContentChannel::Comment),
            ..NodeAnnotation::default()
        };
    }
    if kind == "line_continuation" {
        return NodeAnnotation {
            text: TextTreatment::Verbatim(ContentChannel::Layout),
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
            text: TextTreatment::Verbatim(ContentChannel::Syntax),
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
    let review = (parent_kind == Some("module") && node.is_named()).then_some(if wiring {
        ReviewRole::Wiring
    } else {
        ReviewRole::Content
    });
    // Module and class attributes are declarations; the same statement inside a
    // function body is a local.
    let declares = match kind {
        "decorated_definition" => Declares::DecoratedDefinition,
        "function_definition" | "class_definition" => Declares::Definition,
        "assignment" | "type_alias_statement" if parent_kind == Some("module") => {
            Declares::Definition
        }
        "assignment" | "type_alias_statement"
            if node
                .parent()
                .and_then(|block| block.parent())
                .is_some_and(|owner| owner.kind() == "class_definition") =>
        {
            Declares::Member
        }
        _ => Declares::Nothing,
    };
    NodeAnnotation {
        review,
        identity,
        declares,
        sibling_order: if owner || wiring {
            SiblingOrder::ByIdentity
        } else {
            SiblingOrder::SourceOrder
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
