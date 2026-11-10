//! Language-neutral syntax for correspondence, with both revisions using the same frontend.

mod c;
pub(crate) mod content;
mod cpp;
mod css;
mod go;
mod html;
mod json;
mod line;
mod lower;
mod nix;
mod parse;
mod python;
mod rust;
mod toml;
mod typescript;

use super::Highlight;
use super::source::{LineSpan, Source};
use anyhow::Result;
use std::num::NonZeroU16;
use std::ops::Range;
use std::path::Path;

/// Handle into one syntax arena, independent of the parser's node lifetimes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NodeId(usize);

impl NodeId {
    pub(crate) const fn new(index: usize) -> Self {
        Self(index)
    }

    pub const fn index(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Grammar {
    C,
    Cpp,
    Rust,
    Python,
    Go,
    Json,
    Toml,
    Nix,
    Html,
    Css,
    TypeScript,
    Tsx,
    JavaScript,
    Jsx,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ContentChannel {
    Syntax,
    Comment,
    Opaque,
    /// Formatting preserved for display but excluded from semantic matching.
    Layout,
}

/// Tree-sitter symbol whose numeric identity is meaningful only within `SyntaxTree::grammar`.
/// Correspondence compares these IDs without interpreting language-specific symbol names.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum GrammarSymbol {
    Named(u16),
    Anonymous(u16),
}

impl GrammarSymbol {
    fn new(id: u16, named: bool) -> Self {
        if named {
            Self::Named(id)
        } else {
            Self::Anonymous(id)
        }
    }
}

/// Grammar symbol or synthetic node for source that has no parser representation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Symbol {
    Grammar(GrammarSymbol),
    SourceFragment,
    File,
    Line,
}

/// Tree-sitter field identity scoped by `SyntaxTree::grammar`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GrammarField(NonZeroU16);

/// Parent-relative edge that distinguishes grammar fields from positional children.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ChildSlot {
    #[default]
    Positional,
    Field(GrammarField),
}

/// Presentation role of a review unit: a file-level construct matched independently of its
/// neighbours.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReviewRole {
    Content,
    /// Dependency wiring such as imports or preprocessor directives.
    Wiring,
}

/// Rules for pairing siblings after their parents have been matched.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum SiblingOrder {
    #[default]
    SourceOrder,
    /// A node paired by its own identity or shape before its contents can be compared.
    ByIdentity,
}

/// Permission to cross an unmatched node while proving a unique wrap or unwrap.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
enum WrapperBoundary {
    #[default]
    Traversable,
    Sealed,
}

/// What a named node declares for code elsewhere to refer to.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Declares {
    #[default]
    Nothing,
    /// A field, variant, or signature: named, but referred to through its owner.
    Member,
    /// A type, function, constant, module, or macro with a name of its own.
    Definition,
    /// A decorated definition whose inner definition shares its declaration identity.
    DecoratedDefinition,
}

/// Leaf metadata with payload borrowed from `SyntaxTree::source` through the node's byte range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Leaf {
    pub role: LeafRole,
    pub highlight: Highlight,
    pub channel: ContentChannel,
    pub delimiter: Option<Delimiter>,
}

/// Evidence a leaf contributes to correspondence, independent of its display color.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LeafRole {
    /// Name-like spelling that can identify a direct structural child.
    Identifier,
    /// Content whose overlap helps match edited nodes.
    Payload,
    /// Syntax or comments excluded from identifier and payload evidence.
    Scaffolding,
}

/// Punctuation ownership used to keep delimiters with their syntax during edits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delimiter {
    /// Comma or semicolon, optionally attached to its preceding syntax occurrence.
    Trailing(Option<NodeId>),
    Structural,
}

/// Mutually exclusive contents of a syntax occurrence.
#[derive(Clone, Debug, Eq, PartialEq)]
enum NodeContents {
    Branch(Vec<NodeId>),
    Token(Leaf),
}

/// Language-neutral syntax occurrence with exact source ranges and ordered children.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxNode {
    pub symbol: Symbol,
    pub slot: ChildSlot,
    pub bytes: Range<usize>,
    /// Source extent including any attached trailing delimiter, which lies outside `bytes`.
    pub source_envelope: Range<usize>,
    pub lines: LineSpan,
    pub parent: Option<NodeId>,
    contents: NodeContents,
    /// Source spelling that distinguishes this node from siblings of the same shape.
    identity: Option<Range<usize>>,
    /// Node decorated by this occurrence; the relationship does not merge their source ranges.
    pub decoration_owner: Option<NodeId>,
    pub sibling_order: SiblingOrder,
    wrapper_boundary: WrapperBoundary,
    /// Independently reviewed unit, allowed only on the root or its direct children.
    pub review: Option<ReviewRole>,
    /// Named construct other code can refer to, such as a function, type, field, or macro.
    pub declares: Declares,
}

impl SyntaxNode {
    pub fn is_named(&self) -> bool {
        matches!(
            self.symbol,
            Symbol::File | Symbol::Line | Symbol::Grammar(GrammarSymbol::Named(_))
        )
    }

    pub fn children(&self) -> &[NodeId] {
        match &self.contents {
            NodeContents::Branch(children) => children,
            NodeContents::Token(_) => &[],
        }
    }

    pub fn leaf(&self) -> Option<Leaf> {
        match self.contents {
            NodeContents::Token(leaf) => Some(leaf),
            NodeContents::Branch(_) => None,
        }
    }

    fn children_mut(&mut self) -> &mut Vec<NodeId> {
        match &mut self.contents {
            NodeContents::Token(_) => unreachable!("tokens cannot own children"),
            NodeContents::Branch(children) => children,
        }
    }

    fn leaf_mut(&mut self) -> Option<&mut Leaf> {
        match &mut self.contents {
            NodeContents::Token(leaf) => Some(leaf),
            NodeContents::Branch(_) => None,
        }
    }

    pub const fn seals_wrappers(&self) -> bool {
        matches!(self.wrapper_boundary, WrapperBoundary::Sealed)
    }
}

/// Source and a preorder arena whose node IDs remain stable through correspondence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxTree<'source> {
    pub(crate) source: Source<'source>,
    /// `None` for the line-based fallback, which has no grammar-specific symbols.
    pub(crate) grammar: Option<Grammar>,
    pub(crate) root: NodeId,
    pub(crate) nodes: Vec<SyntaxNode>,
    /// Leaves in source order for range lookup without walking the syntax tree.
    leaves: Vec<NodeId>,
    /// Exclusive preorder end of each node's contiguous subtree.
    subtree_ends: Vec<usize>,
}

impl<'source> SyntaxTree<'source> {
    /// Index a preorder arena for source lookup and containment checks.
    /// Review units must be either the file root alone or its direct children.
    fn from_nodes(
        source: Source<'source>,
        grammar: Option<Grammar>,
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
            .filter_map(|(index, node)| node.leaf().map(|_| NodeId::new(index)))
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
            grammar,
            root,
            nodes,
            leaves,
            subtree_ends,
        }
    }

    pub fn source(&self) -> &Source<'source> {
        &self.source
    }

    pub fn grammar(&self) -> Option<Grammar> {
        self.grammar
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn nodes(&self) -> &[SyntaxNode] {
        &self.nodes
    }

    pub fn node(&self, id: NodeId) -> &SyntaxNode {
        &self.nodes[id.index()]
    }

    /// Borrow a leaf's exact spelling, including an empty slice for a missing token.
    pub fn leaf_text(&self, id: NodeId) -> Option<&'source str> {
        let node = self.node(id);
        node.leaf()?;
        self.source.slice(node.bytes.clone())
    }

    pub fn identity_text(&self, id: NodeId) -> Option<&'source str> {
        let identity = self.node(id).identity.clone()?;
        self.source.slice(identity)
    }

    /// Yield leaves overlapping the byte range in source order.
    pub fn leaf_ids_in(&self, bytes: Range<usize>) -> impl Iterator<Item = NodeId> + '_ {
        let start = self
            .leaves
            .partition_point(|id| self.node(*id).bytes.end <= bytes.start);
        self.leaves[start..]
            .iter()
            .copied()
            .take_while(move |id| self.node(*id).bytes.start < bytes.end)
    }

    /// Spelling and byte range of every non-layout leaf inside the given zero-based lines,
    /// clipped to the line; a leaf crossing a line boundary contributes its part on each line.
    pub fn content_spans_in_lines<'tree>(
        &'tree self,
        lines: &'tree [usize],
    ) -> impl Iterator<Item = (&'source str, Range<usize>)> + 'tree {
        lines
            .iter()
            .filter_map(move |line| self.source.line(super::LineNumber::from_index(*line)))
            .flat_map(move |source_line| {
                let bytes = source_line.content_bytes.clone();
                self.leaf_ids_in(bytes.clone()).filter_map(move |leaf| {
                    let node = self.node(leaf);
                    if node.leaf()?.channel == ContentChannel::Layout {
                        return None;
                    }
                    let span = node.bytes.start.max(bytes.start)..node.bytes.end.min(bytes.end);
                    Some((self.source.slice(span.clone())?, span))
                })
            })
    }

    /// Yield descendants in source preorder, excluding the supplied root.
    pub fn descendants(&self, root: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        (root.index() + 1..self.subtree_ends[root.index()]).map(NodeId::new)
    }

    /// Exclusive preorder index where the node's subtree ends.
    pub fn subtree_end(&self, root: NodeId) -> usize {
        self.subtree_ends[root.index()]
    }

    /// Check containment, counting a node as contained in itself.
    pub fn contains(&self, outer: NodeId, inner: NodeId) -> bool {
        outer.index() <= inner.index() && inner.index() < self.subtree_ends[outer.index()]
    }

    /// Yield independently matched review units in source preorder.
    pub fn review_units(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| node.review.map(|_| NodeId::new(index)))
    }
}

/// Attach commas and semicolons to the preceding named node so edits include its delimiter.
fn attach_trailing_delimiters(nodes: &mut [SyntaxNode]) {
    for parent_index in 0..nodes.len() {
        for position in 0..nodes[parent_index].children().len() {
            let delimiter = nodes[parent_index].children()[position];
            let trailing = nodes[delimiter.index()]
                .leaf()
                .and_then(|leaf| leaf.delimiter)
                .is_some_and(|delimiter| matches!(delimiter, Delimiter::Trailing(_)));
            if !trailing {
                continue;
            }
            let owner = nodes[parent_index].children()[..position]
                .iter()
                .rev()
                .copied()
                .find(|candidate| {
                    let node = &nodes[candidate.index()];
                    node.is_named() && node.decoration_owner.is_none()
                });
            let Some(owner) = owner else {
                continue;
            };
            let delimiter_end = nodes[delimiter.index()].bytes.end;
            nodes[delimiter.index()]
                .leaf_mut()
                .expect("a delimiter is a concrete token")
                .delimiter = Some(Delimiter::Trailing(Some(owner)));
            nodes[owner.index()].source_envelope.end =
                nodes[owner.index()].source_envelope.end.max(delimiter_end);
        }
    }
}

/// Check whether the node and its delimiter occupy whole lines apart from spaces and tabs.
pub fn node_owns_complete_lines(syntax: &SyntaxTree<'_>, node: NodeId) -> bool {
    let node = syntax.node(node);
    let Some(lines) = syntax.source.line_coverage(node.source_envelope.clone()) else {
        return false;
    };
    let Some(first) = syntax.source.line(lines.start) else {
        return false;
    };
    let Some(last_number) = lines.last() else {
        return false;
    };
    let Some(last) = syntax.source.line(last_number) else {
        return false;
    };
    if node.source_envelope.start < first.content_bytes.start
        || node.source_envelope.end > last.full_bytes.end
    {
        return false;
    }

    let prefix = syntax
        .source
        .slice(first.content_bytes.start..node.source_envelope.start);
    let suffix = syntax
        .source
        .slice(node.source_envelope.end.min(last.content_bytes.end)..last.content_bytes.end);
    prefix.is_some_and(horizontal_layout) && suffix.is_some_and(horizontal_layout)
}

pub fn horizontal_layout(text: &str) -> bool {
    text.bytes().all(|byte| matches!(byte, b' ' | b'\t'))
}

/// One of the two revisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Side {
    Before,
    After,
}

impl Side {
    pub const fn other(self) -> Self {
        match self {
            Self::Before => Self::After,
            Self::After => Self::Before,
        }
    }
}

/// Source of the comparison shown to the reader.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Comparison {
    Unchanged,
    Structural(Grammar),
    Lines(LineFallback),
}

/// Reason both revisions use exact physical lines instead of structural matching.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LineFallback {
    UnsupportedLanguage,
    Generated,
    ParseErrors,
    NodeLimit,
    DepthLimit,
    VerificationFailure(String),
}

impl Grammar {
    /// Return the parser language for independent consumers of concrete syntax.
    pub fn language(self) -> tree_sitter::Language {
        parse::tree_sitter_language(self)
    }
}

/// Revision pair using one grammar, or both using the exact-line fallback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxPair<'before, 'after> {
    pub(crate) before: SyntaxTree<'before>,
    pub(crate) after: SyntaxTree<'after>,
    pub(crate) comparison: Comparison,
}

impl SyntaxPair<'_, '_> {
    pub fn comparison(&self) -> &Comparison {
        &self.comparison
    }

    /// The tree of one side, borrowed for as long as the pair is.
    pub fn tree(&self, side: Side) -> &SyntaxTree<'_> {
        match side {
            Side::Before => &self.before,
            Side::After => &self.after,
        }
    }
}

/// Parse both revisions with one grammar, falling back to lines if either cannot be lowered.
/// The shared frontend prevents differences in parser recovery from appearing as source edits.
pub fn syntax_pair<'before, 'after>(
    path: &Path,
    before: &'before str,
    after: &'after str,
    generated: bool,
) -> Result<SyntaxPair<'before, 'after>> {
    if generated {
        return Ok(line_pair(before, after, LineFallback::Generated));
    }
    let Some(grammar) = grammar_for_path(path) else {
        return Ok(line_pair(before, after, LineFallback::UnsupportedLanguage));
    };
    let before_parsed = parse::parse(Source::new(before), grammar, parse::Budget::default())?;
    let after_parsed = parse::parse(Source::new(after), grammar, parse::Budget::default())?;
    let (before_parsed, after_parsed) = match (before_parsed, after_parsed) {
        (parse::Outcome::Fallback(reason), _) | (_, parse::Outcome::Fallback(reason)) => {
            return Ok(line_pair(before, after, reason));
        }
        (parse::Outcome::Ready(before), parse::Outcome::Ready(after)) => (before, after),
    };
    let before_syntax = lower::lower(before_parsed)?;
    let after_syntax = lower::lower(after_parsed)?;
    let (Some(before_syntax), Some(after_syntax)) = (before_syntax, after_syntax) else {
        return Ok(line_pair(before, after, LineFallback::ParseErrors));
    };
    // Checking again after recovery; restored HTML nesting can exceed the parser's depth.
    let depth_limit = parse::Budget::default().depth;
    if exceeds_depth(&before_syntax, depth_limit) || exceeds_depth(&after_syntax, depth_limit) {
        return Ok(line_pair(before, after, LineFallback::DepthLimit));
    }
    Ok(SyntaxPair {
        before: before_syntax,
        after: after_syntax,
        comparison: Comparison::Structural(grammar),
    })
}

fn exceeds_depth(tree: &SyntaxTree<'_>, limit: usize) -> bool {
    let mut depths = vec![0; tree.nodes.len()];
    for (index, node) in tree.nodes.iter().enumerate() {
        let Some(parent) = node.parent else {
            continue;
        };
        depths[index] = depths[parent.index()] + 1;
        if depths[index] > limit {
            return true;
        }
    }
    false
}

pub(crate) fn line_pair<'before, 'after>(
    before: &'before str,
    after: &'after str,
    reason: LineFallback,
) -> SyntaxPair<'before, 'after> {
    SyntaxPair {
        before: line::lower(Source::new(before)),
        after: line::lower(Source::new(after)),
        comparison: Comparison::Lines(reason),
    }
}

pub fn grammar_for_path(path: &Path) -> Option<Grammar> {
    let extension = path.extension()?.to_str()?;
    if extension == "C" {
        return Some(Grammar::Cpp);
    }
    let extension = extension.to_ascii_lowercase();
    match extension.as_str() {
        "rs" => Some(Grammar::Rust),
        "py" | "pyi" | "pyw" => Some(Grammar::Python),
        "go" => Some(Grammar::Go),
        "json" => Some(Grammar::Json),
        "toml" => Some(Grammar::Toml),
        "nix" => Some(Grammar::Nix),
        "c" => Some(Grammar::C),
        "cc" | "cpp" | "cxx" | "c++" | "h" | "hh" | "hpp" | "hxx" | "h++" | "ipp" | "tpp"
        | "ixx" | "cppm" => Some(Grammar::Cpp),
        "html" | "htm" => Some(Grammar::Html),
        "css" => Some(Grammar::Css),
        "ts" | "mts" | "cts" => Some(Grammar::TypeScript),
        "tsx" => Some(Grammar::Tsx),
        "js" | "mjs" | "cjs" => Some(Grammar::JavaScript),
        "jsx" => Some(Grammar::Jsx),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
