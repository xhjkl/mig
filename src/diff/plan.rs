//! Pretty-hunk planning over neutral correspondence facts.
//!
//! Ordinary signal rows grow a three-line context halo. Neutral CST ancestors
//! contribute hierarchy breadcrumbs with display-only halos that do not widen
//! signal merge focus.

use super::correspondence::{
    Correspondence, LeafRelation, MatchedUnit, NodeLink, Placement, UnitEdit,
};
use super::projection::{ContentChannel, NodeId, Projection, ProjectionPair, ReviewTreatment};
use super::source::SourceLine;
use super::{
    CodeLine, CodeRole, CodeSpan, DiffMark, DiffRow, Hunk, LineCoverage, LineEnding, SyntaxClass,
    WordDiff,
};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// Physical lines retained on either side of an ordinary signal.
const CONTEXT_HALO_RADIUS: usize = 3;

/// Two halos coalesce when separating them would hide at most one physical row.
fn context_gap_fits_halos(context_rows: usize) -> bool {
    context_rows <= CONTEXT_HALO_RADIUS * 2 + 1
}

fn context_halo_start<T>(items: &[T], signal: usize, is_context: impl Fn(&T) -> bool) -> usize {
    let mut index = signal;
    let mut retained = 0;
    while index > 0 && retained < CONTEXT_HALO_RADIUS {
        if !is_context(&items[index - 1]) {
            break;
        }
        index -= 1;
        retained += 1;
    }
    index
}

fn context_halo_end<T>(items: &[T], signal: usize, is_context: impl Fn(&T) -> bool) -> usize {
    let mut index = signal + 1;
    let mut retained = 0;
    while index < items.len() && retained < CONTEXT_HALO_RADIUS {
        if !is_context(&items[index]) {
            break;
        }
        index += 1;
        retained += 1;
    }
    index
}

/// Contiguous unchanged rows around each display-only hierarchy step.
fn breadcrumb_halo_indices(
    breadcrumbs: &HashSet<usize>,
    mut context_index: impl FnMut(usize) -> Option<usize>,
) -> Vec<usize> {
    let mut indices = Vec::new();
    for breadcrumb in breadcrumbs {
        indices.extend(context_index(*breadcrumb));

        let mut before = *breadcrumb;
        for _ in 0..CONTEXT_HALO_RADIUS {
            let Some(line) = before.checked_sub(1).filter(|line| *line > 0) else {
                break;
            };
            let Some(index) = context_index(line) else {
                break;
            };
            indices.push(index);
            before = line;
        }

        let mut after = *breadcrumb;
        for _ in 0..CONTEXT_HALO_RADIUS {
            let line = after.saturating_add(1);
            let Some(index) = context_index(line) else {
                break;
            };
            indices.push(index);
            after = line;
        }
    }
    indices.sort_unstable();
    indices.dedup();
    indices
}

/// Merge signal and breadcrumb context halos without widening hunk merge focus.
fn review_selection_ranges(
    context_halo: Range<usize>,
    breadcrumbs: impl IntoIterator<Item = usize>,
    is_context: impl Fn(usize) -> bool,
) -> Vec<Range<usize>> {
    let mut selected = vec![context_halo];
    selected.extend(breadcrumbs.into_iter().map(|index| index..index + 1));
    selected.sort_by_key(|range| range.start);

    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in selected {
        let Some(previous) = merged.last_mut() else {
            merged.push(range);
            continue;
        };
        let one_context_gap =
            range.start == previous.end.saturating_add(1) && is_context(previous.end);
        if range.start <= previous.end || one_context_gap {
            previous.end = previous.end.max(range.end);
            continue;
        }
        merged.push(range);
    }
    merged
}

/// Treatment precedence only when multiple signals share one aligned edit gap.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum HunkPhase {
    Semantic,
    Move,
    Compact,
    Reflow,
}

struct PlannedHunk {
    phase: HunkPhase,
    coverage: LineCoverage,
    groups: Vec<Vec<DiffRow>>,
    focus: HunkFocus,
    context_focus: HunkFocus,
    order: AlignedOrder,
    context: Option<Range<usize>>,
}

impl PlannedHunk {
    fn new(phase: HunkPhase, hunk: Hunk) -> Self {
        let focus = HunkFocus::from_rows(&hunk.rows);
        let context_focus = if phase == HunkPhase::Move {
            HunkFocus::default()
        } else {
            focus.clone()
        };
        Self {
            phase,
            coverage: hunk.coverage,
            groups: group_rows(hunk.rows),
            focus,
            context_focus,
            order: AlignedOrder::LAST,
            context: None,
        }
    }

    fn align(&mut self, alignment: &LineAlignment<'_>) {
        self.order = alignment.focus_order(&self.focus);
        self.context = alignment
            .current_focus(&self.focus)
            .or_else(|| line_hull(&self.focus.before));
    }
}

/// Physical signal lines retained before context halos or breadcrumbs expand selection.
#[derive(Clone, Default)]
struct HunkFocus {
    before: Vec<usize>,
    after: Vec<usize>,
}

impl HunkFocus {
    fn from_rows(rows: &[DiffRow]) -> Self {
        let mut focus = Self::default();
        for row in rows {
            match row {
                DiffRow::Code {
                    line,
                    role: CodeRole::Inline | CodeRole::Reflow,
                } => focus.after.push(line.number),
                DiffRow::Linewise { before, after } => {
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
                DiffRow::Code {
                    role: CodeRole::Context,
                    ..
                }
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

/// Source position plus old-source order within one unmatched edit gap.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AlignedOrder {
    position: usize,
    old_line: usize,
}

impl AlignedOrder {
    const LAST: Self = Self {
        position: usize::MAX,
        old_line: usize::MAX,
    };

    fn current(line: usize) -> Self {
        Self {
            position: line.saturating_mul(2),
            old_line: 0,
        }
    }
}

impl<'graph> LineAlignment<'graph> {
    fn new(graph: &'graph Correspondence, after_lines: usize) -> Self {
        Self { graph, after_lines }
    }

    /// Odd coordinates are before-blocks in a gap; even coordinates are current lines.
    fn before_order(&self, line: usize) -> AlignedOrder {
        let index = line.saturating_sub(1);
        let insertion = self
            .graph
            .line_links
            .partition_point(|link| link.before < index);
        if let Some(link) = self.graph.line_links.get(insertion)
            && link.before == index
        {
            return AlignedOrder::current(link.after + 1);
        }

        let gap_start = insertion
            .checked_sub(1)
            .and_then(|previous| self.graph.line_links.get(previous))
            .map_or(1, |link| link.after + 2);
        AlignedOrder {
            position: gap_start.saturating_mul(2).saturating_sub(1),
            old_line: line,
        }
    }

    fn current_anchor(&self, before_line: usize) -> Option<usize> {
        if self.after_lines == 0 {
            return None;
        }
        let order = self.before_order(before_line).position;
        let line = order.saturating_add(1) / 2;
        Some(line.clamp(1, self.after_lines))
    }

    fn exact_before_line(&self, after_line: usize) -> Option<usize> {
        let index = after_line.checked_sub(1)?;
        let insertion = self
            .graph
            .line_links
            .partition_point(|link| link.after < index);
        let link = self.graph.line_links.get(insertion)?;
        (link.after == index).then_some(link.before + 1)
    }

    fn focus_order(&self, focus: &HunkFocus) -> AlignedOrder {
        focus
            .after
            .iter()
            .map(|line| AlignedOrder::current(*line))
            .min()
            .or_else(|| {
                focus
                    .before
                    .iter()
                    .map(|line| self.before_order(*line))
                    .min()
            })
            .unwrap_or(AlignedOrder::LAST)
    }

    fn current_focus(&self, focus: &HunkFocus) -> Option<Range<usize>> {
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
        .tracked_units()
        .chain(pair.after.tracked_units())
        .all(|(_, node)| {
            node.review
                .as_ref()
                .is_some_and(|review| review.treatment == ReviewTreatment::Linewise)
        });
    let mut planned = if only_linewise {
        semantic_hunks(plan_whole_file_lines(pair, correspondence))
    } else {
        plan_units(pair, correspondence)
    };

    let alignment = LineAlignment::new(correspondence, pair.after.source.lines().len());
    for planned in &mut planned {
        planned.align(&alignment);
    }
    // Geometry is source-ordered so only neighboring context halos can merge.
    planned.sort_by_key(coalescing_order);
    let mut planned = coalesce_hunks(planned);
    // Review cadence is semantic-first after nearby auxiliary facts are attached.
    planned.sort_by_key(presentation_order);
    let mut hunks = Vec::with_capacity(planned.len());
    for mut planned in planned {
        if !planned.context_focus.is_empty() {
            complete_context_halos(pair, &alignment, &mut planned);
            complete_display_gaps(pair, &alignment, &mut planned);
        }
        for group in &mut planned.groups {
            *group = order_replacement_group(std::mem::take(group));
        }
        planned.groups.sort_by_key(|group| {
            group
                .iter()
                .map(|row| row_source_order(row, &alignment))
                .min()
                .unwrap_or(AlignedOrder::LAST)
        });
        hunks.push(Hunk {
            coverage: planned.coverage,
            rows: planned.groups.into_iter().flatten().collect(),
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
                DiffRow::Code {
                    line,
                    role: CodeRole::Inline | CodeRole::Reflow,
                } => Some(line.number),
                DiffRow::Linewise { after, .. } => after.as_ref().map(|line| line.number),
                DiffRow::Moved { after, .. } => Some(after.number),
                DiffRow::Wordwise(word) => word.after_line,
                _ => None,
            })
            .collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        hunk.rows.retain(|row| {
            let DiffRow::Code {
                line,
                role: CodeRole::Context,
            } = row
            else {
                return true;
            };
            !signal_lines.contains(&line.number) && seen.insert(line.number)
        });
    }
    hunks.retain(|hunk| !hunk.rows.is_empty());
}

/// Complete each signal's three-line context halo against the whole current file.
fn complete_context_halos(
    pair: &ProjectionPair<'_, '_>,
    alignment: &LineAlignment<'_>,
    planned: &mut PlannedHunk,
) {
    let line_count = pair.after.source.lines().len();
    if line_count == 0 {
        return;
    }

    let mut signals = planned.context_focus.after.clone();
    signals.extend(
        planned
            .context_focus
            .before
            .iter()
            .filter_map(|line| alignment.current_anchor(*line)),
    );
    signals.sort_unstable();
    signals.dedup();

    let mut shown = planned
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
            planned.groups.push(vec![DiffRow::Code {
                line: build_code_line(&pair.after, line, &[], DiffMark::Context),
                role: CodeRole::Context,
            }]);
            include_line(&mut planned.coverage.after, number);
        }
    }
}

/// Hierarchy pins added outside a producer's range still explain the omitted interval.
fn complete_display_gaps(
    pair: &ProjectionPair<'_, '_>,
    alignment: &LineAlignment<'_>,
    planned: &mut PlannedHunk,
) {
    let mut displayed = planned
        .groups
        .iter()
        .flatten()
        .filter_map(row_displayed_after_source_line)
        .collect::<Vec<_>>();
    displayed.sort_unstable();
    displayed.dedup();
    let before_displayed = displayed
        .iter()
        .filter_map(|line| alignment.exact_before_line(*line))
        .collect::<Vec<_>>();
    trim_elisions_behind_context(&mut planned.groups, &before_displayed, &displayed);
    let elisions = planned
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
            planned.groups.push(vec![DiffRow::Code {
                line: build_code_line(&pair.after, source, &[], DiffMark::Context),
                role: CodeRole::Context,
            }]);
            include_line(&mut planned.coverage.after, number);
            continue;
        }
        planned.groups.push(vec![DiffRow::Elision(LineCoverage {
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

fn semantic_hunks(hunks: Vec<Hunk>) -> Vec<PlannedHunk> {
    hunks
        .into_iter()
        .map(|hunk| PlannedHunk::new(HunkPhase::Semantic, hunk))
        .collect()
}

/// Source order used only while deciding which physical context halos coalesce.
fn coalescing_order(planned: &PlannedHunk) -> (AlignedOrder, HunkPhase) {
    (planned.order, planned.phase)
}

/// Primary logic first, then moves, compact declarations, and pure reflow.
fn presentation_order(planned: &PlannedHunk) -> (HunkPhase, AlignedOrder) {
    (planned.phase, planned.order)
}

/// Context halos that touch (or would omit only one row) form one visual hunk.
fn coalesce_hunks(planned: Vec<PlannedHunk>) -> Vec<PlannedHunk> {
    let mut hunks: Vec<PlannedHunk> = Vec::new();

    for mut planned in planned {
        let Some(previous) = hunks.last_mut() else {
            hunks.push(planned);
            continue;
        };
        if !context_halos_touch(previous.context.as_ref(), planned.context.as_ref()) {
            hunks.push(planned);
            continue;
        }

        include_optional_coverage(&mut previous.coverage.before, planned.coverage.before);
        include_optional_coverage(&mut previous.coverage.after, planned.coverage.after);
        previous.groups.append(&mut planned.groups);
        previous.phase = previous.phase.min(planned.phase);
        previous.focus.merge(planned.focus);
        previous.context_focus.merge(planned.context_focus);
        include_optional_coverage(&mut previous.context, planned.context);
    }

    hunks
}

fn context_halos_touch(left: Option<&Range<usize>>, right: Option<&Range<usize>>) -> bool {
    left.zip(right)
        .is_some_and(|(left, right)| ranges_share_context(left, right))
}

fn line_hull(lines: &[usize]) -> Option<Range<usize>> {
    Some(*lines.first()?..lines.last()?.saturating_add(1))
}

fn ranges_share_context(left: &Range<usize>, right: &Range<usize>) -> bool {
    context_gap_fits_halos(right.start.saturating_sub(left.end))
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

fn plan_units(pair: &ProjectionPair<'_, '_>, correspondence: &Correspondence) -> Vec<PlannedHunk> {
    let mut hunks = Vec::new();

    for edit in &correspondence.units {
        match edit {
            UnitEdit::Matched(unit) => plan_matched_unit(pair, correspondence, unit, &mut hunks),
            UnitEdit::Removed { before } => {
                let node = pair.before.node(*before);
                let review = node
                    .review
                    .as_ref()
                    .expect("review edit owns a review unit");
                match review.treatment {
                    ReviewTreatment::Compact => hunks.push(PlannedHunk::new(
                        HunkPhase::Compact,
                        plan_one_sided_lines(&pair.before, node.lines.clone(), DiffMark::Removed),
                    )),
                    ReviewTreatment::Linewise | ReviewTreatment::Inline => {
                        hunks.extend(semantic_hunks(vec![plan_one_sided_lines(
                            &pair.before,
                            node.lines.clone(),
                            DiffMark::Removed,
                        )]))
                    }
                }
            }
            UnitEdit::Added { after } => {
                let node = pair.after.node(*after);
                let review = node
                    .review
                    .as_ref()
                    .expect("review edit owns a review unit");
                match review.treatment {
                    ReviewTreatment::Compact => hunks.push(PlannedHunk::new(
                        HunkPhase::Compact,
                        plan_one_sided_lines(&pair.after, node.lines.clone(), DiffMark::Added),
                    )),
                    ReviewTreatment::Linewise | ReviewTreatment::Inline => {
                        hunks.extend(semantic_hunks(vec![plan_one_sided_lines(
                            &pair.after,
                            node.lines.clone(),
                            DiffMark::Added,
                        )]))
                    }
                }
            }
        }
    }
    hunks
}

fn plan_matched_unit(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
    hunks: &mut Vec<PlannedHunk>,
) {
    let after_node = pair.after.node(unit.after);
    let review = after_node
        .review
        .as_ref()
        .expect("matched review edit owns a review unit");

    if unit.placement == Placement::Reordered && unit.relation.full_equal() {
        hunks.push(PlannedHunk::new(HunkPhase::Move, plan_move(pair, unit)));
        return;
    }
    if unit.relation.source_equal() {
        return;
    }

    match review.treatment {
        ReviewTreatment::Compact => {
            let before = pair.before.node(unit.before);
            if !node_is_single_line(before) || !node_is_single_line(after_node) {
                let composites = correspondence.unit_composites(unit);
                let line_hunks = plan_line_region(
                    pair,
                    correspondence,
                    Some(before.lines.clone()),
                    Some(after_node.lines.clone()),
                    composites,
                    Some(unit.after),
                );
                hunks.extend(
                    line_hunks
                        .into_iter()
                        .map(|hunk| PlannedHunk::new(HunkPhase::Compact, hunk)),
                );
                return;
            }
            let hunk = plan_compact(pair, Some(unit.before), Some(unit.after));
            hunks.push(PlannedHunk::new(HunkPhase::Compact, hunk));
        }
        ReviewTreatment::Linewise => {
            let composites = correspondence.unit_composites(unit);
            hunks.extend(semantic_hunks(plan_line_region(
                pair,
                correspondence,
                Some(pair.before.node(unit.before).lines.clone()),
                Some(after_node.lines.clone()),
                composites,
                Some(unit.after),
            )));
        }
        ReviewTreatment::Inline => {
            if unit.relation.full_equal() {
                hunks.extend(
                    plan_reflow(pair, correspondence, unit)
                        .into_iter()
                        .map(|hunk| PlannedHunk::new(HunkPhase::Reflow, hunk)),
                );
                return;
            }

            let comments = comment_edits(pair, correspondence, unit);
            if unit.relation.code_equal() {
                hunks.extend(
                    plan_reflow_with_comments(pair, correspondence, unit, comments)
                        .into_iter()
                        .map(|hunk| PlannedHunk::new(HunkPhase::Semantic, hunk)),
                );
                return;
            }
            let needs_physical_plan = pair.before.identity_text(unit.before)
                != pair.after.identity_text(unit.after)
                || has_retainable_reparented_block(pair, correspondence, unit)
                || has_unmatched_before_content(pair, correspondence, unit);
            if needs_physical_plan {
                let line_hunks = plan_line_region(
                    pair,
                    correspondence,
                    Some(pair.before.node(unit.before).lines.clone()),
                    Some(after_node.lines.clone()),
                    correspondence.unit_composites(unit),
                    Some(unit.after),
                );
                hunks.extend(semantic_hunks(line_hunks));
                return;
            }
            hunks.extend(
                plan_inline(pair, correspondence, unit, comments)
                    .into_iter()
                    .map(|hunk| PlannedHunk::new(HunkPhase::Semantic, hunk)),
            );
        }
    }
}

fn has_retainable_reparented_block(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
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
        .any(|link| retained_block(pair, link, &before, &after).is_some())
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

fn plan_one_sided_lines(projection: &Projection<'_>, lines: Range<usize>, mark: DiffMark) -> Hunk {
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
    for line in collect_source_lines(projection, lines) {
        let code = build_code_line(
            projection,
            line,
            &[MarkedRange::new(line.content_bytes.clone(), mark)],
            DiffMark::Context,
        );
        let (before, after) = if mark == DiffMark::Removed {
            (Some(code), None)
        } else {
            (None, Some(code))
        };
        rows.push(DiffRow::Linewise { before, after });
        if line.ending == LineEnding::Missing {
            rows.push(DiffRow::LineEnding {
                before: (mark == DiffMark::Removed).then_some(line.ending),
                after: (mark == DiffMark::Added).then_some(line.ending),
            });
        }
    }
    Hunk { coverage, rows }
}

fn plan_compact(
    pair: &ProjectionPair<'_, '_>,
    before: Option<NodeId>,
    after: Option<NodeId>,
) -> Hunk {
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

    Hunk {
        coverage: LineCoverage {
            before: before_node.map(|node| node.lines.clone()),
            after: after_node.map(|node| node.lines.clone()),
        },
        rows: vec![DiffRow::Wordwise(word)],
    }
}

fn word_diff(
    before_line: Option<usize>,
    after_line: Option<usize>,
    before: &str,
    after: &str,
) -> WordDiff {
    let before = before.chars().collect::<Vec<_>>();
    let after = after.chars().collect::<Vec<_>>();
    let (before_changed, after_changed) = changed_sequence_ranges(&before, &after);

    WordDiff {
        before_line,
        after_line,
        prefix: before[..before_changed.start].iter().collect(),
        removed: before[before_changed.clone()].iter().collect(),
        added: after[after_changed].iter().collect(),
        suffix: before[before_changed.end..].iter().collect(),
    }
}

fn changed_sequence_ranges<T: Eq>(before: &[T], after: &[T]) -> (Range<usize>, Range<usize>) {
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

fn plan_move(pair: &ProjectionPair<'_, '_>, unit: &MatchedUnit) -> Hunk {
    let before = pair.before.node(unit.before);
    let after = pair.after.node(unit.after);
    let coverage = LineCoverage {
        before: Some(before.lines.clone()),
        after: Some(after.lines.clone()),
    };
    let mut lines = build_code_lines(&pair.after, after.lines.clone(), &[], DiffMark::Context);
    let Some(first) = lines.first().cloned() else {
        return Hunk {
            coverage,
            rows: Vec::new(),
        };
    };
    if lines.len() == 1 {
        return Hunk {
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
        rows.push(DiffRow::Code {
            line: lines.pop().expect("one middle line remains"),
            role: CodeRole::Context,
        });
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
    Hunk { coverage, rows }
}

fn plan_reflow(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
) -> Vec<Hunk> {
    let before = pair.before.node(unit.before);
    let after = pair.after.node(unit.after);
    let before_lines = line_indices(Some(before.lines.clone()), pair.before.source.lines().len());
    let after_lines = line_indices(Some(after.lines.clone()), pair.after.source.lines().len());
    let exact_after = correspondence
        .line_links_in(before_lines, after_lines)
        .map(|link| link.after + 1)
        .collect::<HashSet<_>>();
    let rows = build_code_lines(&pair.after, after.lines.clone(), &[], DiffMark::Context)
        .into_iter()
        .map(|line| DiffRow::Code {
            role: if exact_after.contains(&line.number) {
                CodeRole::Context
            } else {
                CodeRole::Reflow
            },
            line,
        })
        .collect::<Vec<_>>();
    select_review_hunks(pair, correspondence, unit, rows)
}

fn plan_reflow_with_comments(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
    mut edits: Vec<LineEdit>,
) -> Vec<Hunk> {
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
    for line in build_code_lines(&pair.after, after.lines.clone(), &[], DiffMark::Context) {
        while edits
            .peek()
            .is_some_and(|edit| line_edit_order(edit) < line.number)
        {
            append_line_edit_rows(&mut rows, edits.next().expect("peeked line edit"));
        }
        if changed_lines.contains(&line.number) {
            continue;
        }
        let role = if exact_after.contains(&line.number) {
            CodeRole::Context
        } else {
            CodeRole::Reflow
        };
        rows.push(DiffRow::Code { line, role });
    }
    for edit in edits {
        append_line_edit_rows(&mut rows, edit);
    }

    select_review_hunks(pair, correspondence, unit, rows)
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

fn plan_inline(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
    mut edits: Vec<LineEdit>,
) -> Vec<Hunk> {
    let before = pair.before.node(unit.before);
    let after = pair.after.node(unit.after);
    edits.extend(modified_code_edits(pair, correspondence, unit));
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
    let before_marked = marked_leaf_ranges(&pair.before, changed_before, DiffMark::Removed);
    let after_marked = marked_leaf_ranges(&pair.after, changed_after, DiffMark::Added);
    edits.extend(fully_marked_code_line_edits(
        pair,
        before.lines.clone(),
        after.lines.clone(),
        &before_marked,
        &after_marked,
    ));
    deduplicate_line_edits(&mut edits);
    expand_line_edits_through_weak_rows(pair, correspondence, &mut edits);
    let changed_lines = edits
        .iter()
        .filter_map(|edit| edit.after.as_ref().map(|line| line.number))
        .collect::<HashSet<_>>();
    let lines = build_code_lines(
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
        let role = if line.spans.iter().any(|span| span.mark != DiffMark::Context) {
            CodeRole::Inline
        } else {
            CodeRole::Context
        };
        rows.push(DiffRow::Code { line, role });
    }
    for edit in edits {
        append_line_edit_rows(&mut rows, edit);
    }

    select_review_hunks(pair, correspondence, unit, rows)
}

/// One physical replacement row retained with both source revisions.
struct LineEdit {
    before: Option<CodeLine>,
    after: Option<CodeLine>,
    before_ending: Option<LineEnding>,
    after_ending: Option<LineEnding>,
}

fn comment_edits(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
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
        if !comment
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

fn modified_code_edits(
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

fn fully_marked_code_line_edits(
    pair: &ProjectionPair<'_, '_>,
    before_lines: Range<usize>,
    after_lines: Range<usize>,
    before_marked: &[MarkedRange],
    after_marked: &[MarkedRange],
) -> Vec<LineEdit> {
    let mut edits = Vec::new();
    for line in collect_source_lines(&pair.before, before_lines) {
        let code = build_code_line(&pair.before, line, before_marked, DiffMark::Context);
        if line_is_fully_marked(&code, DiffMark::Removed) {
            edits.push(changed_line_edit(
                &pair.before,
                Some(line),
                &pair.after,
                None,
            ));
        }
    }
    for line in collect_source_lines(&pair.after, after_lines) {
        let code = build_code_line(&pair.after, line, after_marked, DiffMark::Context);
        if line_is_fully_marked(&code, DiffMark::Added) {
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

fn line_is_fully_marked(line: &CodeLine, mark: DiffMark) -> bool {
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

/// Exact weak rows between nearby edits belong to their shared replacement.
fn expand_line_edits_through_weak_rows(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
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
        let checkpoints =
            exact_line_checkpoints(pair, correspondence, before.clone(), after.clone());
        let all_weak = checkpoints.len() == before.len()
            && checkpoints.iter().enumerate().all(|(offset, checkpoint)| {
                matches!(
                    checkpoint,
                    LineCheckpoint::Exact {
                        before: old,
                        after: current,
                        strong: false,
                    } if *old == before.start + offset && *current == after.start + offset
                )
            });
        if !all_weak {
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
    let (before_code, after_code) = changed_code_lines(before, before_line, after, after_line);
    LineEdit {
        before: before_code,
        after: after_code,
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
    rows.push(DiffRow::Linewise {
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
) -> Vec<Hunk> {
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
    )
}

#[derive(Clone, Debug)]
enum LineEvent {
    Context {
        before: usize,
        after: usize,
        strong: bool,
    },
    Change {
        before: Range<usize>,
        after: Range<usize>,
    },
    Reflow {
        before: Range<usize>,
        after: Range<usize>,
        unchanged_after: HashSet<usize>,
    },
}

impl LineEvent {
    fn is_signal(&self) -> bool {
        !matches!(self, Self::Context { .. })
    }
}

#[derive(Clone, Debug)]
struct RetainedBlock {
    before: Range<usize>,
    after: Range<usize>,
}

#[derive(Clone, Debug)]
enum LineCheckpoint {
    Exact {
        before: usize,
        after: usize,
        strong: bool,
    },
    Retained(RetainedBlock),
}

fn plan_line_region(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    before_lines: Option<Range<usize>>,
    after_lines: Option<Range<usize>>,
    composites: &[NodeLink],
    after_root: Option<NodeId>,
) -> Vec<Hunk> {
    let before = line_indices(before_lines, pair.before.source.lines().len());
    let after = line_indices(after_lines, pair.after.source.lines().len());
    let retained = retained_blocks(pair, composites, &before, &after);
    let events = line_events(pair, correspondence, before, after, retained);
    plan_line_events(pair, correspondence, &events, after_root)
}

fn line_indices(lines: Option<Range<usize>>, line_count: usize) -> Range<usize> {
    let Some(lines) = lines else {
        return 0..0;
    };
    let start = lines.start.saturating_sub(1).min(line_count);
    let end = lines.end.saturating_sub(1).min(line_count);
    start.min(end)..end
}

fn retained_blocks(
    pair: &ProjectionPair<'_, '_>,
    composites: &[NodeLink],
    before_region: &Range<usize>,
    after_region: &Range<usize>,
) -> Vec<RetainedBlock> {
    let mut candidates = composites
        .iter()
        .filter_map(|link| retained_block(pair, *link, before_region, after_region))
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

fn retained_block(
    pair: &ProjectionPair<'_, '_>,
    link: NodeLink,
    before_region: &Range<usize>,
    after_region: &Range<usize>,
) -> Option<RetainedBlock> {
    if link.placement == Placement::Reordered {
        return None;
    }
    let before = pair.before.node(link.before);
    let after = pair.after.node(link.after);
    let before_identity = pair.before.identity_text(link.before)?;
    let after_identity = pair.after.identity_text(link.after)?;
    if before_identity != after_identity {
        return None;
    }
    if subtree_has_opaque_leaf(&pair.before, link.before)
        || subtree_has_opaque_leaf(&pair.after, link.after)
    {
        return None;
    }
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
    if !same_line_endings(pair, &before_lines, &after_lines) {
        return None;
    }
    let physical_equal = physical_lines_equal(pair, &before_lines, &after_lines);
    if physical_equal && !link.reparented {
        return None;
    }

    Some(RetainedBlock {
        before: before_lines,
        after: after_lines,
    })
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

fn subtree_has_opaque_leaf(projection: &Projection<'_>, root: NodeId) -> bool {
    descendant_leaves(projection, root).any(|leaf| {
        projection
            .node(leaf)
            .leaf
            .is_some_and(|leaf| leaf.channel == ContentChannel::Opaque)
    })
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

fn line_events(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    before: Range<usize>,
    after: Range<usize>,
    retained: Vec<RetainedBlock>,
) -> Vec<LineEvent> {
    let mut checkpoints = Vec::new();
    let mut before_start = before.start;
    let mut after_start = after.start;
    for block in retained {
        checkpoints.extend(exact_line_checkpoints(
            pair,
            correspondence,
            before_start..block.before.start,
            after_start..block.after.start,
        ));
        before_start = block.before.end;
        after_start = block.after.end;
        checkpoints.push(LineCheckpoint::Retained(block));
    }
    checkpoints.extend(exact_line_checkpoints(
        pair,
        correspondence,
        before_start..before.end,
        after_start..after.end,
    ));

    let mut events = Vec::new();
    let mut before_start = before.start;
    let mut after_start = after.start;
    for checkpoint in checkpoints {
        let (before_end, after_end) = match &checkpoint {
            LineCheckpoint::Exact { before, after, .. } => (*before, *after),
            LineCheckpoint::Retained(block) => (block.before.start, block.after.start),
        };
        if before_start < before_end || after_start < after_end {
            events.push(LineEvent::Change {
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
                events.push(LineEvent::Context {
                    before,
                    after,
                    strong,
                });
                before_start = before + 1;
                after_start = after + 1;
            }
            LineCheckpoint::Retained(block) => {
                let unchanged_after = correspondence
                    .line_links_in(block.before.clone(), block.after.clone())
                    .map(|link| link.after)
                    .collect();
                before_start = block.before.end;
                after_start = block.after.end;
                events.push(LineEvent::Reflow {
                    before: block.before,
                    after: block.after,
                    unchanged_after,
                });
            }
        }
    }
    if before_start < before.end || after_start < after.end {
        events.push(LineEvent::Change {
            before: before_start..before.end,
            after: after_start..after.end,
        });
    }
    coalesce_weak_line_context(events)
}

fn exact_line_checkpoints(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    before: Range<usize>,
    after: Range<usize>,
) -> Vec<LineCheckpoint> {
    correspondence
        .line_links_in(before, after)
        .map(|link| {
            let strong =
                line_link_is_display_checkpoint(pair, correspondence, link.before, link.after);
            LineCheckpoint::Exact {
                before: link.before,
                after: link.after,
                strong,
            }
        })
        .collect()
}

/// Exact source text splits display blocks only with substantive neutral-leaf evidence.
fn line_link_is_display_checkpoint(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    before: usize,
    after: usize,
) -> bool {
    let Some(before_line) = pair.before.source.lines().get(before) else {
        return false;
    };
    let Some(after_line) = pair.after.source.lines().get(after) else {
        return false;
    };
    let mut leaves = pair
        .before
        .leaf_ids_in(before_line.content_bytes.clone())
        .filter(|leaf| {
            pair.before
                .node(*leaf)
                .leaf
                .is_some_and(|leaf| leaf.channel != ContentChannel::Layout)
        })
        .peekable();
    if leaves.peek().is_none() {
        return false;
    }
    leaves.any(|leaf| {
        let before_leaf = pair.before.node(leaf);
        let bytes = before_leaf.bytes.start.max(before_line.content_bytes.start)
            ..before_leaf.bytes.end.min(before_line.content_bytes.end);
        let substantive = pair
            .before
            .source
            .slice(bytes)
            .is_some_and(substantive_checkpoint_text);
        if !substantive {
            return false;
        }
        correspondence.before_leaf_link(leaf).is_some_and(|link| {
            if link.relation != LeafRelation::Exact
                || link.placement != Placement::Stable
                || link.reparented
            {
                return false;
            }
            let before = pair.before.node(link.before);
            let after = pair.after.node(link.after);
            ranges_overlap(&before.bytes, &before_line.content_bytes)
                && ranges_overlap(&after.bytes, &after_line.content_bytes)
        })
    })
}

fn substantive_checkpoint_text(text: &str) -> bool {
    text.chars()
        .any(|character| character.is_alphanumeric() || matches!(character, '_' | '\'' | '"'))
}

/// Weak exact rows remain context until a surrounding replacement claims their structure.
fn coalesce_weak_line_context(events: Vec<LineEvent>) -> Vec<LineEvent> {
    let mut coalesced = Vec::new();
    let mut flexible = Vec::new();
    for event in events {
        let strong = matches!(
            event,
            LineEvent::Context { strong: true, .. } | LineEvent::Reflow { .. }
        );
        if !strong {
            flexible.push(event);
            continue;
        }
        flush_flexible_line_events(&mut coalesced, &mut flexible);
        coalesced.push(event);
    }
    flush_flexible_line_events(&mut coalesced, &mut flexible);
    coalesced
}

fn flush_flexible_line_events(events: &mut Vec<LineEvent>, flexible: &mut Vec<LineEvent>) {
    let changes = flexible
        .iter()
        .enumerate()
        .filter_map(|(index, event)| matches!(event, LineEvent::Change { .. }).then_some(index))
        .collect::<Vec<_>>();
    if changes.is_empty() {
        events.append(flexible);
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
        events.extend_from_slice(&flexible[cursor..start]);
        events.push(merge_flexible_line_events(&flexible[start..end]));
        cursor = end;
    }
    events.extend_from_slice(&flexible[cursor..]);
}

fn merge_flexible_line_events(events: &[LineEvent]) -> LineEvent {
    let mut before = None;
    let mut after = None;
    for event in events {
        match event {
            LineEvent::Context {
                before: line,
                after: current,
                ..
            } => {
                include_index_range(&mut before, *line..*line + 1);
                include_index_range(&mut after, *current..*current + 1);
            }
            LineEvent::Change {
                before: removed,
                after: added,
            } => {
                include_index_range(&mut before, removed.clone());
                include_index_range(&mut after, added.clone());
            }
            LineEvent::Reflow { .. } => {
                unreachable!("retained blocks flush flexible line events")
            }
        }
    }
    LineEvent::Change {
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

fn plan_line_events(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    events: &[LineEvent],
    after_root: Option<NodeId>,
) -> Vec<Hunk> {
    let changes = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| event.is_signal().then_some(index))
        .collect::<Vec<_>>();
    let Some(first) = changes.first().copied() else {
        return Vec::new();
    };

    let mut hunks = Vec::new();
    let mut group_start = first;
    let mut group_end = first;
    for change in changes.into_iter().skip(1) {
        let separating_context = events[group_end + 1..change]
            .iter()
            .filter(|event| matches!(event, LineEvent::Context { .. }))
            .count();
        // A single omitted context row costs the same as separating the hunks.
        if context_gap_fits_halos(separating_context) {
            group_end = change;
            continue;
        }
        hunks.push(plan_line_hunk(
            pair,
            correspondence,
            events,
            group_start,
            group_end,
            after_root,
        ));
        group_start = change;
        group_end = change;
    }
    hunks.push(plan_line_hunk(
        pair,
        correspondence,
        events,
        group_start,
        group_end,
        after_root,
    ));
    hunks
}

fn plan_line_hunk(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    events: &[LineEvent],
    first_change: usize,
    last_change: usize,
    after_root: Option<NodeId>,
) -> Hunk {
    let is_context = |event: &LineEvent| matches!(event, LineEvent::Context { .. });
    let start = context_halo_start(events, first_change, is_context);
    let end = context_halo_end(events, last_change, is_context);
    let before_signals = line_event_signal_lines(&events[first_change..=last_change], true);
    let after_signals = line_event_signal_lines(&events[first_change..=last_change], false);
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
        context_event_for_after_line(events, line)
    });
    let selected =
        review_selection_ranges(start..end, breadcrumbs, |index| is_context(&events[index]));

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
            rows.push(DiffRow::Elision(line_event_coverage(
                &events[previous_end..range.start],
            )));
        }
        for event in &events[range.clone()] {
            append_line_event_rows(&mut rows, &mut coverage, pair, event);
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
        rows.push(DiffRow::Code {
            line: build_code_line(&pair.after, source, &[], DiffMark::Context),
            role: CodeRole::Context,
        });
        include_line(&mut coverage.after, line);
    }
    Hunk { coverage, rows }
}

/// Binary-search one exact current-world context event in the monotone stream.
fn context_event_for_after_line(events: &[LineEvent], line: usize) -> Option<usize> {
    let line = line.checked_sub(1)?;
    let index = events.partition_point(|event| line_event_after_end(event) <= line);
    matches!(
        events.get(index),
        Some(LineEvent::Context { after, .. }) if *after == line
    )
    .then_some(index)
}

fn line_event_after_end(event: &LineEvent) -> usize {
    match event {
        LineEvent::Context { after, .. } => after + 1,
        LineEvent::Change { after, .. } | LineEvent::Reflow { after, .. } => after.end,
    }
}

fn line_event_signal_lines(events: &[LineEvent], before_side: bool) -> HashSet<usize> {
    let mut lines = HashSet::new();
    for event in events {
        let range = match event {
            LineEvent::Change { before, after } | LineEvent::Reflow { before, after, .. } => {
                if before_side {
                    before
                } else {
                    after
                }
            }
            LineEvent::Context { .. } => continue,
        };
        lines.extend(range.clone().map(|line| line + 1));
    }
    lines
}

fn append_line_event_rows(
    rows: &mut Vec<DiffRow>,
    coverage: &mut LineCoverage,
    pair: &ProjectionPair<'_, '_>,
    event: &LineEvent,
) {
    match event {
        LineEvent::Context { before, after, .. } => {
            include_index_coverage(&mut coverage.before, *before..*before + 1);
            include_index_coverage(&mut coverage.after, *after..*after + 1);
            rows.push(DiffRow::Code {
                line: build_code_line(
                    &pair.after,
                    &pair.after.source.lines()[*after],
                    &[],
                    DiffMark::Context,
                ),
                role: CodeRole::Context,
            });
        }
        LineEvent::Change { before, after } => {
            include_index_coverage(&mut coverage.before, before.clone());
            include_index_coverage(&mut coverage.after, after.clone());
            append_line_change_rows(rows, pair, before.clone(), after.clone());
        }
        LineEvent::Reflow {
            before,
            after,
            unchanged_after,
        } => {
            include_index_coverage(&mut coverage.before, before.clone());
            include_index_coverage(&mut coverage.after, after.clone());
            append_retained_block_rows(rows, pair, after, unchanged_after);
        }
    }
}

fn line_event_coverage(events: &[LineEvent]) -> LineCoverage {
    let side_hull = |before_side| {
        let range = |event: &LineEvent| match event {
            LineEvent::Context { before, after, .. } => {
                let line = if before_side { before } else { after };
                *line..*line + 1
            }
            LineEvent::Change { before, after } | LineEvent::Reflow { before, after, .. } => {
                if before_side {
                    before.clone()
                } else {
                    after.clone()
                }
            }
        };
        let start = events
            .iter()
            .map(&range)
            .find(|range| !range.is_empty())?
            .start;
        let end = events
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
        let (before_code, after_code) = changed_code_lines(
            &pair.before,
            Some(before_line),
            &pair.after,
            Some(after_line),
        );
        rows.push(DiffRow::Linewise {
            before: before_code,
            after: after_code,
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
        rows.push(DiffRow::Linewise {
            before: Some(build_code_line(
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
        rows.push(DiffRow::Linewise {
            before: None,
            after: Some(build_code_line(
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

fn append_retained_block_rows(
    rows: &mut Vec<DiffRow>,
    pair: &ProjectionPair<'_, '_>,
    after: &Range<usize>,
    unchanged_after: &HashSet<usize>,
) {
    for index in after.clone() {
        rows.push(DiffRow::Code {
            line: build_code_line(
                &pair.after,
                &pair.after.source.lines()[index],
                &[],
                DiffMark::Context,
            ),
            role: if unchanged_after.contains(&index) {
                CodeRole::Context
            } else {
                CodeRole::Reflow
            },
        });
    }
}

#[derive(Clone, Debug)]
struct MarkedRange {
    bytes: Range<usize>,
    mark: DiffMark,
}

impl MarkedRange {
    fn new(bytes: Range<usize>, mark: DiffMark) -> Self {
        Self { bytes, mark }
    }
}

fn changed_code_lines(
    before: &Projection<'_>,
    before_line: Option<&SourceLine>,
    after: &Projection<'_>,
    after_line: Option<&SourceLine>,
) -> (Option<CodeLine>, Option<CodeLine>) {
    let (Some(before_line), Some(after_line)) = (before_line, after_line) else {
        let before = before_line.map(|line| {
            build_code_line(
                before,
                line,
                &[MarkedRange::new(
                    line.content_bytes.clone(),
                    DiffMark::Removed,
                )],
                DiffMark::Context,
            )
        });
        let after = after_line.map(|line| {
            build_code_line(
                after,
                line,
                &[MarkedRange::new(
                    line.content_bytes.clone(),
                    DiffMark::Added,
                )],
                DiffMark::Context,
            )
        });
        return (before, after);
    };

    let before_text = before.source.text(before_line);
    let after_text = after.source.text(after_line);
    if before_text == after_text {
        let before_mark = MarkedRange::new(before_line.content_bytes.clone(), DiffMark::Removed);
        let after_mark = MarkedRange::new(after_line.content_bytes.clone(), DiffMark::Added);
        return (
            Some(build_code_line(
                before,
                before_line,
                &[before_mark],
                DiffMark::Context,
            )),
            Some(build_code_line(
                after,
                after_line,
                &[after_mark],
                DiffMark::Context,
            )),
        );
    }

    let (before_changed, after_changed) = changed_byte_ranges(
        before_line.content_bytes.start,
        before_text,
        after_line.content_bytes.start,
        after_text,
    );
    let before_mark =
        (!before_changed.is_empty()).then(|| MarkedRange::new(before_changed, DiffMark::Removed));
    let after_mark =
        (!after_changed.is_empty()).then(|| MarkedRange::new(after_changed, DiffMark::Added));
    (
        Some(build_code_line(
            before,
            before_line,
            before_mark.as_slice(),
            DiffMark::Context,
        )),
        Some(build_code_line(
            after,
            after_line,
            after_mark.as_slice(),
            DiffMark::Context,
        )),
    )
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
    (
        before_start + byte_at_character(before, before_changed.start)
            ..before_start + byte_at_character(before, before_changed.end),
        after_start + byte_at_character(after, after_changed.start)
            ..after_start + byte_at_character(after, after_changed.end),
    )
}

fn byte_at_character(text: &str, character: usize) -> usize {
    text.char_indices()
        .nth(character)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

fn build_code_line(
    projection: &Projection<'_>,
    line: &SourceLine,
    marked: &[MarkedRange],
    default_mark: DiffMark,
) -> CodeLine {
    let bytes = line.content_bytes.clone();
    let leaves = projection.leaves_in(bytes.clone()).collect::<Vec<_>>();
    let mut boundaries = vec![bytes.start, bytes.end];
    for leaf in &leaves {
        boundaries.push(leaf.bytes.start.max(bytes.start));
        boundaries.push(leaf.bytes.end.min(bytes.end));
    }
    for range in marked {
        if !ranges_overlap(&range.bytes, &bytes) {
            continue;
        }
        boundaries.push(range.bytes.start.max(bytes.start));
        boundaries.push(range.bytes.end.min(bytes.end));
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut spans = Vec::new();
    for boundary in boundaries.windows(2) {
        let segment = boundary[0]..boundary[1];
        if segment.is_empty() {
            continue;
        }
        let text = projection
            .source
            .slice(segment.clone())
            .expect("line segments remain on UTF-8 boundaries");
        let syntax = leaves
            .iter()
            .find(|leaf| leaf.bytes.start <= segment.start && leaf.bytes.end >= segment.end)
            .and_then(|leaf| leaf.leaf)
            .map(|leaf| leaf.syntax)
            .unwrap_or(SyntaxClass::Plain);
        let mark = marked
            .iter()
            .find(|marked| marked.bytes.start <= segment.start && marked.bytes.end >= segment.end)
            .map(|marked| marked.mark)
            .unwrap_or(default_mark);
        push_span(&mut spans, text, syntax, mark);
    }
    CodeLine {
        number: line.number,
        spans,
    }
}

fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

fn push_span(spans: &mut Vec<CodeSpan>, text: &str, syntax: SyntaxClass, mark: DiffMark) {
    if text.is_empty() {
        return;
    }
    if let Some(previous) = spans.last_mut()
        && previous.syntax == syntax
        && previous.mark == mark
    {
        previous.text.push_str(text);
        return;
    }
    spans.push(CodeSpan {
        text: text.to_owned(),
        syntax,
        mark,
    });
}

fn build_code_lines(
    projection: &Projection<'_>,
    lines: Range<usize>,
    marked: &[MarkedRange],
    default_mark: DiffMark,
) -> Vec<CodeLine> {
    collect_source_lines(projection, lines)
        .into_iter()
        .map(|line| build_code_line(projection, line, marked, default_mark))
        .collect()
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
fn select_review_hunks(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
    rows: Vec<DiffRow>,
) -> Vec<Hunk> {
    if rows.is_empty() {
        return Vec::new();
    }

    let signals = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            (!matches!(
                row,
                DiffRow::Code {
                    role: CodeRole::Context,
                    ..
                }
            ))
            .then_some(index)
        })
        .collect::<Vec<_>>();
    let Some(first) = signals.first().copied() else {
        return Vec::new();
    };
    let context_rows = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| match row {
            DiffRow::Code {
                line,
                role: CodeRole::Context,
            } => Some((line.number, index)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();

    let mut clusters = Vec::new();
    let mut cluster_start = first;
    let mut cluster_end = first;
    for signal in signals.into_iter().skip(1) {
        // A single omitted context row costs the same as a hunk separator.
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
        .map(|cluster| select_review_hunk(pair, unit, &alignment, &rows, &context_rows, cluster))
        .collect()
}

fn select_review_hunk(
    pair: &ProjectionPair<'_, '_>,
    unit: &MatchedUnit,
    alignment: &LineAlignment<'_>,
    rows: &[DiffRow],
    context_rows: &HashMap<usize, usize>,
    cluster: Range<usize>,
) -> Hunk {
    let is_context = |row: &DiffRow| {
        matches!(
            row,
            DiffRow::Code {
                role: CodeRole::Context,
                ..
            }
        )
    };
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
        selected_rows.push(DiffRow::Code {
            line: build_code_line(&pair.after, source, &[], DiffMark::Context),
            role: CodeRole::Context,
        });
    }

    Hunk {
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

/// Preserve replacement boundaries before independently planned treatments coalesce.
fn group_rows(rows: Vec<DiffRow>) -> Vec<Vec<DiffRow>> {
    let mut groups = Vec::new();
    let mut rows = rows.into_iter().peekable();
    while let Some(row) = rows.next() {
        if !matches!(row, DiffRow::Linewise { .. }) {
            groups.push(vec![row]);
            continue;
        }

        let mut run = vec![row];
        while matches!(
            rows.peek(),
            Some(DiffRow::Linewise { .. } | DiffRow::LineEnding { .. })
        ) {
            run.push(rows.next().expect("peeked replacement row"));
        }
        groups.push(run);
    }
    groups
}

/// Multi-row replacements are revision blocks, never ordinal before/after pairs.
fn order_replacement_group(rows: Vec<DiffRow>) -> Vec<DiffRow> {
    let linewise = rows
        .iter()
        .filter(|row| matches!(row, DiffRow::Linewise { .. }))
        .count();
    if linewise <= 1 {
        return rows;
    }

    let mut before = Vec::new();
    let mut after = Vec::new();
    for row in rows {
        match row {
            DiffRow::Linewise {
                before: old,
                after: current,
            } => {
                before.extend(old.map(|line| DiffRow::Linewise {
                    before: Some(line),
                    after: None,
                }));
                after.extend(current.map(|line| DiffRow::Linewise {
                    before: None,
                    after: Some(line),
                }));
            }
            DiffRow::LineEnding {
                before: old,
                after: current,
            } => {
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

fn row_source_order(row: &DiffRow, alignment: &LineAlignment<'_>) -> AlignedOrder {
    row_after_source_line(row)
        .map(AlignedOrder::current)
        .or_else(|| row_before_source_line(row).map(|line| alignment.before_order(line)))
        .unwrap_or(AlignedOrder::LAST)
}

fn row_after_source_line(row: &DiffRow) -> Option<usize> {
    match row {
        DiffRow::Code { line, .. } => Some(line.number),
        DiffRow::Linewise { after, .. } => after.as_ref().map(|line| line.number),
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
        DiffRow::Linewise { before, .. } => before.as_ref().map(|line| line.number),
        DiffRow::Moved { before, .. } => *before,
        DiffRow::Wordwise(word) => word.before_line,
        DiffRow::Elision(coverage) => coverage.before.as_ref().map(|range| range.start),
        DiffRow::Code { .. } | DiffRow::LineEnding { .. } | DiffRow::FileBoundary => None,
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
    // Sparse structural context can reach EOF without widening semantic coverage.
    let context_reaches = matches!(
        hunk.rows.last(),
        Some(DiffRow::Code { line, .. }) if line.number == after_lines
    );
    coverage_reaches || context_reaches
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::correspondence::correspond;
    use crate::diff::projection::project_pair;
    use std::path::Path;

    fn planned(path: &str, before: &str, after: &str) -> Vec<Hunk> {
        let pair = project_pair(Path::new(path), before, after, false).unwrap();
        let correspondence = correspond(&pair);
        plan_hunks(&pair, &correspondence)
    }

    fn line_text(line: &CodeLine) -> String {
        line.spans.iter().map(|span| span.text.as_str()).collect()
    }

    fn current_line(row: &DiffRow) -> Option<&CodeLine> {
        match row {
            DiffRow::Code { line, .. } | DiffRow::Moved { after: line, .. } => Some(line),
            DiffRow::Linewise { after, .. } => after.as_ref(),
            DiffRow::LineEnding { .. }
            | DiffRow::Wordwise(_)
            | DiffRow::Elision(_)
            | DiffRow::FileBoundary => None,
        }
    }

    #[test]
    fn line_cst_keeps_context_and_the_terminal_boundary() {
        let hunks = planned("notes.txt", "one\nold\nthree\n", "one\nnew\nthree\n");

        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].coverage.before, Some(1..4));
        assert_eq!(hunks[0].coverage.after, Some(1..4));
        assert!(matches!(hunks[0].rows.last(), Some(DiffRow::FileBoundary)));
        assert!(hunks[0].rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Linewise {
                    before: Some(before),
                    after: Some(after),
                } if line_text(before) == "old" && line_text(after) == "new"
            )
        }));
    }

    #[test]
    fn retained_html_child_uses_after_indentation() {
        let before = "<article>\n  <img\n  src=\"ada.webp\"\n  />\n</article>\n";
        let after =
            "<article>\n  <div>\n    <img\n      src=\"ada.webp\"\n    />\n  </div>\n</article>\n";
        let hunks = planned("view.html", before, after);
        let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();

        assert!(rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Code {
                    line,
                    role: CodeRole::Reflow,
                } if line_text(line) == "      src=\"ada.webp\""
            )
        }));
        assert!(!rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Linewise { before, after }
                    if before.iter().chain(after).any(|line| line_text(line).contains("src=\"ada.webp\""))
            )
        }));
    }

    #[test]
    fn opaque_plaintext_body_stays_literal() {
        let before = "<plaintext>\n</plaintext>\n  <img>\n";
        let after = "<plaintext>\n</plaintext>\n  <div>\n    <img>\n  </div>\n";
        let hunks = planned("view.html", before, after);
        let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();

        assert!(
            rows.iter().any(|row| {
                matches!(
                    row,
                    DiffRow::Linewise { before: Some(line), .. }
                        if line_text(line).contains("<img>")
                )
            }),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|row| {
                matches!(
                    row,
                    DiffRow::Linewise { after: Some(line), .. }
                        if line_text(line).contains("<img>")
                )
            }),
            "{rows:#?}"
        );
    }

    #[test]
    fn changed_comment_is_a_linewise_channel_edit() {
        let hunks = planned(
            "lib.rs",
            "fn run() {\n    // old reason\n    work();\n}\n",
            "fn run() {\n    // new reason\n    work();\n}\n",
        );
        let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();

        assert!(
            rows.iter()
                .any(|row| matches!(row, DiffRow::Linewise { .. }))
        );
        assert!(!rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Code { line, .. } if line_text(line).contains("reason")
            )
        }));
    }

    #[test]
    fn comment_edit_does_not_hide_independent_reflow() {
        let before = "fn run() { // old reason\n    work(); }\n";
        let after = "fn run() {\n    // new reason\n    work();\n}\n";
        let hunks = planned("lib.rs", before, after);
        let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();

        assert!(
            rows.iter()
                .any(|row| matches!(row, DiffRow::Linewise { .. }))
        );
        assert!(rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Code {
                    role: CodeRole::Reflow,
                    ..
                }
            )
        }));
    }

    #[test]
    fn multiple_changed_comments_on_one_line_render_one_linewise_row() {
        let before = "fn run() { /* first */ /* second */ work(); }\n";
        let after = "fn run() { /* changed */ /* revised */ work(); }\n";
        let hunks = planned("lib.rs", before, after);
        let linewise = hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .filter(|row| matches!(row, DiffRow::Linewise { .. }))
            .count();

        assert_eq!(linewise, 1);
    }

    #[test]
    fn changed_top_level_comment_pairs_both_source_sides() {
        let hunks = planned(
            "lib.rs",
            "// old license\nfn run() {}\n",
            "// new license\nfn run() {}\n",
        );

        assert!(hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
            matches!(
                row,
                DiffRow::Linewise {
                    before: Some(before),
                    after: Some(after),
                } if line_text(before).contains("old") && line_text(after).contains("new")
            )
        }));
    }

    #[test]
    fn exact_reordered_unit_is_present_world_move() {
        let before = "fn first() {\n    first();\n}\n\nfn second() {\n    second();\n}\n";
        let after = "fn second() {\n    second();\n}\n\nfn first() {\n    first();\n}\n";
        let hunks = planned("lib.rs", before, after);

        assert!(
            hunks
                .iter()
                .flat_map(|hunk| &hunk.rows)
                .any(|row| matches!(row, DiffRow::Moved { .. })),
            "{hunks:#?}"
        );
    }

    #[test]
    fn reordered_subtree_inside_unit_is_not_rendered_as_all_context() {
        let before = "fn run() {\n    first();\n    second();\n}\n";
        let after = "fn run() {\n    second();\n    first();\n}\n";
        let hunks = planned("lib.rs", before, after);

        assert!(
            hunks
                .iter()
                .flat_map(|hunk| &hunk.rows)
                .filter_map(current_line)
                .flat_map(|line| &line.spans)
                .any(|span| span.mark == DiffMark::Added
                    && matches!(span.text.as_str(), "first" | "second"))
        );
    }

    #[test]
    fn exact_leaf_reparented_inside_unit_remains_visible() {
        let before = "fn run() { left(alpha); right(beta); }\n";
        let after = "fn run() { left(beta); right(alpha); }\n";
        let hunks = planned("lib.rs", before, after);

        assert!(hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
            let DiffRow::Code { line, .. } = row else {
                return false;
            };
            line.spans
                .iter()
                .any(|span| span.text == "alpha" && span.mark == DiffMark::Added)
        }));
    }

    #[test]
    fn compact_units_share_affixes() {
        let hunks = planned("lib.rs", "use crate::old_name;\n", "use crate::new_name;\n");
        let word = hunks.iter().flat_map(|hunk| &hunk.rows).find_map(|row| {
            let DiffRow::Wordwise(word) = row else {
                return None;
            };
            Some(word)
        });

        let word = word.expect("compact edit");
        assert_eq!(word.prefix, "use crate::");
        assert_eq!(word.removed, "old");
        assert_eq!(word.added, "new");
        assert_eq!(word.suffix, "_name;");
    }

    #[test]
    fn removed_syntax_is_never_hidden_in_a_current_only_row() {
        for (path, before, after, removed) in [
            (
                "lib.rs",
                "fn run() { let mut value = 1; }\n",
                "fn run() { let value = 1; }\n",
                "mut",
            ),
            (
                "view.ts",
                "function run() { old(); }\n",
                "function run() {}\n",
                "old",
            ),
            (
                "view.css",
                ".card { margin: 1px 2px; }\n",
                ".card { margin: 1px; }\n",
                "2",
            ),
        ] {
            let hunks = planned(path, before, after);
            assert!(
                hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
                    let DiffRow::Linewise {
                        before: Some(line), ..
                    } = row
                    else {
                        return false;
                    };
                    line_text(line).contains(removed)
                        && line.spans.iter().any(|span| {
                            span.mark == DiffMark::Removed && span.text.contains(removed)
                        })
                }),
                "{path}: {hunks:#?}"
            );
        }
    }

    #[test]
    fn overlapping_syntax_units_fall_back_to_one_physical_row() {
        let diff = crate::diff::diff_file(
            "lib.rs",
            "fn alpha() {} fn beta() {}\n",
            "fn alpha() { x(); } fn beta() { y(); }\n",
        )
        .expect("overlapping syntax units must safely reproject");
        let current = diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .filter_map(|row| match row {
                DiffRow::Code { line, .. } => Some(line),
                DiffRow::Linewise { after, .. } => after.as_ref(),
                _ => None,
            })
            .filter(|line| line.number == 1)
            .collect::<Vec<_>>();

        assert_eq!(current.len(), 1, "{:#?}", diff.hunks);
        assert!(line_text(current[0]).contains("x();"));
        assert!(line_text(current[0]).contains("y();"));
    }

    #[test]
    fn file_boundary_is_unique_and_globally_terminal() {
        let hunks = planned(
            "lib.rs",
            "use crate::old;\n\nfn end() { old(); }\n",
            "use crate::new;\n\nfn end() { new(); }\n",
        );
        let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();

        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, DiffRow::FileBoundary))
                .count(),
            1
        );
        assert!(matches!(rows.last(), Some(DiffRow::FileBoundary)));
        assert!(matches!(
            hunks.last().and_then(|hunk| hunk.rows.first()),
            Some(DiffRow::Wordwise(_))
        ));
    }

    #[test]
    fn adjacent_structural_hunks_share_one_context_halo() {
        let hunks = planned(
            "lib.rs",
            "fn first() { old(); }\n\nfn second() { old(); }\n",
            "fn first() { new(); }\n\nfn second() { new(); }\n",
        );
        let shared_blank = hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .filter(|row| {
                matches!(
                    row,
                    DiffRow::Code {
                        line,
                        role: CodeRole::Context,
                    } if line.number == 2
                )
            })
            .count();

        assert_eq!(hunks.len(), 1, "adjacent context halos must coalesce");
        assert_eq!(shared_blank, 1, "{hunks:#?}");
    }

    #[test]
    fn inline_signal_keeps_ancestor_headers_and_local_context() {
        let filler = (0..12)
            .map(|index| format!("        stable_{index}();\n"))
            .collect::<String>();
        let before = format!(
            concat!(
                "pub async fn attempt_turn_on_stream() -> Result<()> {{\n",
                "    prepare();\n",
                "    loop {{\n",
                "        outer_setup();\n",
                "{filler}",
                "        loop {{\n",
                "            settle();\n",
                "            let frame = read().await?;\n",
                "            match frame {{\n",
                "                Frame::Log(line) => show(line),\n",
                "                Frame::ToolCall => handle(),\n",
                "                Frame::Stop => break,\n",
                "                Frame::Request {{ .. }} => {{}}\n",
                "            }}\n",
                "            finish();\n",
                "        }}\n",
                "    }}\n",
                "}}\n",
            ),
            filler = filler,
        );
        let after = before.replace(
            "                Frame::Stop => break,\n",
            concat!(
                "                Frame::Stop => break,\n",
                "                Frame::Error(message) => return Err(eyre!(message)),\n",
            ),
        );

        let hunks = planned("turn.rs", &before, &after);
        let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();
        for context in [
            "pub async fn attempt_turn_on_stream",
            "match frame {",
            "Frame::Log",
            "Frame::ToolCall",
            "Frame::Stop",
            "Frame::Request",
            "finish();",
        ] {
            assert!(
                rows.iter().any(|row| {
                    matches!(
                        row,
                        DiffRow::Code { line, .. } if line_text(line).contains(context)
                    )
                }),
                "missing {context:?}: {hunks:#?}",
            );
        }
        assert_eq!(
            rows.iter()
                .filter(|row| {
                    matches!(
                        row,
                        DiffRow::Code { line, .. } if line_text(line).trim() == "loop {"
                    )
                })
                .count(),
            2,
            "both loop ancestors must be visible: {hunks:#?}",
        );
        assert!(rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Linewise {
                    before: None,
                    after: Some(line),
                } if line_text(line).contains("Frame::Error")
            )
        }));
        assert!(rows.iter().any(|row| matches!(row, DiffRow::Elision(_))));
        assert!(!rows.iter().any(|row| {
            matches!(row, DiffRow::Code { line, .. } if line_text(line).contains("stable_5"))
        }));

        let position = |number| {
            rows.iter()
                .position(|row| current_line(row).is_some_and(|line| line.number == number))
                .unwrap_or_else(|| panic!("missing current line {number}: {hunks:#?}"))
        };
        let hierarchy = [1, 3, 17, 20].map(position);
        assert!(
            hierarchy.windows(2).all(|pair| pair[0] < pair[1]),
            "hierarchy breadcrumbs must remain source ordered: {hunks:#?}",
        );
        for halo in [(1..=6).collect::<Vec<_>>(), (14..=27).collect::<Vec<_>>()] {
            let halo = halo.into_iter().map(position).collect::<Vec<_>>();
            assert!(
                halo.windows(2).all(|pair| pair[0] + 1 == pair[1]),
                "each hierarchy step needs its own contiguous context halo: {hunks:#?}",
            );
        }
        let local = [21, 22, 23, 24, 25, 26, 27].map(position);
        assert_eq!(hierarchy[3] + 1, local[0], "{hunks:#?}");
        assert!(
            local.windows(2).all(|pair| pair[0] + 1 == pair[1]),
            "three rows on either side of the signal must stay contiguous: {hunks:#?}",
        );
    }

    #[test]
    fn distant_inline_windows_repeat_their_callable_hierarchy() {
        let review = |context| {
            let stable = (0..context)
                .map(|index| format!("    stable_{index}();\n"))
                .collect::<String>();
            let before = format!(
                concat!(
                    "fn run() {{\n",
                    "    first(old_alpha);\n",
                    "{stable}",
                    "    second(old_beta);\n",
                    "}}\n",
                ),
                stable = stable,
            );
            let after = before
                .replace("old_alpha", "new_alpha")
                .replace("old_beta", "new_beta");
            planned("lib.rs", &before, &after)
        };

        assert_eq!(review(7).len(), 1, "touching inline halos must coalesce");
        assert_eq!(review(8).len(), 2, "distant inline halos must split");
        let hunks = review(18);

        assert_eq!(hunks.len(), 2, "{hunks:#?}");
        for (index, (present, absent)) in [("new_alpha", "new_beta"), ("new_beta", "new_alpha")]
            .into_iter()
            .enumerate()
        {
            assert!(hunks[index].rows.iter().any(|row| {
                current_line(row).is_some_and(|line| line_text(line).contains("fn run()"))
            }));
            assert!(hunks[index].rows.iter().any(|row| {
                current_line(row).is_some_and(|line| line_text(line).contains(present))
            }));
            assert!(!hunks[index].rows.iter().any(|row| {
                current_line(row).is_some_and(|line| line_text(line).contains(absent))
            }));
        }
    }

    #[test]
    fn multiline_replacement_renders_each_revision_as_one_run() {
        let hunks = planned(
            "notes.txt",
            "header\nthis\nwent\naway\ntail\n",
            "header\nand then\nthis came in\nno meatgrinder\ntail\n",
        );
        let sides = hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .filter_map(|row| match row {
                DiffRow::Linewise {
                    before: Some(_),
                    after: None,
                } => Some('-'),
                DiffRow::Linewise {
                    before: None,
                    after: Some(_),
                } => Some('+'),
                DiffRow::Linewise {
                    before: Some(_),
                    after: Some(_),
                } => Some('±'),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(sides, ['-', '-', '-', '+', '+', '+']);
    }

    #[test]
    fn unrelated_delimiter_cannot_split_a_structural_replacement() {
        let before = concat!(
            "fn run() {\n",
            "    let stdin = read();\n",
            "    let mut history = make_history(stdin);\n",
            "\n",
            "    // Build prompt from the arguments.\n",
            "    // Collect positional arguments.\n",
            "    let prompt = {\n",
            "        let mut args = std::env::args();\n",
            "        let _ = args.next();\n",
            "        args.collect::<Vec<_>>().join(\" \")\n",
            "    };\n",
            "    connect();\n",
            "}\n",
        );
        let after = concat!(
            "fn run() {\n",
            "    let stdin = read();\n",
            "    let system = match backend {\n",
            "        Backend::Old => OLD,\n",
            "        Backend::New => NEW,\n",
            "    };\n",
            "    let mut history = make_history(stdin, system);\n",
            "    connect();\n",
            "}\n",
        );

        let hunks = planned("run.rs", before, after);
        let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();
        let delimiter_sides = rows
            .iter()
            .filter_map(|row| match row {
                DiffRow::Linewise {
                    before: Some(line),
                    after: None,
                } if line_text(line).trim() == "};" => Some('-'),
                DiffRow::Linewise {
                    before: None,
                    after: Some(line),
                } if line_text(line).trim() == "};" => Some('+'),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(delimiter_sides, ['-', '+'], "{hunks:#?}");

        let replacement = rows
            .iter()
            .filter_map(|row| match row {
                DiffRow::Linewise {
                    before: Some(line),
                    after: None,
                } => Some(('-', line_text(line).trim().to_string())),
                DiffRow::Linewise {
                    before: None,
                    after: Some(line),
                } => Some(('+', line_text(line).trim().to_string())),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            replacement,
            [
                ('-', "let mut history = make_history(stdin);".to_string()),
                ('-', "".to_string()),
                ('-', "// Build prompt from the arguments.".to_string()),
                ('-', "// Collect positional arguments.".to_string()),
                ('-', "let prompt = {".to_string()),
                ('-', "let mut args = std::env::args();".to_string()),
                ('-', "let _ = args.next();".to_string()),
                ('-', "args.collect::<Vec<_>>().join(\" \")".to_string()),
                ('-', "};".to_string()),
                ('+', "let system = match backend {".to_string()),
                ('+', "Backend::Old => OLD,".to_string()),
                ('+', "Backend::New => NEW,".to_string()),
                ('+', "};".to_string()),
                (
                    '+',
                    "let mut history = make_history(stdin, system);".to_string()
                ),
            ]
        );

        let mut current_started = false;
        for row in rows {
            let DiffRow::Linewise { before, after } = row else {
                continue;
            };
            if after.is_some() {
                current_started = true;
            }
            assert!(
                !current_started || before.is_none(),
                "before rows must not resume after current rows: {hunks:#?}",
            );
        }
    }

    #[test]
    fn modified_expression_keeps_both_sides_and_its_owner() {
        let before = concat!(
            "fn make_history() {\n",
            "    let reasoning = reasoning();\n",
            "    let mut history = vec![Message::System(\n",
            "        SYSTEM_PREAMBLE\n",
            "            .replace(\"cutoff\", \"old\")\n",
            "            .replace(\"reasoning\", &reasoning),\n",
            "    )];\n",
            "}\n",
        );
        let after = before.replace("SYSTEM_PREAMBLE", "system_preamble");

        let hunks = planned("history.rs", before, &after);
        assert!(hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
            matches!(
                row,
                DiffRow::Linewise {
                    before: Some(before),
                    after: Some(after),
                } if line_text(before).contains("SYSTEM_PREAMBLE")
                    && line_text(after).contains("system_preamble")
            )
        }));
        for context in [
            "fn make_history",
            "let mut history = vec!",
            ".replace(\"cutoff\"",
            ".replace(\"reasoning\"",
        ] {
            assert!(
                hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
                    matches!(row, DiffRow::Code { line, .. } if line_text(line).contains(context))
                }),
                "missing {context:?}: {hunks:#?}",
            );
        }
    }

    #[test]
    fn unmatched_units_at_one_edit_gap_render_before_then_current() {
        let hunks = planned(
            "lib.rs",
            "use crate::old;\nfn stable() {}\n",
            "const NEW: u8 = 1;\nfn stable() {}\n",
        );
        let changes = hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .filter_map(|row| match row {
                DiffRow::Linewise {
                    before: Some(line),
                    after: None,
                } => Some(('-', line_text(line))),
                DiffRow::Linewise {
                    before: None,
                    after: Some(line),
                } => Some(('+', line_text(line))),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(hunks.len(), 1, "{hunks:#?}");
        assert_eq!(
            changes,
            [
                ('-', "use crate::old;".to_string()),
                ('+', "const NEW: u8 = 1;".to_string()),
            ]
        );
    }

    #[test]
    fn one_edit_gap_preserves_old_source_order_before_current() {
        let hunks = planned(
            "lib.rs",
            "use crate::old;\nfn run() {}\n",
            "const KEEP: u8 = 1;\n",
        );
        let changes = hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .filter_map(|row| match row {
                DiffRow::Linewise {
                    before: Some(line),
                    after: None,
                } => Some(('-', line.number, line_text(line))),
                DiffRow::Linewise {
                    before: None,
                    after: Some(line),
                } => Some(('+', line.number, line_text(line))),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(hunks.len(), 1, "{hunks:#?}");
        assert_eq!(
            changes,
            [
                ('-', 1, "use crate::old;".to_string()),
                ('-', 2, "fn run() {}".to_string()),
                ('+', 1, "const KEEP: u8 = 1;".to_string()),
            ],
        );
    }

    #[test]
    fn one_sided_compact_removal_keeps_current_file_context() {
        let hunks = planned(
            "lib.rs",
            concat!(
                "use crate::old;\n",
                "use crate::kept;\n",
                "\n",
                "fn run() {}\n",
                "fn nearby() {}\n",
                "fn far_one() {}\n",
                "fn far_two() {}\n",
            ),
            concat!(
                "use crate::kept;\n",
                "\n",
                "fn run() {}\n",
                "fn nearby() {}\n",
                "fn far_one() {}\n",
                "fn far_two() {}\n",
            ),
        );
        let rows = &hunks[0].rows;
        let removed = rows
            .iter()
            .position(|row| {
                matches!(
                    row,
                    DiffRow::Linewise {
                        before: Some(line),
                        after: None,
                    } if line_text(line).contains("crate::old")
                )
            })
            .expect("removed import");
        let kept = rows
            .iter()
            .position(|row| {
                matches!(
                    row,
                    DiffRow::Code {
                        line,
                        role: CodeRole::Context,
                    } if line.number == 1 && line_text(line).contains("crate::kept")
                )
            })
            .expect("remaining import context");

        assert!(removed < kept, "{hunks:#?}");
        assert!(rows.iter().any(|row| {
            matches!(row, DiffRow::Code { line, .. } if line.number == 2 && line_text(line).is_empty())
        }));
        assert!(rows.iter().any(|row| {
            matches!(row, DiffRow::Code { line, .. } if line.number == 3 && line_text(line).contains("fn run"))
        }));
        assert!(rows.iter().any(|row| {
            matches!(row, DiffRow::Code { line, .. } if line.number == 4 && line_text(line).contains("fn nearby"))
        }));
        assert!(!rows.iter().any(|row| {
            current_line(row).is_some_and(|line| {
                line.number >= 5
                    && (line_text(line).contains("far_one") || line_text(line).contains("far_two"))
            })
        }));
    }

    #[test]
    fn exact_stable_statement_remains_context_between_replacements() {
        let hunks = planned(
            "lib.rs",
            concat!(
                "fn run() {\n",
                "    old_before();\n",
                "    stable();\n",
                "    old_after();\n",
                "}\n",
            ),
            concat!(
                "fn run() {\n",
                "    new_before();\n",
                "    stable();\n",
                "    new_after();\n",
                "}\n",
            ),
        );
        let stable = hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .filter(|row| match row {
                DiffRow::Code {
                    line,
                    role: CodeRole::Context,
                } => line_text(line).contains("stable();"),
                DiffRow::Linewise { before, after } => before
                    .as_ref()
                    .into_iter()
                    .chain(after)
                    .any(|line| line_text(line).contains("stable();")),
                _ => false,
            })
            .collect::<Vec<_>>();

        assert!(matches!(stable.as_slice(), [DiffRow::Code { .. }]));
    }

    #[test]
    fn stable_delimiter_only_leaf_is_weak_in_a_parsed_cst() {
        let hunks = planned(
            "lib.rs",
            concat!(
                "fn run() {\n",
                "    let value = {\n",
                "        old_one();\n",
                "    };\n",
                "    old_two();\n",
                "}\n",
            ),
            concat!(
                "fn run() {\n",
                "    let value = {\n",
                "        new_one();\n",
                "    };\n",
                "    new_two();\n",
                "}\n",
            ),
        );
        let replacement = hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .filter_map(|row| match row {
                DiffRow::Linewise {
                    before: Some(line),
                    after: None,
                } => Some(('-', line_text(line).trim().to_string())),
                DiffRow::Linewise {
                    before: None,
                    after: Some(line),
                } => Some(('+', line_text(line).trim().to_string())),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            replacement,
            [
                ('-', "old_one();".to_string()),
                ('-', "};".to_string()),
                ('-', "old_two();".to_string()),
                ('+', "new_one();".to_string()),
                ('+', "};".to_string()),
                ('+', "new_two();".to_string()),
            ],
            "{hunks:#?}",
        );
    }

    #[test]
    fn multiline_leaf_checkpoint_strength_is_physical_line_local() {
        let source = "/* label\n};\n*/\nfn run() {}\n";
        let pair = project_pair(Path::new("lib.rs"), source, source, false).unwrap();
        let correspondence = correspond(&pair);

        assert!(line_link_is_display_checkpoint(
            &pair,
            &correspondence,
            0,
            0
        ));
        assert!(!line_link_is_display_checkpoint(
            &pair,
            &correspondence,
            1,
            1
        ));
        assert!(!line_link_is_display_checkpoint(
            &pair,
            &correspondence,
            2,
            2
        ));
    }

    #[test]
    fn weak_layout_rows_group_locally_but_never_unboundedly() {
        for (path, before, after) in [
            (
                "lib.rs",
                "fn run() {\n    old_one();\n\n    old_two();\n}\n",
                "fn run() {\n    new_one();\n\n    new_two();\n}\n",
            ),
            (
                "notes.txt",
                "old_one();\n\nold_two();\n",
                "new_one();\n\nnew_two();\n",
            ),
        ] {
            let hunks = planned(path, before, after);
            let blank = hunks
                .iter()
                .flat_map(|hunk| &hunk.rows)
                .filter_map(|row| match row {
                    DiffRow::Linewise {
                        before: Some(line),
                        after: None,
                    } => Some(('-', line_text(line).trim().to_string())),
                    DiffRow::Linewise {
                        before: None,
                        after: Some(line),
                    } => Some(('+', line_text(line).trim().to_string())),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                blank,
                [
                    ('-', "old_one();".to_string()),
                    ('-', "".to_string()),
                    ('-', "old_two();".to_string()),
                    ('+', "new_one();".to_string()),
                    ('+', "".to_string()),
                    ('+', "new_two();".to_string()),
                ],
                "{path}: {hunks:#?}",
            );
        }

        let compact = planned(
            "notes.txt",
            "old_one();\n};\nold_two();\n",
            "new_one();\n};\nnew_two();\n",
        );
        let compact = compact
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .filter_map(|row| match row {
                DiffRow::Linewise {
                    before: Some(line),
                    after: None,
                } => Some(('-', line_text(line))),
                DiffRow::Linewise {
                    before: None,
                    after: Some(line),
                } => Some(('+', line_text(line))),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            compact,
            [
                ('-', "old_one();".to_string()),
                ('-', "};".to_string()),
                ('-', "old_two();".to_string()),
                ('+', "new_one();".to_string()),
                ('+', "};".to_string()),
                ('+', "new_two();".to_string()),
            ]
        );

        for weak in ["};\n".repeat(30), "\n".repeat(30)] {
            let before = format!("old_one();\n{weak}old_two();\n");
            let after = format!("new_one();\n{weak}new_two();\n");
            let focused = planned("notes.txt", &before, &after);
            let replacements = focused
                .iter()
                .flat_map(|hunk| &hunk.rows)
                .filter(|row| matches!(row, DiffRow::Linewise { .. }))
                .count();

            assert_eq!(focused.len(), 2, "{focused:#?}");
            assert!(replacements < 30, "weak context expanded without bound");
        }

        for (weak_rows, expected_hunks) in [(7, 1), (8, 2)] {
            let weak = "};\n".repeat(weak_rows);
            let before = format!("old_one();\n{weak}old_two();\n");
            let after = format!("new_one();\n{weak}new_two();\n");
            let focused = planned("notes.txt", &before, &after);
            assert_eq!(focused.len(), expected_hunks, "{focused:#?}");
        }
    }

    #[test]
    fn mixed_move_hunk_still_completes_normal_edit_context() {
        let hunks = planned(
            "lib.rs",
            "fn first() { one(); }\nfn second() { two(); }\n",
            concat!(
                "fn second() { two(); }\n",
                "fn first() { one(); }\n",
                "use crate::new;\n",
            ),
        );
        assert_eq!(hunks.len(), 1, "{hunks:#?}");
        let rows = &hunks[0].rows;
        assert!(rows.iter().any(|row| matches!(
            row,
            DiffRow::Moved {
                before: Some(1),
                after,
            } if after.number == 2
        )));
        assert!(rows.iter().any(|row| matches!(
            row,
            DiffRow::Linewise {
                before: None,
                after: Some(line),
            } if line.number == 3 && line_text(line).contains("crate::new")
        )));
        assert!(
            rows.iter().any(|row| matches!(
                row,
                DiffRow::Code {
                    line,
                    role: CodeRole::Context,
                } if line.number == 1 && line_text(line).contains("fn second")
            )),
            "mixed move hunk lost current-file context: {hunks:#?}"
        );
    }

    #[test]
    fn current_bearing_move_and_replacement_groups_follow_current_source() {
        let hunks = planned(
            "lib.rs",
            concat!(
                "fn first() { one(); }\n",
                "fn second() { two(); }\n",
                "fn third() { three(); }\n",
            ),
            concat!(
                "fn third() { changed_three(); }\n",
                "fn second() { two(); }\n",
                "fn first() { one(); }\n",
            ),
        );
        let signals = hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .filter_map(|row| match row {
                DiffRow::Moved { after, .. }
                | DiffRow::Linewise {
                    after: Some(after), ..
                } => Some(after.number),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(signals, [1, 2, 3], "{hunks:#?}");
    }

    #[test]
    fn changed_callable_header_is_still_a_distant_body_breadcrumb() {
        let body = (0..12)
            .map(|index| format!("    stable_{index}();\n"))
            .collect::<String>();
        let before = format!("fn run(value: u8) {{\n{body}    old();\n}}\n");
        let after = format!("fn run() {{\n{body}    new();\n}}\n");
        let hunks = planned("lib.rs", &before, &after);
        let body = hunks
            .iter()
            .find(|hunk| {
                hunk.rows.iter().any(|row| {
                    matches!(
                        row,
                        DiffRow::Linewise {
                            after: Some(line),
                            ..
                        } if line_text(line).contains("new();")
                    )
                })
            })
            .expect("body replacement hunk");

        assert!(body.rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Code {
                    line,
                    role: CodeRole::Context,
                } if line.number == 1 && line_text(line).contains("fn run()")
            )
        }));
        assert!(
            body.rows
                .iter()
                .any(|row| matches!(row, DiffRow::Elision(_)))
        );
        for local in ["stable_9", "stable_10", "stable_11"] {
            assert!(body.rows.iter().any(|row| {
                matches!(row, DiffRow::Code { line, .. } if line_text(line).contains(local))
            }));
        }
        for breadcrumb_context in ["stable_0", "stable_1", "stable_2"] {
            assert!(body.rows.iter().any(|row| {
                matches!(row, DiffRow::Code { line, .. } if line_text(line).contains(breadcrumb_context))
            }));
        }
        assert!(!body.rows.iter().any(|row| {
            matches!(row, DiffRow::Code { line, .. } if line_text(line).contains("stable_3"))
        }));
    }

    #[test]
    fn history_shape_crosses_unit_boundaries_without_losing_source_order() {
        let before = concat!(
            "//! Extensions to handle lists of messages.\n",
            "use crate::prompting::SYSTEM_PREAMBLE;\n",
            "use crate::protocol::Message;\n",
            "\n",
            "/// Compose a full session history from the default preamble\n",
            "/// and optional stdin/extra contexts in the canonical order.\n",
            "pub fn make_history(\n",
            "    stdin_content: Option<String>,\n",
            "    stdout_redirection_path: Option<String>,\n",
            ") -> Vec<Message> {\n",
            "    let now = time::OffsetDateTime::now_local();\n",
            "    let now = now.date().to_string();\n",
            "    let reasoning = std::env::var(\"PLEASE_TRY\")\n",
            "        .ok()\n",
            "        .map(|v| v.trim().to_lowercase())\n",
            "        .and_then(|v| match v.as_str() {\n",
            "            _ if v.starts_with(\"h\") => Some(\"high\".to_string()),\n",
            "            _ if v.starts_with(\"m\") => Some(\"medium\".to_string()),\n",
            "            _ => None,\n",
            "        })\n",
            "        .unwrap_or_else(|| \"medium\".to_string());\n",
            "    let mut history = vec![Message::System(\n",
            "        SYSTEM_PREAMBLE\n",
            "            .replace(\"cutoff\", \"old\")\n",
            "            .replace(\"today\", &now)\n",
            "            .replace(\"reasoning\", &reasoning),\n",
            "    )];\n",
            "}\n",
        );
        let after = before
            .replace("use crate::prompting::SYSTEM_PREAMBLE;\n", "")
            .replace("default preamble", "selected backend preamble")
            .replace(
                "    stdout_redirection_path: Option<String>,\n",
                concat!(
                    "    stdout_redirection_path: Option<String>,\n",
                    "    system_preamble: &str,\n",
                ),
            )
            .replace("        SYSTEM_PREAMBLE\n", "        system_preamble\n");
        let hunks = planned("history.rs", before, &after);
        assert_eq!(
            hunks.len(),
            2,
            "adjacent top edits coalesce, while the distant use-site repeats its scope"
        );
        let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();
        let position = |predicate: &dyn Fn(&DiffRow) -> bool| {
            rows.iter()
                .position(|row| predicate(row))
                .expect("expected history row")
        };
        let module = position(&|row| matches!(row, DiffRow::Code { line, .. } if line.number == 1));
        let removed_import = position(&|row| {
            matches!(
                row,
                DiffRow::Linewise {
                    before: Some(line),
                    after: None,
                } if line_text(line).contains("SYSTEM_PREAMBLE;")
            )
        });
        let remaining_import = position(
            &|row| matches!(row, DiffRow::Code { line, .. } if line.number == 2 && line_text(line).contains("protocol::Message")),
        );
        let blank = position(
            &|row| matches!(row, DiffRow::Code { line, .. } if line.number == 3 && line_text(line).is_empty()),
        );
        let doc = position(&|row| {
            matches!(
                row,
                DiffRow::Linewise {
                    before: Some(before),
                    after: Some(after),
                } if line_text(before).contains("default preamble")
                    && line_text(after).contains("selected backend preamble")
            )
        });
        let continuation = position(
            &|row| matches!(row, DiffRow::Code { line, .. } if line.number == 5 && line_text(line).contains("optional stdin")),
        );
        let definition = position(
            &|row| matches!(row, DiffRow::Code { line, .. } if line.number == 6 && line_text(line).contains("make_history")),
        );
        let stdout = position(
            &|row| matches!(row, DiffRow::Code { line, .. } if line.number == 8 && line_text(line).contains("stdout_redirection_path")),
        );
        let parameter = position(&|row| {
            matches!(
                row,
                DiffRow::Linewise {
                    before: None,
                    after: Some(line),
                } if line.number == 9 && line_text(line).contains("system_preamble: &str")
            )
        });
        let signature_end = position(
            &|row| matches!(row, DiffRow::Code { line, .. } if line.number == 10 && line_text(line).contains(") -> Vec<Message>")),
        );

        assert!(module < removed_import);
        assert!(removed_import < remaining_import);
        assert!(remaining_import < blank);
        assert!(blank < doc);
        assert!(doc < continuation);
        assert!(continuation < definition);
        assert!(definition < stdout);
        assert_eq!(stdout + 1, parameter, "added parameter must follow stdout");
        assert_eq!(
            parameter + 1,
            signature_end,
            "signature must stay contiguous"
        );
        assert_eq!(
            rows.iter()
                .filter(|row| {
                    matches!(
                        row,
                        DiffRow::Code { line, .. }
                            if line_text(line).contains("pub fn make_history")
                    )
                })
                .count(),
            2,
            "each distant window needs its callable breadcrumb: {hunks:#?}",
        );

        let local_position = |number| {
            rows.iter()
                .position(|row| current_line(row).is_some_and(|line| line.number == number))
                .unwrap_or_else(|| panic!("missing history line {number}: {hunks:#?}"))
        };
        let local = [20, 21, 22, 23, 24, 25, 26].map(local_position);
        assert!(
            local.windows(2).all(|pair| pair[0] + 1 == pair[1]),
            "expression owner and chain must stay contiguous: {hunks:#?}",
        );
        assert!(matches!(
            rows[local[3]],
            DiffRow::Linewise {
                before: Some(_),
                after: Some(_),
            }
        ));
        let elision = rows
            .iter()
            .position(|row| matches!(row, DiffRow::Elision(_)))
            .expect("distant history context must remain folded");
        assert!(signature_end < elision && elision < local[0], "{hunks:#?}");
    }

    #[test]
    fn context_halo_coalescing_has_an_exact_seven_line_boundary() {
        let review = |context: usize| {
            let stable = (0..context)
                .map(|index| format!("// stable {index}\n"))
                .collect::<String>();
            planned(
                "lib.rs",
                &format!("fn first() {{ old(); }}\n{stable}fn second() {{ old(); }}\n"),
                &format!("fn first() {{ new(); }}\n{stable}fn second() {{ new(); }}\n"),
            )
        };

        let touching = review(7);
        assert_eq!(touching.len(), 1, "{touching:#?}");
        assert!(
            touching[0]
                .rows
                .iter()
                .all(|row| !matches!(row, DiffRow::Elision(_)))
        );

        let separate = review(8);
        assert_eq!(separate.len(), 2, "{separate:#?}");
    }

    #[test]
    fn multiline_compact_units_keep_physical_rows() {
        for (path, before, after) in [
            (
                "lib.rs",
                "use crate::{\n    Alpha,\n    Beta,\n};\n",
                "use crate::{\n    Alpha,\n    Beta,\n    Gamma,\n};\n",
            ),
            (
                "view.ts",
                "import {\n  Alpha,\n  Beta,\n} from \"pkg\";\n",
                "import {\n  Alpha,\n  Beta,\n  Gamma,\n} from \"pkg\";\n",
            ),
        ] {
            let hunks = planned(path, before, after);
            assert!(
                !hunks
                    .iter()
                    .flat_map(|hunk| &hunk.rows)
                    .any(|row| matches!(row, DiffRow::Wordwise(_)))
            );
            assert!(
                hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
                    matches!(
                        row,
                        DiffRow::Linewise {
                            after: Some(line),
                            ..
                        } if line_text(line).contains("Gamma")
                    )
                }),
                "{path}: {hunks:#?}"
            );
        }
    }

    #[test]
    fn jsx_wrapper_reuses_the_generic_reparented_subtree_treatment() {
        let before = "function View() {\n  return (\n    <article>\n      <img src=\"x\" />\n    </article>\n  );\n}\n";
        let after = "function View() {\n  return (\n    <article>\n      <div>\n        <img src=\"x\" />\n      </div>\n    </article>\n  );\n}\n";
        let hunks = planned("view.tsx", before, after);

        assert!(
            hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
                matches!(
                    row,
                    DiffRow::Code {
                        line,
                        role: CodeRole::Reflow,
                    } if line_text(line).trim_start().starts_with("<img")
                )
            }),
            "{hunks:#?}"
        );
        assert!(!hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
            matches!(
                row,
                DiffRow::Linewise { before, after }
                    if before.iter().chain(after).any(|line| line_text(line).contains("<img"))
            )
        }));
    }
}
