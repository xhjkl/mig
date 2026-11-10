//! Mig's diff core: from two revisions of one file to rows a reviewer can read.
//!
//! The crate has no notion of Git or terminals. Feed it a path (for the language), the two
//! texts, and read back either the full presentation (`diff_file`) or any intermediate layer.
//!
//! Stages share immutable source geometry; each stage consumes the evidence it needs:
//!
//! 1. `source` indexes the text into physical lines.
//! 2. `syntax` parses with tree-sitter and lowers the parse into a
//!    language-neutral tree; the per-language modules only annotate.
//! 3. `matching` pairs nodes across the two trees.
//! 4. `islands` lowers the node mapping onto lines: kept pairs and the islands
//!    of change between them, snapped to whole constructs. `islands::verify`
//!    checks that account against the text.
//! 5. `document` lays islands out as hunks of rows with context and orientation.
//! 6. `attribution` attributes hunks to the declarations they touch.
//!
//! Rendering (terminal or plain text) sits outside and never decides what changed.

mod align;
pub mod attribution;
pub mod document;
pub mod islands;
pub mod matching;
pub mod source;
pub mod syntax;

#[cfg(any(test, feature = "fixtures"))]
pub mod fixture;

use anyhow::{Result, bail};
use std::path::Path;

pub use attribution::{Attribution, DeclarationChanges, FileSymbols, TouchedSymbol};
pub use document::{DiffMark, Elision, MoveEdge, Row, SourceRow};
pub use islands::Priority;
pub use source::{EndingChange, LineEnding, LineNumber, LineSpan, Terminator};
pub use syntax::{Comparison, LineFallback};

/// Per-revision line limit, bounding syntax storage even for files of empty lines.
pub const MAX_REVISION_LINES: usize = 100_000;

/// Language-neutral source highlights, independent of change emphasis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Highlight {
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
    pub before: Option<LineSpan>,
    pub after: Option<LineSpan>,
}

/// A displayed excerpt with attribution to the declarations its changes touch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hunk {
    pub excerpt: document::Excerpt,
    pub attribution: Attribution,
}

/// Styled hunks in review-priority order, which may differ from source order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileDiff {
    pub path: String,
    /// Whether either source revision declares itself generated near its header.
    pub generated: bool,
    pub comparison: Comparison,
    pub hunks: Vec<Hunk>,
    pub symbols: FileSymbols,
}

/// Build review hunks using the path's language and both revisions' syntax.
/// Unchanged input produces no hunks; revisions without reliable syntax use exact lines.
pub fn diff_file(path: &str, before: &str, after: &str) -> Result<FileDiff> {
    let generated = has_generated_marker(before) || has_generated_marker(after);
    if before == after {
        return Ok(FileDiff {
            path: path.to_owned(),
            generated,
            comparison: Comparison::Unchanged,
            hunks: Vec::new(),
            symbols: FileSymbols::default(),
        });
    }
    let before_lines = before.lines().count();
    let after_lines = after.lines().count();
    if before_lines > MAX_REVISION_LINES || after_lines > MAX_REVISION_LINES {
        bail!("source exceeds the {MAX_REVISION_LINES}-line per-revision syntax limit");
    }

    let syntax = syntax::syntax_pair(Path::new(path), before, after, generated)?;
    let (syntax, islands) = match verified_islands(&syntax) {
        Err(error) => {
            let syntax = syntax::line_pair(before, after, LineFallback::VerificationFailure(error));
            let islands = verified_islands(&syntax)
                .map_err(|error| anyhow::anyhow!("line comparison failed verification: {error}"))?;
            (syntax, islands)
        }
        Ok(islands) => (syntax, islands),
    };
    let document = document::layout(&syntax, &islands, document::Options::default());
    let (attributions, symbols) = attribution::attribute(&syntax, &islands, &document.sections);
    let hunks = document
        .sections
        .into_iter()
        .zip(attributions)
        .map(|(section, attribution)| Hunk {
            excerpt: section.excerpt,
            attribution,
        })
        .collect();
    Ok(FileDiff {
        path: path.to_owned(),
        generated,
        comparison: syntax.comparison,
        hunks,
        symbols,
    })
}

/// Match, lower to islands, and check the account against the text.
fn verified_islands(syntax: &syntax::SyntaxPair<'_, '_>) -> Result<islands::Islands, String> {
    let mapping = matching::correspond(syntax);
    let islands = islands::islands(syntax, &mapping);
    islands::verify(syntax, &islands)?;
    Ok(islands)
}

/// Limit marker recognition to the header so body text does not classify the file.
pub fn has_generated_marker(source: &str) -> bool {
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
