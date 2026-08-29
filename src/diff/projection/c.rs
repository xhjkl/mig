use super::tree_sitter::{
    self, Adapter, GapContext, HighlightQueries, NodeAnnotation, NodeContext, ProjectFailure,
};
use super::{
    ContentChannel, CorrespondenceRole, Language, LayoutOwnership, Projection, ReviewMode,
    ReviewUnit,
};
use crate::diff::source::Source;
use ::tree_sitter::{Language as TreeSitterLanguage, Node};

static HIGHLIGHT_QUERIES: HighlightQueries =
    HighlightQueries::new(&[tree_sitter_c::HIGHLIGHT_QUERY]);

pub(super) fn project<'source>(
    source: Source<'source>,
) -> Result<Projection<'source>, ProjectFailure<'source>> {
    tree_sitter::project(source, &CAdapter)
}

struct CAdapter;

impl Adapter for CAdapter {
    fn language(&self) -> TreeSitterLanguage {
        tree_sitter_c::LANGUAGE.into()
    }

    fn projected_language(&self) -> Language {
        Language::C
    }

    fn highlight_queries(&self) -> &'static HighlightQueries {
        &HIGHLIGHT_QUERIES
    }

    fn annotate(&self, context: NodeContext<'_, '_>) -> NodeAnnotation {
        let node = context.node;
        if node.kind() == "translation_unit" {
            return NodeAnnotation {
                correspondence: CorrespondenceRole::HardOwner,
                ..NodeAnnotation::default()
            };
        }

        if node.kind() == "comment" {
            let review = (context.parent_kind == Some("translation_unit"))
                .then(|| ReviewUnit::new(ReviewMode::Linewise, LayoutOwnership::None));
            return NodeAnnotation {
                review,
                channel: Some(ContentChannel::Comment),
                descendant_channel: Some(ContentChannel::Comment),
                prune_children: true,
                ..NodeAnnotation::default()
            };
        }

        if is_preprocessor(node.kind()) {
            let review = (context.parent_kind == Some("translation_unit")).then(|| {
                ReviewUnit::new(
                    preprocessor_mode(node.kind()),
                    LayoutOwnership::AdjacentBlankLines,
                )
            });
            return NodeAnnotation {
                review,
                identity: preprocessor_identity(node).map(|identity| identity.byte_range()),
                correspondence: CorrespondenceRole::HardOwner,
                ..NodeAnnotation::default()
            };
        }

        if is_definition(node.kind()) {
            let review = (context.parent_kind == Some("translation_unit")).then(|| {
                ReviewUnit::new(ReviewMode::Structural, LayoutOwnership::AdjacentBlankLines)
            });
            return NodeAnnotation {
                review,
                identity: definition_identity(node).map(|identity| identity.byte_range()),
                correspondence: CorrespondenceRole::HardOwner,
                ..NodeAnnotation::default()
            };
        }

        if context.parent_kind == Some("translation_unit") && node.is_named() {
            return NodeAnnotation {
                review: Some(ReviewUnit::new(
                    ReviewMode::Linewise,
                    LayoutOwnership::AdjacentBlankLines,
                )),
                correspondence: CorrespondenceRole::HardOwner,
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
        NodeAnnotation {
            identity,
            correspondence: if is_semantic_owner(node.kind()) {
                CorrespondenceRole::HardOwner
            } else {
                CorrespondenceRole::Transparent
            },
            ..NodeAnnotation::default()
        }
    }

    fn gap_channel(&self, context: GapContext<'_, '_>) -> ContentChannel {
        if context.is_whitespace()
            && matches!(
                context.parent.kind(),
                "char_literal" | "string_literal" | "system_lib_string"
            )
        {
            return ContentChannel::Syntax;
        }
        context.default_channel()
    }
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

fn preprocessor_mode(kind: &str) -> ReviewMode {
    if matches!(kind, "preproc_if" | "preproc_ifdef") {
        ReviewMode::Structural
    } else {
        ReviewMode::Compact
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_and_calls_expose_local_semantic_identities() {
        let source = Source::new(concat!(
            "struct Alpha { int beta; };\n",
            "static int *gamma(const struct Alpha *alpha) {\n",
            "    return alpha->zeta(delta(alpha->beta));\n",
            "}\n",
        ));
        let Ok(projection) = project(source) else {
            panic!("C source must project");
        };
        let identities = projection
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                matches!(
                    node.kind,
                    "call_expression" | "field_declaration" | "function_definition"
                )
            })
            .map(|(index, node)| {
                (
                    node.kind,
                    projection.identity_text(super::super::NodeId::new(index)),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            identities,
            [
                ("field_declaration", Some("beta")),
                ("function_definition", Some("gamma")),
                ("call_expression", Some("zeta")),
                ("call_expression", Some("delta")),
            ]
        );
    }

    #[test]
    fn top_level_comments_and_preprocessor_forms_are_independent_review_units() {
        let source = Source::new(concat!(
            "#include <alpha.h>\n",
            "#define BETA 1\n",
            "// gamma\n",
        ));
        let Ok(projection) = project(source) else {
            panic!("C source must project");
        };
        let units = projection
            .review_units()
            .map(|(id, node)| {
                (
                    node.kind,
                    projection.identity_text(id),
                    node.leaf.map(|leaf| leaf.channel),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            units,
            [
                ("preproc_include", Some("<alpha.h>"), None),
                ("preproc_def", Some("BETA"), None),
                ("comment", None, Some(ContentChannel::Comment)),
            ]
        );
    }
}
