//! Language-neutral source projections consumed by correspondence.

mod css;
mod html;
mod line;
mod rust;
mod tree_sitter;
mod typescript;

use super::SyntaxClass;
use super::source::Source;
use anyhow::{Context, Result};
use std::ops::Range;
use std::path::Path;

/// Stable arena handle; projections never expose parser-owned node handles.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct NodeId(usize);

impl NodeId {
    pub(super) const fn new(index: usize) -> Self {
        Self(index)
    }

    /// Arena position, useful for dense correspondence tables.
    pub(crate) const fn index(self) -> usize {
        self.0
    }
}

/// Parser chosen from the file path, or the universal line projection.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Language {
    Lines,
    Rust,
    Html,
    Css,
    TypeScript,
    Tsx,
    JavaScript,
    Jsx,
}

/// Why a pair could not safely use its selected syntax grammar.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum FallbackReason {
    Generated,
    Unsupported,
    ParseError(ParseSide),
    SyntaxComplexity(ParseSide),
    /// A concrete source fact, such as a terminator edit, is absent from the CST.
    SourceExactness,
}

/// Revision whose parse prevented symmetric syntax correspondence.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ParseSide {
    Before,
    After,
    Both,
}

/// Whether an arena came from a complete grammar parse or an exact line fallback.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ProjectionHealth {
    Parsed,
    Fallback(FallbackReason),
}

/// Whether leaf payload participates as syntax, commentary, or exact opaque text.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ContentChannel {
    Syntax,
    Comment,
    Opaque,
    /// Parser-omitted formatting that must remain renderable but not semantic.
    Layout,
}

/// Planner treatment requested by a semantic review boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ReviewTreatment {
    Inline,
    Linewise,
    Compact,
}

/// Whether correspondence may claim this boundary independently of its parent.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Tracking {
    Track,
    Ignore,
}

/// Whether stable-order analysis may classify a review boundary as moved.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Movement {
    Track,
    Ignore,
}

/// Context outside a unit that belongs in its review hunk.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Frame {
    None,
    AdjacentBlankLines,
}

/// Language policy attached to one syntax node without leaking grammar types.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewUnit {
    pub(crate) treatment: ReviewTreatment,
    pub(crate) tracking: Tracking,
    pub(crate) movement: Movement,
    pub(crate) frame: Frame,
}

impl ReviewUnit {
    pub(super) fn movable(treatment: ReviewTreatment, frame: Frame) -> Self {
        Self {
            treatment,
            tracking: Tracking::Track,
            movement: Movement::Track,
            frame,
        }
    }

    pub(super) fn stationary(treatment: ReviewTreatment, frame: Frame) -> Self {
        Self {
            treatment,
            tracking: Tracking::Track,
            movement: Movement::Ignore,
            frame,
        }
    }

    pub(super) fn ignored(treatment: ReviewTreatment) -> Self {
        Self {
            treatment,
            tracking: Tracking::Ignore,
            movement: Movement::Ignore,
            frame: Frame::None,
        }
    }
}

/// Concrete CST leaf metadata; payload remains borrowed through `Projection::source`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Leaf {
    pub(crate) syntax: SyntaxClass,
    pub(crate) channel: ContentChannel,
}

/// One neutral CST occurrence with exact source geometry and ordered containment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SyntaxNode {
    pub(crate) kind: &'static str,
    pub(crate) field: Option<&'static str>,
    pub(crate) bytes: Range<usize>,
    pub(crate) lines: Range<usize>,
    pub(crate) parent: Option<NodeId>,
    pub(crate) children: Vec<NodeId>,
    pub(crate) leaf: Option<Leaf>,
    /// Exact source spelling used to disambiguate same-shaped graph nodes.
    pub(crate) identity: Option<Range<usize>>,
    pub(crate) review: Option<ReviewUnit>,
    pub(crate) named: bool,
    pub(crate) extra: bool,
    pub(crate) missing: bool,
}

/// Source plus a parser-independent arena suitable for graph correspondence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Projection<'source> {
    pub(crate) source: Source<'source>,
    pub(crate) language: Language,
    pub(crate) health: ProjectionHealth,
    pub(crate) root: NodeId,
    pub(crate) nodes: Vec<SyntaxNode>,
    /// Source-ordered leaves keep presentation and source certification linear.
    leaves: Vec<NodeId>,
}

impl<'source> Projection<'source> {
    /// Complete neutral arena plus its source-order acceleration index.
    pub(super) fn from_nodes(
        source: Source<'source>,
        language: Language,
        health: ProjectionHealth,
        root: NodeId,
        nodes: Vec<SyntaxNode>,
    ) -> Self {
        let leaves = nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| node.leaf.map(|_| NodeId::new(index)))
            .collect::<Vec<_>>();
        debug_assert!(leaves.windows(2).all(|pair| {
            nodes[pair[0].index()].bytes.start <= nodes[pair[1].index()].bytes.start
        }));
        Self {
            source,
            language,
            health,
            root,
            nodes,
            leaves,
        }
    }

    /// Arena node for a stable projection-local handle.
    pub(crate) fn node(&self, id: NodeId) -> &SyntaxNode {
        &self.nodes[id.index()]
    }

    /// Original payload of a concrete leaf, including a zero-width missing token.
    pub(crate) fn leaf_text(&self, id: NodeId) -> Option<&'source str> {
        let node = self.node(id);
        node.leaf?;
        self.source.slice(node.bytes.clone())
    }

    /// Source spelling selected by a graph node as its correspondence identity.
    pub(crate) fn identity_text(&self, id: NodeId) -> Option<&'source str> {
        let identity = self.node(id).identity.clone()?;
        self.source.slice(identity)
    }

    /// Concrete leaves overlapping one byte range, in source order.
    pub(crate) fn leaves_in(&self, bytes: Range<usize>) -> impl Iterator<Item = &SyntaxNode> {
        let start = self
            .leaves
            .partition_point(|id| self.node(*id).bytes.end <= bytes.start);
        self.leaves[start..]
            .iter()
            .map(|id| self.node(*id))
            .take_while(move |node| node.bytes.start < bytes.end)
    }

    /// Arena descendants in source preorder, excluding the supplied root.
    pub(crate) fn descendants(&self, root: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let mut descendants = Vec::new();
        let mut pending = self
            .node(root)
            .children
            .iter()
            .rev()
            .copied()
            .collect::<Vec<_>>();
        while let Some(node) = pending.pop() {
            descendants.push(node);
            pending.extend(self.node(node).children.iter().rev().copied());
        }
        descendants.into_iter()
    }

    /// Tracked boundaries in source preorder.
    pub(crate) fn tracked_units(&self) -> impl Iterator<Item = (NodeId, &SyntaxNode)> {
        self.nodes.iter().enumerate().filter_map(|(index, node)| {
            let review = node.review.as_ref()?;
            (review.tracking == Tracking::Track).then_some((NodeId::new(index), node))
        })
    }
}

/// Symmetric before/after projections selected as one atomic frontend decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProjectionPair<'before, 'after> {
    pub(crate) before: Projection<'before>,
    pub(crate) after: Projection<'after>,
}

/// Project both revisions with one grammar, falling both back if either parse is unsafe.
pub(crate) fn project_pair<'before, 'after>(
    path: &Path,
    before: &'before str,
    after: &'after str,
    generated: bool,
) -> Result<ProjectionPair<'before, 'after>> {
    if generated {
        return Ok(line_pair(before, after, FallbackReason::Generated));
    }

    let language = language_for_path(path);
    let Some(language) = language else {
        return Ok(line_pair(before, after, FallbackReason::Unsupported));
    };

    let before = project_language(language, Source::new(before));
    let before = match before {
        Err(tree_sitter::ProjectFailure::Setup(error)) => {
            return Err(error).context("failed to initialize the before-source syntax frontend");
        }
        before => before,
    };
    let after = project_language(language, Source::new(after));
    let after = match after {
        Err(tree_sitter::ProjectFailure::Setup(error)) => {
            return Err(error).context("failed to initialize the after-source syntax frontend");
        }
        after => after,
    };
    match (before, after) {
        (
            Err(tree_sitter::ProjectFailure::Untrusted(before, before_reason)),
            Err(tree_sitter::ProjectFailure::Untrusted(after, after_reason)),
        ) => Ok(line_pair(
            before.source.as_str(),
            after.source.as_str(),
            fallback_reason(before_reason, Some(after_reason), ParseSide::Both),
        )),
        (Err(tree_sitter::ProjectFailure::Untrusted(before, reason)), Ok(after)) => Ok(line_pair(
            before.source.as_str(),
            after.source.as_str(),
            fallback_reason(reason, None, ParseSide::Before),
        )),
        (Ok(before), Err(tree_sitter::ProjectFailure::Untrusted(after, reason))) => Ok(line_pair(
            before.source.as_str(),
            after.source.as_str(),
            fallback_reason(reason, None, ParseSide::After),
        )),
        (Ok(before), Ok(after)) => Ok(ProjectionPair { before, after }),
        (Err(tree_sitter::ProjectFailure::Setup(_)), _)
        | (_, Err(tree_sitter::ProjectFailure::Setup(_))) => {
            unreachable!("setup failures returned before symmetric fallback")
        }
    }
}

fn fallback_reason(
    reason: tree_sitter::SyntaxFailure,
    other: Option<tree_sitter::SyntaxFailure>,
    side: ParseSide,
) -> FallbackReason {
    if matches!(reason, tree_sitter::SyntaxFailure::Complexity)
        || matches!(other, Some(tree_sitter::SyntaxFailure::Complexity))
    {
        return FallbackReason::SyntaxComplexity(side);
    }
    FallbackReason::ParseError(side)
}

fn project_language<'source>(
    language: Language,
    source: Source<'source>,
) -> std::result::Result<Projection<'source>, tree_sitter::ProjectFailure<'source>> {
    match language {
        Language::Rust => rust::project(source),
        Language::Html => html::project(source),
        Language::Css => css::project(source),
        Language::TypeScript => typescript::project_typescript(source),
        Language::Tsx => typescript::project_tsx(source),
        Language::JavaScript => typescript::project_javascript(source),
        Language::Jsx => typescript::project_jsx(source),
        Language::Lines => unreachable!("line projection does not invoke a grammar"),
    }
}

/// Reproject both revisions as exact line-leaf trees after a syntax certificate fails.
pub(crate) fn line_pair<'before, 'after>(
    before: &'before str,
    after: &'after str,
    reason: FallbackReason,
) -> ProjectionPair<'before, 'after> {
    ProjectionPair {
        before: line::project(Source::new(before), reason),
        after: line::project(Source::new(after), reason),
    }
}

fn language_for_path(path: &Path) -> Option<Language> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "rs" => Some(Language::Rust),
        "html" | "htm" => Some(Language::Html),
        "css" => Some(Language::Css),
        "ts" | "mts" | "cts" => Some(Language::TypeScript),
        "tsx" => Some(Language::Tsx),
        "js" | "mjs" | "cjs" => Some(Language::JavaScript),
        "jsx" => Some(Language::Jsx),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_projection_is_an_exact_leaf_cst_including_terminators() {
        let pair = project_pair(Path::new("notes.txt"), "a\r\n\nlast", "", false).unwrap();

        assert_eq!(
            pair.before.health,
            ProjectionHealth::Fallback(FallbackReason::Unsupported)
        );
        let root = pair.before.node(pair.before.root);
        let payloads = root
            .children
            .iter()
            .map(|id| pair.before.leaf_text(*id).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(payloads, ["a\r\n", "\n", "last"]);
        assert!(root.children.iter().all(|id| {
            pair.before.node(*id).review.as_ref().is_some_and(|unit| {
                unit.treatment == ReviewTreatment::Linewise && unit.tracking == Tracking::Track
            })
        }));
    }

    #[test]
    fn one_bad_parse_falls_both_revisions_back_to_lines() {
        let pair = project_pair(Path::new("lib.rs"), "fn good() {}\n", "fn {\n", false).unwrap();

        assert_eq!(pair.before.language, Language::Lines);
        assert_eq!(pair.after.language, Language::Lines);
        assert_eq!(
            pair.before.health,
            ProjectionHealth::Fallback(FallbackReason::ParseError(ParseSide::After))
        );
        assert_eq!(pair.before.health, pair.after.health);
    }

    #[test]
    fn generated_source_never_enters_a_language_grammar() {
        let pair = project_pair(Path::new("broken.rs"), "fn {", "fn good() {}", true).unwrap();

        assert_eq!(pair.before.language, Language::Lines);
        assert_eq!(pair.after.language, Language::Lines);
        assert_eq!(
            pair.before.health,
            ProjectionHealth::Fallback(FallbackReason::Generated)
        );
    }

    #[test]
    fn web_extensions_select_real_grammars() {
        let cases = [
            ("view.html", "<main>ok</main>", Language::Html),
            ("view.css", ".card { color: red; }", Language::Css),
            ("view.ts", "const n: number = 1;", Language::TypeScript),
            ("view.tsx", "const x = <Card />;", Language::Tsx),
            ("view.js", "const n = 1;", Language::JavaScript),
            ("view.jsx", "const x = <Card />;", Language::Jsx),
        ];

        for (path, source, language) in cases {
            let pair = project_pair(Path::new(path), source, source, false).unwrap();
            assert_eq!(pair.before.language, language, "{path}");
            assert_eq!(pair.after.language, language, "{path}");
            assert_eq!(pair.before.health, ProjectionHealth::Parsed, "{path}");
        }
    }

    #[test]
    fn html_wrapper_and_child_are_distinct_trackable_nodes() {
        let before = "<article>\n  <img src=\"ada.webp\">\n</article>\n";
        let after = "<article>\n  <div class=\"portrait\">\n    <img src=\"ada.webp\">\n  </div>\n</article>\n";
        let pair = project_pair(Path::new("view.html"), before, after, false).unwrap();

        let before_img = node_with_identity(&pair.before, "img");
        let after_img = node_with_identity(&pair.after, "img");
        let after_div = node_with_identity(&pair.after, "div");
        assert_eq!(pair.before.node(before_img).kind, "element");
        assert_eq!(pair.after.node(after_img).kind, "element");
        assert!(is_descendant(&pair.after, after_img, after_div));
        assert_eq!(pair.after.tracked_units().count(), 1);
        assert_eq!(
            pair.after.tracked_units().next().unwrap().0,
            pair.after.root
        );
    }

    #[test]
    fn whitespace_sensitive_html_is_one_exact_opaque_payload() {
        let source = "<pre>\n  <img src=\"literal\">\n</pre>\n";
        let pair = project_pair(Path::new("view.html"), source, source, false).unwrap();
        let pre = node_with_identity(&pair.before, "pre");
        let pre = pair.before.node(pre);

        assert!(pre.children.is_empty());
        assert_eq!(pre.leaf.unwrap().channel, ContentChannel::Opaque);
        assert_eq!(
            pair.before.source.slice(pre.bytes.clone()),
            Some(source.trim_end_matches('\n'))
        );
    }

    #[test]
    fn html_plaintext_absorbs_apparent_closing_tags_through_eof() {
        let source = "<p>markup</p>\n<plaintext>\n</plaintext>\n<div>still literal</div>\n";
        let pair = project_pair(Path::new("view.html"), source, source, false).unwrap();
        let plaintext = node_with_identity(&pair.before, "plaintext");
        let plaintext = pair.before.node(plaintext);

        assert!(plaintext.children.is_empty());
        assert_eq!(plaintext.leaf.unwrap().channel, ContentChannel::Opaque);
        assert_eq!(plaintext.bytes.end, source.len());
        assert_eq!(
            pair.before.source.slice(plaintext.bytes.clone()),
            Some("<plaintext>\n</plaintext>\n<div>still literal</div>\n")
        );
        assert!(
            !pair
                .before
                .nodes
                .iter()
                .enumerate()
                .map(|(index, _)| NodeId::new(index))
                .any(|id| pair.before.identity_text(id) == Some("div"))
        );
    }

    #[test]
    fn language_review_units_are_non_overlapping_top_level_boundaries() {
        let cases = [
            (
                "lib.rs",
                "use crate::A;\nfn outer() {\n    // nested\n}\n// top\n",
                3,
            ),
            (
                "view.css",
                ".a {\n  /* nested */\n  color: red;\n}\n/* top */\n",
                2,
            ),
            (
                "view.ts",
                "import { a } from './a';\nfunction outer() {\n  // nested\n}\n// top\n",
                3,
            ),
        ];

        for (path, source, expected) in cases {
            let pair = project_pair(Path::new(path), source, source, false).unwrap();
            let units = pair
                .before
                .tracked_units()
                .map(|(id, _)| id)
                .collect::<Vec<_>>();
            assert_eq!(units.len(), expected, "{path}");
            for (index, unit) in units.iter().copied().enumerate() {
                for other in units.iter().copied().skip(index + 1) {
                    assert!(!is_descendant(&pair.before, unit, other), "{path}");
                    assert!(!is_descendant(&pair.before, other, unit), "{path}");
                }
            }
        }
    }

    #[test]
    fn typescript_declarator_identities_survive_edits_and_reordering() {
        let before = "export const alpha = 1;\nexport let beta = 2;\nvar gamma = 3;\n";
        let after = "var gamma = 30;\nexport let beta = 20;\nexport const alpha = 10;\n";
        let pair = project_pair(Path::new("values.ts"), before, after, false).unwrap();

        assert_eq!(tracked_identities(&pair.before), ["alpha", "beta", "gamma"]);
        assert_eq!(tracked_identities(&pair.after), ["gamma", "beta", "alpha"]);
    }

    #[test]
    fn compact_import_identities_survive_reordering() {
        let rust_before = "use crate::alpha;\npub use crate::beta::Thing;\n";
        let rust_after = "pub use crate::beta::Thing;\nuse crate::alpha;\n";
        let rust = project_pair(Path::new("lib.rs"), rust_before, rust_after, false).unwrap();
        assert_eq!(
            tracked_identities(&rust.before),
            ["crate::alpha", "crate::beta::Thing"]
        );
        assert_eq!(
            tracked_identities(&rust.after),
            ["crate::beta::Thing", "crate::alpha"]
        );

        let css_before = "@import \"alpha.css\" screen;\n@import url(\"beta.css\");\n";
        let css_after = "@import url(\"beta.css\");\n@import \"alpha.css\" print;\n";
        let css = project_pair(Path::new("view.css"), css_before, css_after, false).unwrap();
        assert_eq!(
            tracked_identities(&css.before),
            ["\"alpha.css\"", "url(\"beta.css\")"]
        );
        assert_eq!(
            tracked_identities(&css.after),
            ["url(\"beta.css\")", "\"alpha.css\""]
        );
    }

    #[test]
    fn css_group_statements_keep_inline_header_identities_across_body_edits() {
        let before = "@media (min-width: 1px) { .a { color: red; } }\n@supports (display: grid) { .b { display: grid; } }\n@keyframes fade { from { opacity: 0; } }\n@scope (.card) { .title { color: blue; } }\n";
        let after = "@scope (.card) { .title { color: red; } }\n@keyframes fade { to { opacity: 1; } }\n@supports (display: grid) { .b { display: block; } }\n@media (min-width: 1px) { .a { color: green; } }\n";
        let pair = project_pair(Path::new("view.css"), before, after, false).unwrap();

        assert_eq!(
            tracked_identities(&pair.before),
            [
                "@media (min-width: 1px)",
                "@supports (display: grid)",
                "fade",
                "@scope (.card)",
            ]
        );
        assert_eq!(
            tracked_identities(&pair.after),
            [
                "@scope (.card)",
                "fade",
                "@supports (display: grid)",
                "@media (min-width: 1px)",
            ]
        );
        assert!(pair.before.tracked_units().all(|(_, node)| {
            node.review.as_ref().unwrap().treatment == ReviewTreatment::Inline
        }));
    }

    #[test]
    fn tree_sitter_arenas_are_byte_total_and_retain_intrinsic_numeric_prefixes() {
        let cases = [
            ("value.css", ".card { margin: 7rem; }\n"),
            ("value.html", "<div class=\"a b\">\n  <img>\n</div>\n"),
            ("value.rs", "fn value() -> usize { 7 }\n"),
            ("value.ts", "export const value = `a b`;\n"),
        ];
        for (path, source) in cases {
            let pair = project_pair(Path::new(path), source, source, false).unwrap();
            assert_byte_total(&pair.before);
        }

        let pair = project_pair(
            Path::new("value.css"),
            ".card { margin: 7rem; }\n",
            ".card { margin: 7rem; }\n",
            false,
        )
        .unwrap();
        let digit = leaf_with_text(&pair.before, "7");
        assert_eq!(pair.before.node(digit).kind, "source_fragment");
        assert_eq!(
            pair.before.node(digit).leaf,
            Some(Leaf {
                syntax: SyntaxClass::Literal,
                channel: ContentChannel::Syntax,
            })
        );
        let parent = pair.before.node(digit).parent.unwrap();
        assert_eq!(pair.before.node(parent).kind, "integer_value");
    }

    #[test]
    fn intrinsic_css_digits_link_as_modified_and_render_inline() {
        use crate::diff::correspondence::{LeafRelation, correspond};
        use crate::diff::{CodeRole, DiffMark, DiffRow};

        let before = ".card { margin: 6rem; }\n";
        let after = ".card { margin: 7rem; }\n";
        let pair = project_pair(Path::new("value.css"), before, after, false).unwrap();
        let six = leaf_with_text(&pair.before, "6");
        let seven = leaf_with_text(&pair.after, "7");
        let graph = correspond(&pair);
        let link = graph.before_leaf[six.index()]
            .and_then(|index| graph.leaf_links.get(index))
            .expect("numeric leaf must link");
        assert_eq!(link.after, seven);
        assert_eq!(link.relation, LeafRelation::Modified);

        let diff = crate::diff::diff_file("value.css", before, after).unwrap();
        assert!(diff.hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
            let DiffRow::Code {
                line,
                role: CodeRole::Inline,
            } = row
            else {
                return false;
            };
            line.spans
                .iter()
                .any(|span| span.mark == DiffMark::Added && span.text == "7")
        }));
    }

    #[test]
    fn layout_and_significant_whitespace_use_distinct_channels() {
        let css = project_pair(
            Path::new("selector.css"),
            ".a .b { content: \"a b\"; }\n",
            ".a .b { content: \"a b\"; }\n",
            false,
        )
        .unwrap();
        let selector_space =
            css.before
                .nodes
                .iter()
                .enumerate()
                .map(|(index, _)| NodeId::new(index))
                .find(|id| {
                    css.before.leaf_text(*id) == Some(" ")
                        && css.before.node(*id).parent.is_some_and(|parent| {
                            css.before.node(parent).kind == "descendant_selector"
                        })
                })
                .expect("descendant combinator whitespace must be projected");
        assert_eq!(
            css.before.node(selector_space).leaf.unwrap().channel,
            ContentChannel::Syntax
        );
        let string_content = leaf_with_text(&css.before, "a b");
        assert_eq!(
            css.before.node(string_content).leaf.unwrap().channel,
            ContentChannel::Syntax
        );

        let html = project_pair(
            Path::new("space.html"),
            "<div>\n  <span>a</span> <span>b</span>\n</div>\n",
            "<div>\n  <span>a</span> <span>b</span>\n</div>\n",
            false,
        )
        .unwrap();
        assert!(html.before.nodes.iter().enumerate().any(|(index, _)| {
            let id = NodeId::new(index);
            html.before
                .leaf_text(id)
                .is_some_and(|text| text.contains('\n') && text.trim().is_empty())
                && html.before.node(id).leaf.unwrap().channel == ContentChannel::Layout
        }));
        assert!(html.before.nodes.iter().enumerate().any(|(index, _)| {
            let id = NodeId::new(index);
            html.before.leaf_text(id) == Some(" ")
                && html.before.node(id).leaf.unwrap().channel == ContentChannel::Syntax
        }));
    }

    fn node_with_identity(projection: &Projection<'_>, identity: &str) -> NodeId {
        projection
            .nodes
            .iter()
            .enumerate()
            .map(|(index, _)| NodeId::new(index))
            .find(|id| projection.identity_text(*id) == Some(identity))
            .unwrap_or_else(|| panic!("missing {identity:?} graph node"))
    }

    fn tracked_identities<'source>(projection: &Projection<'source>) -> Vec<&'source str> {
        projection
            .tracked_units()
            .map(|(id, _)| {
                projection.identity_text(id).unwrap_or_else(|| {
                    panic!("tracked {:?} lacks an identity", projection.node(id).kind)
                })
            })
            .collect()
    }

    fn leaf_with_text(projection: &Projection<'_>, text: &str) -> NodeId {
        projection
            .nodes
            .iter()
            .enumerate()
            .map(|(index, _)| NodeId::new(index))
            .find(|id| projection.leaf_text(*id) == Some(text))
            .unwrap_or_else(|| panic!("missing leaf {text:?}"))
    }

    fn assert_byte_total(projection: &Projection<'_>) {
        for (index, node) in projection.nodes.iter().enumerate() {
            if node.leaf.is_some() {
                assert!(
                    node.children.is_empty(),
                    "concrete leaf {:?} node {} grew projected children",
                    node.kind,
                    index
                );
                continue;
            }

            let mut cursor = node.bytes.start;
            for child in &node.children {
                let child = projection.node(*child);
                assert_eq!(
                    child.bytes.start, cursor,
                    "gap or overlap under {:?} node {}",
                    node.kind, index
                );
                cursor = child.bytes.end;
            }
            assert_eq!(
                cursor, node.bytes.end,
                "uncovered suffix under {:?} node {}",
                node.kind, index
            );
        }
    }

    fn is_descendant(projection: &Projection<'_>, child: NodeId, ancestor: NodeId) -> bool {
        let mut parent = projection.node(child).parent;
        while let Some(candidate) = parent {
            if candidate == ancestor {
                return true;
            }
            parent = projection.node(candidate).parent;
        }
        false
    }
}
