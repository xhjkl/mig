//! Refine source hunks into the ordered facts shown to reviewers.

use super::LineCoverage;
use super::context::{
    CONTEXT_HALO_RADIUS, breadcrumb_halo_indices, context_gap_fits_halos, context_halo_range,
    include_range, merge_windows_touch, ranges_overlap, review_selection_ranges,
};
use super::syntax::{NodeId, SyntaxPair, SyntaxTree};
use super::tree_diff::{
    ChangeNature, RawSourceDiff, SourceChange, SourceFact, SourceHunk, SourceLayout, SourceOrder,
    select_line,
};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// One prioritized source-space hunk with final coverage and semantic changes.
pub struct RefinedHunk {
    pub coverage: LineCoverage,
    pub changes: Vec<SourceChange>,
}

/// One source-located cluster with refinement-owned review geometry.
struct ReviewCluster {
    nature: ChangeNature,
    groups: Vec<Vec<SourceFact>>,
    halo_lines: Vec<usize>,
    order: SourceOrder,
    merge_window: Range<usize>,
}

impl ReviewCluster {
    fn meld(&mut self, mut later: Self) {
        debug_assert!(self.order <= later.order);
        if review_priority(later.nature) > review_priority(self.nature) {
            self.nature = later.nature;
        }
        self.groups.append(&mut later.groups);
        self.halo_lines.append(&mut later.halo_lines);
        self.merge_window.start = self.merge_window.start.min(later.merge_window.start);
        self.merge_window.end = self.merge_window.end.max(later.merge_window.end);
    }
}

/// Sheaf nearby source changes, apply review priority, and complete context.
pub fn refine_hunks(pair: &SyntaxPair<'_, '_>, raw: RawSourceDiff) -> Vec<RefinedHunk> {
    let RawSourceDiff {
        hunks,
        source_layout,
    } = raw;
    let hunks = hunks
        .into_iter()
        .flat_map(|hunk| split_source_hunk(pair, &source_layout, hunk))
        .collect::<Vec<_>>();
    let mut hunks = hunks
        .into_iter()
        .map(|hunk| place_hunk(hunk, &source_layout))
        .collect::<Vec<_>>();

    // Geometry is source-ordered so only neighboring context halos can merge.
    hunks.sort_by_key(coalescing_order);
    let mut hunks = coalesce_hunks(hunks);
    // Review priority applies only after nearby auxiliary facts are attached.
    hunks.sort_by_key(review_order);
    for hunk in &mut hunks {
        if !hunk.halo_lines.is_empty() {
            complete_context_halos(pair, &source_layout, hunk);
            complete_display_gaps(pair, &source_layout, hunk);
        }
        hunk.groups.sort_by_key(|group| {
            group
                .iter()
                .map(|fact| (fact.script_order, fact.order))
                .min()
                .expect("display group owns a source fact")
        });
    }
    deduplicate_context_rows(&mut hunks);

    hunks
        .into_iter()
        .map(|hunk| {
            let coverage = source_facts_coverage(hunk.groups.iter().flatten());
            let changes = hunk
                .groups
                .into_iter()
                .flatten()
                .map(|fact| fact.change)
                .collect();
            RefinedHunk { coverage, changes }
        })
        .collect()
}

/// Split one complete source hunk into local review regions.
fn split_source_hunk(
    pair: &SyntaxPair<'_, '_>,
    source_layout: &SourceLayout,
    hunk: SourceHunk,
) -> Vec<SourceHunk> {
    let signals = hunk
        .facts
        .iter()
        .enumerate()
        .filter_map(|(index, fact)| fact.change.has_signal().then_some(index))
        .collect::<Vec<_>>();
    let Some(first) = signals.first().copied() else {
        return Vec::new();
    };
    let context_rows = hunk
        .facts
        .iter()
        .enumerate()
        .filter_map(|(index, fact)| fact.change.context_line().map(|line| (line, index)))
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
        .map(|cluster| select_review_region(pair, source_layout, &hunk, &context_rows, cluster))
        .collect()
}

fn select_review_region(
    pair: &SyntaxPair<'_, '_>,
    source_layout: &SourceLayout,
    hunk: &SourceHunk,
    context_rows: &HashMap<usize, usize>,
    cluster: Range<usize>,
) -> SourceHunk {
    let context_halo = context_halo_range(&hunk.facts, cluster.clone(), |fact| {
        fact.change.is_context()
    });
    let mut after_signals = hunk.facts[cluster.clone()]
        .iter()
        .flat_map(|fact| fact.change.displayed_after())
        .collect::<HashSet<_>>();
    after_signals.extend(
        hunk.facts[cluster]
            .iter()
            .flat_map(|fact| fact.coverage.before.clone().into_iter().flatten())
            .filter_map(|line| source_layout.current_anchor(line)),
    );
    let mut hierarchy = hunk
        .context_root
        .map(|root| structural_context_lines(&pair.after, root, &after_signals))
        .unwrap_or_default();
    let breadcrumbs = breadcrumb_halo_indices(&hierarchy, |line| context_rows.get(&line).copied());
    let selected = review_selection_ranges(context_halo, breadcrumbs, |index| {
        hunk.facts[index].change.is_context()
    });

    let mut facts = Vec::new();
    let mut previous_end = None;
    for range in selected {
        if let Some(previous_end) = previous_end
            && previous_end < range.start
        {
            let coverage = source_facts_coverage(hunk.facts[previous_end..range.start].iter());
            if coverage.before.is_some() || coverage.after.is_some() {
                let script_order = minimum_script_order(&hunk.facts, hunk.order.after_gap);
                facts.push(elision_fact(coverage, hunk.order, script_order));
            }
        }
        facts.extend_from_slice(&hunk.facts[range.clone()]);
        previous_end = Some(range.end);
    }

    let shown = facts
        .iter()
        .flat_map(|fact| fact.change.displayed_after())
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
        facts.push(context_fact(select_line(source, &[]), source_layout));
    }

    let order = facts
        .iter()
        .filter(|fact| fact.change.has_signal())
        .map(|fact| fact.order)
        .min()
        .unwrap_or(hunk.order);

    SourceHunk {
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
        include_range(&mut coverage.before, fact.coverage.before.clone());
        include_range(&mut coverage.after, fact.coverage.after.clone());
    }
    coverage
}

fn signal_lines(facts: &[SourceFact]) -> (Vec<usize>, Vec<usize>) {
    let mut before = facts
        .iter()
        .filter(|fact| fact.change.has_signal())
        .flat_map(|fact| fact.coverage.before.clone().into_iter().flatten())
        .collect::<Vec<_>>();
    let mut after = facts
        .iter()
        .filter(|fact| fact.change.has_signal())
        .flat_map(|fact| fact.coverage.after.clone().into_iter().flatten())
        .collect::<Vec<_>>();
    before.sort_unstable();
    before.dedup();
    after.sort_unstable();
    after.dedup();
    (before, after)
}

fn minimum_script_order<'fact>(
    facts: impl IntoIterator<Item = &'fact SourceFact>,
    fallback: usize,
) -> usize {
    facts
        .into_iter()
        .map(|fact| fact.script_order)
        .min()
        .unwrap_or(fallback)
}

fn context_fact(line: super::tree_diff::SelectedLine, source_layout: &SourceLayout) -> SourceFact {
    let mut coverage = LineCoverage {
        before: None,
        after: Some(line.number..line.number + 1),
    };
    if let Some(before) = source_layout.aligned_before_line(line.number) {
        coverage.before = Some(before..before + 1);
    }
    let order = SourceOrder::current(line.number);
    let script_order = source_layout.script_order(line.number);
    SourceFact {
        change: SourceChange::Current(line),
        coverage,
        order,
        script_order,
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

/// Resolve one unranked hunk into current-world review geometry.
fn place_hunk(hunk: SourceHunk, source_layout: &SourceLayout) -> ReviewCluster {
    let (before_lines, after_lines) = signal_lines(&hunk.facts);
    let mapped_before = before_lines
        .iter()
        .filter_map(|line| source_layout.current_anchor(*line))
        .collect::<Vec<_>>();
    let focus = if let Some(line) = hunk.context_anchor {
        line..line + 1
    } else {
        let current_lines = if after_lines.is_empty() {
            &mapped_before
        } else {
            &after_lines
        };
        let lines = if current_lines.is_empty() {
            &before_lines
        } else {
            current_lines
        };
        let first = *lines
            .first()
            .expect("raw hunk owns current or before-side focus");
        let last = *lines
            .last()
            .expect("raw hunk owns current or before-side focus");
        first..last.saturating_add(1)
    };
    let mut halo_lines = match (receives_context(hunk.nature), hunk.context_anchor) {
        (false, _) => Vec::new(),
        (true, Some(line)) => vec![line],
        (true, None) => {
            let mut lines = after_lines;
            lines.extend(mapped_before);
            lines
        }
    };
    halo_lines.sort_unstable();
    halo_lines.dedup();
    ReviewCluster {
        nature: hunk.nature,
        groups: group_facts(hunk.facts),
        halo_lines,
        order: hunk.order,
        merge_window: merge_window(focus, hunk.nature),
    }
}

/// Keep one producer's material interval together while retaining atomic replacements.
fn group_facts(facts: Vec<SourceFact>) -> Vec<Vec<SourceFact>> {
    let Some(first) = facts.iter().position(|fact| fact.change.has_signal()) else {
        return facts.into_iter().map(|fact| vec![fact]).collect();
    };
    let last = facts
        .iter()
        .rposition(|fact| fact.change.has_signal())
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

/// Give each uninterrupted current-world source occurrence one final display owner.
fn deduplicate_context_rows(hunks: &mut Vec<ReviewCluster>) {
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

    let line = fact.change.context_line();
    let duplicate_context = line.is_some_and(|line| visible.contains(&line));
    if duplicate_context {
        return false;
    }
    visible.extend(fact.change.displayed_after());
    true
}

/// Complete each signal's three-line context halo against the whole current file.
fn complete_context_halos(
    pair: &SyntaxPair<'_, '_>,
    source_layout: &SourceLayout,
    hunk: &mut ReviewCluster,
) {
    let line_count = pair.after.source.lines().len();
    if line_count == 0 {
        return;
    }

    let mut signals = hunk.halo_lines.clone();
    signals.sort_unstable();
    signals.dedup();

    let mut shown = hunk
        .groups
        .iter()
        .flatten()
        .flat_map(|fact| fact.change.displayed_after())
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
                .push(vec![context_fact(select_line(line, &[]), source_layout)]);
        }
    }
}

/// Fill or explicitly fold gaps introduced by sparse hierarchy context.
fn complete_display_gaps(
    pair: &SyntaxPair<'_, '_>,
    source_layout: &SourceLayout,
    hunk: &mut ReviewCluster,
) {
    let mut displayed = hunk
        .groups
        .iter()
        .flatten()
        .flat_map(|fact| fact.change.displayed_after())
        .collect::<Vec<_>>();
    displayed.sort_unstable();
    displayed.dedup();
    let before_displayed = displayed
        .iter()
        .filter_map(|line| source_layout.aligned_before_line(*line))
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
    let script_order = minimum_script_order(hunk.groups.iter().flatten(), hunk.order.after_gap);

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
                .push(vec![context_fact(select_line(source, &[]), source_layout)]);
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

fn coalescing_order(hunk: &ReviewCluster) -> (usize, usize, Reverse<u8>) {
    (
        hunk.order.after_gap,
        hunk.order.tie_break,
        Reverse(review_priority(hunk.nature)),
    )
}

fn review_order(hunk: &ReviewCluster) -> (Reverse<u8>, usize, usize) {
    (
        Reverse(review_priority(hunk.nature)),
        hunk.order.after_gap,
        hunk.order.tie_break,
    )
}

fn coalesce_hunks(hunks: Vec<ReviewCluster>) -> Vec<ReviewCluster> {
    let mut coalesced: Vec<ReviewCluster> = Vec::new();
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
