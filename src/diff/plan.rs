//! Presentation planning over parser-independent projection and correspondence facts.

use super::correspondence::{
    Correspondence, LeafRelation, MatchedUnit, NodeLink, Placement, UnitEdit,
};
use super::projection::{
    ContentChannel, Frame, NodeId, Projection, ProjectionPair, ReviewTreatment,
};
use super::source::SourceLine;
use super::{
    CodeLine, CodeRole, CodeSpan, DiffMark, DiffRow, Hunk, LineCoverage, LineEnding, SyntaxClass,
    WordDiff,
};
use std::collections::HashSet;
use std::ops::Range;

const LINE_CONTEXT: usize = 3;

/// Deliberate review cadence, independent of source position and rendered row variants.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum HunkPhase {
    Semantic,
    Move,
    Compact,
    Reflow,
}

struct PlannedHunk {
    phase: HunkPhase,
    hunk: Hunk,
}

impl PlannedHunk {
    fn new(phase: HunkPhase, hunk: Hunk) -> Self {
        Self { phase, hunk }
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

    planned.sort_by_key(hunk_order);
    let mut hunks = planned
        .into_iter()
        .map(|planned| planned.hunk)
        .collect::<Vec<_>>();
    deduplicate_context_rows(&mut hunks);
    append_file_boundary(
        &mut hunks,
        pair.before.source.lines().len(),
        pair.after.source.lines().len(),
    );
    hunks
}

/// A physical current-world context row belongs to the first displayed hunk that frames it.
fn deduplicate_context_rows(hunks: &mut Vec<Hunk>) {
    let mut seen = HashSet::new();
    for hunk in hunks.iter_mut() {
        hunk.rows.retain(|row| {
            let DiffRow::Code {
                line,
                role: CodeRole::Context,
            } = row
            else {
                return true;
            };
            seen.insert(line.number)
        });
    }
    hunks.retain(|hunk| !hunk.rows.is_empty());
}

fn semantic_hunks(hunks: Vec<Hunk>) -> Vec<PlannedHunk> {
    hunks
        .into_iter()
        .map(|hunk| PlannedHunk::new(HunkPhase::Semantic, hunk))
        .collect()
}

/// Keep semantic edits together, then moves, compact replacements, and pure reflow.
fn hunk_order(planned: &PlannedHunk) -> (HunkPhase, usize) {
    let line = planned
        .hunk
        .coverage
        .before
        .as_ref()
        .or(planned.hunk.coverage.after.as_ref())
        .map(|coverage| coverage.start)
        .unwrap_or(usize::MAX);
    (planned.phase, line)
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
                        plan_one_sided_compact(pair, *before, DiffMark::Removed),
                    )),
                    ReviewTreatment::Linewise => {
                        hunks.extend(semantic_hunks(vec![plan_one_sided_lines(
                            &pair.before,
                            node.lines.clone(),
                            DiffMark::Removed,
                        )]))
                    }
                    ReviewTreatment::Inline => {
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
                        plan_one_sided_compact(pair, *after, DiffMark::Added),
                    )),
                    ReviewTreatment::Linewise => {
                        hunks.extend(semantic_hunks(vec![plan_one_sided_lines(
                            &pair.after,
                            node.lines.clone(),
                            DiffMark::Added,
                        )]))
                    }
                    ReviewTreatment::Inline => {
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
            )));
        }
        ReviewTreatment::Inline => {
            if unit.relation.full_equal() {
                let hunk = plan_reflow(pair, correspondence, unit, review.frame);
                hunks.push(PlannedHunk::new(HunkPhase::Reflow, hunk));
                return;
            }

            let comments = comment_edits(pair, correspondence, unit);
            if unit.relation.code_equal() {
                if non_comment_lines_equal(pair, unit) {
                    hunks.extend(
                        comments
                            .into_iter()
                            .map(comment_hunk)
                            .map(|hunk| PlannedHunk::new(HunkPhase::Semantic, hunk)),
                    );
                } else {
                    let hunk = plan_reflow_with_comments(
                        pair,
                        correspondence,
                        unit,
                        review.frame,
                        comments,
                    );
                    hunks.push(PlannedHunk::new(HunkPhase::Semantic, hunk));
                }
                return;
            }
            if pair.before.identity_text(unit.before) != pair.after.identity_text(unit.after) {
                let line_hunks = plan_line_region(
                    pair,
                    correspondence,
                    Some(pair.before.node(unit.before).lines.clone()),
                    Some(after_node.lines.clone()),
                    correspondence.unit_composites(unit),
                );
                hunks.extend(semantic_hunks(line_hunks));
                return;
            }
            if has_retainable_reparented_block(pair, correspondence, unit) {
                let line_hunks = plan_line_region(
                    pair,
                    correspondence,
                    Some(pair.before.node(unit.before).lines.clone()),
                    Some(after_node.lines.clone()),
                    correspondence.unit_composites(unit),
                );
                hunks.extend(semantic_hunks(line_hunks));
                return;
            }
            if has_unmatched_before_content(pair, correspondence, unit) {
                let line_hunks = plan_line_region(
                    pair,
                    correspondence,
                    Some(pair.before.node(unit.before).lines.clone()),
                    Some(after_node.lines.clone()),
                    correspondence.unit_composites(unit),
                );
                hunks.extend(semantic_hunks(line_hunks));
                return;
            }
            let hunk = plan_inline(pair, correspondence, unit, review.frame, comments);
            hunks.push(PlannedHunk::new(HunkPhase::Semantic, hunk));
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
        let code = render_source_line(
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

fn plan_one_sided_compact(pair: &ProjectionPair<'_, '_>, unit: NodeId, mark: DiffMark) -> Hunk {
    let projection = if mark == DiffMark::Removed {
        &pair.before
    } else {
        &pair.after
    };
    let node = projection.node(unit);
    if !node_is_single_line(node) {
        return plan_one_sided_lines(projection, node.lines.clone(), mark);
    }
    if mark == DiffMark::Removed {
        return plan_compact(pair, Some(unit), None);
    }
    plan_compact(pair, None, Some(unit))
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
    let prefix = before
        .iter()
        .zip(&after)
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
    let before_end = before.len().saturating_sub(suffix);
    let after_end = after.len().saturating_sub(suffix);

    WordDiff {
        before_line,
        after_line,
        prefix: before[..prefix].iter().collect(),
        removed: before[prefix..before_end].iter().collect(),
        added: after[prefix..after_end].iter().collect(),
        suffix: before[before_end..].iter().collect(),
    }
}

fn plan_move(pair: &ProjectionPair<'_, '_>, unit: &MatchedUnit) -> Hunk {
    let before = pair.before.node(unit.before);
    let after = pair.after.node(unit.after);
    let coverage = LineCoverage {
        before: Some(before.lines.clone()),
        after: Some(after.lines.clone()),
    };
    let mut lines = render_lines(&pair.after, after.lines.clone(), &[], DiffMark::Context);
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
    _frame: Frame,
) -> Hunk {
    let before = pair.before.node(unit.before);
    let after = pair.after.node(unit.after);
    let before_lines = line_indices(Some(before.lines.clone()), pair.before.source.lines().len());
    let after_lines = line_indices(Some(after.lines.clone()), pair.after.source.lines().len());
    let exact_after = correspondence
        .line_links_in(before_lines, after_lines)
        .map(|link| link.after + 1)
        .collect::<HashSet<_>>();
    let rows = render_lines(&pair.after, after.lines.clone(), &[], DiffMark::Context)
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
    Hunk {
        coverage: LineCoverage {
            before: Some(before.lines.clone()),
            after: Some(after.lines.clone()),
        },
        rows: abbreviate_rows(rows),
    }
}

fn plan_reflow_with_comments(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
    frame: Frame,
    mut comments: Vec<CommentEdit>,
) -> Hunk {
    let before = pair.before.node(unit.before);
    let after = pair.after.node(unit.after);
    let changed_comment_lines = comments
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
                && !changed_comment_lines.contains(&(link.after + 1))
        })
        .map(|link| link.after + 1)
        .collect::<HashSet<_>>();

    comments.sort_by_key(comment_order);
    let mut comments = comments.into_iter().peekable();
    let mut rows = Vec::new();
    add_leading_frame(&pair.after, after.lines.clone(), frame, &mut rows);
    for line in render_lines(&pair.after, after.lines.clone(), &[], DiffMark::Context) {
        while comments
            .peek()
            .is_some_and(|comment| comment_order(comment) < line.number)
        {
            push_comment_rows(&mut rows, comments.next().expect("peeked comment edit"));
        }
        if changed_comment_lines.contains(&line.number) {
            continue;
        }
        let role = if exact_after.contains(&line.number) {
            CodeRole::Context
        } else {
            CodeRole::Reflow
        };
        rows.push(DiffRow::Code { line, role });
    }
    for comment in comments {
        push_comment_rows(&mut rows, comment);
    }
    add_trailing_frame(&pair.after, after.lines.clone(), frame, &mut rows);

    Hunk {
        coverage: LineCoverage {
            before: Some(before.lines.clone()),
            after: Some(after.lines.clone()),
        },
        rows: abbreviate_rows(rows),
    }
}

fn non_comment_lines_equal(pair: &ProjectionPair<'_, '_>, unit: &MatchedUnit) -> bool {
    let before_lines = comment_lines(&pair.before, unit.before);
    let after_lines = comment_lines(&pair.after, unit.after);
    let before = source_line_keys_without(
        &pair.before,
        pair.before.node(unit.before).lines.clone(),
        &before_lines,
    );
    let after = source_line_keys_without(
        &pair.after,
        pair.after.node(unit.after).lines.clone(),
        &after_lines,
    );
    let before = before.into_iter().map(|(_, key)| key).collect::<Vec<_>>();
    let after = after.into_iter().map(|(_, key)| key).collect::<Vec<_>>();
    before == after
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

fn source_line_keys_without<'source>(
    projection: &Projection<'source>,
    lines: Range<usize>,
    excluded: &HashSet<usize>,
) -> Vec<(usize, LineKey<'source>)> {
    collect_source_lines(projection, lines)
        .into_iter()
        .filter(|line| !excluded.contains(&line.number))
        .map(|line| {
            (
                line.number,
                LineKey {
                    text: projection.source.text(line),
                    ending: line.ending,
                },
            )
        })
        .collect()
}

fn plan_inline(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
    frame: Frame,
    mut comments: Vec<CommentEdit>,
) -> Hunk {
    let before = pair.before.node(unit.before);
    let after = pair.after.node(unit.after);
    let changed_comment_lines = comments
        .iter()
        .filter_map(|edit| edit.after.as_ref().map(|line| line.number))
        .collect::<HashSet<_>>();
    let mut changed_after = correspondence
        .unit_leaf_links(unit)
        .iter()
        .filter(|link| {
            link.relation == LeafRelation::Modified
                || link.placement == Placement::Reordered
                || link.reparented
        })
        .map(|link| link.after)
        .collect::<HashSet<_>>();
    for composite in correspondence
        .unit_composites(unit)
        .iter()
        .filter(|composite| composite.placement == Placement::Reordered)
    {
        changed_after.extend(descendant_leaves(&pair.after, composite.after));
    }
    changed_after.extend(descendant_leaves(&pair.after, unit.after).filter(|node| {
        pair.after.node(*node).leaf.is_some_and(|leaf| {
            !matches!(
                leaf.channel,
                ContentChannel::Comment | ContentChannel::Layout
            ) && correspondence.after_leaf_link(*node).is_none()
        })
    }));
    let marked = changed_after
        .into_iter()
        .filter_map(|leaf| {
            let node = pair.after.node(leaf);
            (!matches!(
                node.leaf?.channel,
                ContentChannel::Comment | ContentChannel::Layout
            ))
            .then_some(MarkedRange::new(node.bytes.clone(), DiffMark::Added))
        })
        .collect::<Vec<_>>();
    let lines = render_lines(&pair.after, after.lines.clone(), &marked, DiffMark::Context);
    comments.sort_by_key(comment_order);
    let mut comments = comments.into_iter().peekable();
    let mut rows = Vec::new();
    add_leading_frame(&pair.after, after.lines.clone(), frame, &mut rows);
    for line in lines {
        while comments
            .peek()
            .is_some_and(|comment| comment_order(comment) < line.number)
        {
            push_comment_rows(&mut rows, comments.next().expect("peeked comment edit"));
        }
        if changed_comment_lines.contains(&line.number) {
            continue;
        }
        let role = if line.spans.iter().any(|span| span.mark != DiffMark::Context) {
            CodeRole::Inline
        } else {
            CodeRole::Context
        };
        rows.push(DiffRow::Code { line, role });
    }
    for comment in comments {
        push_comment_rows(&mut rows, comment);
    }
    add_trailing_frame(&pair.after, after.lines.clone(), frame, &mut rows);

    Hunk {
        coverage: LineCoverage {
            before: Some(before.lines.clone()),
            after: Some(after.lines.clone()),
        },
        rows: abbreviate_rows(rows),
    }
}

/// One physical comment-line edit kept out of syntax-token emphasis.
struct CommentEdit {
    before: Option<CodeLine>,
    after: Option<CodeLine>,
    before_ending: Option<LineEnding>,
    after_ending: Option<LineEnding>,
}

fn comment_edits(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    unit: &MatchedUnit,
) -> Vec<CommentEdit> {
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
        push_changed_lines(
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
        push_changed_lines(
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
        push_changed_lines(
            &mut edits,
            &pair.before,
            None,
            &pair.after,
            Some(node.lines.clone()),
        );
    }
    edits.sort_by_key(comment_order);
    edits.dedup_by(|left, right| {
        left.before.as_ref().map(|line| line.number)
            == right.before.as_ref().map(|line| line.number)
            && left.after.as_ref().map(|line| line.number)
                == right.after.as_ref().map(|line| line.number)
    });
    edits
}

fn push_changed_lines(
    edits: &mut Vec<CommentEdit>,
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
) -> CommentEdit {
    let (before_code, after_code) = changed_code_lines(before, before_line, after, after_line);
    CommentEdit {
        before: before_code,
        after: after_code,
        before_ending: before_line.map(|line| line.ending),
        after_ending: after_line.map(|line| line.ending),
    }
}

fn comment_order(edit: &CommentEdit) -> usize {
    edit.after
        .as_ref()
        .or(edit.before.as_ref())
        .map(|line| line.number)
        .expect("a comment edit always has a source side")
}

fn comment_hunk(edit: CommentEdit) -> Hunk {
    let coverage = LineCoverage {
        before: edit
            .before
            .as_ref()
            .map(|line| line.number..line.number + 1),
        after: edit.after.as_ref().map(|line| line.number..line.number + 1),
    };
    let mut rows = Vec::new();
    push_comment_rows(&mut rows, edit);
    Hunk { coverage, rows }
}

fn push_comment_rows(rows: &mut Vec<DiffRow>, edit: CommentEdit) {
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
    )
}

#[derive(Clone, Debug)]
enum LineEvent {
    Context {
        before: usize,
        after: usize,
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
    Exact { before: usize, after: usize },
    Retained(RetainedBlock),
}

fn plan_line_region(
    pair: &ProjectionPair<'_, '_>,
    correspondence: &Correspondence,
    before_lines: Option<Range<usize>>,
    after_lines: Option<Range<usize>>,
    composites: &[NodeLink],
) -> Vec<Hunk> {
    let before = line_indices(before_lines, pair.before.source.lines().len());
    let after = line_indices(after_lines, pair.after.source.lines().len());
    let retained = retained_blocks(pair, composites, &before, &after);
    let events = line_events(correspondence, before, after, retained);
    plan_line_events(pair, &events)
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
            correspondence,
            before_start..block.before.start,
            after_start..block.after.start,
        ));
        before_start = block.before.end;
        after_start = block.after.end;
        checkpoints.push(LineCheckpoint::Retained(block));
    }
    checkpoints.extend(exact_line_checkpoints(
        correspondence,
        before_start..before.end,
        after_start..after.end,
    ));

    let mut events = Vec::new();
    let mut before_start = before.start;
    let mut after_start = after.start;
    for checkpoint in checkpoints {
        let (before_end, after_end) = match &checkpoint {
            LineCheckpoint::Exact { before, after } => (*before, *after),
            LineCheckpoint::Retained(block) => (block.before.start, block.after.start),
        };
        if before_start < before_end || after_start < after_end {
            events.push(LineEvent::Change {
                before: before_start..before_end,
                after: after_start..after_end,
            });
        }

        match checkpoint {
            LineCheckpoint::Exact { before, after } => {
                events.push(LineEvent::Context { before, after });
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
    events
}

#[derive(Eq, Hash, PartialEq)]
struct LineKey<'source> {
    text: &'source str,
    ending: LineEnding,
}

fn exact_line_checkpoints(
    correspondence: &Correspondence,
    before: Range<usize>,
    after: Range<usize>,
) -> Vec<LineCheckpoint> {
    correspondence
        .line_links_in(before, after)
        .map(|link| LineCheckpoint::Exact {
            before: link.before,
            after: link.after,
        })
        .collect()
}

fn plan_line_events(pair: &ProjectionPair<'_, '_>, events: &[LineEvent]) -> Vec<Hunk> {
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
        if separating_context <= LINE_CONTEXT * 2 + 1 {
            group_end = change;
            continue;
        }
        hunks.push(plan_line_hunk(pair, events, group_start, group_end));
        group_start = change;
        group_end = change;
    }
    hunks.push(plan_line_hunk(pair, events, group_start, group_end));
    hunks
}

fn plan_line_hunk(
    pair: &ProjectionPair<'_, '_>,
    events: &[LineEvent],
    first_change: usize,
    last_change: usize,
) -> Hunk {
    let start = context_event_start(events, first_change, LINE_CONTEXT);
    let end = context_event_end(events, last_change, LINE_CONTEXT);
    let mut coverage = LineCoverage {
        before: None,
        after: None,
    };
    let mut rows = Vec::new();
    for event in &events[start..end] {
        match event {
            LineEvent::Context { before, after } => {
                include_index_coverage(&mut coverage.before, *before..*before + 1);
                include_index_coverage(&mut coverage.after, *after..*after + 1);
                rows.push(DiffRow::Code {
                    line: render_source_line(
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
                render_line_change(&mut rows, pair, before.clone(), after.clone());
            }
            LineEvent::Reflow {
                before,
                after,
                unchanged_after,
            } => {
                include_index_coverage(&mut coverage.before, before.clone());
                include_index_coverage(&mut coverage.after, after.clone());
                render_retained_block(&mut rows, pair, after, unchanged_after);
            }
        }
    }
    Hunk { coverage, rows }
}

fn context_event_start(events: &[LineEvent], signal: usize, context: usize) -> usize {
    let mut index = signal;
    let mut retained = 0;
    while index > 0 && retained < context {
        index -= 1;
        if matches!(events[index], LineEvent::Context { .. }) {
            retained += 1;
            continue;
        }
        break;
    }
    index
}

fn context_event_end(events: &[LineEvent], signal: usize, context: usize) -> usize {
    let mut index = signal + 1;
    let mut retained = 0;
    while index < events.len() && retained < context {
        if matches!(events[index], LineEvent::Context { .. }) {
            retained += 1;
            index += 1;
            continue;
        }
        break;
    }
    index
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

fn render_line_change(
    rows: &mut Vec<DiffRow>,
    pair: &ProjectionPair<'_, '_>,
    before: Range<usize>,
    after: Range<usize>,
) {
    let paired = before.len().min(after.len());
    for offset in 0..paired {
        let before_line = &pair.before.source.lines()[before.start + offset];
        let after_line = &pair.after.source.lines()[after.start + offset];
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
    }
    for index in before.start + paired..before.end {
        let line = &pair.before.source.lines()[index];
        rows.push(DiffRow::Linewise {
            before: Some(render_source_line(
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
    for index in after.start + paired..after.end {
        let line = &pair.after.source.lines()[index];
        rows.push(DiffRow::Linewise {
            before: None,
            after: Some(render_source_line(
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

fn render_retained_block(
    rows: &mut Vec<DiffRow>,
    pair: &ProjectionPair<'_, '_>,
    after: &Range<usize>,
    unchanged_after: &HashSet<usize>,
) {
    for index in after.clone() {
        rows.push(DiffRow::Code {
            line: render_source_line(
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
            render_source_line(
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
            render_source_line(
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
            Some(render_source_line(
                before,
                before_line,
                &[before_mark],
                DiffMark::Context,
            )),
            Some(render_source_line(
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
        Some(render_source_line(
            before,
            before_line,
            before_mark.as_slice(),
            DiffMark::Context,
        )),
        Some(render_source_line(
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
    let prefix = before
        .chars()
        .zip(after.chars())
        .take_while(|(before, after)| before == after)
        .count();
    let before_characters = before.chars().count();
    let after_characters = after.chars().count();
    let suffix_budget = before_characters
        .min(after_characters)
        .saturating_sub(prefix);
    let suffix = before
        .chars()
        .rev()
        .zip(after.chars().rev())
        .take(suffix_budget)
        .take_while(|(before, after)| before == after)
        .count();
    let before_prefix = byte_at_character(before, prefix);
    let after_prefix = byte_at_character(after, prefix);
    let before_suffix = byte_at_character(before, before_characters.saturating_sub(suffix));
    let after_suffix = byte_at_character(after, after_characters.saturating_sub(suffix));
    (
        before_start + before_prefix..before_start + before_suffix,
        after_start + after_prefix..after_start + after_suffix,
    )
}

fn byte_at_character(text: &str, character: usize) -> usize {
    text.char_indices()
        .nth(character)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

fn render_source_line(
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

fn render_lines(
    projection: &Projection<'_>,
    lines: Range<usize>,
    marked: &[MarkedRange],
    default_mark: DiffMark,
) -> Vec<CodeLine> {
    collect_source_lines(projection, lines)
        .into_iter()
        .map(|line| render_source_line(projection, line, marked, default_mark))
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

fn add_leading_frame(
    projection: &Projection<'_>,
    lines: Range<usize>,
    frame: Frame,
    rows: &mut Vec<DiffRow>,
) {
    if frame != Frame::AdjacentBlankLines {
        return;
    }
    let Some(number) = lines.start.checked_sub(1) else {
        return;
    };
    let Some(line) = blank_line(projection, number) else {
        return;
    };
    rows.insert(
        0,
        DiffRow::Code {
            line,
            role: CodeRole::Context,
        },
    );
}

fn add_trailing_frame(
    projection: &Projection<'_>,
    lines: Range<usize>,
    frame: Frame,
    rows: &mut Vec<DiffRow>,
) {
    if frame != Frame::AdjacentBlankLines {
        return;
    }
    let Some(line) = blank_line(projection, lines.end) else {
        return;
    };
    rows.push(DiffRow::Code {
        line,
        role: CodeRole::Context,
    });
}

fn blank_line(projection: &Projection<'_>, number: usize) -> Option<CodeLine> {
    let line = projection.source.line(number)?;
    if !projection.source.text(line).trim().is_empty() {
        return None;
    }
    Some(render_source_line(projection, line, &[], DiffMark::Context))
}

/// Preserve the frame and every signal row; fold only distant current-world context.
fn abbreviate_rows(rows: Vec<DiffRow>) -> Vec<DiffRow> {
    if rows.len() <= 4 {
        return rows;
    }
    let mut keep = vec![false; rows.len()];
    for keep in keep.iter_mut().take(2) {
        *keep = true;
    }
    for keep in keep.iter_mut().rev().take(2) {
        *keep = true;
    }
    for (index, row) in rows.iter().enumerate() {
        if !matches!(
            row,
            DiffRow::Code {
                role: CodeRole::Context,
                ..
            }
        ) {
            keep[index] = true;
        }
    }

    let mut abbreviated = Vec::new();
    let mut index = 0;
    while index < rows.len() {
        if keep[index] {
            abbreviated.push(rows[index].clone());
            index += 1;
            continue;
        }
        let start = index;
        while index < rows.len() && !keep[index] {
            index += 1;
        }
        let omitted = &rows[start..index];
        if omitted.len() == 1 {
            abbreviated.push(omitted[0].clone());
            continue;
        }

        let after_start = omitted.iter().filter_map(row_after_line).min();
        let after_end = omitted
            .iter()
            .filter_map(row_after_line)
            .max()
            .map(|line| line + 1);
        abbreviated.push(DiffRow::Elision(LineCoverage {
            before: None,
            after: after_start.zip(after_end).map(|(start, end)| start..end),
        }));
    }
    abbreviated
}

fn row_after_line(row: &DiffRow) -> Option<usize> {
    match row {
        DiffRow::Code { line, .. } => Some(line.number),
        DiffRow::Linewise { before, after } => {
            after.as_ref().or(before.as_ref()).map(|line| line.number)
        }
        DiffRow::LineEnding { .. } => None,
        DiffRow::Moved { after, .. } => Some(after.number),
        DiffRow::Wordwise(word) => word.after_line,
        DiffRow::Elision(coverage) => coverage.after.as_ref().map(|range| range.start),
        DiffRow::FileBoundary => None,
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
    // Structural frames can carry visible current source to EOF without widening semantics.
    let frame_reaches = matches!(
        hunk.rows.last(),
        Some(DiffRow::Code { line, .. }) if line.number == after_lines
    );
    coverage_reaches || frame_reaches
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
                .filter_map(|row| {
                    let DiffRow::Code { line, .. } = row else {
                        return None;
                    };
                    Some(line)
                })
                .flat_map(|line| &line.spans)
                .any(|span| span.mark == DiffMark::Added)
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
    fn adjacent_structural_hunks_share_one_context_frame() {
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

        assert_eq!(shared_blank, 1, "{hunks:#?}");
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
