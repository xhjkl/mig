use mig::diff_model::{
    AlignedBlock, Anchor, AnchorIndex, BlockId, BlockKind, DiffGraph, DiffLayout, DiffSide,
    DiffState, DiffTokenKind, DiffView, DocumentSnapshot, FoldState, InlineAlignment,
    InlineAlignmentId, InlineDiffIndex, LineByteRange, LineRef, LineSpan, LspPosition, LspRange,
    SelectionState, SideId, SideRole, SnapshotByteRange, SnapshotId, TokenEdit, TokenEditKind,
    ViewportState,
};
use std::ops::Range;

pub(crate) fn inline_change_state() -> DiffState {
    let left_text =
        "fn explain_change() {\n    let mode = \"plain diff\";\n    println!(\"{mode}\");\n}\n";
    let right_text =
        "fn explain_change() {\n    let mode = \"token diff\";\n    println!(\"{mode}\");\n}\n";
    let left = DocumentSnapshot::from_text(
        SnapshotId(0),
        Some("fixture://inline-change/left.rs".to_owned()),
        left_text,
    );
    let right = DocumentSnapshot::from_text(
        SnapshotId(1),
        Some("fixture://inline-change/right.rs".to_owned()),
        right_text,
    );

    let left_changed = "    let mode = \"plain diff\";";
    let right_changed = "    let mode = \"token diff\";";
    let left_token = token_range(left_changed, "plain");
    let right_token = token_range(right_changed, "token");
    let anchor_line_hash = right.lines[1].text_hash;
    let anchor_context_before = right.lines[0].text_hash;
    let anchor_context_after = right.lines[2].text_hash;
    let anchor_snapshot_range = snapshot_byte_range_for_line(&right, 1, right_token.clone());

    let inline_alignment = InlineAlignmentId(0);
    let block = BlockId(1);

    DiffState {
        graph: DiffGraph {
            snapshots: vec![left, right],
            sides: two_way_sides("left.rs", "right.rs"),
            blocks: vec![
                AlignedBlock {
                    id: BlockId(0),
                    kind: BlockKind::Equal,
                    sides: vec![
                        Some(LineSpan {
                            snapshot: SnapshotId(0),
                            range: 0..1,
                        }),
                        Some(LineSpan {
                            snapshot: SnapshotId(1),
                            range: 0..1,
                        }),
                    ],
                    inline_alignments: Vec::new(),
                },
                AlignedBlock {
                    id: block,
                    kind: BlockKind::Replace,
                    sides: vec![
                        Some(LineSpan {
                            snapshot: SnapshotId(0),
                            range: 1..2,
                        }),
                        Some(LineSpan {
                            snapshot: SnapshotId(1),
                            range: 1..2,
                        }),
                    ],
                    inline_alignments: vec![inline_alignment],
                },
                AlignedBlock {
                    id: BlockId(2),
                    kind: BlockKind::Equal,
                    sides: vec![
                        Some(LineSpan {
                            snapshot: SnapshotId(0),
                            range: 2..4,
                        }),
                        Some(LineSpan {
                            snapshot: SnapshotId(1),
                            range: 2..4,
                        }),
                    ],
                    inline_alignments: Vec::new(),
                },
            ],
            inline: InlineDiffIndex {
                alignments: vec![InlineAlignment {
                    id: inline_alignment,
                    sides: vec![
                        Some(LineRef {
                            snapshot: SnapshotId(0),
                            line: 1,
                        }),
                        Some(LineRef {
                            snapshot: SnapshotId(1),
                            line: 1,
                        }),
                    ],
                    edits: vec![TokenEdit {
                        kind: TokenEditKind::Replace,
                        token_kind: DiffTokenKind::SyntaxToken,
                        line_ranges: vec![Some(left_token), Some(right_token)],
                    }],
                }],
            },
            anchors: AnchorIndex {
                anchors: vec![Anchor {
                    snapshot: SnapshotId(1),
                    snapshot_byte_range: anchor_snapshot_range,
                    line_hash: anchor_line_hash,
                    context_hash_before: anchor_context_before,
                    context_hash_after: anchor_context_after,
                    symbol_path: Some(vec!["explain_change".to_owned(), "mode".to_owned()]),
                    syntax_path: Some(vec!["Function".to_owned(), "LetBinding".to_owned()]),
                    lsp_range: Some(LspRange {
                        start: LspPosition {
                            line: 1,
                            character: 16,
                        },
                        end: LspPosition {
                            line: 1,
                            character: 21,
                        },
                    }),
                }],
            },
        },
        view: DiffView {
            viewport: ViewportState {
                top_block: Some(BlockId(0)),
                layout: DiffLayout::Auto,
            },
            selection: SelectionState {
                selected_block: Some(block),
                selected_side: Some(SideId(1)),
                selected_line: Some(1),
            },
            folds: FoldState::default(),
        },
    }
}

pub(crate) fn whole_function_state() -> DiffState {
    let left_text = "fn summarize_turn(turn: &Turn) -> String {\n    let files = turn.files.len();\n    format!(\"{files} files changed\")\n}\n\nfn render_status() {\n    println!(\"ready\");\n}\n";
    let right_text = "fn summarize_turn(turn: &Turn) -> String {\n    let changed = turn.files.len();\n    let events = turn.event_count;\n    format!(\"{changed} files across {events} events\")\n}\n\nfn render_status() {\n    println!(\"ready\");\n}\n";
    let left = DocumentSnapshot::from_text(
        SnapshotId(0),
        Some("fixture://whole-function/before.rs".to_owned()),
        left_text,
    );
    let right = DocumentSnapshot::from_text(
        SnapshotId(1),
        Some("fixture://whole-function/after.rs".to_owned()),
        right_text,
    );
    let block = BlockId(0);

    DiffState {
        graph: DiffGraph {
            snapshots: vec![left, right],
            sides: two_way_sides("before.rs", "after.rs"),
            blocks: vec![
                AlignedBlock {
                    id: block,
                    kind: BlockKind::Replace,
                    sides: vec![
                        Some(LineSpan {
                            snapshot: SnapshotId(0),
                            range: 0..4,
                        }),
                        Some(LineSpan {
                            snapshot: SnapshotId(1),
                            range: 0..5,
                        }),
                    ],
                    inline_alignments: Vec::new(),
                },
                AlignedBlock {
                    id: BlockId(1),
                    kind: BlockKind::Equal,
                    sides: vec![
                        Some(LineSpan {
                            snapshot: SnapshotId(0),
                            range: 4..8,
                        }),
                        Some(LineSpan {
                            snapshot: SnapshotId(1),
                            range: 5..9,
                        }),
                    ],
                    inline_alignments: Vec::new(),
                },
            ],
            inline: InlineDiffIndex::default(),
            anchors: AnchorIndex::default(),
        },
        view: DiffView {
            viewport: ViewportState {
                top_block: Some(block),
                layout: DiffLayout::Split,
            },
            selection: SelectionState {
                selected_block: Some(block),
                selected_side: Some(SideId(1)),
                selected_line: Some(0),
            },
            folds: FoldState::default(),
        },
    }
}

// Deliberately rendered as delete+insert until move identity lands.
pub(crate) fn move_without_identity_state() -> DiffState {
    let left_text = "fn prepare_turn(turn: &mut Turn) {\n    trace_turn(turn);\n    normalize_paths(turn);\n    collect_changes(turn);\n}\n\nfn finish_turn(turn: &Turn) {\n    write_summary(turn);\n}\n";
    let right_text = "fn prepare_turn(turn: &mut Turn) {\n    normalize_paths(turn);\n    collect_changes(turn);\n}\n\nfn finish_turn(turn: &Turn) {\n    write_summary(turn);\n    trace_turn(turn);\n}\n";
    let left = DocumentSnapshot::from_text(
        SnapshotId(0),
        Some("fixture://move-without-identity/before.rs".to_owned()),
        left_text,
    );
    let right = DocumentSnapshot::from_text(
        SnapshotId(1),
        Some("fixture://move-without-identity/after.rs".to_owned()),
        right_text,
    );
    let deleted = BlockId(1);

    DiffState {
        graph: DiffGraph {
            snapshots: vec![left, right],
            sides: two_way_sides("before.rs", "after.rs"),
            blocks: vec![
                AlignedBlock {
                    id: BlockId(0),
                    kind: BlockKind::Equal,
                    sides: vec![
                        Some(LineSpan {
                            snapshot: SnapshotId(0),
                            range: 0..1,
                        }),
                        Some(LineSpan {
                            snapshot: SnapshotId(1),
                            range: 0..1,
                        }),
                    ],
                    inline_alignments: Vec::new(),
                },
                AlignedBlock {
                    id: deleted,
                    kind: BlockKind::Delete,
                    sides: vec![
                        Some(LineSpan {
                            snapshot: SnapshotId(0),
                            range: 1..2,
                        }),
                        None,
                    ],
                    inline_alignments: Vec::new(),
                },
                AlignedBlock {
                    id: BlockId(2),
                    kind: BlockKind::Equal,
                    sides: vec![
                        Some(LineSpan {
                            snapshot: SnapshotId(0),
                            range: 2..8,
                        }),
                        Some(LineSpan {
                            snapshot: SnapshotId(1),
                            range: 1..7,
                        }),
                    ],
                    inline_alignments: Vec::new(),
                },
                AlignedBlock {
                    id: BlockId(3),
                    kind: BlockKind::Insert,
                    sides: vec![
                        None,
                        Some(LineSpan {
                            snapshot: SnapshotId(1),
                            range: 7..8,
                        }),
                    ],
                    inline_alignments: Vec::new(),
                },
                AlignedBlock {
                    id: BlockId(4),
                    kind: BlockKind::Equal,
                    sides: vec![
                        Some(LineSpan {
                            snapshot: SnapshotId(0),
                            range: 8..9,
                        }),
                        Some(LineSpan {
                            snapshot: SnapshotId(1),
                            range: 8..9,
                        }),
                    ],
                    inline_alignments: Vec::new(),
                },
            ],
            inline: InlineDiffIndex::default(),
            anchors: AnchorIndex::default(),
        },
        view: DiffView {
            viewport: ViewportState {
                top_block: Some(BlockId(0)),
                layout: DiffLayout::Stacked,
            },
            selection: SelectionState {
                selected_block: Some(deleted),
                selected_side: Some(SideId(0)),
                selected_line: Some(1),
            },
            folds: FoldState::default(),
        },
    }
}

fn two_way_sides(left_label: &str, right_label: &str) -> Vec<DiffSide> {
    vec![
        DiffSide {
            snapshot: SnapshotId(0),
            role: SideRole::Before,
            label: Some(left_label.to_owned()),
            origin: Some("git:HEAD~1".to_owned()),
        },
        DiffSide {
            snapshot: SnapshotId(1),
            role: SideRole::After,
            label: Some(right_label.to_owned()),
            origin: Some("git:worktree".to_owned()),
        },
    ]
}

fn snapshot_byte_range_for_line(
    snapshot: &DocumentSnapshot,
    line: usize,
    range: LineByteRange,
) -> SnapshotByteRange {
    let line = &snapshot.lines[line];
    line.snapshot_byte_range.start + range.start..line.snapshot_byte_range.start + range.end
}

fn token_range(line: &str, token: &str) -> Range<usize> {
    let start = line.find(token).expect("fixture token must exist");
    start..start + token.len()
}
