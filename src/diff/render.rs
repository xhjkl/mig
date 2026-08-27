//! Syntax-aware materialization of source-space planner decisions.

use super::context::ranges_overlap;
use super::projection::Projection;
use super::source::SourceLine;
use super::{DiffMark, DisplayLine, DisplaySpan, SyntaxClass, WordDiff};
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

/// Compact one replacement to the changed text between its shared affixes.
pub(super) fn word_diff(
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

/// Materialize one aligned or one-sided line edit with byte-exact marks.
pub(super) fn changed_display_lines(
    before: &Projection<'_>,
    before_line: Option<&SourceLine>,
    after: &Projection<'_>,
    after_line: Option<&SourceLine>,
) -> (Option<DisplayLine>, Option<DisplayLine>) {
    let (Some(before_line), Some(after_line)) = (before_line, after_line) else {
        let before = before_line.map(|line| {
            build_display_line(
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
            build_display_line(
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
            Some(build_display_line(
                before,
                before_line,
                &[before_mark],
                DiffMark::Context,
            )),
            Some(build_display_line(
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
        Some(build_display_line(
            before,
            before_line,
            before_mark.as_slice(),
            DiffMark::Context,
        )),
        Some(build_display_line(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_byte_ranges_translate_character_edits_to_absolute_utf8_bytes() {
        let (before, after) = changed_byte_ranges(10, "αoldω", 30, "αnewω");

        assert_eq!(before, 12..15);
        assert_eq!(after, 32..35);
    }

    #[test]
    fn word_diff_keeps_unicode_shared_affixes() {
        let word = word_diff(Some(4), Some(7), "café_old_name", "café_new_name");

        assert_eq!(word.before_line, Some(4));
        assert_eq!(word.after_line, Some(7));
        assert_eq!(word.prefix, "café_");
        assert_eq!(word.removed, "old");
        assert_eq!(word.added, "new");
        assert_eq!(word.suffix, "_name");
    }
}
