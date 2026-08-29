//! Refine unranked source hunks into the ordered facts shown to reviewers.

use super::LineCoverage;
use super::context::{
    CONTEXT_HALO_RADIUS, breadcrumb_halo_indices, context_gap_fits_halos, context_halo_end,
    context_halo_start, merge_windows_touch, ranges_overlap, review_selection_ranges,
};
use super::syntax::{NodeId, SyntaxPair, SyntaxTree};
use super::tree_diff::{
    ChangeNature, RawHunk, RawHunks, SourceChange, SourceFact, SourceFocus, SourceMap, SourceOrder,
    select_line,
};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// Compiler-visible handoff from refinement to typed presentation.
pub(super) struct RefinedHunks(Vec<SourceHunk>);

impl RefinedHunks {
    pub(super) fn into_hunks(self) -> Vec<SourceHunk> {
        self.0
    }
}

/// One prioritized source-space hunk with no stale ranking metadata.
pub(super) struct SourceHunk {
    pub(super) coverage: LineCoverage,
    pub(super) facts: Vec<SourceFact>,
    pub(super) file_boundary: bool,
}

/// One source-located hunk after refinement assigns placement and review geometry.
struct PlacedHunk {
    nature: ChangeNature,
    groups: Vec<Vec<SourceFact>>,
    context_focus: SourceFocus,
    order: SourceOrder,
    merge_window: Range<usize>,
}

impl PlacedHunk {
    fn after_gap(&self) -> usize {
        self.order.after_gap
    }

    fn needs_context(&self) -> bool {
        !self.context_focus.is_empty()
    }

    fn meld(&mut self, mut later: Self) {
        debug_assert!(self.order <= later.order);
        if review_priority(later.nature) > review_priority(self.nature) {
            self.nature = later.nature;
        }
        self.groups.append(&mut later.groups);
        self.context_focus.merge(later.context_focus);
        self.merge_window.start = self.merge_window.start.min(later.merge_window.start);
        self.merge_window.end = self.merge_window.end.max(later.merge_window.end);
    }
}

/// Sheaf nearby source changes, apply review priority, and complete context.
pub(super) fn refine_hunks(pair: &SyntaxPair<'_, '_>, raw: RawHunks) -> RefinedHunks {
    let RawHunks {
        hunks,
        source_map,
        before_lines,
        after_lines,
    } = raw;
    let hunks = hunks
        .into_iter()
        .flat_map(|hunk| select_raw_hunks(pair, &source_map, hunk))
        .collect::<Vec<_>>();
    let mut hunks = hunks
        .into_iter()
        .map(|hunk| place_hunk(hunk, &source_map))
        .collect::<Vec<_>>();

    // Geometry is source-ordered so only neighboring context halos can merge.
    hunks.sort_by_key(coalescing_order);
    let mut hunks = coalesce_hunks(hunks);
    // Review priority applies only after nearby auxiliary facts are attached.
    hunks.sort_by_key(review_order);
    for hunk in &mut hunks {
        if hunk.needs_context() {
            complete_context_halos(pair, &source_map, hunk);
            complete_display_gaps(pair, &source_map, hunk);
        }
        order_groups(hunk);
    }
    deduplicate_context_rows(&mut hunks);

    let mut hunks = hunks
        .into_iter()
        .map(|hunk| SourceHunk {
            coverage: source_facts_coverage(hunk.groups.iter().flatten()),
            facts: hunk.groups.into_iter().flatten().collect(),
            file_boundary: false,
        })
        .collect::<Vec<_>>();
    append_file_boundary(&mut hunks, before_lines, after_lines);
    RefinedHunks(hunks)
}

/// Split one complete raw source region into local review excerpts.
fn select_raw_hunks(
    pair: &SyntaxPair<'_, '_>,
    source_map: &SourceMap,
    hunk: RawHunk,
) -> Vec<RawHunk> {
    let signals = hunk
        .facts
        .iter()
        .enumerate()
        .filter_map(|(index, fact)| fact.has_signal().then_some(index))
        .collect::<Vec<_>>();
    let Some(first) = signals.first().copied() else {
        return Vec::new();
    };
    let context_rows = hunk
        .facts
        .iter()
        .enumerate()
        .filter_map(|(index, fact)| fact.context_line().map(|line| (line, index)))
        .collect::<HashMap<_, _>>();

    let mut clusters = Vec::new();
    let mut cluster_start = first;
    let mut cluster_end = first;
    for signal in signals.into_iter().skip(1) {
        if context_gap_fits_halos(signal.saturating_sub(cluster_end + 1)) {
            cluster_end = signal;
            continue;
        }
        clusters.push(cluster_start..cluster_end + 1);
        cluster_start = signal;
        cluster_end = signal;
    }
    clusters.push(cluster_start..cluster_end + 1);

    clusters
        .into_iter()
        .map(|cluster| select_raw_hunk(pair, source_map, &hunk, &context_rows, cluster))
        .collect()
}

fn select_raw_hunk(
    pair: &SyntaxPair<'_, '_>,
    source_map: &SourceMap,
    hunk: &RawHunk,
    context_rows: &HashMap<usize, usize>,
    cluster: Range<usize>,
) -> RawHunk {
    let start = context_halo_start(&hunk.facts, cluster.start, SourceFact::is_context);
    let end = context_halo_end(&hunk.facts, cluster.end - 1, SourceFact::is_context);
    let mut after_signals = hunk.facts[cluster.clone()]
        .iter()
        .flat_map(|fact| fact.displayed_after.iter().copied())
        .collect::<HashSet<_>>();
    after_signals.extend(
        hunk.facts[cluster]
            .iter()
            .flat_map(|fact| coverage_lines(&fact.coverage.before))
            .filter_map(|line| source_map.current_anchor(line)),
    );
    let mut hierarchy = hunk
        .context_root
        .map(|root| structural_context_lines(&pair.after, root, &after_signals))
        .unwrap_or_default();
    let breadcrumbs = breadcrumb_halo_indices(&hierarchy, |line| context_rows.get(&line).copied());
    let selected = review_selection_ranges(start..end, breadcrumbs, |index| {
        hunk.facts[index].is_context()
    });

    let mut facts = Vec::new();
    let mut previous_end = None;
    for range in selected {
        if let Some(previous_end) = previous_end
            && previous_end < range.start
        {
            let coverage = source_facts_coverage(hunk.facts[previous_end..range.start].iter());
            if coverage.before.is_some() || coverage.after.is_some() {
                facts.push(elision_fact(coverage, hunk.order, hunk_order_rank(hunk)));
            }
        }
        facts.extend_from_slice(&hunk.facts[range.clone()]);
        previous_end = Some(range.end);
    }

    let shown = facts
        .iter()
        .flat_map(|fact| fact.displayed_after.iter().copied())
        .collect::<HashSet<_>>();
    hierarchy.retain(|line| !shown.contains(line));
    let mut hierarchy = hierarchy.into_iter().collect::<Vec<_>>();
    hierarchy.sort_unstable();
    for number in hierarchy {
        let source = pair
            .after
            .source
            .line(number)
            .expect("hierarchy line belongs to the current unit");
        facts.push(context_fact(select_line(source, &[]), source_map));
    }

    let order = facts
        .iter()
        .filter(|fact| fact.has_signal())
        .map(|fact| fact.order)
        .min()
        .unwrap_or(hunk.order);

    RawHunk {
        nature: hunk.nature,
        facts,
        order,
        context_anchor: hunk.context_anchor,
        context_root: hunk.context_root,
    }
}

fn source_facts_coverage<'fact>(
    facts: impl IntoIterator<Item = &'fact SourceFact>,
) -> LineCoverage {
    let mut coverage = LineCoverage {
        before: None,
        after: None,
    };
    for fact in facts {
        include_optional_coverage(&mut coverage.before, fact.coverage.before.clone());
        include_optional_coverage(&mut coverage.after, fact.coverage.after.clone());
    }
    coverage
}

fn coverage_lines(coverage: &Option<Range<usize>>) -> Range<usize> {
    coverage.clone().unwrap_or(0..0)
}

fn source_focus(facts: &[SourceFact]) -> SourceFocus {
    let mut before = facts
        .iter()
        .filter(|fact| fact.has_signal())
        .flat_map(|fact| coverage_lines(&fact.coverage.before))
        .collect::<Vec<_>>();
    let mut after = facts
        .iter()
        .filter(|fact| fact.has_signal())
        .flat_map(|fact| coverage_lines(&fact.coverage.after))
        .collect::<Vec<_>>();
    before.sort_unstable();
    before.dedup();
    after.sort_unstable();
    after.dedup();
    SourceFocus { before, after }
}

fn hunk_order_rank(hunk: &RawHunk) -> usize {
    hunk.facts
        .iter()
        .map(|fact| fact.script_order)
        .min()
        .unwrap_or(hunk.order.after_gap)
}

fn hunk_script_order(hunk: &PlacedHunk) -> usize {
    hunk.groups
        .iter()
        .flatten()
        .map(|fact| fact.script_order)
        .min()
        .unwrap_or(hunk.order.after_gap)
}

fn context_fact(line: super::tree_diff::SelectedLine, source_map: &SourceMap) -> SourceFact {
    let mut coverage = LineCoverage {
        before: None,
        after: Some(line.number..line.number + 1),
    };
    if let Some(before) = source_map.aligned_before_line(line.number) {
        coverage.before = Some(before..before + 1);
    }
    let order = SourceOrder::current(line.number);
    let script_order = source_map.script_order(line.number);
    let displayed_after = vec![line.number];
    SourceFact {
        change: SourceChange::Current(line),
        coverage,
        order,
        script_order,
        displayed_after,
    }
}

fn elision_fact(coverage: LineCoverage, fallback: SourceOrder, script_order: usize) -> SourceFact {
    let order = coverage
        .after
        .as_ref()
        .map(|lines| SourceOrder::current(lines.start))
        .unwrap_or(fallback);
    SourceFact {
        change: SourceChange::Elision(coverage.clone()),
        coverage,
        order,
        script_order,
        displayed_after: Vec::new(),
    }
}

/// Find neutral ancestor starts that explain one local signal.
fn structural_context_lines(
    tree: &SyntaxTree<'_>,
    root: NodeId,
    signals: &HashSet<usize>,
) -> HashSet<usize> {
    if signals.is_empty() {
        return HashSet::new();
    }

    let root_lines = tree.node(root).lines.clone();
    let signals = signals
        .iter()
        .copied()
        .filter(|line| root_lines.contains(line))
        .collect::<Vec<_>>();
    if signals.is_empty() {
        return HashSet::new();
    }

    let mut context = HashSet::new();
    if tree.node(root).leaf.is_none() && !root_lines.is_empty() {
        context.insert(root_lines.start);
    }
    for signal in signals {
        let Some(line) = tree.source.line(signal) else {
            continue;
        };
        for leaf in tree.leaf_ids_in(line.content_bytes.clone()) {
            let mut path = Vec::new();
            let mut ancestor = tree.node(leaf).parent;
            while let Some(id) = ancestor {
                let node = tree.node(id);
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

/// Place one raw source fact into current-world review geometry.
fn place_hunk(hunk: RawHunk, source_map: &SourceMap) -> PlacedHunk {
    let source_focus = source_focus(&hunk.facts);
    let anchor_focus = hunk.context_anchor.map(|line| line..line + 1);
    let focus = anchor_focus
        .or_else(|| source_map.current_focus(&source_focus))
        .or_else(|| line_hull(&source_focus.before))
        .expect("raw hunk owns current or before-side focus");
    let context_focus = match (receives_context(hunk.nature), hunk.context_anchor) {
        (false, _) => SourceFocus::default(),
        (true, Some(line)) => SourceFocus::after_line(line),
        (true, None) => source_focus,
    };
    PlacedHunk {
        nature: hunk.nature,
        groups: group_facts(hunk.facts),
        context_focus,
        order: hunk.order,
        merge_window: merge_window(focus, hunk.nature),
    }
}

/// Keep one producer's material interval together while retaining atomic replacements.
fn group_facts(facts: Vec<SourceFact>) -> Vec<Vec<SourceFact>> {
    let Some(first) = facts.iter().position(SourceFact::has_signal) else {
        return facts.into_iter().map(|fact| vec![fact]).collect();
    };
    let last = facts
        .iter()
        .rposition(SourceFact::has_signal)
        .expect("a first signal implies a last signal");
    let mut facts = facts;
    let trailing = facts.split_off(last + 1);
    let material = facts.split_off(first);
    let mut groups = facts.into_iter().map(|fact| vec![fact]).collect::<Vec<_>>();
    groups.push(material);
    groups.extend(trailing.into_iter().map(|fact| vec![fact]));
    groups
}

fn review_priority(nature: ChangeNature) -> u8 {
    match nature {
        ChangeNature::Reflow => 0,
        ChangeNature::Wiring => 1,
        ChangeNature::Move => 2,
        ChangeNature::Edit => 3,
    }
}

fn receives_context(nature: ChangeNature) -> bool {
    !matches!(nature, ChangeNature::Move)
}

fn merge_window(focus: Range<usize>, nature: ChangeNature) -> Range<usize> {
    let radius = if receives_context(nature) {
        CONTEXT_HALO_RADIUS
    } else {
        0
    };
    focus.start.saturating_sub(radius).max(1)..focus.end.saturating_add(radius)
}

fn line_hull(lines: &[usize]) -> Option<Range<usize>> {
    Some(*lines.first()?..lines.last()?.saturating_add(1))
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

fn order_groups(hunk: &mut PlacedHunk) {
    hunk.groups.sort_by_key(|group| {
        group
            .iter()
            .map(|fact| (fact.script_order, fact.order))
            .min()
            .expect("display group owns a source fact")
    });
}

/// Give each uninterrupted current-world source occurrence one final display owner.
fn deduplicate_context_rows(hunks: &mut Vec<PlacedHunk>) {
    let mut visible = HashSet::new();
    for hunk in hunks.iter_mut() {
        for group in &mut hunk.groups {
            group.retain(|row| claim_visible_context(row, &mut visible));
        }
    }

    let mut visible = HashSet::new();
    for hunk in hunks.iter_mut().rev() {
        for group in hunk.groups.iter_mut().rev() {
            let mut rows = std::mem::take(group);
            rows.reverse();
            rows.retain(|row| claim_visible_context(row, &mut visible));
            rows.reverse();
            *group = rows;
        }
        hunk.groups.retain(|group| !group.is_empty());
    }
    hunks.retain(|hunk| !hunk.groups.is_empty());
}

/// Retain one context owner until an explicit fold starts a new visible source run.
fn claim_visible_context(fact: &SourceFact, visible: &mut HashSet<usize>) -> bool {
    if matches!(fact.change, SourceChange::Elision(_)) {
        visible.clear();
        return true;
    }

    let line = fact.context_line();
    let duplicate_context = line.is_some_and(|line| visible.contains(&line));
    if duplicate_context {
        return false;
    }
    visible.extend(fact.displayed_after.iter().copied());
    true
}

/// Complete each signal's three-line context halo against the whole current file.
fn complete_context_halos(
    pair: &SyntaxPair<'_, '_>,
    source_map: &SourceMap,
    hunk: &mut PlacedHunk,
) {
    let line_count = pair.after.source.lines().len();
    if line_count == 0 {
        return;
    }

    let mut signals = hunk.context_focus.after.clone();
    signals.extend(
        hunk.context_focus
            .before
            .iter()
            .filter_map(|line| source_map.current_anchor(*line)),
    );
    signals.sort_unstable();
    signals.dedup();

    let mut shown = hunk
        .groups
        .iter()
        .flatten()
        .flat_map(|fact| fact.displayed_after.iter().copied())
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
            hunk.groups
                .push(vec![context_fact(select_line(line, &[]), source_map)]);
        }
    }
}

/// Fill or explicitly fold gaps introduced by sparse hierarchy context.
fn complete_display_gaps(pair: &SyntaxPair<'_, '_>, source_map: &SourceMap, hunk: &mut PlacedHunk) {
    let mut displayed = hunk
        .groups
        .iter()
        .flatten()
        .flat_map(|fact| fact.displayed_after.iter().copied())
        .collect::<Vec<_>>();
    displayed.sort_unstable();
    displayed.dedup();
    let before_displayed = displayed
        .iter()
        .filter_map(|line| source_map.aligned_before_line(*line))
        .collect::<Vec<_>>();
    trim_elisions_behind_context(&mut hunk.groups, &before_displayed, &displayed);
    let elisions = hunk
        .groups
        .iter()
        .flatten()
        .filter_map(|fact| match &fact.change {
            SourceChange::Elision(coverage) => coverage.after.clone(),
            _ => None,
        })
        .collect::<Vec<_>>();
    let script_order = hunk_script_order(hunk);

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
            hunk.groups
                .push(vec![context_fact(select_line(source, &[]), source_map)]);
            continue;
        }
        let coverage = LineCoverage {
            before: None,
            after: Some(omitted),
        };
        hunk.groups.push(vec![elision_fact(
            coverage,
            SourceOrder::current(lines[0].saturating_add(1)),
            script_order,
        )]);
    }
}

/// Remove folded coordinates already made visible by context.
fn trim_elisions_behind_context(
    groups: &mut Vec<Vec<SourceFact>>,
    before_displayed: &[usize],
    after_displayed: &[usize],
) {
    let mut trimmed = Vec::with_capacity(groups.len());
    for group in std::mem::take(groups) {
        let [fact] = group.as_slice() else {
            trimmed.push(group);
            continue;
        };
        let SourceChange::Elision(coverage) = &fact.change else {
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
            let coverage = LineCoverage {
                before: before.get(index).cloned(),
                after: after.get(index).cloned(),
            };
            trimmed.push(vec![elision_fact(coverage, fact.order, fact.script_order)]);
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

fn coalescing_order(hunk: &PlacedHunk) -> (usize, usize, Reverse<u8>) {
    (
        hunk.after_gap(),
        hunk.order.tie_break,
        Reverse(review_priority(hunk.nature)),
    )
}

fn review_order(hunk: &PlacedHunk) -> (Reverse<u8>, usize, usize) {
    (
        Reverse(review_priority(hunk.nature)),
        hunk.after_gap(),
        hunk.order.tie_break,
    )
}

fn coalesce_hunks(hunks: Vec<PlacedHunk>) -> Vec<PlacedHunk> {
    let mut coalesced: Vec<PlacedHunk> = Vec::new();
    for hunk in hunks {
        let Some(previous) = coalesced.last_mut() else {
            coalesced.push(hunk);
            continue;
        };
        if !merge_windows_touch(&previous.merge_window, &hunk.merge_window) {
            coalesced.push(hunk);
            continue;
        }
        previous.meld(hunk);
    }
    coalesced
}

fn append_file_boundary(hunks: &mut [SourceHunk], before_lines: usize, after_lines: usize) {
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
    if let Some(last) = hunks.last_mut() {
        last.file_boundary = true;
    }
}

fn hunk_reaches_after_boundary(hunk: &SourceHunk, after_lines: usize) -> bool {
    let coverage_reaches = hunk
        .coverage
        .after
        .as_ref()
        .is_some_and(|coverage| coverage.end == after_lines.saturating_add(1));
    let context_reaches = hunk
        .facts
        .last()
        .is_some_and(|fact| fact.displayed_after.last() == Some(&after_lines));
    coverage_reaches || context_reaches
}
