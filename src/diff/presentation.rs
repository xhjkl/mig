//! Translate source-coordinate review facts into public, styled review rows.

use super::context::ranges_overlap;
use super::refine::RefinedHunk;
use super::syntax::SyntaxTree;
use super::tree_diff::{
    CompactChange, LineEndingChange, SelectedLine, SourceChange, changed_sequence_ranges,
};
use super::{LineCoverage, LineEnding, SyntaxClass};

/// Diff role layered over syntax styling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffMark {
    Context,
    Removed,
    Added,
}

/// Smallest independently styled slice of one displayed source line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSpan {
    pub text: String,
    pub syntax: SyntaxClass,
    pub mark: DiffMark,
}

/// One numbered source line materialized for review.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRow {
    pub number: usize,
    pub spans: Vec<SourceSpan>,
}

impl SourceRow {
    /// Whether this line carries an added or removed source span.
    pub fn has_changes(&self) -> bool {
        self.spans.iter().any(|span| span.mark != DiffMark::Context)
    }
}

/// One low-signal replacement compacted to its shared affixes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WordDiff {
    pub before_line: Option<usize>,
    pub after_line: Option<usize>,
    pub prefix: String,
    pub removed: String,
    pub added: String,
    pub suffix: String,
}

/// Presentation-ready row in a bounded diff hunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewRow {
    /// Current-world source; span marks distinguish changed payload from context.
    Current(SourceRow),
    /// Current-world source whose payload survived a physical-layout change.
    Reflow(SourceRow),
    /// One historical source row removed from the previous revision.
    Removed(SourceRow),
    /// One source row added to the current revision.
    Added(SourceRow),
    LineEnding {
        before: Option<LineEnding>,
        after: Option<LineEnding>,
    },
    Moved {
        before: Option<usize>,
        after: SourceRow,
    },
    Wordwise(WordDiff),
    Elision(LineCoverage),
    /// Ordered sentinel that makes the displayed file boundary part of the row stream.
    FileBoundary,
}

/// Bounded view into a file containing related, presentation-ready rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewHunk {
    pub coverage: LineCoverage,
    pub rows: Vec<ReviewRow>,
}

/// Presentation-ready stream of bounded hunks for one source file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresentedFile {
    pub path: String,
    /// Whether either source revision declares itself generated near its header.
    pub generated: bool,
    pub hunks: Vec<ReviewHunk>,
}

/// Split one source line at syntax and diff-mark boundaries.
fn source_row(tree: &SyntaxTree<'_>, line: &SelectedLine, mark: DiffMark) -> SourceRow {
    let source = tree
        .source
        .line(line.number)
        .expect("formed source line exists");
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
    for highlight in &line.highlights {
        if !ranges_overlap(highlight, &bytes) {
            continue;
        }
        boundaries.push(highlight.start.max(bytes.start));
        boundaries.push(highlight.end.min(bytes.end));
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
        let changed = line
            .highlights
            .iter()
            .any(|highlight| highlight.start <= segment.start && highlight.end >= segment.end);
        let mark = if changed { mark } else { DiffMark::Context };
        push_span(&mut spans, text, syntax, mark);
    }
    SourceRow {
        number: line.number,
        spans,
    }
}

/// Coalesce adjacent text with identical syntax and diff marks.
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

/// Materialize a compact source-coordinate replacement at the presentation boundary.
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

/// Materialize all refined source facts at the public presentation boundary.
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
        SourceChange::Current(line) => vec![ReviewRow::Current(source_row(
            after_tree,
            &line,
            DiffMark::Added,
        ))],
        SourceChange::Reflow(line) => vec![ReviewRow::Reflow(source_row(
            after_tree,
            &line,
            DiffMark::Context,
        ))],
        SourceChange::Replace {
            before,
            after,
            line_endings,
        } => {
            let mut rows = before
                .iter()
                .map(|line| ReviewRow::Removed(source_row(before_tree, line, DiffMark::Removed)))
                .collect::<Vec<_>>();
            rows.extend(
                line_endings
                    .iter()
                    .filter(|change| change.after.is_none())
                    .copied()
                    .map(line_ending_row),
            );
            rows.extend(
                after
                    .iter()
                    .map(|line| ReviewRow::Added(source_row(after_tree, line, DiffMark::Added))),
            );
            rows.extend(
                line_endings
                    .into_iter()
                    .filter(|change| change.after.is_some())
                    .map(line_ending_row),
            );
            rows
        }
        SourceChange::LineEnding(change) => vec![line_ending_row(change)],
        SourceChange::Moved {
            before,
            after: line,
        } => vec![ReviewRow::Moved {
            before,
            after: source_row(after_tree, &line, DiffMark::Context),
        }],
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

fn line_ending_row(change: LineEndingChange) -> ReviewRow {
    ReviewRow::LineEnding {
        before: change.before.map(|endpoint| endpoint.ending),
        after: change.after.map(|endpoint| endpoint.ending),
    }
}
