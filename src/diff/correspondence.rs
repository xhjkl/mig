//! Match syntax within corresponding owners, then decide which physical rows can use those matches.

use super::source::LineEnding;
use super::syntax::{
    ChildSlot, ComparisonStrategy, ContentChannel, LayoutOwnership, LeafRole, NodeId,
    SiblingMatching, SourceRole, SyntaxKind, SyntaxPair, SyntaxTree, node_owns_complete_lines,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::ops::Range;

const MAX_LOCAL_ALIGNMENT_CELLS: usize = 16_384;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Correspondence {
    pub tree: TreeDiff,
    pub source: SourceProjection,
}

/// Syntax matches retained even where source projection falls back to linewise review.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeDiff {
    pub units: Vec<UnitEdit>,
    leaves: LeafCorrespondence,
    pub composites: Vec<NodeLink>,
    relocations: Vec<RelocatedNode>,
    pub scopes: Vec<ScopeLink>,
}

/// Leaf pairs with at most one partner per node in either revision.
#[derive(Clone, Debug, Eq, PartialEq)]
struct LeafCorrespondence {
    links: Vec<LeafLink>,
    before: Vec<Option<usize>>,
    after: Vec<Option<usize>>,
}

/// Physical line alignment and the choice of syntax or linewise review for each region.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceProjection {
    pub lines: Vec<LineLink>,
    pub line_endings: Vec<LineLink>,
    pub fallbacks: Vec<LineFallback>,
    review_units: Vec<usize>,
}

impl SourceProjection {
    /// Select exact line matches inside the supplied zero-based ranges.
    pub fn line_links_in(
        &self,
        before: Range<usize>,
        after: Range<usize>,
    ) -> impl Iterator<Item = LineLink> + '_ {
        let start = self
            .lines
            .partition_point(|link| link.before < before.start);
        let end = self.lines.partition_point(|link| link.before < before.end);
        self.lines[start..end]
            .iter()
            .copied()
            .filter(move |link| after.contains(&link.after))
    }

    /// Select paired terminator edits inside the supplied zero-based ranges.
    pub fn line_ending_edits_in(
        &self,
        before: Range<usize>,
        after: Range<usize>,
    ) -> impl Iterator<Item = LineLink> + '_ {
        let start = self
            .line_endings
            .partition_point(|link| link.before < before.start);
        let end = self
            .line_endings
            .partition_point(|link| link.before < before.end);
        self.line_endings[start..end]
            .iter()
            .copied()
            .filter(move |link| after.contains(&link.after))
    }

    /// Yield tree edits whose rows are not claimed by a linewise fallback.
    pub fn review_units<'tree>(
        &'tree self,
        tree: &'tree TreeDiff,
    ) -> impl Iterator<Item = &'tree UnitEdit> + 'tree {
        self.review_units.iter().map(|index| &tree.units[*index])
    }
}

impl TreeDiff {
    pub fn unit_leaf_links(&self, unit: &MatchedUnit) -> &[LeafLink] {
        &self.leaves.links[unit.leaf_links.clone()]
    }

    pub fn leaf_links(&self) -> &[LeafLink] {
        &self.leaves.links
    }

    pub fn unit_composites(&self, unit: &MatchedUnit) -> &[NodeLink] {
        &self.composites[unit.composites.clone()]
    }

    pub fn relocated_nodes(&self) -> &[RelocatedNode] {
        &self.relocations
    }

    pub fn after_leaf_link(&self, node: NodeId) -> Option<&LeafLink> {
        let link = self.leaves.after.get(node.index()).copied().flatten()?;
        self.leaves.links.get(link)
    }

    pub fn before_leaf_link(&self, node: NodeId) -> Option<&LeafLink> {
        let link = self.leaves.before.get(node.index()).copied().flatten()?;
        self.leaves.links.get(link)
    }
}

/// Paired physical rows, addressed by zero-based source-line indices.
/// Exact matches and terminator edits are stored separately in `SourceProjection`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LineLink {
    pub before: usize,
    pub after: usize,
}

/// Zero-based physical source ranges that require local linewise review.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineFallback {
    pub before: Range<usize>,
    pub after: Range<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnitEdit {
    Matched(MatchedUnit),
    Removed { before: NodeId },
    Added { after: NodeId },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchedUnit {
    pub before: NodeId,
    pub after: NodeId,
    /// Comparison policy reconciled symmetrically from both revisions.
    pub comparison: ComparisonStrategy,
    /// Source role reconciled symmetrically from both revisions.
    pub role: SourceRole,
    pub relation: ContentRelation,
    pub placement: Placement,
    leaf_links: Range<usize>,
    composites: Range<usize>,
}

/// Strongest applicable equality level.
/// Source includes layout; full fingerprints omit layout leaves, and payload also omits comments.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ContentRelation {
    SourceEqual,
    FullEqual,
    PayloadEqual,
    Modified,
}

impl ContentRelation {
    pub const fn source_equal(self) -> bool {
        matches!(self, Self::SourceEqual)
    }

    pub const fn full_equal(self) -> bool {
        matches!(self, Self::SourceEqual | Self::FullEqual)
    }

    pub const fn payload_equal(self) -> bool {
        !matches!(self, Self::Modified)
    }
}

/// Relative order inside the frontend-selected movement domain.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Placement {
    Stable,
    Reordered,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LeafLink {
    pub before: NodeId,
    pub after: NodeId,
    pub relation: LeafRelation,
    pub placement: Placement,
    pub parent: ParentCorrespondence,
    /// Enclosing wrapper change, carried to presentation so retained content can appear as reflow.
    pub wrapper: Option<Reparenting>,
}

/// Exact payload equality or a replacement inferred from matching syntax roles.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LeafRelation {
    Exact,
    Modified,
}

/// Exact named subtree with placement and wrapper evidence for presentation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NodeLink {
    pub before: NodeId,
    pub after: NodeId,
    parent: ParentCorrespondence,
    pub wrapper: Option<Reparenting>,
    pub placement: Placement,
}

/// Exact subtree transferred between surviving nested owners.
/// Source formation must still validate line ownership and indentation before displaying a move.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RelocatedNode {
    pub before: NodeId,
    pub after: NodeId,
}

/// Paired semantic containers that constrain where descendant nodes and lines may match.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ScopeLink {
    pub before: NodeId,
    pub after: NodeId,
    placement: Placement,
    parent: ParentCorrespondence,
}

/// Evidence that a link connects matched semantic parents, possibly through a wrapper.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ParentCorrespondence {
    Direct,
    Reparented(Reparenting),
}

/// Wrapper insertion or removal justified by a single surviving containment path.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Reparenting {
    Wrapped,
    Unwrapped,
}

/// Collision-free identifier assigned by structural interning, never by hash value alone.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct FingerprintId(usize);

/// Intrinsic node facts; incoming field names live on parent-to-child edges.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct NodeAtom {
    kind: SyntaxKind,
    channel: Option<ContentChannel>,
    named: bool,
    extra: bool,
    missing: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct FingerprintEdge {
    slot: ChildSlot,
    fingerprint: FingerprintId,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct FingerprintKey<'source> {
    atom: NodeAtom,
    payload: Option<&'source str>,
    children: Vec<FingerprintEdge>,
}

#[derive(Default)]
struct FingerprintInterner<'source> {
    ids: HashMap<FingerprintKey<'source>, FingerprintId>,
}

impl<'source> FingerprintInterner<'source> {
    fn intern(&mut self, fingerprint: FingerprintKey<'source>) -> FingerprintId {
        if let Some(id) = self.ids.get(&fingerprint) {
            return *id;
        }

        let id = FingerprintId(self.ids.len());
        self.ids.insert(fingerprint, id);
        id
    }
}

#[derive(Clone, Copy)]
struct NodeFingerprints {
    full: FingerprintId,
    payload: Option<FingerprintId>,
    shape: FingerprintId,
}

/// Intern fingerprints comparable across both revisions through the shared interner.
/// Full omits layout children, payload also omits comments, and shape omits leaf text.
fn fingerprints<'source>(
    tree: &'source SyntaxTree<'_>,
    interner: &mut FingerprintInterner<'source>,
) -> Vec<NodeFingerprints> {
    let mut fingerprints = vec![None::<NodeFingerprints>; tree.nodes.len()];
    for index in (0..tree.nodes.len()).rev() {
        let id = NodeId::new(index);
        let node = tree.node(id);
        let payload = tree.leaf_text(id);
        let atom = NodeAtom {
            kind: node.kind,
            channel: node.leaf.map(|leaf| leaf.channel),
            named: node.named,
            extra: node.extra,
            missing: node.missing,
        };
        let mut full_children = Vec::new();
        let mut shape_children = Vec::new();
        let mut payload_children = Vec::new();
        for child in &node.children {
            let slot = tree.node(*child).slot;
            let fingerprint: NodeFingerprints =
                fingerprints[child.index()].expect("children follow parents in tree preorder");
            if !is_layout_leaf(tree, *child) {
                full_children.push(FingerprintEdge {
                    slot,
                    fingerprint: fingerprint.full,
                });
                shape_children.push(FingerprintEdge {
                    slot,
                    fingerprint: fingerprint.shape,
                });
            }
            if let Some(fingerprint) = fingerprint.payload {
                payload_children.push(FingerprintEdge { slot, fingerprint });
            }
        }
        let full = interner.intern(FingerprintKey {
            atom,
            payload,
            children: full_children,
        });
        let shape = interner.intern(FingerprintKey {
            atom,
            payload: None,
            children: shape_children,
        });

        let excluded_from_payload = node.leaf.is_some_and(|leaf| {
            matches!(
                leaf.channel,
                ContentChannel::Comment | ContentChannel::Layout
            )
        });
        let payload_fingerprint = if excluded_from_payload {
            None
        } else {
            Some(interner.intern(FingerprintKey {
                atom,
                payload,
                children: payload_children,
            }))
        };
        fingerprints[index] = Some(NodeFingerprints {
            full,
            payload: payload_fingerprint,
            shape,
        });
    }

    fingerprints
        .into_iter()
        .map(|fingerprint| fingerprint.expect("every tree node was fingerprinted"))
        .collect()
}

fn is_layout_leaf(tree: &SyntaxTree<'_>, id: NodeId) -> bool {
    tree.node(id)
        .leaf
        .is_some_and(|leaf| leaf.channel == ContentChannel::Layout)
}

#[derive(Clone)]
struct UnitRecord<'source> {
    id: NodeId,
    kind: SyntaxKind,
    identity: Option<&'source str>,
    atomic: bool,
    decoration_owner: Option<NodeId>,
    fingerprint: NodeFingerprints,
    comparison: ComparisonStrategy,
    role: SourceRole,
}

pub fn correspond(pair: &SyntaxPair<'_, '_>) -> Correspondence {
    let mut interner = FingerprintInterner::default();
    let before_fingerprints = fingerprints(&pair.before, &mut interner);
    let after_fingerprints = fingerprints(&pair.after, &mut interner);
    let before_subtree_sizes = subtree_sizes(&pair.before);
    let before_units = unit_records(&pair.before, &before_fingerprints);
    let after_units = unit_records(&pair.after, &after_fingerprints);
    let (before_match, after_match) = pair_units(
        &before_units,
        &after_units,
        pair.before.root,
        pair.after.root,
    );
    let stable = stable_unit_matches(&before_match, &before_units, &after_units);
    let root_scope = ScopeLink {
        before: pair.before.root,
        after: pair.after.root,
        placement: Placement::Stable,
        parent: ParentCorrespondence::Direct,
    };
    let tree = TreeDiff {
        units: Vec::new(),
        leaves: LeafCorrespondence {
            links: Vec::new(),
            before: vec![None; pair.before.nodes.len()],
            after: vec![None; pair.after.nodes.len()],
        },
        composites: Vec::new(),
        relocations: Vec::new(),
        scopes: vec![root_scope],
    };
    let mut before_scope = vec![None; pair.before.nodes.len()];
    let mut after_scope = vec![None; pair.after.nodes.len()];
    before_scope[pair.before.root.index()] = Some(0);
    after_scope[pair.after.root.index()] = Some(0);
    let mut builder = TreeDiffBuilder {
        pair,
        before_units: &before_units,
        after_units: &after_units,
        before_match: &before_match,
        after_match: &after_match,
        stable: &stable,
        before_fingerprints: &before_fingerprints,
        after_fingerprints: &after_fingerprints,
        before_subtree_sizes: &before_subtree_sizes,
        before_scope,
        after_scope,
        tree,
    };
    let units = builder.unit_script();
    builder.tree.units = units;
    let relocations = cross_owner_relocations(
        pair,
        &builder.tree,
        &before_fingerprints,
        &after_fingerprints,
        &before_subtree_sizes,
    );
    builder.tree.relocations = relocations;
    let tree = builder.tree;
    let source = project_source(pair, &tree);
    Correspondence { tree, source }
}

fn project_source(pair: &SyntaxPair<'_, '_>, tree: &TreeDiff) -> SourceProjection {
    let physical = if pair.before.grammar.is_none() {
        PhysicalLineFacts {
            exact: line_links_from_tree_matches(pair, tree),
            ..PhysicalLineFacts::default()
        }
    } else {
        physical_line_correspondence_in(
            pair,
            0..pair.before.source.lines().len(),
            0..pair.after.source.lines().len(),
        )
    };
    let scope_proof = ScopeProof::new(pair, &tree.scopes);
    let (lines, line_endings) = if pair.before.grammar.is_none() {
        (physical.exact, physical.ending_edits)
    } else {
        let scoped = scoped_physical_line_correspondence(pair, tree, &scope_proof, &physical);
        (scoped.exact, scoped.ending_edits)
    };
    let mut source = SourceProjection {
        lines,
        line_endings,
        fallbacks: Vec::new(),
        review_units: Vec::new(),
    };
    if pair.before.grammar.is_some() {
        let fallbacks = local_line_fallbacks(pair, tree, &source, physical.missing_terminators);
        source.review_units = tree
            .units
            .iter()
            .enumerate()
            .filter_map(|(index, edit)| {
                let unit = unit_line_geometry(pair, &source, edit);
                let overlaps_fallback = fallbacks.iter().any(|fallback| {
                    (!unit.before.is_empty()
                        && unit.before.start < fallback.before.end
                        && fallback.before.start < unit.before.end)
                        || (!unit.after.is_empty()
                            && unit.after.start < fallback.after.end
                            && fallback.after.start < unit.after.end)
                });
                (!overlaps_fallback).then_some(index)
            })
            .collect();
        source.fallbacks = fallbacks;
    } else {
        source.review_units = (0..tree.units.len()).collect();
    }
    debug_assert!(
        scope_correspondence_is_valid(pair, tree, &source, &scope_proof),
        "correspondence escaped a semantic-container fence"
    );
    source
}

/// Find exact subtrees transferred between surviving nested owners.
/// Keeping these moves separate from wrapper changes preserves sibling boundaries.
fn cross_owner_relocations(
    pair: &SyntaxPair<'_, '_>,
    tree: &TreeDiff,
    before_fingerprints: &[NodeFingerprints],
    after_fingerprints: &[NodeFingerprints],
    before_subtree_sizes: &[usize],
) -> Vec<RelocatedNode> {
    let (before_to_after, after_to_before) = paired_composite_owners(tree);
    let before_is_paired = |candidate| before_to_after.contains_key(&candidate);
    let after_is_paired = |candidate| after_to_before.contains_key(&candidate);
    let mut candidates = Vec::new();

    for edit in &tree.units {
        let UnitEdit::Matched(unit) = edit else {
            continue;
        };
        let mut before_groups = HashMap::<FingerprintId, Vec<NodeId>>::new();
        for before in descendant_composites(&pair.before, unit.before) {
            if before_to_after.contains_key(&before) {
                continue;
            }
            before_groups
                .entry(before_fingerprints[before.index()].full)
                .or_default()
                .push(before);
        }
        let mut after_groups = HashMap::<FingerprintId, Vec<NodeId>>::new();
        for after in descendant_composites(&pair.after, unit.after) {
            if after_to_before.contains_key(&after) {
                continue;
            }
            after_groups
                .entry(after_fingerprints[after.index()].full)
                .or_default()
                .push(after);
        }

        for (fingerprint, before_group) in before_groups {
            let Some(after_group) = after_groups.get(&fingerprint) else {
                continue;
            };
            let ([before], [after]) = (before_group.as_slice(), after_group.as_slice()) else {
                continue;
            };
            let before = *before;
            let after = *after;
            let before_node = pair.before.node(before);
            let after_node = pair.after.node(after);
            if before_node.seals_wrappers()
                || after_node.seals_wrappers()
                || before_node.slot != after_node.slot
            {
                continue;
            }

            let (Some(before_parent), Some(after_parent)) = (before_node.parent, after_node.parent)
            else {
                continue;
            };
            let Some(before_parent_after) = before_to_after.get(&before_parent).copied() else {
                continue;
            };
            let Some(after_parent_before) = after_to_before.get(&after_parent).copied() else {
                continue;
            };
            if before_parent_after == after_parent || after_parent_before == before_parent {
                continue;
            }

            let moved_inward = paired_owner_is_nearest_open_ancestor(
                &pair.before,
                after_parent_before,
                before_parent,
                &before_is_paired,
            ) && paired_owner_is_nearest_open_ancestor(
                &pair.after,
                after_parent,
                before_parent_after,
                &after_is_paired,
            );
            let moved_outward = paired_owner_is_nearest_open_ancestor(
                &pair.before,
                before_parent,
                after_parent_before,
                &before_is_paired,
            ) && paired_owner_is_nearest_open_ancestor(
                &pair.after,
                before_parent_after,
                after_parent,
                &after_is_paired,
            );
            if !moved_inward && !moved_outward {
                continue;
            }
            candidates.push(RelocatedNode { before, after });
        }
    }

    // Claiming outer subtrees first so nested candidates cannot report the same move twice.
    candidates.sort_by(|left, right| {
        before_subtree_sizes[right.before.index()]
            .cmp(&before_subtree_sizes[left.before.index()])
            .then_with(|| left.before.cmp(&right.before))
    });
    let mut relocations = Vec::<RelocatedNode>::new();
    for candidate in candidates {
        let overlaps = relocations.iter().any(|relocation| {
            pair.before.contains(relocation.before, candidate.before)
                || pair.before.contains(candidate.before, relocation.before)
                || pair.after.contains(relocation.after, candidate.after)
                || pair.after.contains(candidate.after, relocation.after)
        });
        if !overlaps {
            relocations.push(candidate);
        }
    }
    relocations.sort_unstable_by_key(|relocation| relocation.before);
    relocations
}

/// Check that `outer` is the nearest paired ancestor of `inner`, with no competing owner branches.
fn paired_owner_is_nearest_open_ancestor(
    tree: &SyntaxTree<'_>,
    inner: NodeId,
    outer: NodeId,
    is_paired: &impl Fn(NodeId) -> bool,
) -> bool {
    let mut branch = inner;
    let Some(mut candidate) = tree.node(inner).parent else {
        return false;
    };
    loop {
        if is_paired(candidate) {
            return candidate == outer;
        }

        let node = tree.node(candidate);
        if node.seals_wrappers()
            || node.review.is_some()
            || node.decoration_owner.is_some()
            || node
                .children
                .iter()
                .copied()
                .any(|child| child != branch && subtree_contains_fence(tree, child, is_paired))
        {
            return false;
        }
        branch = candidate;
        let Some(parent) = node.parent else {
            return false;
        };
        candidate = parent;
    }
}

fn paired_composite_owners(tree: &TreeDiff) -> (HashMap<NodeId, NodeId>, HashMap<NodeId, NodeId>) {
    let mut before_to_after = HashMap::new();
    let mut after_to_before = HashMap::new();
    let mut insert = |before, after| {
        let previous_after = before_to_after.insert(before, after);
        let previous_before = after_to_before.insert(after, before);
        debug_assert!(previous_after.is_none_or(|previous| previous == after));
        debug_assert!(previous_before.is_none_or(|previous| previous == before));
    };
    for edit in &tree.units {
        if let UnitEdit::Matched(unit) = edit {
            insert(unit.before, unit.after);
        }
    }
    for link in &tree.scopes {
        insert(link.before, link.after);
    }
    for link in &tree.composites {
        insert(link.before, link.after);
    }
    (before_to_after, after_to_before)
}

fn scope_correspondence_is_valid(
    pair: &SyntaxPair<'_, '_>,
    tree: &TreeDiff,
    source: &SourceProjection,
    scope_proof: &ScopeProof,
) -> bool {
    if !scope_proof.one_to_one {
        return false;
    }
    if tree.scopes.iter().any(|link| {
        !parent_correspondence_is_valid(pair, scope_proof, link.before, link.after, link.parent)
    }) {
        return false;
    }
    let edge_is_valid = |before, after, parent| {
        parent_correspondence_is_valid(pair, scope_proof, before, after, parent)
    };
    if tree
        .leaves
        .links
        .iter()
        .any(|link| !edge_is_valid(link.before, link.after, link.parent))
    {
        return false;
    }
    if tree
        .composites
        .iter()
        .any(|link| !edge_is_valid(link.before, link.after, link.parent))
    {
        return false;
    }

    source
        .lines
        .iter()
        .chain(&source.line_endings)
        .copied()
        .all(|link| scope_proof.line_has_scoped_cover(link, false))
}

fn parent_correspondence_is_valid(
    pair: &SyntaxPair<'_, '_>,
    scope_proof: &ScopeProof,
    before: NodeId,
    after: NodeId,
    parent: ParentCorrespondence,
) -> bool {
    match parent {
        ParentCorrespondence::Direct => scope_edge_is_valid(pair, scope_proof, before, after),
        ParentCorrespondence::Reparented(reparenting) => {
            scope_proof.containment_reparenting(pair, before, after) == Some(reparenting)
        }
    }
}

fn scope_edge_is_valid(
    pair: &SyntaxPair<'_, '_>,
    scope_proof: &ScopeProof,
    before: NodeId,
    after: NodeId,
) -> bool {
    match (
        enclosing_semantic_scope(&pair.before, before),
        enclosing_semantic_scope(&pair.after, after),
    ) {
        (None, None) => true,
        (Some(before), Some(after)) => scope_proof
            .link(before)
            .is_some_and(|link| link.after == after),
        (None, Some(_)) | (Some(_), None) => false,
    }
}

fn enclosing_semantic_scope(tree: &SyntaxTree<'_>, node: NodeId) -> Option<NodeId> {
    nearest_scope_owner(tree, tree.node(node).parent?)
}

/// Reuse line-tree matches as physical anchors without running another alignment.
fn line_links_from_tree_matches(pair: &SyntaxPair<'_, '_>, tree: &TreeDiff) -> Vec<LineLink> {
    let mut links = tree
        .units
        .iter()
        .filter_map(|edit| {
            let UnitEdit::Matched(unit) = edit else {
                return None;
            };
            let before_node = pair.before.node(unit.before);
            let after_node = pair.after.node(unit.after);
            let before_source = pair.before.source.slice(before_node.bytes.clone())?;
            let after_source = pair.after.source.slice(after_node.bytes.clone())?;
            (before_source == after_source).then(|| LineLink {
                before: before_node.lines.start.saturating_sub(1),
                after: after_node.lines.start.saturating_sub(1),
            })
        })
        .collect::<Vec<_>>();
    links.sort_unstable_by_key(|link| (link.before, link.after));
    links
}

#[derive(Default)]
struct PhysicalLineFacts {
    exact: Vec<LineLink>,
    ending_edits: Vec<LineLink>,
    missing_terminators: Vec<LineFallback>,
}

#[derive(Eq, Hash, PartialEq)]
struct ScopedLineValue<'source> {
    text: &'source str,
    /// Scope owners expressed as after-revision node ids on both sides of the comparison.
    scopes: Vec<NodeId>,
}

/// Reject ambiguous blank and punctuation-only anchors outside matched containers.
fn line_can_anchor_without_owner(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && !text
            .chars()
            .all(|character| character.is_ascii_punctuation())
}

/// Matched semantic scopes used to validate line and node correspondence.
struct ScopeProof {
    before_lines: Vec<Vec<NodeId>>,
    after_lines: Vec<Vec<NodeId>>,
    links: Vec<Option<ScopeLink>>,
    reverse_links: Vec<Option<ScopeLink>>,
    one_to_one: bool,
}

impl ScopeProof {
    fn new(pair: &SyntaxPair<'_, '_>, scopes: &[ScopeLink]) -> Self {
        let mut links = vec![None; pair.before.nodes.len()];
        let mut reverse_links = vec![None; pair.after.nodes.len()];
        let mut one_to_one = true;
        for link in scopes {
            let Some(before) = links.get_mut(link.before.index()) else {
                one_to_one = false;
                continue;
            };
            let Some(after) = reverse_links.get_mut(link.after.index()) else {
                one_to_one = false;
                continue;
            };
            if before.is_some() || after.is_some() {
                one_to_one = false;
                continue;
            }
            *before = Some(*link);
            *after = Some(*link);
        }
        Self {
            before_lines: scope_lines(&pair.before),
            after_lines: scope_lines(&pair.after),
            links,
            reverse_links,
            one_to_one,
        }
    }

    fn link(&self, before: NodeId) -> Option<ScopeLink> {
        self.links.get(before.index()).copied().flatten()
    }

    fn reverse_link(&self, after: NodeId) -> Option<ScopeLink> {
        self.reverse_links.get(after.index()).copied().flatten()
    }

    fn containment_reparenting(
        &self,
        pair: &SyntaxPair<'_, '_>,
        before: NodeId,
        after: NodeId,
    ) -> Option<Reparenting> {
        unique_containment_reparenting_with(
            pair,
            before,
            after,
            |candidate| self.link(candidate).map(|link| link.after),
            |candidate| self.reverse_link(candidate).map(|link| link.before),
        )
    }

    fn before_scopes(&self, line: usize) -> &[NodeId] {
        self.before_lines.get(line).map_or(&[], Vec::as_slice)
    }

    fn after_scopes(&self, line: usize) -> &[NodeId] {
        self.after_lines.get(line).map_or(&[], Vec::as_slice)
    }

    fn line_has_scoped_cover(&self, line: LineLink, require_stable: bool) -> bool {
        if !self.one_to_one {
            return false;
        }
        let before = self.before_scopes(line.before);
        let after = self.after_scopes(line.after);
        if before.is_empty() || after.is_empty() {
            return false;
        }

        let mut mapped = Vec::with_capacity(before.len());
        for before in before {
            let Some(link) = self.link(*before) else {
                return false;
            };
            if require_stable && link.placement != Placement::Stable {
                return false;
            }
            mapped.push(link.after);
        }
        mapped.sort_unstable();
        mapped.dedup();
        mapped == after
    }

    fn stable_before_line_scopes(&self, line: usize) -> Option<Vec<NodeId>> {
        let scopes = self.before_scopes(line);
        if scopes.is_empty() {
            return None;
        }
        let mut mapped = Vec::new();
        for before in scopes {
            let link = self.link(*before)?;
            if link.placement != Placement::Stable {
                return None;
            }
            mapped.push(link.after);
        }
        mapped.sort_unstable();
        mapped.dedup();
        Some(mapped)
    }

    fn stable_after_line_scopes(&self, line: usize) -> Option<Vec<NodeId>> {
        let scopes = self.after_scopes(line);
        if scopes.is_empty() {
            return None;
        }
        if scopes.iter().any(|after| {
            self.reverse_link(*after)
                .is_none_or(|link| link.placement != Placement::Stable)
        }) {
            return None;
        }
        Some(scopes.to_vec())
    }
}

/// Map every physical row to its nearest semantic owners, including layout-only rows.
fn scope_lines(tree: &SyntaxTree<'_>) -> Vec<Vec<NodeId>> {
    let mut lines = vec![Vec::new(); tree.source.lines().len()];
    for (index, node) in tree.nodes.iter().enumerate() {
        if node.leaf.is_none() {
            continue;
        }
        let id = NodeId::new(index);
        let Some(owner) = nearest_scope_owner(tree, id) else {
            continue;
        };
        for line in zero_based_lines(tree, id) {
            lines[line].push(owner);
        }
    }
    for owners in &mut lines {
        owners.sort_unstable();
        owners.dedup();
    }
    lines
}

fn nearest_scope_owner(tree: &SyntaxTree<'_>, mut candidate: NodeId) -> Option<NodeId> {
    loop {
        let node = tree.node(candidate);
        if node.is_scope_boundary() {
            return Some(candidate);
        }
        candidate = node.parent?;
    }
}

/// Align line ranges by text and record changes to their terminators.
/// Syntax anchors require a separate check that these matches respect scope ownership.
fn physical_line_correspondence_in(
    pair: &SyntaxPair<'_, '_>,
    before_bounds: Range<usize>,
    after_bounds: Range<usize>,
) -> PhysicalLineFacts {
    let before = &pair.before;
    let after = &pair.after;
    let before_text = before
        .source
        .lines()
        .get(before_bounds.clone())
        .unwrap_or_default()
        .iter()
        .map(|line| before.source.text(line))
        .collect::<Vec<_>>();
    let after_text = after
        .source
        .lines()
        .get(after_bounds.clone())
        .unwrap_or_default()
        .iter()
        .map(|line| after.source.text(line))
        .collect::<Vec<_>>();
    let anchors = ordered_matches(&before_text, &after_text);
    let mut facts = PhysicalLineFacts::default();
    let mut before_start = before_bounds.start;
    let mut after_start = after_bounds.start;
    for anchor in anchors.into_iter().chain(std::iter::once(OrderedMatch {
        before: before_text.len(),
        after: after_text.len(),
    })) {
        let before_anchor = before_bounds.start + anchor.before;
        let after_anchor = after_bounds.start + anchor.after;
        let before_gap = &before.source.lines()[before_start..before_anchor];
        let after_gap = &after.source.lines()[after_start..after_anchor];
        // Pairing terminator edits by offset only in equal-length gaps; insertions or
        // deletions make that offset unreliable.
        let paired = if before_gap.len() == after_gap.len() {
            before_gap.len()
        } else {
            0
        };
        for (offset, (before_line, after_line)) in
            before_gap.iter().zip(after_gap).take(paired).enumerate()
        {
            if before_line.ending == after_line.ending {
                continue;
            }
            facts.ending_edits.push(LineLink {
                before: before_start + offset,
                after: after_start + offset,
            });
        }
        for (offset, line) in before_gap.iter().enumerate().skip(paired) {
            if line.ending != LineEnding::Missing {
                continue;
            }
            let before = before_start + offset;
            facts.missing_terminators.push(LineFallback {
                before: before..before + 1,
                after: after_anchor..after_anchor,
            });
        }
        for (offset, line) in after_gap.iter().enumerate().skip(paired) {
            if line.ending != LineEnding::Missing {
                continue;
            }
            let after = after_start + offset;
            facts.missing_terminators.push(LineFallback {
                before: before_anchor..before_anchor,
                after: after..after + 1,
            });
        }

        if anchor.before < before_text.len() && anchor.after < after_text.len() {
            let link = LineLink {
                before: before_anchor,
                after: after_anchor,
            };
            if before.source.lines()[before_anchor].ending
                == after.source.lines()[after_anchor].ending
            {
                facts.exact.push(link);
            } else {
                facts.ending_edits.push(link);
            }
        }
        before_start = before_anchor.saturating_add(1);
        after_start = after_anchor.saturating_add(1);
    }
    facts
}

/// Pair rows within a matched node pair, including rows with changed terminators.
pub fn local_line_links(pair: &SyntaxPair<'_, '_>, before: NodeId, after: NodeId) -> Vec<LineLink> {
    let mut facts = physical_line_correspondence_in(
        pair,
        zero_based_lines(&pair.before, before),
        zero_based_lines(&pair.after, after),
    );
    facts.exact.append(&mut facts.ending_edits);
    facts
        .exact
        .sort_unstable_by_key(|link| (link.before, link.after));
    facts.exact
}

/// Keep physical anchors within matched scopes so repeated text cannot cross between owners.
fn scoped_physical_line_correspondence(
    pair: &SyntaxPair<'_, '_>,
    tree: &TreeDiff,
    scope_proof: &ScopeProof,
    global: &PhysicalLineFacts,
) -> PhysicalLineFacts {
    let mut scoped = PhysicalLineFacts {
        missing_terminators: global.missing_terminators.clone(),
        ..PhysicalLineFacts::default()
    };
    let semantically_reordered_lines = semantically_reordered_line_links(pair, tree);
    let before_candidates = pair
        .before
        .source
        .lines()
        .iter()
        .enumerate()
        .filter_map(|(line, source)| {
            let text = pair.before.source.text(source);
            let scopes = scope_proof.stable_before_line_scopes(line)?;
            let has_local_owner = scopes.iter().any(|scope| *scope != pair.after.root);
            if !has_local_owner && !line_can_anchor_without_owner(text) {
                return None;
            }
            Some((line, ScopedLineValue { text, scopes }))
        })
        .collect::<Vec<_>>();
    let after_candidates = pair
        .after
        .source
        .lines()
        .iter()
        .enumerate()
        .filter_map(|(line, source)| {
            let text = pair.after.source.text(source);
            let scopes = scope_proof.stable_after_line_scopes(line)?;
            let has_local_owner = scopes.iter().any(|scope| *scope != pair.after.root);
            if !has_local_owner && !line_can_anchor_without_owner(text) {
                return None;
            }
            Some((line, ScopedLineValue { text, scopes }))
        })
        .collect::<Vec<_>>();
    let before_values = before_candidates
        .iter()
        .map(|(_, value)| value)
        .collect::<Vec<_>>();
    let after_values = after_candidates
        .iter()
        .map(|(_, value)| value)
        .collect::<Vec<_>>();
    for edge in ordered_matches(&before_values, &after_values) {
        let link = LineLink {
            before: before_candidates[edge.before].0,
            after: after_candidates[edge.after].0,
        };
        if semantically_reordered_lines.contains(&link) {
            continue;
        }
        if pair.before.source.lines()[link.before].ending
            == pair.after.source.lines()[link.after].ending
        {
            scoped.exact.push(link);
        } else {
            scoped.ending_edits.push(link);
        }
    }

    let mut claimed_before = scoped
        .exact
        .iter()
        .chain(&scoped.ending_edits)
        .map(|link| link.before)
        .collect::<HashSet<_>>();
    let mut claimed_after = scoped
        .exact
        .iter()
        .chain(&scoped.ending_edits)
        .map(|link| link.after)
        .collect::<HashSet<_>>();
    let remaining_before = pair
        .before
        .source
        .lines()
        .iter()
        .enumerate()
        .filter(|(line, _)| !claimed_before.contains(line))
        .collect::<Vec<_>>();
    let remaining_after = pair
        .after
        .source
        .lines()
        .iter()
        .enumerate()
        .filter(|(line, _)| !claimed_after.contains(line))
        .collect::<Vec<_>>();

    // Aligning each gap separately so text matches cannot cross the stronger syntax
    // anchors and put the eventual source edits out of order.
    let mut semantic_anchors = scoped
        .exact
        .iter()
        .chain(&scoped.ending_edits)
        .copied()
        .collect::<Vec<_>>();
    semantic_anchors.sort_unstable_by_key(|link| (link.before, link.after));
    let mut before_start = 0;
    let mut after_start = 0;
    for anchor in semantic_anchors
        .iter()
        .copied()
        .map(Some)
        .chain(std::iter::once(None))
    {
        let before_end = anchor.map_or(pair.before.source.lines().len(), |link| link.before);
        let after_end = anchor.map_or(pair.after.source.lines().len(), |link| link.after);
        let before_gap = remaining_before
            .iter()
            .filter(|(line, _)| (before_start..before_end).contains(line))
            .collect::<Vec<_>>();
        let after_gap = remaining_after
            .iter()
            .filter(|(line, _)| (after_start..after_end).contains(line))
            .collect::<Vec<_>>();
        let before_text = before_gap
            .iter()
            .map(|(_, line)| pair.before.source.text(line))
            .collect::<Vec<_>>();
        let after_text = after_gap
            .iter()
            .map(|(_, line)| pair.after.source.text(line))
            .collect::<Vec<_>>();
        for edge in ordered_matches(&before_text, &after_text) {
            let link = LineLink {
                before: before_gap[edge.before].0,
                after: after_gap[edge.after].0,
            };
            if pair.before.source.lines()[link.before].ending
                != pair.after.source.lines()[link.after].ending
                || semantically_reordered_lines.contains(&link)
                || !scope_proof.line_has_scoped_cover(link, true)
            {
                continue;
            }
            claimed_before.insert(link.before);
            claimed_after.insert(link.after);
            scoped.exact.push(link);
        }
        if let Some(anchor) = anchor {
            before_start = anchor.before + 1;
            after_start = anchor.after + 1;
        }
    }

    let mut accepted = scoped
        .exact
        .iter()
        .chain(&scoped.ending_edits)
        .copied()
        .collect::<Vec<_>>();
    for link in &global.ending_edits {
        if claimed_before.contains(&link.before) || claimed_after.contains(&link.after) {
            continue;
        }
        if semantically_reordered_lines.contains(link) {
            continue;
        }
        if !scope_proof.line_has_scoped_cover(*link, true)
            || !line_link_preserves_order(*link, &accepted)
        {
            continue;
        }
        claimed_before.insert(link.before);
        claimed_after.insert(link.after);
        scoped.ending_edits.push(*link);
        accepted.push(*link);
    }
    sort_and_deduplicate_line_links(&mut scoped.exact);
    sort_and_deduplicate_line_links(&mut scoped.ending_edits);
    debug_assert!(line_links_are_monotone(&scoped.exact, &scoped.ending_edits));
    scoped.missing_terminators.sort_by_key(|fallback| {
        (
            fallback.before.start,
            fallback.before.end,
            fallback.after.start,
            fallback.after.end,
        )
    });
    scoped.missing_terminators.dedup();
    scoped
}

fn line_link_preserves_order(candidate: LineLink, accepted: &[LineLink]) -> bool {
    accepted.iter().all(|anchor| {
        (anchor.before < candidate.before && anchor.after < candidate.after)
            || (anchor.before > candidate.before && anchor.after > candidate.after)
    })
}

fn line_links_are_monotone(exact: &[LineLink], endings: &[LineLink]) -> bool {
    let mut links = exact.iter().chain(endings).copied().collect::<Vec<_>>();
    links.sort_unstable_by_key(|link| (link.before, link.after));
    links
        .windows(2)
        .all(|links| links[0].before < links[1].before && links[0].after < links[1].after)
}

/// Collect moved rows that must not become unchanged line anchors.
fn semantically_reordered_line_links(
    pair: &SyntaxPair<'_, '_>,
    tree: &TreeDiff,
) -> HashSet<LineLink> {
    let mut lines = HashSet::new();
    for relocation in &tree.relocations {
        let before = zero_based_lines(&pair.before, relocation.before);
        let after = zero_based_lines(&pair.after, relocation.after);
        lines.extend(
            before
                .zip(after)
                .map(|(before, after)| LineLink { before, after }),
        );
    }
    for link in &tree.leaves.links {
        if link.relation != LeafRelation::Exact
            || link.placement != Placement::Reordered
            || !node_owns_complete_lines(&pair.before, link.before)
            || !node_owns_complete_lines(&pair.after, link.after)
        {
            continue;
        }
        let before = zero_based_lines(&pair.before, link.before);
        let after = zero_based_lines(&pair.after, link.after);
        lines.extend(
            before.zip(after).filter_map(|(before, after)| {
                (before != after).then_some(LineLink { before, after })
            }),
        );
    }
    lines
}

fn sort_and_deduplicate_line_links(links: &mut Vec<LineLink>) {
    links.sort_unstable_by_key(|link| (link.before, link.after));
    links.dedup();
}

#[derive(Clone, Debug)]
struct UnitLineGeometry {
    before: Range<usize>,
    after: Range<usize>,
    changed: bool,
    expands_fallback: bool,
}

/// Choose linewise review where syntax would omit a change or claim a line twice.
fn local_line_fallbacks(
    pair: &SyntaxPair<'_, '_>,
    tree: &TreeDiff,
    source: &SourceProjection,
    mut fallbacks: Vec<LineFallback>,
) -> Vec<LineFallback> {
    // Letting moved units report their own missing EOF terminator; global line alignment
    // may place its empty counterpart far from the move.
    fallbacks.retain(|fallback| !move_owns_missing_terminator(pair, tree, fallback));
    let units = tree
        .units
        .iter()
        .map(|edit| unit_line_geometry(pair, source, edit))
        .collect::<Vec<_>>();
    let mut before_claims = vec![0_u16; pair.before.source.lines().len()];
    let mut after_claims = vec![0_u16; pair.after.source.lines().len()];
    // Allowing unchanged units to claim rows skipped by line alignment only when every
    // byte agrees, including layout and terminators.
    for unit in units
        .iter()
        .filter(|unit| unit.changed || unit_lines_are_source_equal(pair, unit))
    {
        increment_claims(&mut before_claims, unit.before.clone());
        increment_claims(&mut after_claims, unit.after.clone());
    }
    claim_paired_adjacent_layout(pair, tree, &mut before_claims, &mut after_claims);

    let mut checkpoints = source.lines.clone();
    checkpoints.extend(&source.line_endings);
    checkpoints.sort_unstable_by_key(|link| (link.before, link.after));
    let mut before_start = 0;
    let mut after_start = 0;
    for checkpoint in checkpoints.into_iter().chain(std::iter::once(LineLink {
        before: before_claims.len(),
        after: after_claims.len(),
    })) {
        fallbacks.extend(unclaimed_gap_fallbacks(
            before_start..checkpoint.before,
            after_start..checkpoint.after,
            &before_claims,
            &after_claims,
        ));
        before_start = checkpoint.before.saturating_add(1);
        after_start = checkpoint.after.saturating_add(1);
    }

    for link in &source.line_endings {
        if move_owns_line_ending(pair, tree, *link) {
            continue;
        }
        fallbacks.push(LineFallback {
            before: link.before..link.before + 1,
            after: link.after..link.after + 1,
        });
    }
    fallbacks.extend(conflicting_unit_fallbacks(pair, &units));
    close_fallbacks_over_changed_units(&mut fallbacks, &units);
    normalize_line_fallbacks(fallbacks)
}

fn move_owns_missing_terminator(
    pair: &SyntaxPair<'_, '_>,
    tree: &TreeDiff,
    fallback: &LineFallback,
) -> bool {
    if fallback.before.is_empty() == fallback.after.is_empty() {
        return false;
    }
    tree.units.iter().any(|edit| {
        let UnitEdit::Matched(unit) = edit else {
            return false;
        };
        if unit.placement != Placement::Reordered || !unit.relation.full_equal() {
            return false;
        }
        if fallback.after.is_empty() {
            return range_contains(
                &zero_based_lines(&pair.before, unit.before),
                &fallback.before,
            );
        }
        range_contains(&zero_based_lines(&pair.after, unit.after), &fallback.after)
    })
}

fn range_contains(outer: &Range<usize>, inner: &Range<usize>) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// Check whether an exact move can present this terminator edit itself.
fn move_owns_line_ending(pair: &SyntaxPair<'_, '_>, tree: &TreeDiff, link: LineLink) -> bool {
    tree.units.iter().any(|edit| {
        let UnitEdit::Matched(unit) = edit else {
            return false;
        };
        unit.placement == Placement::Reordered
            && unit.relation.full_equal()
            && zero_based_lines(&pair.before, unit.before).contains(&link.before)
            && zero_based_lines(&pair.after, unit.after).contains(&link.after)
    })
}

fn unit_line_geometry(
    pair: &SyntaxPair<'_, '_>,
    source: &SourceProjection,
    edit: &UnitEdit,
) -> UnitLineGeometry {
    let mut geometry = match edit {
        UnitEdit::Matched(unit) => UnitLineGeometry {
            before: zero_based_lines(&pair.before, unit.before),
            after: zero_based_lines(&pair.after, unit.after),
            changed: unit.relation != ContentRelation::SourceEqual
                || unit.placement != Placement::Stable,
            expands_fallback: true,
        },
        UnitEdit::Removed { before } => UnitLineGeometry {
            before: zero_based_lines(&pair.before, *before),
            after: 0..0,
            changed: true,
            expands_fallback: true,
        },
        UnitEdit::Added { after } => UnitLineGeometry {
            before: 0..0,
            after: zero_based_lines(&pair.after, *after),
            changed: true,
            expands_fallback: true,
        },
    };
    geometry.expands_fallback &= !unit_lines_are_physically_paired(source, &geometry);
    geometry
}

fn unit_lines_are_physically_paired(source: &SourceProjection, unit: &UnitLineGeometry) -> bool {
    if unit.before.len() != unit.after.len() {
        return false;
    }
    let mut pairs = source
        .line_links_in(unit.before.clone(), unit.after.clone())
        .chain(source.line_ending_edits_in(unit.before.clone(), unit.after.clone()))
        .collect::<Vec<_>>();
    pairs.sort_unstable_by_key(|link| (link.before, link.after));
    pairs.len() == unit.before.len()
        && pairs.iter().enumerate().all(|(offset, link)| {
            link.before == unit.before.start + offset && link.after == unit.after.start + offset
        })
}

fn unit_lines_are_source_equal(pair: &SyntaxPair<'_, '_>, unit: &UnitLineGeometry) -> bool {
    unit.before.len() == unit.after.len()
        && unit
            .before
            .clone()
            .zip(unit.after.clone())
            .all(|(before, after)| {
                let before = &pair.before.source.lines()[before];
                let after = &pair.after.source.lines()[after];
                pair.before.source.full_text(before) == pair.after.source.full_text(after)
            })
}

fn zero_based_lines(tree: &SyntaxTree<'_>, node: NodeId) -> Range<usize> {
    let lines = tree.node(node).lines.clone();
    let start = lines.start.saturating_sub(1).min(tree.source.lines().len());
    let end = lines.end.saturating_sub(1).min(tree.source.lines().len());
    start.min(end)..end
}

fn increment_claims(claims: &mut [u16], lines: Range<usize>) {
    for claim in &mut claims[lines] {
        *claim = claim.saturating_add(1);
    }
}

/// Claim unchanged blank separators with their owners so moves do not create spurious edits.
fn claim_paired_adjacent_layout(
    pair: &SyntaxPair<'_, '_>,
    tree: &TreeDiff,
    before_claims: &mut [u16],
    after_claims: &mut [u16],
) {
    for edit in &tree.units {
        let UnitEdit::Matched(unit) = edit else {
            continue;
        };
        let before = pair.before.node(unit.before);
        let after = pair.after.node(unit.after);
        let before_layout = before.review.as_ref().map(|review| review.layout);
        let after_layout = after.review.as_ref().map(|review| review.layout);
        if before_layout != Some(LayoutOwnership::AdjacentBlankLines)
            || after_layout != Some(LayoutOwnership::AdjacentBlankLines)
        {
            continue;
        }

        let before_lines = zero_based_lines(&pair.before, unit.before);
        let after_lines = zero_based_lines(&pair.after, unit.after);
        let before_leading = adjacent_blank_lines(&pair.before, before_lines.start, true);
        let after_leading = adjacent_blank_lines(&pair.after, after_lines.start, true);
        let before_trailing = adjacent_blank_lines(&pair.before, before_lines.end, false);
        let after_trailing = adjacent_blank_lines(&pair.after, after_lines.end, false);
        if unit.placement == Placement::Reordered {
            // Pairing both sides together; a move can turn a leading separator into a trailing one.
            let before_adjacent = before_leading
                .iter()
                .rev()
                .chain(&before_trailing)
                .copied()
                .collect::<Vec<_>>();
            let after_adjacent = after_leading
                .iter()
                .rev()
                .chain(&after_trailing)
                .copied()
                .collect::<Vec<_>>();
            claim_equal_layout_run(
                &pair.before,
                &pair.after,
                &before_adjacent,
                &after_adjacent,
                before_claims,
                after_claims,
            );
            continue;
        }

        claim_equal_layout_run(
            &pair.before,
            &pair.after,
            &before_leading,
            &after_leading,
            before_claims,
            after_claims,
        );
        claim_equal_layout_run(
            &pair.before,
            &pair.after,
            &before_trailing,
            &after_trailing,
            before_claims,
            after_claims,
        );
    }
}

fn adjacent_blank_lines(tree: &SyntaxTree<'_>, boundary: usize, before: bool) -> Vec<usize> {
    let mut lines = Vec::new();
    if before {
        let mut index = boundary;
        while let Some(previous) = index.checked_sub(1) {
            if !source_line_is_blank(tree, previous) {
                break;
            }
            lines.push(previous);
            index = previous;
        }
        lines.reverse();
        return lines;
    }

    let mut index = boundary;
    while source_line_is_blank(tree, index) {
        lines.push(index);
        index += 1;
    }
    lines
}

fn source_line_is_blank(tree: &SyntaxTree<'_>, index: usize) -> bool {
    tree.source
        .lines()
        .get(index)
        .is_some_and(|line| tree.source.text(line).trim().is_empty())
}

fn claim_equal_layout_run(
    before: &SyntaxTree<'_>,
    after: &SyntaxTree<'_>,
    before_lines: &[usize],
    after_lines: &[usize],
    before_claims: &mut [u16],
    after_claims: &mut [u16],
) {
    if before_lines.len() != after_lines.len()
        || !before_lines
            .iter()
            .zip(after_lines)
            .all(|(before_index, after_index)| {
                let before_line = &before.source.lines()[*before_index];
                let after_line = &after.source.lines()[*after_index];
                before.source.full_text(before_line) == after.source.full_text(after_line)
            })
    {
        return;
    }

    for (before_index, after_index) in before_lines
        .iter()
        .copied()
        .zip(after_lines.iter().copied())
    {
        let before_line = &before.source.lines()[before_index];
        let after_line = &after.source.lines()[after_index];
        debug_assert_eq!(
            before.source.full_text(before_line),
            after.source.full_text(after_line)
        );
        before_claims[before_index] = before_claims[before_index].saturating_add(1);
        after_claims[after_index] = after_claims[after_index].saturating_add(1);
    }
}

fn unclaimed_gap_fallbacks(
    before: Range<usize>,
    after: Range<usize>,
    before_claims: &[u16],
    after_claims: &[u16],
) -> Vec<LineFallback> {
    let before_runs = unclaimed_runs(before.clone(), before_claims);
    let after_runs = unclaimed_runs(after.clone(), after_claims);
    if before_runs.is_empty() && after_runs.is_empty() {
        return Vec::new();
    }
    if before_runs.len() == after_runs.len() {
        return before_runs
            .into_iter()
            .zip(after_runs)
            .map(|(before, after)| LineFallback { before, after })
            .collect();
    }
    if before_runs.is_empty() {
        return after_runs
            .into_iter()
            .map(|run| LineFallback {
                before: empty_counterpart(&run, &after, &before),
                after: run,
            })
            .collect();
    }
    if after_runs.is_empty() {
        return before_runs
            .into_iter()
            .map(|run| LineFallback {
                before: run.clone(),
                after: empty_counterpart(&run, &before, &after),
            })
            .collect();
    }

    // Falling back over the whole gap; unequal runs cannot be paired without consuming
    // lines already claimed by syntax.
    vec![LineFallback { before, after }]
}

fn unclaimed_runs(lines: Range<usize>, claims: &[u16]) -> Vec<Range<usize>> {
    let mut runs = Vec::new();
    let mut start = None;
    for line in lines.clone() {
        if claims[line] == 0 {
            start.get_or_insert(line);
            continue;
        }
        if let Some(start) = start.take() {
            runs.push(start..line);
        }
    }
    if let Some(start) = start {
        runs.push(start..lines.end);
    }
    runs
}

fn empty_counterpart(
    run: &Range<usize>,
    own_gap: &Range<usize>,
    other_gap: &Range<usize>,
) -> Range<usize> {
    let boundary = if run.start == own_gap.start {
        other_gap.start
    } else if run.end == own_gap.end {
        other_gap.end
    } else {
        // Interpolating within the gap; internal layout has no known position in the other revision.
        other_gap.start + other_gap.len().saturating_mul(run.start - own_gap.start) / own_gap.len()
    };
    boundary..boundary
}

fn conflicting_unit_fallbacks(
    pair: &SyntaxPair<'_, '_>,
    units: &[UnitLineGeometry],
) -> Vec<LineFallback> {
    let mut before_owner = vec![None; pair.before.source.lines().len()];
    let mut after_owner = vec![None; pair.after.source.lines().len()];
    let mut conflicts = HashSet::new();
    for (index, unit) in units.iter().enumerate() {
        record_claim_conflicts(
            &mut before_owner,
            unit.before.clone(),
            index,
            &mut conflicts,
        );
        record_claim_conflicts(&mut after_owner, unit.after.clone(), index, &mut conflicts);
    }
    conflicts
        .into_iter()
        .filter(|(left, right)| units[*left].changed || units[*right].changed)
        .map(|(left, right)| LineFallback {
            before: range_union(&units[left].before, &units[right].before),
            after: range_union(&units[left].after, &units[right].after),
        })
        .collect()
}

fn record_claim_conflicts(
    owners: &mut [Option<usize>],
    lines: Range<usize>,
    owner: usize,
    conflicts: &mut HashSet<(usize, usize)>,
) {
    for line in lines {
        let Some(previous) = owners[line].replace(owner) else {
            continue;
        };
        if previous != owner {
            conflicts.insert((previous.min(owner), previous.max(owner)));
        }
    }
}

fn close_fallbacks_over_changed_units(
    fallbacks: &mut Vec<LineFallback>,
    units: &[UnitLineGeometry],
) {
    let fallback_count = fallbacks.len();
    let mut regions = std::mem::take(fallbacks);
    regions.extend(
        units
            .iter()
            .filter(|unit| unit.changed && unit.expands_fallback)
            .map(|unit| LineFallback {
                before: unit.before.clone(),
                after: unit.after.clone(),
            }),
    );
    if regions.len() == fallback_count {
        *fallbacks = regions;
        return;
    }
    let mut parents = (0..regions.len()).collect::<Vec<_>>();
    close_fallback_components(&regions, &mut parents);
    let fallback_roots = (0..fallback_count)
        .map(|index| fallback_component_root(&mut parents, index))
        .collect::<HashSet<_>>();
    let components = fallback_component_ranges(&regions, &mut parents);
    *fallbacks = components
        .into_iter()
        .filter_map(|(root, fallback)| fallback_roots.contains(&root).then_some(fallback))
        .collect();
    sort_line_fallbacks(fallbacks);
}

fn normalize_line_fallbacks(mut fallbacks: Vec<LineFallback>) -> Vec<LineFallback> {
    fallbacks.retain(|fallback| !fallback.before.is_empty() || !fallback.after.is_empty());
    let mut parents = (0..fallbacks.len()).collect::<Vec<_>>();
    close_fallback_components(&fallbacks, &mut parents);
    let mut normalized = fallback_component_ranges(&fallbacks, &mut parents)
        .into_iter()
        .map(|(_, fallback)| fallback)
        .collect::<Vec<_>>();
    sort_line_fallbacks(&mut normalized);
    normalized
}

/// Merge regions that overlap in either revision until their combined ranges stop growing.
/// A union on one side can expose another overlap on the other.
fn close_fallback_components(fallbacks: &[LineFallback], parents: &mut [usize]) {
    loop {
        let components = fallback_component_ranges(fallbacks, parents);
        let changed = union_overlapping_fallbacks(&components, parents, true)
            | union_overlapping_fallbacks(&components, parents, false);
        if !changed {
            break;
        }
    }
}

fn sort_line_fallbacks(fallbacks: &mut [LineFallback]) {
    fallbacks.sort_by_key(|fallback| {
        (
            fallback.after.start,
            fallback.before.start,
            fallback.after.end,
            fallback.before.end,
        )
    });
}

fn union_overlapping_fallbacks(
    components: &[(usize, LineFallback)],
    parents: &mut [usize],
    before_side: bool,
) -> bool {
    let range = |index: usize| {
        if before_side {
            &components[index].1.before
        } else {
            &components[index].1.after
        }
    };
    let mut indices = (0..components.len())
        .filter(|index| !range(*index).is_empty())
        .collect::<Vec<_>>();
    indices.sort_unstable_by_key(|index| (range(*index).start, range(*index).end));
    let Some(mut representative) = indices.first().copied() else {
        return false;
    };
    let mut end = range(representative).end;
    let mut changed = false;
    for index in indices.into_iter().skip(1) {
        let current = range(index);
        if current.start >= end {
            representative = index;
            end = current.end;
            continue;
        }
        changed |=
            union_fallback_components(parents, components[representative].0, components[index].0);
        end = end.max(current.end);
    }
    changed
}

fn fallback_component_ranges(
    fallbacks: &[LineFallback],
    parents: &mut [usize],
) -> Vec<(usize, LineFallback)> {
    let mut components = HashMap::<usize, LineFallback>::new();
    for (index, fallback) in fallbacks.iter().enumerate() {
        let root = fallback_component_root(parents, index);
        let component = components.entry(root).or_insert_with(|| fallback.clone());
        component.before = range_union(&component.before, &fallback.before);
        component.after = range_union(&component.after, &fallback.after);
    }
    components.into_iter().collect()
}

fn union_fallback_components(parents: &mut [usize], left: usize, right: usize) -> bool {
    let left = fallback_component_root(parents, left);
    let right = fallback_component_root(parents, right);
    if left == right {
        return false;
    }
    parents[right] = left;
    true
}

fn fallback_component_root(parents: &mut [usize], mut index: usize) -> usize {
    let mut root = index;
    while parents[root] != root {
        root = parents[root];
    }
    while parents[index] != index {
        let parent = parents[index];
        parents[index] = root;
        index = parent;
    }
    root
}

fn range_union(left: &Range<usize>, right: &Range<usize>) -> Range<usize> {
    if left.is_empty() && right.is_empty() {
        return left.clone();
    }
    if left.is_empty() {
        return right.clone();
    }
    if right.is_empty() {
        return left.clone();
    }
    left.start.min(right.start)..left.end.max(right.end)
}

fn unit_records<'source>(
    tree: &SyntaxTree<'source>,
    fingerprints: &[NodeFingerprints],
) -> Vec<UnitRecord<'source>> {
    tree.review_units()
        .map(|(id, node)| {
            let fingerprint = fingerprints[id.index()];
            let review = node
                .review
                .as_ref()
                .expect("review node owns review metadata");
            UnitRecord {
                id,
                kind: node.kind,
                // Keeping leaf payload out of name matching; atomic units use local source order.
                identity: node
                    .leaf
                    .is_none()
                    .then(|| tree.identity_text(id))
                    .flatten(),
                atomic: node.leaf.is_some(),
                decoration_owner: node.decoration_owner,
                fingerprint,
                comparison: review.comparison,
                role: review.role,
            }
        })
        .collect()
}

fn pair_units(
    before: &[UnitRecord<'_>],
    after: &[UnitRecord<'_>],
    before_root: NodeId,
    after_root: NodeId,
) -> (Vec<Option<usize>>, Vec<Option<usize>>) {
    let mut before_match = vec![None; before.len()];
    let mut after_match = vec![None; after.len()];

    let before_root_unit = before.iter().position(|unit| unit.id == before_root);
    let after_root_unit = after.iter().position(|unit| unit.id == after_root);
    if let (Some(before_root_unit), Some(after_root_unit)) = (before_root_unit, after_root_unit) {
        link_unit_indices(
            before_root_unit,
            after_root_unit,
            &mut before_match,
            &mut after_match,
        );
    }

    pair_keyed_units(before, after, &mut before_match, &mut after_match);
    pair_atomic_units(before, after, &mut before_match, &mut after_match);
    pair_compatible_units(before, after, &mut before_match, &mut after_match);
    pair_decorated_units(
        before,
        after,
        before_root,
        after_root,
        &mut before_match,
        &mut after_match,
    );
    (before_match, after_match)
}

/// Pair decorations only within owners that have already matched.
fn pair_decorated_units(
    before: &[UnitRecord<'_>],
    after: &[UnitRecord<'_>],
    before_root: NodeId,
    after_root: NodeId,
    before_match: &mut [Option<usize>],
    after_match: &mut [Option<usize>],
) {
    let mut owner_pairs = before_match
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(before_owner, after_owner)| {
            after_owner.map(|after_owner| (before_owner, after_owner))
        })
        .map(|(before_owner, after_owner)| (before[before_owner].id, after[after_owner].id))
        .collect::<Vec<_>>();
    owner_pairs.push((before_root, after_root));
    owner_pairs.sort_unstable();
    owner_pairs.dedup();
    for (before_owner_id, after_owner_id) in owner_pairs {
        let before_group = before
            .iter()
            .enumerate()
            .filter(|(index, unit)| {
                before_match[*index].is_none() && unit.decoration_owner == Some(before_owner_id)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let after_group = after
            .iter()
            .enumerate()
            .filter(|(index, unit)| {
                after_match[*index].is_none() && unit.decoration_owner == Some(after_owner_id)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if before_group.is_empty() || after_group.is_empty() {
            continue;
        }

        let before_exact = before_group
            .iter()
            .map(|index| before[*index].fingerprint.full)
            .collect::<Vec<_>>();
        let after_exact = after_group
            .iter()
            .map(|index| after[*index].fingerprint.full)
            .collect::<Vec<_>>();
        for edge in ordered_matches(&before_exact, &after_exact) {
            link_unit_indices(
                before_group[edge.before],
                after_group[edge.after],
                before_match,
                after_match,
            );
        }

        let remaining_before = before_group
            .iter()
            .copied()
            .filter(|index| before_match[*index].is_none())
            .collect::<Vec<_>>();
        let remaining_after = after_group
            .iter()
            .copied()
            .filter(|index| after_match[*index].is_none())
            .collect::<Vec<_>>();
        let before_shapes = remaining_before
            .iter()
            .map(|index| (before[*index].kind, before[*index].fingerprint.shape))
            .collect::<Vec<_>>();
        let after_shapes = remaining_after
            .iter()
            .map(|index| (after[*index].kind, after[*index].fingerprint.shape))
            .collect::<Vec<_>>();
        for edge in ordered_matches(&before_shapes, &after_shapes) {
            link_unit_indices(
                remaining_before[edge.before],
                remaining_after[edge.after],
                before_match,
                after_match,
            );
        }

        let remaining_before = before_group
            .iter()
            .copied()
            .filter(|index| before_match[*index].is_none())
            .collect::<Vec<_>>();
        let remaining_after = after_group
            .iter()
            .copied()
            .filter(|index| after_match[*index].is_none())
            .collect::<Vec<_>>();
        let before_kinds = remaining_before
            .iter()
            .map(|index| before[*index].kind)
            .collect::<Vec<_>>();
        let after_kinds = remaining_after
            .iter()
            .map(|index| after[*index].kind)
            .collect::<Vec<_>>();
        for edge in ordered_matches(&before_kinds, &after_kinds) {
            link_unit_indices(
                remaining_before[edge.before],
                remaining_after[edge.after],
                before_match,
                after_match,
            );
        }
    }
}

fn pair_keyed_units(
    before: &[UnitRecord<'_>],
    after: &[UnitRecord<'_>],
    before_match: &mut [Option<usize>],
    after_match: &mut [Option<usize>],
) {
    let mut before_groups = HashMap::<(SyntaxKind, &str), Vec<usize>>::new();
    for (index, unit) in before.iter().enumerate() {
        if before_match[index].is_some() || unit.decoration_owner.is_some() {
            continue;
        }
        let Some(identity) = unit.identity else {
            continue;
        };
        before_groups
            .entry((unit.kind, identity))
            .or_default()
            .push(index);
    }

    let mut after_groups = HashMap::<(SyntaxKind, &str), Vec<usize>>::new();
    for (index, unit) in after.iter().enumerate() {
        if after_match[index].is_some() || unit.decoration_owner.is_some() {
            continue;
        }
        let Some(identity) = unit.identity else {
            continue;
        };
        after_groups
            .entry((unit.kind, identity))
            .or_default()
            .push(index);
    }

    for (key, before_group) in before_groups {
        let Some(after_group) = after_groups.get(&key) else {
            continue;
        };

        // Pairing repeated names in source order so body edits cannot switch their identities.
        for (before_index, after_index) in before_group.into_iter().zip(after_group.iter().copied())
        {
            link_unit_indices(before_index, after_index, before_match, after_match);
        }
    }
}

/// Pair atomic units by nearby exact content before attempting positional replacements.
fn pair_atomic_units(
    before: &[UnitRecord<'_>],
    after: &[UnitRecord<'_>],
    before_match: &mut [Option<usize>],
    after_match: &mut [Option<usize>],
) {
    let before_indices = before
        .iter()
        .enumerate()
        .filter(|(index, unit)| {
            before_match[*index].is_none()
                && unit.identity.is_none()
                && unit.atomic
                && unit.decoration_owner.is_none()
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let after_indices = after
        .iter()
        .enumerate()
        .filter(|(index, unit)| {
            after_match[*index].is_none()
                && unit.identity.is_none()
                && unit.atomic
                && unit.decoration_owner.is_none()
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let before_values = before_indices
        .iter()
        .map(|index| before[*index].fingerprint.full)
        .collect::<Vec<_>>();
    let after_values = after_indices
        .iter()
        .map(|index| after[*index].fingerprint.full)
        .collect::<Vec<_>>();
    for edge in locality_first_matches(&before_values, &after_values) {
        link_unit_indices(
            before_indices[edge.before],
            after_indices[edge.after],
            before_match,
            after_match,
        );
    }
}

/// Pair remaining compatible units within gaps between established matches.
fn pair_compatible_units(
    before: &[UnitRecord<'_>],
    after: &[UnitRecord<'_>],
    before_match: &mut [Option<usize>],
    after_match: &mut [Option<usize>],
) {
    let stable = ordered_unit_anchors(before_match, before);
    let mut anchors = before_match
        .iter()
        .enumerate()
        .filter_map(|(before, after)| {
            let after = (*after)?;
            stable[before].then_some((before, after))
        })
        .collect::<Vec<_>>();
    anchors.push((before.len(), after.len()));

    let mut before_start = 0;
    let mut after_start = 0;
    for (before_end, after_end) in anchors {
        let before_indices = (before_start..before_end)
            .filter(|index| {
                before_match[*index].is_none() && before[*index].decoration_owner.is_none()
            })
            .collect::<Vec<_>>();
        let after_indices = (after_start..after_end)
            .filter(|index| {
                after_match[*index].is_none() && after[*index].decoration_owner.is_none()
            })
            .collect::<Vec<_>>();
        let before_values = before_indices
            .iter()
            .map(|index| (before[*index].kind, before[*index].comparison))
            .collect::<Vec<_>>();
        let after_values = after_indices
            .iter()
            .map(|index| (after[*index].kind, after[*index].comparison))
            .collect::<Vec<_>>();
        for edge in ordered_matches(&before_values, &after_values) {
            link_unit_indices(
                before_indices[edge.before],
                after_indices[edge.after],
                before_match,
                after_match,
            );
        }
        before_start = before_end.saturating_add(1);
        after_start = after_end.saturating_add(1);
    }
}

/// Accept a pair only when each endpoint uniquely prefers the other and exceeds the threshold.
fn reciprocal_unique_matches(
    before_len: usize,
    after_len: usize,
    minimum_similarity: u64,
    similarity: impl Fn(usize, usize) -> u64,
) -> Vec<OrderedMatch> {
    if before_len == 0 || after_len == 0 {
        return Vec::new();
    }

    let mut similarities = vec![0_u64; before_len * after_len];
    for before in 0..before_len {
        for after in 0..after_len {
            similarities[before * after_len + after] = similarity(before, after);
        }
    }
    let before_best = (0..before_len)
        .map(|before| {
            unique_best(
                (0..after_len).map(|after| (after, similarities[before * after_len + after])),
            )
        })
        .collect::<Vec<_>>();
    let after_best = (0..after_len)
        .map(|after| {
            unique_best(
                (0..before_len).map(|before| (before, similarities[before * after_len + after])),
            )
        })
        .collect::<Vec<_>>();

    before_best
        .into_iter()
        .enumerate()
        .filter_map(|(before, best)| {
            let (after, similarity) = best?;
            (similarity > minimum_similarity
                && after_best[after].map(|(candidate, _)| candidate) == Some(before))
            .then_some(OrderedMatch { before, after })
        })
        .collect()
}

fn unique_best(values: impl Iterator<Item = (usize, u64)>) -> Option<(usize, u64)> {
    let mut best = None;
    let mut tied = false;
    for (index, score) in values.filter(|(_, score)| *score > 0) {
        let Some((_, best_score)) = best else {
            best = Some((index, score));
            continue;
        };
        if score > best_score {
            best = Some((index, score));
            tied = false;
        } else if score == best_score {
            tied = true;
        }
    }
    (!tied).then_some(best).flatten()
}

fn link_unit_indices(
    before: usize,
    after: usize,
    before_match: &mut [Option<usize>],
    after_match: &mut [Option<usize>],
) {
    debug_assert!(before_match[before].is_none());
    debug_assert!(after_match[after].is_none());
    before_match[before] = Some(after);
    after_match[after] = Some(before);
}

fn stable_unit_matches(
    before_match: &[Option<usize>],
    before: &[UnitRecord<'_>],
    after: &[UnitRecord<'_>],
) -> Vec<bool> {
    let matched_before = before_match
        .iter()
        .enumerate()
        .filter_map(|(before_index, after_index)| {
            let after_index = (*after_index)?;
            if before[before_index].decoration_owner.is_some() {
                return None;
            }
            let comparison = ComparisonStrategy::reconcile(
                before[before_index].comparison,
                after[after_index].comparison,
            );
            comparison
                .tracks_movement()
                .then_some((before_index, after_index))
        })
        .collect::<Vec<_>>();
    let after_order = matched_before
        .iter()
        .map(|(_, after)| *after)
        .collect::<Vec<_>>();
    let members = increasing_subsequence_members(&after_order);
    let mut stable = vec![false; before.len()];
    for ((before_index, _), member) in matched_before.into_iter().zip(members) {
        stable[before_index] = member;
    }

    // Excluding non-structural units from the order comparison so they cannot imply syntax moves.
    for (before_index, after_index) in before_match.iter().enumerate() {
        let Some(after_index) = *after_index else {
            continue;
        };
        let comparison = ComparisonStrategy::reconcile(
            before[before_index].comparison,
            after[after_index].comparison,
        );
        if !comparison.tracks_movement() {
            stable[before_index] = true;
        }
    }

    // Keeping the paired file root fixed; it provides the frame for child movement.
    if let Some(before_root) = before.iter().position(|unit| unit.id.index() == 0)
        && let Some(after_root) = before_match[before_root]
        && after[after_root].id.index() == 0
    {
        stable[before_root] = true;
    }

    let before_by_id = before
        .iter()
        .enumerate()
        .map(|(index, unit)| (unit.id, index))
        .collect::<HashMap<_, _>>();
    for (before_index, unit) in before.iter().enumerate() {
        let Some(owner) = unit.decoration_owner else {
            continue;
        };
        let Some(owner) = before_by_id.get(&owner).copied() else {
            // Root-owned documentation shares the fixed file frame even without a root review unit.
            if owner.index() == 0 && before_match[before_index].is_some() {
                stable[before_index] = true;
            }
            continue;
        };
        if before_match[before_index].is_some() {
            stable[before_index] = stable[owner];
        }
    }
    stable
}

/// Select noncrossing unit matches as boundaries for alignment and script order.
fn ordered_unit_anchors(before_match: &[Option<usize>], before: &[UnitRecord<'_>]) -> Vec<bool> {
    let matched = before_match
        .iter()
        .enumerate()
        .filter_map(|(before_index, after)| {
            let after = (*after)?;
            before[before_index]
                .decoration_owner
                .is_none()
                .then_some((before_index, after))
        })
        .collect::<Vec<_>>();
    let after_order = matched.iter().map(|(_, after)| *after).collect::<Vec<_>>();
    let members = increasing_subsequence_members(&after_order);
    let mut anchors = vec![false; before_match.len()];
    for ((before, _), member) in matched.into_iter().zip(members) {
        anchors[before] = member;
    }
    anchors
}

fn ordered_script_anchors(
    before_match: &[Option<usize>],
    before: &[UnitRecord<'_>],
    stable: &[bool],
) -> Vec<bool> {
    let mut anchors = ordered_unit_anchors(before_match, before);
    // Fitting stable decorations between established anchors so they cannot change the
    // order chosen for their owners.
    for before_index in 0..before_match.len() {
        let Some(after_index) = before_match[before_index] else {
            continue;
        };
        if before[before_index].decoration_owner.is_none() || !stable[before_index] {
            continue;
        }
        let previous = anchors[..before_index]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, anchor)| anchor.then_some(before_match[index]).flatten());
        let next = anchors[before_index + 1..]
            .iter()
            .enumerate()
            .find_map(|(offset, anchor)| {
                anchor
                    .then_some(before_match[before_index + 1 + offset])
                    .flatten()
            });
        if previous.is_none_or(|previous| previous < after_index)
            && next.is_none_or(|next| after_index < next)
        {
            anchors[before_index] = true;
        }
    }
    anchors
}

struct TreeDiffBuilder<'input, 'before, 'after> {
    pair: &'input SyntaxPair<'before, 'after>,
    before_units: &'input [UnitRecord<'before>],
    after_units: &'input [UnitRecord<'after>],
    before_match: &'input [Option<usize>],
    after_match: &'input [Option<usize>],
    stable: &'input [bool],
    before_fingerprints: &'input [NodeFingerprints],
    after_fingerprints: &'input [NodeFingerprints],
    before_subtree_sizes: &'input [usize],
    before_scope: Vec<Option<usize>>,
    after_scope: Vec<Option<usize>>,
    tree: TreeDiff,
}

impl TreeDiffBuilder<'_, '_, '_> {
    fn unit_script(&mut self) -> Vec<UnitEdit> {
        let script_anchors =
            ordered_script_anchors(self.before_match, self.before_units, self.stable);
        let mut anchors = self
            .before_match
            .iter()
            .enumerate()
            .filter_map(|(before, after)| {
                let after = (*after)?;
                script_anchors[before].then_some((before, after))
            })
            .collect::<Vec<_>>();
        anchors.push((self.before_units.len(), self.after_units.len()));

        let mut edits = Vec::new();
        let mut before_start = 0;
        let mut after_start = 0;
        for (before_anchor, after_anchor) in anchors {
            for before_index in before_start..before_anchor {
                if self.before_match[before_index].is_none() {
                    edits.push(UnitEdit::Removed {
                        before: self.before_units[before_index].id,
                    });
                }
            }
            for after_index in after_start..after_anchor {
                let Some(before_index) = self.after_match[after_index] else {
                    edits.push(UnitEdit::Added {
                        after: self.after_units[after_index].id,
                    });
                    continue;
                };
                self.push_matched_unit(&mut edits, before_index, after_index);
            }

            if before_anchor < self.before_units.len() && after_anchor < self.after_units.len() {
                self.push_matched_unit(&mut edits, before_anchor, after_anchor);
            }
            before_start = before_anchor.saturating_add(1);
            after_start = after_anchor.saturating_add(1);
        }
        edits
    }

    fn push_matched_unit(
        &mut self,
        edits: &mut Vec<UnitEdit>,
        before_index: usize,
        after_index: usize,
    ) {
        let before = self.before_units[before_index].id;
        let after = self.after_units[after_index].id;
        let before_fingerprint = self.before_units[before_index].fingerprint;
        let after_fingerprint = self.after_units[after_index].fingerprint;
        let before_node = self.pair.before.node(before);
        let after_node = self.pair.after.node(after);
        let before_source = self
            .pair
            .before
            .source
            .slice(before_node.bytes.clone())
            .expect("tree node source geometry remains valid");
        let after_source = self
            .pair
            .after
            .source
            .slice(after_node.bytes.clone())
            .expect("tree node source geometry remains valid");
        let relation = if before_source == after_source {
            ContentRelation::SourceEqual
        } else if before_fingerprint.full == after_fingerprint.full {
            ContentRelation::FullEqual
        } else if before_fingerprint.payload == after_fingerprint.payload {
            ContentRelation::PayloadEqual
        } else {
            ContentRelation::Modified
        };
        let placement = if self.stable[before_index] {
            Placement::Stable
        } else {
            Placement::Reordered
        };
        let leaf_start = self.tree.leaves.links.len();
        let composite_start = self.tree.composites.len();
        self.link_unit_contents(before, after, placement);
        let comparison = ComparisonStrategy::reconcile(
            self.before_units[before_index].comparison,
            self.after_units[after_index].comparison,
        );
        let role = SourceRole::reconcile(
            self.before_units[before_index].role,
            self.after_units[after_index].role,
        );
        edits.push(UnitEdit::Matched(MatchedUnit {
            before,
            after,
            comparison,
            role,
            relation,
            placement,
            leaf_links: leaf_start..self.tree.leaves.links.len(),
            composites: composite_start..self.tree.composites.len(),
        }));
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum DelimiterOwnerKey {
    Absent,
    Composite(NodeId),
    Leaf(FingerprintEdge),
    UnmatchedBefore,
    UnmatchedAfter,
}

impl DelimiterOwnerKey {
    /// Keep delimiters tied to atomic owners; composite owners may acquire or lose wrappers.
    fn for_global_match(self) -> Self {
        match self {
            Self::Leaf(_) => self,
            Self::Absent | Self::Composite(_) | Self::UnmatchedBefore | Self::UnmatchedAfter => {
                Self::Absent
            }
        }
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum ParentSlot {
    Unit,
    Node(NodeId),
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct ContextualLeafKey {
    leaf: FingerprintEdge,
    parent: ParentSlot,
    trailing_delimiter_owner: DelimiterOwnerKey,
    decorated: bool,
    decoration_owner: Option<NodeId>,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct LeafShape {
    kind: SyntaxKind,
    slot: ChildSlot,
    channel: ContentChannel,
    named: bool,
    extra: bool,
    missing: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct ContextLink {
    after: NodeId,
    placement: Placement,
    reparenting: Option<Reparenting>,
}

/// Parent pairs established before matching leaves, indexed in both directions.
struct ContextLinks {
    before: HashMap<NodeId, ContextLink>,
    after_to_before: HashMap<NodeId, NodeId>,
}

struct UnitContext<'input, 'before, 'after> {
    pair: &'input SyntaxPair<'before, 'after>,
    parents: &'input ContextLinks,
    before_unit: NodeId,
    after_unit: NodeId,
}

impl UnitContext<'_, '_, '_> {
    fn parents_are_linked(&self, before: NodeId, after: NodeId) -> bool {
        let before_node = self.pair.before.node(before);
        let after_node = self.pair.after.node(after);
        if before_node.slot != after_node.slot {
            return false;
        }
        if before == self.before_unit && after == self.after_unit {
            return true;
        }

        let (Some(before_parent), Some(after_parent)) = (before_node.parent, after_node.parent)
        else {
            return before_node.parent.is_none() && after_node.parent.is_none();
        };
        self.parents
            .before
            .get(&before_parent)
            .is_some_and(|link| link.after == after_parent)
    }

    fn desired_after_parent(&self, before: NodeId) -> Option<ParentSlot> {
        if before == self.before_unit {
            return Some(ParentSlot::Unit);
        }
        let parent = self.pair.before.node(before).parent?;
        self.parents
            .before
            .get(&parent)
            .map(|link| ParentSlot::Node(link.after))
    }

    fn after_parent(&self, after: NodeId) -> Option<ParentSlot> {
        if after == self.after_unit {
            return Some(ParentSlot::Unit);
        }
        self.pair.after.node(after).parent.map(ParentSlot::Node)
    }

    fn desired_after_trailing_delimiter_owner(
        &self,
        before: NodeId,
        leaf_keys: &HashMap<NodeId, FingerprintEdge>,
    ) -> DelimiterOwnerKey {
        let Some(owner) = self.pair.before.delimiter_owner(before) else {
            return DelimiterOwnerKey::Absent;
        };
        if self.pair.before.node(owner).leaf.is_some() {
            return leaf_keys
                .get(&owner)
                .copied()
                .map(DelimiterOwnerKey::Leaf)
                .unwrap_or(DelimiterOwnerKey::UnmatchedBefore);
        }
        self.parents
            .before
            .get(&owner)
            .map(|link| DelimiterOwnerKey::Composite(link.after))
            .unwrap_or(DelimiterOwnerKey::UnmatchedBefore)
    }

    fn after_trailing_delimiter_owner(
        &self,
        after: NodeId,
        leaf_keys: &HashMap<NodeId, FingerprintEdge>,
    ) -> DelimiterOwnerKey {
        let Some(owner) = self.pair.after.delimiter_owner(after) else {
            return DelimiterOwnerKey::Absent;
        };
        if self.pair.after.node(owner).leaf.is_some() {
            return leaf_keys
                .get(&owner)
                .copied()
                .map(DelimiterOwnerKey::Leaf)
                .unwrap_or(DelimiterOwnerKey::UnmatchedAfter);
        }
        DelimiterOwnerKey::Composite(owner)
    }

    fn parent_correspondence(&self, before: NodeId, after: NodeId) -> Option<ParentCorrespondence> {
        if self.parents_are_linked(before, after) {
            return Some(ParentCorrespondence::Direct);
        }
        self.enclosing_wrapper(before, after)
            .map(ParentCorrespondence::Reparented)
    }

    fn enclosing_wrapper(&self, before: NodeId, after: NodeId) -> Option<Reparenting> {
        self.parents
            .before
            .get(&before)
            .and_then(|link| link.reparenting)
            .or_else(|| {
                let parent = self.pair.before.node(before).parent?;
                self.parents
                    .before
                    .get(&parent)
                    .and_then(|link| link.reparenting)
            })
            .or_else(|| unique_containment_reparenting(self.pair, self.parents, before, after))
    }

    fn desired_after_decoration_owner(&self, before: NodeId) -> Option<NodeId> {
        let owner = self.pair.before.node(before).decoration_owner?;
        if owner == self.before_unit {
            return Some(self.after_unit);
        }
        self.parents.before.get(&owner).map(|link| link.after)
    }

    fn decoration_placement(&self, before: NodeId) -> Option<Placement> {
        let owner = self.pair.before.node(before).decoration_owner?;
        self.parents.before.get(&owner).map(|link| link.placement)
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct ContextShape {
    kind: SyntaxKind,
    slot: ChildSlot,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct ContextIdentity<'source> {
    kind: SyntaxKind,
    slot: ChildSlot,
    identity: &'source str,
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct AnonymousContextKey<'source> {
    shape: ContextShape,
    identities: Vec<ContextIdentity<'source>>,
}

impl TreeDiffBuilder<'_, '_, '_> {
    fn link_unit_contents(
        &mut self,
        before_unit: NodeId,
        after_unit: NodeId,
        placement: Placement,
    ) {
        let pair = self.pair;
        let before_composites = descendant_composites(&pair.before, before_unit);
        let after_composites = descendant_composites(&pair.after, after_unit);
        let parents = contextual_links(
            pair,
            before_unit,
            after_unit,
            placement,
            self.before_fingerprints,
            self.after_fingerprints,
        );
        let context = UnitContext {
            pair,
            parents: &parents,
            before_unit,
            after_unit,
        };
        let exact_composites = exact_composite_matches(
            &context,
            &before_composites,
            &after_composites,
            self.before_fingerprints,
            self.after_fingerprints,
        );

        // Matching largest exact subtrees first; pairing their repeated descendants separately
        // could give those descendants partners outside the subtree.
        let mut cover_candidates = exact_composites.clone();
        cover_candidates.sort_by(|left, right| {
            self.before_subtree_sizes[before_composites[right.before].index()]
                .cmp(&self.before_subtree_sizes[before_composites[left.before].index()])
                .then_with(|| left.before.cmp(&right.before))
                .then_with(|| left.after.cmp(&right.after))
        });
        let mut covered_before = HashSet::new();
        let mut covered_after = HashSet::new();
        let mut maximal = Vec::new();
        for edge in cover_candidates {
            let before = before_composites[edge.before];
            let after = after_composites[edge.after];
            if covered_before.contains(&before) || covered_after.contains(&after) {
                continue;
            }
            covered_before.extend(std::iter::once(before).chain(pair.before.descendants(before)));
            covered_after.extend(std::iter::once(after).chain(pair.after.descendants(after)));
            maximal.push(edge);
        }
        maximal.sort_by_key(|edge| edge.before);
        let mut exact_cover = HashMap::new();
        let mut exact_cover_root = HashMap::new();
        for edge in &maximal {
            let before = before_composites[edge.before];
            let after = after_composites[edge.after];
            for (covered_before, covered_after) in exact_subtree_nodes(pair, before, after) {
                let previous = exact_cover.insert(covered_before, covered_after);
                debug_assert!(previous.is_none(), "exact-cover node linked twice");
                exact_cover_root.insert(covered_before, before);
            }
        }
        let exact_cover_wrapper = maximal
            .iter()
            .map(|edge| {
                let before = before_composites[edge.before];
                let after = after_composites[edge.after];
                (before, context.enclosing_wrapper(before, after))
            })
            .collect::<HashMap<_, _>>();
        let exact_cover_after = exact_cover
            .iter()
            .map(|(before, after)| (*after, *before))
            .collect::<HashMap<_, _>>();
        let mut scopes = parents
            .before
            .iter()
            .filter_map(|(before, link)| {
                if *before != before_unit && !pair.before.node(*before).is_scope_boundary() {
                    return None;
                }
                if exact_cover.contains_key(before) || exact_cover_after.contains_key(&link.after) {
                    return None;
                }
                let wrapper = unique_containment_reparenting(pair, &parents, *before, link.after);
                let parent = wrapper
                    .map(ParentCorrespondence::Reparented)
                    .unwrap_or(ParentCorrespondence::Direct);
                Some(ScopeLink {
                    before: *before,
                    after: link.after,
                    placement: link.placement,
                    parent,
                })
            })
            .collect::<Vec<_>>();
        scopes.sort_unstable_by_key(|link| link.before);
        let scope_start = self.tree.scopes.len();
        for scope in scopes {
            if self.push_scope_link(scope) {
                continue;
            }
            for link in self.tree.scopes.drain(scope_start..) {
                self.before_scope[link.before.index()] = None;
                self.after_scope[link.after.index()] = None;
            }
            return;
        }

        // Discarding conflicting duplicate matches before they can affect movement.
        let exact_composites = exact_composites
            .into_iter()
            .filter(|edge| {
                let before = before_composites[edge.before];
                let after = after_composites[edge.after];
                exact_cover.get(&before).copied() == Some(after)
            })
            .collect::<Vec<_>>();
        let placements = match_placements(&exact_composites);
        let exact_links = exact_composites
            .into_iter()
            .zip(placements)
            .filter_map(|(edge, placement)| {
                let before = before_composites[edge.before];
                let after = after_composites[edge.after];
                let placement = context.decoration_placement(before).unwrap_or(placement);
                let parents_are_in_exact_cover = pair
                    .before
                    .node(before)
                    .parent
                    .zip(pair.after.node(after).parent)
                    .is_some_and(|(before, after)| {
                        exact_cover.get(&before).copied() == Some(after)
                    });
                let wrapper = exact_cover_root
                    .get(&before)
                    .and_then(|root| exact_cover_wrapper.get(root))
                    .copied()
                    .flatten();
                let parent = if parents_are_in_exact_cover {
                    ParentCorrespondence::Direct
                } else {
                    context.parent_correspondence(before, after)?
                };
                Some(NodeLink {
                    before,
                    after,
                    parent,
                    wrapper,
                    placement,
                })
            })
            .collect::<Vec<_>>();
        let exact_roots = exact_links
            .iter()
            .map(|link| (link.before, *link))
            .collect::<HashMap<_, _>>();
        // Using all surviving composite pairs to decide movement; considering only the
        // outermost roots can mistake crossings in repeated syntax for a moved definition.
        for edge in maximal {
            let before = before_composites[edge.before];
            let after = after_composites[edge.after];
            let link = exact_roots
                .get(&before)
                .expect("every maximal exact root survives its own cover");
            self.link_exact_subtree(before, after, link.placement, link.parent, link.wrapper);
        }
        self.tree.composites.extend(exact_links);

        let before_leaves = descendant_leaves(&pair.before, before_unit)
            .into_iter()
            .filter(|id| self.tree.leaves.before[id.index()].is_none())
            .collect::<Vec<_>>();
        let after_leaves = descendant_leaves(&pair.after, after_unit)
            .into_iter()
            .filter(|id| self.tree.leaves.after[id.index()].is_none())
            .collect::<Vec<_>>();
        let exact_leaves = exact_leaf_matches(
            &context,
            &before_leaves,
            &after_leaves,
            self.before_fingerprints,
            self.after_fingerprints,
        );
        let placements = match_placements(&exact_leaves);
        for (edge, placement) in exact_leaves.into_iter().zip(placements) {
            let before = before_leaves[edge.before];
            let after = after_leaves[edge.after];
            let placement = context.decoration_placement(before).unwrap_or(placement);
            let Some(parent) = context.parent_correspondence(before, after) else {
                continue;
            };
            self.push_leaf_link(LeafLink {
                before,
                after,
                relation: LeafRelation::Exact,
                placement,
                parent,
                wrapper: context.enclosing_wrapper(before, after),
            });
        }

        let before_remaining = before_leaves
            .into_iter()
            .filter(|id| self.tree.leaves.before[id.index()].is_none())
            .collect::<Vec<_>>();
        let after_remaining = after_leaves
            .into_iter()
            .filter(|id| self.tree.leaves.after[id.index()].is_none())
            .collect::<Vec<_>>();
        let before_shapes = before_remaining
            .iter()
            .map(|id| leaf_shape(&pair.before, *id))
            .collect::<Vec<_>>();
        let after_shapes = after_remaining
            .iter()
            .map(|id| leaf_shape(&pair.after, *id))
            .collect::<Vec<_>>();
        let decorated = decorated_leaf_matches(
            &context,
            &before_remaining,
            &after_remaining,
            &before_shapes,
            &after_shapes,
        );
        let mut before_claimed = vec![false; before_remaining.len()];
        let mut after_claimed = vec![false; after_remaining.len()];
        for edge in decorated {
            before_claimed[edge.before] = true;
            after_claimed[edge.after] = true;
            let before = before_remaining[edge.before];
            let after = after_remaining[edge.after];
            let placement = context
                .decoration_placement(before)
                .expect("a decoration match requires a linked semantic owner");
            let Some(parent) = context.parent_correspondence(before, after) else {
                continue;
            };
            self.push_leaf_link(LeafLink {
                before,
                after,
                relation: LeafRelation::Modified,
                placement,
                parent,
                wrapper: context.enclosing_wrapper(before, after),
            });
        }

        let plain_before = (0..before_remaining.len())
            .filter(|index| {
                !before_claimed[*index]
                    && pair
                        .before
                        .node(before_remaining[*index])
                        .decoration_owner
                        .is_none()
            })
            .collect::<Vec<_>>();
        let plain_after = (0..after_remaining.len())
            .filter(|index| {
                !after_claimed[*index]
                    && pair
                        .after
                        .node(after_remaining[*index])
                        .decoration_owner
                        .is_none()
            })
            .collect::<Vec<_>>();
        // Requiring matched parents for shape-only edits; matching syntax roles cannot
        // establish that a leaf survived a wrapper change.
        let mut contextual_after = HashMap::<(ParentSlot, LeafShape), VecDeque<usize>>::new();
        for after_index in plain_after {
            let after = after_remaining[after_index];
            let Some(parent) = context.after_parent(after) else {
                continue;
            };
            contextual_after
                .entry((parent, after_shapes[after_index]))
                .or_default()
                .push_back(after_index);
        }
        for before_index in plain_before {
            let before = before_remaining[before_index];
            let Some(parent) = context.desired_after_parent(before) else {
                continue;
            };
            let Some(after_index) = contextual_after
                .get_mut(&(parent, before_shapes[before_index]))
                .and_then(VecDeque::pop_front)
            else {
                continue;
            };
            let after = after_remaining[after_index];
            self.push_leaf_link(LeafLink {
                before,
                after,
                relation: LeafRelation::Modified,
                placement: Placement::Stable,
                parent: ParentCorrespondence::Direct,
                wrapper: None,
            });
        }
    }
}

fn exact_composite_matches(
    context: &UnitContext<'_, '_, '_>,
    before: &[NodeId],
    after: &[NodeId],
    before_fingerprints: &[NodeFingerprints],
    after_fingerprints: &[NodeFingerprints],
) -> Vec<OrderedMatch> {
    let after_indices = after
        .iter()
        .copied()
        .enumerate()
        .map(|(index, id)| (id, index))
        .collect::<HashMap<_, _>>();
    before
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(before, before_id)| {
            let link = context.parents.before.get(&before_id)?;
            let after = *after_indices.get(&link.after)?;
            (before_fingerprints[before_id.index()].full
                == after_fingerprints[link.after.index()].full)
                .then_some(OrderedMatch { before, after })
        })
        .collect()
}

fn exact_leaf_matches(
    context: &UnitContext<'_, '_, '_>,
    before: &[NodeId],
    after: &[NodeId],
    before_fingerprints: &[NodeFingerprints],
    after_fingerprints: &[NodeFingerprints],
) -> Vec<OrderedMatch> {
    let mut before_match = vec![None; before.len()];
    let mut after_match = vec![None; after.len()];
    let before_keys = before
        .iter()
        .copied()
        .map(|id| {
            (
                id,
                FingerprintEdge {
                    slot: context.pair.before.node(id).slot,
                    fingerprint: before_fingerprints[id.index()].full,
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let after_keys = after
        .iter()
        .copied()
        .map(|id| {
            (
                id,
                FingerprintEdge {
                    slot: context.pair.after.node(id).slot,
                    fingerprint: after_fingerprints[id.index()].full,
                },
            )
        })
        .collect::<HashMap<_, _>>();

    // Claiming matches under the same parents first so wrapper recovery cannot steal them.
    let mut contextual_after = HashMap::<ContextualLeafKey, VecDeque<usize>>::new();
    for (after_index, after_id) in after.iter().copied().enumerate() {
        let Some(parent) = context.after_parent(after_id) else {
            continue;
        };
        contextual_after
            .entry(ContextualLeafKey {
                leaf: after_keys[&after_id],
                parent,
                trailing_delimiter_owner: context
                    .after_trailing_delimiter_owner(after_id, &after_keys),
                decorated: context.pair.after.node(after_id).decoration_owner.is_some(),
                decoration_owner: context.pair.after.node(after_id).decoration_owner,
            })
            .or_default()
            .push_back(after_index);
    }
    for (before_index, before_id) in before.iter().copied().enumerate() {
        let before_key = before_keys[&before_id];
        let parent = context.desired_after_parent(before_id);
        let after_index = parent.and_then(|parent| {
            contextual_after
                .get_mut(&ContextualLeafKey {
                    leaf: before_key,
                    parent,
                    trailing_delimiter_owner: context
                        .desired_after_trailing_delimiter_owner(before_id, &before_keys),
                    decorated: context
                        .pair
                        .before
                        .node(before_id)
                        .decoration_owner
                        .is_some(),
                    decoration_owner: context.desired_after_decoration_owner(before_id),
                })?
                .pop_front()
        });
        let Some(after_index) = after_index else {
            continue;
        };
        before_match[before_index] = Some(after_index);
        after_match[after_index] = Some(before_index);
    }

    let wrapped_before = (0..before.len())
        .filter(|index| {
            before_match[*index].is_none()
                && context
                    .pair
                    .before
                    .node(before[*index])
                    .decoration_owner
                    .is_none()
        })
        .collect::<Vec<_>>();
    let wrapped_after = (0..after.len())
        .filter(|index| {
            after_match[*index].is_none()
                && context
                    .pair
                    .after
                    .node(after[*index])
                    .decoration_owner
                    .is_none()
        })
        .collect::<Vec<_>>();
    let mut wrapped_before_groups = HashMap::<FingerprintId, Vec<usize>>::new();
    for before_index in wrapped_before {
        wrapped_before_groups
            .entry(before_keys[&before[before_index]].fingerprint)
            .or_default()
            .push(before_index);
    }
    let mut wrapped_after_groups = HashMap::<FingerprintId, Vec<usize>>::new();
    for after_index in wrapped_after {
        wrapped_after_groups
            .entry(after_keys[&after[after_index]].fingerprint)
            .or_default()
            .push(after_index);
    }
    for (fingerprint, before_group) in wrapped_before_groups {
        let Some(after_group) = wrapped_after_groups.get(&fingerprint) else {
            continue;
        };
        for edge in
            reciprocal_unique_matches(before_group.len(), after_group.len(), 0, |left, right| {
                let reparenting = unique_containment_reparenting(
                    context.pair,
                    context.parents,
                    before[before_group[left]],
                    after[after_group[right]],
                );
                u64::from(reparenting.is_some())
            })
        {
            let before_index = before_group[edge.before];
            let after_index = after_group[edge.after];
            before_match[before_index] = Some(after_index);
            after_match[after_index] = Some(before_index);
        }
    }

    let remaining_before = (0..before.len())
        .filter(|index| {
            before_match[*index].is_none()
                && context
                    .pair
                    .before
                    .node(before[*index])
                    .decoration_owner
                    .is_none()
        })
        .collect::<Vec<_>>();
    let remaining_after = (0..after.len())
        .filter(|index| {
            after_match[*index].is_none()
                && context
                    .pair
                    .after
                    .node(after[*index])
                    .decoration_owner
                    .is_none()
        })
        .collect::<Vec<_>>();
    let before_values = remaining_before
        .iter()
        .map(|index| {
            (
                context.desired_after_parent(before[*index]),
                before_keys[&before[*index]],
                context
                    .desired_after_trailing_delimiter_owner(before[*index], &before_keys)
                    .for_global_match(),
            )
        })
        .collect::<Vec<_>>();
    let after_values = remaining_after
        .iter()
        .map(|index| {
            (
                context.after_parent(after[*index]),
                after_keys[&after[*index]],
                context
                    .after_trailing_delimiter_owner(after[*index], &after_keys)
                    .for_global_match(),
            )
        })
        .collect::<Vec<_>>();
    for edge in unordered_matches(&before_values, &after_values) {
        let before_index = remaining_before[edge.before];
        let after_index = remaining_after[edge.after];
        before_match[before_index] = Some(after_index);
        after_match[after_index] = Some(before_index);
    }

    before_match
        .into_iter()
        .enumerate()
        .filter_map(|(before, after)| after.map(|after| OrderedMatch { before, after }))
        .collect()
}

/// Pair edited documentation only when its owner and immediate parent both match.
fn decorated_leaf_matches(
    context: &UnitContext<'_, '_, '_>,
    before: &[NodeId],
    after: &[NodeId],
    before_shapes: &[LeafShape],
    after_shapes: &[LeafShape],
) -> Vec<OrderedMatch> {
    let mut after_groups = HashMap::<(NodeId, ParentSlot, LeafShape), VecDeque<usize>>::new();
    for (index, id) in after.iter().copied().enumerate() {
        let Some(owner) = context.pair.after.node(id).decoration_owner else {
            continue;
        };
        let Some(parent) = context.after_parent(id) else {
            continue;
        };
        after_groups
            .entry((owner, parent, after_shapes[index]))
            .or_default()
            .push_back(index);
    }

    before
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(before, id)| {
            let owner = context.desired_after_decoration_owner(id)?;
            let parent = context.desired_after_parent(id)?;
            let after = after_groups
                .get_mut(&(owner, parent, before_shapes[before]))?
                .pop_front()?;
            Some(OrderedMatch { before, after })
        })
        .collect()
}

fn descendant_composites(tree: &SyntaxTree<'_>, root: NodeId) -> Vec<NodeId> {
    tree.descendants(root)
        .filter(|id| {
            let node = tree.node(*id);
            node.named && node.leaf.is_none()
        })
        .collect()
}

fn descendant_leaves(tree: &SyntaxTree<'_>, root: NodeId) -> Vec<NodeId> {
    let root_node = tree.node(root);
    if root_node.leaf.is_some() && !is_layout_leaf(tree, root) {
        return vec![root];
    }
    tree.descendants(root)
        .filter(|id| tree.node(*id).leaf.is_some() && !is_layout_leaf(tree, *id))
        .collect()
}

fn contextual_links(
    pair: &SyntaxPair<'_, '_>,
    before_unit: NodeId,
    after_unit: NodeId,
    unit_placement: Placement,
    before_fingerprints: &[NodeFingerprints],
    after_fingerprints: &[NodeFingerprints],
) -> ContextLinks {
    let mut links = ContextLinks {
        before: HashMap::new(),
        after_to_before: HashMap::new(),
    };
    if !link_context(before_unit, after_unit, unit_placement, None, &mut links) {
        return links;
    }
    let link_children = |before_parent,
                         after_parent,
                         allow_renames,
                         links: &mut ContextLinks,
                         pending: &mut VecDeque<(NodeId, NodeId)>| {
        let before_children = direct_composites(&pair.before, before_parent);
        let after_children = direct_composites(&pair.after, after_parent);
        let pairs = contextual_child_matches(
            pair,
            links,
            &before_children,
            &after_children,
            before_fingerprints,
            after_fingerprints,
            allow_renames,
        );
        let placements = contextual_match_placements(pair, &before_children, &pairs, links);
        let inherited_reparenting = links
            .before
            .get(&before_parent)
            .and_then(|link| link.reparenting);
        let mut linked = false;
        for (edge, placement) in pairs.into_iter().zip(placements) {
            let before = before_children[edge.before];
            let after = after_children[edge.after];
            if link_context(before, after, placement, inherited_reparenting, links) {
                pending.push_back((before, after));
                linked = true;
            }
        }
        linked
    };
    let mut pending = VecDeque::from([(before_unit, after_unit)]);
    // Deferring rename guesses until wrapper recovery has claimed surviving inner nodes.
    let mut deferred_renames = Vec::new();
    loop {
        while let Some((before_parent, after_parent)) = pending.pop_front() {
            link_children(before_parent, after_parent, false, &mut links, &mut pending);
            deferred_renames.push((before_parent, after_parent));
        }

        let reparented = confident_reparented_context_matches(
            pair,
            before_unit,
            after_unit,
            &links,
            before_fingerprints,
            after_fingerprints,
        );
        if !reparented.is_empty() {
            for (before, after, placement, reparenting) in reparented {
                if link_context(before, after, placement, Some(reparenting), &mut links) {
                    pending.push_back((before, after));
                }
            }
            continue;
        }

        let mut linked_rename = false;
        for (before_parent, after_parent) in std::mem::take(&mut deferred_renames) {
            linked_rename |=
                link_children(before_parent, after_parent, true, &mut links, &mut pending);
        }
        if !linked_rename {
            break;
        }
    }
    retain_valid_context_links(pair, before_unit, after_unit, &mut links);
    links
}

/// Revalidate parent matches against the final graph and refresh their wrapper evidence.
fn retain_valid_context_links(
    pair: &SyntaxPair<'_, '_>,
    before_unit: NodeId,
    after_unit: NodeId,
    links: &mut ContextLinks,
) {
    loop {
        let context = UnitContext {
            pair,
            parents: links,
            before_unit,
            after_unit,
        };
        let invalid = links
            .before
            .iter()
            .filter_map(|(before, link)| {
                (!context.parents_are_linked(*before, link.after)
                    && unique_containment_reparenting(pair, links, *before, link.after).is_none())
                .then_some((*before, link.after))
            })
            .collect::<Vec<_>>();
        if invalid.is_empty() {
            break;
        }
        for (before, after) in invalid {
            links.before.remove(&before);
            links.after_to_before.remove(&after);
        }
    }

    let mut retained = links
        .before
        .iter()
        .map(|(before, link)| (*before, link.after))
        .collect::<Vec<_>>();
    retained.sort_unstable_by_key(|(before, _)| *before);
    for link in links.before.values_mut() {
        link.reparenting = None;
    }
    // Recomputing wrapper evidence from parents to children so no descendant inherits
    // a proof removed above.
    for (before, after) in retained {
        let context = UnitContext {
            pair,
            parents: links,
            before_unit,
            after_unit,
        };
        let reparenting = if context.parents_are_linked(before, after) {
            pair.before
                .node(before)
                .parent
                .and_then(|parent| links.before.get(&parent).and_then(|link| link.reparenting))
        } else {
            unique_containment_reparenting(pair, links, before, after)
        };
        links
            .before
            .get_mut(&before)
            .expect("retained context remains linked")
            .reparenting = reparenting;
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum ReparentedContextKey<'source> {
    Identified(SyntaxKind, &'source str),
    Exact(SyntaxKind, FingerprintId),
}

/// Recover subtrees carried through a unique wrapper path.
/// Anonymous nodes need exact content; nodes with unchanged names may also use shared payload.
/// Outermost matches win so descendants are paired inside their recovered context.
fn confident_reparented_context_matches(
    pair: &SyntaxPair<'_, '_>,
    before_unit: NodeId,
    after_unit: NodeId,
    links: &ContextLinks,
    before_fingerprints: &[NodeFingerprints],
    after_fingerprints: &[NodeFingerprints],
) -> Vec<(NodeId, NodeId, Placement, Reparenting)> {
    let before = descendant_composites(&pair.before, before_unit)
        .into_iter()
        .filter(|id| {
            !links.before.contains_key(id) && pair.before.node(*id).decoration_owner.is_none()
        })
        .collect::<Vec<_>>();
    let after = descendant_composites(&pair.after, after_unit)
        .into_iter()
        .filter(|id| {
            !links.after_to_before.contains_key(id)
                && pair.after.node(*id).decoration_owner.is_none()
        })
        .collect::<Vec<_>>();
    if before.is_empty() || after.is_empty() {
        return Vec::new();
    }

    let before_payload = before
        .iter()
        .map(|id| {
            pair.before.identity_text(*id).map_or_else(Vec::new, |_| {
                meaningful_payload_fingerprints(&pair.before, before_fingerprints, *id)
            })
        })
        .collect::<Vec<_>>();
    let after_payload = after
        .iter()
        .map(|id| {
            pair.after.identity_text(*id).map_or_else(Vec::new, |_| {
                meaningful_payload_fingerprints(&pair.after, after_fingerprints, *id)
            })
        })
        .collect::<Vec<_>>();
    let mut before_groups = HashMap::<ReparentedContextKey<'_>, Vec<usize>>::new();
    for (index, id) in before.iter().copied().enumerate() {
        let node = pair.before.node(id);
        let key = pair.before.identity_text(id).map_or_else(
            || ReparentedContextKey::Exact(node.kind, before_fingerprints[id.index()].full),
            |identity| ReparentedContextKey::Identified(node.kind, identity),
        );
        before_groups.entry(key).or_default().push(index);
    }
    let mut after_groups = HashMap::<ReparentedContextKey<'_>, Vec<usize>>::new();
    for (index, id) in after.iter().copied().enumerate() {
        let node = pair.after.node(id);
        let key = pair.after.identity_text(id).map_or_else(
            || ReparentedContextKey::Exact(node.kind, after_fingerprints[id.index()].full),
            |identity| ReparentedContextKey::Identified(node.kind, identity),
        );
        after_groups.entry(key).or_default().push(index);
    }
    let mut candidates = Vec::new();
    for (key, before_group) in before_groups {
        let Some(after_group) = after_groups.get(&key) else {
            continue;
        };
        candidates.extend(
            reciprocal_unique_matches(before_group.len(), after_group.len(), 0, |left, right| {
                let left = before_group[left];
                let right = after_group[right];
                let left_node = pair.before.node(before[left]);
                let right_node = pair.after.node(after[right]);
                let parents_already_linked =
                    left_node
                        .parent
                        .zip(right_node.parent)
                        .is_some_and(|(left, right)| {
                            links
                                .before
                                .get(&left)
                                .is_some_and(|link| link.after == right)
                        });
                if parents_already_linked
                    || unique_containment_reparenting(pair, links, before[left], after[right])
                        .is_none()
                {
                    return 0;
                }
                if before_fingerprints[before[left].index()].full
                    == after_fingerprints[after[right].index()].full
                {
                    return u64::MAX;
                }
                shared_fingerprint_count(&before_payload[left], &after_payload[right])
            })
            .into_iter()
            .map(|edge| OrderedMatch {
                before: before_group[edge.before],
                after: after_group[edge.after],
            }),
        );
    }
    candidates.sort_unstable_by_key(|edge| edge.before);

    let mut roots: Vec<OrderedMatch> = Vec::new();
    for candidate in candidates {
        let nested = roots.iter().any(|root| {
            pair.before
                .contains(before[root.before], before[candidate.before])
                || pair
                    .after
                    .contains(after[root.after], after[candidate.after])
        });
        if !nested {
            roots.push(candidate);
        }
    }
    let placements = match_placements(&roots);
    roots
        .into_iter()
        .zip(placements)
        .map(|(edge, placement)| {
            let before = before[edge.before];
            let after = after[edge.after];
            let reparenting = unique_containment_reparenting(pair, links, before, after)
                .expect("reparented context candidates carry a wrapper proof");
            (before, after, placement, reparenting)
        })
        .collect()
}

fn unique_containment_reparenting(
    pair: &SyntaxPair<'_, '_>,
    links: &ContextLinks,
    before: NodeId,
    after: NodeId,
) -> Option<Reparenting> {
    unique_containment_reparenting_with(
        pair,
        before,
        after,
        |candidate| links.before.get(&candidate).map(|link| link.after),
        |candidate| links.after_to_before.get(&candidate).copied(),
    )
}

/// Prove a wrap or unwrap through a single chain of unmatched parents.
/// The nearest matched ancestors must be partners. Along the chain, sibling branches
/// cannot contain matched owners or review boundaries. Only one revision may add
/// unmatched parents; replacing wrappers on both sides remains a structural edit.
fn unique_containment_reparenting_with(
    pair: &SyntaxPair<'_, '_>,
    before: NodeId,
    after: NodeId,
    before_link: impl Fn(NodeId) -> Option<NodeId>,
    after_link: impl Fn(NodeId) -> Option<NodeId>,
) -> Option<Reparenting> {
    let before_is_linked = |candidate| before_link(candidate).is_some();
    let after_is_linked = |candidate| after_link(candidate).is_some();
    let (before_anchor, before_reparented) =
        unique_containment_path(&pair.before, before, &before_is_linked, &before_is_linked)?;
    let (after_anchor, after_reparented) =
        unique_containment_path(&pair.after, after, &after_is_linked, &after_is_linked)?;
    if before_link(before_anchor) != Some(after_anchor)
        || after_link(after_anchor) != Some(before_anchor)
    {
        return None;
    }
    match (before_reparented, after_reparented) {
        (false, true) => Some(Reparenting::Wrapped),
        (true, false) => Some(Reparenting::Unwrapped),
        (false, false) | (true, true) => None,
    }
}

/// Find an ancestor anchor without crossing a competing owner or sealed wrapper.
/// The flag records whether the path includes unmatched parents.
fn unique_containment_path(
    tree: &SyntaxTree<'_>,
    candidate: NodeId,
    is_anchor: &impl Fn(NodeId) -> bool,
    is_fence: &impl Fn(NodeId) -> bool,
) -> Option<(NodeId, bool)> {
    let mut branch = candidate;
    let mut candidate = tree.node(candidate).parent?;
    let mut crossed_unmatched_parent = false;
    loop {
        // Stopping at the matched owner; its other children need not satisfy the wrapper proof.
        if is_anchor(candidate) && is_fence(candidate) {
            return Some((candidate, crossed_unmatched_parent));
        }

        let node = tree.node(candidate);
        if node.seals_wrappers()
            || node
                .children
                .iter()
                .copied()
                .any(|child| child != branch && subtree_contains_fence(tree, child, is_fence))
        {
            return None;
        }
        crossed_unmatched_parent = true;
        if is_anchor(candidate) {
            return Some((candidate, crossed_unmatched_parent));
        }
        branch = candidate;
        candidate = node.parent?;
    }
}

fn subtree_contains_fence(
    tree: &SyntaxTree<'_>,
    root: NodeId,
    is_fence: &impl Fn(NodeId) -> bool,
) -> bool {
    std::iter::once(root)
        .chain(tree.descendants(root))
        .any(|candidate| {
            let node = tree.node(candidate);
            is_fence(candidate)
                || node.seals_wrappers()
                || node.review.is_some()
                || node.decoration_owner.is_some()
        })
}

/// Check that exactly one descendant matches along an allowed wrapper path.
fn unique_contained_descendant_matches(
    tree: &SyntaxTree<'_>,
    outer: NodeId,
    is_fence: &impl Fn(NodeId) -> bool,
    mut matches: impl FnMut(NodeId) -> bool,
) -> bool {
    let is_outer = |candidate| candidate == outer;
    let mut retained = None;
    for candidate in tree.descendants(outer) {
        let node = tree.node(candidate);
        if !node.named || node.leaf.is_some() || !matches(candidate) {
            continue;
        }
        if unique_containment_path(tree, candidate, &is_outer, is_fence).is_none() {
            continue;
        }
        if retained.replace(candidate).is_some() {
            return false;
        }
    }
    retained.is_some()
}

/// Collect sorted payload evidence, excluding the context's name, comments, and delimiters.
fn meaningful_payload_fingerprints(
    tree: &SyntaxTree<'_>,
    fingerprints: &[NodeFingerprints],
    root: NodeId,
) -> Vec<FingerprintId> {
    let identity = tree.identity_text(root);
    let mut payload = descendant_leaves(tree, root)
        .into_iter()
        .filter(|id| {
            let node = tree.node(*id);
            let Some(leaf) = node.leaf else {
                return false;
            };
            if leaf.delimiter.is_some()
                || matches!(
                    leaf.channel,
                    ContentChannel::Comment | ContentChannel::Layout
                )
                || leaf.role != LeafRole::Payload
            {
                return false;
            }
            let text = tree.leaf_text(*id);
            text.is_some_and(|text| !text.trim().is_empty() && Some(text) != identity)
        })
        .map(|id| fingerprints[id.index()].full)
        .collect::<Vec<_>>();
    payload.sort_unstable();
    payload
}

fn shared_fingerprint_count(before: &[FingerprintId], after: &[FingerprintId]) -> u64 {
    let mut before_index = 0;
    let mut after_index = 0;
    let mut shared = 0;
    while before_index < before.len() && after_index < after.len() {
        match before[before_index].cmp(&after[after_index]) {
            std::cmp::Ordering::Less => before_index += 1,
            std::cmp::Ordering::Greater => after_index += 1,
            std::cmp::Ordering::Equal => {
                shared += 1;
                before_index += 1;
                after_index += 1;
            }
        }
    }
    shared
}

fn contextual_child_matches(
    pair: &SyntaxPair<'_, '_>,
    context: &ContextLinks,
    before: &[NodeId],
    after: &[NodeId],
    before_fingerprints: &[NodeFingerprints],
    after_fingerprints: &[NodeFingerprints],
    allow_renames: bool,
) -> Vec<OrderedMatch> {
    let mut before_match = vec![None; before.len()];
    let mut after_match = vec![None; after.len()];
    let before_reserved = before
        .iter()
        .map(|id| context.before.contains_key(id))
        .collect::<Vec<_>>();
    let after_reserved = after
        .iter()
        .map(|id| context.after_to_before.contains_key(id))
        .collect::<Vec<_>>();
    let mut before_exact_targets = HashMap::<FingerprintId, Vec<NodeId>>::new();
    for (index, id) in before.iter().copied().enumerate() {
        if !before_reserved[index] && pair.before.identity_text(id).is_some() {
            before_exact_targets
                .entry(before_fingerprints[id.index()].full)
                .or_default()
                .push(id);
        }
    }
    let mut after_exact_targets = HashMap::<FingerprintId, Vec<NodeId>>::new();
    for (index, id) in after.iter().copied().enumerate() {
        if !after_reserved[index] && pair.after.identity_text(id).is_some() {
            after_exact_targets
                .entry(after_fingerprints[id.index()].full)
                .or_default()
                .push(id);
        }
    }
    let before_masks_exact_unwrap = before
        .iter()
        .copied()
        .enumerate()
        .map(|(before_index, outer)| {
            !before_reserved[before_index]
                && !after_exact_targets.contains_key(&before_fingerprints[outer.index()].full)
                && unique_contained_descendant_matches(
                    &pair.before,
                    outer,
                    &|candidate| context.before.contains_key(&candidate),
                    |inner| {
                        after_exact_targets
                            .get(&before_fingerprints[inner.index()].full)
                            .is_some_and(|targets| {
                                targets.len() == 1
                                    && unique_containment_reparenting(
                                        pair, context, inner, targets[0],
                                    )
                                    .is_some()
                            })
                    },
                )
        })
        .collect::<Vec<_>>();
    let after_masks_exact_wrap = after
        .iter()
        .copied()
        .enumerate()
        .map(|(after_index, outer)| {
            !after_reserved[after_index]
                && !before_exact_targets.contains_key(&after_fingerprints[outer.index()].full)
                && unique_contained_descendant_matches(
                    &pair.after,
                    outer,
                    &|candidate| context.after_to_before.contains_key(&candidate),
                    |inner| {
                        before_exact_targets
                            .get(&after_fingerprints[inner.index()].full)
                            .is_some_and(|targets| {
                                targets.len() == 1
                                    && unique_containment_reparenting(
                                        pair, context, targets[0], inner,
                                    )
                                    .is_some()
                            })
                    },
                )
        })
        .collect::<Vec<_>>();
    let before_direct_reserved = before_reserved
        .iter()
        .copied()
        .zip(before_masks_exact_unwrap.iter().copied())
        .map(|(reserved, wrapper)| reserved || wrapper)
        .collect::<Vec<_>>();
    let after_direct_reserved = after_reserved
        .iter()
        .copied()
        .zip(after_masks_exact_wrap.iter().copied())
        .map(|(reserved, wrapper)| reserved || wrapper)
        .collect::<Vec<_>>();
    let identified_before = (0..before.len())
        .filter(|index| {
            !before_reserved[*index]
                && !before_masks_exact_unwrap[*index]
                && pair.before.identity_text(before[*index]).is_some()
                && pair.before.node(before[*index]).decoration_owner.is_none()
        })
        .collect::<Vec<_>>();
    let identified_after = (0..after.len())
        .filter(|index| {
            !after_reserved[*index]
                && !after_masks_exact_wrap[*index]
                && pair.after.identity_text(after[*index]).is_some()
                && pair.after.node(after[*index]).decoration_owner.is_none()
        })
        .collect::<Vec<_>>();
    let before_identity_keys = identified_before
        .iter()
        .map(|index| identity_key(&pair.before, before[*index]))
        .collect::<HashSet<_>>();
    let after_identity_keys = identified_after
        .iter()
        .map(|index| identity_key(&pair.after, after[*index]))
        .collect::<HashSet<_>>();
    let identified_before = identified_before
        .into_iter()
        .filter(|index| {
            !contains_foreign_descendant_identity(
                &pair.before,
                before[*index],
                &after_identity_keys,
            )
        })
        .collect::<Vec<_>>();
    let identified_after = identified_after
        .into_iter()
        .filter(|index| {
            !contains_foreign_descendant_identity(&pair.after, after[*index], &before_identity_keys)
        })
        .collect::<Vec<_>>();
    let local_before = (0..identified_before.len())
        .filter(|index| {
            pair.before
                .node(before[identified_before[*index]])
                .sibling_matching
                != SiblingMatching::OrderedSyntax
        })
        .collect::<Vec<_>>();
    let local_after = (0..identified_after.len())
        .filter(|index| {
            pair.after
                .node(after[identified_after[*index]])
                .sibling_matching
                != SiblingMatching::OrderedSyntax
        })
        .collect::<Vec<_>>();
    let before_local_values = local_before
        .iter()
        .map(|index| {
            let id = before[identified_before[*index]];
            (
                pair.before.node(id).sibling_matching,
                context_identity(&pair.before, id),
            )
        })
        .collect::<Vec<_>>();
    let after_local_values = local_after
        .iter()
        .map(|index| {
            let id = after[identified_after[*index]];
            (
                pair.after.node(id).sibling_matching,
                context_identity(&pair.after, id),
            )
        })
        .collect::<Vec<_>>();
    for edge in unordered_matches(&before_local_values, &after_local_values) {
        let before_index = identified_before[local_before[edge.before]];
        let after_index = identified_after[local_after[edge.after]];
        before_match[before_index] = Some(after_index);
        after_match[after_index] = Some(before_index);
    }

    let ordered_before = (0..identified_before.len())
        .filter(|index| {
            let candidate = identified_before[*index];
            before_match[candidate].is_none()
                && pair.before.node(before[candidate]).sibling_matching
                    == SiblingMatching::OrderedSyntax
        })
        .collect::<Vec<_>>();
    let ordered_after = (0..identified_after.len())
        .filter(|index| {
            let candidate = identified_after[*index];
            after_match[candidate].is_none()
                && pair.after.node(after[candidate]).sibling_matching
                    == SiblingMatching::OrderedSyntax
        })
        .collect::<Vec<_>>();
    let before_identities = ordered_before
        .iter()
        .map(|index| context_identity(&pair.before, before[identified_before[*index]]))
        .collect::<Vec<_>>();
    let after_identities = ordered_after
        .iter()
        .map(|index| context_identity(&pair.after, after[identified_after[*index]]))
        .collect::<Vec<_>>();
    // Pairing names at this level first; children are matched after their parents enter the queue.
    for edge in unordered_matches(&before_identities, &after_identities) {
        let before_index = identified_before[ordered_before[edge.before]];
        let after_index = identified_after[ordered_after[edge.after]];
        before_match[before_index] = Some(after_index);
        after_match[after_index] = Some(before_index);
    }

    if allow_renames {
        for edge in confident_renamed_context_matches(
            pair,
            before,
            after,
            &before_match,
            &after_match,
            &before_direct_reserved,
            &after_direct_reserved,
        )
        .into_iter()
        .filter(|edge| {
            // Preserving recovered inner matches; renaming their wrapper would change ownership.
            !pair
                .before
                .descendants(before[edge.before])
                .any(|id| context.before.contains_key(&id))
                && !pair
                    .after
                    .descendants(after[edge.after])
                    .any(|id| context.after_to_before.contains_key(&id))
        }) {
            before_match[edge.before] = Some(edge.after);
            after_match[edge.after] = Some(edge.before);
        }
    }

    let anonymous_before = (0..before.len())
        .filter(|index| {
            before_match[*index].is_none()
                && !before_reserved[*index]
                && !before_masks_exact_unwrap[*index]
                && pair.before.identity_text(before[*index]).is_none()
                && pair.before.node(before[*index]).decoration_owner.is_none()
        })
        .collect::<Vec<_>>();
    let anonymous_after = (0..after.len())
        .filter(|index| {
            after_match[*index].is_none()
                && !after_reserved[*index]
                && !after_masks_exact_wrap[*index]
                && pair.after.identity_text(after[*index]).is_none()
                && pair.after.node(after[*index]).decoration_owner.is_none()
        })
        .collect::<Vec<_>>();
    let before_keys = anonymous_before
        .iter()
        .map(|index| anonymous_context_key(&pair.before, before[*index]))
        .collect::<Vec<_>>();
    let after_keys = anonymous_after
        .iter()
        .map(|index| anonymous_context_key(&pair.after, after[*index]))
        .collect::<Vec<_>>();
    let local_before = (0..anonymous_before.len())
        .filter(|index| {
            pair.before
                .node(before[anonymous_before[*index]])
                .sibling_matching
                != SiblingMatching::OrderedSyntax
        })
        .collect::<Vec<_>>();
    let local_after = (0..anonymous_after.len())
        .filter(|index| {
            pair.after
                .node(after[anonymous_after[*index]])
                .sibling_matching
                != SiblingMatching::OrderedSyntax
        })
        .collect::<Vec<_>>();
    let before_local_values = local_before
        .iter()
        .map(|index| {
            let id = before[anonymous_before[*index]];
            (
                pair.before.node(id).sibling_matching,
                context_shape(&pair.before, id),
            )
        })
        .collect::<Vec<_>>();
    let after_local_values = local_after
        .iter()
        .map(|index| {
            let id = after[anonymous_after[*index]];
            (
                pair.after.node(id).sibling_matching,
                context_shape(&pair.after, id),
            )
        })
        .collect::<Vec<_>>();
    for edge in unordered_matches(&before_local_values, &after_local_values) {
        let before_index = anonymous_before[local_before[edge.before]];
        let after_index = anonymous_after[local_after[edge.after]];
        before_match[before_index] = Some(after_index);
        after_match[after_index] = Some(before_index);
    }

    let keyed_before = (0..anonymous_before.len())
        .filter(|index| {
            !before_keys[*index].identities.is_empty()
                && before_match[anonymous_before[*index]].is_none()
                && pair
                    .before
                    .node(before[anonymous_before[*index]])
                    .sibling_matching
                    == SiblingMatching::OrderedSyntax
        })
        .collect::<Vec<_>>();
    let keyed_after = (0..anonymous_after.len())
        .filter(|index| {
            !after_keys[*index].identities.is_empty()
                && after_match[anonymous_after[*index]].is_none()
                && pair
                    .after
                    .node(after[anonymous_after[*index]])
                    .sibling_matching
                    == SiblingMatching::OrderedSyntax
        })
        .collect::<Vec<_>>();
    let before_keyed_values = keyed_before
        .iter()
        .map(|index| before_keys[*index].clone())
        .collect::<Vec<_>>();
    let after_keyed_values = keyed_after
        .iter()
        .map(|index| after_keys[*index].clone())
        .collect::<Vec<_>>();
    for edge in unordered_matches(&before_keyed_values, &after_keyed_values) {
        let before_index = anonymous_before[keyed_before[edge.before]];
        let after_index = anonymous_after[keyed_after[edge.after]];
        before_match[before_index] = Some(after_index);
        after_match[after_index] = Some(before_index);
    }

    let remaining_before = anonymous_before
        .iter()
        .copied()
        .filter(|index| {
            before_match[*index].is_none()
                && pair.before.node(before[*index]).sibling_matching
                    == SiblingMatching::OrderedSyntax
        })
        .collect::<Vec<_>>();
    let remaining_after = anonymous_after
        .iter()
        .copied()
        .filter(|index| {
            after_match[*index].is_none()
                && pair.after.node(after[*index]).sibling_matching == SiblingMatching::OrderedSyntax
        })
        .collect::<Vec<_>>();
    let before_shapes = remaining_before
        .iter()
        .map(|index| context_shape(&pair.before, before[*index]))
        .collect::<Vec<_>>();
    let after_shapes = remaining_after
        .iter()
        .map(|index| context_shape(&pair.after, after[*index]))
        .collect::<Vec<_>>();
    for edge in ordered_matches(&before_shapes, &after_shapes) {
        let before_index = remaining_before[edge.before];
        let after_index = remaining_after[edge.after];
        before_match[before_index] = Some(after_index);
        after_match[after_index] = Some(before_index);
    }

    for edge in decoration_matches(
        pair,
        context,
        ContextCandidates {
            nodes: before,
            matched: &before_match,
            reserved: &before_direct_reserved,
            fingerprints: before_fingerprints,
        },
        ContextCandidates {
            nodes: after,
            matched: &after_match,
            reserved: &after_direct_reserved,
            fingerprints: after_fingerprints,
        },
    ) {
        before_match[edge.before] = Some(edge.after);
        after_match[edge.after] = Some(edge.before);
    }

    before_match
        .into_iter()
        .enumerate()
        .filter_map(|(before, after)| after.map(|after| OrderedMatch { before, after }))
        .collect()
}

struct ContextCandidates<'input> {
    nodes: &'input [NodeId],
    matched: &'input [Option<usize>],
    reserved: &'input [bool],
    fingerprints: &'input [NodeFingerprints],
}

/// Pair decorations within already matched semantic owners.
fn decoration_matches(
    pair: &SyntaxPair<'_, '_>,
    context: &ContextLinks,
    before: ContextCandidates<'_>,
    after: ContextCandidates<'_>,
) -> Vec<OrderedMatch> {
    let mut matches = Vec::new();
    let mut before_by_owner = HashMap::<NodeId, Vec<usize>>::new();
    for (index, id) in before.nodes.iter().copied().enumerate() {
        if before.matched[index].is_some() || before.reserved[index] {
            continue;
        }
        let Some(owner) = pair.before.node(id).decoration_owner else {
            continue;
        };
        before_by_owner.entry(owner).or_default().push(index);
    }
    let mut after_by_owner = HashMap::<NodeId, Vec<usize>>::new();
    for (index, id) in after.nodes.iter().copied().enumerate() {
        if after.matched[index].is_some() || after.reserved[index] {
            continue;
        }
        let Some(owner) = pair.after.node(id).decoration_owner else {
            continue;
        };
        after_by_owner.entry(owner).or_default().push(index);
    }
    if before_by_owner.is_empty() || after_by_owner.is_empty() {
        return matches;
    }

    let provisional_owners = before
        .matched
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(before_index, after_index)| {
            let after_index = after_index?;
            let after = after.nodes.get(after_index)?;
            Some((before.nodes[before_index], *after))
        })
        .collect::<HashMap<_, _>>();
    let mut owner_pairs = Vec::new();
    for before_owner in before_by_owner.keys() {
        for after_owner in [
            context.before.get(before_owner).map(|link| link.after),
            provisional_owners.get(before_owner).copied(),
        ]
        .into_iter()
        .flatten()
        {
            if after_by_owner.contains_key(&after_owner) {
                owner_pairs.push((*before_owner, after_owner));
            }
        }
    }
    owner_pairs.sort_unstable();
    owner_pairs.dedup();

    for (before_owner, after_owner) in owner_pairs {
        let Some(before_group) = before_by_owner.get(&before_owner) else {
            continue;
        };
        let Some(after_group) = after_by_owner.get(&after_owner) else {
            continue;
        };
        let before_exact = before_group
            .iter()
            .map(|index| before.fingerprints[before.nodes[*index].index()].full)
            .collect::<Vec<_>>();
        let after_exact = after_group
            .iter()
            .map(|index| after.fingerprints[after.nodes[*index].index()].full)
            .collect::<Vec<_>>();
        let exact = ordered_matches(&before_exact, &after_exact);
        let mut before_claimed = vec![false; before_group.len()];
        let mut after_claimed = vec![false; after_group.len()];
        for edge in exact {
            before_claimed[edge.before] = true;
            after_claimed[edge.after] = true;
            matches.push(OrderedMatch {
                before: before_group[edge.before],
                after: after_group[edge.after],
            });
        }

        let before_remaining = (0..before_group.len())
            .filter(|index| !before_claimed[*index])
            .collect::<Vec<_>>();
        let after_remaining = (0..after_group.len())
            .filter(|index| !after_claimed[*index])
            .collect::<Vec<_>>();
        let before_shapes = before_remaining
            .iter()
            .map(|index| {
                let id = before.nodes[before_group[*index]];
                (
                    context_shape(&pair.before, id),
                    before.fingerprints[id.index()].shape,
                )
            })
            .collect::<Vec<_>>();
        let after_shapes = after_remaining
            .iter()
            .map(|index| {
                let id = after.nodes[after_group[*index]];
                (
                    context_shape(&pair.after, id),
                    after.fingerprints[id.index()].shape,
                )
            })
            .collect::<Vec<_>>();
        for edge in ordered_matches(&before_shapes, &after_shapes) {
            let before_index = before_remaining[edge.before];
            let after_index = after_remaining[edge.after];
            before_claimed[before_index] = true;
            after_claimed[after_index] = true;
            matches.push(OrderedMatch {
                before: before_group[before_index],
                after: after_group[after_index],
            });
        }

        let before_remaining = (0..before_group.len())
            .filter(|index| !before_claimed[*index])
            .collect::<Vec<_>>();
        let after_remaining = (0..after_group.len())
            .filter(|index| !after_claimed[*index])
            .collect::<Vec<_>>();
        let before_context = before_remaining
            .iter()
            .map(|index| context_shape(&pair.before, before.nodes[before_group[*index]]))
            .collect::<Vec<_>>();
        let after_context = after_remaining
            .iter()
            .map(|index| context_shape(&pair.after, after.nodes[after_group[*index]]))
            .collect::<Vec<_>>();
        for edge in ordered_matches(&before_context, &after_context) {
            matches.push(OrderedMatch {
                before: before_group[before_remaining[edge.before]],
                after: after_group[after_remaining[edge.after]],
            });
        }
    }
    matches.sort_by_key(|edge| edge.before);
    matches
}

/// Pair renamed siblings by syntax shape and local order.
/// Descendant identities can veto a match but do not choose its partner.
fn confident_renamed_context_matches(
    pair: &SyntaxPair<'_, '_>,
    before: &[NodeId],
    after: &[NodeId],
    before_match: &[Option<usize>],
    after_match: &[Option<usize>],
    before_reserved: &[bool],
    after_reserved: &[bool],
) -> Vec<OrderedMatch> {
    let before_candidates = (0..before.len())
        .filter(|index| {
            before_match[*index].is_none()
                && !before_reserved[*index]
                && pair.before.identity_text(before[*index]).is_some()
                && pair.before.node(before[*index]).decoration_owner.is_none()
        })
        .collect::<Vec<_>>();
    let after_candidates = (0..after.len())
        .filter(|index| {
            after_match[*index].is_none()
                && !after_reserved[*index]
                && pair.after.identity_text(after[*index]).is_some()
                && pair.after.node(after[*index]).decoration_owner.is_none()
        })
        .collect::<Vec<_>>();
    let before_keys = before_candidates
        .iter()
        .map(|index| identity_key(&pair.before, before[*index]))
        .collect::<HashSet<_>>();
    let after_keys = after_candidates
        .iter()
        .map(|index| identity_key(&pair.after, after[*index]))
        .collect::<HashSet<_>>();
    let before_candidates = before_candidates
        .iter()
        .copied()
        .filter(|index| {
            descendant_identities(&pair.before, before[*index]).is_disjoint(&after_keys)
        })
        .collect::<Vec<_>>();
    let after_candidates = after_candidates
        .iter()
        .copied()
        .filter(|index| descendant_identities(&pair.after, after[*index]).is_disjoint(&before_keys))
        .collect::<Vec<_>>();
    let before_shapes = before_candidates
        .iter()
        .map(|index| {
            let id = before[*index];
            (
                pair.before.node(id).sibling_matching,
                context_shape(&pair.before, id),
            )
        })
        .collect::<Vec<_>>();
    let after_shapes = after_candidates
        .iter()
        .map(|index| {
            let id = after[*index];
            (
                pair.after.node(id).sibling_matching,
                context_shape(&pair.after, id),
            )
        })
        .collect::<Vec<_>>();
    ordered_matches(&before_shapes, &after_shapes)
        .into_iter()
        .map(|edge| OrderedMatch {
            before: before_candidates[edge.before],
            after: after_candidates[edge.after],
        })
        .collect()
}

fn descendant_identities<'source>(
    tree: &'source SyntaxTree<'_>,
    root: NodeId,
) -> HashSet<(SyntaxKind, &'source str)> {
    let mut identities = HashSet::new();
    if tree.node(root).sibling_matching != SiblingMatching::OrderedSyntax {
        return identities;
    }
    let mut pending = tree.node(root).children.clone();
    while let Some(candidate) = pending.pop() {
        let node = tree.node(candidate);
        if node.sibling_matching != SiblingMatching::OrderedSyntax {
            continue;
        }
        if let Some(identity) = tree.identity_text(candidate) {
            identities.insert((node.kind, identity));
            continue;
        }
        pending.extend(node.children.iter().copied());
    }
    identities
}

fn identity_key<'source>(
    tree: &'source SyntaxTree<'source>,
    id: NodeId,
) -> (SyntaxKind, &'source str) {
    (
        tree.node(id).kind,
        tree.identity_text(id)
            .expect("identified context carries source identity"),
    )
}

fn contains_foreign_descendant_identity(
    tree: &SyntaxTree<'_>,
    root: NodeId,
    opposite: &HashSet<(SyntaxKind, &str)>,
) -> bool {
    let own = identity_key(tree, root);
    descendant_identities(tree, root)
        .into_iter()
        .any(|identity| identity != own && opposite.contains(&identity))
}

fn direct_composites(tree: &SyntaxTree<'_>, parent: NodeId) -> Vec<NodeId> {
    tree.node(parent)
        .children
        .iter()
        .copied()
        .filter(|id| {
            let node = tree.node(*id);
            node.named && node.leaf.is_none()
        })
        .collect()
}

fn context_shape(tree: &SyntaxTree<'_>, id: NodeId) -> ContextShape {
    let node = tree.node(id);
    ContextShape {
        kind: node.kind,
        slot: node.slot,
    }
}

/// Identify an anonymous node from its immediate children's names and identifier fields.
/// Deeper composites are matched only after this node has a partner.
fn anonymous_context_key<'source>(
    tree: &'source SyntaxTree<'source>,
    id: NodeId,
) -> AnonymousContextKey<'source> {
    let mut identities = Vec::new();
    for candidate in tree.node(id).children.iter().copied() {
        let node = tree.node(candidate);
        if node.sibling_matching != SiblingMatching::OrderedSyntax {
            continue;
        }
        if let Some(identity) = tree.identity_text(candidate) {
            identities.push(ContextIdentity {
                kind: node.kind,
                slot: node.slot,
                identity,
            });
            continue;
        }
        if matches!(node.slot, ChildSlot::Field(_))
            && node
                .leaf
                .is_some_and(|leaf| leaf.role == LeafRole::Identifier)
        {
            identities.push(ContextIdentity {
                kind: node.kind,
                slot: node.slot,
                identity: tree
                    .leaf_text(candidate)
                    .expect("a concrete identifier owns source spelling"),
            });
            continue;
        }
    }
    AnonymousContextKey {
        shape: context_shape(tree, id),
        identities,
    }
}

fn context_identity<'source>(
    tree: &'source SyntaxTree<'source>,
    id: NodeId,
) -> ContextIdentity<'source> {
    let node = tree.node(id);
    ContextIdentity {
        kind: node.kind,
        slot: node.slot,
        identity: tree
            .identity_text(id)
            .expect("identified context carries source identity"),
    }
}

/// Decide movement from semantic sibling order, then give decorations their owner's placement.
fn contextual_match_placements(
    pair: &SyntaxPair<'_, '_>,
    before: &[NodeId],
    matches: &[OrderedMatch],
    links: &ContextLinks,
) -> Vec<Placement> {
    let semantic = matches
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, edge)| {
            pair.before
                .node(before[edge.before])
                .decoration_owner
                .is_none()
        })
        .collect::<Vec<_>>();
    let semantic_edges = semantic.iter().map(|(_, edge)| *edge).collect::<Vec<_>>();
    let semantic_placements = match_placements(&semantic_edges);
    let mut placements = vec![None; matches.len()];
    let mut owner_placements = links
        .before
        .iter()
        .map(|(before, link)| (*before, link.placement))
        .collect::<HashMap<_, _>>();
    for ((match_index, edge), placement) in semantic.into_iter().zip(semantic_placements) {
        placements[match_index] = Some(placement);
        owner_placements.insert(before[edge.before], placement);
    }
    for (match_index, edge) in matches.iter().enumerate() {
        if placements[match_index].is_some() {
            continue;
        }
        let before = before[edge.before];
        let owner = pair
            .before
            .node(before)
            .decoration_owner
            .expect("only decorations are excluded from semantic placement");
        placements[match_index] = owner_placements.get(&owner).copied();
    }
    placements
        .into_iter()
        .map(|placement| placement.expect("a matched decoration requires a matched owner"))
        .collect()
}

/// Claim a pair, accepting an existing link only if its placement and wrapper evidence also agree.
fn link_context(
    before: NodeId,
    after: NodeId,
    placement: Placement,
    reparenting: Option<Reparenting>,
    links: &mut ContextLinks,
) -> bool {
    let link = ContextLink {
        after,
        placement,
        reparenting,
    };
    let existing_link = links.before.get(&before).copied();
    let existing_before = links.after_to_before.get(&after).copied();
    if existing_link.is_some() || existing_before.is_some() {
        return existing_link == Some(link) && existing_before == Some(before);
    }

    links.before.insert(before, link);
    links.after_to_before.insert(after, before);
    true
}

/// Expand a verified full-fingerprint match into node pairs, omitting layout leaves.
fn exact_subtree_nodes(
    pair: &SyntaxPair<'_, '_>,
    before: NodeId,
    after: NodeId,
) -> Vec<(NodeId, NodeId)> {
    let mut nodes = Vec::new();
    let mut pending = vec![(before, after)];
    while let Some((before, after)) = pending.pop() {
        nodes.push((before, after));
        let before_node = pair.before.node(before);
        let after_node = pair.after.node(after);
        match (before_node.leaf, after_node.leaf) {
            (Some(_), Some(_)) => {}
            (None, None) => {
                let before_children = before_node
                    .children
                    .iter()
                    .filter(|child| !is_layout_leaf(&pair.before, **child));
                let before_children = before_children.copied().collect::<Vec<_>>();
                let after_children = after_node
                    .children
                    .iter()
                    .filter(|child| !is_layout_leaf(&pair.after, **child));
                let after_children = after_children.copied().collect::<Vec<_>>();
                debug_assert_eq!(before_children.len(), after_children.len());
                pending.extend(before_children.into_iter().zip(after_children).rev());
            }
            _ => unreachable!("equal recursive fingerprints retain leaf shape"),
        }
    }
    nodes
}

fn subtree_sizes(tree: &SyntaxTree<'_>) -> Vec<usize> {
    let mut sizes = vec![1; tree.nodes.len()];
    for index in (1..tree.nodes.len()).rev() {
        let node = tree.node(NodeId::new(index));
        let Some(parent) = node.parent else {
            continue;
        };
        sizes[parent.index()] += sizes[index];
    }
    sizes
}

impl TreeDiffBuilder<'_, '_, '_> {
    fn link_exact_subtree(
        &mut self,
        before: NodeId,
        after: NodeId,
        placement: Placement,
        parent: ParentCorrespondence,
        wrapper: Option<Reparenting>,
    ) {
        let root = before;
        let mut pending = vec![(before, after)];
        while let Some((before, after)) = pending.pop() {
            let before_node = self.pair.before.node(before);
            let after_node = self.pair.after.node(after);
            match (before_node.leaf, after_node.leaf) {
                (Some(_), Some(_)) => {
                    self.push_leaf_link(LeafLink {
                        before,
                        after,
                        relation: LeafRelation::Exact,
                        placement,
                        parent: ParentCorrespondence::Direct,
                        wrapper,
                    });
                }
                (None, None) => {
                    if before_node.is_scope_boundary() && after_node.is_scope_boundary() {
                        let parent = if before == root {
                            parent
                        } else {
                            ParentCorrespondence::Direct
                        };
                        let accepted = self.push_scope_link(ScopeLink {
                            before,
                            after,
                            placement,
                            parent,
                        });
                        // Exact text cannot override a conflicting scope match.
                        if !accepted {
                            continue;
                        }
                    }

                    let before_children = before_node
                        .children
                        .iter()
                        .filter(|child| !is_layout_leaf(&self.pair.before, **child))
                        .copied()
                        .collect::<Vec<_>>();
                    let after_children = after_node
                        .children
                        .iter()
                        .filter(|child| !is_layout_leaf(&self.pair.after, **child))
                        .copied()
                        .collect::<Vec<_>>();
                    debug_assert_eq!(before_children.len(), after_children.len());
                    if before_children.len() != after_children.len() {
                        continue;
                    }
                    pending.extend(before_children.into_iter().zip(after_children).rev());
                }
                _ => debug_assert!(false, "equal recursive fingerprints retain leaf shape"),
            }
        }
    }

    /// Claim a leaf pair, returning false if either node already has a different link.
    /// Identical claims succeed without adding another link.
    fn push_leaf_link(&mut self, link: LeafLink) -> bool {
        let before = self.tree.leaves.before[link.before.index()];
        let after = self.tree.leaves.after[link.after.index()];
        if before.is_some() || after.is_some() {
            return before == after
                && before
                    .and_then(|index| self.tree.leaves.links.get(index))
                    .is_some_and(|existing| existing == &link);
        }

        let index = self.tree.leaves.links.len();
        self.tree.leaves.before[link.before.index()] = Some(index);
        self.tree.leaves.after[link.after.index()] = Some(index);
        self.tree.leaves.links.push(link);
        true
    }

    /// Claim a scope pair, returning false if either node already has a different link.
    /// Identical claims succeed without adding another link.
    fn push_scope_link(&mut self, link: ScopeLink) -> bool {
        let before = self.before_scope[link.before.index()];
        let after = self.after_scope[link.after.index()];
        if before.is_some() || after.is_some() {
            return before == after
                && before
                    .and_then(|index| self.tree.scopes.get(index))
                    .is_some_and(|existing| existing == &link);
        }
        let index = self.tree.scopes.len();
        self.before_scope[link.before.index()] = Some(index);
        self.after_scope[link.after.index()] = Some(index);
        self.tree.scopes.push(link);
        true
    }
}

fn leaf_shape(tree: &SyntaxTree<'_>, id: NodeId) -> LeafShape {
    let node = tree.node(id);
    let leaf = node.leaf.expect("leaf collection contains only leaves");
    LeafShape {
        kind: node.kind,
        slot: node.slot,
        channel: leaf.channel,
        named: node.named,
        extra: node.extra,
        missing: node.missing,
    }
}

/// Occurrence indices; the matching algorithm determines whether these edges may cross.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct OrderedMatch {
    before: usize,
    after: usize,
}

/// Match equal values once per occurrence while preserving order in both sequences.
/// Noncrossing unique values anchor the alignment. Small gaps use a longest common
/// subsequence; large gaps use greedy matching to cap memory use.
fn ordered_matches<T: Eq + Hash>(before: &[T], after: &[T]) -> Vec<OrderedMatch> {
    let mut before_positions = HashMap::<&T, Vec<usize>>::new();
    for (index, value) in before.iter().enumerate() {
        before_positions.entry(value).or_default().push(index);
    }

    let mut after_positions = HashMap::<&T, Vec<usize>>::new();
    for (index, value) in after.iter().enumerate() {
        after_positions.entry(value).or_default().push(index);
    }

    let mut candidates = before_positions
        .iter()
        .filter_map(|(value, before_indices)| {
            let [before] = before_indices.as_slice() else {
                return None;
            };
            let [after] = after_positions.get(value)?.as_slice() else {
                return None;
            };
            Some(OrderedMatch {
                before: *before,
                after: *after,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable_by_key(|edge| edge.before);

    let candidate_after = candidates.iter().map(|edge| edge.after).collect::<Vec<_>>();
    let stable = increasing_subsequence_members(&candidate_after);
    let anchors = candidates
        .into_iter()
        .zip(stable)
        .filter_map(|(edge, stable)| stable.then_some(edge));

    let mut matches = Vec::new();
    let mut before_start = 0;
    let mut after_start = 0;
    for anchor in anchors.chain(std::iter::once(OrderedMatch {
        before: before.len(),
        after: after.len(),
    })) {
        let gap = align_gap(
            &before[before_start..anchor.before],
            &after[after_start..anchor.after],
        );
        matches.extend(gap.into_iter().map(|edge| OrderedMatch {
            before: before_start + edge.before,
            after: after_start + edge.after,
        }));

        if anchor.before < before.len() && anchor.after < after.len() {
            matches.push(anchor);
        }
        before_start = anchor.before.saturating_add(1);
        after_start = anchor.after.saturating_add(1);
    }
    matches
}

/// Align atomic units locally so copied runs do not pull surviving lines out of place.
fn locality_first_matches<T: Eq + Hash>(before: &[T], after: &[T]) -> Vec<OrderedMatch> {
    let mut before_start = 0;
    let mut after_start = 0;
    let mut matches = Vec::new();
    while before_start < before.len() && after_start < after.len() {
        let mut before_seen = HashMap::<&T, usize>::new();
        let mut after_seen = HashMap::<&T, usize>::new();
        let mut found = None;
        let remaining = (before.len() - before_start).max(after.len() - after_start);
        for radius in 0..remaining {
            let mut candidates = Vec::with_capacity(2);
            if let Some(index) = before_start
                .checked_add(radius)
                .filter(|index| *index < before.len())
            {
                let value = &before[index];
                before_seen.entry(value).or_insert(index);
                if let Some(after) = after_seen.get(value).copied() {
                    candidates.push(OrderedMatch {
                        before: index,
                        after,
                    });
                }
            }
            if let Some(index) = after_start
                .checked_add(radius)
                .filter(|index| *index < after.len())
            {
                let value = &after[index];
                after_seen.entry(value).or_insert(index);
                if let Some(before) = before_seen.get(value).copied() {
                    candidates.push(OrderedMatch {
                        before,
                        after: index,
                    });
                }
            }
            found = candidates.into_iter().min_by_key(|edge| {
                (
                    edge.before - before_start + edge.after - after_start,
                    edge.after,
                    edge.before,
                )
            });
            if found.is_some() {
                break;
            }
        }
        let Some(edge) = found else {
            break;
        };
        matches.push(edge);
        before_start = edge.before + 1;
        after_start = edge.after + 1;
    }
    matches
}

/// Pair equal values regardless of order, matching repeated values by occurrence number.
fn unordered_matches<T: Eq + Hash>(before: &[T], after: &[T]) -> Vec<OrderedMatch> {
    let mut after_positions = HashMap::<&T, VecDeque<usize>>::new();
    for (index, value) in after.iter().enumerate() {
        after_positions.entry(value).or_default().push_back(index);
    }

    before
        .iter()
        .enumerate()
        .filter_map(|(before, value)| {
            let after = after_positions.get_mut(value)?.pop_front()?;
            Some(OrderedMatch { before, after })
        })
        .collect()
}

/// Mark a longest noncrossing sequence as stable and classify the remaining matches as moves.
/// Input matches must be sorted by their before index.
fn match_placements(matches: &[OrderedMatch]) -> Vec<Placement> {
    let after = matches.iter().map(|edge| edge.after).collect::<Vec<_>>();
    increasing_subsequence_members(&after)
        .into_iter()
        .map(|stable| {
            if stable {
                Placement::Stable
            } else {
                Placement::Reordered
            }
        })
        .collect()
}

/// Mark one longest strictly increasing subsequence.
/// Ties choose the latest occurrence of an equal tail value.
fn increasing_subsequence_members(values: &[usize]) -> Vec<bool> {
    let mut tails = Vec::<usize>::new();
    let mut previous = vec![None; values.len()];
    for (index, value) in values.iter().copied().enumerate() {
        let slot = tails.partition_point(|tail| values[*tail] < value);
        if slot > 0 {
            previous[index] = Some(tails[slot - 1]);
        }
        if slot == tails.len() {
            tails.push(index);
        } else {
            tails[slot] = index;
        }
    }

    let mut members = vec![false; values.len()];
    let Some(mut index) = tails.last().copied() else {
        return members;
    };
    loop {
        members[index] = true;
        let Some(parent) = previous[index] else {
            break;
        };
        index = parent;
    }
    members
}

fn align_gap<T: Eq + Hash>(before: &[T], after: &[T]) -> Vec<OrderedMatch> {
    if before.is_empty() || after.is_empty() {
        return Vec::new();
    }

    let cells = before.len().saturating_mul(after.len());
    if cells > MAX_LOCAL_ALIGNMENT_CELLS {
        return greedy_matches(before, after);
    }
    lcs_matches(before, after)
}

/// Align a large gap in linear memory; the result may retain fewer matches than LCS.
fn greedy_matches<T: Eq + Hash>(before: &[T], after: &[T]) -> Vec<OrderedMatch> {
    let mut after_positions = HashMap::<&T, VecDeque<usize>>::new();
    for (index, value) in after.iter().enumerate() {
        after_positions.entry(value).or_default().push_back(index);
    }

    let mut matches = Vec::new();
    let mut after_floor = 0;
    for (before, value) in before.iter().enumerate() {
        let Some(positions) = after_positions.get_mut(value) else {
            continue;
        };
        while positions.front().is_some_and(|after| *after < after_floor) {
            positions.pop_front();
        }
        let Some(after) = positions.pop_front() else {
            continue;
        };
        matches.push(OrderedMatch { before, after });
        after_floor = after + 1;
    }
    matches
}

/// Maximize equal-value matches, breaking ties by the smallest total positional drift.
/// Callers must bound the gap size because the score table is quadratic.
fn lcs_matches<T: Eq>(before: &[T], after: &[T]) -> Vec<OrderedMatch> {
    #[derive(Clone, Copy, Default, Eq, PartialEq)]
    struct AlignmentScore {
        retained: usize,
        drift: usize,
    }

    let width = after.len() + 1;
    let mut scores = vec![AlignmentScore::default(); (before.len() + 1) * width];
    let better = |left: AlignmentScore, right: AlignmentScore| {
        left.retained > right.retained
            || (left.retained == right.retained && left.drift < right.drift)
    };
    for before_index in (0..before.len()).rev() {
        for after_index in (0..after.len()).rev() {
            let mut score = scores[(before_index + 1) * width + after_index];
            let skip_after = scores[before_index * width + after_index + 1];
            if better(skip_after, score) {
                score = skip_after;
            }
            if before[before_index] == after[after_index] {
                let mut paired = scores[(before_index + 1) * width + after_index + 1];
                paired.retained += 1;
                paired.drift = paired
                    .drift
                    .saturating_add(before_index.abs_diff(after_index));
                if better(paired, score) {
                    score = paired;
                }
            }
            scores[before_index * width + after_index] = score;
        }
    }

    let mut matches = Vec::new();
    let mut before_index = 0;
    let mut after_index = 0;
    while before_index < before.len() && after_index < after.len() {
        if before[before_index] == after[after_index] {
            let mut paired = scores[(before_index + 1) * width + after_index + 1];
            paired.retained += 1;
            paired.drift = paired
                .drift
                .saturating_add(before_index.abs_diff(after_index));
            if paired == scores[before_index * width + after_index] {
                matches.push(OrderedMatch {
                    before: before_index,
                    after: after_index,
                });
                before_index += 1;
                after_index += 1;
                continue;
            }
        }

        let target = scores[before_index * width + after_index];
        let skip_before = scores[(before_index + 1) * width + after_index];
        let skip_after = scores[before_index * width + after_index + 1];
        match (skip_before == target, skip_after == target) {
            (true, false) => before_index += 1,
            (false, true) => after_index += 1,
            (true, true) if before.len() - before_index >= after.len() - after_index => {
                before_index += 1;
            }
            (true, true) => after_index += 1,
            (false, false) => unreachable!("alignment score has no reconstructible edge"),
        }
    }
    matches
}

#[cfg(test)]
mod tests;
