use crate::FileDiff;
use crate::diff_model::{
    AlignedBlock, AnchorIndex, BlockId, BlockKind, DiffGraph, DiffLayout, DiffSide, DiffState,
    DiffTokenKind, DiffView, DocumentSnapshot, FoldState, InlineAlignment, InlineAlignmentId,
    InlineDiffIndex, LineRef, LineSpan, SelectionState, SideId, SideRole, SnapshotId, TokenEdit,
    TokenEditKind, ViewportState,
};
use similar::{DiffTag, TextDiff};
use std::ops::Range;

pub fn file_diff_state(
    file: &FileDiff,
    before_origin: impl Into<String>,
    after_origin: impl Into<String>,
) -> Option<DiffState> {
    let before = file.before();
    let before = match before {
        Some(before) => before.text()?,
        None => "",
    };
    let after = file.after();
    let after = match after {
        Some(after) => after.text()?,
        None => "",
    };

    Some(text_diff_state(TextDiffInput {
        label: &file.path().to_string_lossy(),
        before_origin: before_origin.into(),
        after_origin: after_origin.into(),
        before,
        after,
    }))
}

pub struct TextDiffInput<'a> {
    pub label: &'a str,
    pub before_origin: String,
    pub after_origin: String,
    pub before: &'a str,
    pub after: &'a str,
}

pub fn text_diff_state(input: TextDiffInput<'_>) -> DiffState {
    let before = DocumentSnapshot::from_text(
        SnapshotId(0),
        Some(format!("workspace://before/{}", input.label)),
        input.before,
    );
    let after = DocumentSnapshot::from_text(
        SnapshotId(1),
        Some(format!("workspace://after/{}", input.label)),
        input.after,
    );

    let diff = TextDiff::from_lines(input.before, input.after);
    let mut blocks = Vec::new();
    let mut inline = Vec::new();

    for op in diff.ops() {
        let id = BlockId(blocks.len());
        let old_range = op.old_range();
        let new_range = op.new_range();
        let kind = block_kind(op.tag());
        let inline_alignments =
            inline_alignments_for_block(&before, &after, &old_range, &new_range, kind, &mut inline);

        blocks.push(AlignedBlock {
            id,
            kind,
            sides: vec![
                line_span(SnapshotId(0), old_range),
                line_span(SnapshotId(1), new_range),
            ],
            inline_alignments,
        });
    }

    let selection = selected_change(&blocks);

    DiffState {
        graph: DiffGraph {
            snapshots: vec![before, after],
            sides: vec![
                DiffSide {
                    snapshot: SnapshotId(0),
                    role: SideRole::Before,
                    label: Some(input.label.to_owned()),
                    origin: Some(input.before_origin),
                },
                DiffSide {
                    snapshot: SnapshotId(1),
                    role: SideRole::After,
                    label: Some(input.label.to_owned()),
                    origin: Some(input.after_origin),
                },
            ],
            blocks,
            inline: InlineDiffIndex { alignments: inline },
            anchors: AnchorIndex::default(),
        },
        view: DiffView {
            viewport: ViewportState {
                top_block: Some(BlockId(0)),
                layout: DiffLayout::Auto,
            },
            selection,
            folds: FoldState::default(),
        },
    }
}

fn block_kind(tag: DiffTag) -> BlockKind {
    match tag {
        DiffTag::Equal => BlockKind::Equal,
        DiffTag::Delete => BlockKind::Delete,
        DiffTag::Insert => BlockKind::Insert,
        DiffTag::Replace => BlockKind::Replace,
    }
}

fn line_span(snapshot: SnapshotId, range: Range<usize>) -> Option<LineSpan> {
    if range.is_empty() {
        return None;
    }

    Some(LineSpan {
        snapshot,
        range: range.start as u32..range.end as u32,
    })
}

fn inline_alignments_for_block(
    before: &DocumentSnapshot,
    after: &DocumentSnapshot,
    old_range: &Range<usize>,
    new_range: &Range<usize>,
    kind: BlockKind,
    inline: &mut Vec<InlineAlignment>,
) -> Vec<InlineAlignmentId> {
    if kind != BlockKind::Replace {
        return Vec::new();
    }

    let mut ids = Vec::new();
    let len = old_range.len().min(new_range.len());
    // v0 line pairing inside replace blocks: pair by local offset.
    // Later, use similarity scoring so inserted lines do not shift every
    // following inline alignment.
    for offset in 0..len {
        let old_line = old_range.start + offset;
        let new_line = new_range.start + offset;
        let before = snapshot_line_text(before, old_line as u32);
        let after = snapshot_line_text(after, new_line as u32);
        let Some((before_range, after_range)) = changed_run_ranges(&before, &after) else {
            continue;
        };

        let id = InlineAlignmentId(inline.len());
        ids.push(id);
        inline.push(InlineAlignment {
            id,
            sides: vec![
                Some(LineRef {
                    snapshot: SnapshotId(0),
                    line: old_line as u32,
                }),
                Some(LineRef {
                    snapshot: SnapshotId(1),
                    line: new_line as u32,
                }),
            ],
            edits: vec![TokenEdit {
                kind: token_edit_kind(&before_range, &after_range),
                token_kind: DiffTokenKind::ChangedRun,
                line_ranges: vec![non_empty_range(before_range), non_empty_range(after_range)],
            }],
        });
    }
    ids
}

fn snapshot_line_text(snapshot: &DocumentSnapshot, line: u32) -> String {
    snapshot
        .line_text(line)
        .unwrap_or_default()
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

// v0 inline diff: shrink a replacement to the changed byte run.
// This is not token alignment; replace with tokenizer-backed alignment before
// relying on token_kind for syntax-aware behavior.
fn changed_run_ranges(before: &str, after: &str) -> Option<(Range<usize>, Range<usize>)> {
    if before == after {
        return None;
    }

    let prefix = common_prefix_bytes(before, after);
    let before_tail = &before[prefix..];
    let after_tail = &after[prefix..];
    let suffix = common_suffix_bytes(before_tail, after_tail);
    let before_end = before.len() - suffix;
    let after_end = after.len() - suffix;

    Some((prefix..before_end, prefix..after_end))
}

fn common_prefix_bytes(before: &str, after: &str) -> usize {
    let mut prefix = 0;
    for ((before_index, before_char), (after_index, after_char)) in
        before.char_indices().zip(after.char_indices())
    {
        if before_index != after_index || before_char != after_char {
            break;
        }
        prefix = before_index + before_char.len_utf8();
    }
    prefix
}

fn common_suffix_bytes(before: &str, after: &str) -> usize {
    let mut suffix = 0;
    for (before_char, after_char) in before.chars().rev().zip(after.chars().rev()) {
        if before_char != after_char {
            break;
        }
        suffix += before_char.len_utf8();
    }
    suffix
}

fn token_edit_kind(before: &Range<usize>, after: &Range<usize>) -> TokenEditKind {
    match (before.is_empty(), after.is_empty()) {
        (true, false) => TokenEditKind::Insert,
        (false, true) => TokenEditKind::Delete,
        _ => TokenEditKind::Replace,
    }
}

fn non_empty_range(range: Range<usize>) -> Option<Range<usize>> {
    if range.is_empty() { None } else { Some(range) }
}

fn selected_change(blocks: &[AlignedBlock]) -> SelectionState {
    let selected_block = blocks
        .iter()
        .find(|block| block.kind.is_changed())
        .map(|block| block.id);
    let selected_side = selected_block.map(|_| SideId(1));
    let selected_line = selected_block
        .and_then(|block| blocks.get(block.0))
        .and_then(|block| {
            block
                .sides
                .get(1)
                .and_then(Option::as_ref)
                .or_else(|| block.sides.first().and_then(Option::as_ref))
        })
        .map(|span| span.range.start);

    SelectionState {
        selected_block,
        selected_side,
        selected_line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_model::assert_diff_state_invariants;

    #[test]
    fn text_diff_state_builds_line_and_inline_alignment() {
        let state = text_diff_state(TextDiffInput {
            label: "sample.rs",
            before_origin: "before".to_owned(),
            after_origin: "after".to_owned(),
            before: "let mode = \"plain diff\";\n",
            after: "let mode = \"token diff\";\n",
        });

        assert_diff_state_invariants(&state);
        assert_eq!(state.graph.snapshots.len(), 2);
        assert_eq!(state.graph.blocks.len(), 1);
        assert_eq!(state.graph.blocks[0].kind, BlockKind::Replace);
        assert_eq!(state.graph.inline.alignments.len(), 1);
    }

    #[test]
    fn inline_changed_run_ranges_are_unicode_safe() {
        let state = text_diff_state(TextDiffInput {
            label: "unicode.rs",
            before_origin: "before".to_owned(),
            after_origin: "after".to_owned(),
            before: "let marker = \"🔥\";\n",
            after: "let marker = \"✨\";\n",
        });

        assert_diff_state_invariants(&state);
    }
}
