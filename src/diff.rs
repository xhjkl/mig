mod context;
mod correspondence;
mod presentation;
mod refine;
mod source;
mod syntax;
mod tree_diff;

use anyhow::{Result, bail};
use std::ops::Range;
use std::path::Path;

pub use presentation::{DiffMark, PresentedFile, ReviewRow, SourceRow, WordDiff};
#[cfg(test)]
pub use presentation::{ReviewHunk, SourceSpan};
pub use source::LineEnding;

/// Largest physical-line arena expanded for one revision.
pub const MAX_REVISION_LINES: usize = 100_000;

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

/// One-based, half-open before/after bounds covered by a review row or hunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineCoverage {
    pub before: Option<Range<usize>>,
    pub after: Option<Range<usize>>,
}

/// Parse, lower, reconcile, refine, and present one pair of source revisions.
pub fn diff_file(path: &str, before: &str, after: &str) -> Result<PresentedFile> {
    let generated = has_generated_marker(before) || has_generated_marker(after);
    if before == after {
        return Ok(PresentedFile {
            path: path.to_owned(),
            generated,
            hunks: Vec::new(),
        });
    }
    let before_lines = before.lines().count();
    let after_lines = after.lines().count();
    if before_lines > MAX_REVISION_LINES || after_lines > MAX_REVISION_LINES {
        bail!("source exceeds the {MAX_REVISION_LINES}-line per-revision syntax limit");
    }

    let syntax = syntax::syntax_pair(Path::new(path), before, after, generated)?;
    let correspondence = correspondence::correspond(&syntax);
    let raw_hunks = tree_diff::raw_hunks(&syntax, &correspondence);
    let refined_hunks = refine::refine_hunks(&syntax, raw_hunks);
    let hunks = presentation::present_hunks(&syntax.before, &syntax.after, refined_hunks);
    Ok(PresentedFile {
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
mod corpus_tests;
#[cfg(test)]
mod tests;
