//! Lay islands out for reading: hunks of rows with nearby context, orientation
//! lines for the enclosing constructs, and explicit elisions.
//!
//! Every row names the source lines it shows, so a renderer never has to infer
//! what changed, and a checker can compare every row against the text.

use super::align::ordered_matches;
use super::islands::{Island, Islands, Kept, LineChange, Piece, Priority};
use super::syntax::{SyntaxPair, SyntaxTree};
use super::{EndingChange, Highlight, LineCoverage, LineNumber, LineSpan};
use std::collections::{BTreeSet, HashMap};
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
    pub highlight: Highlight,
    pub mark: DiffMark,
}

/// A one-based source line with its terminator excluded from the spans.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRow {
    pub number: LineNumber,
    pub spans: Vec<SourceSpan>,
}

impl SourceRow {
    pub fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }
}

/// Position of a row within a moved block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MoveEdge {
    Only,
    Start,
    Middle,
    End,
}

/// A skipped source gap or an intact interior of a relocated construct.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Elision {
    Gap {
        before: Option<LineSpan>,
        after: LineSpan,
    },
    Move {
        before: LineSpan,
        after: LineSpan,
    },
}

impl Elision {
    pub fn before(&self) -> Option<&LineSpan> {
        match self {
            Self::Gap { before, .. } => before.as_ref(),
            Self::Move { before, .. } => Some(before),
        }
    }

    pub fn after(&self) -> &LineSpan {
        match self {
            Self::Gap { after, .. } | Self::Move { after, .. } => after,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Row {
    /// Unchanged line present in both revisions.
    Context {
        before: LineNumber,
        after: SourceRow,
    },
    /// Lines whose tokens survived with a different layout: the old lines by number, the new in full.
    Reflow {
        before: Vec<LineNumber>,
        after: Vec<SourceRow>,
    },
    Removed(SourceRow),
    Added(SourceRow),
    /// A one-to-one line edit; both rows carry inline marks.
    Changed {
        before: SourceRow,
        after: SourceRow,
    },
    /// A line of a moved block, shown at its destination with its origin.
    Moved {
        before: LineNumber,
        after: SourceRow,
        edge: MoveEdge,
    },
    /// A terminator change on otherwise accounted lines.
    Ending(EndingChange),
    Elision(Elision),
    /// Opening line of an enclosing construct, shown for orientation; always an unchanged line.
    Crumb {
        before: LineNumber,
        after: SourceRow,
    },
    FileEnd,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Excerpt {
    pub priority: Priority,
    pub coverage: LineCoverage,
    pub rows: Vec<Row>,
}

#[derive(Clone, Debug)]
pub struct Document {
    pub sections: Vec<Section>,
}

/// Displayed excerpt with the source islands needed for attribution.
#[derive(Clone, Debug)]
pub struct Section {
    pub excerpt: Excerpt,
    pub(crate) islands: Vec<usize>,
}

impl Section {
    /// Return indices into the islands used to lay out this document.
    pub fn source_islands(&self) -> &[usize] {
        &self.islands
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Unchanged lines shown around each island.
    pub context: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self { context: 3 }
    }
}

/// Lay the islands out as hunks: edits first, then moves, wiring, and formatting, in source
/// order within each class.
pub fn layout(pair: &SyntaxPair<'_, '_>, islands: &Islands, options: Options) -> Document {
    let layout = Layout::new(pair, islands, options);
    let mut sections = layout
        .clusters()
        .into_iter()
        .map(|cluster| layout.section(&cluster))
        .collect::<Vec<_>>();
    sections.sort_by_key(|section| {
        let hunk = &section.excerpt;
        (
            std::cmp::Reverse(hunk.priority),
            hunk.coverage
                .after
                .as_ref()
                .map_or(0, |range| range.start.get()),
            hunk.coverage
                .before
                .as_ref()
                .map_or(0, |range| range.start.get()),
        )
    });
    Document { sections }
}

struct Layout<'input, 'before, 'after> {
    pair: &'input SyntaxPair<'before, 'after>,
    islands: &'input Islands,
    options: Options,
    /// Kept pair by after line (zero-based).
    kept_by_after: HashMap<usize, Kept>,
}

/// Islands shown together, in after order, plus kept lines whose terminator changed.
struct Cluster {
    islands: Vec<usize>,
    endings: Vec<usize>,
}

/// A row with the after position it belongs to; rows at one position keep their rank order.
struct Event {
    position: usize,
    rank: u8,
    row: Row,
}

/// Zero-based lines a hunk accounts for: shown on each side, and presented as changed.
#[derive(Default)]
struct Accounted {
    shown_before: BTreeSet<usize>,
    shown_after: BTreeSet<usize>,
}

impl Accounted {
    fn show(&mut self, before: usize, after: usize) {
        self.shown_before.insert(before);
        self.shown_after.insert(after);
    }

    fn show_piece(&mut self, piece: &LineChange) {
        self.shown_before.extend(piece.before.iter().copied());
        self.shown_after.extend(piece.after.iter().copied());
    }
}

/// Shortest run of paired, consecutive moved lines worth hiding behind an elision.
const MIN_ELIDED_MOVE_RUN: usize = 3;

const RANK_REMOVED: u8 = 1;
const RANK_LINE: u8 = 2;
const RANK_ENDING: u8 = 3;

impl<'input, 'before, 'after> Layout<'input, 'before, 'after> {
    fn new(
        pair: &'input SyntaxPair<'before, 'after>,
        islands: &'input Islands,
        options: Options,
    ) -> Self {
        Self {
            pair,
            islands,
            options,
            kept_by_after: islands
                .kept
                .iter()
                .map(|kept| (kept.after, *kept))
                .collect(),
        }
    }

    fn after_count(&self) -> usize {
        self.pair.after.source.lines().len()
    }

    fn before_count(&self) -> usize {
        self.pair.before.source.lines().len()
    }

    /// After-side extent an island occupies, or its insertion point as an empty range.
    fn island_after_extent(&self, island: &Island) -> Range<usize> {
        match island.after_lines().next() {
            Some(first) => first..island.after_lines().last().map_or(first, |last| last + 1),
            None => island.anchor_after..island.anchor_after,
        }
    }

    fn clusters(&self) -> Vec<Cluster> {
        let mut seeds = (0..self.islands.islands.len())
            .map(|index| {
                let island = &self.islands.islands[index];
                (
                    self.island_after_extent(island),
                    island.anchor_before,
                    Some(index),
                    None,
                )
            })
            .collect::<Vec<_>>();
        for (index, notice) in self.islands.endings.iter().enumerate() {
            let extent = self.ending_extent(index);
            let before = notice
                .change
                .before()
                .map_or(0, |terminator| terminator.line.index());
            seeds.push((extent, before, None, Some(index)));
        }
        seeds.sort_by_key(|(extent, before, _, _)| (extent.start, *before));
        let mut clusters = Vec::<Cluster>::new();
        let mut previous_end = None::<usize>;
        for (extent, _, island, ending) in seeds {
            let gap = previous_end.map(|end| extent.start.saturating_sub(end));
            match (clusters.last_mut(), gap) {
                (Some(cluster), Some(gap)) if gap <= self.options.context * 2 + 1 => {
                    cluster.islands.extend(island);
                    cluster.endings.extend(ending);
                }
                _ => clusters.push(Cluster {
                    islands: island.into_iter().collect(),
                    endings: ending.into_iter().collect(),
                }),
            }
            previous_end = Some(previous_end.unwrap_or(0).max(extent.end));
        }
        clusters
    }

    fn ending_extent(&self, index: usize) -> Range<usize> {
        let notice = &self.islands.endings[index];
        let start = notice.anchor_after;
        start..start + usize::from(notice.change.after().is_some())
    }

    fn section(&self, cluster: &Cluster) -> Section {
        let islands = cluster
            .islands
            .iter()
            .map(|index| &self.islands.islands[*index])
            .collect::<Vec<_>>();
        let priority = islands
            .iter()
            .map(|island| island.priority)
            .max()
            .unwrap_or(Priority::Reflow);
        let first_after = islands
            .iter()
            .map(|island| self.island_after_extent(island).start)
            .chain(
                cluster
                    .endings
                    .iter()
                    .map(|index| self.ending_extent(*index).start),
            )
            .min()
            .unwrap_or(0);
        let last_after = islands
            .iter()
            .map(|island| self.island_after_extent(island).end)
            .chain(
                cluster
                    .endings
                    .iter()
                    .map(|index| self.ending_extent(*index).end),
            )
            .max()
            .unwrap_or(0);
        let shown_start = first_after.saturating_sub(self.options.context);
        let shown_end = (last_after + self.options.context).min(self.after_count());

        let mut events = Vec::<Event>::new();
        let mut lines = Accounted::default();

        // Orientation: opening lines of the constructs enclosing the islands.
        let crumbs = self.crumbs(&islands, shown_start);
        let mut rows = Vec::new();
        for line in &crumbs {
            let kept = self.kept_by_after[line];
            rows.push(Row::Crumb {
                before: LineNumber::from_index(kept.before),
                after: self.after_row(*line, &[], DiffMark::Context),
            });
            lines.show(kept.before, *line);
        }

        // Body: islands at their positions, kept lines as context.
        for island in &islands {
            self.island_events(island, &mut events, &mut lines);
        }
        let island_after = islands
            .iter()
            .flat_map(|island| island.after_lines())
            .collect::<BTreeSet<_>>();
        for line in shown_start..shown_end {
            if island_after.contains(&line) {
                continue;
            }
            let Some(kept) = self.kept_by_after.get(&line) else {
                continue;
            };
            events.push(Event {
                position: line,
                rank: RANK_LINE,
                row: Row::Context {
                    before: LineNumber::from_index(kept.before),
                    after: self.after_row(line, &[], DiffMark::Context),
                },
            });
            lines.show(kept.before, line);
        }
        for index in &cluster.endings {
            let notice = &self.islands.endings[*index];
            events.push(Event {
                position: notice.anchor_after,
                rank: RANK_ENDING,
                row: Row::Ending(notice.change),
            });
            if let Some(before) = notice.change.before() {
                lines.shown_before.insert(before.line.index());
            }
            if let Some(after) = notice.change.after() {
                lines.shown_after.insert(after.line.index());
            }
        }
        events.sort_by_key(|event| (event.position, event.rank));

        // Elisions wherever consecutive shown after lines leave a gap.
        let mut previous_shown = crumbs.last().copied();
        let mut body = Vec::new();
        for event in events {
            let span = match &event.row {
                Row::Context { after, .. }
                | Row::Added(after)
                | Row::Changed { after, .. }
                | Row::Moved { after, .. } => Some((after.number.index(), after.number.index())),
                Row::Reflow { after, .. } => after
                    .first()
                    .zip(after.last())
                    .map(|(first, last)| (first.number.index(), last.number.index())),
                Row::Elision(elision) => {
                    let range = elision.after();
                    (!range.is_empty()).then(|| (range.start.index(), range.end.index() - 1))
                }
                Row::Removed(_) | Row::Ending(_) | Row::Crumb { .. } | Row::FileEnd => None,
            };
            if let Some((first, last)) = span {
                if let Some(previous) = previous_shown
                    && first > previous + 1
                {
                    body.push(self.elision(previous + 1..first));
                }
                previous_shown = Some(previous_shown.map_or(last, |previous| previous.max(last)));
            }
            body.push(event.row);
        }
        rows.extend(body);

        let reaches_end = shown_end >= self.after_count()
            && (lines
                .shown_before
                .iter()
                .next_back()
                .is_none_or(|last| *last + 1 >= self.before_count())
                || self.before_count() == 0);
        if reaches_end {
            rows.push(Row::FileEnd);
        }
        let rows = merge_elisions(rows);

        let coverage = LineCoverage {
            before: range_of(lines.shown_before.iter()),
            after: range_of(lines.shown_after.iter()),
        };
        Section {
            excerpt: Excerpt {
                priority,
                coverage,
                rows,
            },
            islands: cluster.islands.clone(),
        }
    }

    fn elision(&self, after: Range<usize>) -> Row {
        let before = self.before_range_for(after.clone());
        Row::Elision(Elision::Gap {
            before: before.map(LineSpan::from_indices),
            after: LineSpan::from_indices(after),
        })
    }

    /// Before lines aligned with an elided after range, when kept pairs bound it.
    fn before_range_for(&self, after: Range<usize>) -> Option<Range<usize>> {
        let first = after
            .clone()
            .find_map(|line| self.kept_by_after.get(&line))
            .map(|kept| kept.before)?;
        let last = after
            .rev()
            .find_map(|line| self.kept_by_after.get(&line))
            .map(|kept| kept.before)?;
        Some(first..last + 1)
    }

    /// Ancestors of the islands' owner that open before the shown range, outermost first.
    fn crumbs(&self, islands: &[&Island], shown_start: usize) -> Vec<usize> {
        let tree = &self.pair.after;
        let mut lines = BTreeSet::new();
        for island in islands {
            let mut node = island.owner;
            loop {
                let current = tree.node(node);
                if current.leaf().is_none() && node != tree.root {
                    let first = current.lines.start.index();
                    // A construct whose first line belongs to its first inner construct (a
                    // Python block opening on a statement) has no heading line of its own.
                    let opens_on_a_child = current
                        .children()
                        .first()
                        .map(|child| tree.node(*child))
                        .is_some_and(|child| {
                            child.leaf().is_none() && child.lines.start == current.lines.start
                        });
                    if first < shown_start
                        && current.lines.len() > 1
                        && !opens_on_a_child
                        && self.kept_by_after.contains_key(&first)
                    {
                        lines.insert(first);
                    }
                }
                match current.parent {
                    Some(parent) => node = parent,
                    None => break,
                }
            }
        }
        lines.into_iter().collect()
    }

    fn island_events(&self, island: &Island, events: &mut Vec<Event>, lines: &mut Accounted) {
        for (index, piece) in island.pieces.iter().enumerate() {
            // Removed material sits just before the first added line of its own piece, else of
            // the next piece that has one, else after the previous piece, else at the anchor.
            let change = piece.change();
            let position = change.after.first().copied().unwrap_or_else(|| {
                island.pieces[index + 1..]
                    .iter()
                    .find_map(|later| later.change().after.first().copied())
                    .or_else(|| {
                        island.pieces[..index]
                            .iter()
                            .rev()
                            .find_map(|earlier| earlier.change().after.last().map(|line| line + 1))
                    })
                    .unwrap_or(island.anchor_after)
            });
            match piece {
                Piece::Block(piece) => {
                    let (before_marks, after_marks) = self.block_marks(piece);
                    for line in &piece.before {
                        events.push(Event {
                            position,
                            rank: RANK_REMOVED,
                            row: Row::Removed(self.before_row(
                                *line,
                                &before_marks,
                                DiffMark::Removed,
                            )),
                        });
                    }
                    for line in &piece.after {
                        events.push(Event {
                            position: *line,
                            rank: RANK_LINE,
                            row: Row::Added(self.after_row(*line, &after_marks, DiffMark::Added)),
                        });
                    }
                }
                Piece::Inline(piece) => {
                    for (before, after) in piece.before.iter().zip(&piece.after) {
                        let (before_marks, after_marks) = self.inline_marks(*before, *after);
                        events.push(Event {
                            position: *after,
                            rank: RANK_LINE,
                            row: Row::Changed {
                                before: self.before_row(*before, &before_marks, DiffMark::Removed),
                                after: self.after_row(*after, &after_marks, DiffMark::Added),
                            },
                        });
                    }
                }
                Piece::Reflow(reflow) => {
                    let piece = &reflow.lines;
                    events.push(Event {
                        position,
                        rank: RANK_LINE,
                        row: Row::Reflow {
                            before: piece
                                .before
                                .iter()
                                .copied()
                                .map(LineNumber::from_index)
                                .collect(),
                            after: piece
                                .after
                                .iter()
                                .map(|line| self.after_row(*line, &[], DiffMark::Context))
                                .collect(),
                        },
                    });
                }
                Piece::Move(piece) => self.move_events(piece, events),
            }
            lines.show_piece(piece.change());
        }
    }

    /// Moved lines pair by text, blanks included, so every line of the move has a row or an
    /// elision; lines that did not pair one-to-one were re-wrapped by the move and show as a reflow.
    fn move_events(&self, piece: &LineChange, events: &mut Vec<Event>) {
        let before_text = piece
            .before
            .iter()
            .map(|line| super::syntax::content::tokens(&self.pair.before, &[*line]))
            .collect::<Vec<_>>();
        let after_text = piece
            .after
            .iter()
            .map(|line| super::syntax::content::tokens(&self.pair.after, &[*line]))
            .collect::<Vec<_>>();
        let pairs = ordered_matches(&before_text, &after_text);
        let count = pairs.len();
        // The move's edges are its first and last lines with content; blank separators that
        // travelled with it stay outside the edges as plain moved rows.
        let first_edge = pairs
            .iter()
            .position(|edge| !before_text[edge.before].is_empty())
            .unwrap_or(0);
        let last_edge = pairs
            .iter()
            .rposition(|edge| !before_text[edge.before].is_empty())
            .unwrap_or(count.saturating_sub(1));
        // Interior pairs consecutive on both sides form runs; a long run is elided so the edges of
        // the move stay in view. An elision therefore always covers equally many lines per side.
        let mut elided = vec![false; count];
        let has_ending_notice = |index: usize| {
            let after = LineNumber::from_index(piece.after[pairs[index].after]);
            self.islands.endings.iter().any(|notice| {
                notice
                    .change
                    .after()
                    .is_some_and(|terminator| terminator.line == after)
            })
        };
        let mut start = first_edge + 1;
        while start < last_edge {
            if has_ending_notice(start) {
                start += 1;
                continue;
            }
            let mut end = start;
            while end + 1 < last_edge
                && !has_ending_notice(end + 1)
                && piece.before[pairs[end + 1].before] == piece.before[pairs[end].before] + 1
                && piece.after[pairs[end + 1].after] == piece.after[pairs[end].after] + 1
            {
                end += 1;
            }
            if end + 1 - start >= MIN_ELIDED_MOVE_RUN {
                elided[start..=end].fill(true);
            }
            start = end + 1;
        }
        let mut offset = 0;
        while offset < count {
            if elided[offset] {
                let mut end = offset;
                while end + 1 < count && elided[end + 1] {
                    end += 1;
                }
                let first_before = piece.before[pairs[offset].before];
                let last_before = piece.before[pairs[end].before];
                let first_after = piece.after[pairs[offset].after];
                let last_after = piece.after[pairs[end].after];
                events.push(Event {
                    position: first_after,
                    rank: RANK_LINE,
                    row: Row::Elision(Elision::Move {
                        before: LineSpan::from_indices(first_before..last_before + 1),
                        after: LineSpan::from_indices(first_after..last_after + 1),
                    }),
                });
                offset = end + 1;
                continue;
            }
            let edge = pairs[offset];
            let (before, after) = (piece.before[edge.before], piece.after[edge.after]);
            let edge_kind = match offset {
                _ if first_edge == last_edge && offset == first_edge => MoveEdge::Only,
                _ if offset == first_edge => MoveEdge::Start,
                _ if offset == last_edge => MoveEdge::End,
                _ => MoveEdge::Middle,
            };
            events.push(Event {
                position: after,
                rank: RANK_LINE,
                row: Row::Moved {
                    before: LineNumber::from_index(before),
                    after: self.after_row(after, &[], DiffMark::Context),
                    edge: edge_kind,
                },
            });
            offset += 1;
        }
        let paired_before = pairs
            .iter()
            .map(|edge| edge.before)
            .collect::<std::collections::HashSet<_>>();
        let paired_after = pairs
            .iter()
            .map(|edge| edge.after)
            .collect::<std::collections::HashSet<_>>();
        let leftover_before = piece
            .before
            .iter()
            .enumerate()
            .filter(|(index, _)| !paired_before.contains(index))
            .map(|(_, line)| *line)
            .collect::<Vec<_>>();
        let leftover_after = piece
            .after
            .iter()
            .enumerate()
            .filter(|(index, _)| !paired_after.contains(index))
            .map(|(_, line)| *line)
            .collect::<Vec<_>>();
        if !leftover_before.is_empty() || !leftover_after.is_empty() {
            let position = leftover_after
                .first()
                .copied()
                .or_else(|| piece.after.last().copied())
                .unwrap_or(0);
            events.push(Event {
                position,
                rank: RANK_LINE,
                row: Row::Reflow {
                    before: leftover_before
                        .iter()
                        .copied()
                        .map(LineNumber::from_index)
                        .collect(),
                    after: leftover_after
                        .iter()
                        .map(|line| self.after_row(*line, &[], DiffMark::Context))
                        .collect(),
                },
            });
        }
    }

    /// Byte ranges of the tokens that do not survive a block replacement, per side.
    /// One-sided blocks mark their lines whole.
    fn block_marks(&self, piece: &LineChange) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
        let whole = |tree: &SyntaxTree<'_>, lines: &[usize]| {
            lines
                .iter()
                .map(|line| tree.source.lines()[*line].content_bytes.clone())
                .collect::<Vec<_>>()
        };
        if piece.before.is_empty() || piece.after.is_empty() {
            return (
                whole(&self.pair.before, &piece.before),
                whole(&self.pair.after, &piece.after),
            );
        }
        let before_tokens = line_tokens(&self.pair.before, &piece.before);
        let after_tokens = line_tokens(&self.pair.after, &piece.after);
        let before_text = before_tokens
            .iter()
            .map(|(text, _)| *text)
            .collect::<Vec<_>>();
        let after_text = after_tokens
            .iter()
            .map(|(text, _)| *text)
            .collect::<Vec<_>>();
        let matches = ordered_matches(&before_text, &after_text);
        let mut before_matched = vec![false; before_tokens.len()];
        let mut after_matched = vec![false; after_tokens.len()];
        for edge in matches {
            before_matched[edge.before] = true;
            after_matched[edge.after] = true;
        }
        let unmatched = |tokens: &[(&str, Range<usize>)], matched: &[bool]| {
            let mut marks = Vec::<Range<usize>>::new();
            for (index, (_, bytes)) in tokens.iter().enumerate() {
                if matched[index] {
                    continue;
                }
                if let Some(previous) = marks.last_mut()
                    && previous.end >= bytes.start
                {
                    previous.end = previous.end.max(bytes.end);
                } else {
                    marks.push(bytes.clone());
                }
            }
            marks
        };
        (
            unmatched(&before_tokens, &before_matched),
            unmatched(&after_tokens, &after_matched),
        )
    }

    /// Character ranges that differ between two lines, widened to whole words.
    fn inline_marks(&self, before: usize, after: usize) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
        let before_line = &self.pair.before.source.lines()[before];
        let after_line = &self.pair.after.source.lines()[after];
        let before_text = self.pair.before.source.text(before_line);
        let after_text = self.pair.after.source.text(after_line);
        let before_chars = before_text.char_indices().collect::<Vec<_>>();
        let after_chars = after_text.char_indices().collect::<Vec<_>>();
        let before_values = before_chars
            .iter()
            .map(|(_, character)| *character)
            .collect::<Vec<_>>();
        let after_values = after_chars
            .iter()
            .map(|(_, character)| *character)
            .collect::<Vec<_>>();
        let matches = ordered_matches(&before_values, &after_values);
        let mut matched_before = vec![false; before_chars.len()];
        let mut matched_after = vec![false; after_chars.len()];
        for edge in matches {
            matched_before[edge.before] = true;
            matched_after[edge.after] = true;
        }
        (
            marks(
                &before_chars,
                &matched_before,
                before_line.content_bytes.start,
                before_text,
            ),
            marks(
                &after_chars,
                &matched_after,
                after_line.content_bytes.start,
                after_text,
            ),
        )
    }

    fn after_row(&self, line: usize, marks: &[Range<usize>], mark: DiffMark) -> SourceRow {
        source_row(&self.pair.after, LineNumber::from_index(line), marks, mark)
    }

    fn before_row(&self, line: usize, marks: &[Range<usize>], mark: DiffMark) -> SourceRow {
        source_row(&self.pair.before, LineNumber::from_index(line), marks, mark)
    }
}

/// Non-layout leaves inside the given lines that spell something, with their clipped byte ranges.
fn line_tokens<'tree>(
    tree: &'tree SyntaxTree<'_>,
    lines: &'tree [usize],
) -> Vec<(&'tree str, Range<usize>)> {
    tree.content_spans_in_lines(lines)
        .filter(|(text, _)| !text.trim().is_empty())
        .collect()
}

/// Byte ranges of the unmatched characters, widened to whole words so a rename reads as one token.
fn marks(chars: &[(usize, char)], matched: &[bool], base: usize, text: &str) -> Vec<Range<usize>> {
    let is_word = |character: char| character.is_alphanumeric() || character == '_';
    let mut marks = Vec::<Range<usize>>::new();
    let mut index = 0;
    while index < chars.len() {
        if matched[index] {
            index += 1;
            continue;
        }
        let mut start = index;
        let mut end = index + 1;
        while end < chars.len() && !matched[end] {
            end += 1;
        }
        if is_word(chars[start].1) {
            while start > 0 && is_word(chars[start - 1].1) {
                start -= 1;
            }
        }
        if is_word(chars[end - 1].1) {
            while end < chars.len() && is_word(chars[end].1) {
                end += 1;
            }
        }
        let byte_start = base + chars[start].0;
        let byte_end = base + chars.get(end).map_or(text.len(), |(byte, _)| *byte);
        if let Some(previous) = marks.last_mut()
            && previous.end >= byte_start
        {
            previous.end = previous.end.max(byte_end);
        } else {
            marks.push(byte_start..byte_end);
        }
        index = end;
    }
    marks
}

fn range_of<'line>(lines: impl Iterator<Item = &'line usize>) -> Option<LineSpan> {
    let mut range = None::<Range<usize>>;
    for line in lines {
        let line = *line;
        range = Some(match range {
            None => line..line + 1,
            Some(range) => range.start.min(line)..range.end.max(line + 1),
        });
    }
    range.map(LineSpan::from_indices)
}

/// Join adjacent gaps or consecutive pairs within the same move.
fn merge_elisions(rows: Vec<Row>) -> Vec<Row> {
    let mut merged = Vec::<Row>::new();
    for row in rows {
        if let (Some(Row::Elision(previous)), Row::Elision(current)) = (merged.last_mut(), &row) {
            match (previous, current) {
                (
                    Elision::Gap { before, after },
                    Elision::Gap {
                        before: current_before,
                        after: current_after,
                    },
                ) if after.end == current_after.start => {
                    after.end = current_after.end;
                    match (before, current_before) {
                        (Some(before), Some(current)) => {
                            before.start = before.start.min(current.start);
                            before.end = before.end.max(current.end);
                        }
                        (before @ None, Some(current)) => *before = Some(current.clone()),
                        _ => {}
                    }
                    continue;
                }
                (
                    Elision::Move { before, after },
                    Elision::Move {
                        before: current_before,
                        after: current_after,
                    },
                ) if before.end == current_before.start && after.end == current_after.start => {
                    before.end = current_before.end;
                    after.end = current_after.end;
                    continue;
                }
                _ => {}
            }
        }
        merged.push(row);
    }
    merged
}

/// Split one source line at syntax and mark boundaries.
pub fn source_row(
    tree: &SyntaxTree<'_>,
    number: LineNumber,
    marks: &[Range<usize>],
    mark: DiffMark,
) -> SourceRow {
    let source = tree.source.line(number).expect("rows name existing lines");
    let bytes = source.content_bytes.clone();
    // Marks arrive sorted and disjoint; a block's marks cover every line of the block, so only
    // the ones overlapping this line are consulted.
    debug_assert!(
        marks.windows(2).all(|pair| pair[0].end <= pair[1].start),
        "marks are sorted and disjoint"
    );
    let first = marks.partition_point(|changed| changed.end <= bytes.start);
    let marks = &marks[first..];
    let last = marks.partition_point(|changed| changed.start < bytes.end);
    let marks = &marks[..last];
    let leaves = tree
        .leaf_ids_in(bytes.clone())
        .map(|id| tree.node(id))
        .collect::<Vec<_>>();
    let mut boundaries = vec![bytes.start, bytes.end];
    for leaf in &leaves {
        boundaries.push(leaf.bytes.start.max(bytes.start));
        boundaries.push(leaf.bytes.end.min(bytes.end));
    }
    for changed in marks {
        if changed.start >= bytes.end || changed.end <= bytes.start {
            continue;
        }
        boundaries.push(changed.start.max(bytes.start));
        boundaries.push(changed.end.min(bytes.end));
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut spans = Vec::<SourceSpan>::new();
    // Leaves and segments are both in source order; one cursor serves every lookup.
    let mut leaf_index = 0;
    for boundary in boundaries.windows(2) {
        let segment = boundary[0]..boundary[1];
        if segment.is_empty() {
            continue;
        }
        let Some(text) = tree.source.slice(segment.clone()) else {
            continue;
        };
        while leaf_index < leaves.len() && leaves[leaf_index].bytes.end <= segment.start {
            leaf_index += 1;
        }
        let highlight = leaves
            .get(leaf_index)
            .filter(|leaf| leaf.bytes.start <= segment.start && leaf.bytes.end >= segment.end)
            .and_then(|leaf| leaf.leaf())
            .map(|leaf| leaf.highlight)
            .unwrap_or(Highlight::Plain);
        let changed = marks
            .iter()
            .any(|changed| changed.start <= segment.start && changed.end >= segment.end);
        let mark = if changed { mark } else { DiffMark::Context };
        if let Some(previous) = spans.last_mut()
            && previous.highlight == highlight
            && previous.mark == mark
        {
            previous.text.push_str(text);
            continue;
        }
        spans.push(SourceSpan {
            text: text.to_owned(),
            highlight,
            mark,
        });
    }
    SourceRow { number, spans }
}
