//! Language-agnostic commonality over neutral before/after projections.

use super::SyntaxClass;
use super::projection::{
    ContentChannel, CorrespondenceRole, Language, LayoutOwnership, NodeId, Projection,
    ProjectionPair, ReviewMode, horizontal_layout, node_is_line_isolated,
};
use super::source::LineEnding;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::ops::Range;

const MAX_LOCAL_ALIGNMENT_CELLS: usize = 16_384;

/// Projection-only edit graph; planning consumes it without structural or unit rematching.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Correspondence {
    /// Semantic review units not superseded by an exact local line fallback.
    pub(crate) units: Vec<UnitEdit>,
    /// Exact physical-line alignment certified inside one matched semantic scope.
    pub(crate) line_links: Vec<LineLink>,
    /// Content-paired physical rows whose concrete terminators differ.
    pub(crate) line_ending_edits: Vec<LineLink>,
    /// Source-exact regions reviewed linewise without discarding neighboring syntax.
    pub(crate) line_fallbacks: Vec<LineFallback>,
    /// Physical row pairs local to reordered units and therefore non-monotone globally.
    pub(crate) unit_line_links: Vec<LineLink>,
    pub(crate) leaf_links: Vec<LeafLink>,
    /// Dense before-node lookup into `leaf_links`.
    pub(crate) before_leaf: Vec<Option<usize>>,
    /// Dense after-node lookup into `leaf_links`.
    pub(crate) after_leaf: Vec<Option<usize>>,
    /// Exact named-subtree links, including nested structural evidence.
    pub(crate) composites: Vec<NodeLink>,
    /// Complete parser-node correspondence used to keep later selection on node boundaries.
    pub(crate) scopes: Vec<ScopeLink>,
}

impl Correspondence {
    /// Exact physical-line links wholly contained by a pair of absolute line ranges.
    pub(crate) fn line_links_in(
        &self,
        before: Range<usize>,
        after: Range<usize>,
    ) -> impl Iterator<Item = LineLink> + '_ {
        let start = self
            .line_links
            .partition_point(|link| link.before < before.start);
        let end = self
            .line_links
            .partition_point(|link| link.before < before.end);
        self.line_links[start..end]
            .iter()
            .copied()
            .filter(move |link| after.contains(&link.after))
    }

    /// Terminator edits wholly contained by a pair of absolute line ranges.
    pub(crate) fn line_ending_edits_in(
        &self,
        before: Range<usize>,
        after: Range<usize>,
    ) -> impl Iterator<Item = LineLink> + '_ {
        let start = self
            .line_ending_edits
            .partition_point(|link| link.before < before.start);
        let end = self
            .line_ending_edits
            .partition_point(|link| link.before < before.end);
        self.line_ending_edits[start..end]
            .iter()
            .copied()
            .filter(move |link| after.contains(&link.after))
    }

    /// Leaf links owned by one matched review unit.
    pub(crate) fn unit_leaf_links(&self, unit: &MatchedUnit) -> &[LeafLink] {
        &self.leaf_links[unit.leaf_links.clone()]
    }

    /// Content-paired physical rows owned by one reordered review unit.
    pub(crate) fn unit_line_links(&self, unit: &MatchedUnit) -> &[LineLink] {
        &self.unit_line_links[unit.line_links.clone()]
    }

    /// Exact composite links owned by one matched review unit.
    pub(crate) fn unit_composites(&self, unit: &MatchedUnit) -> &[NodeLink] {
        &self.composites[unit.composites.clone()]
    }

    /// Link terminating at one after-world leaf.
    pub(crate) fn after_leaf_link(&self, node: NodeId) -> Option<&LeafLink> {
        let link = self.after_leaf.get(node.index()).copied().flatten()?;
        self.leaf_links.get(link)
    }

    /// Link originating at one before-world leaf.
    pub(crate) fn before_leaf_link(&self, node: NodeId) -> Option<&LeafLink> {
        let link = self.before_leaf.get(node.index()).copied().flatten()?;
        self.leaf_links.get(link)
    }
}

/// One exact physical-line correspondence, using zero-based source-line indices.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct LineLink {
    pub(crate) before: usize,
    pub(crate) after: usize,
}

/// Zero-based physical source ranges that require local linewise review.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LineFallback {
    pub(crate) before: Range<usize>,
    pub(crate) after: Range<usize>,
}

/// One review unit in the merged before/after edit script.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum UnitEdit {
    Matched(MatchedUnit),
    Removed { before: NodeId },
    Added { after: NodeId },
}

/// One-to-one review-unit correspondence and the facts derived from it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MatchedUnit {
    pub(crate) before: NodeId,
    pub(crate) after: NodeId,
    /// Symmetric review behavior chosen from both frontend classifications.
    pub(crate) mode: ReviewMode,
    pub(crate) relation: ContentRelation,
    pub(crate) placement: Placement,
    line_links: Range<usize>,
    leaf_links: Range<usize>,
    composites: Range<usize>,
}

/// Strongest equality shared by one matched unit, from exact spelling to changed syntax.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ContentRelation {
    SourceEqual,
    FullEqual,
    PayloadEqual,
    Modified,
}

impl ContentRelation {
    pub(crate) const fn source_equal(self) -> bool {
        matches!(self, Self::SourceEqual)
    }

    pub(crate) const fn full_equal(self) -> bool {
        matches!(self, Self::SourceEqual | Self::FullEqual)
    }

    pub(crate) const fn payload_equal(self) -> bool {
        !matches!(self, Self::Modified)
    }
}

/// Relative order inside the frontend-selected movement domain.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Placement {
    Stable,
    Reordered,
}

/// One-to-one concrete-leaf correspondence inside a matched review unit.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct LeafLink {
    pub(crate) before: NodeId,
    pub(crate) after: NodeId,
    pub(crate) relation: LeafRelation,
    pub(crate) placement: Placement,
    pub(crate) parent: ParentCorrespondence,
    /// Enclosing transparent transition retained for presentation as reflow.
    pub(crate) wrapper: Option<Reparenting>,
}

/// Whether a leaf retained exact payload or only a compatible structural role.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum LeafRelation {
    Exact,
    Modified,
}

/// Exact named subtree, possibly retained beneath a different matched parent.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct NodeLink {
    pub(crate) before: NodeId,
    pub(crate) after: NodeId,
    pub(crate) parent: ParentCorrespondence,
    pub(crate) wrapper: Option<Reparenting>,
    pub(crate) placement: Placement,
}

/// One semantic-container pair that fences all descendant correspondence.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ScopeLink {
    pub(crate) before: NodeId,
    pub(crate) after: NodeId,
    pub(crate) placement: Placement,
    pub(crate) parent: ParentCorrespondence,
}

/// Positive proof relating the immediate semantic parents of one syntax link.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum ParentCorrespondence {
    Direct,
    Reparented(Reparenting),
}

/// Selective parent traversal proven to be a transparent wrap or unwrap.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum Reparenting {
    Wrapped,
    Unwrapped,
}

/// Collision-free identifier assigned by structural interning, never by hash value alone.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct FingerprintId(usize);

/// Intrinsic node facts; incoming field names live on parent-to-child edges.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct NodeAtom {
    kind: &'static str,
    channel: Option<ContentChannel>,
    named: bool,
    extra: bool,
    missing: bool,
}

/// Recursive edge retained inside a collision-free structural fingerprint.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct FingerprintChild {
    field: Option<&'static str>,
    fingerprint: FingerprintId,
}

/// Exact recursive shape; payload is present for concrete leaves, including opaque source.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct FingerprintKey<'source> {
    atom: NodeAtom,
    payload: Option<&'source str>,
    children: Vec<FingerprintChild>,
}

/// Shared before/after interner whose equality check resolves all hash collisions.
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

/// Recursive full/payload fingerprints for every arena node.
fn fingerprints<'source>(
    projection: &'source Projection<'_>,
    interner: &mut FingerprintInterner<'source>,
) -> Vec<NodeFingerprints> {
    let mut fingerprints = vec![None::<NodeFingerprints>; projection.nodes.len()];
    for index in (0..projection.nodes.len()).rev() {
        let id = NodeId::new(index);
        let node = projection.node(id);
        let payload = projection.leaf_text(id);
        let atom = NodeAtom {
            kind: node.kind,
            channel: node.leaf.map(|leaf| leaf.channel),
            named: node.named,
            extra: node.extra,
            missing: node.missing,
        };
        let full_children = node
            .children
            .iter()
            .filter(|child| !is_layout_leaf(projection, **child))
            .map(|child| FingerprintChild {
                field: projection.node(*child).field,
                fingerprint: fingerprints[child.index()]
                    .expect("children follow parents in projection preorder")
                    .full,
            })
            .collect();
        let full = interner.intern(FingerprintKey {
            atom,
            payload,
            children: full_children,
        });
        let shape_children = node
            .children
            .iter()
            .filter(|child| !is_layout_leaf(projection, **child))
            .map(|child| FingerprintChild {
                field: projection.node(*child).field,
                fingerprint: fingerprints[child.index()]
                    .expect("children follow parents in projection preorder")
                    .shape,
            })
            .collect();
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
            let children = node
                .children
                .iter()
                .filter_map(|child| {
                    let child_fingerprint: NodeFingerprints = fingerprints[child.index()]
                        .expect("children follow parents in projection preorder");
                    Some(FingerprintChild {
                        field: projection.node(*child).field,
                        fingerprint: child_fingerprint.payload?,
                    })
                })
                .collect();
            Some(interner.intern(FingerprintKey {
                atom,
                payload,
                children,
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
        .map(|fingerprint| fingerprint.expect("every projection node was fingerprinted"))
        .collect()
}

fn is_layout_leaf(projection: &Projection<'_>, id: NodeId) -> bool {
    projection
        .node(id)
        .leaf
        .is_some_and(|leaf| leaf.channel == ContentChannel::Layout)
}

#[derive(Clone)]
struct UnitRecord<'source> {
    id: NodeId,
    kind: &'static str,
    identity: Option<&'source str>,
    atomic: bool,
    decoration_owner: Option<NodeId>,
    fingerprint: NodeFingerprints,
    shape: FingerprintId,
    mode: ReviewMode,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct UnitKey<'source> {
    kind: &'static str,
    identity: &'source str,
}

/// Build the complete neutral correspondence graph for one symmetric projection pair.
pub(crate) fn correspond(pair: &ProjectionPair<'_, '_>) -> Correspondence {
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
    let stable = stable_unit_matches(&before_match, &after_match, &before_units, &after_units);
    let physical = if pair.before.language == Language::Lines {
        PhysicalLineFacts {
            exact: line_links_from_unit_matches(pair, &before_units, &after_units, &before_match),
            ..PhysicalLineFacts::default()
        }
    } else {
        physical_line_correspondence(pair)
    };

    let graph = Correspondence {
        units: Vec::new(),
        line_links: Vec::new(),
        line_ending_edits: Vec::new(),
        line_fallbacks: Vec::new(),
        unit_line_links: Vec::new(),
        leaf_links: Vec::new(),
        before_leaf: vec![None; pair.before.nodes.len()],
        after_leaf: vec![None; pair.after.nodes.len()],
        composites: Vec::new(),
        scopes: Vec::new(),
    };
    let scopes = vec![ScopeLink {
        before: pair.before.root,
        after: pair.after.root,
        placement: Placement::Stable,
        parent: ParentCorrespondence::Direct,
    }];
    let mut before_scope = vec![None; pair.before.nodes.len()];
    let mut after_scope = vec![None; pair.after.nodes.len()];
    before_scope[pair.before.root.index()] = Some(0);
    after_scope[pair.after.root.index()] = Some(0);
    let builder = CorrespondenceBuilder {
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
        scopes,
        graph,
    };
    let (mut graph, scopes) = builder.build();
    let scope_proof = ScopeProof::new(pair, &scopes);
    if pair.before.language == Language::Lines {
        graph.line_links = physical.exact;
        graph.line_ending_edits = physical.ending_edits;
    } else {
        let scoped = scoped_physical_line_correspondence(pair, &graph, &scope_proof, &physical);
        graph.line_links = scoped.exact;
        graph.line_ending_edits = scoped.ending_edits;
    }
    if pair.before.language != Language::Lines {
        let fallbacks = local_line_fallbacks(pair, &graph, physical.missing_terminators);
        let suppressed = graph
            .units
            .iter()
            .map(|edit| {
                let unit = unit_line_geometry(pair, &graph, edit);
                fallbacks.iter().any(|fallback| {
                    (!unit.before.is_empty()
                        && unit.before.start < fallback.before.end
                        && fallback.before.start < unit.before.end)
                        || (!unit.after.is_empty()
                            && unit.after.start < fallback.after.end
                            && fallback.after.start < unit.after.end)
                })
            })
            .collect::<Vec<_>>();
        graph.units = std::mem::take(&mut graph.units)
            .into_iter()
            .enumerate()
            .filter_map(|(index, edit)| (!suppressed[index]).then_some(edit))
            .collect();
        graph.line_fallbacks = fallbacks;
    }
    debug_assert!(
        scope_correspondence_is_valid(pair, &graph, &scopes, &scope_proof),
        "correspondence escaped a semantic-container fence"
    );
    graph.scopes = scopes;
    graph
}

fn scope_correspondence_is_valid(
    pair: &ProjectionPair<'_, '_>,
    graph: &Correspondence,
    scopes: &[ScopeLink],
    scope_proof: &ScopeProof,
) -> bool {
    if !scope_proof.one_to_one {
        return false;
    }
    if scopes.iter().any(|link| {
        !parent_correspondence_is_valid(pair, scope_proof, link.before, link.after, link.parent)
    }) {
        return false;
    }
    let edge_is_valid = |before, after, parent| {
        parent_correspondence_is_valid(pair, scope_proof, before, after, parent)
    };
    if graph
        .leaf_links
        .iter()
        .any(|link| !edge_is_valid(link.before, link.after, link.parent))
    {
        return false;
    }
    if graph
        .composites
        .iter()
        .any(|link| !edge_is_valid(link.before, link.after, link.parent))
    {
        return false;
    }

    let line_is_scoped = |link| scope_proof.line_has_scoped_cover(link, false);
    if graph
        .line_links
        .iter()
        .copied()
        .any(|link| !line_is_scoped(link))
    {
        return false;
    }
    if graph
        .line_ending_edits
        .iter()
        .copied()
        .any(|link| !line_is_scoped(link))
    {
        return false;
    }
    true
}

fn parent_correspondence_is_valid(
    pair: &ProjectionPair<'_, '_>,
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
    pair: &ProjectionPair<'_, '_>,
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

fn enclosing_semantic_scope(projection: &Projection<'_>, node: NodeId) -> Option<NodeId> {
    nearest_scope_owner(projection, projection.node(node).parent?)
}

/// Exact line-leaf unit edges are the canonical physical-line graph for line projections.
fn line_links_from_unit_matches(
    pair: &ProjectionPair<'_, '_>,
    before: &[UnitRecord<'_>],
    after: &[UnitRecord<'_>],
    before_match: &[Option<usize>],
) -> Vec<LineLink> {
    before_match
        .iter()
        .enumerate()
        .filter_map(|(before_index, after_index)| {
            let after_index = (*after_index)?;
            let before_node = pair.before.node(before[before_index].id);
            let after_node = pair.after.node(after[after_index].id);
            let before_source = pair.before.source.slice(before_node.bytes.clone())?;
            let after_source = pair.after.source.slice(after_node.bytes.clone())?;
            (before_source == after_source).then(|| LineLink {
                before: before_node.lines.start.saturating_sub(1),
                after: after_node.lines.start.saturating_sub(1),
            })
        })
        .collect()
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
    /// After-world scope ids provide one canonical witness on both revisions.
    scopes: Vec<NodeId>,
}

/// Canonical semantic-scope witnesses used by every physical-line anchor.
struct ScopeProof {
    before_lines: Vec<Vec<NodeId>>,
    after_lines: Vec<Vec<NodeId>>,
    links: Vec<Option<ScopeLink>>,
    reverse_links: Vec<Option<ScopeLink>>,
    one_to_one: bool,
}

impl ScopeProof {
    fn new(pair: &ProjectionPair<'_, '_>, scopes: &[ScopeLink]) -> Self {
        let mut links = vec![None; pair.before.nodes.len()];
        let mut reverse_links = vec![None; pair.after.nodes.len()];
        let mut before_claimed = vec![false; pair.before.nodes.len()];
        let mut after_claimed = vec![false; pair.after.nodes.len()];
        let mut one_to_one = true;
        for link in scopes {
            let Some(before_claimed) = before_claimed.get_mut(link.before.index()) else {
                one_to_one = false;
                continue;
            };
            let Some(after_claimed) = after_claimed.get_mut(link.after.index()) else {
                one_to_one = false;
                continue;
            };
            if *before_claimed || *after_claimed {
                one_to_one = false;
                continue;
            }
            *before_claimed = true;
            *after_claimed = true;
            links[link.before.index()] = Some(*link);
            reverse_links[link.after.index()] = Some(*link);
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
        pair: &ProjectionPair<'_, '_>,
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
        let mut scopes = scopes.to_vec();
        if scopes.iter().any(|after| {
            self.reverse_link(*after)
                .is_none_or(|link| link.placement != Placement::Stable)
        }) {
            return None;
        }
        scopes.sort_unstable();
        scopes.dedup();
        Some(scopes)
    }
}

/// Map every physical row to its nearest semantic owners, including layout-only rows.
fn scope_lines(projection: &Projection<'_>) -> Vec<Vec<NodeId>> {
    let mut lines = vec![Vec::new(); projection.source.lines().len()];
    for (index, node) in projection.nodes.iter().enumerate() {
        if node.leaf.is_none() {
            continue;
        }
        let id = NodeId::new(index);
        let Some(owner) = nearest_scope_owner(projection, id) else {
            continue;
        };
        for line in zero_based_lines(projection, id) {
            lines[line].push(owner);
        }
    }
    for owners in &mut lines {
        owners.sort_unstable();
        owners.dedup();
    }
    lines
}

fn nearest_scope_owner(projection: &Projection<'_>, mut candidate: NodeId) -> Option<NodeId> {
    loop {
        let node = projection.node(candidate);
        if node.is_scope_boundary() {
            return Some(candidate);
        }
        candidate = node.parent?;
    }
}

/// Align physical content once while retaining concrete terminator differences as source facts.
fn physical_line_correspondence(pair: &ProjectionPair<'_, '_>) -> PhysicalLineFacts {
    physical_line_correspondence_in(
        pair,
        0..pair.before.source.lines().len(),
        0..pair.after.source.lines().len(),
    )
}

/// Align physical content inside one already-corresponding semantic scope.
fn physical_line_correspondence_in(
    pair: &ProjectionPair<'_, '_>,
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
    for anchor in anchors.into_iter().chain(std::iter::once(OrderedMatch::new(
        before_text.len(),
        after_text.len(),
    ))) {
        let before_anchor = before_bounds.start + anchor.before;
        let after_anchor = after_bounds.start + anchor.after;
        let before_gap = &before.source.lines()[before_start..before_anchor];
        let after_gap = &after.source.lines()[after_start..after_anchor];
        // Unequal gaps cannot gain row correspondence merely because a
        // terminator differs at the same ordinal offset.
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

/// Separate scoped physical anchors from exact payload recurring in another semantic owner.
fn scoped_physical_line_correspondence(
    pair: &ProjectionPair<'_, '_>,
    graph: &Correspondence,
    scope_proof: &ScopeProof,
    global: &PhysicalLineFacts,
) -> PhysicalLineFacts {
    let mut scoped = PhysicalLineFacts {
        missing_terminators: global.missing_terminators.clone(),
        ..PhysicalLineFacts::default()
    };
    let semantically_reordered_lines = semantically_reordered_line_links(pair, graph);
    let before_candidates = pair
        .before
        .source
        .lines()
        .iter()
        .enumerate()
        .filter_map(|(line, source)| {
            Some((
                line,
                ScopedLineValue {
                    text: pair.before.source.text(source),
                    scopes: scope_proof.stable_before_line_scopes(line)?,
                },
            ))
        })
        .collect::<Vec<_>>();
    let after_candidates = pair
        .after
        .source
        .lines()
        .iter()
        .enumerate()
        .filter_map(|(line, source)| {
            Some((
                line,
                ScopedLineValue {
                    text: pair.after.source.text(source),
                    scopes: scope_proof.stable_after_line_scopes(line)?,
                },
            ))
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
    let before_text = remaining_before
        .iter()
        .map(|(_, line)| pair.before.source.text(line))
        .collect::<Vec<_>>();
    let after_text = remaining_after
        .iter()
        .map(|(_, line)| pair.after.source.text(line))
        .collect::<Vec<_>>();
    for edge in ordered_matches(&before_text, &after_text) {
        let link = LineLink {
            before: remaining_before[edge.before].0,
            after: remaining_after[edge.after].0,
        };
        if pair.before.source.lines()[link.before].ending
            != pair.after.source.lines()[link.after].ending
        {
            continue;
        }
        if claimed_before.contains(&link.before) || claimed_after.contains(&link.after) {
            continue;
        }
        if semantically_reordered_lines.contains(&link) {
            continue;
        }
        let stable_scope = scope_proof.line_has_scoped_cover(link, true);
        if stable_scope {
            claimed_before.insert(link.before);
            claimed_after.insert(link.after);
            scoped.exact.push(link);
        }
    }
    for link in &global.ending_edits {
        if claimed_before.contains(&link.before) || claimed_after.contains(&link.after) {
            continue;
        }
        if semantically_reordered_lines.contains(link) {
            continue;
        }
        if scope_proof.line_has_scoped_cover(*link, true) {
            scoped.ending_edits.push(*link);
        }
    }
    sort_and_deduplicate_line_links(&mut scoped.exact);
    sort_and_deduplicate_line_links(&mut scoped.ending_edits);
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

/// Collect reordered leaf rows whose physical equality cannot erase semantic movement.
fn semantically_reordered_line_links(
    pair: &ProjectionPair<'_, '_>,
    graph: &Correspondence,
) -> HashSet<LineLink> {
    let mut lines = HashSet::new();
    for link in &graph.leaf_links {
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

/// Whether one subtree owns every non-layout token on its first and last rows.
///
/// Grammar delimiters such as a trailing comma or semicolon follow the preceding semantic
/// subtree, so they may extend its physical provenance without becoming correspondence.
fn node_owns_complete_lines(projection: &Projection<'_>, node: NodeId) -> bool {
    if node_is_line_isolated(projection, node) {
        return true;
    }
    let node_ref = projection.node(node);
    let Some(first) = projection.source.line(node_ref.lines.start) else {
        return false;
    };
    let Some(last_number) = node_ref.lines.end.checked_sub(1) else {
        return false;
    };
    let Some(last) = projection.source.line(last_number) else {
        return false;
    };
    if node_ref.bytes.start < first.content_bytes.start
        || node_ref.bytes.end > last.content_bytes.end
    {
        return false;
    }
    boundary_is_owned(
        projection,
        node,
        first.content_bytes.start..node_ref.bytes.start,
    ) && boundary_is_owned(projection, node, node_ref.bytes.end..last.content_bytes.end)
}

fn boundary_is_owned(projection: &Projection<'_>, owner: NodeId, bytes: Range<usize>) -> bool {
    let mut cursor = bytes.start;
    for leaf in projection.leaf_ids_in(bytes.clone()) {
        let node = projection.node(leaf);
        let start = node.bytes.start.max(bytes.start);
        let end = node.bytes.end.min(bytes.end);
        let Some(gap) = projection.source.slice(cursor..start) else {
            return false;
        };
        if !horizontal_layout(gap) {
            return false;
        }
        if is_layout_leaf(projection, leaf) {
            let Some(layout) = projection.source.slice(start..end) else {
                return false;
            };
            if !horizontal_layout(layout) {
                return false;
            }
            cursor = end;
            continue;
        }
        if node.bytes.start < bytes.start || node.bytes.end > bytes.end {
            return false;
        }
        let owned_delimiter = node.leaf.is_some_and(|leaf| leaf.delimiter)
            && trailing_delimiter_owner(projection, leaf) == Some(owner);
        if !owned_delimiter {
            return false;
        }
        cursor = end;
    }
    projection
        .source
        .slice(cursor..bytes.end)
        .is_some_and(horizontal_layout)
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

/// Retain syntax everywhere except concrete physical regions it cannot own exactly.
fn local_line_fallbacks(
    pair: &ProjectionPair<'_, '_>,
    graph: &Correspondence,
    mut fallbacks: Vec<LineFallback>,
) -> Vec<LineFallback> {
    // A move producer can own an unmatched EOF terminator without borrowing
    // the empty global-LCS coordinate on the opposite revision.
    fallbacks.retain(|fallback| !move_owns_missing_terminator(pair, graph, fallback));
    let units = graph
        .units
        .iter()
        .map(|edit| unit_line_geometry(pair, graph, edit))
        .collect::<Vec<_>>();
    let mut before_claims = vec![0_u16; pair.before.source.lines().len()];
    let mut after_claims = vec![0_u16; pair.after.source.lines().len()];
    for unit in units
        .iter()
        .filter(|unit| unit.changed || unit_lines_are_source_equal(pair, unit))
    {
        increment_claims(&mut before_claims, unit.before.clone());
        increment_claims(&mut after_claims, unit.after.clone());
    }
    claim_paired_adjacent_layout(pair, graph, &mut before_claims, &mut after_claims);

    let checkpoints = physical_line_checkpoints(graph);
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

    for link in &graph.line_ending_edits {
        if move_owns_line_ending(pair, graph, *link) {
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
    pair: &ProjectionPair<'_, '_>,
    graph: &Correspondence,
    fallback: &LineFallback,
) -> bool {
    if fallback.before.is_empty() == fallback.after.is_empty() {
        return false;
    }
    graph.units.iter().any(|edit| {
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

/// Exact reordered units render their own concrete terminator facts beside the move.
fn move_owns_line_ending(
    pair: &ProjectionPair<'_, '_>,
    graph: &Correspondence,
    link: LineLink,
) -> bool {
    graph.units.iter().any(|edit| {
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
    pair: &ProjectionPair<'_, '_>,
    graph: &Correspondence,
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
    geometry.expands_fallback &= !unit_lines_are_physically_paired(graph, &geometry);
    geometry
}

fn unit_lines_are_physically_paired(graph: &Correspondence, unit: &UnitLineGeometry) -> bool {
    if unit.before.len() != unit.after.len() {
        return false;
    }
    let mut pairs = graph
        .line_links_in(unit.before.clone(), unit.after.clone())
        .chain(graph.line_ending_edits_in(unit.before.clone(), unit.after.clone()))
        .collect::<Vec<_>>();
    pairs.sort_unstable_by_key(|link| (link.before, link.after));
    pairs.len() == unit.before.len()
        && pairs.iter().enumerate().all(|(offset, link)| {
            link.before == unit.before.start + offset && link.after == unit.after.start + offset
        })
}

/// Stable semantic rows may bypass global monotone alignment only when their full source agrees.
fn unit_lines_are_source_equal(pair: &ProjectionPair<'_, '_>, unit: &UnitLineGeometry) -> bool {
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

fn zero_based_lines(projection: &Projection<'_>, node: NodeId) -> Range<usize> {
    let lines = projection.node(node).lines.clone();
    let start = lines
        .start
        .saturating_sub(1)
        .min(projection.source.lines().len());
    let end = lines
        .end
        .saturating_sub(1)
        .min(projection.source.lines().len());
    start.min(end)..end
}

fn increment_claims(claims: &mut [u16], lines: Range<usize>) {
    for claim in &mut claims[lines] {
        *claim = claim.saturating_add(1);
    }
}

/// Equal blank separators may travel with their matched owner without becoming edit signal.
fn claim_paired_adjacent_layout(
    pair: &ProjectionPair<'_, '_>,
    graph: &Correspondence,
    before_claims: &mut [u16],
    after_claims: &mut [u16],
) {
    for edit in &graph.units {
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
            // A moved owner may carry its separator from one physical side to the other.
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

fn adjacent_blank_lines(projection: &Projection<'_>, boundary: usize, before: bool) -> Vec<usize> {
    let mut lines = Vec::new();
    if before {
        let mut index = boundary;
        while let Some(previous) = index.checked_sub(1) {
            if !source_line_is_blank(projection, previous) {
                break;
            }
            lines.push(previous);
            index = previous;
        }
        lines.reverse();
        return lines;
    }

    let mut index = boundary;
    while source_line_is_blank(projection, index) {
        lines.push(index);
        index += 1;
    }
    lines
}

fn source_line_is_blank(projection: &Projection<'_>, index: usize) -> bool {
    projection
        .source
        .lines()
        .get(index)
        .is_some_and(|line| projection.source.text(line).trim().is_empty())
}

fn claim_equal_layout_run(
    before: &Projection<'_>,
    after: &Projection<'_>,
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

fn physical_line_checkpoints(graph: &Correspondence) -> Vec<LineLink> {
    let mut checkpoints = graph.line_links.clone();
    checkpoints.extend(&graph.line_ending_edits);
    checkpoints.sort_unstable_by_key(|link| (link.before, link.after));
    checkpoints
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

    // Ambiguous interleaving cannot be assigned locally without borrowing claimed payload.
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
        // Internal unmatched layout has no trustworthy cross-revision coordinate.
        other_gap.start + other_gap.len().saturating_mul(run.start - own_gap.start) / own_gap.len()
    };
    boundary..boundary
}

fn conflicting_unit_fallbacks(
    pair: &ProjectionPair<'_, '_>,
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

/// Close cross-revision interval components without comparing every fallback to every unit.
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

/// Either revision may connect regions whose order is interleaved in the other revision.
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
    projection: &Projection<'source>,
    fingerprints: &[NodeFingerprints],
) -> Vec<UnitRecord<'source>> {
    projection
        .review_units()
        .map(|(id, node)| {
            let fingerprint = fingerprints[id.index()];
            UnitRecord {
                id,
                kind: node.kind,
                // Leaf units use ordered exact payload matching, even if the frontend exposes
                // that payload as an identity for other graph consumers.
                identity: node
                    .leaf
                    .is_none()
                    .then(|| projection.identity_text(id))
                    .flatten(),
                atomic: node.leaf.is_some(),
                decoration_owner: node.decoration_owner,
                fingerprint,
                shape: fingerprint.shape,
                mode: node
                    .review
                    .as_ref()
                    .expect("review node owns review metadata")
                    .mode,
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

/// Decorations pair only after their semantic owners have established correspondence.
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
            .map(|index| (before[*index].kind, before[*index].shape))
            .collect::<Vec<_>>();
        let after_shapes = remaining_after
            .iter()
            .map(|index| (after[*index].kind, after[*index].shape))
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
    let mut before_groups = HashMap::<UnitKey<'_>, Vec<usize>>::new();
    for (index, unit) in before.iter().enumerate() {
        if before_match[index].is_some() || unit.decoration_owner.is_some() {
            continue;
        }
        let Some(identity) = unit.identity else {
            continue;
        };
        before_groups
            .entry(UnitKey {
                kind: unit.kind,
                identity,
            })
            .or_default()
            .push(index);
    }

    let mut after_groups = HashMap::<UnitKey<'_>, Vec<usize>>::new();
    for (index, unit) in after.iter().enumerate() {
        if after_match[index].is_some() || unit.decoration_owner.is_some() {
            continue;
        }
        let Some(identity) = unit.identity else {
            continue;
        };
        after_groups
            .entry(UnitKey {
                kind: unit.kind,
                identity,
            })
            .or_default()
            .push(index);
    }

    for (key, before_group) in before_groups {
        let Some(after_group) = after_groups.get(&key) else {
            continue;
        };

        // Container identity is local: duplicate spellings retain source-order identity
        // instead of borrowing descendant bodies to choose an occurrence.
        for (before_index, after_index) in before_group.into_iter().zip(after_group.iter().copied())
        {
            link_unit_indices(before_index, after_index, before_match, after_match);
        }
    }
}

/// Pair exact leaf-like review units before positional unit alignment.
///
/// Atomic units have no descendant container whose payload could vote for a parent match.
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

/// Align unclaimed review units by local kind and mode inside established anchor gaps.
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
        for edge in compatible_unit_matches(before, after, &before_indices, &after_indices) {
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

fn compatible_unit_matches(
    before: &[UnitRecord<'_>],
    after: &[UnitRecord<'_>],
    before_indices: &[usize],
    after_indices: &[usize],
) -> Vec<OrderedMatch> {
    let before_values = before_indices
        .iter()
        .map(|index| (before[*index].kind, before[*index].mode))
        .collect::<Vec<_>>();
    let after_values = after_indices
        .iter()
        .map(|index| (after[*index].kind, after[*index].mode))
        .collect::<Vec<_>>();
    ordered_matches(&before_values, &after_values)
}

/// Retain reciprocal unique-best edges whose evidence clears one strict threshold.
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
            .then(|| OrderedMatch::new(before, after))
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
    _after_match: &[Option<usize>],
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
            let mode = ReviewMode::reconcile(before[before_index].mode, after[after_index].mode);
            mode.tracks_movement()
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

    // Non-structural streams do not vote on movement among structural review units.
    for (before_index, after_index) in before_match.iter().enumerate() {
        let Some(after_index) = *after_index else {
            continue;
        };
        let mode = ReviewMode::reconcile(before[before_index].mode, after[after_index].mode);
        if !mode.tracks_movement() {
            stable[before_index] = true;
        }
    }

    // A paired review root is the graph frame, not a movable child occurrence.
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
            // Root-owned inner documentation follows the stable graph frame,
            // which is not itself necessarily promoted to a review unit.
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

/// Non-crossing anchors used only to serialize the merged edit script.
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
    // Stable decorations may serialize inside the semantic anchors' gaps, but
    // never participate in choosing those anchors.
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

/// Stateful graph assembly keeps mutation local while projection facts stay immutable.
struct CorrespondenceBuilder<'input, 'before, 'after> {
    pair: &'input ProjectionPair<'before, 'after>,
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
    scopes: Vec<ScopeLink>,
    graph: Correspondence,
}

impl CorrespondenceBuilder<'_, '_, '_> {
    fn build(mut self) -> (Correspondence, Vec<ScopeLink>) {
        self.graph.units = self.unit_script();
        (self.graph, self.scopes)
    }

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
                self.push_after_unit(&mut edits, after_index);
            }

            if before_anchor < self.before_units.len() && after_anchor < self.after_units.len() {
                self.push_matched_unit(&mut edits, before_anchor, after_anchor);
            }
            before_start = before_anchor.saturating_add(1);
            after_start = after_anchor.saturating_add(1);
        }
        edits
    }

    fn push_after_unit(&mut self, edits: &mut Vec<UnitEdit>, after_index: usize) {
        let Some(before_index) = self.after_match[after_index] else {
            edits.push(UnitEdit::Added {
                after: self.after_units[after_index].id,
            });
            return;
        };

        self.push_matched_unit(edits, before_index, after_index);
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
            .expect("projection node source geometry remains valid");
        let after_source = self
            .pair
            .after
            .source
            .slice(after_node.bytes.clone())
            .expect("projection node source geometry remains valid");
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
        let leaf_start = self.graph.leaf_links.len();
        let composite_start = self.graph.composites.len();
        self.link_unit_contents(before, after, placement);
        let mode = ReviewMode::reconcile(
            self.before_units[before_index].mode,
            self.after_units[after_index].mode,
        );
        let line_start = self.graph.unit_line_links.len();
        if placement == Placement::Reordered && relation.full_equal() {
            let mut lines = physical_line_correspondence_in(
                self.pair,
                zero_based_lines(&self.pair.before, before),
                zero_based_lines(&self.pair.after, after),
            );
            self.graph.unit_line_links.append(&mut lines.exact);
            self.graph.unit_line_links.append(&mut lines.ending_edits);
            self.graph.unit_line_links[line_start..]
                .sort_unstable_by_key(|link| (link.before, link.after));
        }

        edits.push(UnitEdit::Matched(MatchedUnit {
            before,
            after,
            mode,
            relation,
            placement,
            line_links: line_start..self.graph.unit_line_links.len(),
            leaf_links: leaf_start..self.graph.leaf_links.len(),
            composites: composite_start..self.graph.composites.len(),
        }));
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct ExactLeafKey {
    field: Option<&'static str>,
    fingerprint: FingerprintId,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum DelimiterOwnerKey {
    Absent,
    Composite(NodeId),
    Leaf(ExactLeafKey),
    UnmatchedBefore,
    UnmatchedAfter,
}

impl DelimiterOwnerKey {
    /// Keep leaf atoms indivisible globally; composite wrappers may still change around them.
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
    leaf: ExactLeafKey,
    parent: ParentSlot,
    trailing_delimiter_owner: DelimiterOwnerKey,
    decorated: bool,
    decoration_owner: Option<NodeId>,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct LeafShape {
    kind: &'static str,
    field: Option<&'static str>,
    channel: ContentChannel,
    named: bool,
    extra: bool,
    missing: bool,
}

/// Actual same-context composite pairing used to judge parent changes.
struct ContextLinks {
    before: HashMap<NodeId, NodeId>,
    after: HashMap<NodeId, NodeId>,
    placement: HashMap<NodeId, Placement>,
    /// Before-side contexts retained through a proven transparent wrapper transition.
    reparenting: HashMap<NodeId, Reparenting>,
}

/// Parent correspondence inside one matched review unit.
struct UnitContext<'input, 'before, 'after> {
    pair: &'input ProjectionPair<'before, 'after>,
    parents: &'input ContextLinks,
    before_unit: NodeId,
    after_unit: NodeId,
}

impl UnitContext<'_, '_, '_> {
    fn parents_are_linked(&self, before: NodeId, after: NodeId) -> bool {
        let before_node = self.pair.before.node(before);
        let after_node = self.pair.after.node(after);
        if before_node.field != after_node.field {
            return false;
        }
        if before == self.before_unit && after == self.after_unit {
            return true;
        }

        let (Some(before_parent), Some(after_parent)) = (before_node.parent, after_node.parent)
        else {
            return before_node.parent.is_none() && after_node.parent.is_none();
        };
        self.parents.before.get(&before_parent).copied() == Some(after_parent)
    }

    fn desired_after_parent(&self, before: NodeId) -> Option<ParentSlot> {
        if before == self.before_unit {
            return Some(ParentSlot::Unit);
        }
        let parent = self.pair.before.node(before).parent?;
        self.parents
            .before
            .get(&parent)
            .copied()
            .map(ParentSlot::Node)
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
        leaf_keys: &HashMap<NodeId, ExactLeafKey>,
    ) -> DelimiterOwnerKey {
        let Some(owner) = trailing_delimiter_owner(&self.pair.before, before) else {
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
            .copied()
            .map(DelimiterOwnerKey::Composite)
            .unwrap_or(DelimiterOwnerKey::UnmatchedBefore)
    }

    fn after_trailing_delimiter_owner(
        &self,
        after: NodeId,
        leaf_keys: &HashMap<NodeId, ExactLeafKey>,
    ) -> DelimiterOwnerKey {
        let Some(owner) = trailing_delimiter_owner(&self.pair.after, after) else {
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
        self.parents
            .reparenting
            .get(&before)
            .copied()
            .or_else(|| {
                let parent = self.pair.before.node(before).parent?;
                self.parents.reparenting.get(&parent).copied()
            })
            .or_else(|| unique_containment_reparenting(self.pair, self.parents, before, after))
            .map(ParentCorrespondence::Reparented)
    }

    fn enclosing_wrapper(&self, before: NodeId, after: NodeId) -> Option<Reparenting> {
        self.parents
            .reparenting
            .get(&before)
            .copied()
            .or_else(|| {
                let parent = self.pair.before.node(before).parent?;
                self.parents.reparenting.get(&parent).copied()
            })
            .or_else(|| unique_containment_reparenting(self.pair, self.parents, before, after))
    }

    fn desired_after_decoration_owner(&self, before: NodeId) -> Option<NodeId> {
        let owner = self.pair.before.node(before).decoration_owner?;
        if owner == self.before_unit {
            return Some(self.after_unit);
        }
        self.parents.before.get(&owner).copied()
    }

    fn after_decoration_owner(&self, after: NodeId) -> Option<NodeId> {
        self.pair.after.node(after).decoration_owner
    }

    fn decoration_placement(&self, before: NodeId) -> Option<Placement> {
        let owner = self.pair.before.node(before).decoration_owner?;
        self.parents.placement.get(&owner).copied()
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct ContextShape {
    kind: &'static str,
    field: Option<&'static str>,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct ContextIdentity<'source> {
    kind: &'static str,
    field: Option<&'static str>,
    identity: &'source str,
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct AnonymousContextKey<'source> {
    shape: ContextShape,
    identities: Vec<ContextIdentity<'source>>,
}

impl CorrespondenceBuilder<'_, '_, '_> {
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

        // Recursive propagation chooses the authoritative non-overlapping cover. Repeated
        // leaf-shaped composites can otherwise acquire plausible FIFO pairings that disagree
        // with the larger exact subtrees containing them.
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
            mark_subtree(&pair.before, before, &mut covered_before);
            mark_subtree(&pair.after, after, &mut covered_after);
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
            .filter_map(|(before, after)| {
                if *before != before_unit && !pair.before.node(*before).is_scope_boundary() {
                    return None;
                }
                if exact_cover.contains_key(before) || exact_cover_after.contains_key(after) {
                    return None;
                }
                let wrapper = unique_containment_reparenting(pair, &parents, *before, *after);
                let parent = wrapper
                    .map(ParentCorrespondence::Reparented)
                    .unwrap_or(ParentCorrespondence::Direct);
                Some(ScopeLink {
                    before: *before,
                    after: *after,
                    placement: parents
                        .placement
                        .get(before)
                        .copied()
                        .expect("a linked scope owns placement"),
                    parent,
                })
            })
            .collect::<Vec<_>>();
        scopes.sort_unstable_by_key(|link| link.before);
        let scope_start = self.scopes.len();
        for scope in scopes {
            if self.push_scope_link(scope) {
                continue;
            }
            for link in self.scopes.drain(scope_start..) {
                self.before_scope[link.before.index()] = None;
                self.after_scope[link.after.index()] = None;
            }
            return;
        }

        // Nested composite facts remain useful only when they agree with that exact cover.
        // Recompute placement after discarding speculative duplicate-occurrence pairings.
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
        // The dense composite script is the placement authority. A sparse maximal cover can
        // otherwise call an exact definition reordered because adjacent repeated syntax crossed.
        for edge in maximal {
            let before = before_composites[edge.before];
            let after = after_composites[edge.after];
            let link = exact_roots
                .get(&before)
                .expect("every maximal exact root survives its own cover");
            self.link_exact_subtree(before, after, link.placement, link.parent, link.wrapper);
        }
        self.graph.composites.extend(exact_links);

        let before_leaves = descendant_leaves(&pair.before, before_unit)
            .into_iter()
            .filter(|id| self.graph.before_leaf[id.index()].is_none())
            .collect::<Vec<_>>();
        let after_leaves = descendant_leaves(&pair.after, after_unit)
            .into_iter()
            .filter(|id| self.graph.after_leaf[id.index()].is_none())
            .collect::<Vec<_>>();
        let before_exact = before_leaves
            .iter()
            .map(|id| ExactLeafKey {
                field: pair.before.node(*id).field,
                fingerprint: self.before_fingerprints[id.index()].full,
            })
            .collect::<Vec<_>>();
        let after_exact = after_leaves
            .iter()
            .map(|id| ExactLeafKey {
                field: pair.after.node(*id).field,
                fingerprint: self.after_fingerprints[id.index()].full,
            })
            .collect::<Vec<_>>();
        let exact_leaves = exact_leaf_matches(
            &context,
            LeafCandidates {
                nodes: &before_leaves,
                keys: &before_exact,
            },
            LeafCandidates {
                nodes: &after_leaves,
                keys: &after_exact,
            },
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
            .filter(|id| self.graph.before_leaf[id.index()].is_none())
            .collect::<Vec<_>>();
        let after_remaining = after_leaves
            .into_iter()
            .filter(|id| self.graph.after_leaf[id.index()].is_none())
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
        // Exact content may prove reparenting; shape-only edits need a parent pair
        // established by the contextual composite walk above.
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

struct LeafCandidates<'input> {
    nodes: &'input [NodeId],
    keys: &'input [ExactLeafKey],
}

/// Exact composite pairs must agree with the recursive context walk.
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
    let mut before_match = vec![None; before.len()];
    let mut after_match = vec![None; after.len()];
    for (before_index, before_id) in before.iter().copied().enumerate() {
        let Some(after_id) = context.parents.before.get(&before_id) else {
            continue;
        };
        let Some(after_index) = after_indices.get(after_id).copied() else {
            continue;
        };
        if before_fingerprints[before_id.index()].full != after_fingerprints[after_id.index()].full
        {
            continue;
        }
        debug_assert!(after_match[after_index].is_none());
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
    for edge in reciprocal_unique_matches(
        wrapped_before.len(),
        wrapped_after.len(),
        0,
        |left, right| {
            let before = before[wrapped_before[left]];
            let after = after[wrapped_after[right]];
            let before_node = context.pair.before.node(before);
            let after_node = context.pair.after.node(after);
            u64::from(
                before_node.kind == after_node.kind
                    && before_fingerprints[before.index()].full
                        == after_fingerprints[after.index()].full
                    && unique_containment_reparenting(context.pair, context.parents, before, after)
                        .is_some(),
            )
        },
    ) {
        let before_index = wrapped_before[edge.before];
        let after_index = wrapped_after[edge.after];
        before_match[before_index] = Some(after_index);
        after_match[after_index] = Some(before_index);
    }

    before_match
        .into_iter()
        .enumerate()
        .filter_map(|(before, after)| after.map(|after| OrderedMatch::new(before, after)))
        .collect()
}

fn exact_leaf_matches(
    context: &UnitContext<'_, '_, '_>,
    before: LeafCandidates<'_>,
    after: LeafCandidates<'_>,
) -> Vec<OrderedMatch> {
    let mut before_match = vec![None; before.nodes.len()];
    let mut after_match = vec![None; after.nodes.len()];
    let before_keys = before
        .nodes
        .iter()
        .copied()
        .zip(before.keys.iter().copied())
        .collect::<HashMap<_, _>>();
    let after_keys = after
        .nodes
        .iter()
        .copied()
        .zip(after.keys.iter().copied())
        .collect::<HashMap<_, _>>();

    // Same-parent occurrences win before the remaining exact payloads pair globally.
    let mut contextual_after = HashMap::<ContextualLeafKey, VecDeque<usize>>::new();
    for (after_index, after_id) in after.nodes.iter().copied().enumerate() {
        let Some(parent) = context.after_parent(after_id) else {
            continue;
        };
        contextual_after
            .entry(ContextualLeafKey {
                leaf: after.keys[after_index],
                parent,
                trailing_delimiter_owner: context
                    .after_trailing_delimiter_owner(after_id, &after_keys),
                decorated: context.pair.after.node(after_id).decoration_owner.is_some(),
                decoration_owner: context.after_decoration_owner(after_id),
            })
            .or_default()
            .push_back(after_index);
    }
    for (before_index, (before_id, before_key)) in before
        .nodes
        .iter()
        .copied()
        .zip(before.keys.iter().copied())
        .enumerate()
    {
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

    let wrapped_before = (0..before.nodes.len())
        .filter(|index| {
            before_match[*index].is_none()
                && context
                    .pair
                    .before
                    .node(before.nodes[*index])
                    .decoration_owner
                    .is_none()
        })
        .collect::<Vec<_>>();
    let wrapped_after = (0..after.nodes.len())
        .filter(|index| {
            after_match[*index].is_none()
                && context
                    .pair
                    .after
                    .node(after.nodes[*index])
                    .decoration_owner
                    .is_none()
        })
        .collect::<Vec<_>>();
    for edge in reciprocal_unique_matches(
        wrapped_before.len(),
        wrapped_after.len(),
        0,
        |left, right| {
            let before_index = wrapped_before[left];
            let after_index = wrapped_after[right];
            let exact =
                before.keys[before_index].fingerprint == after.keys[after_index].fingerprint;
            let reparenting = unique_containment_reparenting(
                context.pair,
                context.parents,
                before.nodes[before_index],
                after.nodes[after_index],
            );
            u64::from(exact && reparenting.is_some())
        },
    ) {
        let before_index = wrapped_before[edge.before];
        let after_index = wrapped_after[edge.after];
        before_match[before_index] = Some(after_index);
        after_match[after_index] = Some(before_index);
    }

    let remaining_before = (0..before.nodes.len())
        .filter(|index| {
            before_match[*index].is_none()
                && context
                    .pair
                    .before
                    .node(before.nodes[*index])
                    .decoration_owner
                    .is_none()
        })
        .collect::<Vec<_>>();
    let remaining_after = (0..after.nodes.len())
        .filter(|index| {
            after_match[*index].is_none()
                && context
                    .pair
                    .after
                    .node(after.nodes[*index])
                    .decoration_owner
                    .is_none()
        })
        .collect::<Vec<_>>();
    let before_values = remaining_before
        .iter()
        .map(|index| {
            (
                context.desired_after_parent(before.nodes[*index]),
                before.keys[*index],
                context
                    .desired_after_trailing_delimiter_owner(before.nodes[*index], &before_keys)
                    .for_global_match(),
            )
        })
        .collect::<Vec<_>>();
    let after_values = remaining_after
        .iter()
        .map(|index| {
            (
                context.after_parent(after.nodes[*index]),
                after.keys[*index],
                context
                    .after_trailing_delimiter_owner(after.nodes[*index], &after_keys)
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
        .filter_map(|(before, after)| after.map(|after| OrderedMatch::new(before, after)))
        .collect()
}

/// Modified documentation leaves require both their semantic owner and direct parent to match.
fn decorated_leaf_matches(
    context: &UnitContext<'_, '_, '_>,
    before: &[NodeId],
    after: &[NodeId],
    before_shapes: &[LeafShape],
    after_shapes: &[LeafShape],
) -> Vec<OrderedMatch> {
    let mut after_groups = HashMap::<(NodeId, ParentSlot, LeafShape), VecDeque<usize>>::new();
    for (index, id) in after.iter().copied().enumerate() {
        let Some(owner) = context.after_decoration_owner(id) else {
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
            Some(OrderedMatch::new(before, after))
        })
        .collect()
}

fn descendant_composites(projection: &Projection<'_>, root: NodeId) -> Vec<NodeId> {
    projection
        .descendants(root)
        .filter(|id| {
            let node = projection.node(*id);
            node.named && node.leaf.is_none()
        })
        .collect()
}

fn descendant_leaves(projection: &Projection<'_>, root: NodeId) -> Vec<NodeId> {
    let root_node = projection.node(root);
    if root_node.leaf.is_some() && !is_layout_leaf(projection, root) {
        return vec![root];
    }
    projection
        .descendants(root)
        .filter(|id| projection.node(*id).leaf.is_some() && !is_layout_leaf(projection, *id))
        .collect()
}

fn contextual_links(
    pair: &ProjectionPair<'_, '_>,
    before_unit: NodeId,
    after_unit: NodeId,
    unit_placement: Placement,
    before_fingerprints: &[NodeFingerprints],
    after_fingerprints: &[NodeFingerprints],
) -> ContextLinks {
    let mut links = ContextLinks {
        before: HashMap::new(),
        after: HashMap::new(),
        placement: HashMap::new(),
        reparenting: HashMap::new(),
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
        let inherited_reparenting = links.reparenting.get(&before_parent).copied();
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
    // Deferring renamed-owner guesses; certified containment claims the occurrence first.
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
    links
}

/// Recover a changed semantic subtree along a uniquely certified containment spine.
///
/// Same identity and reciprocal exact descendant evidence are both required. The outermost
/// confident roots win so their children can be aligned inside the recovered context.
fn confident_reparented_context_matches(
    pair: &ProjectionPair<'_, '_>,
    before_unit: NodeId,
    after_unit: NodeId,
    links: &ContextLinks,
    before_fingerprints: &[NodeFingerprints],
    after_fingerprints: &[NodeFingerprints],
) -> Vec<(NodeId, NodeId, Placement, Reparenting)> {
    let before = descendant_composites(&pair.before, before_unit)
        .into_iter()
        .filter(|id| {
            !links.before.contains_key(id)
                && pair.before.identity_text(*id).is_some()
                && pair.before.node(*id).decoration_owner.is_none()
        })
        .collect::<Vec<_>>();
    let after = descendant_composites(&pair.after, after_unit)
        .into_iter()
        .filter(|id| {
            !links.after.contains_key(id)
                && pair.after.identity_text(*id).is_some()
                && pair.after.node(*id).decoration_owner.is_none()
        })
        .collect::<Vec<_>>();
    if before.is_empty() || after.is_empty() {
        return Vec::new();
    }

    let before_payload = before
        .iter()
        .map(|id| meaningful_payload_fingerprints(&pair.before, before_fingerprints, *id))
        .collect::<Vec<_>>();
    let after_payload = after
        .iter()
        .map(|id| meaningful_payload_fingerprints(&pair.after, after_fingerprints, *id))
        .collect::<Vec<_>>();
    let mut before_groups = HashMap::<(&str, &str), Vec<usize>>::new();
    for (index, id) in before.iter().copied().enumerate() {
        before_groups
            .entry((
                pair.before.node(id).kind,
                pair.before
                    .identity_text(id)
                    .expect("reparenting candidates carry an identity"),
            ))
            .or_default()
            .push(index);
    }
    let mut after_groups = HashMap::<(&str, &str), Vec<usize>>::new();
    for (index, id) in after.iter().copied().enumerate() {
        after_groups
            .entry((
                pair.after.node(id).kind,
                pair.after
                    .identity_text(id)
                    .expect("reparenting candidates carry an identity"),
            ))
            .or_default()
            .push(index);
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
                let parents_already_linked = left_node
                    .parent
                    .zip(right_node.parent)
                    .is_some_and(|(left, right)| links.before.get(&left).copied() == Some(right));
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
            .map(|edge| OrderedMatch::new(before_group[edge.before], after_group[edge.after])),
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
    pair: &ProjectionPair<'_, '_>,
    links: &ContextLinks,
    before: NodeId,
    after: NodeId,
) -> Option<Reparenting> {
    unique_containment_reparenting_with(
        pair,
        before,
        after,
        |candidate| links.before.get(&candidate).copied(),
        |candidate| links.after.get(&candidate).copied(),
    )
}

/// Prove that one paired occurrence is carried through an unmatched containment spine.
///
/// The first already-paired owner must agree on both revisions. Unmatched ancestors may be
/// arbitrarily deep, but no sibling branch may carry another paired owner or review boundary.
/// Exactly one revision must contribute the unmatched spine: replacing one wrapper hierarchy
/// with another remains an ordinary structural edit rather than a one-sided wrap or unwrap.
fn unique_containment_reparenting_with(
    pair: &ProjectionPair<'_, '_>,
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

/// Climb to one designated owner through a single correspondence-bearing child branch.
fn unique_containment_path(
    projection: &Projection<'_>,
    candidate: NodeId,
    is_anchor: &impl Fn(NodeId) -> bool,
    is_fence: &impl Fn(NodeId) -> bool,
) -> Option<(NodeId, bool)> {
    let mut branch = candidate;
    let mut candidate = projection.node(candidate).parent?;
    let mut crossed_unmatched_parent = false;
    loop {
        // A paired owner is the breadth fence itself; its other children are outside this proof.
        if is_anchor(candidate) && is_fence(candidate) {
            return Some((candidate, crossed_unmatched_parent));
        }

        let node = projection.node(candidate);
        if node.is_hard_owner()
            || node
                .children
                .iter()
                .copied()
                .any(|child| child != branch && subtree_contains_fence(projection, child, is_fence))
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
    projection: &Projection<'_>,
    root: NodeId,
    is_fence: &impl Fn(NodeId) -> bool,
) -> bool {
    std::iter::once(root)
        .chain(projection.descendants(root))
        .any(|candidate| {
            let node = projection.node(candidate);
            is_fence(candidate)
                || node.is_hard_owner()
                || node.review.is_some()
                || node.decoration_owner.is_some()
        })
}

/// Find exactly one matching descendant reachable along an unpaired containment spine.
fn unique_contained_descendant_matches(
    projection: &Projection<'_>,
    outer: NodeId,
    is_fence: &impl Fn(NodeId) -> bool,
    mut matches: impl FnMut(NodeId) -> bool,
) -> bool {
    let is_outer = |candidate| candidate == outer;
    let mut retained = None;
    for candidate in projection.descendants(outer) {
        let node = projection.node(candidate);
        if !node.named || node.leaf.is_some() || !matches(candidate) {
            continue;
        }
        if unique_containment_path(projection, candidate, &is_outer, is_fence).is_none() {
            continue;
        }
        if retained.replace(candidate).is_some() {
            return false;
        }
    }
    retained.is_some()
}

/// Collect exact non-delimiter payload below an identity-bearing context.
fn meaningful_payload_fingerprints(
    projection: &Projection<'_>,
    fingerprints: &[NodeFingerprints],
    root: NodeId,
) -> Vec<FingerprintId> {
    let identity = projection.identity_text(root);
    let mut payload = descendant_leaves(projection, root)
        .into_iter()
        .filter(|id| {
            let node = projection.node(*id);
            let Some(leaf) = node.leaf else {
                return false;
            };
            if leaf.delimiter
                || matches!(
                    leaf.channel,
                    ContentChannel::Comment | ContentChannel::Layout
                )
                || !matches!(
                    leaf.syntax,
                    SyntaxClass::Plain | SyntaxClass::Literal | SyntaxClass::String
                )
            {
                return false;
            }
            let text = projection.leaf_text(*id);
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
    pair: &ProjectionPair<'_, '_>,
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
        .map(|id| context.after.contains_key(id))
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
                    &|candidate| context.after.contains_key(&candidate),
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
                .correspondence
                != CorrespondenceRole::Transparent
        })
        .collect::<Vec<_>>();
    let local_after = (0..identified_after.len())
        .filter(|index| {
            pair.after
                .node(after[identified_after[*index]])
                .correspondence
                != CorrespondenceRole::Transparent
        })
        .collect::<Vec<_>>();
    let before_local_values = local_before
        .iter()
        .map(|index| {
            let id = before[identified_before[*index]];
            (
                pair.before.node(id).correspondence,
                context_identity(&pair.before, id),
            )
        })
        .collect::<Vec<_>>();
    let after_local_values = local_after
        .iter()
        .map(|index| {
            let id = after[identified_after[*index]];
            (
                pair.after.node(id).correspondence,
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

    let transparent_before = (0..identified_before.len())
        .filter(|index| {
            let candidate = identified_before[*index];
            before_match[candidate].is_none()
                && pair.before.node(before[candidate]).correspondence
                    == CorrespondenceRole::Transparent
        })
        .collect::<Vec<_>>();
    let transparent_after = (0..identified_after.len())
        .filter(|index| {
            let candidate = identified_after[*index];
            after_match[candidate].is_none()
                && pair.after.node(after[candidate]).correspondence
                    == CorrespondenceRole::Transparent
        })
        .collect::<Vec<_>>();
    let before_identities = transparent_before
        .iter()
        .map(|index| context_identity(&pair.before, before[identified_before[*index]]))
        .collect::<Vec<_>>();
    let after_identities = transparent_after
        .iter()
        .map(|index| context_identity(&pair.after, after[identified_after[*index]]))
        .collect::<Vec<_>>();
    let before_exact = transparent_before
        .iter()
        .map(|index| before_fingerprints[before[identified_before[*index]].index()].full)
        .collect::<Vec<_>>();
    let after_exact = transparent_after
        .iter()
        .map(|index| after_fingerprints[after[identified_after[*index]].index()].full)
        .collect::<Vec<_>>();
    for edge in locality_preserving_sibling_matches(
        &before_identities,
        &after_identities,
        &before_exact,
        &after_exact,
    ) {
        let before_index = identified_before[transparent_before[edge.before]];
        let after_index = identified_after[transparent_after[edge.after]];
        before_match[before_index] = Some(after_index);
        after_match[after_index] = Some(before_index);
    }

    if allow_renames {
        for edge in confident_renamed_context_matches(
            pair,
            before,
            after,
            ContextClaims {
                matched: &before_match,
                reserved: &before_direct_reserved,
            },
            ContextClaims {
                matched: &after_match,
                reserved: &after_direct_reserved,
            },
        ) {
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
                .correspondence
                != CorrespondenceRole::Transparent
        })
        .collect::<Vec<_>>();
    let local_after = (0..anonymous_after.len())
        .filter(|index| {
            pair.after
                .node(after[anonymous_after[*index]])
                .correspondence
                != CorrespondenceRole::Transparent
        })
        .collect::<Vec<_>>();
    let before_local_values = local_before
        .iter()
        .map(|index| {
            let id = before[anonymous_before[*index]];
            (
                pair.before.node(id).correspondence,
                context_shape(&pair.before, id),
            )
        })
        .collect::<Vec<_>>();
    let after_local_values = local_after
        .iter()
        .map(|index| {
            let id = after[anonymous_after[*index]];
            (
                pair.after.node(id).correspondence,
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
                    .correspondence
                    == CorrespondenceRole::Transparent
        })
        .collect::<Vec<_>>();
    let keyed_after = (0..anonymous_after.len())
        .filter(|index| {
            !after_keys[*index].identities.is_empty()
                && after_match[anonymous_after[*index]].is_none()
                && pair
                    .after
                    .node(after[anonymous_after[*index]])
                    .correspondence
                    == CorrespondenceRole::Transparent
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
    let before_exact = keyed_before
        .iter()
        .map(|index| before_fingerprints[before[anonymous_before[*index]].index()].full)
        .collect::<Vec<_>>();
    let after_exact = keyed_after
        .iter()
        .map(|index| after_fingerprints[after[anonymous_after[*index]].index()].full)
        .collect::<Vec<_>>();
    for edge in locality_preserving_sibling_matches(
        &before_keyed_values,
        &after_keyed_values,
        &before_exact,
        &after_exact,
    ) {
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
                && pair.before.node(before[*index]).correspondence
                    == CorrespondenceRole::Transparent
        })
        .collect::<Vec<_>>();
    let remaining_after = anonymous_after
        .iter()
        .copied()
        .filter(|index| {
            after_match[*index].is_none()
                && pair.after.node(after[*index]).correspondence == CorrespondenceRole::Transparent
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
        .filter_map(|(before, after)| after.map(|after| OrderedMatch::new(before, after)))
        .collect()
}

/// Match one sibling layer without letting a copied payload displace its local occurrence.
///
/// Exact payload decides identity only when two provisional edges cross, which is concrete
/// sibling-reordering evidence. Otherwise equal local keys retain FIFO source identity.
fn locality_preserving_sibling_matches<K: Clone + Eq + Hash>(
    before_keys: &[K],
    after_keys: &[K],
    before_exact: &[FingerprintId],
    after_exact: &[FingerprintId],
) -> Vec<OrderedMatch> {
    debug_assert_eq!(before_keys.len(), before_exact.len());
    debug_assert_eq!(after_keys.len(), after_exact.len());

    let mut before_groups = HashMap::<K, Vec<usize>>::new();
    for (index, key) in before_keys.iter().cloned().enumerate() {
        before_groups.entry(key).or_default().push(index);
    }
    let mut after_groups = HashMap::<K, Vec<usize>>::new();
    for (index, key) in after_keys.iter().cloned().enumerate() {
        after_groups.entry(key).or_default().push(index);
    }

    let mut matches = Vec::new();
    for (key, before_group) in before_groups {
        let Some(after_group) = after_groups.get(&key) else {
            continue;
        };
        let before_values = before_group
            .iter()
            .map(|index| before_exact[*index])
            .collect::<Vec<_>>();
        let after_values = after_group
            .iter()
            .map(|index| after_exact[*index])
            .collect::<Vec<_>>();
        let mut before_counts = HashMap::new();
        for value in &before_values {
            *before_counts.entry(*value).or_insert(0_usize) += 1;
        }
        let mut after_counts = HashMap::new();
        for value in &after_values {
            *after_counts.entry(*value).or_insert(0_usize) += 1;
        }
        let exact = unordered_matches(&before_values, &after_values)
            .into_iter()
            .filter(|edge| {
                before_counts[&before_values[edge.before]] == 1
                    && after_counts[&after_values[edge.after]] == 1
            })
            .collect::<Vec<_>>();
        let mut provisional = exact
            .iter()
            .copied()
            .map(|edge| (edge, true))
            .collect::<Vec<_>>();
        let mut before_claimed = vec![false; before_group.len()];
        let mut after_claimed = vec![false; after_group.len()];
        for edge in &exact {
            before_claimed[edge.before] = true;
            after_claimed[edge.after] = true;
        }
        provisional.extend(
            before_claimed
                .iter()
                .enumerate()
                .filter_map(|(index, claimed)| (!claimed).then_some(index))
                .zip(
                    after_claimed
                        .iter()
                        .enumerate()
                        .filter_map(|(index, claimed)| (!claimed).then_some(index)),
                )
                .map(|(before, after)| (OrderedMatch::new(before, after), false)),
        );
        provisional.sort_unstable_by_key(|(edge, _)| edge.before);
        let crossing = crossing_match_members(
            &provisional
                .iter()
                .map(|(edge, _)| *edge)
                .collect::<Vec<_>>(),
        );

        before_claimed.fill(false);
        after_claimed.fill(false);
        for ((edge, exact), crossing) in provisional.into_iter().zip(crossing) {
            if !exact || !crossing {
                continue;
            }
            before_claimed[edge.before] = true;
            after_claimed[edge.after] = true;
            matches.push(OrderedMatch::new(
                before_group[edge.before],
                after_group[edge.after],
            ));
        }
        matches.extend(
            before_claimed
                .iter()
                .enumerate()
                .filter_map(|(index, claimed)| (!claimed).then_some(index))
                .zip(
                    after_claimed
                        .iter()
                        .enumerate()
                        .filter_map(|(index, claimed)| (!claimed).then_some(index)),
                )
                .map(|(before, after)| OrderedMatch::new(before_group[before], after_group[after])),
        );
    }
    matches.sort_unstable_by_key(|edge| edge.before);
    matches
}

/// Mark every edge that participates in an inversion with another edge.
fn crossing_match_members(matches: &[OrderedMatch]) -> Vec<bool> {
    let mut prefix_max = vec![None; matches.len()];
    let mut maximum = None;
    for (index, edge) in matches.iter().enumerate() {
        prefix_max[index] = maximum;
        maximum = Some(maximum.map_or(edge.after, |value: usize| value.max(edge.after)));
    }
    let mut suffix_min = vec![None; matches.len()];
    let mut minimum = None;
    for (index, edge) in matches.iter().enumerate().rev() {
        suffix_min[index] = minimum;
        minimum = Some(minimum.map_or(edge.after, |value: usize| value.min(edge.after)));
    }
    matches
        .iter()
        .enumerate()
        .map(|(index, edge)| {
            prefix_max[index].is_some_and(|after| after > edge.after)
                || suffix_min[index].is_some_and(|after| after < edge.after)
        })
        .collect()
}

struct ContextCandidates<'input> {
    nodes: &'input [NodeId],
    matched: &'input [Option<usize>],
    reserved: &'input [bool],
    fingerprints: &'input [NodeFingerprints],
}

#[derive(Clone, Copy)]
struct ContextClaims<'input> {
    matched: &'input [Option<usize>],
    reserved: &'input [bool],
}

/// Decorations are aligned only inside one already-linked semantic owner pair.
fn decoration_matches(
    pair: &ProjectionPair<'_, '_>,
    context: &ContextLinks,
    before: ContextCandidates<'_>,
    after: ContextCandidates<'_>,
) -> Vec<OrderedMatch> {
    let mut matches = Vec::new();
    let mut owner_pairs = context
        .before
        .iter()
        .map(|(before, after)| (*before, *after))
        .chain(before.matched.iter().copied().enumerate().filter_map(
            |(before_index, after_index)| {
                let after_index = after_index?;
                let after = after.nodes.get(after_index)?;
                Some((before.nodes[before_index], *after))
            },
        ))
        .collect::<Vec<_>>();
    owner_pairs.sort_unstable();
    owner_pairs.dedup();
    for (before_owner, after_owner) in owner_pairs {
        let before_group = before
            .nodes
            .iter()
            .copied()
            .enumerate()
            .filter(|(index, id)| {
                before.matched[*index].is_none()
                    && !before.reserved[*index]
                    && pair.before.node(*id).decoration_owner == Some(before_owner)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let after_group = after
            .nodes
            .iter()
            .copied()
            .enumerate()
            .filter(|(index, id)| {
                after.matched[*index].is_none()
                    && !after.reserved[*index]
                    && pair.after.node(*id).decoration_owner == Some(after_owner)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
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
            matches.push(OrderedMatch::new(
                before_group[edge.before],
                after_group[edge.after],
            ));
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
            matches.push(OrderedMatch::new(
                before_group[before_index],
                after_group[after_index],
            ));
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
            matches.push(OrderedMatch::new(
                before_group[before_remaining[edge.before]],
                after_group[after_remaining[edge.after]],
            ));
        }
    }
    matches.sort_by_key(|edge| edge.before);
    matches
}

/// Pair renamed identity-bearing siblings by local order, never by descendant payload.
fn confident_renamed_context_matches(
    pair: &ProjectionPair<'_, '_>,
    before: &[NodeId],
    after: &[NodeId],
    before_claims: ContextClaims<'_>,
    after_claims: ContextClaims<'_>,
) -> Vec<OrderedMatch> {
    let before_candidates = (0..before.len())
        .filter(|index| {
            before_claims.matched[*index].is_none()
                && !before_claims.reserved[*index]
                && pair.before.identity_text(before[*index]).is_some()
                && pair.before.node(before[*index]).decoration_owner.is_none()
        })
        .collect::<Vec<_>>();
    let after_candidates = (0..after.len())
        .filter(|index| {
            after_claims.matched[*index].is_none()
                && !after_claims.reserved[*index]
                && pair.after.identity_text(after[*index]).is_some()
                && pair.after.node(after[*index]).decoration_owner.is_none()
        })
        .collect::<Vec<_>>();
    let before_keys = before_candidates
        .iter()
        .map(|index| {
            let id = before[*index];
            (
                pair.before.node(id).kind,
                pair.before
                    .identity_text(id)
                    .expect("rename candidates carry an identity"),
            )
        })
        .collect::<HashSet<_>>();
    let after_keys = after_candidates
        .iter()
        .map(|index| {
            let id = after[*index];
            (
                pair.after.node(id).kind,
                pair.after
                    .identity_text(id)
                    .expect("rename candidates carry an identity"),
            )
        })
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
                pair.before.node(id).correspondence,
                context_shape(&pair.before, id),
            )
        })
        .collect::<Vec<_>>();
    let after_shapes = after_candidates
        .iter()
        .map(|index| {
            let id = after[*index];
            (
                pair.after.node(id).correspondence,
                context_shape(&pair.after, id),
            )
        })
        .collect::<Vec<_>>();
    ordered_matches(&before_shapes, &after_shapes)
        .into_iter()
        .map(|edge| OrderedMatch::new(before_candidates[edge.before], after_candidates[edge.after]))
        .collect()
}

fn descendant_identities<'source>(
    projection: &'source Projection<'_>,
    root: NodeId,
) -> HashSet<(&'static str, &'source str)> {
    let mut identities = HashSet::new();
    if projection.node(root).correspondence != CorrespondenceRole::Transparent {
        return identities;
    }
    let mut pending = projection.node(root).children.clone();
    while let Some(candidate) = pending.pop() {
        let node = projection.node(candidate);
        if node.correspondence != CorrespondenceRole::Transparent {
            continue;
        }
        if let Some(identity) = projection.identity_text(candidate) {
            identities.insert((node.kind, identity));
            continue;
        }
        pending.extend(node.children.iter().copied());
    }
    identities
}

fn identity_key<'source>(
    projection: &'source Projection<'source>,
    id: NodeId,
) -> (&'static str, &'source str) {
    (
        projection.node(id).kind,
        projection
            .identity_text(id)
            .expect("identified context carries source identity"),
    )
}

fn contains_foreign_descendant_identity(
    projection: &Projection<'_>,
    root: NodeId,
    opposite: &HashSet<(&'static str, &str)>,
) -> bool {
    let own = identity_key(projection, root);
    descendant_identities(projection, root)
        .into_iter()
        .any(|identity| identity != own && opposite.contains(&identity))
}

fn direct_composites(projection: &Projection<'_>, parent: NodeId) -> Vec<NodeId> {
    projection
        .node(parent)
        .children
        .iter()
        .copied()
        .filter(|id| {
            let node = projection.node(*id);
            node.named && node.leaf.is_none()
        })
        .collect()
}

fn context_shape(projection: &Projection<'_>, id: NodeId) -> ContextShape {
    let node = projection.node(id);
    ContextShape {
        kind: node.kind,
        field: node.field,
    }
}

/// Describe an anonymous sibling by the first semantic identities exposed on each child branch.
fn anonymous_context_key<'source>(
    projection: &'source Projection<'source>,
    id: NodeId,
) -> AnonymousContextKey<'source> {
    let mut identities = Vec::new();
    let mut pending = projection
        .node(id)
        .children
        .iter()
        .rev()
        .copied()
        .collect::<Vec<_>>();
    while let Some(candidate) = pending.pop() {
        let node = projection.node(candidate);
        if node.correspondence != CorrespondenceRole::Transparent {
            continue;
        }
        if let Some(identity) = projection.identity_text(candidate) {
            identities.push(ContextIdentity {
                kind: node.kind,
                field: node.field,
                identity,
            });
            continue;
        }
        if node.field.is_some()
            && node
                .leaf
                .is_some_and(|leaf| leaf.syntax == SyntaxClass::Identifier)
        {
            identities.push(ContextIdentity {
                kind: node.kind,
                field: node.field,
                identity: projection
                    .leaf_text(candidate)
                    .expect("a concrete identifier owns source spelling"),
            });
            continue;
        }
        pending.extend(node.children.iter().rev().copied());
    }
    AnonymousContextKey {
        shape: context_shape(projection, id),
        identities,
    }
}

fn context_identity<'source>(
    projection: &'source Projection<'source>,
    id: NodeId,
) -> ContextIdentity<'source> {
    let node = projection.node(id);
    ContextIdentity {
        kind: node.kind,
        field: node.field,
        identity: projection
            .identity_text(id)
            .expect("identified context carries source identity"),
    }
}

/// Movement is decided by semantic siblings; each decoration inherits its owner's result.
fn contextual_match_placements(
    pair: &ProjectionPair<'_, '_>,
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
    let mut owner_placements = links.placement.clone();
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

/// Claim one context pair, accepting exact repeats and rejecting conflicting partners.
fn link_context(
    before: NodeId,
    after: NodeId,
    placement: Placement,
    reparenting: Option<Reparenting>,
    links: &mut ContextLinks,
) -> bool {
    let existing_after = links.before.get(&before).copied();
    let existing_before = links.after.get(&after).copied();
    if existing_after.is_some() || existing_before.is_some() {
        let accepted = existing_after == Some(after)
            && existing_before == Some(before)
            && links.placement.get(&before) == Some(&placement)
            && links.reparenting.get(&before).copied() == reparenting;
        return accepted;
    }

    links.before.insert(before, after);
    links.after.insert(after, before);
    links.placement.insert(before, placement);
    if let Some(reparenting) = reparenting {
        links.reparenting.insert(before, reparenting);
    }
    true
}

fn mark_subtree(projection: &Projection<'_>, root: NodeId, marked: &mut HashSet<NodeId>) {
    marked.extend(std::iter::once(root).chain(projection.descendants(root)));
}

/// Concrete node pairs implied by one collision-checked recursive fingerprint match.
fn exact_subtree_nodes(
    pair: &ProjectionPair<'_, '_>,
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
                let children = before_children
                    .into_iter()
                    .zip(after_children)
                    .collect::<Vec<_>>();
                pending.extend(children.into_iter().rev());
            }
            _ => unreachable!("equal recursive fingerprints retain leaf shape"),
        }
    }
    nodes
}

fn subtree_sizes(projection: &Projection<'_>) -> Vec<usize> {
    let mut sizes = vec![1; projection.nodes.len()];
    for index in (1..projection.nodes.len()).rev() {
        let node = projection.node(NodeId::new(index));
        let Some(parent) = node.parent else {
            continue;
        };
        sizes[parent.index()] += sizes[index];
    }
    sizes
}

impl CorrespondenceBuilder<'_, '_, '_> {
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
                        // Descending only through an accepted parent-scope proof.
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

    /// Claim one leaf pair without replacing either endpoint's existing partner.
    fn push_leaf_link(&mut self, link: LeafLink) -> bool {
        let before = self.graph.before_leaf[link.before.index()];
        let after = self.graph.after_leaf[link.after.index()];
        if before.is_some() || after.is_some() {
            return before == after
                && before
                    .and_then(|index| self.graph.leaf_links.get(index))
                    .is_some_and(|existing| existing == &link);
        }

        let index = self.graph.leaf_links.len();
        self.graph.before_leaf[link.before.index()] = Some(index);
        self.graph.after_leaf[link.after.index()] = Some(index);
        self.graph.leaf_links.push(link);
        true
    }

    /// Claim one semantic-scope pair without replacing an existing proof.
    fn push_scope_link(&mut self, link: ScopeLink) -> bool {
        let before = self.before_scope[link.before.index()];
        let after = self.after_scope[link.after.index()];
        if before.is_some() || after.is_some() {
            return before == after
                && before
                    .and_then(|index| self.scopes.get(index))
                    .is_some_and(|existing| existing == &link);
        }
        let index = self.scopes.len();
        self.before_scope[link.before.index()] = Some(index);
        self.after_scope[link.after.index()] = Some(index);
        self.scopes.push(link);
        true
    }
}

fn trailing_delimiter_owner(projection: &Projection<'_>, leaf: NodeId) -> Option<NodeId> {
    if !projection.node(leaf).leaf?.delimiter {
        return None;
    }
    if !matches!(projection.leaf_text(leaf)?, "," | ";") {
        return None;
    }
    let parent = projection.node(leaf).parent?;
    let siblings = &projection.node(parent).children;
    let position = siblings.iter().position(|candidate| *candidate == leaf)?;
    siblings[..position]
        .iter()
        .rev()
        .copied()
        .find(|candidate| {
            let node = projection.node(*candidate);
            node.named && node.decoration_owner.is_none()
        })
}

fn leaf_shape(projection: &Projection<'_>, id: NodeId) -> LeafShape {
    let node = projection.node(id);
    let leaf = node.leaf.expect("leaf collection contains only leaves");
    LeafShape {
        kind: node.kind,
        field: node.field,
        channel: leaf.channel,
        named: node.named,
        extra: node.extra,
        missing: node.missing,
    }
}

/// One equality-preserving edge in an ordered before/after correspondence.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct OrderedMatch {
    pub(crate) before: usize,
    pub(crate) after: usize,
}

impl OrderedMatch {
    fn new(before: usize, after: usize) -> Self {
        Self { before, after }
    }
}

/// Match equal values without crossing edges or allowing either occurrence to be reused.
///
/// Values unique on both sides become patience-style anchors. Their longest increasing
/// subsequence partitions the remaining work into local gaps, where bounded LCS preserves
/// repeated occurrences. An unusually large anchorless gap uses linear-memory greedy
/// alignment so adversarial inputs cannot allocate a quadratic table.
pub(crate) fn ordered_matches<T: Eq + Hash>(before: &[T], after: &[T]) -> Vec<OrderedMatch> {
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
            Some(OrderedMatch::new(*before, *after))
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
    for anchor in anchors.chain(std::iter::once(OrderedMatch::new(
        before.len(),
        after.len(),
    ))) {
        let gap = align_gap(
            &before[before_start..anchor.before],
            &after[after_start..anchor.after],
        );
        matches.extend(
            gap.into_iter().map(|edge| {
                OrderedMatch::new(before_start + edge.before, after_start + edge.after)
            }),
        );

        if anchor.before < before.len() && anchor.after < after.len() {
            matches.push(anchor);
        }
        before_start = anchor.before.saturating_add(1);
        after_start = anchor.after.saturating_add(1);
    }
    matches
}

/// Align atomic leaves by the nearest shared prefixes, never by a farther copied run.
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
                    candidates.push(OrderedMatch::new(index, after));
                }
            }
            if let Some(index) = after_start
                .checked_add(radius)
                .filter(|index| *index < after.len())
            {
                let value = &after[index];
                after_seen.entry(value).or_insert(index);
                if let Some(before) = before_seen.get(value).copied() {
                    candidates.push(OrderedMatch::new(before, index));
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

/// Exact occurrence pairing independent of order; duplicates retain FIFO identity.
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
            Some(OrderedMatch::new(before, after))
        })
        .collect()
}

/// LIS placement facts for matches already ordered by their before occurrence.
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

/// Membership mask for one deterministic longest strictly increasing subsequence.
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

/// Linear-memory alignment for one unusually large anchorless region.
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
        matches.push(OrderedMatch::new(before, after));
        after_floor = after + 1;
    }
    matches
}

/// Quadratic dynamic programming reserved for small gaps between exact anchors.
fn lcs_matches<T: Eq>(before: &[T], after: &[T]) -> Vec<OrderedMatch> {
    let width = after.len() + 1;
    let mut lengths = vec![0_usize; (before.len() + 1) * width];
    for before_index in (0..before.len()).rev() {
        for after_index in (0..after.len()).rev() {
            let length = if before[before_index] == after[after_index] {
                1 + lengths[(before_index + 1) * width + after_index + 1]
            } else {
                lengths[(before_index + 1) * width + after_index]
                    .max(lengths[before_index * width + after_index + 1])
            };
            lengths[before_index * width + after_index] = length;
        }
    }

    let mut matches = Vec::new();
    let mut before_index = 0;
    let mut after_index = 0;
    while before_index < before.len() && after_index < after.len() {
        if before[before_index] == after[after_index] {
            matches.push(OrderedMatch::new(before_index, after_index));
            before_index += 1;
            after_index += 1;
            continue;
        }

        let skip_before = lengths[(before_index + 1) * width + after_index];
        let skip_after = lengths[before_index * width + after_index + 1];
        if skip_before >= skip_after {
            before_index += 1;
        } else {
            after_index += 1;
        }
    }
    matches
}

#[cfg(test)]
mod tests;
