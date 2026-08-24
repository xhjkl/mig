//! Pretty-hunk planning over neutral correspondence facts.
//!
//! Every signal except a move grows a three-line context halo. Neutral CST ancestors
//! contribute hierarchy breadcrumbs with display-only halos that do not widen
//! the physical signal focus used to meld neighboring changes.

use super::change::{AfterGap, Buoyancy, Signal};
use super::context::{
    CONTEXT_HALO_RADIUS, breadcrumb_halo_indices, context_gap_fits_halos, context_halo_end,
    context_halo_start, merge_windows_touch, ranges_overlap, review_selection_ranges,
};
use super::correspondence::{
    Correspondence, LeafRelation, LineLink, MatchedUnit, NodeLink, Placement, UnitEdit,
    ordered_matches,
};
use super::projection::{ContentChannel, NodeId, Projection, ProjectionPair, ReviewMode};
use super::render::{
    MarkedRange, build_display_line, build_display_lines, changed_display_lines, word_diff,
};
use super::source::SourceLine;
use super::{DiffMark, DiffRow, DisplayLine, Hunk, LineCoverage, LineEnding};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// Producer-local source excerpt with materialized review rows and coverage.
struct ReviewExcerpt {
    coverage: LineCoverage,
    rows: Vec<DiffRow>,
}

/// One classified producer fragment in its native before/after source coordinates.
struct ChangeFragment {
    signal: Signal,
    coverage: LineCoverage,
    groups: Vec<Vec<DiffRow>>,
    focus: SignalFocus,
    order_hint: Option<SourceOrder>,
}

impl ChangeFragment {
    fn new(signal: Signal, excerpt: ReviewExcerpt) -> Self {
        let focus = SignalFocus::from_rows(&excerpt.rows);
        Self {
            signal,
            coverage: excerpt.coverage,
            groups: group_rows(excerpt.rows),
            focus,
            order_hint: None,
        }
    }

    /// Semantic-unit placement for a before-only fragment with no current source rows.
    fn at_order(mut self, order: SourceOrder) -> Self {
        self.order_hint = Some(order);
        self
    }

    fn edit(excerpt: ReviewExcerpt) -> Self {
        Self::new(Signal::Edit, excerpt)
    }

    /// Low-signal source wiring selected explicitly by the language frontend.
    fn wiring(excerpt: ReviewExcerpt) -> Self {
        Self::new(Signal::Wiring, excerpt)
    }

    fn moved(excerpt: ReviewExcerpt) -> Self {
        Self::new(Signal::Move, excerpt)
    }

    fn reflow(excerpt: ReviewExcerpt) -> Self {
        Self::new(Signal::Reflow, excerpt)
    }

    /// Place source-local focus into current-world geometry for ordering and melding.
    fn place(self, alignment: &LineAlignment<'_>) -> AnchoredChange {
        let order = self
            .order_hint
            .unwrap_or_else(|| alignment.focus_order(&self.focus));
        let hinted_focus = self
            .order_hint
            .and_then(|order| alignment.order_focus(order));
        let hinted_line = hinted_focus.as_ref().map(|focus| focus.start);
        let focus = hinted_focus
            .or_else(|| alignment.current_focus(&self.focus))
            .or_else(|| line_hull(&self.focus.before))
            .expect("classified change owns current or before-side focus");
        let merge_window = merge_window(focus, self.signal);
        let context_focus = match (self.signal.receives_context(), hinted_line) {
            (false, _) => SignalFocus::default(),
            (true, Some(line)) => SignalFocus::after_line(line),
            (true, None) => self.focus,
        };
        AnchoredChange {
            buoyancy: self.signal.buoyancy(),
            coverage: self.coverage,
            groups: self.groups,
            context_focus,
            order,
            merge_window,
        }
    }
}

/// Review fragments anchored to current-world order and ready to meld.
struct AnchoredChange {
    buoyancy: Buoyancy,
    coverage: LineCoverage,
    groups: Vec<Vec<DiffRow>>,
    context_focus: SignalFocus,
    order: SourceOrder,
    merge_window: Range<usize>,
}

impl AnchoredChange {
    fn after_gap(&self) -> AfterGap {
        self.order.after_gap()
    }

    fn needs_context(&self) -> bool {
        !self.context_focus.is_empty()
    }

    /// Absorb a later anchored change after source geometry selects neighboring focus.
    fn meld(&mut self, mut later: Self) {
        debug_assert!(self.order <= later.order);
        self.buoyancy = self.buoyancy.max(later.buoyancy);
        include_optional_coverage(&mut self.coverage.before, later.coverage.before);
        include_optional_coverage(&mut self.coverage.after, later.coverage.after);
        self.groups.append(&mut later.groups);
        self.context_focus.merge(later.context_focus);
        self.merge_window.start = self.merge_window.start.min(later.merge_window.start);
        self.merge_window.end = self.merge_window.end.max(later.merge_window.end);
    }
}

/// Physical signal lines retained before context halos or breadcrumbs expand selection.
#[derive(Clone, Default)]
struct SignalFocus {
    before: Vec<usize>,
    after: Vec<usize>,
}

impl SignalFocus {
    fn after_line(line: usize) -> Self {
        Self {
            before: Vec::new(),
            after: vec![line],
        }
    }

    fn from_rows(rows: &[DiffRow]) -> Self {
        let mut focus = Self::default();
        for row in rows {
            match row {
                DiffRow::Line(line) if line.has_changes() => focus.after.push(line.number),
                DiffRow::Reflow(line) => focus.after.push(line.number),
                DiffRow::LineChange { before, after } => {
                    focus.before.extend(before.as_ref().map(|line| line.number));
                    focus.after.extend(after.as_ref().map(|line| line.number));
                }
                DiffRow::Moved { before, after } => {
                    focus.before.extend(*before);
                    focus.after.push(after.number);
                }
                DiffRow::Wordwise(word) => {
                    focus.before.extend(word.before_line);
                    focus.after.extend(word.after_line);
                }
                DiffRow::Line(_)
                | DiffRow::LineEnding { .. }
                | DiffRow::Elision(_)
                | DiffRow::FileBoundary => {}
            }
        }
        focus.normalize();
        focus
    }

    fn merge(&mut self, mut other: Self) {
        self.before.append(&mut other.before);
        self.after.append(&mut other.after);
    }

    fn normalize(&mut self) {
        self.before.sort_unstable();
        self.before.dedup();
        self.after.sort_unstable();
        self.after.dedup();
    }

    fn is_empty(&self) -> bool {
        self.before.is_empty() && self.after.is_empty()
    }
}

/// Monotone edit-script coordinates shared by before-only and current-world rows.
struct LineAlignment<'graph> {
    graph: &'graph Correspondence,
    after_lines: usize,
}

/// Unit-script ownership retained after nearby fragments meld into one visual hunk.
struct ReviewSequence {
    before_owner: Vec<Option<usize>>,
    after_owner: Vec<Option<usize>>,
    after_units: Vec<(Range<usize>, usize)>,
}

impl ReviewSequence {
    fn new(
        pair: &ProjectionPair<'_, '_>,
        correspondence: &Correspondence,
        suppressed_units: &[bool],
    ) -> Self {
        let mut sequence = Self {
            before_owner: vec![None; pair.before.source.lines().len() + 1],
            after_owner: vec![None; pair.after.source.lines().len() + 1],
            after_units: Vec::new(),
        };
        for (order, edit) in correspondence.units.iter().enumerate() {
            if suppressed_units[order] {
                continue;
            }
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
            debug_assert!(owner.is_none() || *owner == Some(order));
            *owner = Some(order);
        }
    }

    fn claim_after(&mut self, lines: Range<usize>, order: usize) {
        for line in lines.clone() {
            let Some(owner) = self.after_owner.get_mut(line) else {
                continue;
            };
            debug_assert!(owner.is_none() || *owner == Some(order));
            *owner = Some(order);
        }
        if !lines.is_empty() {
            self.after_units.push((lines, order));
        }
    }

    fn group_rank(&self, group: &[DiffRow], alignment: &LineAlignment<'_>) -> usize {
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
                    .filter_map(|line| alignment.current_anchor(line))
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
struct SourceOrder {
    after_gap: AfterGap,
    within_gap: GapOrder,
}

/// Deletions render inside a gap before the current line that begins there.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum GapOrder {
    Deletion(usize),
    Current,
}

impl SourceOrder {
    fn current(line: usize) -> Self {
        Self {
            after_gap: AfterGap::new(line.saturating_sub(1)),
            within_gap: GapOrder::Current,
        }
    }

    fn deletion(after_gap: AfterGap, before_line: usize) -> Self {
        Self {
            after_gap,
            within_gap: GapOrder::Deletion(before_line),
        }
    }

    fn after_gap(self) -> AfterGap {
        self.after_gap
    }

    fn within_gap(self) -> GapOrder {
        self.within_gap
    }
}

impl<'graph> LineAlignment<'graph> {
    fn new(graph: &'graph Correspondence, after_lines: usize) -> Self {
        Self { graph, after_lines }
    }

    /// Unmatched old lines occupy an explicit gap; exact lines share current order.
    fn before_order(&self, line: usize) -> SourceOrder {
        let index = line.saturating_sub(1);
        if let Some(link) = self.line_pair_at_before(index) {
            return SourceOrder::current(link.after + 1);
        }

        let preceding_lines = self
            .preceding_line_pair(index)
            .map_or(0, |link| link.after + 1);
        let after_gap = if preceding_lines == 0 {
            AfterGap::BEFORE_FIRST
        } else {
            AfterGap::new(preceding_lines)
        };
        SourceOrder::deletion(after_gap, line)
    }

    fn current_anchor(&self, before_line: usize) -> Option<usize> {
        if self.after_lines == 0 {
            return None;
        }
        let gap = self.before_order(before_line).after_gap();
        let line = gap.preceding_lines().saturating_add(1);
        Some(line.clamp(1, self.after_lines))
    }

    fn aligned_before_line(&self, after_line: usize) -> Option<usize> {
        let index = after_line.checked_sub(1)?;
        self.line_pair_at_after(index).map(|link| link.before + 1)
    }

    /// Exact rows and explicit terminator edits share physical source order.
    fn line_pair_at_before(&self, before: usize) -> Option<LineLink> {
        [
            line_link_at_before(&self.graph.line_links, before),
            line_link_at_before(&self.graph.line_ending_edits, before),
        ]
        .into_iter()
        .flatten()
        .min_by_key(|link| link.after)
    }

    fn line_pair_at_after(&self, after: usize) -> Option<LineLink> {
        [
            line_link_at_after(&self.graph.line_links, after),
            line_link_at_after(&self.graph.line_ending_edits, after),
        ]
        .into_iter()
        .flatten()
        .min_by_key(|link| link.before)
    }

    fn preceding_line_pair(&self, before: usize) -> Option<LineLink> {
        [
            preceding_line_link(&self.graph.line_links, before),
            preceding_line_link(&self.graph.line_ending_edits, before),
        ]
        .into_iter()
        .flatten()
        .max_by_key(|link| (link.before, link.after))
    }

    fn focus_order(&self, focus: &SignalFocus) -> SourceOrder {
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

    fn current_focus(&self, focus: &SignalFocus) -> Option<Range<usize>> {
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
        let line = order
            .after_gap()
            .preceding_lines()
            .saturating_add(1)
            .clamp(1, self.after_lines);
        Some(line..line + 1)
    }
}

fn line_link_at_before(links: &[LineLink], before: usize) -> Option<LineLink> {
    let index = links.partition_point(|link| link.before < before);
    links
        .get(index)
        .copied()
        .filter(|link| link.before == before)
}

fn line_link_at_after(links: &[LineLink], after: usize) -> Option<LineLink> {
    let index = links.partition_point(|link| link.after < after);
    links.get(index).copied().filter(|link| link.after == after)
}

fn preceding_line_link(links: &[LineLink], before: usize) -> Option<LineLink> {
    let index = links.partition_point(|link| link.before < before);
    index
        .checked_sub(1)
        .and_then(|index| links.get(index))
        .copied()
}

/// Turn one neutral edit graph into the complete ordered review stream.
pub(crate) fn plan_hunks(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
) -> Vec<Hunk> {
    if pair.before.source.as_str() == pair.after.source.as_str() {
        return Vec::new();
    }

    let only_linewise = pair
        .before
        .review_units()
        .chain(pair.after.review_units())
        .all(|(_, node)| {
            node.review
                .as_ref()
                .is_some_and(|review| review.mode == ReviewMode::Linewise)
        });
    let anchor_facts = AnchorFacts::new(pair);
    let fallback_index = FallbackIndex::new(&correspondence.line_fallbacks);
    let suppressed_units = correspondence
        .units
        .iter()
        .map(|edit| fallback_index.suppresses(pair, edit))
        .collect::<Vec<_>>();
    let changes = if only_linewise {
        edit_fragments(plan_whole_file_lines(pair, correspondence, &anchor_facts))
    } else {
        plan_units(pair, correspondence, &anchor_facts, &suppressed_units)
    };

    let alignment = LineAlignment::new(correspondence, pair.after.source.lines().len());
    let review_sequence =
        (!only_linewise).then(|| ReviewSequence::new(pair, correspondence, &suppressed_units));
    let mut changes = changes
        .into_iter()
        .map(|change| change.place(&alignment))
        .collect::<Vec<_>>();
    // Geometry is source-ordered so only neighboring context halos can merge.
    changes.sort_by_key(coalescing_order);
    let mut changes = coalesce_changes(changes);
    // Structural-edit buoyancy applies only after nearby auxiliary facts are attached.
    changes.sort_by_key(presentation_order);
    let mut hunks = Vec::with_capacity(changes.len());
    for mut change in changes {
        if change.needs_context() {
            complete_context_halos(pair, &alignment, &mut change);
            complete_display_gaps(pair, &alignment, &mut change);
        }
        for group in &mut change.groups {
            *group = order_replacement_group(std::mem::take(group));
        }
        if let Some(review_sequence) = &review_sequence {
            change.groups.sort_by_key(|group| {
                (
                    review_sequence.group_rank(group, &alignment),
                    group_source_order(group, &alignment),
                )
            });
        } else {
            change
                .groups
                .sort_by_key(|group| group_source_order(group, &alignment));
        }
        hunks.push(Hunk {
            coverage: change.coverage,
            rows: change.groups.into_iter().flatten().collect(),
        });
    }
    deduplicate_context_rows(&mut hunks);
    append_file_boundary(
        &mut hunks,
        pair.before.source.lines().len(),
        pair.after.source.lines().len(),
    );
    hunks
}

/// One hunk contains each physical current-world context row at most once.
fn deduplicate_context_rows(hunks: &mut Vec<Hunk>) {
    for hunk in hunks.iter_mut() {
        let signal_lines = hunk
            .rows
            .iter()
            .filter_map(|row| match row {
                DiffRow::Line(line) if line.has_changes() => Some(line.number),
                DiffRow::Reflow(line) => Some(line.number),
                DiffRow::LineChange { after, .. } => after.as_ref().map(|line| line.number),
                DiffRow::Moved { after, .. } => Some(after.number),
                DiffRow::Wordwise(word) => word.after_line,
                _ => None,
            })
            .collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        hunk.rows.retain(|row| {
            let DiffRow::Line(line) = row else {
                return true;
            };
            if line.has_changes() {
                return true;
            }
            !signal_lines.contains(&line.number) && seen.insert(line.number)
        });
    }
    hunks.retain(|hunk| !hunk.rows.is_empty());
}

/// Complete each signal's three-line context halo against the whole current file.
fn complete_context_halos(
    pair: &ProjectionPair<'_, '_>,
    alignment: &LineAlignment<'_>,
    change: &mut AnchoredChange,
) {
    let line_count = pair.after.source.lines().len();
    if line_count == 0 {
        return;
    }

    let mut signals = change.context_focus.after.clone();
    signals.extend(
        change
            .context_focus
            .before
            .iter()
            .filter_map(|line| alignment.current_anchor(*line)),
    );
    signals.sort_unstable();
    signals.dedup();

    let mut shown = change
        .groups
        .iter()
        .flatten()
        .filter_map(row_displayed_after_source_line)
        .collect::<HashSet<_>>();
    for signal in signals {
        let start = signal.saturating_sub(CONTEXT_HALO_RADIUS).max(1);
        let end = signal.saturating_add(CONTEXT_HALO_RADIUS).min(line_count);
        for number in start..=end {
            if !shown.insert(number) {
                continue;
            }
            let line = pair
                .after
                .source
                .line(number)
                .expect("selected current context line exists");
            change.groups.push(vec![DiffRow::Line(build_display_line(
                &pair.after,
                line,
                &[],
                DiffMark::Context,
            ))]);
            include_line(&mut change.coverage.after, number);
        }
    }
}

/// Hierarchy pins added outside a producer's range still explain the omitted interval.
fn complete_display_gaps(
    pair: &ProjectionPair<'_, '_>,
    alignment: &LineAlignment<'_>,
    change: &mut AnchoredChange,
) {
    let mut displayed = change
        .groups
        .iter()
        .flatten()
        .filter_map(row_displayed_after_source_line)
        .collect::<Vec<_>>();
    displayed.sort_unstable();
    displayed.dedup();
    let before_displayed = displayed
        .iter()
        .filter_map(|line| alignment.aligned_before_line(*line))
        .collect::<Vec<_>>();
    trim_elisions_behind_context(&mut change.groups, &before_displayed, &displayed);
    let elisions = change
        .groups
        .iter()
        .flatten()
        .filter_map(|row| match row {
            DiffRow::Elision(coverage) => coverage.after.clone(),
            _ => None,
        })
        .collect::<Vec<_>>();

    for lines in displayed.windows(2) {
        let omitted = lines[0].saturating_add(1)..lines[1];
        if omitted.is_empty()
            || elisions
                .iter()
                .any(|coverage| ranges_overlap(coverage, &omitted))
        {
            continue;
        }
        if omitted.len() == 1 {
            let number = omitted.start;
            let source = pair
                .after
                .source
                .line(number)
                .expect("one omitted current context line exists");
            change.groups.push(vec![DiffRow::Line(build_display_line(
                &pair.after,
                source,
                &[],
                DiffMark::Context,
            ))]);
            include_line(&mut change.coverage.after, number);
            continue;
        }
        change.groups.push(vec![DiffRow::Elision(LineCoverage {
            before: None,
            after: Some(omitted),
        })]);
    }
}

/// Displayed context replaces, rather than overlaps, any earlier folded coverage.
fn trim_elisions_behind_context(
    groups: &mut Vec<Vec<DiffRow>>,
    before_displayed: &[usize],
    after_displayed: &[usize],
) {
    let mut trimmed = Vec::with_capacity(groups.len());
    for group in std::mem::take(groups) {
        let [DiffRow::Elision(coverage)] = group.as_slice() else {
            trimmed.push(group);
            continue;
        };
        let mut before_excluded = before_displayed.to_vec();
        if let (Some(before), Some(after)) = (&coverage.before, &coverage.after)
            && before.len() == after.len()
        {
            before_excluded.extend(
                after_displayed
                    .iter()
                    .filter(|line| after.start <= **line && **line < after.end)
                    .map(|line| before.start + (*line - after.start)),
            );
            before_excluded.sort_unstable();
            before_excluded.dedup();
        }
        let before = coverage
            .before
            .clone()
            .map(|range| range_excluding_lines(range, &before_excluded))
            .unwrap_or_default();
        let after = coverage
            .after
            .clone()
            .map(|range| range_excluding_lines(range, after_displayed))
            .unwrap_or_default();
        for index in 0..before.len().max(after.len()) {
            trimmed.push(vec![DiffRow::Elision(LineCoverage {
                before: before.get(index).cloned(),
                after: after.get(index).cloned(),
            })]);
        }
    }
    *groups = trimmed;
}

fn range_excluding_lines(range: Range<usize>, excluded: &[usize]) -> Vec<Range<usize>> {
    let mut segments = Vec::new();
    let mut start = range.start;
    for line in excluded
        .iter()
        .copied()
        .filter(|line| range.start <= *line && *line < range.end)
    {
        if start < line {
            segments.push(start..line);
        }
        start = line.saturating_add(1);
    }
    if start < range.end {
        segments.push(start..range.end);
    }
    segments
}

fn edit_fragments(excerpts: Vec<ReviewExcerpt>) -> Vec<ChangeFragment> {
    excerpts.into_iter().map(ChangeFragment::edit).collect()
}

/// Current-world gap first, then deterministic ordering among changes in that gap.
fn coalescing_order(change: &AnchoredChange) -> (AfterGap, GapOrder, Reverse<Buoyancy>) {
    (
        change.after_gap(),
        change.order.within_gap(),
        Reverse(change.buoyancy),
    )
}

/// Structural edits first, then moves, compact source wiring, and pure reflow.
fn presentation_order(change: &AnchoredChange) -> (Reverse<Buoyancy>, AfterGap, GapOrder) {
    (
        Reverse(change.buoyancy),
        change.after_gap(),
        change.order.within_gap(),
    )
}

/// Context halos that touch (or would omit only one row) form one visual hunk.
fn coalesce_changes(changes: Vec<AnchoredChange>) -> Vec<AnchoredChange> {
    let mut coalesced: Vec<AnchoredChange> = Vec::new();

    for change in changes {
        let Some(previous) = coalesced.last_mut() else {
            coalesced.push(change);
            continue;
        };
        if !merge_windows_touch(&previous.merge_window, &change.merge_window) {
            coalesced.push(change);
            continue;
        }

        previous.meld(change);
    }

    coalesced
}

/// Expand only signals that actually receive ordinary context.
fn merge_window(focus: Range<usize>, signal: Signal) -> Range<usize> {
    let radius = if signal.receives_context() {
        CONTEXT_HALO_RADIUS
    } else {
        0
    };
    focus.start.saturating_sub(radius).max(1)..focus.end.saturating_add(radius)
}

fn line_hull(lines: &[usize]) -> Option<Range<usize>> {
    Some(*lines.first()?..lines.last()?.saturating_add(1))
}

fn include_line(coverage: &mut Option<Range<usize>>, line: usize) {
    include_optional_coverage(coverage, Some(line..line + 1));
}

fn include_optional_coverage(coverage: &mut Option<Range<usize>>, addition: Option<Range<usize>>) {
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

fn plan_units(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    anchor_facts: &AnchorFacts,
    suppressed_units: &[bool],
) -> Vec<ChangeFragment> {
    let mut fragments = Vec::new();
    let mut preceding_after_lines = 0;

    for (index, edit) in correspondence.units.iter().enumerate() {
        if suppressed_units[index] {
            continue;
        }
        match edit {
            UnitEdit::Matched(unit) => {
                plan_matched_unit(pair, correspondence, anchor_facts, unit, &mut fragments);
                let after = pair.after.node(unit.after);
                preceding_after_lines =
                    preceding_after_lines.max(after.lines.end.saturating_sub(1));
            }
            UnitEdit::Removed { before } => {
                let node = pair.before.node(*before);
                let order =
                    SourceOrder::deletion(AfterGap::new(preceding_after_lines), node.lines.start);
                let review = node
                    .review
                    .as_ref()
                    .expect("review edit owns a review unit");
                match review.mode {
                    ReviewMode::Compact => fragments.push(
                        ChangeFragment::wiring(plan_one_sided_lines(
                            &pair.before,
                            node.lines.clone(),
                            DiffMark::Removed,
                        ))
                        .at_order(order),
                    ),
                    ReviewMode::Linewise | ReviewMode::Structural => {
                        fragments.push(
                            ChangeFragment::edit(plan_one_sided_lines(
                                &pair.before,
                                node.lines.clone(),
                                DiffMark::Removed,
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
                match review.mode {
                    ReviewMode::Compact => fragments.push(ChangeFragment::wiring(
                        plan_one_sided_lines(&pair.after, node.lines.clone(), DiffMark::Added),
                    )),
                    ReviewMode::Linewise | ReviewMode::Structural => {
                        fragments.extend(edit_fragments(vec![plan_one_sided_lines(
                            &pair.after,
                            node.lines.clone(),
                            DiffMark::Added,
                        )]))
                    }
                }
                preceding_after_lines = preceding_after_lines.max(node.lines.end.saturating_sub(1));
            }
        }
    }
    for fallback in &correspondence.line_fallbacks {
        let excerpts = plan_line_region(
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

/// Disjoint local-fallback intervals shared by planning and final source ordering.
struct FallbackIndex {
    before: Vec<Range<usize>>,
    after: Vec<Range<usize>>,
}

impl FallbackIndex {
    fn new(fallbacks: &[super::correspondence::LineFallback]) -> Self {
        let mut before = fallbacks
            .iter()
            .filter(|fallback| !fallback.before.is_empty())
            .map(|fallback| fallback.before.clone())
            .collect::<Vec<_>>();
        let mut after = fallbacks
            .iter()
            .filter(|fallback| !fallback.after.is_empty())
            .map(|fallback| fallback.after.clone())
            .collect::<Vec<_>>();
        before.sort_unstable_by_key(|range| (range.start, range.end));
        after.sort_unstable_by_key(|range| (range.start, range.end));
        debug_assert!(before.windows(2).all(|pair| pair[0].end <= pair[1].start));
        debug_assert!(after.windows(2).all(|pair| pair[0].end <= pair[1].start));
        Self { before, after }
    }

    fn suppresses(&self, pair: &ProjectionPair<'_, '_>, edit: &UnitEdit) -> bool {
        let (before, after) = unit_line_indices(pair, edit);
        indexed_ranges_overlap(&self.before, &before) || indexed_ranges_overlap(&self.after, &after)
    }
}

fn indexed_ranges_overlap(ranges: &[Range<usize>], query: &Range<usize>) -> bool {
    if query.is_empty() {
        return false;
    }
    let candidate = ranges.partition_point(|range| range.end <= query.start);
    ranges
        .get(candidate)
        .is_some_and(|range| range.start < query.end)
}

fn unit_line_indices(
    pair: &ProjectionPair<'_, '_>,
    edit: &UnitEdit,
) -> (Range<usize>, Range<usize>) {
    match edit {
        UnitEdit::Matched(unit) => (
            node_line_indices(&pair.before, unit.before),
            node_line_indices(&pair.after, unit.after),
        ),
        UnitEdit::Removed { before } => (node_line_indices(&pair.before, *before), 0..0),
        UnitEdit::Added { after } => (0..0, node_line_indices(&pair.after, *after)),
    }
}

fn node_line_indices(projection: &Projection<'_>, node: NodeId) -> Range<usize> {
    line_indices(
        Some(projection.node(node).lines.clone()),
        projection.source.lines().len(),
    )
}

fn one_based_line_range(lines: &Range<usize>) -> Option<Range<usize>> {
    (!lines.is_empty()).then(|| lines.start + 1..lines.end + 1)
}

fn plan_matched_unit(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    anchor_facts: &AnchorFacts,
    unit: &MatchedUnit,
    fragments: &mut Vec<ChangeFragment>,
) {
    let after_node = pair.after.node(unit.after);
    if unit.placement == Placement::Reordered && unit.relation.full_equal() {
        fragments.push(ChangeFragment::moved(plan_move(pair, unit)));
        return;
    }
    if unit.relation.source_equal() {
        return;
    }

    match unit.mode {
        ReviewMode::Compact => {
            let before = pair.before.node(unit.before);
            if !node_is_single_line(before) || !node_is_single_line(after_node) {
                let composites = correspondence.unit_composites(unit);
                let excerpts = plan_line_region(
                    pair,
                    correspondence,
                    Some(before.lines.clone()),
                    Some(after_node.lines.clone()),
                    composites,
                    Some(unit.after),
                    LineAnchors::new(structural_anchor_basis(pair, unit), anchor_facts),
                );
                fragments.extend(excerpts.into_iter().map(ChangeFragment::wiring));
                return;
            }
            let excerpt = plan_wiring(pair, Some(unit.before), Some(unit.after));
            fragments.push(ChangeFragment::wiring(excerpt));
        }
        ReviewMode::Linewise => {
            let composites = correspondence.unit_composites(unit);
            fragments.extend(edit_fragments(plan_line_region(
                pair,
                correspondence,
                Some(pair.before.node(unit.before).lines.clone()),
                Some(after_node.lines.clone()),
                composites,
                Some(unit.after),
                LineAnchors::new(AnchorBasis::Physical, anchor_facts),
            )));
        }
        ReviewMode::Structural => {
            if unit.relation.full_equal() {
                fragments.extend(
                    plan_reflow(pair, correspondence, unit)
                        .into_iter()
                        .map(ChangeFragment::reflow),
                );
                return;
            }

            let dependents = presentation_dependents(pair, correspondence, unit);
            let comments = comment_edits(pair, correspondence, unit, &dependents);
            if unit.relation.payload_equal() {
                fragments.extend(
                    plan_reflow_with_comments(pair, correspondence, unit, comments)
                        .into_iter()
                        .map(ChangeFragment::edit),
                );
                return;
            }
            let needs_physical_plan = pair.before.identity_text(unit.before)
                != pair.after.identity_text(unit.after)
                || has_retainable_reparented_region(pair, correspondence, anchor_facts, unit)
                || has_unmatched_before_content(pair, correspondence, unit);
            if needs_physical_plan {
                let excerpts = plan_line_region(
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
                plan_payload(
                    pair,
                    correspondence,
                    anchor_facts,
                    unit,
                    &dependents,
                    comments,
                )
                .into_iter()
                .map(ChangeFragment::edit),
            );
        }
    }
}

fn has_retainable_reparented_region(
    pair: &ProjectionPair<'_, '_>,
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
        .filter(|link| link.reparented)
        .any(|link| retained_region(pair, anchor_facts, link, &before, &after).is_some())
}

fn has_unmatched_before_content(
    pair: &ProjectionPair<'_, '_>,
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

fn node_is_single_line(node: &super::projection::SyntaxNode) -> bool {
    node.lines.end.saturating_sub(node.lines.start) <= 1
}

fn plan_one_sided_lines(
    projection: &Projection<'_>,
    lines: Range<usize>,
    mark: DiffMark,
) -> ReviewExcerpt {
    let coverage = if mark == DiffMark::Removed {
        LineCoverage {
            before: Some(lines.clone()),
            after: None,
        }
    } else {
        LineCoverage {
            before: None,
            after: Some(lines.clone()),
        }
    };
    let mut rows = Vec::new();
    for source in collect_source_lines(projection, lines) {
        let ending = source.ending;
        let line = build_display_line(
            projection,
            source,
            &[MarkedRange::new(source.content_bytes.clone(), mark)],
            DiffMark::Context,
        );
        let (before, after) = if mark == DiffMark::Removed {
            (Some(line), None)
        } else {
            (None, Some(line))
        };
        rows.push(DiffRow::LineChange { before, after });
        if ending == LineEnding::Missing {
            rows.push(DiffRow::LineEnding {
                before: (mark == DiffMark::Removed).then_some(ending),
                after: (mark == DiffMark::Added).then_some(ending),
            });
        }
    }
    ReviewExcerpt { coverage, rows }
}

fn plan_wiring(
    pair: &ProjectionPair<'_, '_>,
    before: Option<NodeId>,
    after: Option<NodeId>,
) -> ReviewExcerpt {
    let before_node = before.map(|node| pair.before.node(node));
    let after_node = after.map(|node| pair.after.node(node));
    let before_text = before_node
        .and_then(|node| pair.before.source.slice(node.bytes.clone()))
        .unwrap_or("");
    let after_text = after_node
        .and_then(|node| pair.after.source.slice(node.bytes.clone()))
        .unwrap_or("");
    let word = word_diff(
        before_node.map(|node| node.lines.start),
        after_node.map(|node| node.lines.start),
        before_text,
        after_text,
    );

    ReviewExcerpt {
        coverage: LineCoverage {
            before: before_node.map(|node| node.lines.clone()),
            after: after_node.map(|node| node.lines.clone()),
        },
        rows: vec![DiffRow::Wordwise(word)],
    }
}

fn plan_move(pair: &ProjectionPair<'_, '_>, unit: &MatchedUnit) -> ReviewExcerpt {
    let before = pair.before.node(unit.before);
    let after = pair.after.node(unit.after);
    let coverage = LineCoverage {
        before: Some(before.lines.clone()),
        after: Some(after.lines.clone()),
    };
    let mut lines = build_display_lines(&pair.after, after.lines.clone(), &[], DiffMark::Context);
    let Some(first) = lines.first().cloned() else {
        return ReviewExcerpt {
            coverage,
            rows: Vec::new(),
        };
    };
    if let Some(rows) =
        moved_rows_with_line_endings(pair, before.lines.clone(), after.lines.clone(), &lines)
    {
        return ReviewExcerpt { coverage, rows };
    }
    if lines.len() == 1 {
        return ReviewExcerpt {
            coverage,
            rows: vec![DiffRow::Moved {
                before: Some(before.lines.start),
                after: first,
            }],
        };
    }

    let last = lines.pop().expect("a multi-line move has a final line");
    lines.remove(0);
    let mut rows = vec![DiffRow::Moved {
        before: Some(before.lines.start),
        after: first,
    }];
    if lines.len() == 1 {
        rows.push(DiffRow::Line(lines.pop().expect("one middle line remains")));
    } else if !lines.is_empty() {
        rows.push(DiffRow::Elision(LineCoverage {
            before: Some(before.lines.start + 1..before.lines.end.saturating_sub(1)),
            after: Some(after.lines.start + 1..after.lines.end.saturating_sub(1)),
        }));
    }
    rows.push(DiffRow::Moved {
        before: None,
        after: last,
    });
    ReviewExcerpt { coverage, rows }
}

/// A move remains the primary fact, but concrete terminator edits inside it cannot disappear.
fn moved_rows_with_line_endings(
    pair: &ProjectionPair<'_, '_>,
    before: Range<usize>,
    after: Range<usize>,
    lines: &[DisplayLine],
) -> Option<Vec<DiffRow>> {
    let before_indices = line_indices(Some(before.clone()), pair.before.source.lines().len());
    let after_indices = line_indices(Some(after.clone()), pair.after.source.lines().len());
    if after_indices.len() != lines.len() {
        return None;
    }
    let before_text = before_indices
        .clone()
        .map(|index| pair.before.source.text(&pair.before.source.lines()[index]))
        .collect::<Vec<_>>();
    let after_text = after_indices
        .clone()
        .map(|index| pair.after.source.text(&pair.after.source.lines()[index]))
        .collect::<Vec<_>>();
    let matches = ordered_matches(&before_text, &after_text);
    let mut matched_before = vec![false; before_indices.len()];
    let mut matched_after = vec![false; after_indices.len()];
    let mut endings = vec![None; after_indices.len()];
    for link in matches {
        matched_before[link.before] = true;
        matched_after[link.after] = true;
        let before_ending = pair.before.source.lines()[before_indices.start + link.before].ending;
        let after_ending = pair.after.source.lines()[after_indices.start + link.after].ending;
        if before_ending != after_ending {
            endings[link.after] = Some((Some(before_ending), Some(after_ending)));
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
        before_only.extend(before_offsets.into_iter().skip(cancelled).map(|_| ending));
        for offset in after_offsets.into_iter().skip(cancelled) {
            endings[offset] = Some((None, Some(ending)));
        }
    }
    if before_only.is_empty() && endings.iter().all(Option::is_none) {
        return None;
    }

    let mut rows = vec![DiffRow::Moved {
        before: Some(before.start),
        after: lines[0].clone(),
    }];
    // A one-sided old terminator has no current line coordinate; the move header
    // is the explicit owner of that old unit.
    for ending in before_only {
        rows.push(DiffRow::LineEnding {
            before: Some(ending),
            after: None,
        });
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
            rows.push(DiffRow::Line(lines[offset].clone()));
            append_moved_line_ending(&mut rows, endings[offset]);
            offset += 1;
            continue;
        }

        let start = offset;
        while offset < last && show_only_middle != Some(offset) && endings[offset].is_none() {
            offset += 1;
        }
        rows.push(DiffRow::Elision(LineCoverage {
            before: (before.len() == after.len())
                .then(|| before.start + start..before.start + offset),
            after: Some(after.start + start..after.start + offset),
        }));
    }
    rows.push(DiffRow::Moved {
        before: None,
        after: lines[last].clone(),
    });
    append_moved_line_ending(&mut rows, endings[last]);
    Some(rows)
}

fn append_moved_line_ending(
    rows: &mut Vec<DiffRow>,
    ending: Option<(Option<LineEnding>, Option<LineEnding>)>,
) {
    let Some((before, after)) = ending else {
        return;
    };
    rows.push(DiffRow::LineEnding { before, after });
}

fn plan_reflow(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
) -> Vec<ReviewExcerpt> {
    let before = pair.before.node(unit.before);
    let after = pair.after.node(unit.after);
    let before_lines = line_indices(Some(before.lines.clone()), pair.before.source.lines().len());
    let after_lines = line_indices(Some(after.lines.clone()), pair.after.source.lines().len());
    let exact_after = correspondence
        .line_links_in(before_lines, after_lines)
        .map(|link| link.after + 1)
        .collect::<HashSet<_>>();
    let rows = build_display_lines(&pair.after, after.lines.clone(), &[], DiffMark::Context)
        .into_iter()
        .map(|line| {
            if exact_after.contains(&line.number) {
                DiffRow::Line(line)
            } else {
                DiffRow::Reflow(line)
            }
        })
        .collect::<Vec<_>>();
    select_review_excerpts(pair, correspondence, unit, rows)
}

fn plan_reflow_with_comments(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
    mut edits: Vec<LineEdit>,
) -> Vec<ReviewExcerpt> {
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
    for line in build_display_lines(&pair.after, after.lines.clone(), &[], DiffMark::Context) {
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
            DiffRow::Line(line)
        } else {
            DiffRow::Reflow(line)
        };
        rows.push(row);
    }
    for edit in edits {
        append_line_edit_rows(&mut rows, edit);
    }

    select_review_excerpts(pair, correspondence, unit, rows)
}

fn comment_lines(projection: &Projection<'_>, unit: NodeId) -> HashSet<usize> {
    descendant_leaves(projection, unit)
        .filter_map(|leaf| {
            let node = projection.node(leaf);
            (node.leaf?.channel == ContentChannel::Comment).then_some(node.lines.clone())
        })
        .flatten()
        .collect()
}

fn plan_payload(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    anchor_facts: &AnchorFacts,
    unit: &MatchedUnit,
    dependents: &PresentationDependents,
    mut edits: Vec<LineEdit>,
) -> Vec<ReviewExcerpt> {
    let before = pair.before.node(unit.before);
    let after = pair.after.node(unit.after);
    edits.extend(modified_payload_edits(pair, correspondence, unit));
    let mut changed_before = correspondence
        .unit_leaf_links(unit)
        .iter()
        .filter(|link| {
            link.relation != LeafRelation::Modified
                && (link.placement == Placement::Reordered || link.reparented)
        })
        .map(|link| link.before)
        .collect::<HashSet<_>>();
    let mut changed_after = correspondence
        .unit_leaf_links(unit)
        .iter()
        .filter(|link| {
            link.relation != LeafRelation::Modified
                && (link.placement == Placement::Reordered || link.reparented)
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
    let before_marked = marked_leaf_ranges(&pair.before, changed_before, DiffMark::Removed);
    let after_marked = marked_leaf_ranges(&pair.after, changed_after, DiffMark::Added);
    edits.extend(fully_marked_line_edits(
        pair,
        before.lines.clone(),
        after.lines.clone(),
        &before_marked,
        &after_marked,
    ));
    deduplicate_line_edits(&mut edits);
    if line_edits_compete_for_source_rows(&edits) {
        return plan_line_region(
            pair,
            correspondence,
            Some(before.lines.clone()),
            Some(after.lines.clone()),
            correspondence.unit_composites(unit),
            Some(unit.after),
            LineAnchors::new(structural_anchor_basis(pair, unit), anchor_facts),
        );
    }
    let before_region = line_indices(Some(before.lines.clone()), pair.before.source.lines().len());
    let after_region = line_indices(Some(after.lines.clone()), pair.after.source.lines().len());
    let retained = retained_regions(
        pair,
        anchor_facts,
        correspondence.unit_composites(unit),
        &before_region,
        &after_region,
    );
    expand_line_edits_through_weak_rows(pair, correspondence, &retained, &mut edits);
    let changed_lines = edits
        .iter()
        .filter_map(|edit| edit.after.as_ref().map(|line| line.number))
        .collect::<HashSet<_>>();
    let lines = build_display_lines(
        &pair.after,
        after.lines.clone(),
        &after_marked,
        DiffMark::Context,
    );
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
        rows.push(DiffRow::Line(line));
    }
    for edit in edits {
        append_line_edit_rows(&mut rows, edit);
    }

    select_review_excerpts(pair, correspondence, unit, rows)
}

/// One physical replacement row retained with both source revisions.
struct LineEdit {
    before: Option<DisplayLine>,
    after: Option<DisplayLine>,
    before_ending: Option<LineEnding>,
    after_ending: Option<LineEnding>,
}

fn comment_edits(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
    dependents: &PresentationDependents,
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
                && !link.reparented
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
struct PresentationDependents {
    before: HashSet<NodeId>,
    after: HashSet<NodeId>,
}

fn presentation_dependents(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
) -> PresentationDependents {
    let mut dependents = PresentationDependents::default();
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
    pair: &ProjectionPair<'_, '_>,
    before: NodeId,
    after: NodeId,
) -> bool {
    if !node_is_presentational_line_isolated(&pair.before, before)
        || !node_is_presentational_line_isolated(&pair.after, after)
    {
        return false;
    }
    let before = node_line_indices(&pair.before, before);
    let after = node_line_indices(&pair.after, after);
    physical_lines_equal(pair, &before, &after)
}

/// A decoration may own its newline, but never neighboring source content.
fn node_is_presentational_line_isolated(projection: &Projection<'_>, node: NodeId) -> bool {
    let node = projection.node(node);
    let Some(first) = projection.source.line(node.lines.start) else {
        return false;
    };
    let Some(last_number) = node.lines.end.checked_sub(1) else {
        return false;
    };
    let Some(last) = projection.source.line(last_number) else {
        return false;
    };
    if node.bytes.start < first.content_bytes.start || node.bytes.end > last.full_bytes.end {
        return false;
    }
    if node.bytes.end > last.content_bytes.end && node.bytes.end != last.full_bytes.end {
        return false;
    }

    let prefix = projection
        .source
        .slice(first.content_bytes.start..node.bytes.start);
    let suffix_start = node.bytes.end.min(last.content_bytes.end);
    let suffix = projection
        .source
        .slice(suffix_start..last.content_bytes.end);
    prefix.is_some_and(horizontal_layout) && suffix.is_some_and(horizontal_layout)
}

fn modified_payload_edits(
    pair: &ProjectionPair<'_, '_>,
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

fn marked_leaf_ranges(
    projection: &Projection<'_>,
    leaves: HashSet<NodeId>,
    mark: DiffMark,
) -> Vec<MarkedRange> {
    leaves
        .into_iter()
        .filter_map(|leaf| {
            let node = projection.node(leaf);
            (!matches!(
                node.leaf?.channel,
                ContentChannel::Comment | ContentChannel::Layout
            ))
            .then_some(MarkedRange::new(node.bytes.clone(), mark))
        })
        .collect()
}

fn fully_marked_line_edits(
    pair: &ProjectionPair<'_, '_>,
    before_lines: Range<usize>,
    after_lines: Range<usize>,
    before_marked: &[MarkedRange],
    after_marked: &[MarkedRange],
) -> Vec<LineEdit> {
    let mut edits = Vec::new();
    for line in collect_source_lines(&pair.before, before_lines) {
        let display = build_display_line(&pair.before, line, before_marked, DiffMark::Context);
        if line_is_fully_marked(&display, DiffMark::Removed) {
            edits.push(changed_line_edit(
                &pair.before,
                Some(line),
                &pair.after,
                None,
            ));
        }
    }
    for line in collect_source_lines(&pair.after, after_lines) {
        let display = build_display_line(&pair.after, line, after_marked, DiffMark::Context);
        if line_is_fully_marked(&display, DiffMark::Added) {
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

fn line_is_fully_marked(line: &DisplayLine, mark: DiffMark) -> bool {
    let significant = line
        .spans
        .iter()
        .filter(|span| !span.text.trim().is_empty())
        .collect::<Vec<_>>();
    !significant.is_empty() && significant.into_iter().all(|span| span.mark == mark)
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

/// Exact weak rows between nearby edits belong to their shared replacement.
fn expand_line_edits_through_weak_rows(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    retained: &[RetainedRegion],
    edits: &mut Vec<LineEdit>,
) {
    let mut context = Vec::new();
    for edits in edits.windows(2) {
        let [left, right] = edits else {
            unreachable!("a line-edit window has two entries")
        };
        let (Some(left_before), Some(left_after), Some(right_before), Some(right_after)) = (
            left.before.as_ref(),
            left.after.as_ref(),
            right.before.as_ref(),
            right.after.as_ref(),
        ) else {
            continue;
        };
        if left_before.number >= right_before.number || left_after.number >= right_after.number {
            continue;
        }

        let before = left_before.number..right_before.number.saturating_sub(1);
        let after = left_after.number..right_after.number.saturating_sub(1);
        if before.is_empty() || before.len() != after.len() || !context_gap_fits_halos(before.len())
        {
            continue;
        }
        if retained_regions_overlap(retained, &before, &after) {
            continue;
        }
        let links = correspondence
            .line_links_in(before.clone(), after.clone())
            .collect::<Vec<_>>();
        let exact = links.len() == before.len()
            && links.iter().enumerate().all(|(offset, link)| {
                link.before == before.start + offset && link.after == after.start + offset
            });
        if !exact {
            continue;
        }

        for (before, after) in before.zip(after) {
            context.push(changed_line_edit(
                &pair.before,
                pair.before.source.lines().get(before),
                &pair.after,
                pair.after.source.lines().get(after),
            ));
        }
    }
    edits.extend(context);
    deduplicate_line_edits(edits);
}

fn retained_regions_overlap(
    retained: &[RetainedRegion],
    before: &Range<usize>,
    after: &Range<usize>,
) -> bool {
    let before_index = retained.partition_point(|region| region.before.end <= before.start);
    let before_overlap = retained
        .get(before_index)
        .is_some_and(|region| ranges_overlap(&region.before, before));
    let after_index = retained.partition_point(|region| region.after.end <= after.start);
    let after_overlap = retained
        .get(after_index)
        .is_some_and(|region| ranges_overlap(&region.after, after));
    before_overlap || after_overlap
}

fn append_changed_line_edits(
    edits: &mut Vec<LineEdit>,
    before: &Projection<'_>,
    before_lines: Option<Range<usize>>,
    after: &Projection<'_>,
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

fn changed_line_edit(
    before: &Projection<'_>,
    before_line: Option<&SourceLine>,
    after: &Projection<'_>,
    after_line: Option<&SourceLine>,
) -> LineEdit {
    let (before_display, after_display) =
        changed_display_lines(before, before_line, after, after_line);
    LineEdit {
        before: before_display,
        after: after_display,
        before_ending: before_line.map(|line| line.ending),
        after_ending: after_line.map(|line| line.ending),
    }
}

fn line_edit_order(edit: &LineEdit) -> usize {
    edit.after
        .as_ref()
        .or(edit.before.as_ref())
        .map(|line| line.number)
        .expect("a line edit always has a source side")
}

fn append_line_edit_rows(rows: &mut Vec<DiffRow>, edit: LineEdit) {
    let ending_changed = edit.before_ending != edit.after_ending;
    let visible_missing = edit.before_ending == Some(LineEnding::Missing)
        || edit.after_ending == Some(LineEnding::Missing);
    rows.push(DiffRow::LineChange {
        before: edit.before,
        after: edit.after,
    });
    if ending_changed && (edit.before_ending.is_some() && edit.after_ending.is_some())
        || visible_missing && (edit.before_ending.is_none() || edit.after_ending.is_none())
    {
        rows.push(DiffRow::LineEnding {
            before: edit.before_ending,
            after: edit.after_ending,
        });
    }
}

fn descendant_leaves<'projection>(
    projection: &'projection Projection<'_>,
    root: NodeId,
) -> impl Iterator<Item = NodeId> + 'projection {
    std::iter::once(root)
        .chain(projection.descendants(root))
        .filter(|id| projection.node(*id).leaf.is_some())
}

fn plan_whole_file_lines(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    anchor_facts: &AnchorFacts,
) -> Vec<ReviewExcerpt> {
    let before =
        (!pair.before.source.lines().is_empty()).then(|| 1..pair.before.source.lines().len() + 1);
    let after =
        (!pair.after.source.lines().is_empty()).then(|| 1..pair.after.source.lines().len() + 1);
    plan_line_region(
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
        before: usize,
        after: usize,
        strong: bool,
    },
    Edit {
        before: Range<usize>,
        after: Range<usize>,
    },
    /// A physically paired row kept atomic so its concrete terminator stays visible.
    TerminatorEdit { before: usize, after: usize },
    Reflow {
        before: Range<usize>,
        after: Range<usize>,
        unchanged_after: HashSet<usize>,
    },
}

impl LineFact {
    fn is_signal(&self) -> bool {
        !matches!(self, Self::Context { .. })
    }
}

#[derive(Clone, Debug)]
struct RetainedRegion {
    before: Range<usize>,
    after: Range<usize>,
    retention: Retention,
}

/// Projection-wide subtree facts used to admit structural display anchors in constant time.
struct AnchorFacts {
    before: Vec<NodeAnchorFacts>,
    after: Vec<NodeAnchorFacts>,
}

impl AnchorFacts {
    fn new(pair: &ProjectionPair<'_, '_>) -> Self {
        Self {
            before: projection_anchor_facts(&pair.before),
            after: projection_anchor_facts(&pair.after),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct NodeAnchorFacts {
    has_opaque: bool,
    has_payload: bool,
}

fn projection_anchor_facts(projection: &Projection<'_>) -> Vec<NodeAnchorFacts> {
    let mut facts = vec![NodeAnchorFacts::default(); projection.nodes.len()];
    for index in (0..projection.nodes.len()).rev() {
        let node = &projection.nodes[index];
        let mut fact = node
            .leaf
            .map_or_else(NodeAnchorFacts::default, |leaf| NodeAnchorFacts {
                has_opaque: leaf.channel == ContentChannel::Opaque,
                has_payload: leaf.channel != ContentChannel::Layout && !leaf.delimiter,
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
    Exact {
        before: usize,
        after: usize,
        strong: bool,
    },
    TerminatorEdit {
        before: usize,
        after: usize,
    },
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
fn structural_anchor_basis(pair: &ProjectionPair<'_, '_>, unit: &MatchedUnit) -> AnchorBasis {
    let same_owner = match (
        pair.before.identity_text(unit.before),
        pair.after.identity_text(unit.after),
    ) {
        (Some(before), Some(after)) => before == after,
        (None, _) | (_, None) => false,
    };
    AnchorBasis::Structural { same_owner }
}

fn plan_line_region(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    before_lines: Option<Range<usize>>,
    after_lines: Option<Range<usize>>,
    composites: &[NodeLink],
    after_root: Option<NodeId>,
    anchors: LineAnchors<'_>,
) -> Vec<ReviewExcerpt> {
    let before = line_indices(before_lines, pair.before.source.lines().len());
    let after = line_indices(after_lines, pair.after.source.lines().len());
    let retained = retained_regions(pair, anchors.facts, composites, &before, &after);
    let facts = line_facts(pair, correspondence, before, after, retained, anchors.basis);
    plan_line_facts(pair, correspondence, &facts, after_root)
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
    pair: &ProjectionPair<'_, '_>,
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
fn structural_link_crossings(pair: &ProjectionPair<'_, '_>, composites: &[NodeLink]) -> Vec<bool> {
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
    pair: &ProjectionPair<'_, '_>,
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
    if !node_is_line_isolated(&pair.before, link.before)
        || !node_is_line_isolated(&pair.after, link.after)
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
    let retention = if physical_equal && !link.reparented {
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
fn link_belongs_to_decoration(pair: &ProjectionPair<'_, '_>, link: NodeLink) -> bool {
    node_belongs_to_decoration(&pair.before, link.before)
        || node_belongs_to_decoration(&pair.after, link.after)
}

fn node_belongs_to_decoration(projection: &Projection<'_>, mut id: NodeId) -> bool {
    loop {
        let node = projection.node(id);
        if node.decoration_owner.is_some() {
            return true;
        }
        let Some(parent) = node.parent else {
            return false;
        };
        id = parent;
    }
}

/// Retaining a subtree may claim whole rows only when adjacent bytes are indentation.
fn node_is_line_isolated(projection: &Projection<'_>, node: NodeId) -> bool {
    let node = projection.node(node);
    let Some(first) = projection.source.line(node.lines.start) else {
        return false;
    };
    let Some(last_number) = node.lines.end.checked_sub(1) else {
        return false;
    };
    let Some(last) = projection.source.line(last_number) else {
        return false;
    };
    if node.bytes.start < first.content_bytes.start || node.bytes.end > last.content_bytes.end {
        return false;
    }

    let prefix = projection
        .source
        .slice(first.content_bytes.start..node.bytes.start);
    let suffix = projection
        .source
        .slice(node.bytes.end..last.content_bytes.end);
    prefix.is_some_and(horizontal_layout) && suffix.is_some_and(horizontal_layout)
}

fn horizontal_layout(text: &str) -> bool {
    text.bytes().all(|byte| matches!(byte, b' ' | b'\t'))
}

fn same_line_endings(
    pair: &ProjectionPair<'_, '_>,
    before: &Range<usize>,
    after: &Range<usize>,
) -> bool {
    before.clone().zip(after.clone()).all(|(before, after)| {
        pair.before.source.lines()[before].ending == pair.after.source.lines()[after].ending
    })
}

fn physical_lines_equal(
    pair: &ProjectionPair<'_, '_>,
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
    pair: &ProjectionPair<'_, '_>,
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
            pair,
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
        pair,
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
            LineCheckpoint::Exact { before, after, .. } => (*before, *after),
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
            LineCheckpoint::Exact {
                before,
                after,
                strong,
            } => {
                facts.push(LineFact::Context {
                    before,
                    after,
                    strong,
                });
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
                        facts.extend(region.before.zip(region.after).map(|(before, after)| {
                            LineFact::Context {
                                before,
                                after,
                                strong: true,
                            }
                        }));
                    }
                    Retention::Reflow => {
                        let unchanged_after = correspondence
                            .line_links_in(region.before.clone(), region.after.clone())
                            .map(|link| link.after)
                            .collect();
                        facts.push(LineFact::Reflow {
                            before: region.before,
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
    coalesce_weak_line_context(facts)
}

fn display_line_checkpoints(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    before: Range<usize>,
    after: Range<usize>,
    before_bounds: &Range<usize>,
    after_bounds: &Range<usize>,
    basis: AnchorBasis,
) -> Vec<LineCheckpoint> {
    let checkpoints = physical_line_checkpoints(pair, correspondence, before, after);
    let AnchorBasis::Structural { same_owner } = basis else {
        return checkpoints;
    };

    checkpoints
        .into_iter()
        .filter_map(|checkpoint| {
            let LineCheckpoint::Exact { before, after, .. } = checkpoint else {
                return Some(checkpoint);
            };
            let first = before == before_bounds.start && after == after_bounds.start;
            let last = before.checked_add(1) == Some(before_bounds.end)
                && after.checked_add(1) == Some(after_bounds.end);
            if !same_owner && (first || last) {
                return None;
            }
            Some(LineCheckpoint::Exact {
                before,
                after,
                strong: false,
            })
        })
        .collect()
}

fn physical_line_checkpoints(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    before: Range<usize>,
    after: Range<usize>,
) -> Vec<LineCheckpoint> {
    let mut checkpoints = correspondence
        .line_links_in(before.clone(), after.clone())
        .map(|link| {
            let strong = line_link_is_display_checkpoint(pair, link.before, link.after);
            LineCheckpoint::Exact {
                before: link.before,
                after: link.after,
                strong,
            }
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

/// Exact substantive source text is sufficient to split physical replacement regions.
fn line_link_is_display_checkpoint(
    pair: &ProjectionPair<'_, '_>,
    before: usize,
    after: usize,
) -> bool {
    let Some(before_line) = pair.before.source.lines().get(before) else {
        return false;
    };
    let Some(after_line) = pair.after.source.lines().get(after) else {
        return false;
    };
    if pair.before.source.full_text(before_line) != pair.after.source.full_text(after_line) {
        return false;
    }
    substantive_checkpoint_text(pair.after.source.text(after_line))
}

fn substantive_checkpoint_text(text: &str) -> bool {
    text.chars()
        .any(|character| character.is_alphanumeric() || matches!(character, '_' | '\'' | '"'))
}

/// Weak exact rows remain context until a surrounding replacement claims their structure.
fn coalesce_weak_line_context(facts: Vec<LineFact>) -> Vec<LineFact> {
    let mut coalesced = Vec::new();
    let mut flexible = Vec::new();
    for fact in facts {
        let strong = matches!(
            fact,
            LineFact::Context { strong: true, .. }
                | LineFact::TerminatorEdit { .. }
                | LineFact::Reflow { .. }
        );
        if !strong {
            flexible.push(fact);
            continue;
        }
        flush_flexible_line_facts(&mut coalesced, &mut flexible);
        coalesced.push(fact);
    }
    flush_flexible_line_facts(&mut coalesced, &mut flexible);
    coalesced
}

fn flush_flexible_line_facts(facts: &mut Vec<LineFact>, flexible: &mut Vec<LineFact>) {
    let changes = flexible
        .iter()
        .enumerate()
        .filter_map(|(index, fact)| matches!(fact, LineFact::Edit { .. }).then_some(index))
        .collect::<Vec<_>>();
    if changes.is_empty() {
        facts.append(flexible);
        return;
    }

    let mut groups = Vec::new();
    let mut first = changes[0];
    let mut last = first;
    for change in changes.into_iter().skip(1) {
        let separating = change.saturating_sub(last + 1);
        if context_gap_fits_halos(separating) {
            last = change;
            continue;
        }
        groups.push((first, last));
        first = change;
        last = change;
    }
    groups.push((first, last));

    let flexible = std::mem::take(flexible);
    let mut cursor = 0;
    for (first, last) in groups {
        let start = first;
        let end = last.saturating_add(1);
        facts.extend_from_slice(&flexible[cursor..start]);
        facts.push(merge_flexible_line_facts(&flexible[start..end]));
        cursor = end;
    }
    facts.extend_from_slice(&flexible[cursor..]);
}

fn merge_flexible_line_facts(facts: &[LineFact]) -> LineFact {
    let mut before = None;
    let mut after = None;
    for fact in facts {
        match fact {
            LineFact::Context {
                before: line,
                after: current,
                ..
            } => {
                include_index_range(&mut before, *line..*line + 1);
                include_index_range(&mut after, *current..*current + 1);
            }
            LineFact::Edit {
                before: removed,
                after: added,
            } => {
                include_index_range(&mut before, removed.clone());
                include_index_range(&mut after, added.clone());
            }
            LineFact::TerminatorEdit { .. } | LineFact::Reflow { .. } => {
                unreachable!("atomic and retained facts flush flexible line facts")
            }
        }
    }
    LineFact::Edit {
        before: before.unwrap_or(0..0),
        after: after.unwrap_or(0..0),
    }
}

fn include_index_range(coverage: &mut Option<Range<usize>>, addition: Range<usize>) {
    if addition.is_empty() {
        return;
    }
    let Some(coverage) = coverage else {
        *coverage = Some(addition);
        return;
    };
    coverage.start = coverage.start.min(addition.start);
    coverage.end = coverage.end.max(addition.end);
}

fn plan_line_facts(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    facts: &[LineFact],
    after_root: Option<NodeId>,
) -> Vec<ReviewExcerpt> {
    let signals = facts
        .iter()
        .enumerate()
        .filter_map(|(index, fact)| fact.is_signal().then_some(index))
        .collect::<Vec<_>>();
    let Some(first) = signals.first().copied() else {
        return Vec::new();
    };

    let mut excerpts = Vec::new();
    let mut group_start = first;
    let mut group_end = first;
    for signal in signals.into_iter().skip(1) {
        let separating_context = facts[group_end + 1..signal]
            .iter()
            .filter(|fact| matches!(fact, LineFact::Context { .. }))
            .count();
        // A single omitted context row costs the same as separating the excerpts.
        if context_gap_fits_halos(separating_context) {
            group_end = signal;
            continue;
        }
        excerpts.push(plan_line_excerpt(
            pair,
            correspondence,
            facts,
            group_start,
            group_end,
            after_root,
        ));
        group_start = signal;
        group_end = signal;
    }
    excerpts.push(plan_line_excerpt(
        pair,
        correspondence,
        facts,
        group_start,
        group_end,
        after_root,
    ));
    excerpts
}

fn plan_line_excerpt(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    facts: &[LineFact],
    first_signal: usize,
    last_signal: usize,
    after_root: Option<NodeId>,
) -> ReviewExcerpt {
    let is_context = |fact: &LineFact| matches!(fact, LineFact::Context { .. });
    let start = context_halo_start(facts, first_signal, is_context);
    let end = context_halo_end(facts, last_signal, is_context);
    let before_signals = line_fact_signal_lines(&facts[first_signal..=last_signal], true);
    let after_signals = line_fact_signal_lines(&facts[first_signal..=last_signal], false);
    let alignment = LineAlignment::new(correspondence, pair.after.source.lines().len());
    let mut after_signals = after_signals;
    after_signals.extend(
        before_signals
            .into_iter()
            .filter_map(|line| alignment.current_anchor(line)),
    );
    let after_hierarchy = after_root
        .map(|root| structural_context_lines(&pair.after, root, &after_signals))
        .unwrap_or_default();
    let breadcrumbs = breadcrumb_halo_indices(&after_hierarchy, |line| {
        context_fact_for_after_line(facts, line)
    });
    let selected =
        review_selection_ranges(start..end, breadcrumbs, |index| is_context(&facts[index]));

    let mut coverage = LineCoverage {
        before: None,
        after: None,
    };
    let mut rows = Vec::new();
    let mut previous_end = None;
    for range in selected {
        if let Some(previous_end) = previous_end
            && previous_end < range.start
        {
            rows.push(DiffRow::Elision(line_fact_coverage(
                &facts[previous_end..range.start],
            )));
        }
        for fact in &facts[range.clone()] {
            append_line_fact_rows(&mut rows, &mut coverage, pair, fact);
        }
        previous_end = Some(range.end);
    }
    let shown = rows
        .iter()
        .filter_map(row_displayed_after_source_line)
        .collect::<HashSet<_>>();
    for line in after_hierarchy {
        if shown.contains(&line) {
            continue;
        }
        let source = pair
            .after
            .source
            .line(line)
            .expect("hierarchy line belongs to the current unit");
        rows.push(DiffRow::Line(build_display_line(
            &pair.after,
            source,
            &[],
            DiffMark::Context,
        )));
        include_line(&mut coverage.after, line);
    }
    ReviewExcerpt { coverage, rows }
}

/// Binary-search one exact current-world context fact in the monotone stream.
fn context_fact_for_after_line(facts: &[LineFact], line: usize) -> Option<usize> {
    let line = line.checked_sub(1)?;
    let index = facts.partition_point(|fact| line_fact_after_end(fact) <= line);
    matches!(
        facts.get(index),
        Some(LineFact::Context { after, .. }) if *after == line
    )
    .then_some(index)
}

fn line_fact_after_end(fact: &LineFact) -> usize {
    match fact {
        LineFact::Context { after, .. } => after + 1,
        LineFact::TerminatorEdit { after, .. } => after + 1,
        LineFact::Edit { after, .. } | LineFact::Reflow { after, .. } => after.end,
    }
}

fn line_fact_signal_lines(facts: &[LineFact], before_side: bool) -> HashSet<usize> {
    let mut lines = HashSet::new();
    for fact in facts {
        let range = match fact {
            LineFact::Edit { before, after } | LineFact::Reflow { before, after, .. } => {
                if before_side {
                    before
                } else {
                    after
                }
            }
            LineFact::TerminatorEdit { before, after } => {
                let line = if before_side { before } else { after };
                lines.insert(line + 1);
                continue;
            }
            LineFact::Context { .. } => continue,
        };
        lines.extend(range.clone().map(|line| line + 1));
    }
    lines
}

fn append_line_fact_rows(
    rows: &mut Vec<DiffRow>,
    coverage: &mut LineCoverage,
    pair: &ProjectionPair<'_, '_>,
    fact: &LineFact,
) {
    match fact {
        LineFact::Context { before, after, .. } => {
            include_index_coverage(&mut coverage.before, *before..*before + 1);
            include_index_coverage(&mut coverage.after, *after..*after + 1);
            rows.push(DiffRow::Line(build_display_line(
                &pair.after,
                &pair.after.source.lines()[*after],
                &[],
                DiffMark::Context,
            )));
        }
        LineFact::Edit { before, after } => {
            include_index_coverage(&mut coverage.before, before.clone());
            include_index_coverage(&mut coverage.after, after.clone());
            append_line_change_rows(rows, pair, before.clone(), after.clone());
        }
        LineFact::TerminatorEdit { before, after } => {
            include_index_coverage(&mut coverage.before, *before..*before + 1);
            include_index_coverage(&mut coverage.after, *after..*after + 1);
            append_line_change_rows(rows, pair, *before..*before + 1, *after..*after + 1);
        }
        LineFact::Reflow {
            before,
            after,
            unchanged_after,
        } => {
            include_index_coverage(&mut coverage.before, before.clone());
            include_index_coverage(&mut coverage.after, after.clone());
            append_retained_region_rows(rows, pair, after, unchanged_after);
        }
    }
}

fn line_fact_coverage(facts: &[LineFact]) -> LineCoverage {
    let side_hull = |before_side| {
        let range = |fact: &LineFact| match fact {
            LineFact::Context { before, after, .. } => {
                let line = if before_side { before } else { after };
                *line..*line + 1
            }
            LineFact::Edit { before, after } | LineFact::Reflow { before, after, .. } => {
                if before_side {
                    before.clone()
                } else {
                    after.clone()
                }
            }
            LineFact::TerminatorEdit { before, after } => {
                let line = if before_side { before } else { after };
                *line..*line + 1
            }
        };
        let start = facts
            .iter()
            .map(&range)
            .find(|range| !range.is_empty())?
            .start;
        let end = facts
            .iter()
            .rev()
            .map(range)
            .find(|range| !range.is_empty())?
            .end;
        Some(start + 1..end + 1)
    };

    LineCoverage {
        before: side_hull(true),
        after: side_hull(false),
    }
}

fn include_index_coverage(coverage: &mut Option<Range<usize>>, indices: Range<usize>) {
    if indices.is_empty() {
        return;
    }
    let lines = indices.start + 1..indices.end + 1;
    let Some(coverage) = coverage else {
        *coverage = Some(lines);
        return;
    };
    coverage.start = coverage.start.min(lines.start);
    coverage.end = coverage.end.max(lines.end);
}

fn append_line_change_rows(
    rows: &mut Vec<DiffRow>,
    pair: &ProjectionPair<'_, '_>,
    before: Range<usize>,
    after: Range<usize>,
) {
    if before.len() == 1 && after.len() == 1 {
        let before_line = &pair.before.source.lines()[before.start];
        let after_line = &pair.after.source.lines()[after.start];
        let (before_display, after_display) = changed_display_lines(
            &pair.before,
            Some(before_line),
            &pair.after,
            Some(after_line),
        );
        rows.push(DiffRow::LineChange {
            before: before_display,
            after: after_display,
        });
        if before_line.ending != after_line.ending {
            rows.push(DiffRow::LineEnding {
                before: Some(before_line.ending),
                after: Some(after_line.ending),
            });
        }
        return;
    }

    // A gap has no line-level correspondence: keep each revision as one coherent run.
    for index in before {
        let line = &pair.before.source.lines()[index];
        rows.push(DiffRow::LineChange {
            before: Some(build_display_line(
                &pair.before,
                line,
                &[MarkedRange::new(
                    line.content_bytes.clone(),
                    DiffMark::Removed,
                )],
                DiffMark::Context,
            )),
            after: None,
        });
        if line.ending == LineEnding::Missing {
            rows.push(DiffRow::LineEnding {
                before: Some(line.ending),
                after: None,
            });
        }
    }
    for index in after {
        let line = &pair.after.source.lines()[index];
        rows.push(DiffRow::LineChange {
            before: None,
            after: Some(build_display_line(
                &pair.after,
                line,
                &[MarkedRange::new(
                    line.content_bytes.clone(),
                    DiffMark::Added,
                )],
                DiffMark::Context,
            )),
        });
        if line.ending == LineEnding::Missing {
            rows.push(DiffRow::LineEnding {
                before: None,
                after: Some(line.ending),
            });
        }
    }
}

fn append_retained_region_rows(
    rows: &mut Vec<DiffRow>,
    pair: &ProjectionPair<'_, '_>,
    after: &Range<usize>,
    unchanged_after: &HashSet<usize>,
) {
    for index in after.clone() {
        let line = build_display_line(
            &pair.after,
            &pair.after.source.lines()[index],
            &[],
            DiffMark::Context,
        );
        let row = if unchanged_after.contains(&index) {
            DiffRow::Line(line)
        } else {
            DiffRow::Reflow(line)
        };
        rows.push(row);
    }
}

fn collect_source_lines<'projection>(
    projection: &'projection Projection<'_>,
    lines: Range<usize>,
) -> Vec<&'projection SourceLine> {
    lines
        .filter_map(|number| projection.source.line(number))
        .collect()
}

/// Cluster signals before each local context halo receives hierarchy breadcrumbs.
fn select_review_excerpts(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
    rows: Vec<DiffRow>,
) -> Vec<ReviewExcerpt> {
    if rows.is_empty() {
        return Vec::new();
    }

    let signals = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| (!row_is_context(row)).then_some(index))
        .collect::<Vec<_>>();
    let Some(first) = signals.first().copied() else {
        return Vec::new();
    };
    let context_rows = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            let DiffRow::Line(line) = row else {
                return None;
            };
            (!line.has_changes()).then_some((line.number, index))
        })
        .collect::<HashMap<_, _>>();

    let mut clusters = Vec::new();
    let mut cluster_start = first;
    let mut cluster_end = first;
    for signal in signals.into_iter().skip(1) {
        // A single omitted context row costs the same as an excerpt separator.
        if context_gap_fits_halos(signal.saturating_sub(cluster_end + 1)) {
            cluster_end = signal;
            continue;
        }
        clusters.push(cluster_start..cluster_end + 1);
        cluster_start = signal;
        cluster_end = signal;
    }
    clusters.push(cluster_start..cluster_end + 1);

    let alignment = LineAlignment::new(correspondence, pair.after.source.lines().len());
    clusters
        .into_iter()
        .map(|cluster| select_review_excerpt(pair, unit, &alignment, &rows, &context_rows, cluster))
        .collect()
}

fn select_review_excerpt(
    pair: &ProjectionPair<'_, '_>,
    unit: &MatchedUnit,
    alignment: &LineAlignment<'_>,
    rows: &[DiffRow],
    context_rows: &HashMap<usize, usize>,
    cluster: Range<usize>,
) -> ReviewExcerpt {
    let is_context = row_is_context;
    let start = context_halo_start(rows, cluster.start, is_context);
    let end = context_halo_end(rows, cluster.end - 1, is_context);
    let mut after_signals = rows[cluster.clone()]
        .iter()
        .filter_map(row_displayed_after_source_line)
        .collect::<HashSet<_>>();
    after_signals.extend(
        rows[cluster]
            .iter()
            .filter_map(row_before_source_line)
            .filter_map(|line| alignment.current_anchor(line)),
    );
    let after_hierarchy = structural_context_lines(&pair.after, unit.after, &after_signals);

    let breadcrumbs =
        breadcrumb_halo_indices(&after_hierarchy, |line| context_rows.get(&line).copied());
    let selected =
        review_selection_ranges(start..end, breadcrumbs, |index| is_context(&rows[index]));

    let mut selected_rows = Vec::new();
    let mut previous_line: Option<usize> = None;
    for range in selected {
        let next_line = rows[range.clone()].iter().find_map(row_after_source_line);
        if let Some((previous, next)) = previous_line.zip(next_line)
            && previous.saturating_add(1) < next
        {
            selected_rows.push(DiffRow::Elision(LineCoverage {
                before: None,
                after: Some(previous + 1..next),
            }));
        }
        selected_rows.extend_from_slice(&rows[range]);
        previous_line = selected_rows.iter().rev().find_map(row_after_source_line);
    }

    let shown = selected_rows
        .iter()
        .filter_map(row_displayed_after_source_line)
        .collect::<HashSet<_>>();
    for line in after_hierarchy {
        if shown.contains(&line) {
            continue;
        }
        let source = pair
            .after
            .source
            .line(line)
            .expect("hierarchy line belongs to the current unit");
        selected_rows.push(DiffRow::Line(build_display_line(
            &pair.after,
            source,
            &[],
            DiffMark::Context,
        )));
    }

    ReviewExcerpt {
        coverage: review_rows_coverage(&selected_rows),
        rows: selected_rows,
    }
}

fn review_rows_coverage(rows: &[DiffRow]) -> LineCoverage {
    let mut coverage = LineCoverage {
        before: None,
        after: None,
    };
    for row in rows {
        if let DiffRow::Elision(elision) = row {
            include_optional_coverage(&mut coverage.before, elision.before.clone());
            include_optional_coverage(&mut coverage.after, elision.after.clone());
            continue;
        }
        if let Some(line) = row_before_source_line(row) {
            include_line(&mut coverage.before, line);
        }
        if let Some(line) = row_after_source_line(row) {
            include_line(&mut coverage.after, line);
        }
    }
    coverage
}

/// Neutral CST ancestor starts form sparse, grammar-independent hierarchy breadcrumbs.
fn structural_context_lines(
    projection: &Projection<'_>,
    root: NodeId,
    signals: &HashSet<usize>,
) -> HashSet<usize> {
    if signals.is_empty() {
        return HashSet::new();
    }

    let root_lines = projection.node(root).lines.clone();
    let signals = signals
        .iter()
        .copied()
        .filter(|line| root_lines.contains(line))
        .collect::<Vec<_>>();
    if signals.is_empty() {
        return HashSet::new();
    }

    let mut context = HashSet::new();
    if projection.node(root).leaf.is_none() && !root_lines.is_empty() {
        context.insert(root_lines.start);
    }
    for signal in signals {
        let Some(line) = projection.source.line(signal) else {
            continue;
        };
        for leaf in projection.leaf_ids_in(line.content_bytes.clone()) {
            let mut path = Vec::new();
            let mut ancestor = projection.node(leaf).parent;
            while let Some(id) = ancestor {
                let node = projection.node(id);
                if node.leaf.is_none() && !node.lines.is_empty() {
                    path.push(node.lines.start);
                }
                if id == root {
                    context.extend(path);
                    break;
                }
                ancestor = node.parent;
            }
        }
    }
    context
}

/// Preserve replacement boundaries before independently planned excerpts coalesce.
fn group_rows(rows: Vec<DiffRow>) -> Vec<Vec<DiffRow>> {
    let mut groups: Vec<Vec<DiffRow>> = Vec::new();
    let mut rows = rows.into_iter().peekable();
    while let Some(row) = rows.next() {
        if matches!(row, DiffRow::LineEnding { .. })
            && let Some(group) = groups.last_mut()
        {
            // Terminators have no independent geometry; they follow the source
            // row whose physical ending they describe.
            group.push(row);
            continue;
        }
        if !matches!(row, DiffRow::LineChange { .. }) {
            groups.push(vec![row]);
            continue;
        }

        let mut run = vec![row];
        while matches!(
            rows.peek(),
            Some(DiffRow::LineChange { .. } | DiffRow::LineEnding { .. })
        ) {
            run.push(rows.next().expect("peeked replacement row"));
        }
        groups.push(run);
    }
    groups
}

/// Multi-row replacements are revision runs, never ordinal before/after pairs.
fn order_replacement_group(rows: Vec<DiffRow>) -> Vec<DiffRow> {
    let line_changes = rows
        .iter()
        .filter(|row| matches!(row, DiffRow::LineChange { .. }))
        .count();
    if line_changes <= 1 {
        return rows;
    }

    let mut before = Vec::new();
    let mut after = Vec::new();
    for row in rows {
        match row {
            DiffRow::LineChange {
                before: old,
                after: current,
            } => {
                before.extend(old.map(|line| DiffRow::LineChange {
                    before: Some(line),
                    after: None,
                }));
                after.extend(current.map(|line| DiffRow::LineChange {
                    before: None,
                    after: Some(line),
                }));
            }
            DiffRow::LineEnding {
                before: old,
                after: current,
            } => {
                // A concrete terminator replacement is one source fact. Keep it
                // beside the current row instead of fabricating remove/add facts.
                if let (Some(old), Some(current)) = (old, current) {
                    after.push(DiffRow::LineEnding {
                        before: Some(old),
                        after: Some(current),
                    });
                    continue;
                }
                before.extend(old.map(|ending| DiffRow::LineEnding {
                    before: Some(ending),
                    after: None,
                }));
                after.extend(current.map(|ending| DiffRow::LineEnding {
                    before: None,
                    after: Some(ending),
                }));
            }
            _ => unreachable!("replacement group contains only source rows and line endings"),
        }
    }
    before.extend(after);
    before
}

/// A mixed replacement belongs at its current-world position. Its old run is
/// display payload, not an earlier independent source event.
fn group_source_order(group: &[DiffRow], alignment: &LineAlignment<'_>) -> SourceOrder {
    group
        .iter()
        .filter_map(row_after_source_line)
        .min()
        .map(SourceOrder::current)
        .or_else(|| {
            group
                .iter()
                .filter_map(row_before_source_line)
                .map(|line| alignment.before_order(line))
                .min()
        })
        .expect("display group owns source geometry")
}

fn row_is_context(row: &DiffRow) -> bool {
    matches!(row, DiffRow::Line(line) if !line.has_changes())
}

fn row_after_source_line(row: &DiffRow) -> Option<usize> {
    match row {
        DiffRow::Line(line) | DiffRow::Reflow(line) => Some(line.number),
        DiffRow::LineChange { after, .. } => after.as_ref().map(|line| line.number),
        DiffRow::Moved { after, .. } => Some(after.number),
        DiffRow::Wordwise(word) => word.after_line,
        DiffRow::Elision(coverage) => coverage.after.as_ref().map(|range| range.start),
        DiffRow::LineEnding { .. } | DiffRow::FileBoundary => None,
    }
}

fn row_displayed_after_source_line(row: &DiffRow) -> Option<usize> {
    if matches!(row, DiffRow::Elision(_)) {
        return None;
    }
    row_after_source_line(row)
}

fn row_before_source_line(row: &DiffRow) -> Option<usize> {
    match row {
        DiffRow::LineChange { before, .. } => before.as_ref().map(|line| line.number),
        DiffRow::Moved { before, .. } => *before,
        DiffRow::Wordwise(word) => word.before_line,
        DiffRow::Elision(coverage) => coverage.before.as_ref().map(|range| range.start),
        DiffRow::Line(_)
        | DiffRow::Reflow(_)
        | DiffRow::LineEnding { .. }
        | DiffRow::FileBoundary => None,
    }
}

/// Append one terminal boundary after global hunk ordering and abbreviation are final.
fn append_file_boundary(hunks: &mut [Hunk], before_lines: usize, after_lines: usize) {
    let reaches_current = hunks
        .iter()
        .any(|hunk| hunk_reaches_after_boundary(hunk, after_lines));
    let reaches_removed = !reaches_current
        && hunks.iter().any(|hunk| {
            hunk.coverage.after.is_none()
                && hunk
                    .coverage
                    .before
                    .as_ref()
                    .is_some_and(|coverage| coverage.end == before_lines.saturating_add(1))
        });
    if !reaches_current && !reaches_removed {
        return;
    }

    let Some(last) = hunks.last_mut() else {
        return;
    };
    last.rows.push(DiffRow::FileBoundary);
}

fn hunk_reaches_after_boundary(hunk: &Hunk, after_lines: usize) -> bool {
    let coverage_reaches = hunk
        .coverage
        .after
        .as_ref()
        .is_some_and(|coverage| coverage.end == after_lines.saturating_add(1));
    // Sparse structural context can reach EOF without widening signal coverage.
    let context_reaches = matches!(
        hunk.rows.last(),
        Some(DiffRow::Line(line) | DiffRow::Reflow(line)) if line.number == after_lines
    );
    coverage_reaches || context_reaches
}

#[cfg(test)]
mod tests;
