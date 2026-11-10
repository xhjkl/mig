//! Lower the node mapping onto source lines: which lines are kept, and the
//! islands of change between them.
//!
//! The output is a complete account of both revisions. Every line of either
//! file is kept (paired with identical content on the other side), part of a
//! move, or inside exactly one island. `verify` checks that account against the
//! text before document layout adds context and orientation.

use super::align::{changed_extent, ordered_matches};
use super::matching::{Mapping, Relation};
use super::syntax::{NodeId, ReviewRole, Side, SyntaxPair, SyntaxTree, node_owns_complete_lines};
use super::{EndingChange, LineEnding, LineNumber, Terminator};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

/// Matched lines with identical content; terminators are accounted separately. Indices are zero-based.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Kept {
    pub before: usize,
    pub after: usize,
}

/// Source lines claimed by one change, in ascending order on each side.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineChange {
    pub before: Vec<usize>,
    pub after: Vec<usize>,
}

/// Layout changes within syntax occurrences already proved to correspond.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reflow {
    pub lines: LineChange,
    pub before_scope: NodeId,
    pub after_scope: NodeId,
}

/// Presentation of one source change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Piece {
    /// Removed lines followed by added lines; either side may be empty.
    Block(LineChange),
    /// Paired line edits, with equal numbers of lines on both sides.
    Inline(LineChange),
    Reflow(Reflow),
    /// Intact syntax shown at a different source position.
    Move(LineChange),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Change {
    Addition,
    Deletion,
    Replacement,
    Reflow,
    Move,
}

/// Review priority, in ascending order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Priority {
    Reflow,
    Wiring,
    Move,
    Edit,
}

#[derive(Clone, Debug)]
pub struct Island {
    pub(crate) priority: Priority,
    pub(crate) pieces: Vec<Piece>,
    /// First after line of the island, or the insertion point of a deletion.
    pub(crate) anchor_after: usize,
    /// First before line of the island, or the position of an addition.
    pub(crate) anchor_before: usize,
    /// Innermost matched after-side node enclosing the island, for orientation.
    pub(crate) owner: NodeId,
}

impl Kept {
    pub fn line(&self, side: Side) -> usize {
        match side {
            Side::Before => self.before,
            Side::After => self.after,
        }
    }
}

impl Piece {
    pub fn change(&self) -> &LineChange {
        match self {
            Self::Block(lines) | Self::Inline(lines) | Self::Move(lines) => lines,
            Self::Reflow(reflow) => &reflow.lines,
        }
    }

    pub fn lines(&self, side: Side) -> &[usize] {
        let change = self.change();
        match side {
            Side::Before => &change.before,
            Side::After => &change.after,
        }
    }
}

impl Island {
    pub fn pieces(&self) -> &[Piece] {
        &self.pieces
    }
    pub fn priority(&self) -> Priority {
        self.priority
    }
    pub fn owner(&self) -> NodeId {
        self.owner
    }

    pub fn lines(&self, side: Side) -> impl Iterator<Item = usize> + '_ {
        self.pieces
            .iter()
            .flat_map(move |piece| piece.lines(side).iter().copied())
    }

    pub fn before_lines(&self) -> impl Iterator<Item = usize> + '_ {
        self.lines(Side::Before)
    }

    pub fn after_lines(&self) -> impl Iterator<Item = usize> + '_ {
        self.lines(Side::After)
    }

    pub fn anchor(&self, side: Side) -> usize {
        match side {
            Side::Before => self.anchor_before,
            Side::After => self.anchor_after,
        }
    }

    pub fn change(&self) -> Change {
        if self.is_move() {
            return Change::Move;
        }
        summarize(self)
    }

    pub fn is_move(&self) -> bool {
        self.pieces
            .iter()
            .all(|piece| matches!(piece, Piece::Move(_)))
    }
}

#[derive(Clone, Debug)]
pub struct Islands {
    pub(crate) kept: Vec<Kept>,
    pub(crate) islands: Vec<Island>,
    pub(crate) endings: Vec<EndingNotice>,
}

/// Terminator change anchored in after-side display order, including deletions.
#[derive(Clone, Copy, Debug)]
pub struct EndingNotice {
    pub change: EndingChange,
    pub(crate) anchor_after: usize,
}

impl Islands {
    pub fn kept(&self) -> &[Kept] {
        &self.kept
    }
    pub fn islands(&self) -> &[Island] {
        &self.islands
    }
    pub fn endings(&self) -> &[EndingNotice] {
        &self.endings
    }
}

/// Lower a mapping onto the two sources.
pub fn islands(pair: &SyntaxPair<'_, '_>, mapping: &Mapping) -> Islands {
    let mut builder = Builder::new(pair, mapping);
    let before_lines = pair.before.source.lines().len();
    let after_lines = pair.after.source.lines().len();
    builder.align_region(
        pair.before.root,
        pair.after.root,
        0..before_lines,
        0..after_lines,
        pair.after.root,
    );
    builder.finish()
}

struct Builder<'input, 'before, 'after> {
    pair: &'input SyntaxPair<'before, 'after>,
    mapping: &'input Mapping,
    complete_before: Vec<bool>,
    complete_after: Vec<bool>,
    /// Moved atoms by their before node, with the after node they became. Ordered, so that
    /// separators between adjacent moves are claimed the same way on every run.
    moves: BTreeMap<NodeId, NodeId>,
    move_targets: BTreeMap<NodeId, NodeId>,
    /// Lines each move carries, including separators that travel with it.
    move_lines_before: BTreeMap<NodeId, Vec<usize>>,
    move_lines_after: BTreeMap<NodeId, Vec<usize>>,
    moved_before: Vec<bool>,
    moved_after: Vec<bool>,
    recorded_moves: HashSet<NodeId>,
    weak_before: Vec<bool>,
    weak_after: Vec<bool>,
    kept: Vec<Kept>,
    islands: Vec<Island>,
}

/// Runs of atoms on each side that correspond, by index into the region's atom lists.
struct Anchor {
    before: Range<usize>,
    after: Range<usize>,
}

/// The inner atoms an anchor aligns, and the line region on each side they are aligned over.
struct Regions {
    inner_before: Vec<NodeId>,
    inner_after: Vec<NodeId>,
    before: Range<usize>,
    after: Range<usize>,
}

/// Snapshot of the output so a region can be collapsed into one block.
#[derive(Clone, Copy)]
struct Checkpoint {
    kept: usize,
    islands: usize,
}

/// A change split into the line pairs equal at either end and the differing middle.
struct SharedEdges {
    prefix: Vec<(usize, usize)>,
    suffix: Vec<(usize, usize)>,
    before: Vec<usize>,
    after: Vec<usize>,
}

impl<'input, 'before, 'after> Builder<'input, 'before, 'after> {
    fn new(pair: &'input SyntaxPair<'before, 'after>, mapping: &'input Mapping) -> Self {
        let complete_before = (0..pair.before.nodes.len())
            .map(|index| node_owns_complete_lines(&pair.before, NodeId::new(index)))
            .collect::<Vec<_>>();
        let complete_after = (0..pair.after.nodes.len())
            .map(|index| node_owns_complete_lines(&pair.after, NodeId::new(index)))
            .collect::<Vec<_>>();
        let mut builder = Self {
            pair,
            mapping,
            complete_before,
            complete_after,
            moves: BTreeMap::new(),
            move_targets: BTreeMap::new(),
            move_lines_before: BTreeMap::new(),
            move_lines_after: BTreeMap::new(),
            moved_before: vec![false; pair.before.source.lines().len()],
            moved_after: vec![false; pair.after.source.lines().len()],
            recorded_moves: HashSet::new(),
            weak_before: weak_lines(&pair.before),
            weak_after: weak_lines(&pair.after),
            kept: Vec::new(),
            islands: Vec::new(),
        };
        builder.collect_moves();
        builder
    }

    /// Moved nodes that own complete lines on both sides can be shown as moves.
    fn collect_moves(&mut self) {
        let pair = self.pair;
        for index in 0..pair.before.nodes.len() {
            let before = NodeId::new(index);
            if !self.mapping.is_moved(before) {
                continue;
            }
            let Some(after) = self.mapping.partner(before) else {
                continue;
            };
            if self.mapping.relation(before) == Some(Relation::Modified) {
                continue;
            }
            if !self.complete_before[index] || !self.complete_after[after.index()] {
                continue;
            }
            let node = pair.before.node(before);
            if !node.is_named() || zero_based_lines(&pair.before, before).is_empty() {
                continue;
            }
            self.moves.insert(before, after);
            self.move_targets.insert(after, before);
        }
        // Outer moves own their descendants' lines: a move nested in another move's origin, or
        // whose destination lies inside another move's destination, is that move's content.
        let nested_origins = self
            .moves
            .keys()
            .filter(|before| {
                has_ancestor(&pair.before, **before, |id| self.moves.contains_key(&id))
            })
            .copied()
            .collect::<Vec<_>>();
        self.forget_moves(&nested_origins);
        let nested_destinations = self
            .moves
            .iter()
            .filter(|(_, after)| {
                has_ancestor(&pair.after, **after, |id| {
                    self.move_targets.contains_key(&id)
                })
            })
            .map(|(before, _)| *before)
            .collect::<Vec<_>>();
        self.forget_moves(&nested_destinations);
        let outer = self.moves.keys().copied().collect::<Vec<_>>();
        let mut claimed_before = vec![false; pair.before.source.lines().len()];
        let mut claimed_after = vec![false; pair.after.source.lines().len()];
        for before in &outer {
            let after = self.moves[before];
            for line in zero_based_lines(&pair.before, *before) {
                claimed_before[line] = true;
            }
            for line in zero_based_lines(&pair.after, after) {
                claimed_after[line] = true;
            }
        }
        for before in outer {
            let after = self.moves[&before];
            let lines_before = zero_based_lines(&pair.before, before);
            let lines_after = zero_based_lines(&pair.after, after);
            let mut separators_before = blank_run(
                &self.weak_before,
                &claimed_before,
                &pair.before,
                lines_before.start,
                true,
            );
            separators_before.extend(blank_run(
                &self.weak_before,
                &claimed_before,
                &pair.before,
                lines_before.end,
                false,
            ));
            let mut separators_after = blank_run(
                &self.weak_after,
                &claimed_after,
                &pair.after,
                lines_after.start,
                true,
            );
            separators_after.extend(blank_run(
                &self.weak_after,
                &claimed_after,
                &pair.after,
                lines_after.end,
                false,
            ));
            let (mut all_before, mut all_after) = (
                lines_before.clone().collect::<Vec<_>>(),
                lines_after.clone().collect::<Vec<_>>(),
            );
            if separators_before.len() == separators_after.len() {
                all_before.extend(separators_before);
                all_after.extend(separators_after);
                all_before.sort_unstable();
                all_after.sort_unstable();
            }
            for line in &all_before {
                claimed_before[*line] = true;
                self.moved_before[*line] = true;
            }
            for line in &all_after {
                claimed_after[*line] = true;
                self.moved_after[*line] = true;
            }
            self.move_lines_before.insert(before, all_before);
            self.move_lines_after.insert(after, all_after);
        }
    }

    fn forget_moves(&mut self, origins: &[NodeId]) {
        for before in origins {
            if let Some(after) = self.moves.remove(before) {
                self.move_targets.remove(&after);
            }
        }
    }

    fn complete(&self, side: Side) -> &[bool] {
        match side {
            Side::Before => &self.complete_before,
            Side::After => &self.complete_after,
        }
    }

    /// Outermost named complete-line descendants of `root` lying within `lines`.
    fn atoms(&self, side: Side, root: NodeId, lines: &Range<usize>) -> Vec<NodeId> {
        let tree = self.pair.tree(side);
        let complete = self.complete(side);
        let mut atoms = Vec::new();
        let end = tree.subtree_end(root);
        let mut index = root.index() + 1;
        while index < end {
            let id = NodeId::new(index);
            let node = tree.node(id);
            if node.is_named() && complete[index] {
                let node_lines = zero_based_lines(tree, id);
                if !node_lines.is_empty()
                    && node_lines.start >= lines.start
                    && node_lines.end <= lines.end
                {
                    atoms.push(id);
                    index = tree.subtree_end(id);
                    continue;
                }
            }
            index += 1;
        }
        atoms
    }

    fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            kept: self.kept.len(),
            islands: self.islands.len(),
        }
    }

    /// Account for `before_lines` and `after_lines`, owned by the matched pair `(before, after)`.
    fn align_region(
        &mut self,
        before: NodeId,
        after: NodeId,
        before_lines: Range<usize>,
        after_lines: Range<usize>,
        owner: NodeId,
    ) {
        let atoms_before = self.atoms(Side::Before, before, &before_lines);
        let atoms_after = self.atoms(Side::After, after, &after_lines);
        self.align_atoms(atoms_before, atoms_after, before_lines, after_lines, owner);
    }

    /// Account for two line ranges given the complete-line atoms inside each.
    fn align_atoms(
        &mut self,
        atoms_before: Vec<NodeId>,
        atoms_after: Vec<NodeId>,
        before_lines: Range<usize>,
        after_lines: Range<usize>,
        owner: NodeId,
    ) {
        let pair = self.pair;
        let anchors = self.anchors(&atoms_before, &atoms_after);
        let mut cursor_before = before_lines.start;
        let mut cursor_after = after_lines.start;
        for anchor in anchors {
            let first_before = atoms_before[anchor.before.start];
            let last_before = atoms_before[anchor.before.end - 1];
            let first_after = atoms_after[anchor.after.start];
            let last_after = atoms_after[anchor.after.end - 1];
            let lines_before = zero_based_lines(&pair.before, first_before).start
                ..zero_based_lines(&pair.before, last_before).end;
            let lines_after = zero_based_lines(&pair.after, first_after).start
                ..zero_based_lines(&pair.after, last_after).end;
            self.align_gap(
                cursor_before..lines_before.start,
                cursor_after..lines_after.start,
                &atoms_before,
                &atoms_after,
                owner,
            );
            let direct = anchor.before.len() == 1
                && anchor.after.len() == 1
                && self.mapping.partner(first_before) == Some(first_after);
            match (direct, self.mapping.relation(first_before)) {
                (true, Some(Relation::Identical | Relation::Equivalent)) => {
                    self.reflow(
                        lines_before.clone(),
                        lines_after.clone(),
                        first_before,
                        first_after,
                    );
                }
                _ => {
                    let checkpoint = self.checkpoint();
                    // A wrapper that appeared keeps the before atoms themselves as the inner
                    // atoms; a wrapper that vanished keeps the after atoms. A direct pair
                    // descends into both.
                    let after_is_wrapper = anchor.after.len() == 1
                        && atoms_before[anchor.before.clone()].iter().all(|atom| {
                            self.mapping.partner(*atom).is_some_and(|partner| {
                                partner != first_after && pair.after.contains(first_after, partner)
                            })
                        });
                    let regions = if direct {
                        Regions {
                            inner_before: self.atoms(Side::Before, first_before, &lines_before),
                            inner_after: self.atoms(Side::After, first_after, &lines_after),
                            before: lines_before.clone(),
                            after: lines_after.clone(),
                        }
                    } else if after_is_wrapper {
                        let run = &atoms_before[anchor.before.clone()];
                        let (inner_before, inner_after, before, after) = self.wrapper_regions(
                            Side::After,
                            run,
                            first_after,
                            lines_before.clone(),
                            lines_after.clone(),
                        );
                        Regions {
                            inner_before,
                            inner_after,
                            before,
                            after,
                        }
                    } else {
                        let run = &atoms_after[anchor.after.clone()];
                        let (inner_after, inner_before, after, before) = self.wrapper_regions(
                            Side::Before,
                            run,
                            first_before,
                            lines_after.clone(),
                            lines_before.clone(),
                        );
                        Regions {
                            inner_before,
                            inner_after,
                            before,
                            after,
                        }
                    };
                    let region_owner = if anchor.after.len() == 1 {
                        first_after
                    } else {
                        owner
                    };
                    self.align_gap(
                        lines_before.start..regions.before.start,
                        lines_after.start..regions.after.start,
                        &atoms_before,
                        &atoms_after,
                        region_owner,
                    );
                    self.align_atoms(
                        regions.inner_before,
                        regions.inner_after,
                        regions.before.clone(),
                        regions.after.clone(),
                        region_owner,
                    );
                    self.align_gap(
                        regions.before.end..lines_before.end,
                        regions.after.end..lines_after.end,
                        &atoms_before,
                        &atoms_after,
                        region_owner,
                    );
                    self.collapse_if_shredded(
                        checkpoint,
                        lines_before.clone(),
                        lines_after.clone(),
                        region_owner,
                    );
                }
            }
            cursor_before = lines_before.end;
            cursor_after = lines_after.end;
        }
        self.align_gap(
            cursor_before..before_lines.end,
            cursor_after..after_lines.end,
            &atoms_before,
            &atoms_after,
            owner,
        );
    }

    /// What a run of atoms anchored to a wrapper on the other side aligns over, as (inner atoms
    /// of the run side, inner atoms of the wrapper side, run region, wrapper region). The run
    /// itself stays the inner atoms and the wrapper's contents face it. A single atom whose
    /// partner shares lines with the wrapper (a call that gained a `?` or a method chain) aligns
    /// against the innermost complete-line construct around that partner instead, so the
    /// wrapper's remaining lines read as its own addition or removal.
    fn wrapper_regions(
        &self,
        wrapper_side: Side,
        run: &[NodeId],
        wrapper: NodeId,
        run_lines: Range<usize>,
        wrapper_lines: Range<usize>,
    ) -> (Vec<NodeId>, Vec<NodeId>, Range<usize>, Range<usize>) {
        let run_side = wrapper_side.other();
        let tree = self.pair.tree(wrapper_side);
        let complete = self.complete(wrapper_side);
        let shares_lines = |partner: NodeId| {
            !complete[partner.index()] || zero_based_lines(tree, partner).is_empty()
        };
        match run {
            [atom]
                if let Some(partner) = self
                    .mapping
                    .counterpart(run_side, *atom)
                    .filter(|partner| shares_lines(*partner)) =>
            {
                let host = complete_host(tree, complete, partner, wrapper);
                let host_lines = zero_based_lines(tree, host);
                (
                    self.atoms(run_side, *atom, &run_lines),
                    self.atoms(wrapper_side, host, &host_lines),
                    run_lines,
                    host_lines,
                )
            }
            _ => (
                run.to_vec(),
                self.atoms(wrapper_side, wrapper, &wrapper_lines),
                run_lines,
                wrapper_lines,
            ),
        }
    }

    /// Pair atoms that anchor the alignment: direct partners, a run of atoms that a new
    /// wrapper now encloses, or a wrapper whose contents came out as a run of atoms.
    fn anchors(&self, atoms_before: &[NodeId], atoms_after: &[NodeId]) -> Vec<Anchor> {
        let pair = self.pair;
        let after_index = atoms_after
            .iter()
            .enumerate()
            .map(|(index, atom)| (*atom, index))
            .collect::<HashMap<_, _>>();
        let movable = |atom: NodeId| self.moves.contains_key(&atom) || self.mapping.is_moved(atom);
        let mut anchors = Vec::new();
        let mut used_before = vec![false; atoms_before.len()];
        let mut used_after = vec![false; atoms_after.len()];
        for (index, atom) in atoms_before.iter().enumerate() {
            if movable(*atom) {
                continue;
            }
            let Some(partner) = self.mapping.partner(*atom) else {
                continue;
            };
            if let Some(after) = after_index.get(&partner).copied() {
                used_before[index] = true;
                used_after[after] = true;
                anchors.push(Anchor {
                    before: index..index + 1,
                    after: after..after + 1,
                });
            }
        }
        // Before atoms whose partners live inside one unmatched after atom: a wrapper appeared.
        for (after, wrapper) in atoms_after.iter().enumerate() {
            if used_after[after] || self.mapping.is_matched(Side::After, *wrapper) {
                continue;
            }
            let inside = atoms_before
                .iter()
                .enumerate()
                .filter(|(index, atom)| {
                    !used_before[*index]
                        && !movable(**atom)
                        && self
                            .mapping
                            .partner(**atom)
                            .is_some_and(|partner| pair.after.contains(*wrapper, partner))
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            let Some((&first, &last)) = inside.first().zip(inside.last()) else {
                continue;
            };
            if inside.len() != last - first + 1
                || used_before[first..=last].iter().any(|used| *used)
            {
                continue;
            }
            used_before[first..=last].fill(true);
            used_after[after] = true;
            anchors.push(Anchor {
                before: first..last + 1,
                after: after..after + 1,
            });
        }
        // After atoms whose partners live inside one unmatched before atom: a wrapper vanished.
        for (before, wrapper) in atoms_before.iter().enumerate() {
            if used_before[before]
                || self.mapping.is_matched(Side::Before, *wrapper)
                || movable(*wrapper)
            {
                continue;
            }
            let inside = atoms_after
                .iter()
                .enumerate()
                .filter(|(index, atom)| {
                    !used_after[*index]
                        && self.mapping.partner_before(**atom).is_some_and(|partner| {
                            pair.before.contains(*wrapper, partner) && !movable(partner)
                        })
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            let Some((&first, &last)) = inside.first().zip(inside.last()) else {
                continue;
            };
            if inside.len() != last - first + 1 || used_after[first..=last].iter().any(|used| *used)
            {
                continue;
            }
            used_after[first..=last].fill(true);
            used_before[before] = true;
            anchors.push(Anchor {
                before: before..before + 1,
                after: first..last + 1,
            });
        }
        anchors.sort_by_key(|anchor| anchor.before.start);
        let order = anchors
            .iter()
            .map(|anchor| anchor.after.start)
            .collect::<Vec<_>>();
        let stable = super::align::increasing_subsequence_members(&order);
        anchors
            .into_iter()
            .zip(stable)
            .filter_map(|(anchor, stable)| stable.then_some(anchor))
            .collect()
    }

    fn keep_lines(&mut self, before: Range<usize>, after: Range<usize>) {
        let pair = self.pair;
        if before.len() != after.len() {
            // Identical source cannot differ in line count; fall back to a physical comparison.
            self.align_gap(before, after, &[], &[], pair.after.root);
            return;
        }
        let equal = before.clone().zip(after.clone()).all(|(before, after)| {
            pair.before.source.text(&pair.before.source.lines()[before])
                == pair.after.source.text(&pair.after.source.lines()[after])
        });
        if !equal {
            self.align_gap(before, after, &[], &[], pair.after.root);
            return;
        }
        for (before, after) in before.zip(after) {
            self.kept.push(Kept { before, after });
        }
    }

    /// Same tokens, different layout: keep equal lines and show the rest as reflow.
    fn reflow(
        &mut self,
        before: Range<usize>,
        after: Range<usize>,
        before_scope: NodeId,
        owner: NodeId,
    ) {
        let pair = self.pair;
        let before_text = before
            .clone()
            .map(|line| pair.before.source.text(&pair.before.source.lines()[line]))
            .collect::<Vec<_>>();
        let after_text = after
            .clone()
            .map(|line| pair.after.source.text(&pair.after.source.lines()[line]))
            .collect::<Vec<_>>();
        let mut matches = ordered_matches(&before_text, &after_text);
        let gaps_keep_tokens = {
            let mut cursor_before = 0;
            let mut cursor_after = 0;
            let mut consistent = true;
            for pair_ in matches
                .iter()
                .copied()
                .chain(std::iter::once(super::align::Pair {
                    before: before.len(),
                    after: after.len(),
                }))
            {
                let gap_before =
                    (before.start + cursor_before..before.start + pair_.before).collect::<Vec<_>>();
                let gap_after =
                    (after.start + cursor_after..after.start + pair_.after).collect::<Vec<_>>();
                if !super::syntax::content::same_layout_under(
                    &pair.before,
                    &gap_before,
                    before_scope,
                    &pair.after,
                    &gap_after,
                    owner,
                ) {
                    consistent = false;
                    break;
                }
                cursor_before = pair_.before + 1;
                cursor_after = pair_.after + 1;
            }
            consistent
        };
        if !gaps_keep_tokens {
            matches.clear();
        }
        let mut cursor_before = before.start;
        let mut cursor_after = after.start;
        for pair_ in matches
            .into_iter()
            .chain(std::iter::once(super::align::Pair {
                before: before.len(),
                after: after.len(),
            }))
        {
            let next_before = before.start + pair_.before;
            let next_after = after.start + pair_.after;
            let gap_before = (cursor_before..next_before).collect::<Vec<_>>();
            let gap_after = (cursor_after..next_after).collect::<Vec<_>>();
            if !gap_before.is_empty() || !gap_after.is_empty() {
                self.push_island(
                    Piece::Reflow(Reflow {
                        lines: LineChange {
                            before: gap_before,
                            after: gap_after,
                        },
                        before_scope,
                        after_scope: owner,
                    }),
                    Priority::Reflow,
                    owner,
                );
            }
            if next_before < before.end && next_after < after.end {
                self.keep_lines(next_before..next_before + 1, next_after..next_after + 1);
            }
            cursor_before = next_before + 1;
            cursor_after = next_after + 1;
        }
    }

    /// Account for the lines between two anchors: moves, removed or added atoms, and the
    /// owner's own lines, which align by text.
    fn align_gap(
        &mut self,
        before: Range<usize>,
        after: Range<usize>,
        atoms_before: &[NodeId],
        atoms_after: &[NodeId],
        owner: NodeId,
    ) {
        if before.is_empty() && after.is_empty() {
            return;
        }
        let pair = self.pair;
        let mut excluded_before = vec![false; before.len()];
        let mut excluded_after = vec![false; after.len()];
        let mut atom_lines_before = vec![false; before.len()];
        let mut atom_lines_after = vec![false; after.len()];
        let mut wiring_atoms = 0_usize;
        let mut atom_count = 0_usize;

        for atom in atoms_before.iter().copied() {
            let lines = zero_based_lines(&pair.before, atom);
            if lines.start < before.start || lines.end > before.end {
                continue;
            }
            atom_count += 1;
            if is_wiring(&pair.before, atom) {
                wiring_atoms += 1;
            }
            if let Some(destination) = self.moves.get(&atom).copied() {
                self.record_move(atom, destination, owner);
                continue;
            }
            for line in lines {
                atom_lines_before[line - before.start] = true;
            }
        }
        for line in before.clone() {
            excluded_before[line - before.start] = self.moved_before[line];
        }
        for line in after.clone() {
            excluded_after[line - after.start] = self.moved_after[line];
        }
        // Moves nested inside removed or added material are still announced here.
        let origins = self
            .move_lines_before
            .iter()
            .filter(|(_, lines)| lines.iter().any(|line| before.contains(line)))
            .map(|(origin, _)| *origin)
            .collect::<Vec<_>>();
        for origin in origins {
            let destination = self.moves[&origin];
            self.record_move(origin, destination, owner);
        }
        let destinations = self
            .move_lines_after
            .iter()
            .filter(|(_, lines)| lines.iter().any(|line| after.contains(line)))
            .map(|(destination, _)| *destination)
            .collect::<Vec<_>>();
        for destination in destinations {
            let origin = self.move_targets[&destination];
            self.record_move(origin, destination, owner);
        }
        for atom in atoms_after.iter().copied() {
            let lines = zero_based_lines(&pair.after, atom);
            if lines.start < after.start || lines.end > after.end {
                continue;
            }
            atom_count += 1;
            if is_wiring(&pair.after, atom) {
                wiring_atoms += 1;
            }
            if let Some(origin) = self.move_targets.get(&atom).copied() {
                self.record_move(origin, atom, owner);
                continue;
            }
            for line in lines {
                atom_lines_after[line - after.start] = true;
            }
        }

        // Strong fringe lines anchor by text; atom lines and weak lines never do.
        let fringe_before = before
            .clone()
            .filter(|line| {
                let offset = line - before.start;
                !excluded_before[offset] && !atom_lines_before[offset] && !self.weak_before[*line]
            })
            .collect::<Vec<_>>();
        let fringe_after = after
            .clone()
            .filter(|line| {
                let offset = line - after.start;
                !excluded_after[offset] && !atom_lines_after[offset] && !self.weak_after[*line]
            })
            .collect::<Vec<_>>();
        let before_text = fringe_before
            .iter()
            .map(|line| pair.before.source.text(&pair.before.source.lines()[*line]))
            .collect::<Vec<_>>();
        let after_text = fringe_after
            .iter()
            .map(|line| pair.after.source.text(&pair.after.source.lines()[*line]))
            .collect::<Vec<_>>();
        let anchors = ordered_matches(&before_text, &after_text)
            .into_iter()
            .map(|edge| (fringe_before[edge.before], fringe_after[edge.after]))
            .collect::<Vec<_>>();

        let priority =
            if atom_count > 0 && wiring_atoms == atom_count || is_wiring(&pair.after, owner) {
                Priority::Wiring
            } else {
                Priority::Edit
            };
        let mut cursor_before = before.start;
        let mut cursor_after = after.start;
        for (anchor_before, anchor_after) in anchors
            .into_iter()
            .chain(std::iter::once((before.end, after.end)))
        {
            let gap_before = (cursor_before..anchor_before)
                .filter(|line| !excluded_before[line - before.start])
                .collect::<Vec<_>>();
            let gap_after = (cursor_after..anchor_after)
                .filter(|line| !excluded_after[line - after.start])
                .collect::<Vec<_>>();
            let reordered_edit = atoms_before.iter().any(|atom| {
                self.mapping.is_moved(*atom)
                    && self.mapping.relation(*atom) == Some(Relation::Modified)
                    && zero_based_lines(&pair.before, *atom).any(|line| gap_before.contains(&line))
            });
            if reordered_edit && !gap_before.is_empty() && !gap_after.is_empty() {
                // Keeping edited reorders together; positional line pairing would swap their owners.
                self.push_island(
                    Piece::Block(LineChange {
                        before: gap_before,
                        after: gap_after,
                    }),
                    priority,
                    owner,
                );
            } else {
                self.emit_change(gap_before, gap_after, priority, owner);
            }
            if anchor_before < before.end && anchor_after < after.end {
                self.keep_lines(
                    anchor_before..anchor_before + 1,
                    anchor_after..anchor_after + 1,
                );
            }
            cursor_before = anchor_before + 1;
            cursor_after = anchor_after + 1;
        }
    }

    fn record_move(&mut self, origin: NodeId, destination: NodeId, owner: NodeId) {
        if !self.recorded_moves.insert(origin) {
            return;
        }
        let pair = self.pair;
        let before = self.move_lines_before[&origin].clone();
        let after = self.move_lines_after[&destination].clone();
        let priority = if is_wiring(&pair.before, origin) {
            Priority::Wiring
        } else {
            Priority::Move
        };
        let anchor_after = after.first().copied().unwrap_or(0);
        let anchor_before = before.first().copied().unwrap_or(0);
        self.islands.push(Island {
            priority,
            pieces: vec![Piece::Move(LineChange { before, after })],
            anchor_after,
            anchor_before,
            owner,
        });
    }

    /// Turn the lines between two anchors into pieces: shared edges stay kept, the middle changes.
    fn emit_change(
        &mut self,
        before: Vec<usize>,
        after: Vec<usize>,
        priority: Priority,
        owner: NodeId,
    ) {
        if before.is_empty() && after.is_empty() {
            return;
        }
        let edges = self.shared_edges(before, after);
        for (before, after) in edges.prefix {
            self.keep_lines(before..before + 1, after..after + 1);
        }
        // A deletion anchors after the last kept line, so the suffix is kept only once it is placed.
        if !edges.before.is_empty() || !edges.after.is_empty() {
            self.emit_piece(edges.before, edges.after, priority, owner);
        }
        for (before, after) in edges.suffix {
            self.keep_lines(before..before + 1, after..after + 1);
        }
    }

    /// Split a change into the line pairs equal at either end and the differing middle.
    fn shared_edges(&self, mut before: Vec<usize>, mut after: Vec<usize>) -> SharedEdges {
        let pair = self.pair;
        let before_text = before
            .iter()
            .map(|line| pair.before.source.text(&pair.before.source.lines()[*line]))
            .collect::<Vec<_>>();
        let after_text = after
            .iter()
            .map(|line| pair.after.source.text(&pair.after.source.lines()[*line]))
            .collect::<Vec<_>>();
        let (changed_before, changed_after) = changed_extent(&before_text, &after_text);
        debug_assert_eq!(changed_before.start, changed_after.start);
        let suffix = before
            .split_off(changed_before.end)
            .into_iter()
            .zip(after.split_off(changed_after.end))
            .collect();
        let prefix = before
            .drain(..changed_before.start)
            .zip(after.drain(..changed_after.start))
            .collect();
        SharedEdges {
            prefix,
            suffix,
            before,
            after,
        }
    }

    fn emit_piece(
        &mut self,
        before: Vec<usize>,
        after: Vec<usize>,
        priority: Priority,
        owner: NodeId,
    ) {
        if before.is_empty() && after.is_empty() {
            return;
        }
        let pair = self.pair;
        if before.is_empty() || after.is_empty() {
            self.push_island(Piece::Block(LineChange { before, after }), priority, owner);
            return;
        }
        let contiguous = after.windows(2).all(|pair| pair[1] == pair[0] + 1)
            && before.windows(2).all(|pair| pair[1] == pair[0] + 1);
        if contiguous
            && super::syntax::content::same_layout(&pair.before, &before, &pair.after, &after)
        {
            self.push_island(
                Piece::Reflow(Reflow {
                    lines: LineChange { before, after },
                    before_scope: pair.before.root,
                    after_scope: pair.after.root,
                }),
                Priority::Reflow,
                owner,
            );
            return;
        }
        let inline = before.len() == after.len()
            && before.iter().zip(&after).all(|(before, after)| {
                let before = pair
                    .before
                    .source
                    .text(&pair.before.source.lines()[*before]);
                let after = pair.after.source.text(&pair.after.source.lines()[*after]);
                before == after || inline_eligible(before, after)
            });
        if !inline {
            self.push_island(Piece::Block(LineChange { before, after }), priority, owner);
            return;
        }
        let mut run = LineChange {
            before: Vec::new(),
            after: Vec::new(),
        };
        for (before, after) in before.into_iter().zip(after) {
            let before_text = pair.before.source.text(&pair.before.source.lines()[before]);
            let after_text = pair.after.source.text(&pair.after.source.lines()[after]);
            if before_text != after_text {
                run.before.push(before);
                run.after.push(after);
                continue;
            }
            if !run.before.is_empty() {
                self.push_island(Piece::Inline(run), priority, owner);
                run = LineChange {
                    before: Vec::new(),
                    after: Vec::new(),
                };
            }
            self.keep_lines(before..before + 1, after..after + 1);
        }
        if !run.before.is_empty() {
            self.push_island(Piece::Inline(run), priority, owner);
        }
    }

    fn push_island(&mut self, piece: Piece, priority: Priority, owner: NodeId) {
        let lines = piece.change();
        let anchor_after = lines
            .after
            .first()
            .copied()
            .unwrap_or_else(|| self.next_position(Side::After));
        let anchor_before = lines
            .before
            .first()
            .copied()
            .unwrap_or_else(|| self.next_position(Side::Before));
        self.islands.push(Island {
            priority,
            pieces: vec![piece],
            anchor_after,
            anchor_before,
            owner,
        });
    }

    fn next_position(&self, side: Side) -> usize {
        let kept = self.kept.last().map(|kept| kept.line(side) + 1);
        let island = self
            .islands
            .iter()
            .rev()
            .find(|island| !island.is_move())
            .map(|island| {
                island
                    .lines(side)
                    .last()
                    .map_or(island.anchor(side), |line| line + 1)
            });
        kept.into_iter().chain(island).max().unwrap_or(0)
    }

    /// Replace a region's shredded interior with one block when little of it survived.
    fn collapse_if_shredded(
        &mut self,
        checkpoint: Checkpoint,
        before: Range<usize>,
        after: Range<usize>,
        owner: NodeId,
    ) {
        let inner = &self.islands[checkpoint.islands..];
        if inner.iter().any(Island::is_move)
            || before.clone().any(|line| self.moved_before[line])
            || after.clone().any(|line| self.moved_after[line])
        {
            return;
        }
        let edits = inner
            .iter()
            .flat_map(|island| &island.pieces)
            .filter(|piece| !matches!(piece, Piece::Reflow(_)))
            .collect::<Vec<_>>();
        if edits.len() < 2 {
            return;
        }
        let kept_inside = self.kept.len() - checkpoint.kept;
        let changed_before = edits
            .iter()
            .map(|piece| piece.change().before.len())
            .sum::<usize>();
        let changed_after = edits
            .iter()
            .map(|piece| piece.change().after.len())
            .sum::<usize>();
        let span = before.len().max(after.len());
        let shredded = edits.len() >= 3 && kept_inside < edits.len();
        let mostly_changed = span >= 4 && changed_before.max(changed_after) * 10 >= span * 6;
        if !shredded && !mostly_changed {
            return;
        }
        self.kept.truncate(checkpoint.kept);
        self.islands.truncate(checkpoint.islands);
        let priority = if is_wiring(&self.pair.after, owner) {
            Priority::Wiring
        } else {
            Priority::Edit
        };
        self.push_island(
            Piece::Block(LineChange {
                before: before.collect(),
                after: after.collect(),
            }),
            priority,
            owner,
        );
    }

    fn finish(mut self) -> Islands {
        self.kept
            .sort_unstable_by_key(|kept| (kept.before, kept.after));
        self.kept.dedup();
        self.islands
            .sort_by_key(|island| (island.anchor_after, island.anchor_before));
        let mut islands = merge_adjacent(
            &self.pair.after,
            &self.kept,
            std::mem::take(&mut self.islands),
        );
        self.keep_identical_edges(&mut islands);
        self.kept
            .sort_unstable_by_key(|kept| (kept.before, kept.after));
        self.kept.dedup();
        let endings = ending_notices(self.pair, &self.kept, &islands);
        Islands {
            kept: self.kept,
            islands,
            endings,
        }
    }

    /// A block formed by joining pieces may start or end with lines equal on both sides;
    /// those are kept lines, not a replacement.
    fn keep_identical_edges(&mut self, islands: &mut Vec<Island>) {
        for island in islands.iter_mut() {
            let mut trimmed = false;
            for piece in &mut island.pieces {
                let Piece::Block(piece) = piece else {
                    continue;
                };
                if piece.before.is_empty() || piece.after.is_empty() {
                    continue;
                }
                let edges = self.shared_edges(
                    std::mem::take(&mut piece.before),
                    std::mem::take(&mut piece.after),
                );
                if edges.prefix.is_empty() && edges.suffix.is_empty() {
                    (piece.before, piece.after) = (edges.before, edges.after);
                    continue;
                }
                for (before, after) in edges.prefix.into_iter().chain(edges.suffix) {
                    self.keep_lines(before..before + 1, after..after + 1);
                }
                (piece.before, piece.after) = (edges.before, edges.after);
                trimmed = true;
            }
            if !trimmed {
                continue;
            }
            island.pieces.retain(|piece| {
                !piece.change().before.is_empty() || !piece.change().after.is_empty()
            });
            if !island.pieces.is_empty() {
                let anchor_after = island.after_lines().next();
                let anchor_before = island.before_lines().next();
                island.anchor_after = anchor_after.unwrap_or(island.anchor_after);
                island.anchor_before = anchor_before.unwrap_or(island.anchor_before);
            }
        }
        islands.retain(|island| !island.pieces.is_empty());
    }
}

/// Account for terminators before presentation chooses rows or elisions.
fn ending_notices(
    pair: &SyntaxPair<'_, '_>,
    kept: &[Kept],
    islands: &[Island],
) -> Vec<EndingNotice> {
    let terminator = |side: Side, line: usize| Terminator {
        line: LineNumber::from_index(line),
        ending: pair.tree(side).source.lines()[line].ending,
    };
    let mut notices = Vec::new();
    let mut compare = |before: Option<usize>, after: Option<usize>, anchor: usize| {
        let before = before.map(|line| terminator(Side::Before, line));
        let after = after.map(|line| terminator(Side::After, line));
        let change = match (before, after) {
            (Some(before), Some(after)) if before.ending != after.ending => {
                EndingChange::Replaced { before, after }
            }
            (Some(before), None) if before.ending != LineEnding::Lf => {
                EndingChange::Removed(before)
            }
            (None, Some(after)) if after.ending != LineEnding::Lf => EndingChange::Added(after),
            _ => return,
        };
        notices.push(EndingNotice {
            change,
            anchor_after: anchor,
        });
    };
    for line in kept {
        compare(Some(line.before), Some(line.after), line.after);
    }
    for island in islands {
        for piece in &island.pieces {
            let lines = piece.change();
            if lines.before.len() == lines.after.len() {
                for (before, after) in lines.before.iter().zip(&lines.after) {
                    compare(Some(*before), Some(*after), *after);
                }
            } else {
                let before = lines.before.last().copied();
                let after = lines.after.last().copied();
                compare(before, after, after.unwrap_or(island.anchor_after));
            }
        }
    }
    notices.sort_by_key(|notice| notice.anchor_after);
    notices
}

/// Join islands with no kept line between them, and moved runs into one reorder.
fn merge_adjacent(after_tree: &SyntaxTree<'_>, kept: &[Kept], islands: Vec<Island>) -> Vec<Island> {
    let kept_after = kept.iter().map(|kept| kept.after).collect::<HashSet<_>>();
    let kept_before = kept.iter().map(|kept| kept.before).collect::<HashSet<_>>();
    let mut merged: Vec<Island> = Vec::new();
    for island in islands {
        let Some(previous) = merged.last_mut() else {
            merged.push(island);
            continue;
        };
        let touching = !previous.is_move()
            && !island.is_move()
            && touches(previous, &island, &kept_before, &kept_after);
        if !touching {
            merged.push(island);
            continue;
        }
        previous.owner = common_ancestor(after_tree, previous.owner, island.owner);
        previous.pieces.extend(island.pieces);
        previous.priority = previous.priority.max(island.priority);
    }
    for island in &mut merged {
        coalesce_pieces(island);
    }
    merged
}

/// Read touching changes as one before/after block rather than an interleaving.
/// Pieces are kept apart when their rows would not alternate between removed and added runs,
/// which keeps line-for-line edits, wrappers around retained text, and layout pieces readable.
fn coalesce_pieces(island: &mut Island) {
    if island.pieces.len() < 2 {
        return;
    }
    // Inline pieces render as paired rows and layout pieces as retained text; only blocks
    // can make removed lines follow added ones.
    let mut seen_added = false;
    let mut alternates = false;
    for piece in &island.pieces {
        let Piece::Block(piece) = piece else {
            continue;
        };
        if !piece.before.is_empty() && seen_added {
            alternates = true;
            break;
        }
        if !piece.after.is_empty() {
            seen_added = true;
        }
    }
    if !alternates {
        return;
    }
    let mut before = island.before_lines().collect::<Vec<_>>();
    let mut after = island.after_lines().collect::<Vec<_>>();
    before.sort_unstable();
    after.sort_unstable();
    island.pieces = vec![Piece::Block(LineChange { before, after })];
}

fn has_ancestor(
    tree: &SyntaxTree<'_>,
    id: NodeId,
    mut matches: impl FnMut(NodeId) -> bool,
) -> bool {
    let mut ancestor = tree.node(id).parent;
    while let Some(candidate) = ancestor {
        if matches(candidate) {
            return true;
        }
        ancestor = tree.node(candidate).parent;
    }
    false
}

/// The innermost complete-line construct enclosing `inner` within `wrapper`, else the wrapper.
fn complete_host(
    tree: &SyntaxTree<'_>,
    complete: &[bool],
    inner: NodeId,
    wrapper: NodeId,
) -> NodeId {
    let mut node = inner;
    loop {
        if node == wrapper || (complete[node.index()] && !zero_based_lines(tree, node).is_empty()) {
            return node;
        }
        match tree.node(node).parent {
            Some(parent) if tree.contains(wrapper, parent) => node = parent,
            _ => return wrapper,
        }
    }
}

/// Blank lines adjacent to a boundary that no move has claimed yet.
fn blank_run(
    weak: &[bool],
    claimed: &[bool],
    tree: &SyntaxTree<'_>,
    boundary: usize,
    leading: bool,
) -> Vec<usize> {
    let is_blank = |line: usize| {
        weak.get(line).copied().unwrap_or(false)
            && !claimed[line]
            && tree
                .source
                .text(&tree.source.lines()[line])
                .trim()
                .is_empty()
    };
    let mut lines = Vec::new();
    if leading {
        let mut line = boundary;
        while line > 0 && is_blank(line - 1) {
            line -= 1;
            lines.push(line);
        }
        lines.reverse();
    } else {
        let mut line = boundary;
        while line < claimed.len() && is_blank(line) {
            lines.push(line);
            line += 1;
        }
    }
    lines
}

fn common_ancestor(tree: &SyntaxTree<'_>, mut left: NodeId, right: NodeId) -> NodeId {
    while !tree.contains(left, right) {
        left = tree
            .node(left)
            .parent
            .expect("the file contains every syntax occurrence");
    }
    left
}

/// Two islands touch when no kept line separates them on either side.
fn touches(
    left: &Island,
    right: &Island,
    kept_before: &HashSet<usize>,
    kept_after: &HashSet<usize>,
) -> bool {
    let after_end = left
        .after_lines()
        .last()
        .map(|line| line + 1)
        .unwrap_or(left.anchor_after);
    let before_end = left
        .before_lines()
        .last()
        .map(|line| line + 1)
        .unwrap_or(left.anchor_before);
    let after_gap_clear = (after_end..right.anchor_after).all(|line| !kept_after.contains(&line));
    let before_gap_clear =
        (before_end..right.anchor_before).all(|line| !kept_before.contains(&line));
    right.anchor_after >= after_end
        && right.anchor_before >= before_end
        && after_gap_clear
        && before_gap_clear
        && (right.anchor_after == after_end || right.anchor_before == before_end)
}

fn summarize(island: &Island) -> Change {
    let has_before = island.before_lines().next().is_some();
    let has_after = island.after_lines().next().is_some();
    let all_reflow = island
        .pieces
        .iter()
        .all(|piece| matches!(piece, Piece::Reflow(_)));
    match (has_before, has_after) {
        (false, false) => Change::Reflow,
        (true, false) => Change::Deletion,
        (false, true) => Change::Addition,
        (true, true) if all_reflow => Change::Reflow,
        (true, true) => Change::Replacement,
    }
}

pub(crate) fn zero_based_lines(tree: &SyntaxTree<'_>, node: NodeId) -> Range<usize> {
    let lines = tree.node(node).lines.clone();
    let count = tree.source.lines().len();
    let start = lines.start.index().min(count);
    let end = lines.end.index().min(count);
    start.min(end)..end
}

fn is_wiring(tree: &SyntaxTree<'_>, node: NodeId) -> bool {
    tree.node(node).review == Some(ReviewRole::Wiring)
}

/// Blank or punctuation-only lines carry no evidence of correspondence on their own.
fn weak_lines(tree: &SyntaxTree<'_>) -> Vec<bool> {
    tree.source
        .lines()
        .iter()
        .map(|line| {
            let text = tree.source.text(line).trim();
            text.len() <= 3
                || text
                    .chars()
                    .all(|character| character.is_ascii_punctuation() || character.is_whitespace())
        })
        .collect()
}

/// A line pair reads as one edited line when most of its characters survive in order.
fn inline_eligible(before: &str, after: &str) -> bool {
    let before_chars = before.trim().chars().collect::<Vec<_>>();
    let after_chars = after.trim().chars().collect::<Vec<_>>();
    if before_chars.is_empty() || after_chars.is_empty() {
        return false;
    }
    let shared = ordered_matches(&before_chars, &after_chars)
        .into_iter()
        .filter(|edge| !before_chars[edge.before].is_whitespace())
        .count();
    let longer = before_chars
        .iter()
        .filter(|character| !character.is_whitespace())
        .count()
        .max(
            after_chars
                .iter()
                .filter(|character| !character.is_whitespace())
                .count(),
        );
    shared >= 4 && shared * 2 >= longer
}

/// Check the account against the text; the error names the first violation.
pub fn verify(pair: &SyntaxPair<'_, '_>, islands: &Islands) -> Result<(), String> {
    let before_lines = pair.before.source.lines();
    let after_lines = pair.after.source.lines();
    let text_before = |line: usize| pair.before.source.text(&before_lines[line]);
    let text_after = |line: usize| pair.after.source.text(&after_lines[line]);
    let mut before_owner = vec![None::<&str>; before_lines.len()];
    let mut after_owner = vec![None::<&str>; after_lines.len()];
    let claim = |owners: &mut Vec<Option<&str>>,
                 line: usize,
                 what: &'static str,
                 side: &str|
     -> Result<(), String> {
        let Some(slot) = owners.get_mut(line) else {
            return Err(format!(
                "{what} names {side} line {} beyond the file",
                line + 1
            ));
        };
        let previous = slot.replace(what);
        if let Some(previous) = previous {
            return Err(format!(
                "{side} line {} claimed by {previous} and {what}",
                line + 1
            ));
        }
        Ok(())
    };

    let mut last_kept = None::<Kept>;
    for kept in &islands.kept {
        claim(&mut before_owner, kept.before, "kept", "before")?;
        claim(&mut after_owner, kept.after, "kept", "after")?;
        if text_before(kept.before) != text_after(kept.after) {
            return Err(format!(
                "kept pair {}:{} differs: {:?} vs {:?}",
                kept.before + 1,
                kept.after + 1,
                text_before(kept.before),
                text_after(kept.after)
            ));
        }
        if let Some(previous) = last_kept
            && (previous.before >= kept.before || previous.after >= kept.after)
        {
            return Err(format!(
                "kept pairs out of order: {}:{} then {}:{}",
                previous.before + 1,
                previous.after + 1,
                kept.before + 1,
                kept.after + 1
            ));
        }
        last_kept = Some(*kept);
    }

    for notice in &islands.endings {
        for (side, terminator) in [
            (Side::Before, notice.change.before()),
            (Side::After, notice.change.after()),
        ] {
            if let Some(terminator) = terminator {
                let actual = pair.tree(side).source.line(terminator.line);
                if !actual.is_some_and(|line| line.ending == terminator.ending) {
                    return Err(format!(
                        "terminator notice does not reproduce {side:?} line {}",
                        terminator.line
                    ));
                }
            }
        }
    }
    let gap_of = |kept: &[Kept], line: usize, before_side: bool| -> usize {
        kept.partition_point(|kept| {
            if before_side {
                kept.before < line
            } else {
                kept.after < line
            }
        })
    };
    let mut last_anchor = (0_usize, 0_usize);
    for island in &islands.islands {
        if island.pieces.is_empty() {
            return Err("island without pieces".to_owned());
        }
        for presentation in &island.pieces {
            let piece = presentation.change();
            if piece.before.is_empty() && piece.after.is_empty() {
                return Err("empty piece".to_owned());
            }
            let what = match presentation {
                Piece::Block(_) => "block",
                Piece::Inline(_) => "inline",
                Piece::Reflow(_) => "reflow",
                Piece::Move(_) => "move",
            };
            for line in &piece.before {
                claim(&mut before_owner, *line, what, "before")?;
            }
            for line in &piece.after {
                claim(&mut after_owner, *line, what, "after")?;
            }
            if piece.before.windows(2).any(|pair| pair[0] >= pair[1])
                || piece.after.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err(format!("{what} piece lines are not ascending"));
            }
            let before_text = piece
                .before
                .iter()
                .map(|line| text_before(*line))
                .collect::<Vec<_>>();
            let after_text = piece
                .after
                .iter()
                .map(|line| text_after(*line))
                .collect::<Vec<_>>();
            match presentation {
                Piece::Block(_) if !piece.before.is_empty() && !piece.after.is_empty() => {
                    if before_text == after_text {
                        return Err(format!(
                            "block replaces lines {}.. with identical text {:?}",
                            piece.before[0] + 1,
                            before_text[0]
                        ));
                    }
                }
                Piece::Block(_) => {}
                Piece::Inline(_) => {
                    if piece.before.len() != piece.after.len() {
                        return Err("inline piece with unequal sides".to_owned());
                    }
                    for (before, after) in before_text.iter().zip(&after_text) {
                        if before == after {
                            return Err(format!("inline piece pairs identical lines {before:?}"));
                        }
                    }
                }
                Piece::Reflow(_) | Piece::Move(_) => {
                    let preserved = match presentation {
                        Piece::Reflow(reflow) => super::syntax::content::same_layout_under(
                            &pair.before,
                            &piece.before,
                            reflow.before_scope,
                            &pair.after,
                            &piece.after,
                            reflow.after_scope,
                        ),
                        Piece::Move(_) => super::syntax::content::same_tokens(
                            &pair.before,
                            &piece.before,
                            &pair.after,
                            &piece.after,
                        ),
                        _ => unreachable!(),
                    };
                    if !preserved {
                        return Err(format!(
                            "{what} piece changes tokens at before line {}",
                            piece.before.first().map_or(0, |line| line + 1)
                        ));
                    }
                    if matches!(presentation, Piece::Reflow(_)) && before_text == after_text {
                        return Err("reflow piece with identical text".to_owned());
                    }
                }
            }
            if !matches!(presentation, Piece::Move(_)) {
                // Every line of a piece lies in the same gap between kept pairs on both sides.
                let gaps_before = piece
                    .before
                    .iter()
                    .map(|line| gap_of(&islands.kept, *line, true))
                    .collect::<HashSet<_>>();
                let gaps_after = piece
                    .after
                    .iter()
                    .map(|line| gap_of(&islands.kept, *line, false))
                    .collect::<HashSet<_>>();
                let mut gaps = gaps_before.union(&gaps_after).copied().collect::<Vec<_>>();
                gaps.sort_unstable();
                if gaps.len() > 1 {
                    return Err(format!(
                        "{what} piece spans kept pairs (before {:?}, after {:?})",
                        piece.before.first().map(|line| line + 1),
                        piece.after.first().map(|line| line + 1)
                    ));
                }
            }
        }
        if !island.is_move() {
            let anchor = (island.anchor_after, island.anchor_before);
            if anchor < last_anchor {
                return Err(format!(
                    "islands out of order at after line {}",
                    island.anchor_after + 1
                ));
            }
            last_anchor = anchor;
        }
    }

    if let Some(line) = before_owner.iter().position(Option::is_none) {
        return Err(format!(
            "before line {} is unaccounted for: {:?}",
            line + 1,
            text_before(line)
        ));
    }
    if let Some(line) = after_owner.iter().position(Option::is_none) {
        return Err(format!(
            "after line {} is unaccounted for: {:?}",
            line + 1,
            text_after(line)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn lower<'a>(path: &str, before: &'a str, after: &'a str) -> (SyntaxPair<'a, 'a>, Islands) {
        let pair = super::super::syntax::syntax_pair(Path::new(path), before, after, false)
            .expect("syntax");
        let mapping = super::super::matching::correspond(&pair);
        let islands = islands(&pair, &mapping);
        if let Err(error) = verify(&pair, &islands) {
            panic!("{error}\n{islands:#?}");
        }
        (pair, islands)
    }

    #[test]
    fn unchanged_statements_around_an_insertion_are_kept() {
        let (_, islands) = lower(
            "alpha.rs",
            "fn a() {\n    one();\n    two();\n}\n",
            "fn a() {\n    one();\n    inserted();\n    two();\n}\n",
        );
        assert_eq!(islands.islands.len(), 1);
        let island = &islands.islands[0];
        assert_eq!(island.change(), Change::Addition);
        assert_eq!(island.pieces[0].change().after, [2]);
        assert_eq!(islands.kept.len(), 4);
    }

    #[test]
    fn rewritten_statement_is_one_replacement() {
        let (_, islands) = lower(
            "alpha.rs",
            "fn a() {\n    let x = compute(1);\n    use_it(x);\n}\n",
            "fn a() {\n    let y = compute(2);\n    use_it(y);\n}\n",
        );
        assert_eq!(islands.islands.len(), 1, "{islands:#?}");
        let island = &islands.islands[0];
        assert_eq!(island.change(), Change::Replacement);
        assert_eq!(
            island
                .pieces
                .iter()
                .map(|piece| piece.change().before.len())
                .sum::<usize>(),
            2
        );
    }

    #[test]
    fn reordered_functions_are_moves() {
        let (_, islands) = lower(
            "alpha.rs",
            "fn a() {\n    1;\n}\n\nfn b() {\n    2;\n}\n",
            "fn b() {\n    2;\n}\n\nfn a() {\n    1;\n}\n",
        );
        assert!(
            islands
                .islands
                .iter()
                .any(|island| island.change() == Change::Move),
            "{islands:#?}"
        );
        assert!(
            islands
                .islands
                .iter()
                .all(|island| island.change() == Change::Move),
            "{islands:#?}"
        );
    }

    #[test]
    fn layout_only_change_is_reflow() {
        let (_, islands) = lower(
            "alpha.rs",
            "fn a() { beta(); }\n",
            "fn a() {\n    beta();\n}\n",
        );
        assert_eq!(islands.islands.len(), 1);
        assert_eq!(islands.islands[0].change(), Change::Reflow);
    }

    #[test]
    fn plain_text_uses_line_alignment() {
        let (_, islands) = lower("notes.txt", "alpha\nbeta\ngamma\n", "alpha\ngamma\ndelta\n");
        assert_eq!(islands.kept.len(), 2);
        assert_eq!(islands.islands.len(), 2);
        assert_eq!(islands.islands[0].change(), Change::Deletion);
        assert_eq!(islands.islands[1].change(), Change::Addition);
    }

    #[test]
    fn whole_file_addition_and_deletion() {
        let (_, added) = lower("alpha.rs", "", "fn a() {}\n");
        assert_eq!(added.islands.len(), 1);
        assert_eq!(added.islands[0].change(), Change::Addition);
        let (_, removed) = lower("alpha.rs", "fn a() {}\n", "");
        assert_eq!(removed.islands[0].change(), Change::Deletion);
    }

    #[test]
    fn a_partner_sharing_lines_with_its_wrapper_aligns_against_the_enclosing_construct() {
        let (_, islands) = lower(
            "alpha.rs",
            "fn a() {\n    host(|| {\n        resolve(\n            alpha,\n            beta,\n        )\n    })\n}\n",
            "fn a() {\n    host(|| {\n        resolve(\n            alpha,\n            beta,\n        )?\n        .into_js(global)\n    })\n}\n",
        );
        for line in 2..5 {
            assert!(
                islands
                    .kept
                    .iter()
                    .any(|kept| kept.before == line && kept.after == line),
                "line {line} is not kept\n{islands:#?}"
            );
        }
        assert_eq!(islands.islands.len(), 1, "{islands:#?}");
        let island = &islands.islands[0];
        assert_eq!(
            island.before_lines().collect::<Vec<_>>(),
            [5],
            "{islands:#?}"
        );
        assert_eq!(
            island.after_lines().collect::<Vec<_>>(),
            [5, 6],
            "{islands:#?}"
        );
    }
}
