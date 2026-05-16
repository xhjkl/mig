use ropey::Rope;
use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;

// All text ranges are byte offsets. A field name must say whether it is
// snapshot-relative or line-relative.
pub type SnapshotByteRange = Range<usize>;
pub type LineByteRange = Range<usize>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct SnapshotId(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct SideId(pub usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SideRole {
    Base,
    Before,
    After,
    Ours,
    Theirs,
}

#[derive(Clone, Debug)]
pub struct DiffSide {
    pub snapshot: SnapshotId,
    pub role: SideRole,
    pub label: Option<String>,
    pub origin: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct BlockId(pub usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct InlineAlignmentId(pub usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct AnchorId(pub u64);

#[derive(Clone, Debug)]
pub struct DiffGraph {
    pub snapshots: Vec<DocumentSnapshot>,
    pub sides: Vec<DiffSide>,
    pub blocks: Vec<AlignedBlock>,
    pub inline: InlineDiffIndex,
    pub anchors: AnchorIndex,
}

#[derive(Clone, Debug)]
pub struct DiffView {
    pub viewport: ViewportState,
    pub selection: SelectionState,
    pub folds: FoldState,
}

#[derive(Clone, Debug)]
pub struct DiffState {
    pub graph: DiffGraph,
    pub view: DiffView,
}

impl DiffState {
    pub fn side(&self, side: SideId) -> Option<&DiffSide> {
        self.graph.sides.get(side.0)
    }

    pub fn snapshot(&self, snapshot: SnapshotId) -> Option<&DocumentSnapshot> {
        let index = usize::try_from(snapshot.0).ok()?;
        self.graph
            .snapshots
            .get(index)
            .filter(|candidate| candidate.id == snapshot)
    }
}

#[derive(Clone, Debug)]
pub struct DocumentSnapshot {
    pub id: SnapshotId,
    pub uri: Option<String>,
    pub version: Option<i32>,
    pub text: Rope,
    pub lines: Vec<LineMeta>,
    pub lsp_index: LspIndex,
}

impl DocumentSnapshot {
    pub fn from_text(id: SnapshotId, uri: Option<String>, text: &str) -> Self {
        let text = Rope::from_str(text);
        let mut lines = Vec::new();

        for ordinal in 0..text.len_lines() {
            let byte_start = text.line_to_byte(ordinal);
            let byte_end = if ordinal + 1 < text.len_lines() {
                text.line_to_byte(ordinal + 1)
            } else {
                text.len_bytes()
            };
            let line = text.line(ordinal).to_string();
            lines.push(LineMeta {
                ordinal: ordinal as u32,
                snapshot_byte_range: byte_start..byte_end,
                text_hash: hash_text(&line),
                normalized_hash: hash_text(&normalize_line(&line)),
                line_anchor: AnchorId(hash_text(&format!("{id:?}:{ordinal}:{line}"))),
            });
        }

        Self {
            id,
            uri,
            version: None,
            text,
            lines,
            lsp_index: LspIndex::default(),
        }
    }

    pub fn line_text(&self, line: u32) -> Option<String> {
        let line = usize::try_from(line).ok()?;
        if line >= self.text.len_lines() {
            return None;
        }
        Some(self.text.line(line).to_string())
    }
}

#[derive(Clone, Debug)]
pub struct LineMeta {
    pub ordinal: u32,
    pub snapshot_byte_range: SnapshotByteRange,
    pub text_hash: u64,
    pub normalized_hash: u64,
    // Line anchors are snapshot-local. Semantic recovery belongs in AnchorIndex.
    pub line_anchor: AnchorId,
}

#[derive(Clone, Debug, Default)]
pub struct LspIndex {
    pub symbol_paths: Vec<Vec<String>>,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct AlignedBlock {
    pub id: BlockId,
    pub kind: BlockKind,
    pub sides: Vec<Option<LineSpan>>,
    pub inline_alignments: Vec<InlineAlignmentId>,
}

#[derive(Clone, Debug)]
pub struct LineSpan {
    pub snapshot: SnapshotId,
    pub range: Range<u32>,
}

// BlockKind is a cached classification of the alignment, not the source of
// truth. Side presence in AlignedBlock::sides determines which snapshots
// participate; the aligned snapshot ranges determine the real diff content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockKind {
    Equal,
    Insert,
    Delete,
    Replace,
    Move,
    Conflict,
}

impl BlockKind {
    pub fn is_changed(self) -> bool {
        match self {
            Self::Equal => false,
            Self::Insert | Self::Delete | Self::Replace | Self::Move | Self::Conflict => true,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct InlineDiffIndex {
    pub alignments: Vec<InlineAlignment>,
}

impl InlineDiffIndex {
    pub fn get(&self, id: InlineAlignmentId) -> Option<&InlineAlignment> {
        self.alignments.get(id.0)
    }
}

#[derive(Clone, Debug)]
pub struct InlineAlignment {
    pub id: InlineAlignmentId,
    pub sides: Vec<Option<LineRef>>,
    pub edits: Vec<TokenEdit>,
}

#[derive(Clone, Debug)]
pub struct LineRef {
    pub snapshot: SnapshotId,
    pub line: u32,
}

#[derive(Clone, Debug)]
pub struct TokenEdit {
    pub kind: TokenEditKind,
    pub token_kind: DiffTokenKind,
    pub line_ranges: Vec<Option<LineByteRange>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenEditKind {
    Equal,
    Insert,
    Delete,
    Replace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffTokenKind {
    ChangedRun,
    Word,
    Whitespace,
    Punctuation,
    Grapheme,
    SyntaxToken,
}

#[derive(Clone, Debug, Default)]
pub struct AnchorIndex {
    pub anchors: Vec<Anchor>,
}

#[derive(Clone, Debug)]
pub struct Anchor {
    pub snapshot: SnapshotId,
    pub snapshot_byte_range: SnapshotByteRange,
    pub line_hash: u64,
    pub context_hash_before: u64,
    pub context_hash_after: u64,
    pub symbol_path: Option<Vec<String>>,
    pub syntax_path: Option<Vec<String>>,
    pub lsp_range: Option<LspRange>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LspPosition {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LspRange {
    pub start: LspPosition,
    pub end: LspPosition,
}

#[derive(Clone, Debug, Default)]
pub struct ViewportState {
    pub top_block: Option<BlockId>,
    pub layout: DiffLayout,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DiffLayout {
    #[default]
    Auto,
    Split,
    Stacked,
}

#[derive(Clone, Debug, Default)]
pub struct SelectionState {
    pub selected_block: Option<BlockId>,
    pub selected_side: Option<SideId>,
    pub selected_line: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct FoldState {
    pub collapsed: HashSet<BlockId>,
}

pub fn assert_diff_state_invariants(state: &DiffState) {
    let graph = &state.graph;
    let side_count = graph.sides.len();

    for (index, snapshot) in graph.snapshots.iter().enumerate() {
        assert_eq!(snapshot.id.0 as usize, index);
    }

    for side in &graph.sides {
        assert!(
            state.snapshot(side.snapshot).is_some(),
            "side references missing snapshot {:?}",
            side.snapshot
        );
    }

    for (index, block) in graph.blocks.iter().enumerate() {
        assert_eq!(block.id.0, index);
        assert_eq!(block.sides.len(), side_count);

        for (side_index, span) in block.sides.iter().enumerate() {
            let Some(span) = span else {
                continue;
            };
            assert_eq!(span.snapshot, graph.sides[side_index].snapshot);
            let snapshot = state.snapshot(span.snapshot).expect("span snapshot exists");
            assert!(span.range.end as usize <= snapshot.lines.len());
        }

        for inline in &block.inline_alignments {
            assert_eq!(graph.inline.alignments[inline.0].id, *inline);
        }
    }

    for (index, inline) in graph.inline.alignments.iter().enumerate() {
        assert_eq!(inline.id.0, index);
        assert_eq!(inline.sides.len(), side_count);

        for (side_index, line) in inline.sides.iter().enumerate() {
            let Some(line) = line else {
                continue;
            };
            assert_eq!(line.snapshot, graph.sides[side_index].snapshot);
            let snapshot = state.snapshot(line.snapshot).expect("line snapshot exists");
            assert!((line.line as usize) < snapshot.lines.len());
        }

        for edit in &inline.edits {
            assert_eq!(edit.line_ranges.len(), side_count);
            for (side_index, range) in edit.line_ranges.iter().enumerate() {
                let Some(range) = range else {
                    continue;
                };
                let Some(Some(line)) = inline.sides.get(side_index) else {
                    panic!("token range has no matching line reference");
                };
                let snapshot = state.snapshot(line.snapshot).expect("line snapshot exists");
                let text = snapshot
                    .line_text(line.line)
                    .unwrap_or_default()
                    .trim_end_matches(['\r', '\n'])
                    .to_owned();
                assert!(range.end <= text.len());
                assert!(text.is_char_boundary(range.start));
                assert!(text.is_char_boundary(range.end));
            }
        }
    }

    for anchor in &graph.anchors.anchors {
        let snapshot = state
            .snapshot(anchor.snapshot)
            .expect("anchor snapshot exists");
        assert!(anchor.snapshot_byte_range.end <= snapshot.text.len_bytes());
    }

    if let Some(block) = state.view.selection.selected_block {
        assert!(block.0 < graph.blocks.len());
    }
    if let Some(side) = state.view.selection.selected_side {
        assert!(side.0 < side_count);
    }
    for block in &state.view.folds.collapsed {
        assert!(block.0 < graph.blocks.len());
    }
}

fn hash_text(text: &str) -> u64 {
    // Runtime-only hash. Do not persist or compare across Mig versions.
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn normalize_line(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}
