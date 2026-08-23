mod change;
mod context;
mod correspondence;
mod plan;
mod projection;
mod render;
mod source;

use anyhow::{Result, bail};
use std::ops::Range;
use std::path::Path;

pub use source::LineEnding;

/// Largest physical-line arena expanded for one revision.
pub const MAX_REVISION_LINES: usize = 100_000;

/// Physical lines without allocating the source geometry they will later own.
pub fn revision_line_count(source: &str) -> usize {
    source.lines().count()
}

/// Coarse language syntax category understood by the terminal palette.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxClass {
    Plain,
    Keyword,
    Identifier,
    Type,
    Literal,
    String,
    Comment,
    Punctuation,
}

/// Diff role layered over syntax styling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffMark {
    Context,
    Removed,
    Added,
}

/// Smallest independently styled slice of one displayed source line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplaySpan {
    pub text: String,
    pub syntax: SyntaxClass,
    pub mark: DiffMark,
}

/// Original source line retained inside a bounded diff hunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayLine {
    pub number: usize,
    pub spans: Vec<DisplaySpan>,
}

impl DisplayLine {
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

/// One-based, half-open before/after bounds covered by a review row or hunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineCoverage {
    pub before: Option<Range<usize>>,
    pub after: Option<Range<usize>>,
}

/// Presentation-ready row chosen while planning a bounded diff hunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiffRow {
    /// Current-world source; span marks distinguish changed payload from context.
    Line(DisplayLine),
    /// Current-world source whose payload survived a physical-layout change.
    Reflow(DisplayLine),
    /// Paired or one-sided physical-line change.
    LineChange {
        before: Option<DisplayLine>,
        after: Option<DisplayLine>,
    },
    LineEnding {
        before: Option<LineEnding>,
        after: Option<LineEnding>,
    },
    Moved {
        before: Option<usize>,
        after: DisplayLine,
    },
    Wordwise(WordDiff),
    Elision(LineCoverage),
    /// Ordered sentinel that makes the displayed file boundary part of the row stream.
    FileBoundary,
}

/// Bounded view into a file containing related, presentation-ready rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hunk {
    pub coverage: LineCoverage,
    pub rows: Vec<DiffRow>,
}

/// Render-ready stream of bounded hunks for one source file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileDiff {
    pub path: String,
    /// Whether either source revision declares itself generated near its header.
    pub generated: bool,
    pub hunks: Vec<Hunk>,
}

/// Project, find language-neutral correspondence, then plan render-ready rows.
pub fn diff_file(path: &str, before: &str, after: &str) -> Result<FileDiff> {
    let generated = has_generated_marker(before) || has_generated_marker(after);
    if before == after {
        return Ok(FileDiff {
            path: path.to_owned(),
            generated,
            hunks: Vec::new(),
        });
    }
    let before_lines = revision_line_count(before);
    let after_lines = revision_line_count(after);
    if before_lines > MAX_REVISION_LINES || after_lines > MAX_REVISION_LINES {
        bail!("source exceeds the {MAX_REVISION_LINES}-line per-revision projection limit");
    }

    let pair = projection::project_pair(Path::new(path), before, after, generated)?;
    let correspondence = correspondence::correspond(&pair);
    let hunks = plan::plan_hunks(&pair, &correspondence);
    Ok(FileDiff {
        path: path.to_owned(),
        generated,
        hunks,
    })
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

#[cfg(test)]
mod tests;
