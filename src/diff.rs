use crate::syntax::{Comment, Definition, Import, SourceLine, SyntaxFile, Token, parse_rust};
use anyhow::Result;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::ops::Range;
use std::path::Path;

pub use crate::syntax::SyntaxClass;

/// Diff role layered over syntax styling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffMark {
    Context,
    Removed,
    Added,
}

/// Concrete terminator shown when a literal line's ending itself changed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LineEnding {
    Missing,
    Lf,
    CrLf,
}

/// Smallest independently styled slice of one displayed source line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeSpan {
    pub text: String,
    pub syntax: SyntaxClass,
    pub mark: DiffMark,
}

/// Original source line retained inside a bounded diff window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeLine {
    pub number: usize,
    pub spans: Vec<CodeSpan>,
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

/// One-based, half-open before/current bounds covered by a review row or window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineMapping {
    pub before: Option<Range<usize>>,
    pub after: Option<Range<usize>>,
}

/// How one current-world source line communicates its role in the window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodeRole {
    Context,
    Inline,
    Reflow,
}

/// Presentation-ready row chosen while planning a bounded diff window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiffRow {
    Code {
        line: CodeLine,
        role: CodeRole,
    },
    Linewise {
        before: Option<CodeLine>,
        after: Option<CodeLine>,
    },
    LineEnding {
        before: Option<LineEnding>,
        after: Option<LineEnding>,
    },
    Moved {
        before: Option<usize>,
        after: CodeLine,
    },
    Wordwise(WordDiff),
    Elision(LineMapping),
}

/// Bounded view into a file containing related, presentation-ready rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffWindow {
    pub mapping: LineMapping,
    pub rows: Vec<DiffRow>,
}

/// Render-ready stream of bounded windows for one source file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileDiff {
    pub path: String,
    /// Whether either source revision declares itself generated near its header.
    pub generated: bool,
    pub windows: Vec<DiffWindow>,
}

/// Select syntax-aware Rust review when safe, otherwise plan an exact line review.
pub fn diff_file(path: &str, before: &str, after: &str) -> Result<FileDiff> {
    let generated = has_generated_marker(before) || has_generated_marker(after);
    if generated || !is_rust_path(path) {
        return Ok(diff_plain(path, before, after, generated));
    }

    diff_rust(path, before, after)
}

/// Parse Rust, establish one-to-one syntax correspondence, and plan a unified review.
fn diff_rust(path: &str, before: &str, after: &str) -> Result<FileDiff> {
    let before_file = parse_rust(before)?;
    let after_file = parse_rust(after)?;
    if before_file.tree.root_node().has_error() || after_file.tree.root_node().has_error() {
        return Ok(diff_plain(path, before, after, false));
    }

    let matches = correspond(&before_file.definitions, &after_file.definitions);
    let windows = plan_unified(&before_file, &after_file, &matches);
    // Unknown top-level forms must not disappear beside recognized structural edits.
    if before != after
        && (windows.is_empty()
            || !structural_projection_covers_plain_changes(
                before,
                after,
                &before_file,
                &after_file,
            ))
    {
        return Ok(diff_plain(path, before, after, false));
    }

    Ok(FileDiff {
        path: path.to_owned(),
        generated: false,
        windows,
    })
}

fn structural_projection_covers_plain_changes(
    before: &str,
    after: &str,
    before_file: &SyntaxFile<'_>,
    after_file: &SyntaxFile<'_>,
) -> bool {
    let before_lines = plain_lines(before);
    let after_lines = plain_lines(after);
    let before_keys = before_lines
        .iter()
        .map(|line| (line.text, line.ending))
        .collect::<Vec<_>>();
    let after_keys = after_lines
        .iter()
        .map(|line| (line.text, line.ending))
        .collect::<Vec<_>>();
    let matches = local_matches(&before_keys, &after_keys);
    let events = plain_events(before_lines.len(), after_lines.len(), matches);

    events.into_iter().all(|event| {
        let PlainEvent::Change { before, after } = event else {
            return true;
        };
        if plain_ranges_change_endings(&before, &after, &before_lines, &after_lines) {
            return false;
        }
        plain_range_is_projected(before, &before_lines, before_file)
            && plain_range_is_projected(after, &after_lines, after_file)
    })
}

fn plain_ranges_change_endings(
    before: &Range<usize>,
    after: &Range<usize>,
    before_lines: &[PlainLine<'_>],
    after_lines: &[PlainLine<'_>],
) -> bool {
    let paired = before.len().min(after.len());
    for index in 0..paired {
        if before_lines[before.start + index].ending != after_lines[after.start + index].ending {
            return true;
        }
    }
    before_lines[before.start + paired..before.end]
        .iter()
        .chain(&after_lines[after.start + paired..after.end])
        .any(|line| line.ending == LineEnding::Missing)
}

fn plain_range_is_projected(
    indices: Range<usize>,
    lines: &[PlainLine<'_>],
    file: &SyntaxFile<'_>,
) -> bool {
    indices.into_iter().all(|index| {
        let line = &lines[index];
        line.text.trim().is_empty()
            || file
                .definitions
                .iter()
                .any(|definition| definition.lines.contains(&line.number))
            || file.imports.iter().any(|import| import.line == line.number)
    })
}

fn is_rust_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension == "rs")
}

/// Conventional marker search is deliberately header-bounded and case-sensitive.
fn has_generated_marker(source: &str) -> bool {
    source.lines().take(20).any(is_generated_marker_line)
}

fn is_generated_marker_line(line: &str) -> bool {
    let line = line.trim_start();
    let marker_line = line.starts_with("@generated")
        || line.starts_with("//")
        || line.starts_with('#')
        || line.starts_with("/*")
        || line.starts_with('*')
        || line.starts_with("<!--")
        || line.starts_with("--")
        || line.starts_with(';');
    if !marker_line {
        return false;
    }

    line.match_indices("@generated").any(|(marker, _)| {
        let before = &line[..marker];
        let after = &line[marker + "@generated".len()..];
        let bounded_before = before
            .chars()
            .next_back()
            .is_none_or(|character| !character.is_alphanumeric() && character != '_');
        let bounded_after = after.chars().next().is_none_or(|character| {
            !character.is_alphanumeric() && !matches!(character, '_' | '/' | '-')
        });
        bounded_before && bounded_after
    })
}

const PLAIN_CONTEXT_LINES: usize = 3;

/// One exact text record; terminators participate in matching and surface only when changed.
#[derive(Clone, Copy)]
struct PlainLine<'source> {
    number: usize,
    text: &'source str,
    ending: LineEnding,
}

/// Ordered exact-line correspondence before presentation expands changed gaps.
enum PlainEvent {
    Context {
        before: usize,
        after: usize,
    },
    Change {
        before: Range<usize>,
        after: Range<usize>,
    },
}

/// Exact-line fallback for generated Rust and every syntax Mig does not understand.
fn diff_plain(path: &str, before: &str, after: &str, generated: bool) -> FileDiff {
    let before = plain_lines(before);
    let after = plain_lines(after);
    let before_keys = before
        .iter()
        .map(|line| (line.text, line.ending))
        .collect::<Vec<_>>();
    let after_keys = after
        .iter()
        .map(|line| (line.text, line.ending))
        .collect::<Vec<_>>();
    let matches = local_matches(&before_keys, &after_keys);
    let events = plain_events(before.len(), after.len(), matches);
    let windows = plan_plain_windows(&before, &after, &events);

    FileDiff {
        path: path.to_owned(),
        generated,
        windows,
    }
}

fn plain_lines(source: &str) -> Vec<PlainLine<'_>> {
    if source.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in source.bytes().enumerate() {
        if byte != b'\n' {
            continue;
        }

        let (text_end, ending) = if index > start && source.as_bytes()[index - 1] == b'\r' {
            (index - 1, LineEnding::CrLf)
        } else {
            (index, LineEnding::Lf)
        };
        lines.push(PlainLine {
            number: lines.len() + 1,
            text: &source[start..text_end],
            ending,
        });
        start = index + 1;
    }
    if start < source.len() {
        lines.push(PlainLine {
            number: lines.len() + 1,
            text: &source[start..],
            ending: LineEnding::Missing,
        });
    }
    lines
}

fn plain_events(
    before_len: usize,
    after_len: usize,
    matches: Vec<(usize, usize)>,
) -> Vec<PlainEvent> {
    let mut events = Vec::new();
    let mut before_start = 0;
    let mut after_start = 0;
    for (before_end, after_end) in matches
        .into_iter()
        .chain(std::iter::once((before_len, after_len)))
    {
        if before_start < before_end || after_start < after_end {
            events.push(PlainEvent::Change {
                before: before_start..before_end,
                after: after_start..after_end,
            });
        }
        if before_end < before_len && after_end < after_len {
            events.push(PlainEvent::Context {
                before: before_end,
                after: after_end,
            });
        }
        before_start = before_end.saturating_add(1);
        after_start = after_end.saturating_add(1);
    }
    events
}

/// Merge nearby changes using the same context radius on both sides.
fn plan_plain_windows(
    before: &[PlainLine<'_>],
    after: &[PlainLine<'_>],
    events: &[PlainEvent],
) -> Vec<DiffWindow> {
    let changes = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| matches!(event, PlainEvent::Change { .. }).then_some(index))
        .collect::<Vec<_>>();
    let Some(first) = changes.first().copied() else {
        return Vec::new();
    };

    let mut windows = Vec::new();
    let mut group_start = first;
    let mut group_end = first;
    for change in changes.into_iter().skip(1) {
        let separating_context = change.saturating_sub(group_end + 1);
        // Keeping one otherwise-hidden line costs the same row as separating hunks.
        if separating_context <= PLAIN_CONTEXT_LINES * 2 + 1 {
            group_end = change;
            continue;
        }

        windows.push(plan_plain_window(
            before,
            after,
            events,
            group_start,
            group_end,
        ));
        group_start = change;
        group_end = change;
    }
    windows.push(plan_plain_window(
        before,
        after,
        events,
        group_start,
        group_end,
    ));
    windows
}

fn plan_plain_window(
    before: &[PlainLine<'_>],
    after: &[PlainLine<'_>],
    events: &[PlainEvent],
    first_change: usize,
    last_change: usize,
) -> DiffWindow {
    let start = first_change.saturating_sub(PLAIN_CONTEXT_LINES);
    let end = (last_change + PLAIN_CONTEXT_LINES + 1).min(events.len());
    let events = &events[start..end];
    let mut mapping = LineMapping {
        before: None,
        after: None,
    };
    let mut rows = Vec::new();

    for event in events {
        match event {
            PlainEvent::Context {
                before: before_index,
                after: after_index,
            } => {
                include_plain_mapping(
                    &mut mapping.before,
                    before,
                    &(*before_index..*before_index + 1),
                );
                include_plain_mapping(&mut mapping.after, after, &(*after_index..*after_index + 1));
                rows.push(DiffRow::Code {
                    line: plain_code_line(after[*after_index], DiffMark::Context),
                    role: CodeRole::Context,
                });
            }
            PlainEvent::Change {
                before: before_range,
                after: after_range,
            } => {
                include_plain_mapping(&mut mapping.before, before, before_range);
                include_plain_mapping(&mut mapping.after, after, after_range);
                render_plain_change(
                    &mut rows,
                    &before[before_range.clone()],
                    &after[after_range.clone()],
                );
            }
        }
    }

    DiffWindow { mapping, rows }
}

fn include_plain_mapping(
    mapping: &mut Option<Range<usize>>,
    lines: &[PlainLine<'_>],
    indices: &Range<usize>,
) {
    let Some(first) = lines.get(indices.start) else {
        return;
    };
    let Some(last_index) = indices.end.checked_sub(1) else {
        return;
    };
    let Some(last) = lines.get(last_index) else {
        return;
    };
    let covered = first.number..last.number + 1;

    let Some(mapping) = mapping else {
        *mapping = Some(covered);
        return;
    };
    mapping.start = mapping.start.min(covered.start);
    mapping.end = mapping.end.max(covered.end);
}

fn render_plain_change(rows: &mut Vec<DiffRow>, before: &[PlainLine<'_>], after: &[PlainLine<'_>]) {
    let paired = before.len().min(after.len());
    for index in 0..paired {
        let before_line = before[index];
        let after_line = after[index];
        let (before, after) = if before_line.text == after_line.text {
            (
                plain_code_line(before_line, DiffMark::Removed),
                plain_code_line(after_line, DiffMark::Added),
            )
        } else {
            let (before_spans, after_spans) =
                aligned_text_spans(before_line.text, after_line.text, SyntaxClass::Plain);
            (
                CodeLine {
                    number: before_line.number,
                    spans: before_spans,
                },
                CodeLine {
                    number: after_line.number,
                    spans: after_spans,
                },
            )
        };
        rows.push(DiffRow::Linewise {
            before: Some(before),
            after: Some(after),
        });
        if before_line.ending != after_line.ending {
            rows.push(DiffRow::LineEnding {
                before: Some(before_line.ending),
                after: Some(after_line.ending),
            });
        }
    }
    for line in &before[paired..] {
        rows.push(DiffRow::Linewise {
            before: Some(plain_code_line(*line, DiffMark::Removed)),
            after: None,
        });
        if line.ending == LineEnding::Missing {
            rows.push(DiffRow::LineEnding {
                before: Some(line.ending),
                after: None,
            });
        }
    }
    for line in &after[paired..] {
        rows.push(DiffRow::Linewise {
            before: None,
            after: Some(plain_code_line(*line, DiffMark::Added)),
        });
        if line.ending == LineEnding::Missing {
            rows.push(DiffRow::LineEnding {
                before: None,
                after: Some(line.ending),
            });
        }
    }
}

fn plain_code_line(line: PlainLine<'_>, mark: DiffMark) -> CodeLine {
    let mut spans = Vec::new();
    push_code_span(&mut spans, line.text, SyntaxClass::Plain, mark);
    CodeLine {
        number: line.number,
        spans,
    }
}

/// One-to-one top-level syntax correspondence; impossible empty pairs are unrepresentable.
#[derive(Clone, Copy)]
enum DefinitionMatch<'a> {
    Matched {
        before: &'a Definition<'a>,
        after: &'a Definition<'a>,
    },
    Removed(&'a Definition<'a>),
    Added(&'a Definition<'a>),
}

fn correspond<'a>(
    before: &'a [Definition<'a>],
    after: &'a [Definition<'a>],
) -> Vec<DefinitionMatch<'a>> {
    let mut after_by_key = HashMap::<(&str, &str), VecDeque<usize>>::new();
    for (index, definition) in after.iter().enumerate() {
        after_by_key
            .entry((definition.kind, definition.name))
            .or_default()
            .push_back(index);
    }

    let mut matched_after = vec![false; after.len()];
    let mut matches = Vec::with_capacity(before.len().max(after.len()));
    for definition in before {
        let candidates = after_by_key.get_mut(&(definition.kind, definition.name));
        let index = candidates.and_then(|candidates| {
            // Exact occurrences anchor duplicate names before local order breaks ties.
            let exact = candidates
                .iter()
                .position(|index| after[*index].full_fingerprint == definition.full_fingerprint);
            exact
                .and_then(|position| candidates.remove(position))
                .or_else(|| candidates.pop_front())
        });
        let Some(index) = index else {
            matches.push(DefinitionMatch::Removed(definition));
            continue;
        };

        matched_after[index] = true;
        matches.push(DefinitionMatch::Matched {
            before: definition,
            after: &after[index],
        });
    }
    matches.extend(
        after
            .iter()
            .enumerate()
            .filter(|(index, _)| !matched_after[*index])
            .map(|(_, definition)| DefinitionMatch::Added(definition)),
    );
    matches
}

/// Sole owner of review grouping, ordering, row treatments, and elision.
fn plan_unified(
    before: &SyntaxFile<'_>,
    after: &SyntaxFile<'_>,
    matches: &[DefinitionMatch<'_>],
) -> Vec<DiffWindow> {
    let moves = moved_definitions(matches);
    let moved_starts = moves
        .iter()
        .map(|(before, _)| before.lines.start)
        .collect::<HashSet<_>>();

    let mut definitions = plan_definition_windows(before, after, matches);
    definitions.sort_by_key(|(line, _)| *line);

    let mut windows = definitions
        .into_iter()
        .map(|(_, window)| window)
        .collect::<Vec<_>>();
    windows.extend(
        moves
            .into_iter()
            .map(|(before_definition, after_definition)| {
                plan_move_window(after, before_definition, after_definition)
            }),
    );
    windows.extend(plan_import_windows(&before.imports, &after.imports));
    windows.extend(plan_reflow_windows(before, after, matches, &moved_starts));
    windows
}

fn plan_definition_windows(
    before_file: &SyntaxFile<'_>,
    after_file: &SyntaxFile<'_>,
    matches: &[DefinitionMatch<'_>],
) -> Vec<(usize, DiffWindow)> {
    let mut windows = Vec::new();
    for matched in matches {
        match *matched {
            DefinitionMatch::Matched { before, after } => {
                let comments = comment_edits(before, after);
                if definition_source(before_file, before) == definition_source(after_file, after) {
                    continue;
                }
                if !before.has_error
                    && !after.has_error
                    && before.code_fingerprint == after.code_fingerprint
                {
                    windows.extend(comments.into_iter().map(|comment| {
                        let line = comment_line(&comment);
                        (line, plan_comment_window(comment))
                    }));
                    continue;
                }

                let correspondence =
                    align_tokens(&before.tokens, &after.tokens, |token| !token.is_comment);
                let before_context = correspondence.before_indices();
                let after_context = correspondence.after_indices();
                let before_lines = render_definition(
                    before_file,
                    before,
                    &before_context,
                    DiffMark::Removed,
                    true,
                );
                let after_lines =
                    render_definition(after_file, after, &after_context, DiffMark::Added, true);
                let boundaries = blank_boundaries(after_file, after);
                let mapping = LineMapping {
                    before: Some(before.lines.clone()),
                    after: Some(after.lines.clone()),
                };
                windows.push((
                    before.lines.start,
                    plan_definition_window(
                        mapping,
                        before_lines,
                        after_lines,
                        boundaries,
                        comments,
                    ),
                ));
            }
            DefinitionMatch::Removed(definition) => {
                let lines = render_definition(
                    before_file,
                    definition,
                    &HashSet::new(),
                    DiffMark::Removed,
                    false,
                );
                let boundaries = blank_boundaries(before_file, definition);
                let mapping = LineMapping {
                    before: Some(definition.lines.clone()),
                    after: None,
                };
                windows.push((
                    definition.lines.start,
                    plan_definition_window(mapping, lines, Vec::new(), boundaries, Vec::new()),
                ));
            }
            DefinitionMatch::Added(definition) => {
                let lines = render_definition(
                    after_file,
                    definition,
                    &HashSet::new(),
                    DiffMark::Added,
                    false,
                );
                let boundaries = blank_boundaries(after_file, definition);
                let mapping = LineMapping {
                    before: None,
                    after: Some(definition.lines.clone()),
                };
                windows.push((
                    definition.lines.start,
                    plan_definition_window(mapping, Vec::new(), lines, boundaries, Vec::new()),
                ));
            }
        }
    }
    windows
}

fn plan_definition_window(
    mapping: LineMapping,
    before: Vec<CodeLine>,
    after: Vec<CodeLine>,
    boundaries: (Option<CodeLine>, Option<CodeLine>),
    commentary: Vec<CommentEdit>,
) -> DiffWindow {
    let mut rows = Vec::new();
    if let Some(line) = boundaries.0 {
        rows.push(DiffRow::Code {
            line,
            role: CodeRole::Context,
        });
    }

    let present = if after.is_empty() { before } else { after };
    let mut commentary = commentary.into_iter().peekable();
    for line in present {
        while commentary
            .peek()
            .is_some_and(|commentary| comment_line(commentary) < line.number)
        {
            let commentary = commentary.next().expect("peeked commentary");
            rows.push(DiffRow::Linewise {
                before: commentary.before,
                after: commentary.after,
            });
        }
        let role = if line.spans.iter().any(|span| span.mark != DiffMark::Context) {
            CodeRole::Inline
        } else {
            CodeRole::Context
        };
        rows.push(DiffRow::Code { line, role });
    }
    for commentary in commentary {
        rows.push(DiffRow::Linewise {
            before: commentary.before,
            after: commentary.after,
        });
    }
    if let Some(line) = boundaries.1 {
        rows.push(DiffRow::Code {
            line,
            role: CodeRole::Context,
        });
    }

    DiffWindow {
        mapping,
        rows: abbreviate_rows(rows),
    }
}

fn plan_comment_window(change: CommentEdit) -> DiffWindow {
    DiffWindow {
        mapping: LineMapping {
            before: change
                .before
                .as_ref()
                .map(|line| line.number..line.number + 1),
            after: change
                .after
                .as_ref()
                .map(|line| line.number..line.number + 1),
        },
        rows: vec![DiffRow::Linewise {
            before: change.before,
            after: change.after,
        }],
    }
}

fn plan_move_window(
    after_file: &SyntaxFile<'_>,
    before: &Definition<'_>,
    after: &Definition<'_>,
) -> DiffWindow {
    let mapping = LineMapping {
        before: Some(before.lines.clone()),
        after: Some(after.lines.clone()),
    };
    let context = (0..after.tokens.len()).collect::<HashSet<_>>();
    let preview = render_definition(after_file, after, &context, DiffMark::Context, false);
    let mut preview = preview.into_iter();
    let Some(first) = preview.next() else {
        return DiffWindow {
            mapping,
            rows: Vec::new(),
        };
    };
    let mut preview = preview.collect::<Vec<_>>();
    let last = preview.pop();
    // Establish the old/current mapping once, then remain in current source.
    let mut rows = vec![DiffRow::Moved {
        before: Some(before.lines.start),
        after: first,
    }];
    let before_range = before.lines.start + 1..before.lines.end.saturating_sub(1);
    let after_range = after.lines.start + 1..after.lines.end.saturating_sub(1);
    // A one-line fold saves no space; keep its present-world context instead.
    if preview.len() == 1 {
        let line = preview.pop().expect("one preview line remains");
        rows.push(DiffRow::Code {
            line,
            role: CodeRole::Context,
        });
    } else if !preview.is_empty() {
        rows.push(DiffRow::Elision(LineMapping {
            before: Some(before_range),
            after: Some(after_range),
        }));
    }
    if let Some(last) = last {
        rows.push(DiffRow::Moved {
            before: None,
            after: last,
        });
    }
    DiffWindow { mapping, rows }
}

fn plan_import_windows(before: &[Import<'_>], after: &[Import<'_>]) -> Vec<DiffWindow> {
    let before_text = before
        .iter()
        .map(|import| import.text)
        .collect::<HashSet<_>>();
    let after_text = after
        .iter()
        .map(|import| import.text)
        .collect::<HashSet<_>>();
    let removed = before
        .iter()
        .filter(|import| !after_text.contains(import.text))
        .collect::<Vec<_>>();
    let added = after
        .iter()
        .filter(|import| !before_text.contains(import.text))
        .collect::<Vec<_>>();
    let count = removed.len().max(added.len());

    (0..count)
        .map(|index| word_diff(removed.get(index), added.get(index)))
        .map(|word| DiffWindow {
            mapping: LineMapping {
                before: word.before_line.map(|line| line..line + 1),
                after: word.after_line.map(|line| line..line + 1),
            },
            rows: vec![DiffRow::Wordwise(word)],
        })
        .collect()
}

fn moved_definitions<'a>(
    matches: &[DefinitionMatch<'a>],
) -> Vec<(&'a Definition<'a>, &'a Definition<'a>)> {
    let paired = matches
        .iter()
        .filter_map(|matched| {
            let DefinitionMatch::Matched { before, after } = *matched else {
                return None;
            };
            Some((before, after))
        })
        .collect::<Vec<_>>();

    // Every matched occurrence votes on stable order; otherwise one reflow can
    // make the actually moved definition win an arbitrary LIS tie.
    let mut after_order = (0..paired.len()).collect::<Vec<_>>();
    after_order.sort_by_key(|index| paired[*index].1.lines.start);
    let stable = increasing_subsequence(&after_order);

    paired
        .into_iter()
        .enumerate()
        .filter(|(index, (before, after))| {
            !before.has_error
                && !after.has_error
                && before.full_fingerprint == after.full_fingerprint
                && !stable.contains(index)
                && before.lines.start != after.lines.start
        })
        .map(|(_, definitions)| definitions)
        .collect()
}

fn increasing_subsequence(values: &[usize]) -> HashSet<usize> {
    let mut tails = Vec::<usize>::new();
    let mut previous = vec![None; values.len()];
    for (index, value) in values.iter().copied().enumerate() {
        let slot = tails.partition_point(|tail| values[*tail] < value);
        if slot > 0 {
            previous[index] = Some(tails[slot - 1]);
        }
        if slot == tails.len() {
            tails.push(index);
        } else {
            tails[slot] = index;
        }
    }

    let Some(mut index) = tails.last().copied() else {
        return HashSet::new();
    };
    let mut stable = HashSet::new();
    loop {
        stable.insert(values[index]);
        let Some(parent) = previous[index] else {
            break;
        };
        index = parent;
    }
    stable
}

fn plan_reflow_windows(
    before_file: &SyntaxFile<'_>,
    after_file: &SyntaxFile<'_>,
    matches: &[DefinitionMatch<'_>],
    moved_starts: &HashSet<usize>,
) -> Vec<DiffWindow> {
    // Reflow is a semantic claim, so parser recovery anywhere makes us conservative.
    if before_file.tree.root_node().has_error() || after_file.tree.root_node().has_error() {
        return Vec::new();
    }

    matches
        .iter()
        .filter_map(|matched| {
            let DefinitionMatch::Matched { before, after } = *matched else {
                return None;
            };
            if before.has_error
                || after.has_error
                || moved_starts.contains(&before.lines.start)
                || before.full_fingerprint != after.full_fingerprint
            {
                return None;
            }
            if definition_source(before_file, before) == definition_source(after_file, after) {
                return None;
            }

            let before_context = (0..before.tokens.len()).collect::<HashSet<_>>();
            let after_context = (0..after.tokens.len()).collect::<HashSet<_>>();
            let before_lines = render_definition(
                before_file,
                before,
                &before_context,
                DiffMark::Context,
                false,
            );
            let after_lines =
                render_definition(after_file, after, &after_context, DiffMark::Context, false);
            let before_text = before_lines.iter().map(code_line_text).collect::<Vec<_>>();
            let after_text = after_lines.iter().map(code_line_text).collect::<Vec<_>>();
            let unchanged_after = local_matches(&before_text, &after_text)
                .into_iter()
                .map(|(_, after_index)| after_index)
                .collect::<HashSet<_>>();
            let rows = after_lines
                .into_iter()
                .enumerate()
                .map(|(index, line)| DiffRow::Code {
                    line,
                    role: if unchanged_after.contains(&index) {
                        CodeRole::Context
                    } else {
                        CodeRole::Reflow
                    },
                })
                .collect();
            Some(DiffWindow {
                mapping: LineMapping {
                    before: Some(before.lines.clone()),
                    after: Some(after.lines.clone()),
                },
                rows,
            })
        })
        .collect()
}

fn definition_source<'source>(
    file: &SyntaxFile<'source>,
    definition: &Definition<'_>,
) -> &'source str {
    &file.source[definition.bytes.clone()]
}

fn blank_boundaries(
    file: &SyntaxFile<'_>,
    definition: &Definition<'_>,
) -> (Option<CodeLine>, Option<CodeLine>) {
    let leading = definition
        .lines
        .start
        .checked_sub(1)
        .and_then(|number| blank_line(file, number));
    let trailing = blank_line(file, definition.lines.end);
    (leading, trailing)
}

fn blank_line(file: &SyntaxFile<'_>, number: usize) -> Option<CodeLine> {
    let line = number
        .checked_sub(1)
        .and_then(|index| file.lines.get(index))?;
    let text = &file.source[line.bytes.clone()];
    if !text.trim().is_empty() {
        return None;
    }
    Some(CodeLine {
        number,
        spans: vec![context_span(text)],
    })
}

/// One changed comment occurrence, including pure insertion or deletion.
struct CommentEdit {
    before: Option<CodeLine>,
    after: Option<CodeLine>,
}

fn comment_edits(before: &Definition<'_>, after: &Definition<'_>) -> Vec<CommentEdit> {
    let before_text = before
        .comments
        .iter()
        .map(|comment| comment.text)
        .collect::<Vec<_>>();
    let after_text = after
        .comments
        .iter()
        .map(|comment| comment.text)
        .collect::<Vec<_>>();
    let matches = local_matches(&before_text, &after_text);

    let mut edits = Vec::new();
    let mut before_start = 0;
    let mut after_start = 0;
    for (before_end, after_end) in matches.into_iter().chain(std::iter::once((
        before.comments.len(),
        after.comments.len(),
    ))) {
        push_comment_gap(
            &mut edits,
            &before.comments[before_start..before_end],
            &after.comments[after_start..after_end],
        );
        before_start = before_end.saturating_add(1);
        after_start = after_end.saturating_add(1);
    }
    edits.sort_by_key(comment_line);
    edits
}

fn push_comment_gap(edits: &mut Vec<CommentEdit>, before: &[Comment<'_>], after: &[Comment<'_>]) {
    let paired = before.len().min(after.len());
    for index in 0..paired {
        edits.push(render_comment_edit(
            Some(&before[index]),
            Some(&after[index]),
        ));
    }
    edits.extend(
        before[paired..]
            .iter()
            .map(|comment| render_comment_edit(Some(comment), None)),
    );
    edits.extend(
        after[paired..]
            .iter()
            .map(|comment| render_comment_edit(None, Some(comment))),
    );
}

fn render_comment_edit(before: Option<&Comment<'_>>, after: Option<&Comment<'_>>) -> CommentEdit {
    let (before_spans, after_spans) = match (before, after) {
        (Some(before), Some(after)) => {
            let (mut before_spans, mut after_spans) =
                aligned_text_spans(before.text, after.text, SyntaxClass::Comment);
            before_spans.insert(0, context_span(before.indent));
            after_spans.insert(0, context_span(after.indent));
            (Some(before_spans), Some(after_spans))
        }
        (Some(before), None) => (
            Some(vec![
                context_span(before.indent),
                CodeSpan {
                    text: before.text.to_owned(),
                    syntax: SyntaxClass::Comment,
                    mark: DiffMark::Removed,
                },
            ]),
            None,
        ),
        (None, Some(after)) => (
            None,
            Some(vec![
                context_span(after.indent),
                CodeSpan {
                    text: after.text.to_owned(),
                    syntax: SyntaxClass::Comment,
                    mark: DiffMark::Added,
                },
            ]),
        ),
        (None, None) => unreachable!("a comment edit always has a source side"),
    };

    CommentEdit {
        before: before.zip(before_spans).map(|(comment, spans)| CodeLine {
            number: comment.line,
            spans,
        }),
        after: after.zip(after_spans).map(|(comment, spans)| CodeLine {
            number: comment.line,
            spans,
        }),
    }
}

fn comment_line(edit: &CommentEdit) -> usize {
    edit.after
        .as_ref()
        .or(edit.before.as_ref())
        .map(|line| line.number)
        .expect("a comment edit always has a source side")
}

fn context_span(text: &str) -> CodeSpan {
    CodeSpan {
        text: text.to_owned(),
        syntax: SyntaxClass::Plain,
        mark: DiffMark::Context,
    }
}

/// Preserve the window frame and every signal row; fold only distant context.
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
        // The ellipsis would occupy the same row while hiding useful context.
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
        abbreviated.push(DiffRow::Elision(LineMapping {
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
        DiffRow::Elision(mapping) => mapping.after.as_ref().map(|range| range.start),
    }
}

fn code_line_text(line: &CodeLine) -> String {
    line.spans.iter().map(|span| span.text.as_str()).collect()
}

fn word_diff(before: Option<&&Import<'_>>, after: Option<&&Import<'_>>) -> WordDiff {
    let before_text = before.map(|import| import.text).unwrap_or("");
    let after_text = after.map(|import| import.text).unwrap_or("");
    let before_characters = before_text.chars().collect::<Vec<_>>();
    let after_characters = after_text.chars().collect::<Vec<_>>();
    let prefix_length = before_characters
        .iter()
        .zip(&after_characters)
        .take_while(|(before, after)| before == after)
        .count();
    let remaining = before_characters
        .len()
        .min(after_characters.len())
        .saturating_sub(prefix_length);
    let suffix_length = before_characters[prefix_length..]
        .iter()
        .rev()
        .zip(after_characters[prefix_length..].iter().rev())
        .take(remaining)
        .take_while(|(before, after)| before == after)
        .count();
    let before_end = before_characters.len().saturating_sub(suffix_length);
    let after_end = after_characters.len().saturating_sub(suffix_length);

    WordDiff {
        before_line: before.map(|import| import.line),
        after_line: after.map(|import| import.line),
        prefix: before_characters[..prefix_length].iter().collect(),
        removed: before_characters[prefix_length..before_end]
            .iter()
            .collect(),
        added: after_characters[prefix_length..after_end].iter().collect(),
        suffix: before_characters[before_end..].iter().collect(),
    }
}

/// Exact old/current token occurrence pairs inside one matched definition.
struct TokenCorrespondence {
    pairs: Vec<(usize, usize)>,
}

impl TokenCorrespondence {
    fn before_indices(&self) -> HashSet<usize> {
        self.pairs.iter().map(|(before, _)| *before).collect()
    }

    fn after_indices(&self) -> HashSet<usize> {
        self.pairs.iter().map(|(_, after)| *after).collect()
    }
}

fn align_tokens(
    before: &[Token<'_>],
    after: &[Token<'_>],
    include: impl Fn(&Token<'_>) -> bool,
) -> TokenCorrespondence {
    let before_indices = before
        .iter()
        .enumerate()
        .filter(|(_, token)| include(token))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let after_indices = after
        .iter()
        .enumerate()
        .filter(|(_, token)| include(token))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let before_values = before_indices
        .iter()
        .map(|index| {
            let token = &before[*index];
            (token.kind, token.field, token.text)
        })
        .collect::<Vec<_>>();
    let after_values = after_indices
        .iter()
        .map(|index| {
            let token = &after[*index];
            (token.kind, token.field, token.text)
        })
        .collect::<Vec<_>>();
    let matches = local_matches(&before_values, &after_values);

    let pairs = matches
        .into_iter()
        .map(|(before_index, after_index)| {
            (before_indices[before_index], after_indices[after_index])
        })
        .collect();
    TokenCorrespondence { pairs }
}

fn aligned_text_spans(
    before: &str,
    after: &str,
    syntax: SyntaxClass,
) -> (Vec<CodeSpan>, Vec<CodeSpan>) {
    let before_parts = text_parts(before);
    let after_parts = text_parts(after);
    let before_values = before_parts
        .iter()
        .map(|part| part.as_str())
        .collect::<Vec<_>>();
    let after_values = after_parts
        .iter()
        .map(|part| part.as_str())
        .collect::<Vec<_>>();
    let matches = local_matches(&before_values, &after_values);
    let before_context = matches
        .iter()
        .map(|(before_index, _)| *before_index)
        .collect::<HashSet<_>>();
    let after_context = matches
        .iter()
        .map(|(_, after_index)| *after_index)
        .collect::<HashSet<_>>();

    let before = before_parts
        .into_iter()
        .enumerate()
        .map(|(index, text)| CodeSpan {
            text,
            syntax,
            mark: if before_context.contains(&index) {
                DiffMark::Context
            } else {
                DiffMark::Removed
            },
        })
        .collect();
    let after = after_parts
        .into_iter()
        .enumerate()
        .map(|(index, text)| CodeSpan {
            text,
            syntax,
            mark: if after_context.contains(&index) {
                DiffMark::Context
            } else {
                DiffMark::Added
            },
        })
        .collect();
    (before, after)
}

fn text_parts(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut current_class = None;
    for character in text.chars() {
        let class = if character.is_whitespace() {
            0
        } else if character.is_alphanumeric() || character == '_' {
            1
        } else {
            2
        };
        if current_class.is_some_and(|current_class| current_class != class) {
            parts.push(std::mem::take(&mut current));
        }
        current.push(character);
        current_class = Some(class);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn render_definition(
    file: &SyntaxFile<'_>,
    definition: &Definition<'_>,
    context: &HashSet<usize>,
    changed: DiffMark,
    hide_comment_lines: bool,
) -> Vec<CodeLine> {
    render_source_lines(
        file.source,
        &file.lines,
        definition,
        context,
        changed,
        hide_comment_lines,
    )
}

fn render_source_lines(
    source: &str,
    lines: &[SourceLine],
    definition: &Definition<'_>,
    context: &HashSet<usize>,
    changed: DiffMark,
    hide_comment_lines: bool,
) -> Vec<CodeLine> {
    let mut rendered = Vec::new();
    for line in lines {
        if line.bytes.end <= definition.bytes.start || line.bytes.start >= definition.bytes.end {
            continue;
        }
        let text = &source[line.bytes.clone()];
        if hide_comment_lines && is_comment_only(text) {
            continue;
        }

        let mut spans = Vec::new();
        let mut cursor = line.bytes.start.max(definition.bytes.start);
        let line_end = line.bytes.end.min(definition.bytes.end);
        for (index, token) in definition.tokens.iter().enumerate() {
            if token.bytes.end <= cursor || token.bytes.start >= line_end {
                continue;
            }
            let token_start = token.bytes.start.max(cursor);
            let token_end = token.bytes.end.min(line_end);
            if cursor < token_start {
                push_code_span(
                    &mut spans,
                    &source[cursor..token_start],
                    SyntaxClass::Plain,
                    DiffMark::Context,
                );
            }
            let mark = if context.contains(&index) || (hide_comment_lines && token.is_comment) {
                DiffMark::Context
            } else {
                changed
            };
            push_code_span(
                &mut spans,
                &source[token_start..token_end],
                token.syntax_class(),
                mark,
            );
            cursor = token_end;
        }
        if cursor < line_end {
            push_code_span(
                &mut spans,
                &source[cursor..line_end],
                SyntaxClass::Plain,
                DiffMark::Context,
            );
        }
        rendered.push(CodeLine {
            number: line.number,
            spans,
        });
    }
    rendered
}

fn push_code_span(spans: &mut Vec<CodeSpan>, text: &str, syntax: SyntaxClass, mark: DiffMark) {
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

fn is_comment_only(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("//") || line.starts_with("/*") || line.starts_with('*')
}

const MAX_LOCAL_ALIGNMENT_CELLS: usize = 16_384;

/// Exact unique anchors split correspondence into bounded ordered gaps.
fn local_matches<T: Eq + Hash>(before: &[T], after: &[T]) -> Vec<(usize, usize)> {
    let mut before_positions = HashMap::<&T, Vec<usize>>::new();
    for (index, value) in before.iter().enumerate() {
        before_positions.entry(value).or_default().push(index);
    }
    let mut after_positions = HashMap::<&T, Vec<usize>>::new();
    for (index, value) in after.iter().enumerate() {
        after_positions.entry(value).or_default().push(index);
    }

    let candidates = before_positions
        .iter()
        .filter_map(|(value, before_indices)| {
            let [before_index] = before_indices.as_slice() else {
                return None;
            };
            let [after_index] = after_positions.get(value)?.as_slice() else {
                return None;
            };
            Some((*before_index, *after_index))
        })
        .collect::<Vec<_>>();
    let mut candidates = candidates;
    candidates.sort_unstable_by_key(|(before_index, _)| *before_index);
    let candidate_after = candidates
        .iter()
        .map(|(_, after_index)| *after_index)
        .collect::<Vec<_>>();
    let stable_after = increasing_subsequence(&candidate_after);
    let anchors = candidates
        .into_iter()
        .filter(|(_, after_index)| stable_after.contains(after_index))
        .collect::<Vec<_>>();

    let mut matches = Vec::new();
    let mut before_start = 0;
    let mut after_start = 0;
    for (before_anchor, after_anchor) in anchors
        .into_iter()
        .chain(std::iter::once((before.len(), after.len())))
    {
        matches.extend(
            align_gap(
                &before[before_start..before_anchor],
                &after[after_start..after_anchor],
            )
            .into_iter()
            .map(|(before_index, after_index)| {
                (before_start + before_index, after_start + after_index)
            }),
        );
        if before_anchor < before.len() && after_anchor < after.len() {
            matches.push((before_anchor, after_anchor));
        }
        before_start = before_anchor.saturating_add(1);
        after_start = after_anchor.saturating_add(1);
    }
    matches
}

fn align_gap<T: Eq + Hash>(before: &[T], after: &[T]) -> Vec<(usize, usize)> {
    if before.is_empty() || after.is_empty() {
        return Vec::new();
    }
    let cells = before.len().saturating_mul(after.len());
    if cells > MAX_LOCAL_ALIGNMENT_CELLS {
        return greedy_matches(before, after);
    }
    lcs_values(before, after)
}

/// Linear-memory fallback for an unusually large anchorless region.
fn greedy_matches<T: Eq + Hash>(before: &[T], after: &[T]) -> Vec<(usize, usize)> {
    let mut after_positions = HashMap::<&T, VecDeque<usize>>::new();
    for (index, value) in after.iter().enumerate() {
        after_positions.entry(value).or_default().push_back(index);
    }

    let mut matches = Vec::new();
    let mut after_floor = 0;
    for (before_index, value) in before.iter().enumerate() {
        let Some(positions) = after_positions.get_mut(value) else {
            continue;
        };
        while positions.front().is_some_and(|index| *index < after_floor) {
            positions.pop_front();
        }
        let Some(after_index) = positions.pop_front() else {
            continue;
        };
        matches.push((before_index, after_index));
        after_floor = after_index + 1;
    }
    matches
}

/// Quadratic alignment is reserved for small gaps between exact anchors.
fn lcs_values<T: Eq>(before: &[T], after: &[T]) -> Vec<(usize, usize)> {
    let width = after.len() + 1;
    let mut lengths = vec![0_usize; (before.len() + 1) * width];
    for before_index in (0..before.len()).rev() {
        for after_index in (0..after.len()).rev() {
            let value = if before[before_index] == after[after_index] {
                1 + lengths[(before_index + 1) * width + after_index + 1]
            } else {
                lengths[(before_index + 1) * width + after_index]
                    .max(lengths[before_index * width + after_index + 1])
            };
            lengths[before_index * width + after_index] = value;
        }
    }

    let mut matches = Vec::new();
    let mut before_index = 0;
    let mut after_index = 0;
    while before_index < before.len() && after_index < after.len() {
        if before[before_index] == after[after_index] {
            matches.push((before_index, after_index));
            before_index += 1;
            after_index += 1;
            continue;
        }
        let skip_before = lengths[(before_index + 1) * width + after_index];
        let skip_after = lengths[before_index * width + after_index + 1];
        if skip_before >= skip_after {
            before_index += 1;
        } else {
            after_index += 1;
        }
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{AFTER, BEFORE, LABEL};

    #[test]
    fn fixture_becomes_an_ordered_stream_of_windows() {
        let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");

        assert_eq!(diff.windows.len(), 6);
        assert!(matches!(
            diff.windows[4].rows.as_slice(),
            [DiffRow::Wordwise(_)]
        ));
        assert!(matches!(
            diff.windows[5].rows.as_slice(),
            [
                DiffRow::Code { .. },
                DiffRow::Code { .. },
                DiffRow::Code { .. }
            ]
        ));
    }

    #[test]
    fn definition_window_groups_treatments_and_elides_distant_context() {
        let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");
        let window = window_containing(&diff, "fn load_profile");

        assert_eq!(window.mapping.before, Some(23..31));
        assert_eq!(window.mapping.after, Some(16..24));
        assert_eq!(window.rows.len(), 8);
        assert_eq!(
            window
                .rows
                .iter()
                .filter(|row| matches!(row, DiffRow::Elision(_)))
                .count(),
            2
        );

        let linewise = window.rows.iter().find_map(|row| {
            let DiffRow::Linewise { before, after } = row else {
                return None;
            };
            Some((before.as_ref()?, after.as_ref()?))
        });
        let Some((before_comment, after_comment)) = linewise else {
            panic!("comment edit must stay inside its definition window");
        };
        assert!(line_text(before_comment).contains("already trusted"));
        assert!(line_text(after_comment).contains("must be revalidated"));
        assert!(line_text(after_comment).starts_with("    //"));

        let inline = window.rows.iter().find_map(|row| {
            let DiffRow::Code {
                line,
                role: CodeRole::Inline,
            } = row
            else {
                return None;
            };
            Some(line)
        });
        let Some(inline) = inline else {
            panic!("execution-point edit must use an inline treatment");
        };
        assert!(line_text(inline).contains("cached.and_then(validate_profile)"));
        assert!(
            inline
                .spans
                .iter()
                .any(|span| { span.text.contains("and_then") && span.mark == DiffMark::Added })
        );
    }

    #[test]
    fn one_context_line_between_edits_stays_visible() {
        let before = "fn run() {\n    before_one();\n\n    before_two();\n}\n";
        let after = "fn run() {\n    after_one();\n\n    after_two();\n}\n";
        let diff = diff_file("src/run.rs", before, after).expect("source must parse");
        let window = window_containing(&diff, "fn run");

        assert!(
            window
                .rows
                .iter()
                .all(|row| !matches!(row, DiffRow::Elision(_)))
        );
        assert!(window.rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Code {
                    line,
                    role: CodeRole::Context,
                } if line.number == 3 && line_text(line).is_empty()
            )
        }));
    }

    #[test]
    fn move_window_lives_in_the_present_and_elides_its_unchanged_body() {
        let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");
        let window = window_containing(&diff, "fn cache_key");

        assert_eq!(window.mapping.before, Some(16..22));
        assert_eq!(window.mapping.after, Some(38..44));
        assert_eq!(window.rows.len(), 3);

        let DiffRow::Moved {
            before: Some(before),
            after,
        } = &window.rows[0]
        else {
            panic!("move must begin with an old-to-current line mapping");
        };
        assert_eq!((*before, after.number), (16, 38));
        assert!(line_text(after).contains("fn cache_key"));

        let DiffRow::Elision(mapping) = &window.rows[1] else {
            panic!("unchanged moved body must be abbreviated");
        };
        assert_eq!(mapping.before, Some(17..21));
        assert_eq!(mapping.after, Some(39..43));

        let DiffRow::Moved {
            before: None,
            after,
        } = &window.rows[2]
        else {
            panic!("the closing line must use only its current line number");
        };
        assert_eq!(after.number, 43);
        assert_eq!(line_text(after), "}");
    }

    #[test]
    fn moved_definition_keeps_its_single_body_line_visible() {
        let before = "fn first() {\n    first_body();\n}\n\nfn second() {\n    second_body();\n}\n";
        let after = "fn second() {\n    second_body();\n}\n\nfn first() {\n    first_body();\n}\n";
        let diff = diff_file("src/run.rs", before, after).expect("source must parse");
        let moved = diff
            .windows
            .iter()
            .find(|window| matches!(window.rows.first(), Some(DiffRow::Moved { .. })))
            .expect("one definition must be presented as moved");

        assert!(matches!(
            moved.rows.as_slice(),
            [
                DiffRow::Moved { .. },
                DiffRow::Code {
                    role: CodeRole::Context,
                    ..
                },
                DiffRow::Moved { .. }
            ]
        ));
    }

    #[test]
    fn expanded_fixture_adds_distinct_policy_and_payload_windows() {
        let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");

        let policy = window_containing(&diff, "fn should_refresh");
        assert!(window_has_text(policy, "Only stale profiles"));
        assert!(window_has_text(policy, "Stale and legacy profiles"));
        assert!(window_has_text(
            policy,
            "profile.schema < 4 || age > Duration::from_secs(300)"
        ));
        assert!(window_has_added_text(policy, "schema"));

        let normalization = window_containing(&diff, "fn display_label");
        assert!(window_has_added_text(normalization, "replace"));
        assert!(window_has_text(
            normalization,
            "profile.display_name.trim().to_owned().replace('\\n', \" \")"
        ));
    }

    #[test]
    fn imports_and_reflow_use_distinct_late_windows() {
        let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");

        let import = diff
            .windows
            .iter()
            .find(|window| matches!(window.rows.as_slice(), [DiffRow::Wordwise(_)]))
            .expect("fixture must include a wordwise import window");
        let formatting = window_containing(&diff, "fn render_response");
        let [DiffRow::Wordwise(import)] = import.rows.as_slice() else {
            panic!("import replacement must be a wordwise row");
        };
        assert_eq!(import.prefix, "use crate::telemetry::");
        assert_eq!(import.removed, "legacy_counter");
        assert_eq!(import.added, "{Metric, ReviewMeter}");
        assert_eq!(import.suffix, ";");

        assert_eq!(formatting.mapping.before, Some(32..38));
        assert_eq!(formatting.mapping.after, Some(25..28));
        assert!(matches!(
            formatting.rows.as_slice(),
            [
                DiffRow::Code {
                    role: CodeRole::Context,
                    ..
                },
                DiffRow::Code {
                    role: CodeRole::Reflow,
                    ..
                },
                DiffRow::Code {
                    role: CodeRole::Context,
                    ..
                }
            ]
        ));
    }

    #[test]
    fn identical_source_has_no_review_work() {
        let source = "use std::fmt;\n\nfn stable() { fmt::write(); }\n";
        let diff = diff_file("src/stable.rs", source, source).expect("source must parse");

        assert!(diff.windows.is_empty());
    }

    #[test]
    fn identical_recovered_source_has_no_review_work() {
        let source = "fn broken(value: u32 {}\n";

        let diff = diff_file("src/broken.rs", source, source).expect("parser must recover");

        assert!(diff.windows.is_empty());
    }

    #[test]
    fn duplicate_definition_names_keep_one_to_one_correspondence() {
        let before =
            "impl Thing { fn first() { old(); } }\nimpl Thing { fn second() { stable(); } }\n";
        let after =
            "impl Thing { fn first() { new(); } }\nimpl Thing { fn second() { stable(); } }\n";

        let diff = diff_file("src/thing.rs", before, after).expect("source must parse");

        assert_eq!(diff.windows.len(), 1);
        assert!(window_has_added_text(&diff.windows[0], "new"));
        assert!(!window_has_text(&diff.windows[0], "second"));
    }

    #[test]
    fn inserted_and_removed_comments_keep_their_source_side() {
        let plain = "fn run() {\n    work();\n}\n";
        let commented = "fn run() {\n    // explain why\n    work();\n}\n";

        let added = diff_file("src/run.rs", plain, commented).expect("source must parse");
        let removed = diff_file("src/run.rs", commented, plain).expect("source must parse");

        assert!(matches!(
            added.windows[0].rows.as_slice(),
            [DiffRow::Linewise {
                before: None,
                after: Some(_)
            }]
        ));
        assert!(matches!(
            removed.windows[0].rows.as_slice(),
            [DiffRow::Linewise {
                before: Some(_),
                after: None
            }]
        ));
    }

    #[test]
    fn comments_inside_an_added_definition_are_added_content() {
        let after = "fn run() {\n    // explain why\n    work();\n}\n";

        let diff = diff_file("src/run.rs", "", after).expect("source must parse");

        assert!(window_has_added_text(&diff.windows[0], "explain why"));
    }

    #[test]
    fn unknown_syntax_uses_aligned_plain_lines() {
        let before = "alpha\nold value\nomega\n";
        let after = "alpha\nnew value\nomega\n";

        let diff = diff_file("notes.txt", before, after).expect("plain diff cannot fail");

        assert!(!diff.generated);
        assert_eq!(diff.windows.len(), 1);
        assert_eq!(diff.windows[0].mapping.before, Some(1..4));
        assert_eq!(diff.windows[0].mapping.after, Some(1..4));
        let changed = diff.windows[0].rows.iter().find_map(|row| {
            let DiffRow::Linewise {
                before: Some(before),
                after: Some(after),
            } = row
            else {
                return None;
            };
            Some((before, after))
        });
        let Some((before, after)) = changed else {
            panic!("plain replacement needs both source sides");
        };
        assert_eq!((before.number, after.number), (2, 2));
        assert!(
            before
                .spans
                .iter()
                .all(|span| span.syntax == SyntaxClass::Plain)
        );
        assert!(
            before
                .spans
                .iter()
                .any(|span| span.text == "old" && span.mark == DiffMark::Removed)
        );
        assert!(
            after
                .spans
                .iter()
                .any(|span| span.text == "new" && span.mark == DiffMark::Added)
        );
    }

    #[test]
    fn plain_insertions_and_deletions_keep_their_source_numbers() {
        let before = "one\nremove\ntwo\nthree\n";
        let after = "one\ntwo\nadd\nthree\n";

        let diff = diff_file("notes", before, after).expect("plain diff cannot fail");
        let rows = &diff.windows[0].rows;

        assert!(rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Linewise {
                    before: Some(line),
                    after: None,
                } if line.number == 2 && line_text(line) == "remove"
            )
        }));
        assert!(rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Linewise {
                    before: None,
                    after: Some(line),
                } if line.number == 3 && line_text(line) == "add"
            )
        }));
    }

    #[test]
    fn distant_plain_changes_become_focused_windows() {
        let before = "one\nold two\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\nold twelve\nthirteen\nfourteen\n";
        let after = "one\nnew two\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\nnew twelve\nthirteen\nfourteen\n";

        let diff = diff_file("notes.md", before, after).expect("plain diff cannot fail");

        assert_eq!(diff.windows.len(), 2);
        assert_eq!(diff.windows[0].mapping.before, Some(1..6));
        assert_eq!(diff.windows[0].mapping.after, Some(1..6));
        assert_eq!(diff.windows[1].mapping.before, Some(9..15));
        assert_eq!(diff.windows[1].mapping.after, Some(9..15));
    }

    #[test]
    fn plain_hunks_do_not_hide_one_context_line() {
        let before = "old first\none\ntwo\nthree\nfour\nfive\nsix\nseven\nold last\n";
        let after = "new first\none\ntwo\nthree\nfour\nfive\nsix\nseven\nnew last\n";

        let diff = diff_file("notes.txt", before, after).expect("plain diff cannot fail");

        assert_eq!(diff.windows.len(), 1);
        assert!(diff.windows[0].rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Code {
                    line,
                    role: CodeRole::Context,
                } if line.number == 5 && line_text(line) == "four"
            )
        }));
    }

    #[test]
    fn plain_diff_retains_end_of_file_newline_changes() {
        let diff = diff_file("notes.txt", "same\n", "same").expect("plain diff cannot fail");

        assert!(matches!(
            diff.windows[0].rows.as_slice(),
            [
                DiffRow::Linewise {
                    before: Some(before),
                    after: Some(after),
                },
                DiffRow::LineEnding {
                    before: Some(LineEnding::Lf),
                    after: Some(LineEnding::Missing),
                }
            ] if before.number == 1
                && after.number == 1
                && before.spans.iter().all(|span| span.mark == DiffMark::Removed)
                && after.spans.iter().all(|span| span.mark == DiffMark::Added)
        ));
    }

    #[test]
    fn generated_rust_is_flagged_and_forced_through_plain_diff() {
        let before = "// @generated by build.rs\nuse crate::old;\n";
        let after = "use crate::new;\n";

        let diff = diff_file("src/bindings.rs", before, after).expect("plain diff cannot fail");
        let marker_added =
            diff_file("src/bindings.rs", after, before).expect("plain diff cannot fail");

        assert!(diff.generated);
        assert!(marker_added.generated);
        assert!(
            diff.windows[0]
                .rows
                .iter()
                .any(|row| matches!(row, DiffRow::Linewise { .. }))
        );
        assert!(
            diff.windows[0]
                .rows
                .iter()
                .all(|row| !matches!(row, DiffRow::Wordwise(_)))
        );
    }

    #[test]
    fn generated_marker_is_exact_and_header_bounded() {
        let mut below_header = "ordinary\n".repeat(20);
        below_header.push_str("// @generated\n");

        assert!(has_generated_marker("// @generated\ncontent\n"));
        assert!(has_generated_marker(
            "# This file is automatically @generated by Cargo.\n"
        ));
        assert!(has_generated_marker(
            "// package @generated/client; file @generated\n"
        ));
        assert!(!has_generated_marker("// @Generated\ncontent\n"));
        assert!(!has_generated_marker("// contact foo@generated.example\n"));
        assert!(!has_generated_marker(
            "import client from \"@generated/client\";\n"
        ));
        assert!(!has_generated_marker(
            "const PACKAGE: &str = \"@generated\";\n"
        ));
        assert!(!has_generated_marker(&below_header));
    }

    #[test]
    fn malformed_or_unprojected_rust_falls_back_to_plain_rows() {
        for (before, after) in [
            ("fn broken(value: u32 {}\n", "fn broken(value: u64 {}\n"),
            ("// old license\n", "// new license\n"),
            (
                "// old license\nfn run() { old(); }\n",
                "// new license\nfn run() { new(); }\n",
            ),
            ("fn run() { old(); }\n", "fn run() { new(); }"),
        ] {
            let diff = diff_file("src/lib.rs", before, after).expect("plain fallback cannot fail");

            assert!(!diff.windows.is_empty());
            assert!(
                diff.windows
                    .iter()
                    .flat_map(|window| &window.rows)
                    .any(|row| matches!(row, DiffRow::Linewise { .. }))
            );
        }
    }

    #[test]
    fn identical_plain_source_has_no_review_work() {
        let source = "plain text\nwith no grammar\n";

        let diff = diff_file("README", source, source).expect("plain diff cannot fail");

        assert!(diff.windows.is_empty());
    }

    #[test]
    fn large_anchorless_alignment_uses_the_bounded_fallback() {
        let before = vec!["same"; 200];
        let after = vec!["same"; 200];

        let matches = local_matches(&before, &after);

        assert_eq!(matches.len(), 200);
        assert_eq!(matches.first(), Some(&(0, 0)));
        assert_eq!(matches.last(), Some(&(199, 199)));
    }

    fn line_text(line: &CodeLine) -> String {
        line.spans.iter().map(|span| span.text.as_str()).collect()
    }

    fn window_containing<'a>(diff: &'a FileDiff, needle: &str) -> &'a DiffWindow {
        diff.windows
            .iter()
            .find(|window| window_has_text(window, needle))
            .unwrap_or_else(|| panic!("fixture must contain a window for {needle}"))
    }

    fn window_has_text(window: &DiffWindow, needle: &str) -> bool {
        window.rows.iter().any(|row| match row {
            DiffRow::Code { line, .. } | DiffRow::Moved { after: line, .. } => {
                line_text(line).contains(needle)
            }
            DiffRow::Linewise { before, after } => {
                before
                    .as_ref()
                    .is_some_and(|line| line_text(line).contains(needle))
                    || after
                        .as_ref()
                        .is_some_and(|line| line_text(line).contains(needle))
            }
            DiffRow::Wordwise(diff) => [
                diff.prefix.as_str(),
                diff.removed.as_str(),
                diff.added.as_str(),
                diff.suffix.as_str(),
            ]
            .concat()
            .contains(needle),
            DiffRow::LineEnding { .. } => false,
            DiffRow::Elision(_) => false,
        })
    }

    fn window_has_added_text(window: &DiffWindow, needle: &str) -> bool {
        window.rows.iter().any(|row| match row {
            DiffRow::Code { line, .. } | DiffRow::Moved { after: line, .. } => line
                .spans
                .iter()
                .any(|span| span.mark == DiffMark::Added && span.text.contains(needle)),
            DiffRow::Linewise { after, .. } => after.as_ref().is_some_and(|after| {
                after
                    .spans
                    .iter()
                    .any(|span| span.mark == DiffMark::Added && span.text.contains(needle))
            }),
            DiffRow::Wordwise(diff) => diff.added.contains(needle),
            DiffRow::LineEnding { .. } => false,
            DiffRow::Elision(_) => false,
        })
    }
}
