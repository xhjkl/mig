//! Syntax-aware materialization of source-space planner decisions.

use super::context::ranges_overlap;
use super::projection::Projection;
use super::source::SourceLine;
use super::{DiffMark, DisplayLine, DisplaySpan, SyntaxClass};
use std::ops::Range;

/// Absolute source bytes carrying one diff role during line materialization.
#[derive(Clone, Debug)]
pub(super) struct MarkedRange {
    bytes: Range<usize>,
    mark: DiffMark,
}

impl MarkedRange {
    /// One absolute source interval to layer over syntax styling.
    pub(super) fn new(bytes: Range<usize>, mark: DiffMark) -> Self {
        Self { bytes, mark }
    }
}

/// Split one source line at syntax and diff-mark boundaries.
pub(super) fn build_display_line(
    projection: &Projection<'_>,
    line: &SourceLine,
    marked: &[MarkedRange],
    default_mark: DiffMark,
) -> DisplayLine {
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
    DisplayLine {
        number: line.number,
        spans,
    }
}

/// Coalesce adjacent text with identical syntax and diff marks.
fn push_span(spans: &mut Vec<DisplaySpan>, text: &str, syntax: SyntaxClass, mark: DiffMark) {
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
    spans.push(DisplaySpan {
        text: text.to_owned(),
        syntax,
        mark,
    });
}

/// Materialize a one-based physical-line range in source order.
pub(super) fn build_display_lines(
    projection: &Projection<'_>,
    lines: Range<usize>,
    marked: &[MarkedRange],
    default_mark: DiffMark,
) -> Vec<DisplayLine> {
    lines
        .filter_map(|number| projection.source.line(number))
        .map(|line| build_display_line(projection, line, marked, default_mark))
        .collect()
}
