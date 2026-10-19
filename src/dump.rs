//! Plain-text rendering of review rows for inspection outside the terminal interface.

use crate::diff::{DiffMark, LineEnding, PresentedFile, ReviewRow, SourceRow};
use crate::review::{FileNotice, ReviewEntry};
use anyhow::Result;
use std::io::{self, BufWriter, Write};

pub fn print(reviews: &[ReviewEntry]) -> Result<()> {
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    for (index, review) in reviews.iter().enumerate() {
        if index > 0 {
            writeln!(out)?;
        }
        match review {
            ReviewEntry::Diff(diff) => print_diff(&mut out, diff)?,
            ReviewEntry::Notice(notice) => print_notice(&mut out, notice)?,
        }
    }
    out.flush()?;
    Ok(())
}

fn print_notice(out: &mut impl Write, notice: &FileNotice) -> Result<()> {
    match notice {
        FileNotice::TooLarge { path, .. } => writeln!(out, "=== {path} (too large)")?,
        FileNotice::TooManyLines { path, .. } => writeln!(out, "=== {path} (too many lines)")?,
    }
    Ok(())
}

fn print_diff(out: &mut impl Write, diff: &PresentedFile) -> Result<()> {
    let badge = if diff.generated { " @generated" } else { "" };
    writeln!(
        out,
        "=== {}{badge} [consumers={}]",
        diff.path, diff.consumers
    )?;
    for (index, hunk) in diff.hunks.iter().enumerate() {
        let contract = if hunk.impact.contract {
            " contract"
        } else {
            ""
        };
        writeln!(
            out,
            "--- hunk {} [{:?} consumers={}{contract}] {}",
            index + 1,
            hunk.nature,
            hunk.consumers,
            hunk.impact
                .symbols
                .iter()
                .map(|symbol| match &symbol.owner {
                    Some(owner) => format!("{owner}.{}", symbol.name),
                    None => symbol.name.clone(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        )?;
        for row in &hunk.rows {
            print_row(out, row)?;
        }
    }
    Ok(())
}

fn print_row(out: &mut impl Write, row: &ReviewRow) -> Result<()> {
    match row {
        ReviewRow::Current(line) => {
            let marker = if line.has_changes() { '*' } else { ' ' };
            writeln!(out, "{marker} {:>5} │ {}", line.number, text(line))?;
        }
        ReviewRow::Reflow(line) => writeln!(out, "~ {:>5} │ {}", line.number, text(line))?,
        ReviewRow::Removed(line) => writeln!(out, "- {:>5} │ {}", line.number, text(line))?,
        ReviewRow::Added(line) => writeln!(out, "+ {:>5} │ {}", line.number, text(line))?,
        ReviewRow::LineEnding { before, after } => {
            writeln!(out, "⏎       │ {} → {}", ending(*before), ending(*after))?
        }
        ReviewRow::Moved { before, after } => match before {
            Some(before) => writeln!(out, "{before:>3}→{:<3} │ {}", after.number, text(after))?,
            None => writeln!(out, "  →{:<3} │ {}", after.number, text(after))?,
        },
        ReviewRow::Wordwise(word) => {
            let number = word.after_line.or(word.before_line).unwrap_or(0);
            writeln!(
                out,
                "w {number:>5} │ {}{{-{}-}}{{+{}+}}{}",
                word.prefix, word.removed, word.added, word.suffix
            )?
        }
        ReviewRow::Elision(coverage) => {
            let before = coverage
                .before
                .as_ref()
                .map(|range| format!("{}..{}", range.start, range.end))
                .unwrap_or_default();
            let after = coverage
                .after
                .as_ref()
                .map(|range| format!("{}..{}", range.start, range.end))
                .unwrap_or_default();
            writeln!(out, "⋮       │ ⋮ ({before} / {after})")?
        }
        ReviewRow::FileBoundary => writeln!(out, "        │ EOF")?,
    }
    Ok(())
}

fn ending(ending: Option<LineEnding>) -> &'static str {
    match ending {
        None => "∅",
        Some(LineEnding::Missing) => "no newline",
        Some(LineEnding::Lf) => "LF",
        Some(LineEnding::CrLf) => "CRLF",
    }
}

struct SourceSpanText {
    mark: DiffMark,
    text: String,
}

/// Inline change marks: `{+added+}` and `{-removed-}` around marked spans.
fn text(line: &SourceRow) -> String {
    let mut rendered = String::new();
    let mut merged: Vec<(DiffMark, String)> = Vec::new();
    for span in &line.spans {
        match merged.last_mut() {
            Some((mark, text)) if *mark == span.mark => text.push_str(&span.text),
            _ => merged.push((span.mark, span.text.clone())),
        }
    }
    for (mark, text) in merged {
        let span = SourceSpanText { mark, text };
        match span.mark {
            DiffMark::Context => rendered.push_str(&span.text),
            DiffMark::Added => {
                rendered.push_str("{+");
                rendered.push_str(&span.text);
                rendered.push_str("+}");
            }
            DiffMark::Removed => {
                rendered.push_str("{-");
                rendered.push_str(&span.text);
                rendered.push_str("-}");
            }
        }
    }
    rendered
}
