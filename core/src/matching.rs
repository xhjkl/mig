//! Pair syntax nodes across the two revisions: which construct became which.
//!
//! Matching stays in the syntax arena; line counts only weight which siblings
//! keep their order. Later stages choose line alignment and display. Matching proceeds from
//! the file's review units downward: paired parents constrain which children may
//! pair, so repeated text cannot drift between owners.

use super::align::{
    Pair, ordered_matches, ordered_matches_preferring, reciprocal_unique_matches, stable_members,
    weighted_stable_members,
};
use super::syntax::{
    ChildSlot, ContentChannel, LeafRole, NodeId, SiblingOrder, Side, Symbol, SyntaxPair, SyntaxTree,
};
use std::collections::HashMap;

/// How a matched before node relates to its after partner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Relation {
    /// Byte-identical source, layout included.
    Identical,
    /// Same tokens and structure; only layout differs.
    Equivalent,
    /// Something inside changed.
    Modified,
}

/// Ownership of a matched node relative to its surviving parent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ancestry {
    Same,
    Wrapped,
    Unwrapped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Placement {
    Stable,
    Relocated,
}

/// Facts that exist together once a before node has an after partner.
#[derive(Clone, Copy, Debug)]
struct Correspondence {
    after: NodeId,
    relation: Relation,
    placement: Placement,
    ancestry: Ancestry,
}

/// Node correspondence, with a reverse index for after-side lookup.
#[derive(Clone, Debug)]
pub struct Mapping {
    pairs: Vec<Option<Correspondence>>,
    before_by_after: Vec<Option<NodeId>>,
}

impl Mapping {
    fn new(before: usize, after: usize) -> Self {
        Self {
            pairs: vec![None; before],
            before_by_after: vec![None; after],
        }
    }

    fn correspondence(&self, before: NodeId) -> Option<&Correspondence> {
        self.pairs.get(before.index())?.as_ref()
    }

    pub fn partner(&self, before: NodeId) -> Option<NodeId> {
        self.correspondence(before).map(|pair| pair.after)
    }

    pub fn partner_before(&self, after: NodeId) -> Option<NodeId> {
        self.before_by_after.get(after.index()).copied().flatten()
    }

    pub fn relation(&self, before: NodeId) -> Option<Relation> {
        self.correspondence(before).map(|pair| pair.relation)
    }

    pub fn is_moved(&self, before: NodeId) -> bool {
        self.correspondence(before)
            .is_some_and(|pair| pair.placement == Placement::Relocated)
    }

    pub fn ancestry(&self, before: NodeId) -> Option<Ancestry> {
        self.correspondence(before).map(|pair| pair.ancestry)
    }

    pub fn counterpart(&self, side: Side, id: NodeId) -> Option<NodeId> {
        match side {
            Side::Before => self.partner(id),
            Side::After => self.partner_before(id),
        }
    }

    pub fn is_matched(&self, side: Side, id: NodeId) -> bool {
        self.counterpart(side, id).is_some()
    }

    fn link(&mut self, before: NodeId, after: NodeId, relation: Relation) -> bool {
        if self.pairs[before.index()].is_some() || self.before_by_after[after.index()].is_some() {
            return false;
        }
        self.pairs[before.index()] = Some(Correspondence {
            after,
            relation,
            placement: Placement::Stable,
            ancestry: Ancestry::Same,
        });
        self.before_by_after[after.index()] = Some(before);
        true
    }

    fn relocate(&mut self, before: NodeId) {
        self.pairs[before.index()]
            .as_mut()
            .expect("only matched nodes relocate")
            .placement = Placement::Relocated;
    }

    fn reparent(&mut self, before: NodeId, ancestry: Ancestry) {
        self.pairs[before.index()]
            .as_mut()
            .expect("only matched nodes change owners")
            .ancestry = ancestry;
    }

    fn classify(&mut self, before: NodeId, relation: Relation) {
        self.pairs[before.index()]
            .as_mut()
            .expect("only matched nodes have a relation")
            .relation = relation;
    }

    /// Forget every link in a before subtree and its reverse-index entry.
    fn unlink_before_subtree(&mut self, tree: &SyntaxTree<'_>, root: NodeId) {
        for id in std::iter::once(root).chain(tree.descendants(root)) {
            let pair = self.pairs[id.index()].take();
            if let Some(pair) = pair {
                self.before_by_after[pair.after.index()] = None;
            }
        }
    }

    /// Forget every link in an after subtree and its correspondence record.
    fn unlink_after_subtree(&mut self, tree: &SyntaxTree<'_>, root: NodeId) {
        for id in std::iter::once(root).chain(tree.descendants(root)) {
            let before = self.before_by_after[id.index()].take();
            if let Some(before) = before {
                self.pairs[before.index()] = None;
            }
        }
    }
}

/// Collision-free identifier assigned by structural interning, never by hash value alone.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Fingerprint(usize);

/// How one child of a matched pair stands among its siblings while they are being paired.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Slot {
    Open,
    /// Paired with the sibling at this index on the other side.
    Sibling(usize),
    /// Linked to a node inside an unmatched sibling on the other side; see `Recovered`.
    Recovered,
}

/// A child linked through a wrapper, placed among its siblings at the wrapper's slot.
struct Recovered {
    slots: Pair,
    /// The before-side node that was linked.
    before: NodeId,
    after: NodeId,
}

/// Largest group of unmatched candidates still compared pairwise for similarity.
const MAX_SIMILARITY_CANDIDATES: usize = 96;

/// Evidence accepted when recovering a node through a wrapper.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Recovery {
    Exact,
    Identity,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum RecoveryKey {
    Exact(Fingerprint),
    Identity(Symbol, String),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum TokenText<'source> {
    Exact(&'source str),
    Shape,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Key<'source> {
    Branch {
        symbol: Symbol,
        children: Vec<(ChildSlot, Fingerprint)>,
    },
    Token {
        symbol: Symbol,
        channel: ContentChannel,
        text: TokenText<'source>,
    },
}

#[derive(Default)]
struct Interner<'source> {
    ids: HashMap<Key<'source>, Fingerprint>,
}

impl<'source> Interner<'source> {
    fn intern(&mut self, key: Key<'source>) -> Fingerprint {
        let next = Fingerprint(self.ids.len());
        let id = self.ids.entry(key).or_insert(next);
        *id
    }
}

/// Structural digests comparable across both revisions through one interner.
#[derive(Clone, Copy, Debug)]
struct Fingerprints {
    /// Tokens and structure, omitting layout leaves.
    pub full: Fingerprint,
    /// Structure only; leaf spelling is ignored.
    pub shape: Fingerprint,
}

fn fingerprints<'source>(
    tree: &'source SyntaxTree<'_>,
    interner: &mut Interner<'source>,
) -> Vec<Fingerprints> {
    let mut digests = vec![None::<Fingerprints>; tree.nodes.len()];
    for index in (0..tree.nodes.len()).rev() {
        let id = NodeId::new(index);
        let node = tree.node(id);
        let (full_key, shape_key) = match node.leaf() {
            Some(leaf) => {
                let spelling = tree
                    .leaf_text(id)
                    .expect("a token owns its source spelling");
                (
                    Key::Token {
                        symbol: node.symbol,
                        channel: leaf.channel,
                        text: TokenText::Exact(spelling),
                    },
                    Key::Token {
                        symbol: node.symbol,
                        channel: leaf.channel,
                        text: TokenText::Shape,
                    },
                )
            }
            None => {
                let mut full_children = Vec::new();
                let mut shape_children = Vec::new();
                for child in node.children() {
                    if is_layout_leaf(tree, *child) {
                        continue;
                    }
                    let slot = tree.node(*child).slot;
                    let digest = digests[child.index()]
                        .expect("children precede parents in reverse preorder");
                    full_children.push((slot, digest.full));
                    shape_children.push((slot, digest.shape));
                }
                (
                    Key::Branch {
                        symbol: node.symbol,
                        children: full_children,
                    },
                    Key::Branch {
                        symbol: node.symbol,
                        children: shape_children,
                    },
                )
            }
        };
        let full = interner.intern(full_key);
        let shape = interner.intern(shape_key);
        digests[index] = Some(Fingerprints { full, shape });
    }
    digests
        .into_iter()
        .map(|digest| digest.expect("every node was fingerprinted"))
        .collect()
}

fn is_layout_leaf(tree: &SyntaxTree<'_>, id: NodeId) -> bool {
    tree.node(id)
        .leaf()
        .is_some_and(|leaf| leaf.channel == ContentChannel::Layout)
}

/// Payload leaves carry the evidence that two constructs are the same construct.
fn is_meaningful_leaf(tree: &SyntaxTree<'_>, id: NodeId) -> bool {
    let node = tree.node(id);
    let Some(leaf) = node.leaf() else {
        return false;
    };
    leaf.delimiter.is_none()
        && !matches!(
            leaf.channel,
            ContentChannel::Comment | ContentChannel::Layout
        )
        && leaf.role != LeafRole::Scaffolding
        && tree
            .leaf_text(id)
            .is_some_and(|text| !text.trim().is_empty())
}

/// Facts about both trees that every matching step consults.
struct Matcher<'input, 'before, 'after> {
    pair: &'input SyntaxPair<'before, 'after>,
    before: Vec<Fingerprints>,
    after: Vec<Fingerprints>,
    /// Number of meaningful payload leaves below each node, for similarity and move thresholds.
    before_weight: Vec<u32>,
    after_weight: Vec<u32>,
    mapping: Mapping,
}

/// Build the correspondence between two revisions of one file.
pub fn correspond(pair: &SyntaxPair<'_, '_>) -> Mapping {
    let mut interner = Interner::default();
    let before = fingerprints(&pair.before, &mut interner);
    let after = fingerprints(&pair.after, &mut interner);
    let mut matcher = Matcher {
        pair,
        before_weight: weights(&pair.before),
        after_weight: weights(&pair.after),
        before,
        after,
        mapping: Mapping::new(pair.before.nodes.len(), pair.after.nodes.len()),
    };
    matcher.run();
    matcher.mapping
}

fn weights(tree: &SyntaxTree<'_>) -> Vec<u32> {
    let mut weights = vec![0_u32; tree.nodes.len()];
    for index in (0..tree.nodes.len()).rev() {
        let id = NodeId::new(index);
        let node = tree.node(id);
        let mut weight = u32::from(is_meaningful_leaf(tree, id));
        for child in node.children() {
            weight = weight.saturating_add(weights[child.index()]);
        }
        weights[index] = weight;
    }
    weights
}

impl Matcher<'_, '_, '_> {
    fn run(&mut self) {
        let pair = self.pair;
        let root_relation = self.relation_of(pair.before.root, pair.after.root);
        if root_relation != Relation::Modified {
            self.link_subtree(pair.before.root, pair.after.root, root_relation);
            return;
        }
        if pair.before.node(pair.before.root).review.is_some() {
            // The whole file is one review unit: match its contents directly.
            self.mapping
                .link(pair.before.root, pair.after.root, Relation::Modified);
            self.match_children(pair.before.root, pair.after.root);
            return;
        }
        self.mapping
            .link(pair.before.root, pair.after.root, Relation::Modified);
        let before_units = pair.before.review_units().collect::<Vec<_>>();
        let after_units = pair.after.review_units().collect::<Vec<_>>();
        let pairs = self.pair_units(&before_units, &after_units);
        let mut before_match = vec![Slot::Open; before_units.len()];
        let mut after_match = vec![Slot::Open; after_units.len()];
        for edge in &pairs {
            before_match[edge.before] = Slot::Sibling(edge.after);
            after_match[edge.after] = Slot::Sibling(edge.before);
        }
        let chain = self.place(&before_units, &after_units, pairs);
        let mut recovered = Vec::new();
        for recovery in [Recovery::Exact, Recovery::Identity] {
            recovered.extend(self.recover_wrapped_children(
                &before_units,
                &after_units,
                &mut before_match,
                &mut after_match,
                recovery,
            ));
        }
        self.mark_crossing_moves(&chain, &recovered);
        self.recover_cross_owner_moves();
    }

    fn tree(&self, side: Side) -> &SyntaxTree<'_> {
        self.pair.tree(side)
    }

    fn fingerprints(&self, side: Side, id: NodeId) -> Fingerprints {
        match side {
            Side::Before => self.before[id.index()],
            Side::After => self.after[id.index()],
        }
    }

    fn weight(&self, side: Side, id: NodeId) -> u32 {
        match side {
            Side::Before => self.before_weight[id.index()],
            Side::After => self.after_weight[id.index()],
        }
    }

    /// Link sibling pairs and recurse into them, marking as moved the ones outside the heaviest
    /// in-order chain, which is returned. Input pairs index into the two sibling lists.
    fn place(&mut self, before: &[NodeId], after: &[NodeId], mut pairs: Vec<Pair>) -> Vec<Pair> {
        pairs.sort_unstable_by_key(|edge| edge.before);
        let weights = pairs
            .iter()
            .map(|edge| self.pair.before.node(before[edge.before]).lines.len())
            .collect::<Vec<_>>();
        let stable = weighted_stable_members(&pairs, &weights);
        let mut chain = Vec::new();
        for (edge, stable) in pairs.into_iter().zip(stable) {
            let (before_id, after_id) = (before[edge.before], after[edge.after]);
            self.match_pair(before_id, after_id);
            if stable {
                chain.push(edge);
            } else if self.mapping.partner(before_id) == Some(after_id) {
                self.mapping.relocate(before_id);
            }
        }
        chain
    }

    /// Compare the nodes' source including any attached trailing delimiter.
    fn relation_of(&self, before: NodeId, after: NodeId) -> Relation {
        let pair = self.pair;
        let before_node = pair.before.node(before);
        let after_node = pair.after.node(after);
        if self.before[before.index()].full != self.after[after.index()].full {
            return Relation::Modified;
        }
        let before_text = pair
            .before
            .source
            .slice(before_node.source_envelope.clone())
            .expect("syntax owns valid source ranges");
        let after_text = pair
            .after
            .source
            .slice(after_node.source_envelope.clone())
            .expect("syntax owns valid source ranges");
        if before_text == after_text {
            return Relation::Identical;
        }
        // Including attached delimiters; they live outside the node's structural fingerprint.
        let before_tokens = super::syntax::content::tokens_in_bytes(
            &pair.before,
            before_node.source_envelope.clone(),
        );
        let after_tokens = super::syntax::content::tokens_in_bytes(
            &pair.after,
            after_node.source_envelope.clone(),
        );
        if before_tokens == after_tokens {
            return Relation::Equivalent;
        }
        Relation::Modified
    }

    /// Link a pair, descending into equal structure or matching children of changed structure.
    fn match_pair(&mut self, before: NodeId, after: NodeId) {
        let relation = self.relation_of(before, after);
        if relation != Relation::Modified {
            self.link_subtree(before, after, relation);
            return;
        }
        if !self.mapping.link(before, after, Relation::Modified) {
            return;
        }
        self.match_children(before, after);
    }

    /// Link two subtrees with equal full fingerprints node by node.
    fn link_subtree(&mut self, before: NodeId, after: NodeId, relation: Relation) {
        let pair = self.pair;
        let mut pending = vec![(before, after)];
        while let Some((before, after)) = pending.pop() {
            let relation = if relation == Relation::Identical {
                Relation::Identical
            } else {
                self.relation_of(before, after)
            };
            if !self.mapping.link(before, after, relation) {
                if self.mapping.partner(before) == Some(after) {
                    self.mapping.classify(before, relation);
                } else {
                    // Equal subtrees outrank whatever claimed a part of either earlier: typically a
                    // statement recovered through a wrapper into a block that turns out to have
                    // moved whole. The earlier claimant goes back to being unmatched.
                    self.mapping.unlink_before_subtree(&pair.before, before);
                    self.mapping.unlink_after_subtree(&pair.after, after);
                    if !self.mapping.link(before, after, relation) {
                        continue;
                    }
                }
            }
            let before_children = content_children(&pair.before, before);
            let after_children = content_children(&pair.after, after);
            if before_children.len() != after_children.len() {
                debug_assert!(false, "equal fingerprints have equal child counts");
                continue;
            }
            pending.extend(before_children.into_iter().zip(after_children).rev());
        }
    }

    /// Pair the file's review units: definitions by name, comments and lines by content,
    /// then whatever is compatible between those anchors; decorations follow their owners.
    fn pair_units(&self, before: &[NodeId], after: &[NodeId]) -> Vec<Pair> {
        let pair = self.pair;
        let mut before_match = vec![Slot::Open; before.len()];
        let mut after_match = vec![Slot::Open; after.len()];
        let is_decoration =
            |tree: &SyntaxTree<'_>, id: NodeId| tree.node(id).decoration_owner.is_some();
        let full = |side: Side, ids: &[NodeId], indices: &[usize]| {
            indices
                .iter()
                .map(|index| self.fingerprints(side, ids[*index]).full)
                .collect::<Vec<_>>()
        };

        // Composite units with a name pair by that name, identical content first.
        let named = |tree: &SyntaxTree<'_>, ids: &[NodeId]| {
            (0..ids.len())
                .filter(|index| {
                    let id = ids[*index];
                    tree.node(id).leaf().is_none()
                        && !is_decoration(tree, id)
                        && tree.identity_text(id).is_some()
                })
                .collect::<Vec<_>>()
        };
        let keyed_before = named(&pair.before, before);
        let keyed_after = named(&pair.after, after);
        let before_keys = keyed_before
            .iter()
            .map(|index| unit_key(&pair.before, before[*index]))
            .collect::<Vec<_>>();
        let after_keys = keyed_after
            .iter()
            .map(|index| unit_key(&pair.after, after[*index]))
            .collect::<Vec<_>>();
        for edge in pair_keyed(
            &before_keys,
            &after_keys,
            &full(Side::Before, before, &keyed_before),
            &full(Side::After, after, &keyed_after),
        ) {
            claim(
                &mut before_match,
                &mut after_match,
                keyed_before[edge.before],
                keyed_after[edge.after],
            );
        }

        // Atomic units (comments, plain lines) pair by exact content in order.
        let atomic_before = open(&before_match)
            .filter(|index| {
                pair.before.node(before[*index]).leaf().is_some()
                    && !is_decoration(&pair.before, before[*index])
            })
            .collect::<Vec<_>>();
        let atomic_after = open(&after_match)
            .filter(|index| {
                pair.after.node(after[*index]).leaf().is_some()
                    && !is_decoration(&pair.after, after[*index])
            })
            .collect::<Vec<_>>();
        for edge in ordered_matches(
            &full(Side::Before, before, &atomic_before),
            &full(Side::After, after, &atomic_after),
        ) {
            claim(
                &mut before_match,
                &mut after_match,
                atomic_before[edge.before],
                atomic_after[edge.after],
            );
        }

        // Unnamed or renamed units pair by content similarity when the preference is mutual,
        // then by kind between the anchors established so far.
        let open_before = open(&before_match)
            .filter(|index| !is_decoration(&pair.before, before[*index]))
            .collect::<Vec<_>>();
        let open_after = open(&after_match)
            .filter(|index| !is_decoration(&pair.after, after[*index]))
            .collect::<Vec<_>>();
        if open_before.len().max(open_after.len()) <= MAX_SIMILARITY_CANDIDATES {
            let before_tokens = open_before
                .iter()
                .map(|index| self.leaf_digests(Side::Before, before[*index]))
                .collect::<Vec<_>>();
            let after_tokens = open_after
                .iter()
                .map(|index| self.leaf_digests(Side::After, after[*index]))
                .collect::<Vec<_>>();
            for edge in reciprocal_unique_matches(open_before.len(), open_after.len(), 0, |b, a| {
                if pair.before.node(before[open_before[b]]).symbol
                    != pair.after.node(after[open_after[a]]).symbol
                {
                    return 0;
                }
                similar_enough(&before_tokens[b], &after_tokens[a])
            }) {
                claim(
                    &mut before_match,
                    &mut after_match,
                    open_before[edge.before],
                    open_after[edge.after],
                );
            }
        }

        let anchors = {
            let mut anchors = sibling_pairs(&before_match)
                .filter(|edge| !is_decoration(&pair.before, before[edge.before]))
                .collect::<Vec<_>>();
            anchors.sort_unstable_by_key(|edge| edge.before);
            let stable = stable_members(&anchors);
            anchors
                .into_iter()
                .zip(stable)
                .filter_map(|(edge, stable)| stable.then_some(edge))
                .collect::<Vec<_>>()
        };
        let mut before_start = 0;
        let mut after_start = 0;
        for anchor in anchors.into_iter().chain(std::iter::once(Pair {
            before: before.len(),
            after: after.len(),
        })) {
            let gap_before = (before_start..anchor.before)
                .filter(|index| {
                    before_match[*index] == Slot::Open
                        && !is_decoration(&pair.before, before[*index])
                })
                .collect::<Vec<_>>();
            let gap_after = (after_start..anchor.after)
                .filter(|index| {
                    after_match[*index] == Slot::Open && !is_decoration(&pair.after, after[*index])
                })
                .collect::<Vec<_>>();
            let before_kinds = gap_before
                .iter()
                .map(|index| pair.before.node(before[*index]).symbol)
                .collect::<Vec<_>>();
            let after_kinds = gap_after
                .iter()
                .map(|index| pair.after.node(after[*index]).symbol)
                .collect::<Vec<_>>();
            for edge in ordered_matches_preferring(&before_kinds, &after_kinds, |b, a| {
                self.fingerprints(Side::Before, before[gap_before[b]]).shape
                    == self.fingerprints(Side::After, after[gap_after[a]]).shape
            }) {
                claim(
                    &mut before_match,
                    &mut after_match,
                    gap_before[edge.before],
                    gap_after[edge.after],
                );
            }
            before_start = anchor.before.saturating_add(1);
            after_start = anchor.after.saturating_add(1);
        }

        // Decorations pair within matched owners: exact first, then by kind in order.
        let decorations = |tree: &SyntaxTree<'_>, ids: &[NodeId], slots: &[Slot]| {
            let mut by_owner = HashMap::<NodeId, Vec<usize>>::new();
            for index in open(slots) {
                if let Some(owner) = tree.node(ids[index]).decoration_owner {
                    by_owner.entry(owner).or_default().push(index);
                }
            }
            by_owner
        };
        let decorations_before = decorations(&pair.before, before, &before_match);
        let decorations_after = decorations(&pair.after, after, &after_match);
        let mut owner_pairs = sibling_pairs(&before_match)
            .map(|edge| (before[edge.before], after[edge.after]))
            .collect::<Vec<_>>();
        owner_pairs.push((pair.before.root, pair.after.root));
        for (before_owner, after_owner) in owner_pairs {
            let still_open = |group: Option<&Vec<usize>>, slots: &[Slot]| {
                group
                    .map(|group| {
                        group
                            .iter()
                            .copied()
                            .filter(|index| slots[*index] == Slot::Open)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            };
            let group_before = still_open(decorations_before.get(&before_owner), &before_match);
            let group_after = still_open(decorations_after.get(&after_owner), &after_match);
            if group_before.is_empty() || group_after.is_empty() {
                continue;
            }
            for edge in ordered_matches(
                &full(Side::Before, before, &group_before),
                &full(Side::After, after, &group_after),
            ) {
                claim(
                    &mut before_match,
                    &mut after_match,
                    group_before[edge.before],
                    group_after[edge.after],
                );
            }
            let rest_before = still_open(Some(&group_before), &before_match);
            let rest_after = still_open(Some(&group_after), &after_match);
            let before_kinds = rest_before
                .iter()
                .map(|index| pair.before.node(before[*index]).symbol)
                .collect::<Vec<_>>();
            let after_kinds = rest_after
                .iter()
                .map(|index| pair.after.node(after[*index]).symbol)
                .collect::<Vec<_>>();
            for edge in ordered_matches(&before_kinds, &after_kinds) {
                claim(
                    &mut before_match,
                    &mut after_match,
                    rest_before[edge.before],
                    rest_after[edge.after],
                );
            }
        }

        sibling_pairs(&before_match).collect()
    }

    /// Sorted digests of the meaningful leaves below a node, for similarity scoring.
    fn leaf_digests(&self, side: Side, root: NodeId) -> Vec<Fingerprint> {
        let tree = self.tree(side);
        let identity = tree.identity_text(root);
        let mut tokens = std::iter::once(root)
            .chain(tree.descendants(root))
            .filter(|id| is_meaningful_leaf(tree, *id))
            .filter(|id| tree.leaf_text(*id) != identity)
            .map(|id| self.fingerprints(side, id).full)
            .collect::<Vec<_>>();
        tokens.sort_unstable();
        tokens
    }

    /// Pair the children of two matched composites, then recurse into the pairs.
    fn match_children(&mut self, before_parent: NodeId, after_parent: NodeId) {
        let pair = self.pair;
        let before = content_children(&pair.before, before_parent);
        let after = content_children(&pair.after, after_parent);
        let mut before_match = vec![Slot::Open; before.len()];
        let mut after_match = vec![Slot::Open; after.len()];
        let full = |side: Side, ids: &[NodeId], indices: &[usize]| {
            indices
                .iter()
                .map(|index| self.fingerprints(side, ids[*index]).full)
                .collect::<Vec<_>>()
        };

        // Named members pair by name regardless of order; named statements pair by name in order.
        let keyed =
            |tree: &SyntaxTree<'_>, ids: &[NodeId], slots: &[Slot], matching: SiblingOrder| {
                open(slots)
                    .filter(|index| keyed_child(tree, ids[*index], matching))
                    .collect::<Vec<_>>()
            };
        let local_before = keyed(
            &pair.before,
            &before,
            &before_match,
            SiblingOrder::ByIdentity,
        );
        let local_after = keyed(&pair.after, &after, &after_match, SiblingOrder::ByIdentity);
        let before_keys = local_before
            .iter()
            .map(|index| unit_key(&pair.before, before[*index]))
            .collect::<Vec<_>>();
        let after_keys = local_after
            .iter()
            .map(|index| unit_key(&pair.after, after[*index]))
            .collect::<Vec<_>>();
        for edge in pair_keyed(
            &before_keys,
            &after_keys,
            &full(Side::Before, &before, &local_before),
            &full(Side::After, &after, &local_after),
        ) {
            claim(
                &mut before_match,
                &mut after_match,
                local_before[edge.before],
                local_after[edge.after],
            );
        }
        let ordered_before = keyed(
            &pair.before,
            &before,
            &before_match,
            SiblingOrder::SourceOrder,
        );
        let ordered_after = keyed(&pair.after, &after, &after_match, SiblingOrder::SourceOrder);
        let before_keys = ordered_before
            .iter()
            .map(|index| unit_key(&pair.before, before[*index]))
            .collect::<Vec<_>>();
        let after_keys = ordered_after
            .iter()
            .map(|index| unit_key(&pair.after, after[*index]))
            .collect::<Vec<_>>();
        for edge in ordered_matches_preferring(&before_keys, &after_keys, |b, a| {
            self.fingerprints(Side::Before, before[ordered_before[b]])
                .full
                == self.fingerprints(Side::After, after[ordered_after[a]]).full
        }) {
            claim(
                &mut before_match,
                &mut after_match,
                ordered_before[edge.before],
                ordered_after[edge.after],
            );
        }

        // Exact content in order.
        let open_before = open(&before_match).collect::<Vec<_>>();
        let open_after = open(&after_match).collect::<Vec<_>>();
        let before_full = open_before
            .iter()
            .map(|index| {
                (
                    pair.before.node(before[*index]).slot,
                    self.fingerprints(Side::Before, before[*index]).full,
                )
            })
            .collect::<Vec<_>>();
        let after_full = open_after
            .iter()
            .map(|index| {
                (
                    pair.after.node(after[*index]).slot,
                    self.fingerprints(Side::After, after[*index]).full,
                )
            })
            .collect::<Vec<_>>();
        for edge in ordered_matches(&before_full, &after_full) {
            claim(
                &mut before_match,
                &mut after_match,
                open_before[edge.before],
                open_after[edge.after],
            );
        }

        // Exact content that survived inside a new wrapper, or came out of an old one, pairs
        // before positional shape can claim it for something else.
        let mut recovered = Vec::new();
        for recovery in [Recovery::Exact, Recovery::Identity] {
            recovered.extend(self.recover_wrapped_children(
                &before,
                &after,
                &mut before_match,
                &mut after_match,
                recovery,
            ));
        }

        // Same syntax role in order, preferring equal structure.
        let open_before = open(&before_match).collect::<Vec<_>>();
        let open_after = open(&after_match).collect::<Vec<_>>();
        let before_shape = open_before
            .iter()
            .map(|index| child_shape(&pair.before, before[*index]))
            .collect::<Vec<_>>();
        let after_shape = open_after
            .iter()
            .map(|index| child_shape(&pair.after, after[*index]))
            .collect::<Vec<_>>();
        for edge in ordered_matches_preferring(&before_shape, &after_shape, |b, a| {
            self.fingerprints(Side::Before, before[open_before[b]])
                .shape
                == self.fingerprints(Side::After, after[open_after[a]]).shape
        }) {
            claim(
                &mut before_match,
                &mut after_match,
                open_before[edge.before],
                open_after[edge.after],
            );
        }

        // Composites of one kind that kept their content but changed name pair by similarity.
        let open_before = open(&before_match)
            .filter(|index| pair.before.node(before[*index]).leaf().is_none())
            .collect::<Vec<_>>();
        let open_after = open(&after_match)
            .filter(|index| pair.after.node(after[*index]).leaf().is_none())
            .collect::<Vec<_>>();
        if !open_before.is_empty()
            && !open_after.is_empty()
            && open_before.len().max(open_after.len()) <= MAX_SIMILARITY_CANDIDATES
        {
            let before_tokens = open_before
                .iter()
                .map(|index| self.leaf_digests(Side::Before, before[*index]))
                .collect::<Vec<_>>();
            let after_tokens = open_after
                .iter()
                .map(|index| self.leaf_digests(Side::After, after[*index]))
                .collect::<Vec<_>>();
            for edge in reciprocal_unique_matches(open_before.len(), open_after.len(), 0, |b, a| {
                if pair.before.node(before[open_before[b]]).symbol
                    != pair.after.node(after[open_after[a]]).symbol
                {
                    return 0;
                }
                similar_enough(&before_tokens[b], &after_tokens[a])
            }) {
                claim(
                    &mut before_match,
                    &mut after_match,
                    open_before[edge.before],
                    open_after[edge.after],
                );
            }
        }

        // Placement before recursion: children that left the common order moved. Children
        // recovered through a wrapper are already linked and carry no sibling partner.
        let chain = self.place(&before, &after, sibling_pairs(&before_match).collect());
        self.mark_crossing_moves(&chain, &recovered);
    }

    /// Pair unmatched children with a unique counterpart found inside an unmatched child of the
    /// other side. The pair is linked immediately so later steps cannot claim either node.
    fn recover_wrapped_children(
        &mut self,
        before: &[NodeId],
        after: &[NodeId],
        before_match: &mut [Slot],
        after_match: &mut [Slot],
        recovery: Recovery,
    ) -> Vec<Recovered> {
        let mut recovered = Vec::new();
        let open_before = open(before_match)
            .filter(|index| !self.mapping.is_matched(Side::Before, before[*index]))
            .collect::<Vec<_>>();
        let open_after = open(after_match)
            .filter(|index| !self.mapping.is_matched(Side::After, after[*index]))
            .collect::<Vec<_>>();
        if open_before.is_empty() || open_after.is_empty() {
            return recovered;
        }
        let after_roots = open_after
            .iter()
            .map(|index| (*index, after[*index]))
            .collect::<Vec<_>>();
        let after_index = self.recoverable_index(Side::After, &after_roots, recovery);
        let mut wrapped = Vec::new();
        for index in &open_before {
            let before_id = before[*index];
            if self.weight(Side::Before, before_id) == 0 {
                continue;
            }
            let Some(key) = self.recovery_key(Side::Before, before_id, recovery) else {
                continue;
            };
            if let Some([(slot, inner)]) = after_index.get(&key).map(Vec::as_slice) {
                wrapped.push((*index, before_id, *slot, *inner));
            }
        }
        for (index, before_id, slot, inner) in wrapped {
            if self.mapping.is_matched(Side::Before, before_id)
                || self.mapping.is_matched(Side::After, inner)
            {
                continue;
            }
            before_match[index] = Slot::Recovered;
            self.match_pair(before_id, inner);
            if self.mapping.partner(before_id) == Some(inner) {
                self.mapping.reparent(before_id, Ancestry::Wrapped);
                recovered.push(Recovered {
                    slots: Pair {
                        before: index,
                        after: slot,
                    },
                    before: before_id,
                    after: inner,
                });
            }
        }
        let before_roots = open_before
            .iter()
            .filter(|index| before_match[**index] == Slot::Open)
            .map(|index| (*index, before[*index]))
            .collect::<Vec<_>>();
        let before_index = self.recoverable_index(Side::Before, &before_roots, recovery);
        let mut unwrapped = Vec::new();
        for index in &open_after {
            let after_id = after[*index];
            if self.mapping.is_matched(Side::After, after_id)
                || self.weight(Side::After, after_id) == 0
            {
                continue;
            }
            let Some(key) = self.recovery_key(Side::After, after_id, recovery) else {
                continue;
            };
            if let Some([(slot, inner)]) = before_index.get(&key).map(Vec::as_slice) {
                unwrapped.push((*index, *slot, *inner, after_id));
            }
        }
        for (index, slot, inner, after_id) in unwrapped {
            if self.mapping.is_matched(Side::Before, inner)
                || self.mapping.is_matched(Side::After, after_id)
            {
                continue;
            }
            after_match[index] = Slot::Recovered;
            self.match_pair(inner, after_id);
            if self.mapping.partner(inner) == Some(after_id) {
                self.mapping.reparent(inner, Ancestry::Unwrapped);
                recovered.push(Recovered {
                    slots: Pair {
                        before: slot,
                        after: index,
                    },
                    before: inner,
                    after: after_id,
                });
            }
        }
        recovered
    }

    /// A child recovered through a wrapper sits at its wrapper's slot among the siblings.
    /// When that position crosses the stable chain of sibling matches, the child also moved.
    fn mark_crossing_moves(&mut self, chain: &[Pair], recovered: &[Recovered]) {
        for item in recovered {
            // Skipping superseded recoveries; equal subtrees reclaim their descendants.
            if self.mapping.partner(item.before) != Some(item.after) {
                continue;
            }
            let crosses = chain.iter().any(|edge| {
                (edge.before < item.slots.before && edge.after > item.slots.after)
                    || (edge.before > item.slots.before && edge.after < item.slots.after)
            });
            if crosses {
                self.mapping.relocate(item.before);
            }
        }
    }

    /// What a node must share with a counterpart to be recovered through a wrapper.
    fn recovery_key(&self, side: Side, id: NodeId, recovery: Recovery) -> Option<RecoveryKey> {
        match recovery {
            Recovery::Exact => Some(RecoveryKey::Exact(self.fingerprints(side, id).full)),
            Recovery::Identity => {
                let tree = self.tree(side);
                let node = tree.node(id);
                if node.leaf().is_some() {
                    return None;
                }
                let identity = tree.identity_text(id)?;
                Some(RecoveryKey::Identity(node.symbol, identity.to_owned()))
            }
        }
    }

    /// Index the nodes reachable inside unmatched roots, by the root's sibling slot. Exact
    /// recovery never crosses a sealed owner, so a statement cannot be pulled into a new
    /// function; identity recovery may, because the same name under a new wrapper is the same
    /// construct.
    fn recoverable_index(
        &self,
        side: Side,
        roots: &[(usize, NodeId)],
        recovery: Recovery,
    ) -> HashMap<RecoveryKey, Vec<(usize, NodeId)>> {
        let tree = self.tree(side);
        let mut index = HashMap::<RecoveryKey, Vec<(usize, NodeId)>>::new();
        for (slot, root) in roots {
            let root_node = tree.node(*root);
            if root_node.leaf().is_some()
                || (recovery == Recovery::Exact && root_node.seals_wrappers())
            {
                continue;
            }
            let mut stack = root_node.children().to_vec();
            while let Some(id) = stack.pop() {
                let node = tree.node(id);
                if self.mapping.is_matched(side, id) || node.review.is_some() {
                    continue;
                }
                if recovery == Recovery::Exact && node.seals_wrappers() {
                    continue;
                }
                if let Some(key) = self.recovery_key(side, id, recovery)
                    && (node.is_named() || node.leaf().is_some())
                {
                    index.entry(key).or_default().push((*slot, id));
                }
                if node.leaf().is_none() {
                    stack.extend(node.children().iter().copied());
                }
            }
        }
        index
    }

    /// Statements that left one surviving owner for another keep their identity as a move.
    fn recover_cross_owner_moves(&mut self) {
        let pair = self.pair;
        let before_groups = self.relocation_candidates(Side::Before);
        if before_groups.is_empty() {
            return;
        }
        let after_groups = self.relocation_candidates(Side::After);
        let mut candidates = Vec::new();
        for (digest, before_group) in before_groups {
            let Some(after_group) = after_groups.get(&digest) else {
                continue;
            };
            let ([before], [after]) = (before_group.as_slice(), after_group.as_slice()) else {
                continue;
            };
            candidates.push((*before, *after));
        }
        // Outer subtrees first, so a moved block is one move rather than many.
        candidates.sort_by_key(|(before, _)| {
            (
                std::cmp::Reverse(self.before_weight[before.index()]),
                *before,
            )
        });
        for (before, after) in candidates {
            if self.mapping.is_matched(Side::Before, before)
                || self.mapping.is_matched(Side::After, after)
            {
                continue;
            }
            // Moves stay within one review unit: a statement that reappears in another
            // definition is a removal and an addition, not a tunnel between owners.
            let before_unit = enclosing_unit(&pair.before, before);
            let after_unit = enclosing_unit(&pair.after, after);
            if self.mapping.partner(before_unit) != Some(after_unit) {
                continue;
            }
            let relation = self.relation_of(before, after);
            self.link_subtree(before, after, relation);
            self.mapping.relocate(before);
        }
    }

    /// Unmatched named composites under a matched parent, grouped by content: substantial ones,
    /// or small ones that carry a name they may move with.
    fn relocation_candidates(&self, side: Side) -> HashMap<Fingerprint, Vec<NodeId>> {
        let tree = self.tree(side);
        let mut groups = HashMap::<Fingerprint, Vec<NodeId>>::new();
        for index in 0..tree.nodes.len() {
            let id = NodeId::new(index);
            let node = tree.node(id);
            if node.leaf().is_some()
                || !node.is_named()
                || self.mapping.is_matched(side, id)
                || (self.weight(side, id) < 3 && !relocatable_by_name(tree, id))
                || node
                    .parent
                    .is_none_or(|parent| !self.mapping.is_matched(side, parent))
            {
                continue;
            }
            groups
                .entry(self.fingerprints(side, id).full)
                .or_default()
                .push(id);
        }
        groups
    }
}

/// Children that take part in matching: every child but layout whitespace.
fn content_children(tree: &SyntaxTree<'_>, parent: NodeId) -> Vec<NodeId> {
    tree.node(parent)
        .children()
        .iter()
        .copied()
        .filter(|child| !is_layout_leaf(tree, *child))
        .collect()
}

/// Indices of the children still open for pairing.
fn open(slots: &[Slot]) -> impl Iterator<Item = usize> + '_ {
    slots
        .iter()
        .enumerate()
        .filter(|(_, slot)| **slot == Slot::Open)
        .map(|(index, _)| index)
}

/// The sibling pairs claimed so far, by index into the two sibling lists.
fn sibling_pairs(before_match: &[Slot]) -> impl Iterator<Item = Pair> + '_ {
    before_match
        .iter()
        .enumerate()
        .filter_map(|(before, slot)| match slot {
            Slot::Sibling(after) => Some(Pair {
                before,
                after: *after,
            }),
            Slot::Open | Slot::Recovered => None,
        })
}

fn keyed_child(tree: &SyntaxTree<'_>, id: NodeId, order: SiblingOrder) -> bool {
    let node = tree.node(id);
    node.leaf().is_none()
        && node.sibling_order == order
        && node.decoration_owner.is_none()
        && tree.identity_text(id).is_some()
}

/// Pair same-keyed candidates: identical content first, then the rest in order of appearance.
/// Several imports from one module or several impls of one type share a key; exact content
/// decides which is which before position does.
fn pair_keyed<K: Eq + std::hash::Hash>(
    before_keys: &[K],
    after_keys: &[K],
    before_digests: &[Fingerprint],
    after_digests: &[Fingerprint],
) -> Vec<Pair> {
    let mut after_groups = HashMap::<&K, Vec<usize>>::new();
    for (index, key) in after_keys.iter().enumerate() {
        after_groups.entry(key).or_default().push(index);
    }
    let mut before_groups = HashMap::<&K, Vec<usize>>::new();
    for (index, key) in before_keys.iter().enumerate() {
        before_groups.entry(key).or_default().push(index);
    }
    let mut pairs = Vec::new();
    for (key, before_group) in before_groups {
        let Some(after_group) = after_groups.get(key) else {
            continue;
        };
        let before_values = before_group
            .iter()
            .map(|index| before_digests[*index])
            .collect::<Vec<_>>();
        let after_values = after_group
            .iter()
            .map(|index| after_digests[*index])
            .collect::<Vec<_>>();
        let mut before_used = vec![false; before_group.len()];
        let mut after_used = vec![false; after_group.len()];
        for edge in ordered_matches(&before_values, &after_values) {
            before_used[edge.before] = true;
            after_used[edge.after] = true;
            pairs.push(Pair {
                before: before_group[edge.before],
                after: after_group[edge.after],
            });
        }
        let rest_before = (0..before_group.len()).filter(|index| !before_used[*index]);
        let rest_after = (0..after_group.len()).filter(|index| !after_used[*index]);
        for (before, after) in rest_before.zip(rest_after) {
            pairs.push(Pair {
                before: before_group[before],
                after: after_group[after],
            });
        }
    }
    pairs.sort_unstable_by_key(|edge| (edge.before, edge.after));
    pairs
}

/// The nearest independently reviewed construct enclosing a node, or the file root.
fn enclosing_unit(tree: &SyntaxTree<'_>, mut id: NodeId) -> NodeId {
    loop {
        let node = tree.node(id);
        match node.parent {
            Some(parent) if node.review.is_none() => id = parent,
            _ => return id,
        }
    }
}

/// A small named construct may move between owners, but a member declaration belongs to its
/// owner: a same-named field in another structure is a different field.
fn relocatable_by_name(tree: &SyntaxTree<'_>, id: NodeId) -> bool {
    tree.identity_text(id).is_some() && tree.node(id).declares != super::syntax::Declares::Member
}

/// Pair two open siblings; a side already claimed stays as it is.
fn claim(before_match: &mut [Slot], after_match: &mut [Slot], before: usize, after: usize) {
    if before_match[before] == Slot::Open && after_match[after] == Slot::Open {
        before_match[before] = Slot::Sibling(after);
        after_match[after] = Slot::Sibling(before);
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct UnitKey<'source> {
    symbol: Symbol,
    identity: &'source str,
}

fn unit_key<'source>(tree: &'source SyntaxTree<'source>, id: NodeId) -> UnitKey<'source> {
    UnitKey {
        symbol: tree.node(id).symbol,
        identity: tree.identity_text(id).unwrap_or(""),
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct ChildShape {
    symbol: Symbol,
    slot: ChildSlot,
    leaf: bool,
}

fn child_shape(tree: &SyntaxTree<'_>, id: NodeId) -> ChildShape {
    let node = tree.node(id);
    ChildShape {
        symbol: node.symbol,
        slot: node.slot,
        leaf: node.leaf().is_some(),
    }
}

/// Similarity that qualifies two constructs as versions of each other, else zero: at least two
/// shared tokens making up half of the smaller side, and the larger side at most eight times the
/// shared part, so a small construct never pairs with a container that merely mentions its words.
fn similar_enough(before: &[Fingerprint], after: &[Fingerprint]) -> u64 {
    let score = similarity(before, after);
    let smaller = before.len().min(after.len()) as u64;
    let larger = before.len().max(after.len()) as u64;
    if score < 2 || score * 2 < smaller || score * 8 < larger {
        return 0;
    }
    score
}

/// Count shared digests between two sorted token lists.
fn similarity(before: &[Fingerprint], after: &[Fingerprint]) -> u64 {
    let mut shared = 0;
    let (mut b, mut a) = (0, 0);
    while b < before.len() && a < after.len() {
        match before[b].cmp(&after[a]) {
            std::cmp::Ordering::Less => b += 1,
            std::cmp::Ordering::Greater => a += 1,
            std::cmp::Ordering::Equal => {
                shared += 1;
                b += 1;
                a += 1;
            }
        }
    }
    shared
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn pair<'a>(path: &str, before: &'a str, after: &'a str) -> SyntaxPair<'a, 'a> {
        super::super::syntax::syntax_pair(Path::new(path), before, after, false).expect("syntax")
    }

    fn find(tree: &SyntaxTree<'_>, text: &str) -> NodeId {
        (0..tree.nodes.len())
            .map(NodeId::new)
            .find(|id| {
                let node = tree.node(*id);
                node.is_named()
                    && node.leaf().is_none()
                    && tree.source.slice(node.bytes.clone()) == Some(text)
            })
            .unwrap_or_else(|| panic!("no node spelled {text:?}"))
    }

    #[test]
    fn renamed_function_keeps_its_body_matched() {
        let pair = pair(
            "alpha.rs",
            "fn alpha() { beta(); gamma(); }\n",
            "fn delta() { beta(); gamma(); }\n",
        );
        let mapping = correspond(&pair);
        let before_fn = find(&pair.before, "fn alpha() { beta(); gamma(); }");
        let after_fn = find(&pair.after, "fn delta() { beta(); gamma(); }");
        assert_eq!(mapping.partner(before_fn), Some(after_fn));
        let body = find(&pair.before, "beta();");
        assert_eq!(mapping.relation(body), Some(Relation::Identical));
    }

    #[test]
    fn wrapped_expression_survives_inside_its_wrapper() {
        let pair = pair(
            "alpha.rs",
            "fn a() { let y = x; }\n",
            "fn a() { let y = Some(x); }\n",
        );
        let mapping = correspond(&pair);
        let x = (0..pair.before.nodes.len())
            .map(NodeId::new)
            .find(|id| pair.before.leaf_text(*id) == Some("x"))
            .expect("x leaf");
        assert!(mapping.partner(x).is_some());
        assert_eq!(mapping.ancestry(x), Some(Ancestry::Wrapped));
    }

    #[test]
    fn reordered_definitions_are_moves_not_edits() {
        let pair = pair(
            "alpha.rs",
            "fn a() { 1 }\n\nfn b() { 2 }\n",
            "fn b() { 2 }\n\nfn a() { 1 }\n",
        );
        let mapping = correspond(&pair);
        let a = find(&pair.before, "fn a() { 1 }");
        let b = find(&pair.before, "fn b() { 2 }");
        assert_eq!(mapping.relation(a), Some(Relation::Identical));
        assert_eq!(mapping.relation(b), Some(Relation::Identical));
        assert!(mapping.is_moved(a) ^ mapping.is_moved(b));
    }

    #[test]
    fn statement_moved_between_functions_is_not_a_move() {
        let pair = pair(
            "alpha.rs",
            "fn a() {\n    call_one(alpha, beta);\n}\nfn b() {\n}\n",
            "fn a() {\n}\nfn b() {\n    call_one(alpha, beta);\n}\n",
        );
        let mapping = correspond(&pair);
        let statement = find(&pair.before, "call_one(alpha, beta);");
        assert!(mapping.partner(statement).is_none());
    }

    #[test]
    fn statement_moved_within_a_function_is_recovered() {
        let pair = pair(
            "alpha.rs",
            "fn a() {\n    first();\n    if x {\n        call_one(alpha, beta);\n    }\n}\n",
            "fn a() {\n    call_one(alpha, beta);\n    first();\n    if x {\n    }\n}\n",
        );
        let mapping = correspond(&pair);
        let statement = find(&pair.before, "call_one(alpha, beta);");
        assert!(mapping.partner(statement).is_some());
        assert!(mapping.is_moved(statement));
    }

    #[test]
    fn units_sharing_a_name_pair_by_identical_content_before_position() {
        let pair = pair(
            "alpha.rs",
            "impl Alpha {\n    fn one(&self) -> u8 { 1 }\n}\n\nimpl Alpha {\n    fn two(&self) -> u8 { 2 }\n}\n",
            "impl Alpha {\n    fn two(&self) -> u8 { 2 }\n}\n\nimpl Alpha {\n    fn one(&self) -> u8 { 1 + 1 }\n}\n",
        );
        let mapping = correspond(&pair);
        let two = find(
            &pair.before,
            "impl Alpha {\n    fn two(&self) -> u8 { 2 }\n}",
        );
        let one = find(
            &pair.before,
            "impl Alpha {\n    fn one(&self) -> u8 { 1 }\n}",
        );
        assert_eq!(mapping.relation(two), Some(Relation::Identical));
        assert_eq!(mapping.relation(one), Some(Relation::Modified));
        assert_eq!(
            mapping.partner(one),
            Some(find(
                &pair.after,
                "impl Alpha {\n    fn one(&self) -> u8 { 1 + 1 }\n}"
            ))
        );
    }

    /// Debug aid for a dumped stress case: prints the nodes covering one line on each side.
    /// `MIG_CASE_BEFORE`, `MIG_CASE_AFTER` name the files, `MIG_CASE_PATH` the original path,
    /// `MIG_CASE_BEFORE_LINE` / `MIG_CASE_AFTER_LINE` the one-based lines of interest.
    #[test]
    #[ignore = "debug aid, driven by MIG_CASE_* environment variables"]
    fn debug_case_mapping() {
        let read = |name: &str| {
            std::fs::read_to_string(std::env::var(name).expect(name)).expect("readable")
        };
        let (before, after) = (read("MIG_CASE_BEFORE"), read("MIG_CASE_AFTER"));
        let path = std::env::var("MIG_CASE_PATH").unwrap_or_else(|_| "case.rs".to_owned());
        let pair = pair(&path, &before, &after);
        let mapping = correspond(&pair);
        eprintln!("{:#?}", crate::islands::islands(&pair, &mapping));
        let describe = |tree: &SyntaxTree<'_>, id: NodeId| {
            let node = tree.node(id);
            format!("{:?} #{} lines {:?}", node.symbol, id.index(), node.lines)
        };
        let report =
            |name: &str, tree: &SyntaxTree<'_>, other: &SyntaxTree<'_>, before_side: bool| {
                let Ok(line) = std::env::var(name) else {
                    return;
                };
                let line: usize = line.parse().expect("line number");
                for index in 0..tree.nodes.len() {
                    let id = NodeId::new(index);
                    let node = tree.node(id);
                    if !(node.lines.start.get() <= line && line < node.lines.end.get())
                        || node.lines.len() > 40
                    {
                        continue;
                    }
                    let partner = if before_side {
                        mapping.partner(id)
                    } else {
                        mapping.partner_before(id)
                    };
                    let (relation, moved) = if before_side {
                        (mapping.relation(id), mapping.is_moved(id))
                    } else {
                        (
                            partner.and_then(|partner| mapping.relation(partner)),
                            partner.is_some_and(|partner| mapping.is_moved(partner)),
                        )
                    };
                    eprintln!(
                        "{name}: {} -> {} relation {relation:?} moved {moved} ancestry {:?}",
                        describe(tree, id),
                        partner
                            .map(|partner| describe(other, partner))
                            .unwrap_or_else(|| "unmatched".to_owned()),
                        if before_side {
                            mapping.ancestry(id)
                        } else {
                            partner.and_then(|partner| mapping.ancestry(partner))
                        },
                    );
                }
            };
        report("MIG_CASE_BEFORE_LINE", &pair.before, &pair.after, true);
        report("MIG_CASE_AFTER_LINE", &pair.after, &pair.before, false);
        // Optional: dump one subtree on either side, with each node's partner.
        for (name, tree, other, before_side) in [
            ("MIG_CASE_BEFORE_SUBTREE", &pair.before, &pair.after, true),
            ("MIG_CASE_AFTER_SUBTREE", &pair.after, &pair.before, false),
        ] {
            let Ok(root) = std::env::var(name) else {
                continue;
            };
            let root = NodeId::new(root.parse().expect("node index"));
            let mut depth = vec![(root, 0_usize)];
            while let Some((id, level)) = depth.pop() {
                let node = tree.node(id);
                let partner = if before_side {
                    mapping.partner(id)
                } else {
                    mapping.partner_before(id)
                };
                let text = tree
                    .source
                    .slice(node.bytes.clone())
                    .unwrap_or("")
                    .chars()
                    .take(40)
                    .collect::<String>();
                eprintln!(
                    "{name}: {}{} named {} leaf {:?} bytes {:?} complete-lines {:?} -> {} | {text:?}",
                    "  ".repeat(level),
                    describe(tree, id),
                    node.is_named(),
                    node.leaf().map(|leaf| leaf.channel),
                    node.bytes,
                    super::super::islands::zero_based_lines(tree, id),
                    partner
                        .map(|partner| describe(other, partner))
                        .unwrap_or_else(|| "unmatched".to_owned()),
                );
                depth.extend(
                    node.children()
                        .iter()
                        .rev()
                        .map(|child| (*child, level + 1)),
                );
            }
        }
    }

    #[test]
    fn an_exact_moved_block_keeps_its_contents_from_a_stray_twin() {
        // "two" moves out of the inner group whole; "inspect" loses its own `h.record(1);`. The
        // moved block's statement belongs to the block, not to the vanished twin.
        let pair = pair(
            "alpha.ts",
            "describe(\"a\", () => {\n  describe(\"inner\", () => {\n    test(\"one\", () => {\n      check(1);\n    });\n    test(\"two\", () => {\n      const h = make();\n      h.record(1);\n      check(h.min, 1);\n      check(h.max, 1);\n    });\n  });\n  test(\"inspect\", () => {\n    const h = make();\n    h.record(1);\n    check(inspect(h), \"x\");\n  });\n});\n",
            "describe(\"a\", () => {\n  test(\"two\", () => {\n    const h = make();\n    h.record(1);\n    check(h.min, 1);\n    check(h.max, 1);\n  });\n  describe(\"inner\", () => {\n    test(\"one\", () => {\n      check(1);\n    });\n  });\n  test(\"inspect\", () => {\n    const h = make();\n    check(inspect(h), \"y\");\n  });\n});\n",
        );
        let mapping = correspond(&pair);
        let block_statement = find(&pair.before, "h.record(1);");
        let stray = (0..pair.before.nodes.len())
            .map(NodeId::new)
            .filter(|id| {
                pair.before
                    .source
                    .slice(pair.before.node(*id).bytes.clone())
                    == Some("h.record(1);")
            })
            .find(|id| *id != block_statement)
            .expect("two statements");
        let inner = find(&pair.after, "h.record(1);");
        assert_eq!(
            mapping.partner(block_statement),
            Some(inner),
            "the block keeps its statement"
        );
        assert_eq!(
            mapping.partner(stray),
            None,
            "the vanished twin is a removal"
        );
    }
}
