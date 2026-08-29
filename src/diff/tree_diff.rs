//! Form source-coordinate changes from neutral syntax correspondence.
//!
//! This storey owns source-complete change facts. It does not assign review priority,
//! context halos, breadcrumbs, or presentation order.

use super::context::ranges_overlap;
use super::correspondence::{
    Correspondence, LeafRelation, LineLink, MatchedUnit, NodeLink, ParentCorrespondence, Placement,
    UnitEdit,
};
use super::source::SourceLine;
use super::syntax::{
    ComparisonStrategy, ContentChannel, NodeId, SourceRole, SyntaxPair, SyntaxTree,
    horizontal_layout, node_owns_complete_lines,
};
use super::{LineCoverage, LineEnding};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// Semantic nature of one source change before review priority is assigned.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum ChangeNature {
    Reflow,
    Wiring,
    Move,
    Edit,
}

/// Revision selected by a source-space operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RevisionSide {
    Before,
    After,
}

/// One numbered source line plus the changed byte intervals it owns.
#[derive(Clone, Debug)]
pub(super) struct SelectedLine {
    pub(super) number: usize,
    pub(super) highlights: Vec<Range<usize>>,
}

impl SelectedLine {
    pub(super) fn has_changes(&self) -> bool {
        !self.highlights.is_empty()
    }

    fn clear_changes(&mut self) {
        self.highlights.clear();
    }
}

/// A row-sized source event retained without allocating styled presentation text.
#[derive(Clone, Debug)]
enum RowEvent {
    Current(SelectedLine),
    Reflow(SelectedLine),
    Removed(SelectedLine),
    Added(SelectedLine),
    LineEnding(LineEndingChange),
    Moved {
        before: Option<usize>,
        after: SelectedLine,
    },
    Compact(CompactChange),
    Elision(LineCoverage),
}

/// One indivisible source change handed to review refinement.
///
/// Both revisions of a replacement remain in the same value until presentation,
/// so ordering, coalescing, and context selection cannot separate their meanings.
#[derive(Clone, Debug)]
pub(super) enum SourceChange {
    Current(SelectedLine),
    Reflow(SelectedLine),
    Replace {
        before: Vec<SelectedLine>,
        after: Vec<SelectedLine>,
        line_endings: Vec<LineEndingChange>,
    },
    LineEnding(LineEndingChange),
    Moved {
        before: Option<usize>,
        after: SelectedLine,
    },
    Compact(CompactChange),
    Elision(LineCoverage),
}

/// A source-located change whose placement is fixed before refinement.
#[derive(Clone, Debug)]
pub(super) struct SourceFact {
    pub(super) change: SourceChange,
    pub(super) coverage: LineCoverage,
    pub(super) order: SourceOrder,
    pub(super) script_order: usize,
    /// Current-world lines physically emitted by this fact.
    pub(super) displayed_after: Vec<usize>,
}

impl SourceFact {
    pub(super) fn is_context(&self) -> bool {
        matches!(&self.change, SourceChange::Current(line) if !line.has_changes())
    }

    pub(super) fn has_signal(&self) -> bool {
        !self.is_context() && !matches!(self.change, SourceChange::Elision(_))
    }

    pub(super) fn context_line(&self) -> Option<usize> {
        match &self.change {
            SourceChange::Current(line) if !line.has_changes() => Some(line.number),
            _ => None,
        }
    }
}

/// One compact replacement retained solely as source coordinates until presentation.
#[derive(Clone, Debug)]
pub(super) struct CompactChange {
    pub(super) before_line: Option<usize>,
    pub(super) after_line: Option<usize>,
    pub(super) before_bytes: Option<Range<usize>>,
    pub(super) after_bytes: Option<Range<usize>>,
}

/// One concrete line terminator and the source row that owns it.
#[derive(Clone, Copy, Debug)]
pub(super) struct LineEndingEndpoint {
    pub(super) line: usize,
    pub(super) ending: LineEnding,
}

/// Before/after provenance for one changed line terminator.
#[derive(Clone, Copy, Debug)]
pub(super) struct LineEndingChange {
    pub(super) before: Option<LineEndingEndpoint>,
    pub(super) after: Option<LineEndingEndpoint>,
}

fn ending_endpoint(line: &SourceLine) -> LineEndingEndpoint {
    LineEndingEndpoint {
        line: line.number,
        ending: line.ending,
    }
}

/// Select changed byte intervals on one source line without allocating its text.
pub(super) fn select_line(line: &SourceLine, changed: &[Range<usize>]) -> SelectedLine {
    let bytes = line.content_bytes.clone();
    let mut highlights = Vec::<Range<usize>>::new();
    for range in changed {
        if !ranges_overlap(range, &bytes) {
            continue;
        }
        highlights.push(range.start.max(bytes.start)..range.end.min(bytes.end));
    }
    highlights.sort_unstable_by_key(|range| range.start);
    let mut merged = Vec::<Range<usize>>::with_capacity(highlights.len());
    for highlight in highlights {
        if let Some(previous) = merged.last_mut()
            && highlight.start <= previous.end
        {
            previous.end = previous.end.max(highlight.end);
            continue;
        }
        merged.push(highlight);
    }
    SelectedLine {
        number: line.number,
        highlights: merged,
    }
}

fn select_lines(
    syntax: &SyntaxTree<'_>,
    lines: Range<usize>,
    changed: &[Range<usize>],
) -> Vec<SelectedLine> {
    lines
        .filter_map(|number| syntax.source.line(number))
        .map(|line| select_line(line, changed))
        .collect()
}

/// Transient producer rows that share one syntax context root.
struct FormedExcerpt {
    rows: Vec<RowEvent>,
    context_root: Option<NodeId>,
}

/// One classified producer fragment in its native before/after source coordinates.
struct FormedHunk {
    nature: ChangeNature,
    rows: Vec<RowEvent>,
    order_hint: Option<SourceOrder>,
    context_root: Option<NodeId>,
}

impl FormedHunk {
    fn new(nature: ChangeNature, excerpt: FormedExcerpt) -> Self {
        Self {
            nature,
            rows: excerpt.rows,
            order_hint: None,
            context_root: excerpt.context_root,
        }
    }

    /// Semantic-unit placement for a before-only fragment with no current source rows.
    fn at_order(mut self, order: SourceOrder) -> Self {
        self.order_hint = Some(order);
        self
    }

    fn edit(excerpt: FormedExcerpt) -> Self {
        Self::new(ChangeNature::Edit, excerpt)
    }

    /// Low-signal source wiring selected explicitly by the language frontend.
    fn wiring(excerpt: FormedExcerpt) -> Self {
        Self::new(ChangeNature::Wiring, excerpt)
    }

    fn moved(excerpt: FormedExcerpt) -> Self {
        Self::new(ChangeNature::Move, excerpt)
    }

    fn reflow(excerpt: FormedExcerpt) -> Self {
        Self::new(ChangeNature::Reflow, excerpt)
    }
}

/// Physical signal lines retained before context halos or breadcrumbs expand selection.
#[derive(Clone, Default)]
pub(super) struct SourceFocus {
    pub(super) before: Vec<usize>,
    pub(super) after: Vec<usize>,
}

impl SourceFocus {
    pub(super) fn after_line(line: usize) -> Self {
        Self {
            before: Vec::new(),
            after: vec![line],
        }
    }

    fn from_rows<'row>(rows: impl IntoIterator<Item = &'row RowEvent>) -> Self {
        let mut focus = Self::default();
        for row in rows {
            match row {
                RowEvent::Current(line) if line.has_changes() => focus.after.push(line.number),
                RowEvent::Reflow(line) => focus.after.push(line.number),
                RowEvent::Removed(line) => focus.before.push(line.number),
                RowEvent::Added(line) => focus.after.push(line.number),
                RowEvent::Moved { before, after } => {
                    focus.before.extend(*before);
                    focus.after.push(after.number);
                }
                RowEvent::Compact(word) => {
                    focus.before.extend(word.before_line);
                    focus.after.extend(word.after_line);
                }
                RowEvent::LineEnding(change) => {
                    focus
                        .before
                        .extend(change.before.map(|endpoint| endpoint.line));
                    focus
                        .after
                        .extend(change.after.map(|endpoint| endpoint.line));
                }
                RowEvent::Current(_) | RowEvent::Elision(_) => {}
            }
        }
        focus.normalize();
        focus
    }

    pub(super) fn merge(&mut self, mut other: Self) {
        self.before.append(&mut other.before);
        self.after.append(&mut other.after);
    }

    fn normalize(&mut self) {
        self.before.sort_unstable();
        self.before.dedup();
        self.after.sort_unstable();
        self.after.dedup();
    }

    pub(super) fn is_empty(&self) -> bool {
        self.before.is_empty() && self.after.is_empty()
    }
}

/// Owned physical-line map shared by raw facts and context refinement.
pub(super) struct SourceMap {
    before_order: Vec<SourceOrder>,
    after_to_before: Vec<Option<usize>>,
    after_lines: usize,
    after_script_order: Vec<usize>,
}

/// Unit-script ownership retained after nearby fragments meld into one visual hunk.
struct SourceSequence {
    before_owner: Vec<Option<usize>>,
    after_owner: Vec<Option<usize>>,
    after_units: Vec<(Range<usize>, usize)>,
}

impl SourceSequence {
    fn new(pair: &SyntaxPair<'_, '_>, correspondence: &Correspondence) -> Self {
        let mut sequence = Self {
            before_owner: vec![None; pair.before.source.lines().len() + 1],
            after_owner: vec![None; pair.after.source.lines().len() + 1],
            after_units: Vec::new(),
        };
        for (order, edit) in correspondence.units.iter().enumerate() {
            match edit {
                UnitEdit::Matched(unit) => {
                    sequence.claim_before(pair.before.node(unit.before).lines.clone(), order);
                    sequence.claim_after(pair.after.node(unit.after).lines.clone(), order);
                }
                UnitEdit::Removed { before } => {
                    sequence.claim_before(pair.before.node(*before).lines.clone(), order);
                }
                UnitEdit::Added { after } => {
                    sequence.claim_after(pair.after.node(*after).lines.clone(), order);
                }
            }
        }
        sequence.after_units.sort_by_key(|(lines, _)| lines.start);
        sequence
    }

    fn claim_before(&mut self, lines: Range<usize>, order: usize) {
        for line in lines {
            let Some(owner) = self.before_owner.get_mut(line) else {
                continue;
            };
            // Adjacent review units can share a physical line. Changed overlaps are
            // formed by a local line fallback; unchanged overlaps retain the first
            // unit's edit-script position as their common presentation anchor.
            owner.get_or_insert(order);
        }
    }

    fn claim_after(&mut self, lines: Range<usize>, order: usize) {
        for line in lines.clone() {
            let Some(owner) = self.after_owner.get_mut(line) else {
                continue;
            };
            // Mirroring before-world ownership keeps shared-line context stable
            // across revisions without inventing a second display row.
            owner.get_or_insert(order);
        }
        if !lines.is_empty() {
            self.after_units.push((lines, order));
        }
    }

    fn group_rank(&self, group: &[RowEvent], source_map: &SourceMap) -> usize {
        let mut owners = group.iter().flat_map(|row| {
            let after = row_after_source_line(row)
                .and_then(|line| self.after_owner.get(line).copied().flatten());
            let before = row_before_source_line(row)
                .and_then(|line| self.before_owner.get(line).copied().flatten());
            [after, before].into_iter().flatten()
        });
        let owner = owners.next();
        debug_assert!(owners.all(|candidate| Some(candidate) == owner));
        if let Some(owner) = owner {
            return owner.saturating_mul(2).saturating_add(2);
        }

        let after_line = group
            .iter()
            .filter_map(row_after_source_line)
            .min()
            .or_else(|| {
                group
                    .iter()
                    .filter_map(row_before_source_line)
                    .filter_map(|line| source_map.current_anchor(line))
                    .min()
            })
            .unwrap_or(1);
        self.unowned_after_rank(after_line)
    }

    fn unowned_after_rank(&self, line: usize) -> usize {
        let mut preceding = None;
        for (lines, order) in &self.after_units {
            if lines.contains(&line) {
                return order.saturating_mul(2).saturating_add(2);
            }
            if lines.end <= line {
                preceding = Some(*order);
                continue;
            }
            if let Some(preceding) = preceding {
                return preceding.saturating_mul(2).saturating_add(3);
            }
            return order.saturating_mul(2).saturating_add(1);
        }
        preceding.map_or(1, |order| order.saturating_mul(2).saturating_add(3))
    }
}

/// Current-world gap plus ordering inside that gap.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct SourceOrder {
    /// Number of current-world lines preceding this source fact.
    pub(super) after_gap: usize,
    /// Old source line for a deletion, or `usize::MAX` for current-world source.
    pub(super) tie_break: usize,
}

impl SourceOrder {
    pub(super) fn current(line: usize) -> Self {
        Self {
            after_gap: line.saturating_sub(1),
            tie_break: usize::MAX,
        }
    }

    fn deletion(after_gap: usize, before_line: usize) -> Self {
        Self {
            after_gap,
            tie_break: before_line,
        }
    }
}

/// One unranked source hunk with fixed source placement.
pub(super) struct RawHunk {
    pub(super) nature: ChangeNature,
    pub(super) facts: Vec<SourceFact>,
    pub(super) order: SourceOrder,
    /// Explicit current-world context anchor for a before-only producer.
    pub(super) context_anchor: Option<usize>,
    pub(super) context_root: Option<NodeId>,
}

/// Compiler-visible handoff from tree differencing to review refinement.
pub(super) struct RawHunks {
    pub(super) hunks: Vec<RawHunk>,
    pub(super) source_map: SourceMap,
    pub(super) before_lines: usize,
    pub(super) after_lines: usize,
}

impl SourceMap {
    fn new(graph: &Correspondence, before_lines: usize, after_lines: usize) -> Self {
        let mut before_to_after = vec![None; before_lines + 1];
        let mut after_to_before = vec![None; after_lines + 1];
        for link in graph.line_links.iter().chain(&graph.line_ending_edits) {
            let before = link.before + 1;
            let after = link.after + 1;
            if let Some(mapped) = before_to_after.get_mut(before) {
                *mapped = Some(mapped.map_or(after, |mapped: usize| mapped.min(after)));
            }
            if let Some(mapped) = after_to_before.get_mut(after) {
                *mapped = Some(mapped.map_or(before, |mapped: usize| mapped.min(before)));
            }
        }

        let mut preceding_after = 0;
        let mut before_order = Vec::with_capacity(before_lines + 1);
        before_order.push(SourceOrder::deletion(0, 0));
        for (before, after) in before_to_after.into_iter().enumerate().skip(1) {
            let order = match after {
                Some(after) => {
                    preceding_after = after;
                    SourceOrder::current(after)
                }
                None => SourceOrder::deletion(preceding_after, before),
            };
            before_order.push(order);
        }

        Self {
            before_order,
            after_to_before,
            after_lines,
            after_script_order: (0..=after_lines)
                .map(|line| line.saturating_sub(1))
                .collect(),
        }
    }

    /// Unmatched old lines occupy an explicit gap; exact lines share current order.
    pub(super) fn before_order(&self, line: usize) -> SourceOrder {
        self.before_order
            .get(line)
            .copied()
            .unwrap_or_else(|| SourceOrder::deletion(0, line))
    }

    pub(super) fn current_anchor(&self, before_line: usize) -> Option<usize> {
        if self.after_lines == 0 {
            return None;
        }
        let gap = self.before_order(before_line).after_gap;
        let line = gap.saturating_add(1);
        Some(line.clamp(1, self.after_lines))
    }

    pub(super) fn aligned_before_line(&self, after_line: usize) -> Option<usize> {
        self.after_to_before.get(after_line).copied().flatten()
    }

    pub(super) fn focus_order(&self, focus: &SourceFocus) -> SourceOrder {
        focus
            .after
            .iter()
            .map(|line| SourceOrder::current(*line))
            .min()
            .or_else(|| {
                focus
                    .before
                    .iter()
                    .map(|line| self.before_order(*line))
                    .min()
            })
            .expect("classified change owns at least one signal line")
    }

    pub(super) fn current_focus(&self, focus: &SourceFocus) -> Option<Range<usize>> {
        let lines = if focus.after.is_empty() {
            focus
                .before
                .iter()
                .filter_map(|line| self.current_anchor(*line))
                .collect::<Vec<_>>()
        } else {
            focus.after.to_vec()
        };
        let start = lines.iter().copied().min()?;
        let end = lines.iter().copied().max()?.saturating_add(1);
        Some(start..end)
    }

    fn order_focus(&self, order: SourceOrder) -> Option<Range<usize>> {
        if self.after_lines == 0 {
            return None;
        }
        let line = order.after_gap.saturating_add(1).clamp(1, self.after_lines);
        Some(line..line + 1)
    }

    pub(super) fn script_order(&self, after_line: usize) -> usize {
        self.after_script_order
            .get(after_line)
            .copied()
            .unwrap_or_else(|| after_line.saturating_sub(1))
    }

    fn apply_sequence(&mut self, sequence: &SourceSequence) {
        for line in 1..=self.after_lines {
            self.after_script_order[line] = sequence
                .after_owner
                .get(line)
                .copied()
                .flatten()
                .map(|owner| owner.saturating_mul(2).saturating_add(2))
                .unwrap_or_else(|| sequence.unowned_after_rank(line));
        }
    }
}

/// Form unranked source hunks from one neutral syntax correspondence graph.
pub(super) fn raw_hunks(pair: &SyntaxPair<'_, '_>, correspondence: &Correspondence) -> RawHunks {
    let mut source_map = SourceMap::new(
        correspondence,
        pair.before.source.lines().len(),
        pair.after.source.lines().len(),
    );
    if pair.before.source.as_str() == pair.after.source.as_str() {
        return RawHunks {
            hunks: Vec::new(),
            source_map,
            before_lines: pair.before.source.lines().len(),
            after_lines: pair.after.source.lines().len(),
        };
    }

    let only_ordinary_linewise = pair
        .before
        .review_units()
        .chain(pair.after.review_units())
        .all(|(_, node)| {
            node.review.as_ref().is_some_and(|review| {
                review.comparison == ComparisonStrategy::Linewise
                    && review.role == SourceRole::Content
            })
        });
    let anchor_facts = AnchorFacts::new(pair);
    // A one-sided file has no competing revision geometry or structural edit order.
    // Treating its syntax units as independently buoyant can lift separators ahead of
    // the declarations they separate, so retain the file as one physical line region.
    let one_sided = pair.before.source.lines().is_empty() || pair.after.source.lines().is_empty();
    let source_ordered = only_ordinary_linewise || one_sided;
    let mut changes = if source_ordered {
        edit_fragments(form_whole_file_lines(pair, correspondence, &anchor_facts))
    } else {
        form_units(pair, correspondence, &anchor_facts)
    };
    // Correspondence owns sameness and wrapper decisions. Resolve those facts before
    // source placement, hunk coalescing, or halo expansion can depend on the row shape.
    normalize_stable_revision_rows(&mut changes, correspondence);
    normalize_containment(&mut changes, pair, correspondence);
    let sequence = (!source_ordered).then(|| SourceSequence::new(pair, correspondence));
    if let Some(sequence) = &sequence {
        source_map.apply_sequence(sequence);
    }
    let hunks = changes
        .into_iter()
        .map(|hunk| finalize_hunk(hunk, &source_map, sequence.as_ref()))
        .collect();
    RawHunks {
        hunks,
        source_map,
        before_lines: pair.before.source.lines().len(),
        after_lines: pair.after.source.lines().len(),
    }
}

/// Freeze transient producer rows into atomic, source-located facts.
fn finalize_hunk(
    hunk: FormedHunk,
    source_map: &SourceMap,
    sequence: Option<&SourceSequence>,
) -> RawHunk {
    let focus = SourceFocus::from_rows(&hunk.rows);
    let order = hunk
        .order_hint
        .unwrap_or_else(|| source_map.focus_order(&focus));
    let context_anchor = hunk
        .order_hint
        .and_then(|order| source_map.order_focus(order))
        .map(|focus| focus.start);
    let facts = atomic_event_groups(hunk.rows)
        .into_iter()
        .map(|events| {
            let script_order = sequence
                .map(|sequence| sequence.group_rank(&events, source_map))
                .unwrap_or_else(|| event_group_order(&events, source_map, order).after_gap);
            source_fact(events, source_map, order, script_order)
        })
        .collect::<Vec<_>>();

    RawHunk {
        nature: hunk.nature,
        facts,
        order,
        context_anchor,
        context_root: hunk.context_root,
    }
}

/// Keep each contiguous before/after run together as one replacement fact.
fn atomic_event_groups(rows: Vec<RowEvent>) -> Vec<Vec<RowEvent>> {
    let mut rows = rows.into_iter().peekable();
    let mut groups = Vec::new();
    while let Some(row) = rows.next() {
        if !matches!(row, RowEvent::Removed(_) | RowEvent::Added(_)) {
            groups.push(vec![row]);
            continue;
        }
        let mut replacement = vec![row];
        while rows.peek().is_some_and(|row| {
            matches!(
                row,
                RowEvent::Removed(_) | RowEvent::Added(_) | RowEvent::LineEnding(_)
            )
        }) {
            replacement.push(rows.next().expect("peeked replacement row exists"));
        }
        groups.push(replacement);
    }
    groups
}

fn source_fact(
    events: Vec<RowEvent>,
    source_map: &SourceMap,
    fallback_order: SourceOrder,
    script_order: usize,
) -> SourceFact {
    let coverage = event_group_coverage(&events, source_map);
    let order = event_group_order(&events, source_map, fallback_order);
    let displayed_after = events
        .iter()
        .filter_map(row_displayed_after_source_line)
        .collect();
    let change = if events
        .first()
        .is_some_and(|row| matches!(row, RowEvent::Removed(_) | RowEvent::Added(_)))
    {
        let mut before = Vec::new();
        let mut after = Vec::new();
        let mut line_endings = Vec::new();
        for event in events {
            match event {
                RowEvent::Removed(line) => before.push(line),
                RowEvent::Added(line) => after.push(line),
                RowEvent::LineEnding(change) => line_endings.push(change),
                _ => unreachable!("atomic replacement contains revision rows and terminators"),
            }
        }
        SourceChange::Replace {
            before,
            after,
            line_endings,
        }
    } else {
        debug_assert_eq!(events.len(), 1);
        match events
            .into_iter()
            .next()
            .expect("source fact owns one event")
        {
            RowEvent::Current(line) => SourceChange::Current(line),
            RowEvent::Reflow(line) => SourceChange::Reflow(line),
            RowEvent::LineEnding(change) => SourceChange::LineEnding(change),
            RowEvent::Moved { before, after } => SourceChange::Moved { before, after },
            RowEvent::Compact(change) => SourceChange::Compact(change),
            RowEvent::Elision(coverage) => SourceChange::Elision(coverage),
            RowEvent::Removed(_) | RowEvent::Added(_) => {
                unreachable!("one-sided revision rows are replacements too")
            }
        }
    };
    SourceFact {
        change,
        coverage,
        order,
        script_order,
        displayed_after,
    }
}

fn event_group_coverage(events: &[RowEvent], source_map: &SourceMap) -> LineCoverage {
    let mut coverage = LineCoverage {
        before: None,
        after: None,
    };
    for event in events {
        if let RowEvent::Elision(elision) = event {
            include_coverage(&mut coverage.before, elision.before.clone());
            include_coverage(&mut coverage.after, elision.after.clone());
            continue;
        }
        if let RowEvent::Current(line) | RowEvent::Reflow(line) = event
            && let Some(before) = source_map.aligned_before_line(line.number)
        {
            include_source_line(&mut coverage.before, before);
        }
        if let Some(before) = row_before_source_line(event) {
            include_source_line(&mut coverage.before, before);
        }
        if let Some(after) = row_after_source_line(event) {
            include_source_line(&mut coverage.after, after);
        }
    }
    coverage
}

fn event_group_order(
    events: &[RowEvent],
    source_map: &SourceMap,
    fallback: SourceOrder,
) -> SourceOrder {
    events
        .iter()
        .filter_map(row_after_source_line)
        .min()
        .map(SourceOrder::current)
        .or_else(|| {
            events
                .iter()
                .filter_map(row_before_source_line)
                .map(|line| source_map.before_order(line))
                .min()
        })
        .unwrap_or(fallback)
}

fn include_coverage(coverage: &mut Option<Range<usize>>, addition: Option<Range<usize>>) {
    let Some(addition) = addition else {
        return;
    };
    let Some(coverage) = coverage else {
        *coverage = Some(addition);
        return;
    };
    coverage.start = coverage.start.min(addition.start);
    coverage.end = coverage.end.max(addition.end);
}

fn include_source_line(coverage: &mut Option<Range<usize>>, line: usize) {
    include_coverage(coverage, Some(line..line + 1));
}

/// Present exact physical correspondence once, in the current revision.
///
/// Independently formed structural units can claim the two sides of a stable line as if it
/// were a removal and addition. The physical-line graph is the language-neutral authority for
/// those rows: old ghosts disappear and one current owner remains as context. Explicit
/// structural movement already uses `Moved` rows and remains outside this normalization.
fn normalize_stable_revision_rows(changes: &mut Vec<FormedHunk>, correspondence: &Correspondence) {
    let mut changed_before = HashSet::new();
    let mut changed_after = HashSet::new();
    let mut current_owners = HashSet::new();
    let mut context_owners = HashSet::new();
    let mut material_current_owners = HashSet::new();
    for row in changes.iter().flat_map(|change| &change.rows) {
        match row {
            RowEvent::Removed(line) => {
                changed_before.insert(line.number);
            }
            RowEvent::Added(line) => {
                changed_after.insert(line.number);
            }
            RowEvent::Current(line) if !line.has_changes() => {
                current_owners.insert(line.number);
                context_owners.insert(line.number);
            }
            _ => {
                let after = row_displayed_after_source_line(row);
                current_owners.extend(after);
                material_current_owners.extend(after);
            }
        }
    }

    let stable = correspondence
        .line_links
        .iter()
        .filter_map(|link| {
            let before = link.before + 1;
            let after = link.after + 1;
            let both_material = changed_before.contains(&before) && changed_after.contains(&after);
            let duplicates_current_owner = current_owners.contains(&after)
                && (changed_before.contains(&before) || changed_after.contains(&after));
            (both_material || duplicates_current_owner).then_some((before, after))
        })
        .collect::<Vec<_>>();
    if stable.is_empty() {
        return;
    }

    let stable_before = stable
        .iter()
        .map(|(before, _)| *before)
        .collect::<HashSet<_>>();
    let stable_after = stable
        .iter()
        .map(|(_, after)| *after)
        .collect::<HashSet<_>>();
    // A material current-side claim identifies the line's structural home more
    // precisely than context borrowed by another producer. Rebuild that claim as
    // context in place so extracted payload follows its new declaration.
    let relocated_after = stable_after
        .intersection(&changed_after)
        .filter(|line| context_owners.contains(line) && !material_current_owners.contains(line))
        .copied()
        .collect::<HashSet<_>>();
    current_owners.retain(|line| !relocated_after.contains(line));
    for change in changes.iter_mut() {
        change.rows = std::mem::take(&mut change.rows)
            .into_iter()
            .flat_map(|row| {
                if matches!(
                    &row,
                    RowEvent::Current(line)
                        if !line.has_changes() && relocated_after.contains(&line.number)
                ) {
                    return Vec::new();
                }
                match row {
                    RowEvent::Removed(line) if stable_before.contains(&line.number) => Vec::new(),
                    RowEvent::Added(mut line) if stable_after.contains(&line.number) => {
                        if !current_owners.insert(line.number) {
                            return Vec::new();
                        }
                        line.clear_changes();
                        vec![RowEvent::Current(line)]
                    }
                    row => vec![row],
                }
            })
            .collect();
    }
    changes.retain(|change| change.rows.iter().any(row_has_signal));
}

/// Present a containment change around its exact retained payload.
///
/// One old physical row may spread across several current rows, or several old rows may collapse
/// into one. Exact leaf correspondence remains the authority: retained ranges stay context while
/// only the one-sided shell carries the change.
fn normalize_containment(
    changes: &mut Vec<FormedHunk>,
    pair: &SyntaxPair<'_, '_>,
    correspondence: &Correspondence,
) {
    let mut wrapper_pairs = HashSet::new();
    let mut stable_exact = Vec::new();
    for link in &correspondence.leaf_links {
        let before = pair.before.node(link.before);
        let after = pair.after.node(link.after);
        if link.placement != Placement::Stable {
            continue;
        }

        let exact_spelling = pair.before.leaf_text(link.before) == pair.after.leaf_text(link.after);
        let retained = link.relation == LeafRelation::Exact
            || (link.parent == ParentCorrespondence::Direct && exact_spelling);
        if retained && before.lines.len() == 1 && after.lines.len() == 1 {
            stable_exact.push((
                before.lines.start,
                after.lines.start,
                before.bytes.clone(),
                after.bytes.clone(),
            ));
        }
        if link.relation == LeafRelation::Exact
            && let Some(wrapper) = link.wrapper
            && before.lines.len() == 1
            && after.lines.len() == 1
            && leaf_is_meaningful_payload(&pair.before, link.before)
            && leaf_is_meaningful_payload(&pair.after, link.after)
        {
            let (before, after) = widest_exact_node_pair(pair, link.before, link.after);
            let before = pair.before.node(before);
            let after = pair.after.node(after);
            if before.lines.len() == 1 && after.lines.len() == 1 {
                stable_exact.push((
                    before.lines.start,
                    after.lines.start,
                    before.bytes.clone(),
                    after.bytes.clone(),
                ));
                wrapper_pairs.insert((before.lines.start, after.lines.start, wrapper));
            }
        }
    }
    if wrapper_pairs.is_empty() {
        return;
    }
    let line_pairs = wrapper_pairs.into_iter().collect::<Vec<_>>();

    let mut before_target_count = HashMap::<usize, usize>::new();
    let mut after_source_count = HashMap::<usize, usize>::new();
    for (before, after, _) in &line_pairs {
        *before_target_count.entry(*before).or_default() += 1;
        *after_source_count.entry(*after).or_default() += 1;
    }

    let retained_pairs = line_pairs
        .into_iter()
        .filter(|(before_line, after_line, _)| {
            before_target_count.get(before_line) == Some(&1)
                && after_source_count.get(after_line) == Some(&1)
        })
        .collect::<Vec<_>>();

    let mut retained_before = HashMap::<usize, Vec<Range<usize>>>::new();
    let mut retained_after = HashMap::<usize, Vec<Range<usize>>>::new();
    let mut paired_before_by_after = HashMap::<usize, Vec<usize>>::new();
    for (before_line, after_line, wrapper) in retained_pairs {
        paired_before_by_after
            .entry(after_line)
            .or_default()
            .push(before_line);
        for (exact_before, exact_after, before_bytes, after_bytes) in &stable_exact {
            let belongs = match wrapper {
                super::correspondence::Reparenting::Wrapped => *exact_before == before_line,
                super::correspondence::Reparenting::Unwrapped => *exact_after == after_line,
            };
            if belongs {
                retained_before
                    .entry(*exact_before)
                    .or_default()
                    .push(before_bytes.clone());
                retained_after
                    .entry(*exact_after)
                    .or_default()
                    .push(after_bytes.clone());
            }
        }
    }
    for ranges in retained_before.values_mut() {
        bridge_retained_layout(&pair.before, ranges);
    }
    for ranges in retained_after.values_mut() {
        bridge_retained_layout(&pair.after, ranges);
    }
    if retained_before.is_empty() || retained_after.is_empty() {
        return;
    }

    let mut material_removed = HashSet::new();
    for change in changes.iter_mut() {
        change.rows = std::mem::take(&mut change.rows)
            .into_iter()
            .filter_map(|row| match row {
                RowEvent::Removed(line) => {
                    let Some(ranges) = retained_before.get(&line.number) else {
                        return Some(RowEvent::Removed(line));
                    };
                    let line = retained_source_row(&pair.before, &line, ranges);
                    if !line_has_material_highlight(&pair.before, &line) {
                        return None;
                    }
                    material_removed.insert(line.number);
                    Some(RowEvent::Removed(line))
                }
                row => Some(row),
            })
            .collect();
    }
    for change in changes.iter_mut() {
        change.rows = std::mem::take(&mut change.rows)
            .into_iter()
            .map(|row| match row {
                RowEvent::Added(line) => {
                    let Some(ranges) = retained_after.get(&line.number) else {
                        return RowEvent::Added(line);
                    };
                    let line = retained_source_row(&pair.after, &line, ranges);
                    let paired_removal =
                        paired_before_by_after
                            .get(&line.number)
                            .is_some_and(|before| {
                                before.iter().any(|line| material_removed.contains(line))
                            });
                    if paired_removal {
                        RowEvent::Added(line)
                    } else {
                        RowEvent::Current(line)
                    }
                }
                row => row,
            })
            .collect();
    }
    changes.retain(|change| change.rows.iter().any(row_has_signal));
}

/// Expand retained wrapper payload through source-identical parser parents.
fn widest_exact_node_pair(
    pair: &SyntaxPair<'_, '_>,
    mut before: NodeId,
    mut after: NodeId,
) -> (NodeId, NodeId) {
    while let Some(before_parent) = pair.before.node(before).parent {
        let Some(after_parent) = pair.after.node(after).parent else {
            break;
        };
        let before_node = pair.before.node(before_parent);
        let after_node = pair.after.node(after_parent);
        if before_node.kind != after_node.kind
            || before_node.seals_wrappers()
            || after_node.seals_wrappers()
            || pair.before.source.slice(before_node.bytes.clone())
                != pair.after.source.slice(after_node.bytes.clone())
        {
            break;
        }
        before = before_parent;
        after = after_parent;
    }
    (before, after)
}

fn retained_source_row(
    tree: &SyntaxTree<'_>,
    line: &SelectedLine,
    retained: &[Range<usize>],
) -> SelectedLine {
    let source = &tree.source.lines()[line.number - 1];
    let mut cursor = source.content_bytes.start;
    let mut retained = retained.to_vec();
    for highlight in &line.highlights {
        if cursor < highlight.start
            && tree
                .source
                .slice(cursor..highlight.start)
                .is_some_and(|text| text.chars().all(char::is_whitespace))
        {
            retained.push(cursor..highlight.start);
        }
        cursor = cursor.max(highlight.end);
    }
    if cursor < source.content_bytes.end
        && tree
            .source
            .slice(cursor..source.content_bytes.end)
            .is_some_and(|text| text.chars().all(char::is_whitespace))
    {
        retained.push(cursor..source.content_bytes.end);
    }
    let changed = ranges_excluding(source.content_bytes.clone(), &retained);
    select_line(source, &changed)
}

fn ranges_excluding(bytes: Range<usize>, excluded: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut excluded = excluded
        .iter()
        .filter(|range| ranges_overlap(range, &bytes))
        .map(|range| range.start.max(bytes.start)..range.end.min(bytes.end))
        .collect::<Vec<_>>();
    excluded.sort_unstable_by_key(|range| range.start);

    let mut changed = Vec::new();
    let mut cursor = bytes.start;
    for excluded in excluded {
        if cursor < excluded.start {
            changed.push(cursor..excluded.start);
        }
        cursor = cursor.max(excluded.end);
    }
    if cursor < bytes.end {
        changed.push(cursor..bytes.end);
    }
    changed
}

fn line_has_material_highlight(tree: &SyntaxTree<'_>, line: &SelectedLine) -> bool {
    line.highlights.iter().any(|highlight| {
        tree.source
            .slice(highlight.clone())
            .is_some_and(|text| !text.trim().is_empty())
    })
}

/// Join retained byte ranges across unchanged horizontal layout on one source line.
fn bridge_retained_layout(tree: &SyntaxTree<'_>, ranges: &mut Vec<Range<usize>>) {
    ranges.sort_unstable_by_key(|range| range.start);
    let mut joined: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in std::mem::take(ranges) {
        let Some(previous) = joined.last_mut() else {
            joined.push(range);
            continue;
        };
        let overlaps = range.start <= previous.end;
        let separated_by_layout = !overlaps
            && tree
                .source
                .slice(previous.end..range.start)
                .is_some_and(horizontal_layout);
        if overlaps || separated_by_layout {
            previous.end = previous.end.max(range.end);
        } else {
            joined.push(range);
        }
    }
    *ranges = joined;
}

fn row_has_signal(row: &RowEvent) -> bool {
    match row {
        RowEvent::Current(line) => line.has_changes(),
        RowEvent::Reflow(_)
        | RowEvent::Removed(_)
        | RowEvent::Added(_)
        | RowEvent::LineEnding(_)
        | RowEvent::Moved { .. }
        | RowEvent::Compact(_) => true,
        RowEvent::Elision(_) => false,
    }
}

fn edit_fragments(excerpts: Vec<FormedExcerpt>) -> Vec<FormedHunk> {
    excerpts.into_iter().map(FormedHunk::edit).collect()
}

fn formed_excerpts(rows: Vec<RowEvent>, context_root: Option<NodeId>) -> Vec<FormedExcerpt> {
    if !rows.iter().any(row_has_signal) {
        return Vec::new();
    }
    vec![FormedExcerpt { rows, context_root }]
}

fn form_units(
    pair: &SyntaxPair<'_, '_>,
    correspondence: &Correspondence,
    anchor_facts: &AnchorFacts,
) -> Vec<FormedHunk> {
    let mut fragments = Vec::new();
    let mut preceding_after_lines = 0;

    for edit in &correspondence.units {
        match edit {
            UnitEdit::Matched(unit) => {
                form_matched_unit(pair, correspondence, anchor_facts, unit, &mut fragments);
                let after = pair.after.node(unit.after);
                preceding_after_lines =
                    preceding_after_lines.max(after.lines.end.saturating_sub(1));
            }
            UnitEdit::Removed { before } => {
                let node = pair.before.node(*before);
                let order = SourceOrder::deletion(preceding_after_lines, node.lines.start);
                let review = node
                    .review
                    .as_ref()
                    .expect("review edit owns a review unit");
                match review.role {
                    SourceRole::Wiring => fragments.push(
                        FormedHunk::wiring(form_one_sided_lines(
                            &pair.before,
                            node.lines.clone(),
                            RevisionSide::Before,
                        ))
                        .at_order(order),
                    ),
                    SourceRole::Content => {
                        fragments.push(
                            FormedHunk::edit(form_one_sided_lines(
                                &pair.before,
                                node.lines.clone(),
                                RevisionSide::Before,
                            ))
                            .at_order(order),
                        );
                    }
                }
            }
            UnitEdit::Added { after } => {
                let node = pair.after.node(*after);
                let review = node
                    .review
                    .as_ref()
                    .expect("review edit owns a review unit");
                match review.role {
                    SourceRole::Wiring => fragments.push(FormedHunk::wiring(form_one_sided_lines(
                        &pair.after,
                        node.lines.clone(),
                        RevisionSide::After,
                    ))),
                    SourceRole::Content => {
                        fragments.extend(edit_fragments(vec![form_one_sided_lines(
                            &pair.after,
                            node.lines.clone(),
                            RevisionSide::After,
                        )]))
                    }
                }
                preceding_after_lines = preceding_after_lines.max(node.lines.end.saturating_sub(1));
            }
        }
    }
    for fallback in &correspondence.line_fallbacks {
        let excerpts = form_line_region(
            pair,
            correspondence,
            one_based_line_range(&fallback.before),
            one_based_line_range(&fallback.after),
            &[],
            None,
            LineAnchors::new(AnchorBasis::Physical, anchor_facts),
        );
        fragments.extend(edit_fragments(excerpts));
    }
    fragments
}

fn one_based_line_range(lines: &Range<usize>) -> Option<Range<usize>> {
    (!lines.is_empty()).then(|| lines.start + 1..lines.end + 1)
}

fn form_matched_unit(
    pair: &SyntaxPair<'_, '_>,
    correspondence: &Correspondence,
    anchor_facts: &AnchorFacts,
    unit: &MatchedUnit,
    fragments: &mut Vec<FormedHunk>,
) {
    let after_node = pair.after.node(unit.after);
    if unit.placement == Placement::Reordered && unit.relation.full_equal() {
        fragments.push(FormedHunk::moved(form_move(pair, correspondence, unit)));
        return;
    }
    if unit.relation.source_equal() {
        return;
    }

    if unit.role == SourceRole::Wiring {
        let before = pair.before.node(unit.before);
        if before.lines.len() != 1 || after_node.lines.len() != 1 {
            let composites = correspondence.unit_composites(unit);
            let excerpts = form_line_region(
                pair,
                correspondence,
                Some(before.lines.clone()),
                Some(after_node.lines.clone()),
                composites,
                Some(unit.after),
                LineAnchors::new(structural_anchor_basis(pair, unit), anchor_facts),
            );
            fragments.extend(excerpts.into_iter().map(FormedHunk::wiring));
            return;
        }
        let excerpt = form_wiring(pair, Some(unit.before), Some(unit.after));
        fragments.push(FormedHunk::wiring(excerpt));
        return;
    }

    match unit.comparison {
        ComparisonStrategy::Linewise => {
            let composites = correspondence.unit_composites(unit);
            fragments.extend(edit_fragments(form_line_region(
                pair,
                correspondence,
                Some(pair.before.node(unit.before).lines.clone()),
                Some(after_node.lines.clone()),
                composites,
                Some(unit.after),
                LineAnchors::new(AnchorBasis::Physical, anchor_facts),
            )));
        }
        ComparisonStrategy::Structural => {
            if unit.relation.full_equal() {
                fragments.extend(
                    form_reflow(pair, correspondence, unit)
                        .into_iter()
                        .map(FormedHunk::reflow),
                );
                return;
            }

            let dependents = retained_decorations(pair, correspondence, unit);
            let comments = comment_edits(pair, correspondence, unit, &dependents);
            if unit.relation.payload_equal() {
                fragments.extend(
                    form_reflow_with_comments(pair, correspondence, unit, comments)
                        .into_iter()
                        .map(FormedHunk::edit),
                );
                return;
            }
            // Root units own the complete edit order; nested units share it with sibling edits.
            let owns_file_order = unit.before == pair.before.root && unit.after == pair.after.root;
            let needs_physical_plan = pair.before.identity_text(unit.before)
                != pair.after.identity_text(unit.after)
                || has_retainable_reparented_region(pair, correspondence, anchor_facts, unit)
                || (!owns_file_order && has_unmatched_before_content(pair, correspondence, unit));
            if needs_physical_plan {
                let excerpts = form_line_region(
                    pair,
                    correspondence,
                    Some(pair.before.node(unit.before).lines.clone()),
                    Some(after_node.lines.clone()),
                    correspondence.unit_composites(unit),
                    Some(unit.after),
                    LineAnchors::new(structural_anchor_basis(pair, unit), anchor_facts),
                );
                fragments.extend(edit_fragments(excerpts));
                return;
            }
            fragments.extend(
                form_payload(
                    pair,
                    correspondence,
                    anchor_facts,
                    unit,
                    &dependents,
                    comments,
                )
                .into_iter()
                .map(FormedHunk::edit),
            );
        }
    }
}

fn has_retainable_reparented_region(
    pair: &SyntaxPair<'_, '_>,
    correspondence: &Correspondence,
    anchor_facts: &AnchorFacts,
    unit: &MatchedUnit,
) -> bool {
    let before = line_indices(
        Some(pair.before.node(unit.before).lines.clone()),
        pair.before.source.lines().len(),
    );
    let after = line_indices(
        Some(pair.after.node(unit.after).lines.clone()),
        pair.after.source.lines().len(),
    );
    correspondence
        .unit_composites(unit)
        .iter()
        .copied()
        .filter(|link| link.wrapper.is_some())
        .any(|link| retained_region(pair, anchor_facts, link, &before, &after).is_some())
}

fn has_unmatched_before_content(
    pair: &SyntaxPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
) -> bool {
    descendant_leaves(&pair.before, unit.before).any(|leaf_id| {
        let node = pair.before.node(leaf_id);
        let significant = node.leaf.is_some_and(|leaf| {
            !matches!(
                leaf.channel,
                ContentChannel::Comment | ContentChannel::Layout
            )
        });
        significant && correspondence.before_leaf_link(leaf_id).is_none()
    })
}

fn form_one_sided_lines(
    tree: &SyntaxTree<'_>,
    lines: Range<usize>,
    side: RevisionSide,
) -> FormedExcerpt {
    let mut rows = Vec::new();
    for source in collect_source_lines(tree, lines) {
        let ending = source.ending;
        let line = select_line(source, std::slice::from_ref(&source.content_bytes));
        let row = if side == RevisionSide::Before {
            RowEvent::Removed(line)
        } else {
            RowEvent::Added(line)
        };
        rows.push(row);
        if ending == LineEnding::Missing {
            let endpoint = LineEndingEndpoint {
                line: source.number,
                ending,
            };
            rows.push(RowEvent::LineEnding(LineEndingChange {
                before: (side == RevisionSide::Before).then_some(endpoint),
                after: (side == RevisionSide::After).then_some(endpoint),
            }));
        }
    }
    FormedExcerpt {
        rows,
        context_root: None,
    }
}

fn form_wiring(
    pair: &SyntaxPair<'_, '_>,
    before: Option<NodeId>,
    after: Option<NodeId>,
) -> FormedExcerpt {
    let before_node = before.map(|node| pair.before.node(node));
    let after_node = after.map(|node| pair.after.node(node));
    let compact = CompactChange {
        before_line: before_node.map(|node| node.lines.start),
        after_line: after_node.map(|node| node.lines.start),
        before_bytes: before_node.map(|node| node.bytes.clone()),
        after_bytes: after_node.map(|node| node.bytes.clone()),
    };

    FormedExcerpt {
        rows: vec![RowEvent::Compact(compact)],
        context_root: None,
    }
}

fn form_move(
    pair: &SyntaxPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
) -> FormedExcerpt {
    let before = pair.before.node(unit.before);
    let after = pair.after.node(unit.after);
    let mut lines = select_lines(&pair.after, after.lines.clone(), &[]);
    let Some(first) = lines.first().cloned() else {
        return FormedExcerpt {
            rows: Vec::new(),
            context_root: None,
        };
    };
    if let Some(rows) = moved_rows_with_line_endings(
        pair,
        before.lines.clone(),
        after.lines.clone(),
        correspondence.unit_line_links(unit),
        &lines,
    ) {
        return FormedExcerpt {
            rows,
            context_root: None,
        };
    }
    if lines.len() == 1 {
        return FormedExcerpt {
            rows: vec![RowEvent::Moved {
                before: Some(before.lines.start),
                after: first,
            }],
            context_root: None,
        };
    }

    let last = lines.pop().expect("a multi-line move has a final line");
    lines.remove(0);
    let mut rows = vec![RowEvent::Moved {
        before: Some(before.lines.start),
        after: first,
    }];
    if lines.len() == 1 {
        rows.push(RowEvent::Current(
            lines.pop().expect("one middle line remains"),
        ));
    } else if !lines.is_empty() {
        rows.push(RowEvent::Elision(LineCoverage {
            before: Some(before.lines.start + 1..before.lines.end.saturating_sub(1)),
            after: Some(after.lines.start + 1..after.lines.end.saturating_sub(1)),
        }));
    }
    rows.push(RowEvent::Moved {
        before: None,
        after: last,
    });
    FormedExcerpt {
        rows,
        context_root: None,
    }
}

/// A move remains the primary fact, but concrete terminator edits inside it cannot disappear.
fn moved_rows_with_line_endings(
    pair: &SyntaxPair<'_, '_>,
    before: Range<usize>,
    after: Range<usize>,
    links: &[LineLink],
    lines: &[SelectedLine],
) -> Option<Vec<RowEvent>> {
    let before_indices = line_indices(Some(before.clone()), pair.before.source.lines().len());
    let after_indices = line_indices(Some(after.clone()), pair.after.source.lines().len());
    if after_indices.len() != lines.len() {
        return None;
    }
    let mut matched_before = vec![false; before_indices.len()];
    let mut matched_after = vec![false; after_indices.len()];
    let mut endings = vec![None; after_indices.len()];
    for link in links {
        let before = link.before - before_indices.start;
        let after = link.after - after_indices.start;
        matched_before[before] = true;
        matched_after[after] = true;
        let before_ending = pair.before.source.lines()[link.before].ending;
        let after_ending = pair.after.source.lines()[link.after].ending;
        if before_ending != after_ending {
            endings[after] = Some(LineEndingChange {
                before: Some(LineEndingEndpoint {
                    line: link.before + 1,
                    ending: before_ending,
                }),
                after: Some(LineEndingEndpoint {
                    line: link.after + 1,
                    ending: after_ending,
                }),
            });
        }
    }

    // Unmatched layout cannot create a line pair. Cancel equal special endings as
    // multisets, then retain only the concrete one-sided source facts that remain.
    let mut before_only = Vec::new();
    for ending in [LineEnding::Missing, LineEnding::CrLf] {
        let before_offsets = before_indices
            .clone()
            .enumerate()
            .filter(|(offset, index)| {
                !matched_before[*offset] && pair.before.source.lines()[*index].ending == ending
            })
            .map(|(_, index)| index)
            .collect::<Vec<_>>();
        let after_offsets = after_indices
            .clone()
            .enumerate()
            .filter(|(offset, index)| {
                !matched_after[*offset] && pair.after.source.lines()[*index].ending == ending
            })
            .map(|(offset, _)| offset)
            .collect::<Vec<_>>();
        let cancelled = before_offsets.len().min(after_offsets.len());
        before_only.extend(before_offsets.into_iter().skip(cancelled).map(|line| {
            LineEndingEndpoint {
                line: line + 1,
                ending,
            }
        }));
        for offset in after_offsets.into_iter().skip(cancelled) {
            endings[offset] = Some(LineEndingChange {
                before: None,
                after: Some(LineEndingEndpoint {
                    line: after_indices.start + offset + 1,
                    ending,
                }),
            });
        }
    }
    if before_only.is_empty() && endings.iter().all(Option::is_none) {
        return None;
    }

    let mut rows = vec![RowEvent::Moved {
        before: Some(before.start),
        after: lines[0].clone(),
    }];
    for endpoint in before_only {
        rows.push(RowEvent::LineEnding(LineEndingChange {
            before: Some(endpoint),
            after: None,
        }));
    }
    append_moved_line_ending(&mut rows, endings[0]);
    if lines.len() == 1 {
        return Some(rows);
    }

    let last = lines.len() - 1;
    let show_only_middle = (lines.len() == 3).then_some(1);
    let mut offset = 1;
    while offset < last {
        let selected = show_only_middle == Some(offset) || endings[offset].is_some();
        if selected {
            rows.push(RowEvent::Current(lines[offset].clone()));
            append_moved_line_ending(&mut rows, endings[offset]);
            offset += 1;
            continue;
        }

        let start = offset;
        while offset < last && show_only_middle != Some(offset) && endings[offset].is_none() {
            offset += 1;
        }
        rows.push(RowEvent::Elision(LineCoverage {
            before: (before.len() == after.len())
                .then(|| before.start + start..before.start + offset),
            after: Some(after.start + start..after.start + offset),
        }));
    }
    rows.push(RowEvent::Moved {
        before: None,
        after: lines[last].clone(),
    });
    append_moved_line_ending(&mut rows, endings[last]);
    Some(rows)
}

fn append_moved_line_ending(rows: &mut Vec<RowEvent>, ending: Option<LineEndingChange>) {
    let Some(ending) = ending else {
        return;
    };
    rows.push(RowEvent::LineEnding(ending));
}

fn form_reflow(
    pair: &SyntaxPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
) -> Vec<FormedExcerpt> {
    let before = pair.before.node(unit.before);
    let after = pair.after.node(unit.after);
    let before_lines = line_indices(Some(before.lines.clone()), pair.before.source.lines().len());
    let after_lines = line_indices(Some(after.lines.clone()), pair.after.source.lines().len());
    let exact_after = correspondence
        .line_links_in(before_lines, after_lines)
        .map(|link| link.after + 1)
        .collect::<HashSet<_>>();
    let rows = select_lines(&pair.after, after.lines.clone(), &[])
        .into_iter()
        .map(|line| {
            if exact_after.contains(&line.number) {
                RowEvent::Current(line)
            } else {
                RowEvent::Reflow(line)
            }
        })
        .collect::<Vec<_>>();
    formed_excerpts(rows, Some(unit.after))
}

fn form_reflow_with_comments(
    pair: &SyntaxPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
    mut edits: Vec<LineEdit>,
) -> Vec<FormedExcerpt> {
    let before = pair.before.node(unit.before);
    let after = pair.after.node(unit.after);
    let changed_lines = edits
        .iter()
        .filter_map(|edit| edit.after.as_ref().map(|line| line.number))
        .collect::<HashSet<_>>();
    let before_comment_lines = comment_lines(&pair.before, unit.before);
    let before_lines = line_indices(Some(before.lines.clone()), pair.before.source.lines().len());
    let after_lines = line_indices(Some(after.lines.clone()), pair.after.source.lines().len());
    let exact_after = correspondence
        .line_links_in(before_lines, after_lines)
        .filter(|link| {
            !before_comment_lines.contains(&(link.before + 1))
                && !changed_lines.contains(&(link.after + 1))
        })
        .map(|link| link.after + 1)
        .collect::<HashSet<_>>();

    edits.sort_by_key(line_edit_order);
    let mut edits = edits.into_iter().peekable();
    let mut rows = Vec::new();
    for line in select_lines(&pair.after, after.lines.clone(), &[]) {
        while edits
            .peek()
            .is_some_and(|edit| line_edit_order(edit) < line.number)
        {
            append_line_edit_rows(&mut rows, edits.next().expect("peeked line edit"));
        }
        if changed_lines.contains(&line.number) {
            continue;
        }
        let row = if exact_after.contains(&line.number) {
            RowEvent::Current(line)
        } else {
            RowEvent::Reflow(line)
        };
        rows.push(row);
    }
    for edit in edits {
        append_line_edit_rows(&mut rows, edit);
    }

    formed_excerpts(rows, Some(unit.after))
}

fn comment_lines(tree: &SyntaxTree<'_>, unit: NodeId) -> HashSet<usize> {
    descendant_leaves(tree, unit)
        .filter_map(|leaf| {
            let node = tree.node(leaf);
            (node.leaf?.channel == ContentChannel::Comment).then_some(node.lines.clone())
        })
        .flatten()
        .collect()
}

fn form_payload(
    pair: &SyntaxPair<'_, '_>,
    correspondence: &Correspondence,
    anchor_facts: &AnchorFacts,
    unit: &MatchedUnit,
    dependents: &RetainedDecorations,
    mut edits: Vec<LineEdit>,
) -> Vec<FormedExcerpt> {
    let before = pair.before.node(unit.before);
    let after = pair.after.node(unit.after);
    if payload_requires_node_snap(pair, correspondence, unit) {
        return form_line_region(
            pair,
            correspondence,
            Some(before.lines.clone()),
            Some(after.lines.clone()),
            correspondence.unit_composites(unit),
            Some(unit.after),
            LineAnchors::new(structural_anchor_basis(pair, unit), anchor_facts),
        );
    }
    edits.extend(modified_payload_edits(pair, correspondence, unit));
    let mut changed_before = correspondence
        .unit_leaf_links(unit)
        .iter()
        .filter(|link| {
            link.relation != LeafRelation::Modified && link.placement == Placement::Reordered
        })
        .map(|link| link.before)
        .collect::<HashSet<_>>();
    let mut changed_after = correspondence
        .unit_leaf_links(unit)
        .iter()
        .filter(|link| {
            link.relation != LeafRelation::Modified && link.placement == Placement::Reordered
        })
        .map(|link| link.after)
        .collect::<HashSet<_>>();
    for composite in correspondence
        .unit_composites(unit)
        .iter()
        .filter(|composite| composite.placement == Placement::Reordered)
    {
        changed_before.extend(descendant_leaves(&pair.before, composite.before));
        changed_after.extend(descendant_leaves(&pair.after, composite.after));
    }
    changed_before.extend(descendant_leaves(&pair.before, unit.before).filter(|node| {
        pair.before.node(*node).leaf.is_some_and(|leaf| {
            !matches!(
                leaf.channel,
                ContentChannel::Comment | ContentChannel::Layout
            ) && correspondence.before_leaf_link(*node).is_none()
        })
    }));
    changed_after.extend(descendant_leaves(&pair.after, unit.after).filter(|node| {
        pair.after.node(*node).leaf.is_some_and(|leaf| {
            !matches!(
                leaf.channel,
                ContentChannel::Comment | ContentChannel::Layout
            ) && correspondence.after_leaf_link(*node).is_none()
        })
    }));
    changed_before.retain(|leaf| !dependents.before.contains(leaf));
    changed_after.retain(|leaf| !dependents.after.contains(leaf));
    let before_marked = marked_leaf_ranges(&pair.before, changed_before);
    let after_marked = marked_leaf_ranges(&pair.after, changed_after);
    let claimed_before = edits
        .iter()
        .filter_map(|edit| edit.before.as_ref().map(|line| line.number))
        .collect::<HashSet<_>>();
    let partial_removal = collect_source_lines(&pair.before, before.lines.clone())
        .into_iter()
        .filter(|line| !claimed_before.contains(&line.number))
        .map(|line| select_line(line, &before_marked))
        .any(|line| line.has_changes() && !line_is_fully_highlighted(&pair.before, &line));
    if partial_removal {
        return form_line_region(
            pair,
            correspondence,
            Some(before.lines.clone()),
            Some(after.lines.clone()),
            correspondence.unit_composites(unit),
            Some(unit.after),
            LineAnchors::new(structural_anchor_basis(pair, unit), anchor_facts),
        );
    }
    edits.extend(fully_marked_line_edits(
        pair,
        before.lines.clone(),
        after.lines.clone(),
        &before_marked,
        &after_marked,
    ));
    deduplicate_line_edits(&mut edits);
    if line_edits_compete_for_source_rows(&edits) {
        return form_line_region(
            pair,
            correspondence,
            Some(before.lines.clone()),
            Some(after.lines.clone()),
            correspondence.unit_composites(unit),
            Some(unit.after),
            LineAnchors::new(structural_anchor_basis(pair, unit), anchor_facts),
        );
    }
    let changed_lines = edits
        .iter()
        .filter_map(|edit| edit.after.as_ref().map(|line| line.number))
        .collect::<HashSet<_>>();
    let lines = select_lines(&pair.after, after.lines.clone(), &after_marked);
    edits.sort_by_key(line_edit_order);
    let mut edits = edits.into_iter().peekable();
    let mut rows = Vec::new();
    for line in lines {
        while edits
            .peek()
            .is_some_and(|edit| line_edit_order(edit) < line.number)
        {
            append_line_edit_rows(&mut rows, edits.next().expect("peeked line edit"));
        }
        if changed_lines.contains(&line.number) {
            continue;
        }
        rows.push(RowEvent::Current(line));
    }
    for edit in edits {
        append_line_edit_rows(&mut rows, edit);
    }

    formed_excerpts(rows, Some(unit.after))
}

fn leaf_is_meaningful_payload(tree: &SyntaxTree<'_>, id: NodeId) -> bool {
    let node = tree.node(id);
    let Some(leaf) = node.leaf else {
        return false;
    };
    leaf.delimiter.is_none()
        && !matches!(
            leaf.channel,
            ContentChannel::Comment | ContentChannel::Layout
        )
        && tree
            .leaf_text(id)
            .is_some_and(|text| !text.trim().is_empty())
}

/// Detect edits whose physical rows cannot represent their smallest matched parser owner.
fn payload_requires_node_snap(
    pair: &SyntaxPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
) -> bool {
    correspondence.unit_leaf_links(unit).iter().any(|link| {
        if link.relation != LeafRelation::Modified {
            return false;
        }
        let before = pair.before.node(link.before);
        let after = pair.after.node(link.after);
        let significant = before.leaf.is_some_and(|leaf| {
            !matches!(
                leaf.channel,
                ContentChannel::Comment | ContentChannel::Layout
            )
        }) && after.leaf.is_some_and(|leaf| {
            !matches!(
                leaf.channel,
                ContentChannel::Comment | ContentChannel::Layout
            )
        });
        if !significant {
            return false;
        }
        if before.lines.len() > 1 || after.lines.len() > 1 {
            return true;
        }

        let owner = correspondence
            .scopes
            .iter()
            .filter(|scope| {
                pair.before.contains(scope.before, link.before)
                    && pair.after.contains(scope.after, link.after)
                    && pair.before.contains(unit.before, scope.before)
                    && pair.after.contains(unit.after, scope.after)
            })
            .min_by_key(|scope| {
                pair.before.node(scope.before).bytes.len()
                    + pair.after.node(scope.after).bytes.len()
            });
        let Some(owner) = owner else {
            return false;
        };
        if parser_node_frames_match(pair, owner.before, owner.after) {
            return false;
        }

        !correspondence.unit_leaf_links(unit).iter().any(|retained| {
            retained.wrapper.is_some()
                && pair.before.contains(owner.before, retained.before)
                && pair.after.contains(owner.after, retained.after)
        })
    })
}

/// Whether a matched parser node occupies equivalent physical-line surroundings.
fn parser_node_frames_match(pair: &SyntaxPair<'_, '_>, before: NodeId, after: NodeId) -> bool {
    let Some((before_prefix, before_suffix, before_lines)) = node_line_frame(&pair.before, before)
    else {
        return false;
    };
    let Some((after_prefix, after_suffix, after_lines)) = node_line_frame(&pair.after, after)
    else {
        return false;
    };
    before_lines == after_lines
        && frame_fragment_matches(before_prefix, after_prefix)
        && frame_fragment_matches(before_suffix, after_suffix)
}

fn node_line_frame<'source>(
    tree: &'source SyntaxTree<'_>,
    id: NodeId,
) -> Option<(&'source str, &'source str, usize)> {
    let node = tree.node(id);
    let first = tree.source.line(node.lines.start)?;
    let last = tree.source.line(node.lines.end.checked_sub(1)?)?;
    if node.bytes.start < first.content_bytes.start || node.bytes.end > last.content_bytes.end {
        return None;
    }
    let prefix = tree
        .source
        .slice(first.content_bytes.start..node.bytes.start)?;
    let suffix = tree.source.slice(node.bytes.end..last.content_bytes.end)?;
    Some((prefix, suffix, node.lines.len()))
}

fn frame_fragment_matches(before: &str, after: &str) -> bool {
    before == after || horizontal_layout(before) && horizontal_layout(after)
}

/// One physical replacement row retained with both source revisions.
struct LineEdit {
    before: Option<SelectedLine>,
    after: Option<SelectedLine>,
    line_ending: Option<LineEndingChange>,
}

fn comment_edits(
    pair: &SyntaxPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
    dependents: &RetainedDecorations,
) -> Vec<LineEdit> {
    let mut edits = Vec::new();
    let linked_before = correspondence
        .unit_leaf_links(unit)
        .iter()
        .map(|link| link.before)
        .collect::<HashSet<_>>();
    let linked_after = correspondence
        .unit_leaf_links(unit)
        .iter()
        .map(|link| link.after)
        .collect::<HashSet<_>>();

    for link in correspondence.unit_leaf_links(unit) {
        let before_leaf = pair.before.node(link.before).leaf;
        let after_leaf = pair.after.node(link.after).leaf;
        let comment = before_leaf.is_some_and(|leaf| leaf.channel == ContentChannel::Comment)
            && after_leaf.is_some_and(|leaf| leaf.channel == ContentChannel::Comment);
        let dependent =
            dependents.before.contains(&link.before) && dependents.after.contains(&link.after);
        if !comment
            || dependent
            || link.relation == LeafRelation::Exact
                && link.placement == Placement::Stable
                && link.wrapper.is_none()
        {
            continue;
        }
        append_changed_line_edits(
            &mut edits,
            &pair.before,
            Some(pair.before.node(link.before).lines.clone()),
            &pair.after,
            Some(pair.after.node(link.after).lines.clone()),
        );
    }

    for leaf in descendant_leaves(&pair.before, unit.before) {
        let node = pair.before.node(leaf);
        if linked_before.contains(&leaf)
            || !node
                .leaf
                .is_some_and(|leaf| leaf.channel == ContentChannel::Comment)
        {
            continue;
        }
        append_changed_line_edits(
            &mut edits,
            &pair.before,
            Some(node.lines.clone()),
            &pair.after,
            None,
        );
    }
    for leaf in descendant_leaves(&pair.after, unit.after) {
        let node = pair.after.node(leaf);
        if linked_after.contains(&leaf)
            || !node
                .leaf
                .is_some_and(|leaf| leaf.channel == ContentChannel::Comment)
        {
            continue;
        }
        append_changed_line_edits(
            &mut edits,
            &pair.before,
            None,
            &pair.after,
            Some(node.lines.clone()),
        );
    }
    deduplicate_line_edits(&mut edits);
    edits
}

/// Exact decorations presented with their semantic owner, never as independent signals.
#[derive(Default)]
struct RetainedDecorations {
    before: HashSet<NodeId>,
    after: HashSet<NodeId>,
}

fn retained_decorations(
    pair: &SyntaxPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
) -> RetainedDecorations {
    let mut dependents = RetainedDecorations::default();
    for link in correspondence.unit_composites(unit) {
        let before = pair.before.node(link.before);
        let after = pair.after.node(link.after);
        if before.decoration_owner.is_none()
            || after.decoration_owner.is_none()
            || !physically_equal_isolated_nodes(pair, link.before, link.after)
        {
            continue;
        }
        dependents
            .before
            .extend(descendant_leaves(&pair.before, link.before));
        dependents
            .after
            .extend(descendant_leaves(&pair.after, link.after));
    }
    for link in correspondence.unit_leaf_links(unit) {
        if link.relation != LeafRelation::Exact
            || pair.before.node(link.before).decoration_owner.is_none()
            || pair.after.node(link.after).decoration_owner.is_none()
            || !physically_equal_isolated_nodes(pair, link.before, link.after)
        {
            continue;
        }
        dependents.before.insert(link.before);
        dependents.after.insert(link.after);
    }
    dependents
}

fn physically_equal_isolated_nodes(
    pair: &SyntaxPair<'_, '_>,
    before: NodeId,
    after: NodeId,
) -> bool {
    if !node_is_presentational_line_isolated(&pair.before, before)
        || !node_is_presentational_line_isolated(&pair.after, after)
    {
        return false;
    }
    let lines = |tree: &SyntaxTree<'_>, node| {
        line_indices(
            Some(tree.node(node).lines.clone()),
            tree.source.lines().len(),
        )
    };
    let before = lines(&pair.before, before);
    let after = lines(&pair.after, after);
    physical_lines_equal(pair, &before, &after)
}

/// A decoration may own its newline, but never neighboring source content.
fn node_is_presentational_line_isolated(tree: &SyntaxTree<'_>, node: NodeId) -> bool {
    let node = tree.node(node);
    let Some(first) = tree.source.line(node.lines.start) else {
        return false;
    };
    let Some(last_number) = node.lines.end.checked_sub(1) else {
        return false;
    };
    let Some(last) = tree.source.line(last_number) else {
        return false;
    };
    if node.bytes.start < first.content_bytes.start || node.bytes.end > last.full_bytes.end {
        return false;
    }
    if node.bytes.end > last.content_bytes.end && node.bytes.end != last.full_bytes.end {
        return false;
    }

    let prefix = tree
        .source
        .slice(first.content_bytes.start..node.bytes.start);
    let suffix_start = node.bytes.end.min(last.content_bytes.end);
    let suffix = tree.source.slice(suffix_start..last.content_bytes.end);
    prefix.is_some_and(horizontal_layout) && suffix.is_some_and(horizontal_layout)
}

fn modified_payload_edits(
    pair: &SyntaxPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
) -> Vec<LineEdit> {
    let mut edits = Vec::new();
    for link in correspondence.unit_leaf_links(unit) {
        if link.relation != LeafRelation::Modified {
            continue;
        }
        let before = pair.before.node(link.before);
        let after = pair.after.node(link.after);
        let significant = before.leaf.is_some_and(|leaf| {
            !matches!(
                leaf.channel,
                ContentChannel::Comment | ContentChannel::Layout
            )
        }) && after.leaf.is_some_and(|leaf| {
            !matches!(
                leaf.channel,
                ContentChannel::Comment | ContentChannel::Layout
            )
        });
        if !significant {
            continue;
        }
        append_changed_line_edits(
            &mut edits,
            &pair.before,
            Some(before.lines.clone()),
            &pair.after,
            Some(after.lines.clone()),
        );
    }
    deduplicate_line_edits(&mut edits);
    edits
}

fn marked_leaf_ranges(tree: &SyntaxTree<'_>, leaves: HashSet<NodeId>) -> Vec<Range<usize>> {
    leaves
        .iter()
        .copied()
        .filter_map(|leaf| {
            let node = tree.node(leaf);
            if matches!(
                node.leaf?.channel,
                ContentChannel::Comment | ContentChannel::Layout
            ) {
                return None;
            }

            let mut bytes = node.bytes.clone();
            let mut parent = node.parent;
            while let Some(owner) = parent {
                let payload = descendant_leaves(tree, owner)
                    .filter(|candidate| leaf_is_meaningful_payload(tree, *candidate))
                    .collect::<Vec<_>>();
                if payload.is_empty() || payload.iter().any(|candidate| !leaves.contains(candidate))
                {
                    break;
                }

                let owner = tree.node(owner);
                bytes = owner.source_envelope.clone();
                if owner.review.is_some() {
                    break;
                }
                parent = owner.parent;
            }
            Some(bytes)
        })
        .collect()
}

fn fully_marked_line_edits(
    pair: &SyntaxPair<'_, '_>,
    before_lines: Range<usize>,
    after_lines: Range<usize>,
    before_marked: &[Range<usize>],
    after_marked: &[Range<usize>],
) -> Vec<LineEdit> {
    let mut edits = Vec::new();
    for line in collect_source_lines(&pair.before, before_lines) {
        let display = select_line(line, before_marked);
        if line_is_fully_highlighted(&pair.before, &display) {
            edits.push(changed_line_edit(
                &pair.before,
                Some(line),
                &pair.after,
                None,
            ));
        }
    }
    for line in collect_source_lines(&pair.after, after_lines) {
        let display = select_line(line, after_marked);
        if line_is_fully_highlighted(&pair.after, &display) {
            edits.push(changed_line_edit(
                &pair.before,
                None,
                &pair.after,
                Some(line),
            ));
        }
    }
    edits
}

fn line_is_fully_highlighted(tree: &SyntaxTree<'_>, line: &SelectedLine) -> bool {
    if !line_has_material_highlight(tree, line) {
        return false;
    }
    let source = &tree.source.lines()[line.number - 1];
    let mut cursor = source.content_bytes.start;
    for highlight in &line.highlights {
        if cursor < highlight.start
            && tree
                .source
                .slice(cursor..highlight.start)
                .is_some_and(|text| !text.trim().is_empty())
        {
            return false;
        }
        cursor = cursor.max(highlight.end);
    }
    cursor >= source.content_bytes.end
        || tree
            .source
            .slice(cursor..source.content_bytes.end)
            .is_some_and(|text| text.trim().is_empty())
}

fn deduplicate_line_edits(edits: &mut Vec<LineEdit>) {
    edits.sort_by_key(line_edit_order);
    edits.dedup_by(|left, right| {
        left.before.as_ref().map(|line| line.number)
            == right.before.as_ref().map(|line| line.number)
            && left.after.as_ref().map(|line| line.number)
                == right.after.as_ref().map(|line| line.number)
    });
}

/// Whole-line rendering cannot represent two semantic facts that claim the same source row.
fn line_edits_compete_for_source_rows(edits: &[LineEdit]) -> bool {
    let mut before = HashSet::new();
    let mut after = HashSet::new();
    edits.iter().any(|edit| {
        edit.before
            .as_ref()
            .is_some_and(|line| !before.insert(line.number))
            || edit
                .after
                .as_ref()
                .is_some_and(|line| !after.insert(line.number))
    })
}

fn append_changed_line_edits(
    edits: &mut Vec<LineEdit>,
    before: &SyntaxTree<'_>,
    before_lines: Option<Range<usize>>,
    after: &SyntaxTree<'_>,
    after_lines: Option<Range<usize>>,
) {
    let before_lines = before_lines
        .map(|lines| collect_source_lines(before, lines))
        .unwrap_or_default();
    let after_lines = after_lines
        .map(|lines| collect_source_lines(after, lines))
        .unwrap_or_default();
    let paired = before_lines.len().min(after_lines.len());
    for index in 0..paired {
        edits.push(changed_line_edit(
            before,
            Some(before_lines[index]),
            after,
            Some(after_lines[index]),
        ));
    }
    for line in &before_lines[paired..] {
        edits.push(changed_line_edit(before, Some(line), after, None));
    }
    for line in &after_lines[paired..] {
        edits.push(changed_line_edit(before, None, after, Some(line)));
    }
}

/// Materialize one raw-diff-classified line edit with byte-exact marks.
fn changed_source_rows(
    before: &SyntaxTree<'_>,
    before_line: Option<&SourceLine>,
    after: &SyntaxTree<'_>,
    after_line: Option<&SourceLine>,
) -> (Option<SelectedLine>, Option<SelectedLine>) {
    let (Some(before_line), Some(after_line)) = (before_line, after_line) else {
        let before =
            before_line.map(|line| select_line(line, std::slice::from_ref(&line.content_bytes)));
        let after =
            after_line.map(|line| select_line(line, std::slice::from_ref(&line.content_bytes)));
        return (before, after);
    };

    let before_text = before.source.text(before_line);
    let after_text = after.source.text(after_line);
    if before_text == after_text {
        return (
            Some(select_line(
                before_line,
                std::slice::from_ref(&before_line.content_bytes),
            )),
            Some(select_line(
                after_line,
                std::slice::from_ref(&after_line.content_bytes),
            )),
        );
    }

    let (before_changed, after_changed) = changed_byte_ranges(
        before_line.content_bytes.start,
        before_text,
        after_line.content_bytes.start,
        after_text,
    );
    let before_changed = snap_change_to_syntax_node(before, before_line, before_changed);
    let after_changed = snap_change_to_syntax_node(after, after_line, after_changed);
    let before_mark = (!before_changed.is_empty()).then_some(before_changed);
    let after_mark = (!after_changed.is_empty()).then_some(after_changed);
    (
        Some(select_line(before_line, before_mark.as_slice())),
        Some(select_line(after_line, after_mark.as_slice())),
    )
}

/// Expand one textual delta to the smallest complete syntax node made entirely of that delta.
fn snap_change_to_syntax_node(
    syntax: &SyntaxTree<'_>,
    line: &SourceLine,
    changed: Range<usize>,
) -> Range<usize> {
    syntax
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            node.named
                && line.content_bytes.start <= node.source_envelope.start
                && node.source_envelope.end <= line.content_bytes.end
                && node.source_envelope.start <= changed.start
                && changed.end <= node.source_envelope.end
        })
        .filter_map(|(index, node)| {
            let id = NodeId::new(index);
            let payload = descendant_leaves(syntax, id)
                .filter(|leaf| leaf_is_meaningful_payload(syntax, *leaf))
                .collect::<Vec<_>>();
            (!payload.is_empty()
                && payload.iter().all(|leaf| {
                    let bytes = &syntax.node(*leaf).bytes;
                    ranges_overlap(bytes, &changed)
                }))
            .then_some((
                node.source_envelope == node.bytes,
                node.source_envelope.clone(),
            ))
        })
        .min_by_key(|(has_no_owned_layout, bytes)| {
            (*has_no_owned_layout, bytes.end.saturating_sub(bytes.start))
        })
        .map(|(_, bytes)| bytes)
        .unwrap_or(changed)
}

fn changed_byte_ranges(
    before_start: usize,
    before: &str,
    after_start: usize,
    after: &str,
) -> (Range<usize>, Range<usize>) {
    let before_characters = before.chars().collect::<Vec<_>>();
    let after_characters = after.chars().collect::<Vec<_>>();
    let (before_changed, after_changed) =
        changed_sequence_ranges(&before_characters, &after_characters);
    let byte_at_character = |text: &str, character| {
        text.char_indices()
            .nth(character)
            .map(|(byte, _)| byte)
            .unwrap_or(text.len())
    };
    (
        before_start + byte_at_character(before, before_changed.start)
            ..before_start + byte_at_character(before, before_changed.end),
        after_start + byte_at_character(after, after_changed.start)
            ..after_start + byte_at_character(after, after_changed.end),
    )
}

pub(super) fn changed_sequence_ranges<T: Eq>(
    before: &[T],
    after: &[T],
) -> (Range<usize>, Range<usize>) {
    let prefix = before
        .iter()
        .zip(after)
        .take_while(|(before, after)| before == after)
        .count();
    let suffix_budget = before.len().min(after.len()).saturating_sub(prefix);
    let suffix = before[prefix..]
        .iter()
        .rev()
        .zip(after[prefix..].iter().rev())
        .take(suffix_budget)
        .take_while(|(before, after)| before == after)
        .count();
    (
        prefix..before.len().saturating_sub(suffix),
        prefix..after.len().saturating_sub(suffix),
    )
}

fn changed_line_edit(
    before: &SyntaxTree<'_>,
    before_line: Option<&SourceLine>,
    after: &SyntaxTree<'_>,
    after_line: Option<&SourceLine>,
) -> LineEdit {
    let (before_row, after_row) = changed_source_rows(before, before_line, after, after_line);
    LineEdit {
        before: before_row,
        after: after_row,
        line_ending: visible_line_ending_change(before_line, after_line),
    }
}

fn visible_line_ending_change(
    before: Option<&SourceLine>,
    after: Option<&SourceLine>,
) -> Option<LineEndingChange> {
    let before = before.map(ending_endpoint);
    let after = after.map(ending_endpoint);
    let ending_changed =
        before.map(|endpoint| endpoint.ending) != after.map(|endpoint| endpoint.ending);
    let visible_missing = before.is_some_and(|endpoint| endpoint.ending == LineEnding::Missing)
        || after.is_some_and(|endpoint| endpoint.ending == LineEnding::Missing);
    let paired_change = ending_changed && before.is_some() && after.is_some();
    let one_sided_missing = visible_missing && (before.is_none() || after.is_none());
    (paired_change || one_sided_missing).then_some(LineEndingChange { before, after })
}

fn line_edit_order(edit: &LineEdit) -> usize {
    edit.after
        .as_ref()
        .or(edit.before.as_ref())
        .map(|line| line.number)
        .expect("a line edit always has a source side")
}

fn append_line_edit_rows(rows: &mut Vec<RowEvent>, edit: LineEdit) {
    rows.extend(edit.before.map(RowEvent::Removed));
    rows.extend(edit.after.map(RowEvent::Added));
    rows.extend(edit.line_ending.map(RowEvent::LineEnding));
}

fn descendant_leaves<'tree>(
    tree: &'tree SyntaxTree<'_>,
    root: NodeId,
) -> impl Iterator<Item = NodeId> + 'tree {
    std::iter::once(root)
        .chain(tree.descendants(root))
        .filter(|id| tree.node(*id).leaf.is_some())
}

fn form_whole_file_lines(
    pair: &SyntaxPair<'_, '_>,
    correspondence: &Correspondence,
    anchor_facts: &AnchorFacts,
) -> Vec<FormedExcerpt> {
    let before =
        (!pair.before.source.lines().is_empty()).then(|| 1..pair.before.source.lines().len() + 1);
    let after =
        (!pair.after.source.lines().is_empty()).then(|| 1..pair.after.source.lines().len() + 1);
    form_line_region(
        pair,
        correspondence,
        before,
        after,
        &correspondence.composites,
        None,
        LineAnchors::new(AnchorBasis::Physical, anchor_facts),
    )
}

/// Monotone physical-line fact retained before rows are selected or abbreviated.
#[derive(Clone, Debug)]
enum LineFact {
    Context {
        after: usize,
    },
    Edit {
        before: Range<usize>,
        after: Range<usize>,
    },
    /// A physically paired row kept atomic so its concrete terminator stays visible.
    TerminatorEdit {
        before: usize,
        after: usize,
    },
    Reflow {
        after: Range<usize>,
        unchanged_after: HashSet<usize>,
    },
}

#[derive(Clone, Debug)]
struct RetainedRegion {
    before: Range<usize>,
    after: Range<usize>,
    retention: Retention,
}

/// SyntaxTree-wide subtree facts used to admit structural display anchors in constant time.
struct AnchorFacts {
    before: Vec<NodeAnchorFacts>,
    after: Vec<NodeAnchorFacts>,
}

impl AnchorFacts {
    fn new(pair: &SyntaxPair<'_, '_>) -> Self {
        Self {
            before: syntax_anchor_facts(&pair.before),
            after: syntax_anchor_facts(&pair.after),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct NodeAnchorFacts {
    has_opaque: bool,
    has_payload: bool,
}

fn syntax_anchor_facts(tree: &SyntaxTree<'_>) -> Vec<NodeAnchorFacts> {
    let mut facts = vec![NodeAnchorFacts::default(); tree.nodes.len()];
    for index in (0..tree.nodes.len()).rev() {
        let node = &tree.nodes[index];
        let mut fact = node
            .leaf
            .map_or_else(NodeAnchorFacts::default, |leaf| NodeAnchorFacts {
                has_opaque: leaf.channel == ContentChannel::Opaque,
                has_payload: leaf.channel != ContentChannel::Layout && leaf.delimiter.is_none(),
            });
        for child in &node.children {
            let child = facts[child.index()];
            fact.has_opaque |= child.has_opaque;
            fact.has_payload |= child.has_payload;
        }
        facts[index] = fact;
    }
    facts
}

/// Whether a structural region survived exactly or changed physical layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Retention {
    Exact,
    Reflow,
}

#[derive(Clone, Debug)]
enum LineCheckpoint {
    Exact { before: usize, after: usize },
    TerminatorEdit { before: usize, after: usize },
    Retained(RetainedRegion),
}

/// How exact correspondence participates in one physical replacement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AnchorBasis {
    /// Exact physical rows are the only available structure for line-oriented content.
    Physical,
    /// Closed subtrees anchor; remaining exact rows only guide display cadence.
    Structural {
        /// Shared outer rows frame the same source owner instead of a renamed replacement.
        same_owner: bool,
    },
}

/// Structural evidence available while partitioning one physical line region.
#[derive(Clone, Copy)]
struct LineAnchors<'facts> {
    basis: AnchorBasis,
    facts: &'facts AnchorFacts,
}

impl<'facts> LineAnchors<'facts> {
    fn new(basis: AnchorBasis, facts: &'facts AnchorFacts) -> Self {
        Self { basis, facts }
    }
}

/// Shared outer rows frame a definition only while its source identity survives.
fn structural_anchor_basis(pair: &SyntaxPair<'_, '_>, unit: &MatchedUnit) -> AnchorBasis {
    let same_owner = match (
        pair.before.identity_text(unit.before),
        pair.after.identity_text(unit.after),
    ) {
        (Some(before), Some(after)) => before == after,
        (None, _) | (_, None) => false,
    };
    AnchorBasis::Structural { same_owner }
}

fn form_line_region(
    pair: &SyntaxPair<'_, '_>,
    correspondence: &Correspondence,
    before_lines: Option<Range<usize>>,
    after_lines: Option<Range<usize>>,
    composites: &[NodeLink],
    after_root: Option<NodeId>,
    anchors: LineAnchors<'_>,
) -> Vec<FormedExcerpt> {
    let before = line_indices(before_lines, pair.before.source.lines().len());
    let after = line_indices(after_lines, pair.after.source.lines().len());
    let retained = retained_regions(pair, anchors.facts, composites, &before, &after);
    let facts = line_facts(correspondence, before, after, retained, anchors.basis);
    form_line_facts(pair, &facts, after_root)
}

fn line_indices(lines: Option<Range<usize>>, line_count: usize) -> Range<usize> {
    let Some(lines) = lines else {
        return 0..0;
    };
    let start = lines.start.saturating_sub(1).min(line_count);
    let end = lines.end.saturating_sub(1).min(line_count);
    start.min(end)..end
}

fn retained_regions(
    pair: &SyntaxPair<'_, '_>,
    anchor_facts: &AnchorFacts,
    composites: &[NodeLink],
    before_region: &Range<usize>,
    after_region: &Range<usize>,
) -> Vec<RetainedRegion> {
    // Only locally eligible anchors vote on global order. Decorations follow
    // semantic owners, and reordered links cannot partition stable source.
    let composites = composites
        .iter()
        .copied()
        .filter(|link| {
            link.placement == Placement::Stable && !link_belongs_to_decoration(pair, *link)
        })
        .collect::<Vec<_>>();
    let crossed = structural_link_crossings(pair, &composites);
    let mut candidates = composites
        .into_iter()
        .zip(crossed)
        .filter(|(_, crossed)| !crossed)
        .filter_map(|(link, _)| {
            retained_region(pair, anchor_facts, link, before_region, after_region)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.before
            .start
            .cmp(&right.before.start)
            .then_with(|| right.before.len().cmp(&left.before.len()))
            .then_with(|| left.after.start.cmp(&right.after.start))
    });

    let mut retained = Vec::new();
    let mut before_floor = before_region.start;
    let mut after_floor = after_region.start;
    for candidate in candidates {
        if candidate.before.start < before_floor || candidate.after.start < after_floor {
            continue;
        }
        before_floor = candidate.before.end;
        after_floor = candidate.after.end;
        retained.push(candidate);
    }
    retained
}

/// Exact regions participating in an order inversion cannot partition presentation.
fn structural_link_crossings(pair: &SyntaxPair<'_, '_>, composites: &[NodeLink]) -> Vec<bool> {
    let mut order = (0..composites.len()).collect::<Vec<_>>();
    order.sort_unstable_by_key(|index| {
        let link = composites[*index];
        (
            pair.before.node(link.before).bytes.start,
            pair.after.node(link.after).bytes.start,
        )
    });
    let mut crossed = vec![false; composites.len()];

    let mut prefix_max = None;
    let mut start = 0;
    while start < order.len() {
        let before = pair
            .before
            .node(composites[order[start]].before)
            .bytes
            .start;
        let end = order[start..].partition_point(|index| {
            pair.before.node(composites[*index].before).bytes.start == before
        }) + start;
        for index in &order[start..end] {
            let after = pair.after.node(composites[*index].after).bytes.start;
            crossed[*index] |= prefix_max.is_some_and(|maximum| maximum > after);
        }
        let group_max = order[start..end]
            .iter()
            .map(|index| pair.after.node(composites[*index].after).bytes.start)
            .max();
        prefix_max = prefix_max.max(group_max);
        start = end;
    }

    let mut suffix_min = None;
    let mut end = order.len();
    while end > 0 {
        let before = pair
            .before
            .node(composites[order[end - 1]].before)
            .bytes
            .start;
        let start = order[..end].partition_point(|index| {
            pair.before.node(composites[*index].before).bytes.start < before
        });
        for index in &order[start..end] {
            let after = pair.after.node(composites[*index].after).bytes.start;
            crossed[*index] |= suffix_min.is_some_and(|minimum| minimum < after);
        }
        let group_min = order[start..end]
            .iter()
            .map(|index| pair.after.node(composites[*index].after).bytes.start)
            .min();
        suffix_min = match (suffix_min, group_min) {
            (Some(suffix), Some(group)) => Some(suffix.min(group)),
            (None, group) | (group, None) => group,
        };
        end = start;
    }
    crossed
}

fn retained_region(
    pair: &SyntaxPair<'_, '_>,
    anchor_facts: &AnchorFacts,
    link: NodeLink,
    before_region: &Range<usize>,
    after_region: &Range<usize>,
) -> Option<RetainedRegion> {
    if link.placement == Placement::Reordered || link_belongs_to_decoration(pair, link) {
        return None;
    }
    let before = pair.before.node(link.before);
    let after = pair.after.node(link.after);
    if !node_owns_complete_lines(&pair.before, link.before)
        || !node_owns_complete_lines(&pair.after, link.after)
    {
        return None;
    }

    let before_lines = line_indices(Some(before.lines.clone()), pair.before.source.lines().len());
    let after_lines = line_indices(Some(after.lines.clone()), pair.after.source.lines().len());
    if before_lines.is_empty()
        || after_lines.is_empty()
        || before_lines.start < before_region.start
        || before_lines.end > before_region.end
        || after_lines.start < after_region.start
        || after_lines.end > after_region.end
    {
        return None;
    }
    let before_facts = anchor_facts.before[link.before.index()];
    let after_facts = anchor_facts.after[link.after.index()];
    if before_facts.has_opaque
        || after_facts.has_opaque
        || !before_facts.has_payload
        || !after_facts.has_payload
    {
        return None;
    }
    if !same_line_endings(pair, &before_lines, &after_lines) {
        return None;
    }
    let physical_equal = physical_lines_equal(pair, &before_lines, &after_lines);
    let retention = if physical_equal && link.wrapper.is_none() {
        Retention::Exact
    } else {
        Retention::Reflow
    };
    Some(RetainedRegion {
        before: before_lines,
        after: after_lines,
        retention,
    })
}

/// Decoration subtrees inherit presentation from their semantic owner.
fn link_belongs_to_decoration(pair: &SyntaxPair<'_, '_>, link: NodeLink) -> bool {
    node_belongs_to_decoration(&pair.before, link.before)
        || node_belongs_to_decoration(&pair.after, link.after)
}

fn node_belongs_to_decoration(tree: &SyntaxTree<'_>, mut id: NodeId) -> bool {
    loop {
        let node = tree.node(id);
        if node.decoration_owner.is_some() {
            return true;
        }
        let Some(parent) = node.parent else {
            return false;
        };
        id = parent;
    }
}

fn same_line_endings(
    pair: &SyntaxPair<'_, '_>,
    before: &Range<usize>,
    after: &Range<usize>,
) -> bool {
    before.clone().zip(after.clone()).all(|(before, after)| {
        pair.before.source.lines()[before].ending == pair.after.source.lines()[after].ending
    })
}

fn physical_lines_equal(
    pair: &SyntaxPair<'_, '_>,
    before: &Range<usize>,
    after: &Range<usize>,
) -> bool {
    if before.len() != after.len() {
        return false;
    }
    before.clone().zip(after.clone()).all(|(before, after)| {
        let before = &pair.before.source.lines()[before];
        let after = &pair.after.source.lines()[after];
        pair.before.source.full_text(before) == pair.after.source.full_text(after)
    })
}

fn line_facts(
    correspondence: &Correspondence,
    before: Range<usize>,
    after: Range<usize>,
    retained: Vec<RetainedRegion>,
    anchor_basis: AnchorBasis,
) -> Vec<LineFact> {
    let before_bounds = before.clone();
    let after_bounds = after.clone();
    let mut checkpoints = Vec::new();
    let mut before_start = before.start;
    let mut after_start = after.start;
    for region in retained {
        checkpoints.extend(display_line_checkpoints(
            correspondence,
            before_start..region.before.start,
            after_start..region.after.start,
            &before_bounds,
            &after_bounds,
            anchor_basis,
        ));
        before_start = region.before.end;
        after_start = region.after.end;
        checkpoints.push(LineCheckpoint::Retained(region));
    }
    checkpoints.extend(display_line_checkpoints(
        correspondence,
        before_start..before.end,
        after_start..after.end,
        &before_bounds,
        &after_bounds,
        anchor_basis,
    ));

    let mut facts = Vec::new();
    let mut before_start = before.start;
    let mut after_start = after.start;
    for checkpoint in checkpoints {
        let (before_end, after_end) = match &checkpoint {
            LineCheckpoint::Exact { before, after } => (*before, *after),
            LineCheckpoint::TerminatorEdit { before, after } => (*before, *after),
            LineCheckpoint::Retained(region) => (region.before.start, region.after.start),
        };
        if before_start < before_end || after_start < after_end {
            facts.push(LineFact::Edit {
                before: before_start..before_end,
                after: after_start..after_end,
            });
        }

        match checkpoint {
            LineCheckpoint::Exact { before, after } => {
                facts.push(LineFact::Context { after });
                before_start = before + 1;
                after_start = after + 1;
            }
            LineCheckpoint::TerminatorEdit { before, after } => {
                facts.push(LineFact::TerminatorEdit { before, after });
                before_start = before + 1;
                after_start = after + 1;
            }
            LineCheckpoint::Retained(region) => {
                before_start = region.before.end;
                after_start = region.after.end;
                match region.retention {
                    Retention::Exact => {
                        debug_assert_eq!(region.before.len(), region.after.len());
                        facts.extend(
                            region
                                .before
                                .zip(region.after)
                                .map(|(_, after)| LineFact::Context { after }),
                        );
                    }
                    Retention::Reflow => {
                        let unchanged_after = correspondence
                            .line_links_in(region.before.clone(), region.after.clone())
                            .map(|link| link.after)
                            .collect();
                        facts.push(LineFact::Reflow {
                            after: region.after,
                            unchanged_after,
                        });
                    }
                }
            }
        }
    }
    if before_start < before.end || after_start < after.end {
        facts.push(LineFact::Edit {
            before: before_start..before.end,
            after: after_start..after.end,
        });
    }
    facts
}

fn display_line_checkpoints(
    correspondence: &Correspondence,
    before: Range<usize>,
    after: Range<usize>,
    before_bounds: &Range<usize>,
    after_bounds: &Range<usize>,
    basis: AnchorBasis,
) -> Vec<LineCheckpoint> {
    let checkpoints = physical_line_checkpoints(correspondence, before, after);
    let AnchorBasis::Structural { same_owner } = basis else {
        return checkpoints;
    };

    checkpoints
        .into_iter()
        .filter_map(|checkpoint| {
            let LineCheckpoint::Exact { before, after } = checkpoint else {
                return Some(checkpoint);
            };
            let first = before == before_bounds.start && after == after_bounds.start;
            let last = before.checked_add(1) == Some(before_bounds.end)
                && after.checked_add(1) == Some(after_bounds.end);
            if !same_owner && (first || last) {
                return None;
            }
            Some(LineCheckpoint::Exact { before, after })
        })
        .collect()
}

fn physical_line_checkpoints(
    correspondence: &Correspondence,
    before: Range<usize>,
    after: Range<usize>,
) -> Vec<LineCheckpoint> {
    let mut checkpoints = correspondence
        .line_links_in(before.clone(), after.clone())
        .map(|link| LineCheckpoint::Exact {
            before: link.before,
            after: link.after,
        })
        .collect::<Vec<_>>();
    checkpoints.extend(
        correspondence
            .line_ending_edits_in(before.clone(), after.clone())
            .map(|link| LineCheckpoint::TerminatorEdit {
                before: link.before,
                after: link.after,
            }),
    );
    checkpoints.sort_by_key(|checkpoint| match checkpoint {
        LineCheckpoint::Exact { before, after, .. }
        | LineCheckpoint::TerminatorEdit { before, after } => (*before, *after),
        LineCheckpoint::Retained(_) => unreachable!("retained regions are inserted separately"),
    });
    checkpoints
}

fn form_line_facts(
    pair: &SyntaxPair<'_, '_>,
    facts: &[LineFact],
    after_root: Option<NodeId>,
) -> Vec<FormedExcerpt> {
    let mut rows = Vec::new();
    for fact in facts {
        append_line_fact_rows(&mut rows, pair, fact);
    }
    formed_excerpts(rows, after_root)
}

fn append_line_fact_rows(rows: &mut Vec<RowEvent>, pair: &SyntaxPair<'_, '_>, fact: &LineFact) {
    match fact {
        LineFact::Context { after } => {
            rows.push(RowEvent::Current(select_line(
                &pair.after.source.lines()[*after],
                &[],
            )));
        }
        LineFact::Edit { before, after } => {
            append_line_change_rows(rows, pair, before.clone(), after.clone());
        }
        LineFact::TerminatorEdit { before, after } => {
            append_line_change_rows(rows, pair, *before..*before + 1, *after..*after + 1);
        }
        LineFact::Reflow {
            after,
            unchanged_after,
        } => {
            append_retained_region_rows(rows, pair, after, unchanged_after);
        }
    }
}

fn append_line_change_rows(
    rows: &mut Vec<RowEvent>,
    pair: &SyntaxPair<'_, '_>,
    before: Range<usize>,
    after: Range<usize>,
) {
    if before.len() == 1 && after.len() == 1 {
        let before_line = &pair.before.source.lines()[before.start];
        let after_line = &pair.after.source.lines()[after.start];
        let (before_row, after_row) = changed_source_rows(
            &pair.before,
            Some(before_line),
            &pair.after,
            Some(after_line),
        );
        rows.extend(before_row.map(RowEvent::Removed));
        rows.extend(after_row.map(RowEvent::Added));
        rows.extend(
            visible_line_ending_change(Some(before_line), Some(after_line))
                .map(RowEvent::LineEnding),
        );
        return;
    }

    // A gap has no line-level correspondence: keep each revision as one coherent run.
    for index in before {
        let line = &pair.before.source.lines()[index];
        rows.push(RowEvent::Removed(select_line(
            line,
            std::slice::from_ref(&line.content_bytes),
        )));
        if line.ending == LineEnding::Missing {
            rows.push(RowEvent::LineEnding(LineEndingChange {
                before: Some(ending_endpoint(line)),
                after: None,
            }));
        }
    }
    for index in after {
        let line = &pair.after.source.lines()[index];
        rows.push(RowEvent::Added(select_line(
            line,
            std::slice::from_ref(&line.content_bytes),
        )));
        if line.ending == LineEnding::Missing {
            rows.push(RowEvent::LineEnding(LineEndingChange {
                before: None,
                after: Some(ending_endpoint(line)),
            }));
        }
    }
}

fn append_retained_region_rows(
    rows: &mut Vec<RowEvent>,
    pair: &SyntaxPair<'_, '_>,
    after: &Range<usize>,
    unchanged_after: &HashSet<usize>,
) {
    for index in after.clone() {
        let line = select_line(&pair.after.source.lines()[index], &[]);
        let row = if unchanged_after.contains(&index) {
            RowEvent::Current(line)
        } else {
            RowEvent::Reflow(line)
        };
        rows.push(row);
    }
}

fn collect_source_lines<'tree>(
    tree: &'tree SyntaxTree<'_>,
    lines: Range<usize>,
) -> Vec<&'tree SourceLine> {
    lines
        .filter_map(|number| tree.source.line(number))
        .collect()
}

fn row_after_source_line(row: &RowEvent) -> Option<usize> {
    match row {
        RowEvent::Current(line) | RowEvent::Reflow(line) => Some(line.number),
        RowEvent::Added(line) => Some(line.number),
        RowEvent::Moved { after, .. } => Some(after.number),
        RowEvent::Compact(word) => word.after_line,
        RowEvent::LineEnding(change) => change.after.map(|endpoint| endpoint.line),
        RowEvent::Elision(coverage) => coverage.after.as_ref().map(|range| range.start),
        RowEvent::Removed(_) => None,
    }
}

fn row_displayed_after_source_line(row: &RowEvent) -> Option<usize> {
    if matches!(row, RowEvent::LineEnding(_) | RowEvent::Elision(_)) {
        return None;
    }
    row_after_source_line(row)
}

fn row_before_source_line(row: &RowEvent) -> Option<usize> {
    match row {
        RowEvent::Removed(line) => Some(line.number),
        RowEvent::Moved { before, .. } => *before,
        RowEvent::Compact(word) => word.before_line,
        RowEvent::LineEnding(change) => change.before.map(|endpoint| endpoint.line),
        RowEvent::Elision(coverage) => coverage.before.as_ref().map(|range| range.start),
        RowEvent::Current(_) | RowEvent::Reflow(_) | RowEvent::Added(_) => None,
    }
}
