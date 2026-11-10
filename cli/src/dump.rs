//! Plain-text rendering of review rows for inspection outside the terminal interface.

use crate::review::ReviewFile;
use crate::review::{FileNotice, ReviewEntry};
use anyhow::Result;
use mig_core::{DiffMark, LineEnding, MoveEdge, Row, SourceRow};
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

pub(crate) fn print_diff(out: &mut impl Write, diff: &ReviewFile) -> Result<()> {
    let badge = if diff.generated { " @generated" } else { "" };
    let degraded = if matches!(diff.comparison, mig_core::Comparison::Lines(_)) {
        " (plain lines)"
    } else {
        ""
    };
    writeln!(
        out,
        "=== {}{badge}{degraded} [consumers={}]",
        diff.path, diff.consumers
    )?;
    for (index, hunk) in diff.hunks.iter().enumerate() {
        let contract = if hunk.attribution.changes.contract {
            " contract"
        } else {
            ""
        };
        writeln!(
            out,
            "--- hunk {} [{:?} consumers={}{contract}] {}",
            index + 1,
            hunk.excerpt.priority,
            hunk.consumers,
            hunk.attribution
                .symbols()
                .map(|symbol| match &symbol.owner {
                    Some(owner) => format!("{owner}.{}", symbol.name),
                    None => symbol.name.clone(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        )?;
        for row in &hunk.excerpt.rows {
            print_row(out, row)?;
        }
    }
    Ok(())
}

fn print_row(out: &mut impl Write, row: &Row) -> Result<()> {
    match row {
        Row::Context { after, .. } => writeln!(out, "  {:>5} │ {}", after.number, text(after))?,
        Row::Crumb { after, .. } => writeln!(out, "§ {:>5} │ {}", after.number, text(after))?,
        Row::Reflow { after, .. } => {
            for line in after {
                writeln!(out, "~ {:>5} │ {}", line.number, text(line))?;
            }
        }
        Row::Removed(line) => writeln!(out, "- {:>5} │ {}", line.number, text(line))?,
        Row::Added(line) => writeln!(out, "+ {:>5} │ {}", line.number, text(line))?,
        Row::Changed { before, after } => {
            writeln!(out, "- {:>5} │ {}", before.number, text(before))?;
            writeln!(out, "+ {:>5} │ {}", after.number, text(after))?;
        }
        Row::Ending(change) => {
            let before = change.before();
            let after = change.after();
            writeln!(out, "⏎       │ {} → {}", ending(before), ending(after))?
        }
        Row::Moved {
            before,
            after,
            edge,
        } => match edge {
            MoveEdge::Start | MoveEdge::Only => {
                writeln!(out, "{before:>3}→{:<3} │ {}", after.number, text(after))?
            }
            MoveEdge::Middle | MoveEdge::End => {
                writeln!(out, "  →{:<3} │ {}", after.number, text(after))?
            }
        },
        Row::Elision(elision) => {
            let before = elision
                .before()
                .map(|range| format!("{}..{}", range.start(), range.end()))
                .unwrap_or_default();
            let range = elision.after();
            let after = format!("{}..{}", range.start(), range.end());
            writeln!(out, "⋮       │ ⋮ ({before} / {after})")?
        }
        Row::FileEnd => writeln!(out, "        │ EOF")?,
    }
    Ok(())
}

fn ending(ending: Option<mig_core::Terminator>) -> String {
    match ending {
        None => "∅".to_owned(),
        Some(mig_core::Terminator {
            line,
            ending: LineEnding::Missing,
        }) => format!("line {line}: no newline"),
        Some(mig_core::Terminator {
            line,
            ending: LineEnding::Lf,
        }) => format!("line {line}: LF"),
        Some(mig_core::Terminator {
            line,
            ending: LineEnding::CrLf,
        }) => format!("line {line}: CRLF"),
    }
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
        match mark {
            DiffMark::Context => rendered.push_str(&text),
            DiffMark::Added => {
                rendered.push_str("{+");
                rendered.push_str(&text);
                rendered.push_str("+}");
            }
            DiffMark::Removed => {
                rendered.push_str("{-");
                rendered.push_str(&text);
                rendered.push_str("-}");
            }
        }
    }
    rendered
}
