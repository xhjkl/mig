//! Language-neutral syntax for correspondence, with both revisions using the same frontend.

mod c;
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

use super::SyntaxClass;
use super::source::Source;
use anyhow::{Context, Result};
use std::num::NonZeroU16;
use std::ops::Range;
use std::path::Path;

/// Handle into one syntax arena, independent of the parser's node lifetimes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NodeId(usize);

impl NodeId {
    pub const fn new(index: usize) -> Self {
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
pub struct GrammarSymbol(u16);

/// Grammar symbol or synthetic node for source that has no parser representation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SyntaxKind {
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

/// Comparison policy for one independently matched review unit.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ComparisonStrategy {
    /// Syntax comparison that permits move and reflow detection.
    Structural,
    Linewise,
}

impl ComparisonStrategy {
    pub const fn tracks_movement(self) -> bool {
        matches!(self, Self::Structural)
    }

    /// Require structural support from both revisions so comparison stays symmetric.
    pub const fn reconcile(before: Self, after: Self) -> Self {
        match (before, after) {
            (Self::Structural, Self::Structural) => Self::Structural,
            _ => Self::Linewise,
        }
    }
}

/// Presentation role, independent of how the unit's contents are compared.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SourceRole {
    Content,
    /// Dependency wiring such as imports or preprocessor directives.
    Wiring,
}

impl SourceRole {
    pub const fn reconcile(before: Self, after: Self) -> Self {
        match (before, after) {
            (Self::Wiring, Self::Wiring) => Self::Wiring,
            _ => Self::Content,
        }
    }
}

/// Permission to claim unchanged blank separators along with a matched review unit.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LayoutOwnership {
    None,
    AdjacentBlankLines,
}

/// Rules for pairing siblings after their parents have been matched.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum SiblingMatching {
    #[default]
    OrderedSyntax,
    /// A node paired by its own identity or shape before its contents can be compared.
    LocalIdentity,
}

/// Permission to cross an unmatched node while proving a unique wrap or unwrap.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
enum WrapperBoundary {
    #[default]
    Traversable,
    Sealed,
}

/// Policies for a file-level unit matched independently of its neighbors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReviewUnit {
    pub comparison: ComparisonStrategy,
    pub role: SourceRole,
    pub layout: LayoutOwnership,
}

impl ReviewUnit {
    const fn structural(layout: LayoutOwnership) -> Self {
        Self {
            comparison: ComparisonStrategy::Structural,
            role: SourceRole::Content,
            layout,
        }
    }

    const fn linewise(layout: LayoutOwnership) -> Self {
        Self {
            comparison: ComparisonStrategy::Linewise,
            role: SourceRole::Content,
            layout,
        }
    }

    const fn wiring(layout: LayoutOwnership) -> Self {
        Self {
            comparison: ComparisonStrategy::Linewise,
            role: SourceRole::Wiring,
            layout,
        }
    }
}

/// Leaf metadata with payload borrowed from `SyntaxTree::source` through the node's byte range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Leaf {
    pub role: LeafRole,
    pub syntax: SyntaxClass,
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

/// Language-neutral syntax occurrence with exact source ranges and ordered children.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxNode {
    pub kind: SyntaxKind,
    pub slot: ChildSlot,
    pub bytes: Range<usize>,
    /// Source extent including any attached trailing delimiter, which lies outside `bytes`.
    pub source_envelope: Range<usize>,
    pub lines: Range<usize>,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub leaf: Option<Leaf>,
    /// Source spelling that distinguishes this node from siblings of the same shape.
    identity: Option<Range<usize>>,
    /// Node decorated by this occurrence; the relationship does not merge their source ranges.
    pub decoration_owner: Option<NodeId>,
    pub sibling_matching: SiblingMatching,
    wrapper_boundary: WrapperBoundary,
    /// Independent review policy, allowed only on the root or its direct children.
    pub review: Option<ReviewUnit>,
    pub named: bool,
    pub extra: bool,
    pub missing: bool,
}

impl SyntaxNode {
    pub const fn seals_wrappers(&self) -> bool {
        matches!(self.wrapper_boundary, WrapperBoundary::Sealed)
    }

    /// Identify boundaries that keep source alignment inside a matched node pair.
    pub const fn is_scope_boundary(&self) -> bool {
        self.leaf.is_none() || self.review.is_some()
    }
}

/// Source and a preorder arena whose node IDs remain stable through correspondence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxTree<'source> {
    pub source: Source<'source>,
    /// `None` for the line-based fallback, which has no grammar-specific symbols.
    pub grammar: Option<Grammar>,
    pub root: NodeId,
    pub nodes: Vec<SyntaxNode>,
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
            grammar,
            root,
            nodes,
            leaves,
            subtree_ends,
        }
    }

    pub fn node(&self, id: NodeId) -> &SyntaxNode {
        &self.nodes[id.index()]
    }

    /// Borrow a leaf's exact spelling, including an empty slice for a missing token.
    pub fn leaf_text(&self, id: NodeId) -> Option<&'source str> {
        let node = self.node(id);
        node.leaf?;
        self.source.slice(node.bytes.clone())
    }

    pub fn delimiter_owner(&self, id: NodeId) -> Option<NodeId> {
        match self.node(id).leaf?.delimiter? {
            Delimiter::Trailing(owner) => owner,
            Delimiter::Structural => None,
        }
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

    /// Yield descendants in source preorder, excluding the supplied root.
    pub fn descendants(&self, root: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        (root.index() + 1..self.subtree_ends[root.index()]).map(NodeId::new)
    }

    /// Check containment, counting a node as contained in itself.
    pub fn contains(&self, outer: NodeId, inner: NodeId) -> bool {
        outer.index() <= inner.index() && inner.index() < self.subtree_ends[outer.index()]
    }

    /// Yield independently matched review units in source preorder.
    pub fn review_units(&self) -> impl Iterator<Item = (NodeId, &SyntaxNode)> {
        self.nodes.iter().enumerate().filter_map(|(index, node)| {
            node.review.as_ref()?;
            Some((NodeId::new(index), node))
        })
    }
}

/// Attach commas and semicolons to the preceding named node so edits include its delimiter.
fn attach_trailing_delimiters(nodes: &mut [SyntaxNode]) {
    for parent_index in 0..nodes.len() {
        for position in 0..nodes[parent_index].children.len() {
            let delimiter = nodes[parent_index].children[position];
            let trailing = nodes[delimiter.index()]
                .leaf
                .and_then(|leaf| leaf.delimiter)
                .is_some_and(|delimiter| matches!(delimiter, Delimiter::Trailing(_)));
            if !trailing {
                continue;
            }
            let owner = nodes[parent_index].children[..position]
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

pub fn horizontal_layout(text: &str) -> bool {
    text.bytes().all(|byte| matches!(byte, b' ' | b'\t'))
}

/// Revision pair using one grammar, or both using the exact-line fallback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxPair<'before, 'after> {
    pub before: SyntaxTree<'before>,
    pub after: SyntaxTree<'after>,
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
        return Ok(line_pair(before, after));
    }

    let grammar = grammar_for_path(path);
    let Some(grammar) = grammar else {
        return Ok(line_pair(before, after));
    };

    let before_parsed = parse::parse(Source::new(before), grammar, None);
    let after_parsed = parse::parse(Source::new(after), grammar, None);
    let before_parsed =
        before_parsed.context("failed to initialize the before-source syntax parser")?;
    let after_parsed =
        after_parsed.context("failed to initialize the after-source syntax parser")?;
    let (Some(before_parsed), Some(after_parsed)) = (before_parsed, after_parsed) else {
        return Ok(line_pair(before, after));
    };

    let before_syntax = lower::lower(before_parsed);
    let after_syntax = lower::lower(after_parsed);
    let before_syntax =
        before_syntax.context("failed to initialize the before-source syntax lowerer")?;
    let after_syntax =
        after_syntax.context("failed to initialize the after-source syntax lowerer")?;
    let (Some(before), Some(after)) = (before_syntax, after_syntax) else {
        return Ok(line_pair(before, after));
    };
    Ok(SyntaxPair { before, after })
}

fn line_pair<'before, 'after>(
    before: &'before str,
    after: &'after str,
) -> SyntaxPair<'before, 'after> {
    SyntaxPair {
        before: line::lower(Source::new(before)),
        after: line::lower(Source::new(after)),
    }
}

fn grammar_for_path(path: &Path) -> Option<Grammar> {
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
