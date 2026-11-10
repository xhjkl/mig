use super::*;
use mig_core::{DiffMark, Elision, Highlight};

fn row(index: usize, text: &str) -> SourceRow {
    SourceRow {
        number: LineNumber::from_index(index),
        spans: vec![mig_core::document::SourceSpan {
            text: text.to_owned(),
            highlight: Highlight::Plain,
            mark: DiffMark::Context,
        }],
    }
}

#[test]
fn the_checker_rejects_token_boundary_loss_in_a_reflow() {
    let before = "int f(int x, int y) { return x + ++y; }\n";
    let after = "int f(int x, int y) { return x++ + y; }\n";
    let mut presented = diff_file("a.c", before, after).expect("source diff");
    presented.hunks[0].excerpt.rows = vec![Row::Reflow {
        before: vec![LineNumber::from_index(0)],
        after: vec![row(0, after.trim_end_matches('\n'))],
    }];
    let checked = check_presentation("a.c", before, after, &presented).expect("audit");
    assert!(
        checked
            .findings
            .iter()
            .any(|finding| finding.kind == Kind::MoveMismatch)
    );
}

#[test]
fn a_gap_elision_cannot_certify_changed_source() {
    let before = "old\nleft\n";
    let after = "new\nright\n";
    let mut presented = diff_file("a.txt", before, after).expect("source diff");
    presented.hunks[0].excerpt.rows = vec![
        Row::Elision(Elision::Gap {
            before: Some(LineSpan::from_indices(0..1)),
            after: LineSpan::from_indices(0..1),
        }),
        Row::Changed {
            before: row(1, "left"),
            after: row(1, "right"),
        },
    ];
    let checked = check_presentation("a.txt", before, after, &presented).expect("audit");
    assert!(
        checked
            .findings
            .iter()
            .any(|finding| finding.kind == Kind::MissedChange)
    );
}

#[test]
fn a_move_elision_must_preserve_its_tokens() {
    let before = "old\nleft\n";
    let after = "new\nright\n";
    let mut presented = diff_file("a.txt", before, after).expect("source diff");
    presented.hunks[0].excerpt.rows = vec![
        Row::Elision(Elision::Move {
            before: LineSpan::from_indices(0..1),
            after: LineSpan::from_indices(0..1),
        }),
        Row::Changed {
            before: row(1, "left"),
            after: row(1, "right"),
        },
    ];
    let checked = check_presentation("a.txt", before, after, &presented).expect("audit");
    assert!(
        checked
            .findings
            .iter()
            .any(|finding| finding.kind == Kind::MoveMismatch)
    );
}

#[test]
fn the_checker_requires_a_final_newline_notice_even_during_reflow() {
    let before = "fn f() { call(); }";
    let after = "fn f() {\n    call();\n}\n";
    let mut presented = diff_file("a.rs", before, after).expect("source diff");
    for hunk in &mut presented.hunks {
        hunk.excerpt
            .rows
            .retain(|row| !matches!(row, Row::Ending(_)));
    }
    let checked = check_presentation("a.rs", before, after, &presented).expect("audit");
    assert!(
        checked
            .findings
            .iter()
            .any(|finding| finding.kind == Kind::MissedChange)
    );
}
