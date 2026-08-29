//! Parse source symmetrically, then lower it into neutral syntax trees.

mod c;
mod css;
mod html;
mod line;
mod lower;
mod parse;
mod rust;
mod typescript;

use super::SyntaxClass;
use super::source::Source;
use anyhow::{Context, Result};
use parse::{ParserLanguage, SyntaxFailure};
use std::num::NonZeroU16;
use std::ops::Range;
use std::path::Path;

/// Stable arena handle; neutral syntax never exposes parser-owned node handles.
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

/// Parser chosen from the file path, or the universal line syntax.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Language {
    Lines,
    C,
    Rust,
    Html,
    Css,
    TypeScript,
    Tsx,
    JavaScript,
    Jsx,
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

/// Tree-sitter grammar symbol scoped by `SyntaxTree::language`.
///
/// The numeric identity is retained only for exact structural comparison; parser
/// spellings remain confined to language lowering.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct GrammarSymbol(u16);

impl GrammarSymbol {
    pub(super) const fn new(id: u16) -> Self {
        Self(id)
    }
}

/// Synthetic forms that have no parser grammar symbol.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum SyntaxKind {
    Grammar(GrammarSymbol),
    SourceFragment,
    File,
    Line,
}

/// Tree-sitter field identity scoped by `SyntaxTree::language`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct GrammarField(NonZeroU16);

impl GrammarField {
    pub(super) const fn new(id: NonZeroU16) -> Self {
        Self(id)
    }
}

/// Incoming position of one syntax occurrence beneath its parent.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum ChildSlot {
    #[default]
    Positional,
    Field(GrammarField),
}

/// How one independently matched review unit is compared.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ComparisonStrategy {
    /// Structure-aware content eligible for move and reflow detection.
    Structural,
    /// Content compared through physical lines.
    Linewise,
}

impl ComparisonStrategy {
    pub(crate) const fn tracks_movement(self) -> bool {
        matches!(self, Self::Structural)
    }

    /// Preserve structural comparison only when both frontends certify it.
    pub(crate) const fn reconcile(before: Self, after: Self) -> Self {
        match (before, after) {
            (Self::Structural, Self::Structural) => Self::Structural,
            _ => Self::Linewise,
        }
    }
}

/// Semantic source role carried independently of a unit's comparison strategy.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum SourceRole {
    Content,
    /// Dependency wiring such as imports or preprocessor directives.
    Wiring,
}

impl SourceRole {
    /// Keep wiring semantics only when both revisions agree on the role.
    pub(crate) const fn reconcile(before: Self, after: Self) -> Self {
        match (before, after) {
            (Self::Wiring, Self::Wiring) => Self::Wiring,
            _ => Self::Content,
        }
    }
}

/// Parser-omitted layout that a unit owns for source-completeness certification.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum LayoutOwnership {
    None,
    AdjacentBlankLines,
}

/// How one syntax occurrence participates among siblings under its paired parent.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) enum SiblingMatching {
    /// Grammar scaffolding follows ordered structural correspondence.
    #[default]
    OrderedSyntax,
    /// The occurrence establishes a local identity/shape domain before its contents pair.
    LocalIdentity,
}

/// Whether a unique wrap or unwrap proof may traverse one unmatched occurrence.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) enum WrapperBoundary {
    #[default]
    Traversable,
    /// Descendants cannot escape through this unmatched semantic boundary.
    Sealed,
}

/// Frontend-selected policies for one independently matched file-level unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReviewUnit {
    pub(crate) comparison: ComparisonStrategy,
    pub(crate) role: SourceRole,
    pub(crate) layout: LayoutOwnership,
}

impl ReviewUnit {
    pub(super) const fn structural(layout: LayoutOwnership) -> Self {
        Self {
            comparison: ComparisonStrategy::Structural,
            role: SourceRole::Content,
            layout,
        }
    }

    pub(super) const fn linewise(layout: LayoutOwnership) -> Self {
        Self {
            comparison: ComparisonStrategy::Linewise,
            role: SourceRole::Content,
            layout,
        }
    }

    pub(super) const fn wiring(layout: LayoutOwnership) -> Self {
        Self {
            comparison: ComparisonStrategy::Linewise,
            role: SourceRole::Wiring,
            layout,
        }
    }
}

/// Concrete CST leaf metadata; payload remains borrowed through `SyntaxTree::source`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Leaf {
    /// Parser-level meaning used by correspondence, independent of terminal coloring.
    pub(crate) role: LeafRole,
    pub(crate) syntax: SyntaxClass,
    pub(crate) channel: ContentChannel,
    /// Grammar delimiter status and its optional preceding syntax owner.
    pub(crate) delimiter: Option<Delimiter>,
}

/// Semantic contribution of one concrete leaf to neutral correspondence.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum LeafRole {
    /// Name-like spelling that can identify a direct structural child.
    Identifier,
    /// Source payload that contributes evidence of near-sameness.
    Payload,
    /// Grammar or commentary scaffolding that carries neither role.
    Scaffolding,
}

/// One punctuation delimiter classified independently of terminal highlighting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Delimiter {
    pub(crate) kind: DelimiterKind,
    pub(crate) owner: Option<NodeId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DelimiterKind {
    Comma,
    Semicolon,
    Structural,
}

/// One neutral CST occurrence with exact source geometry and ordered containment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SyntaxNode {
    /// Parser symbol identity or one explicitly synthetic source form.
    pub(crate) kind: SyntaxKind,
    /// Grammar-owned identity of this node's incoming field edge.
    pub(crate) slot: ChildSlot,
    pub(crate) bytes: Range<usize>,
    /// Complete source extent owned by this occurrence, including an attached trailing delimiter.
    pub(crate) source_envelope: Range<usize>,
    pub(crate) lines: Range<usize>,
    pub(crate) parent: Option<NodeId>,
    pub(crate) children: Vec<NodeId>,
    pub(crate) leaf: Option<Leaf>,
    /// Exact source spelling used to disambiguate same-shaped graph nodes.
    pub(crate) identity: Option<Range<usize>>,
    /// Semantic syntax node decorated by this occurrence, independent of source extent.
    pub(crate) decoration_owner: Option<NodeId>,
    pub(crate) sibling_matching: SiblingMatching,
    pub(crate) wrapper_boundary: WrapperBoundary,
    /// Presence promotes this node to an independently matched file-level unit.
    pub(crate) review: Option<ReviewUnit>,
    pub(crate) named: bool,
    pub(crate) extra: bool,
    pub(crate) missing: bool,
}

impl SyntaxNode {
    /// Whether this node stops a wrap or unwrap proof from crossing its semantic domain.
    pub(crate) const fn seals_wrappers(&self) -> bool {
        matches!(self.wrapper_boundary, WrapperBoundary::Sealed)
    }

    /// Whether this node fences correspondence to its own matched parent pair.
    pub(crate) const fn is_scope_boundary(&self) -> bool {
        self.leaf.is_none() || self.review.is_some()
    }
}

/// Source plus a parser-independent arena suitable for graph correspondence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SyntaxTree<'source> {
    pub(crate) source: Source<'source>,
    pub(crate) language: Language,
    pub(crate) root: NodeId,
    pub(crate) nodes: Vec<SyntaxNode>,
    /// Source-ordered leaves keep presentation and source certification linear.
    leaves: Vec<NodeId>,
    /// Exclusive preorder end of each node's contiguous subtree.
    subtree_ends: Vec<usize>,
}

impl<'source> SyntaxTree<'source> {
    /// Complete neutral arena plus its source-order acceleration index.
    pub(super) fn from_nodes(
        source: Source<'source>,
        language: Language,
        root: NodeId,
        mut nodes: Vec<SyntaxNode>,
    ) -> Self {
        let root_is_reviewed = nodes[root.index()].review.is_some();
        assert!(
            nodes.iter().enumerate().all(|(index, node)| {
                node.review.is_none()
                    || if root_is_reviewed {
                        index == root.index()
                    } else {
                        node.parent == Some(root)
                    }
            }),
            "review units must be flat beneath the file root",
        );
        attach_trailing_delimiters(&mut nodes);
        let leaves = nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| node.leaf.map(|_| NodeId::new(index)))
            .collect::<Vec<_>>();
        let mut subtree_ends = (1..=nodes.len()).collect::<Vec<_>>();
        for index in (0..nodes.len()).rev() {
            let Some(parent) = nodes[index].parent else {
                continue;
            };
            debug_assert!(parent.index() < index, "syntax arenas are preorder");
            subtree_ends[parent.index()] = subtree_ends[parent.index()].max(subtree_ends[index]);
        }
        debug_assert!(leaves.windows(2).all(|pair| {
            nodes[pair[0].index()].bytes.start <= nodes[pair[1].index()].bytes.start
        }));
        Self {
            source,
            language,
            root,
            nodes,
            leaves,
            subtree_ends,
        }
    }

    /// Arena node for a stable syntax-local handle.
    pub(crate) fn node(&self, id: NodeId) -> &SyntaxNode {
        &self.nodes[id.index()]
    }

    /// Original payload of a concrete leaf, including a zero-width missing token.
    pub(crate) fn leaf_text(&self, id: NodeId) -> Option<&'source str> {
        let node = self.node(id);
        node.leaf?;
        self.source.slice(node.bytes.clone())
    }

    /// Syntax occurrence owning this trailing delimiter, if lowering attached one.
    pub(crate) fn delimiter_owner(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).leaf?.delimiter?.owner
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
        (root.index() + 1..self.subtree_ends[root.index()]).map(NodeId::new)
    }

    /// Whether one arena node belongs to another node's preorder subtree.
    pub(crate) fn contains(&self, outer: NodeId, inner: NodeId) -> bool {
        outer.index() <= inner.index() && inner.index() < self.subtree_ends[outer.index()]
    }

    /// Independently matched file-level units in source preorder.
    pub(crate) fn review_units(&self) -> impl Iterator<Item = (NodeId, &SyntaxNode)> {
        self.nodes.iter().enumerate().filter_map(|(index, node)| {
            node.review.as_ref()?;
            Some((NodeId::new(index), node))
        })
    }
}

/// Attach comma and semicolon tokens to their preceding named syntax occurrence.
fn attach_trailing_delimiters(nodes: &mut [SyntaxNode]) {
    for parent_index in 0..nodes.len() {
        let children = nodes[parent_index].children.clone();
        for (position, delimiter) in children.iter().copied().enumerate() {
            let Some(delimiter_kind) = nodes[delimiter.index()]
                .leaf
                .and_then(|leaf| leaf.delimiter)
                .map(|delimiter| delimiter.kind)
                .filter(|kind| matches!(kind, DelimiterKind::Comma | DelimiterKind::Semicolon))
            else {
                continue;
            };
            let owner = children[..position]
                .iter()
                .rev()
                .copied()
                .find(|candidate| {
                    let node = &nodes[candidate.index()];
                    node.named && node.decoration_owner.is_none()
                });
            let Some(owner) = owner else {
                continue;
            };
            let delimiter_end = nodes[delimiter.index()].bytes.end;
            nodes[delimiter.index()]
                .leaf
                .as_mut()
                .expect("a delimiter is a concrete token")
                .delimiter = Some(Delimiter {
                kind: delimiter_kind,
                owner: Some(owner),
            });
            nodes[owner.index()].source_envelope.end =
                nodes[owner.index()].source_envelope.end.max(delimiter_end);
        }
    }
}

/// Whether one syntax node owns complete physical rows apart from indentation.
pub(crate) fn node_owns_complete_lines(syntax: &SyntaxTree<'_>, node: NodeId) -> bool {
    let node = syntax.node(node);
    let Some(lines) = syntax.source.line_coverage(node.source_envelope.clone()) else {
        return false;
    };
    let Some(first) = syntax.source.line(lines.start) else {
        return false;
    };
    let Some(last_number) = lines.end.checked_sub(1) else {
        return false;
    };
    let Some(last) = syntax.source.line(last_number) else {
        return false;
    };
    if node.source_envelope.start < first.content_bytes.start
        || node.source_envelope.end > last.content_bytes.end
    {
        return false;
    }

    let prefix = syntax
        .source
        .slice(first.content_bytes.start..node.source_envelope.start);
    let suffix = syntax
        .source
        .slice(node.source_envelope.end..last.content_bytes.end);
    prefix.is_some_and(horizontal_layout) && suffix.is_some_and(horizontal_layout)
}

pub(crate) fn horizontal_layout(text: &str) -> bool {
    text.bytes().all(|byte| matches!(byte, b' ' | b'\t'))
}

/// Symmetric before/after syntax selected as one atomic frontend decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SyntaxPair<'before, 'after> {
    pub(crate) before: SyntaxTree<'before>,
    pub(crate) after: SyntaxTree<'after>,
}

/// Parse and lower both revisions with one grammar, falling both back if either is unsafe.
pub(crate) fn syntax_pair<'before, 'after>(
    path: &Path,
    before: &'before str,
    after: &'after str,
    generated: bool,
) -> Result<SyntaxPair<'before, 'after>> {
    if generated {
        return Ok(line_pair(before, after));
    }

    let language = parser_language_for_path(path);
    let Some(language) = language else {
        return Ok(line_pair(before, after));
    };

    let before_parsed = parse::parse(Source::new(before), language);
    let after_parsed = parse::parse(Source::new(after), language);
    let before_parsed = fallback_or_setup(
        before_parsed,
        "failed to initialize the before-source syntax parser",
    )?;
    let after_parsed = fallback_or_setup(
        after_parsed,
        "failed to initialize the after-source syntax parser",
    )?;
    let (Some(before_parsed), Some(after_parsed)) = (before_parsed, after_parsed) else {
        return Ok(line_pair(before, after));
    };

    let before_syntax = lower::lower(before_parsed);
    let after_syntax = lower::lower(after_parsed);
    let before_syntax = fallback_or_setup(
        before_syntax,
        "failed to initialize the before-source syntax lowerer",
    )?;
    let after_syntax = fallback_or_setup(
        after_syntax,
        "failed to initialize the after-source syntax lowerer",
    )?;
    let (Some(before), Some(after)) = (before_syntax, after_syntax) else {
        return Ok(line_pair(before, after));
    };
    Ok(SyntaxPair { before, after })
}

fn fallback_or_setup<T>(
    result: std::result::Result<T, SyntaxFailure>,
    setup_context: &'static str,
) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(SyntaxFailure::Fallback) => Ok(None),
        Err(SyntaxFailure::Setup(error)) => Err(error).context(setup_context),
    }
}

/// Lower both revisions as exact line-leaf trees after a syntax certificate fails.
pub(crate) fn line_pair<'before, 'after>(
    before: &'before str,
    after: &'after str,
) -> SyntaxPair<'before, 'after> {
    SyntaxPair {
        before: line::lower(Source::new(before)),
        after: line::lower(Source::new(after)),
    }
}

fn parser_language_for_path(path: &Path) -> Option<ParserLanguage> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "rs" => Some(ParserLanguage::Rust),
        "c" | "h" => Some(ParserLanguage::C),
        "html" | "htm" => Some(ParserLanguage::Html),
        "css" => Some(ParserLanguage::Css),
        "ts" | "mts" | "cts" => Some(ParserLanguage::TypeScript),
        "tsx" => Some(ParserLanguage::Tsx),
        "js" | "mjs" | "cjs" => Some(ParserLanguage::JavaScript),
        "jsx" => Some(ParserLanguage::Jsx),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
