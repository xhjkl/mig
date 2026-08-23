//! Symmetric neutral CST projections; an exact line-leaf tree is the universal fallback.

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
}

/// Revision whose parse prevented symmetric syntax correspondence.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ParseSide {
    Before,
    After,
    Both,
}

/// Whether an arena came from a complete parse, certified recovery, or exact line fallback.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ProjectionHealth {
    Parsed,
    /// Byte-total grammar recovery accepted by the language adapter.
    Recovered,
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

/// How one independently matched review unit is compared and presented.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ReviewMode {
    /// Structure-aware content eligible for move and reflow detection.
    Structural,
    /// Low-signal content kept compact when its replacement fits one line.
    Compact,
    /// Content compared and presented through physical lines.
    Linewise,
}

impl ReviewMode {
    pub(crate) const fn tracks_movement(self) -> bool {
        matches!(self, Self::Structural)
    }

    /// Preserve a shared mode; mixed frontend classifications require exact line review.
    pub(crate) const fn reconcile(before: Self, after: Self) -> Self {
        match (before, after) {
            (Self::Structural, Self::Structural) => Self::Structural,
            (Self::Compact, Self::Compact) => Self::Compact,
            (Self::Linewise, Self::Linewise) => Self::Linewise,
            _ => Self::Linewise,
        }
    }
}

/// Parser-omitted layout that a unit owns for source-completeness certification.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum LayoutOwnership {
    None,
    AdjacentBlankLines,
}

/// Frontend-selected review behavior and layout ownership for one syntax node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReviewUnit {
    pub(crate) mode: ReviewMode,
    pub(crate) layout: LayoutOwnership,
}

impl ReviewUnit {
    pub(super) const fn new(mode: ReviewMode, layout: LayoutOwnership) -> Self {
        Self { mode, layout }
    }
}

/// Concrete CST leaf metadata; payload remains borrowed through `Projection::source`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Leaf {
    pub(crate) syntax: SyntaxClass,
    pub(crate) channel: ContentChannel,
    /// Grammar delimiter status, kept independent of terminal highlighting.
    pub(crate) delimiter: bool,
}

/// One neutral CST occurrence with exact source geometry and ordered containment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SyntaxNode {
    /// Tree-sitter-provided static symbol name, or a static literal in a synthetic projection.
    pub(crate) kind: &'static str,
    /// Grammar-owned static name of this node's incoming field edge.
    pub(crate) field: Option<&'static str>,
    pub(crate) bytes: Range<usize>,
    pub(crate) lines: Range<usize>,
    pub(crate) parent: Option<NodeId>,
    pub(crate) children: Vec<NodeId>,
    pub(crate) leaf: Option<Leaf>,
    /// Exact source spelling used to disambiguate same-shaped graph nodes.
    pub(crate) identity: Option<Range<usize>>,
    /// Semantic syntax node decorated by this occurrence, independent of source extent.
    pub(crate) decoration_owner: Option<NodeId>,
    /// Presence promotes this syntax node to an independently matched review unit.
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
        self.leaf_ids_in(bytes).map(|id| self.node(id))
    }

    /// Concrete leaf handles overlapping one byte range, in source order.
    pub(crate) fn leaf_ids_in(&self, bytes: Range<usize>) -> impl Iterator<Item = NodeId> + '_ {
        let start = self
            .leaves
            .partition_point(|id| self.node(*id).bytes.end <= bytes.start);
        self.leaves[start..]
            .iter()
            .copied()
            .take_while(move |id| self.node(*id).bytes.start < bytes.end)
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

    /// Independently matched review units in source preorder.
    pub(crate) fn review_units(&self) -> impl Iterator<Item = (NodeId, &SyntaxNode)> {
        self.nodes.iter().enumerate().filter_map(|(index, node)| {
            node.review.as_ref()?;
            Some((NodeId::new(index), node))
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
            Err(tree_sitter::ProjectFailure::Fallback(before, before_reason)),
            Err(tree_sitter::ProjectFailure::Fallback(after, after_reason)),
        ) => Ok(line_pair(
            before.source.as_str(),
            after.source.as_str(),
            fallback_reason(before_reason, Some(after_reason), ParseSide::Both),
        )),
        (Err(tree_sitter::ProjectFailure::Fallback(before, reason)), Ok(after)) => Ok(line_pair(
            before.source.as_str(),
            after.source.as_str(),
            fallback_reason(reason, None, ParseSide::Before),
        )),
        (Ok(before), Err(tree_sitter::ProjectFailure::Fallback(after, reason))) => Ok(line_pair(
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
mod tests;
