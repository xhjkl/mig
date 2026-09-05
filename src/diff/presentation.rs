//! Turn source coordinates into styled rows after refinement has fixed their order and coverage.

use super::context::ranges_overlap;
use super::refine::RefinedHunk;
use super::syntax::SyntaxTree;
use super::tree_diff::{
    CompactChange, LineEndingChange, MoveBlock, SourceChange, changed_sequence_ranges,
};
use super::{LineCoverage, LineEnding, SyntaxClass};
use std::ops::Range;

/// Change emphasis, independent of syntax coloring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffMark {
    Context,
    Removed,
    Added,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSpan {
    pub text: String,
    pub syntax: SyntaxClass,
    pub mark: DiffMark,
}

/// A one-based source line with its terminator excluded from the spans.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRow {
    pub number: usize,
    pub spans: Vec<SourceSpan>,
}

impl SourceRow {
    pub fn has_changes(&self) -> bool {
        self.spans.iter().any(|span| span.mark != DiffMark::Context)
    }
}

/// Compact wiring edit with its common prefix and suffix shown only once.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WordDiff {
    pub before_line: Option<usize>,
    pub after_line: Option<usize>,
    pub prefix: String,
    pub removed: String,
    pub added: String,
    pub suffix: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewRow {
    /// Current source; span marks distinguish edits from unchanged context.
    Current(SourceRow),
    /// Retained source whose layout changed, such as an indentation shift.
    Reflow(SourceRow),
    Removed(SourceRow),
    Added(SourceRow),
    LineEnding {
        before: Option<LineEnding>,
        after: Option<LineEnding>,
    },
    /// First or last visible row of a move; only the first carries the old line number.
    Moved {
        before: Option<usize>,
        after: SourceRow,
    },
    Wordwise(WordDiff),
    Elision(LineCoverage),
    /// File boundary included in the row stream so it scrolls with the source.
    FileBoundary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewHunk {
    pub coverage: LineCoverage,
    pub rows: Vec<ReviewRow>,
}

/// Styled hunks in review-priority order, which may differ from source order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresentedFile {
    pub path: String,
    /// Whether either source revision declares itself generated near its header.
    pub generated: bool,
    pub hunks: Vec<ReviewHunk>,
}

/// Split one source line at syntax and diff-mark boundaries.
fn source_row(
    tree: &SyntaxTree<'_>,
    number: usize,
    changed_ranges: &[Range<usize>],
    mark: DiffMark,
) -> SourceRow {
    let source = tree.source.line(number).expect("formed source line exists");
    let bytes = source.content_bytes.clone();
    let leaves = tree
        .leaf_ids_in(bytes.clone())
        .map(|id| tree.node(id))
        .collect::<Vec<_>>();
    let mut boundaries = vec![bytes.start, bytes.end];
    for leaf in &leaves {
        boundaries.push(leaf.bytes.start.max(bytes.start));
        boundaries.push(leaf.bytes.end.min(bytes.end));
    }
    for changed in changed_ranges {
        if !ranges_overlap(changed, &bytes) {
            continue;
        }
        boundaries.push(changed.start.max(bytes.start));
        boundaries.push(changed.end.min(bytes.end));
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut spans = Vec::new();
    for boundary in boundaries.windows(2) {
        let segment = boundary[0]..boundary[1];
        if segment.is_empty() {
            continue;
        }
        let text = tree
            .source
            .slice(segment.clone())
            .expect("line segments remain on UTF-8 boundaries");
        let syntax = leaves
            .iter()
            .find(|leaf| leaf.bytes.start <= segment.start && leaf.bytes.end >= segment.end)
            .and_then(|leaf| leaf.leaf)
            .map(|leaf| leaf.syntax)
            .unwrap_or(SyntaxClass::Plain);
        let changed = changed_ranges
            .iter()
            .any(|changed| changed.start <= segment.start && changed.end >= segment.end);
        let mark = if changed { mark } else { DiffMark::Context };
        push_span(&mut spans, text, syntax, mark);
    }
    SourceRow { number, spans }
}

fn push_span(spans: &mut Vec<SourceSpan>, text: &str, syntax: SyntaxClass, mark: DiffMark) {
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
    spans.push(SourceSpan {
        text: text.to_owned(),
        syntax,
        mark,
    });
}

fn word_diff(before: &SyntaxTree<'_>, after: &SyntaxTree<'_>, change: CompactChange) -> WordDiff {
    let before_text = change
        .before_bytes
        .and_then(|bytes| before.source.slice(bytes))
        .unwrap_or("");
    let after_text = change
        .after_bytes
        .and_then(|bytes| after.source.slice(bytes))
        .unwrap_or("");
    let before_chars = before_text.chars().collect::<Vec<_>>();
    let after_chars = after_text.chars().collect::<Vec<_>>();
    let (before_changed, after_changed) = changed_sequence_ranges(&before_chars, &after_chars);

    WordDiff {
        before_line: change.before_line,
        after_line: change.after_line,
        prefix: before_chars[..before_changed.start].iter().collect(),
        removed: before_chars[before_changed.clone()].iter().collect(),
        added: after_chars[after_changed].iter().collect(),
        suffix: before_chars[before_changed.end..].iter().collect(),
    }
}

/// Render refined changes while preserving their order and coverage.
pub fn present_hunks(
    before: &SyntaxTree<'_>,
    after: &SyntaxTree<'_>,
    hunks: Vec<RefinedHunk>,
) -> Vec<ReviewHunk> {
    let before_lines = before.source.lines().len();
    let after_lines = after.source.lines().len();
    let show_file_boundary = hunks.iter().any(|hunk| {
        hunk.coverage
            .after
            .as_ref()
            .is_some_and(|coverage| coverage.end == after_lines.saturating_add(1))
            || hunk
                .changes
                .last()
                .is_some_and(|change| change.displayed_after().last() == Some(after_lines))
            || hunk.coverage.after.is_none()
                && hunk
                    .coverage
                    .before
                    .as_ref()
                    .is_some_and(|coverage| coverage.end == before_lines.saturating_add(1))
    });
    // Placing the EOF marker last, even when review priority has reordered the hunks.
    let last_hunk = hunks.len().saturating_sub(1);
    hunks
        .into_iter()
        .enumerate()
        .map(|(index, hunk)| {
            let mut rows = hunk
                .changes
                .into_iter()
                .flat_map(|change| present_change(before, after, change))
                .collect::<Vec<_>>();
            if show_file_boundary && index == last_hunk {
                rows.push(ReviewRow::FileBoundary);
            }
            ReviewHunk {
                coverage: hunk.coverage,
                rows,
            }
        })
        .collect()
}

fn present_change(
    before_tree: &SyntaxTree<'_>,
    after_tree: &SyntaxTree<'_>,
    change: SourceChange,
) -> Vec<ReviewRow> {
    match change {
        SourceChange::Context(line) => vec![ReviewRow::Current(source_row(
            after_tree,
            line,
            &[],
            DiffMark::Context,
        ))],
        SourceChange::Edited(line) => vec![ReviewRow::Current(source_row(
            after_tree,
            line.number,
            &line.changed_bytes,
            DiffMark::Added,
        ))],
        SourceChange::Reflow(line) => vec![ReviewRow::Reflow(source_row(
            after_tree,
            line,
            &[],
            DiffMark::Context,
        ))],
        SourceChange::Replace {
            before,
            after,
            line_endings,
        } => {
            let mut rows = before
                .iter()
                .map(|line| {
                    ReviewRow::Removed(source_row(
                        before_tree,
                        line.number,
                        &line.changed_bytes,
                        DiffMark::Removed,
                    ))
                })
                .collect::<Vec<_>>();
            rows.extend(
                line_endings
                    .iter()
                    .filter(|change| change.after.is_none())
                    .copied()
                    .map(line_ending_row),
            );
            rows.extend(after.iter().map(|line| {
                ReviewRow::Added(source_row(
                    after_tree,
                    line.number,
                    &line.changed_bytes,
                    DiffMark::Added,
                ))
            }));
            rows.extend(
                line_endings
                    .into_iter()
                    .filter(|change| change.after.is_some())
                    .map(line_ending_row),
            );
            rows
        }
        SourceChange::LineEnding(change) => vec![line_ending_row(change)],
        SourceChange::Move(change) => move_rows(after_tree, change),
        SourceChange::Compact(change) => {
            vec![ReviewRow::Wordwise(word_diff(
                before_tree,
                after_tree,
                change,
            ))]
        }
        SourceChange::Elision(coverage) => vec![ReviewRow::Elision(coverage)],
    }
}

/// Compact moves around their endpoints, keeping terminator edits visible.
fn move_rows(after_tree: &SyntaxTree<'_>, change: MoveBlock) -> Vec<ReviewRow> {
    let mut lines = change.after.clone();
    let Some(first) = lines.next() else {
        return Vec::new();
    };
    let mut rows = vec![ReviewRow::Moved {
        before: Some(change.before.start),
        after: source_row(after_tree, first, &[], DiffMark::Context),
    }];
    rows.extend(
        change
            .line_endings
            .iter()
            .filter(|ending| ending.after.is_none())
            .copied()
            .map(line_ending_row),
    );
    rows.extend(
        change
            .line_endings
            .iter()
            .filter(|ending| ending.after.is_some_and(|endpoint| endpoint.line == first))
            .copied()
            .map(line_ending_row),
    );
    if change.after.len() == 1 {
        return rows;
    }

    let last = change.after.end - 1;
    let show_only_middle = (change.after.len() == 3).then_some(change.after.start + 1);
    let has_ending = |line| {
        change
            .line_endings
            .iter()
            .any(|ending| ending.after.is_some_and(|endpoint| endpoint.line == line))
    };
    let mut line = change.after.start + 1;
    while line < last {
        if show_only_middle == Some(line) || has_ending(line) {
            rows.push(ReviewRow::Current(source_row(
                after_tree,
                line,
                &[],
                DiffMark::Context,
            )));
            rows.extend(
                change
                    .line_endings
                    .iter()
                    .filter(|ending| ending.after.is_some_and(|endpoint| endpoint.line == line))
                    .copied()
                    .map(line_ending_row),
            );
            line += 1;
            continue;
        }

        let start = line;
        while line < last && show_only_middle != Some(line) && !has_ending(line) {
            line += 1;
        }
        let offset = start - change.after.start;
        let end_offset = line - change.after.start;
        rows.push(ReviewRow::Elision(LineCoverage {
            before: (change.before.len() == change.after.len())
                .then(|| change.before.start + offset..change.before.start + end_offset),
            after: Some(start..line),
        }));
    }
    rows.push(ReviewRow::Moved {
        before: None,
        after: source_row(after_tree, last, &[], DiffMark::Context),
    });
    rows.extend(
        change
            .line_endings
            .into_iter()
            .filter(|ending| ending.after.is_some_and(|endpoint| endpoint.line == last))
            .map(line_ending_row),
    );
    rows
}

fn line_ending_row(change: LineEndingChange) -> ReviewRow {
    ReviewRow::LineEnding {
        before: change.before.map(|endpoint| endpoint.ending),
        after: change.after.map(|endpoint| endpoint.ending),
    }
}
