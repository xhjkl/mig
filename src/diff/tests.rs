use super::*;
use crate::fixture::{AFTER, BEFORE, LABEL};
use std::collections::HashSet;

#[test]
fn definition_hunk_keeps_hierarchy_local_context_and_distant_elision() {
    let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");
    let hunk = hunk_containing(&diff, "fn gamma");

    assert!(
        hunk.rows
            .iter()
            .any(|row| matches!(row, ReviewRow::Elision(_)))
    );
    for context in ["fn gamma", "let epsilon", "gamma.filter"] {
        assert!(hunk_has_text(hunk, context), "missing {context:?}");
    }

    let before_comment = hunk.rows.iter().find_map(|row| {
        let ReviewRow::Removed(line) = row else {
            return None;
        };
        line_text(line).contains("already beta").then_some(line)
    });
    let before_comment = before_comment.expect("old comment must stay inside its definition hunk");
    let after_comment = hunk.rows.iter().find_map(|row| {
        let ReviewRow::Added(line) = row else {
            return None;
        };
        line_text(line).contains("must become beta").then_some(line)
    });
    let after_comment = after_comment.expect("new comment must stay inside its definition hunk");

    assert!(
        before_comment
            .spans
            .iter()
            .any(|span| span.mark == DiffMark::Removed),
        "{hunk:#?}"
    );
    assert!(line_text(after_comment).starts_with("    //"));
    assert!(
        after_comment
            .spans
            .iter()
            .any(|span| span.mark == DiffMark::Added),
        "{hunk:#?}"
    );

    assert!(hunk_has_text(hunk, "epsilon.and_then(alpha)"), "{hunk:#?}");
    assert!(hunk_has_added_text(hunk, "and_then"), "{hunk:#?}");
}

#[test]
fn structural_context_can_carry_a_hunk_to_eof() {
    let before = "fn run() { old(); }\n\n";
    let after = "fn run() { new(); }\n\n";

    let diff = diff_file("alpha.rs", before, after).expect("source must parse");
    let hunk = &diff.hunks[0];

    assert_eq!(hunk.coverage.after, Some(1..3));
    assert!(matches!(
        hunk.rows.iter().rev().nth(1),
        Some(ReviewRow::Current(line)) if line.number == 2 && line_text(line).is_empty()
    ));
    assert!(matches!(hunk.rows.last(), Some(ReviewRow::FileBoundary)));
}

#[test]
fn two_sided_hunk_uses_the_current_file_boundary() {
    let before = "fn run() { old(); }\n";
    let after = "fn run() { new(); }\n\nfn later() {}\n";

    let diff = diff_file("alpha.rs", before, after).expect("source must parse");
    let hunk = hunk_containing(&diff, "fn run");

    assert_eq!(diff.hunks.len(), 1);
    assert_eq!(hunk.coverage.before, Some(1..2));
    assert_eq!(hunk.coverage.after, Some(1..4));
    assert!(hunk_has_text(hunk, "fn later"));
    assert!(matches!(hunk.rows.last(), Some(ReviewRow::FileBoundary)));
}

#[test]
fn move_hunk_lives_in_the_present_and_elides_its_unchanged_body() {
    let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");
    let hunk = hunk_containing(&diff, "fn beta");

    assert!(hunk.rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::Moved {
                before: Some(16),
                after,
            } if after.number == 38 && line_text(after).contains("fn beta")
        )
    }));
    assert!(
        hunk.rows.iter().any(|row| {
            matches!(
                row,
                ReviewRow::Elision(coverage)
                    if coverage.before == Some(17..21) && coverage.after == Some(39..43)
            )
        }),
        "{hunk:#?}"
    );
    assert!(hunk.rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::Moved {
                before: None,
                after,
            } if after.number == 43 && line_text(after) == "}"
        )
    }));
}

#[test]
fn moved_definition_keeps_its_single_body_line_visible() {
    let before = "fn first() {\n    first_body();\n}\n\nfn second() {\n    second_body();\n}\n";
    let after = "fn second() {\n    second_body();\n}\n\nfn first() {\n    first_body();\n}\n";
    let diff = diff_file("alpha.rs", before, after).expect("source must parse");
    let moved = diff
        .hunks
        .iter()
        .find(|hunk| matches!(hunk.rows.first(), Some(ReviewRow::Moved { .. })))
        .expect("one definition must be presented as moved");

    assert!(matches!(
        moved.rows.as_slice(),
        [
            ReviewRow::Moved { .. },
            ReviewRow::Current(_),
            ReviewRow::Moved { .. },
            ReviewRow::FileBoundary
        ]
    ));
}

#[test]
fn moved_definition_keeps_its_terminator_edit_visible() {
    let before = "fn alpha() { alpha(); }\nfn beta() { beta(); }\n";
    let after = "fn beta() { beta(); }\nfn alpha() { alpha(); }\r\n";
    let diff = diff_file("alpha.rs", before, after).expect("source must parse");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    assert!(
        rows.iter().any(|row| {
            matches!(
                row,
                ReviewRow::Moved {
                    before: Some(_),
                    after,
                } if line_text(after).contains("fn alpha") || line_text(after).contains("fn beta")
            )
        }),
        "{rows:#?}"
    );
    assert!(rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::LineEnding {
                before: Some(LineEnding::Lf),
                after: Some(LineEnding::CrLf),
            }
        )
    }));
}

#[test]
fn moved_unit_owns_a_terminator_fallback_retained_by_physical_lcs() {
    let before = concat!(
        "fn alpha() {\n",
        "    one();\n",
        "    two();\n",
        "    three();\n",
        "}\n",
        "fn beta() { beta(); }\n",
    );
    let after = concat!(
        "fn beta() { beta(); }\n",
        "fn alpha() {\r\n",
        "    one();\n",
        "    two();\n",
        "    three();\n",
        "}\n",
    );
    let diff = diff_file("alpha.rs", before, after).expect("source must parse");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    assert!(
        rows.windows(2).any(|rows| {
            matches!(
                rows,
                [
                    ReviewRow::Moved {
                        before: Some(1),
                        after,
                    },
                    ReviewRow::LineEnding {
                        before: Some(LineEnding::Lf),
                        after: Some(LineEnding::CrLf),
                    }
                ] if after.number == 2 && line_text(after).contains("fn alpha")
            )
        }),
        "{rows:#?}"
    );
    assert!(
        !rows.iter().any(|row| {
            matches!(
                row,
                ReviewRow::Removed(line) if line.number == 1
            ) || matches!(
                row,
                ReviewRow::Added(line) if line.number == 2
            )
        }),
        "the move must remain the sole producer: {rows:#?}"
    );
    assert_source_space_invariants("alpha.rs", &diff);
}

#[test]
fn moved_reflow_owns_an_unmatched_missing_terminator() {
    let before = "fn alpha(){alpha();}\nfn beta(){beta();}\n";
    let after = "fn beta(){beta();}\nfn alpha() {\n    alpha();\n}";
    let diff = diff_file("alpha.rs", before, after).expect("source must parse");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    assert!(
        rows.iter().any(|row| {
            matches!(
                row,
                ReviewRow::Moved { after, .. } if line_text(after).contains("fn alpha")
            )
        }),
        "{rows:#?}"
    );
    assert!(
        rows.windows(2).any(|rows| {
            matches!(
                rows,
                [
                    ReviewRow::Moved { after, .. },
                    ReviewRow::LineEnding {
                        before: None,
                        after: Some(LineEnding::Missing),
                    }
                ] if line_text(after) == "}"
            )
        }),
        "{rows:#?}"
    );
    assert!(
        !rows
            .iter()
            .any(|row| matches!(row, ReviewRow::Removed(_) | ReviewRow::Added(_))),
        "the move must remain the sole producer: {rows:#?}"
    );
    assert_source_space_invariants("alpha.rs", &diff);
}

#[test]
fn moved_reflow_keeps_an_unmatched_crlf_visible() {
    let before = "fn alpha(){alpha();}\nfn beta(){beta();}\n";
    let after = "fn beta(){beta();}\nfn alpha() {\r\n    alpha();\n}\n";
    let diff = diff_file("alpha.rs", before, after).expect("source must parse");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    assert!(
        rows.iter().any(|row| {
            matches!(
                row,
                ReviewRow::Moved { after, .. }
                    if after.number == 2 && line_text(after).contains("fn alpha")
            )
        }),
        "{rows:#?}"
    );
    assert!(
        rows.iter().any(|row| {
            matches!(
                row,
                ReviewRow::LineEnding {
                    before: None,
                    after: Some(LineEnding::CrLf),
                }
            )
        }),
        "{rows:#?}"
    );
    assert_source_space_invariants("alpha.rs", &diff);
}

#[test]
fn imports_and_reflow_keep_their_signals_and_local_context() {
    let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");

    let import = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .find_map(|row| match row {
            ReviewRow::Wordwise(import) => Some(import),
            _ => None,
        })
        .expect("fixture must include a wordwise import hunk");
    let formatting = hunk_containing(&diff, "fn delta");
    assert_eq!(import.prefix, "use crate::gamma::");
    assert_eq!(import.removed, "theta");
    assert_eq!(import.added, "{Theta, Iota}");
    assert_eq!(import.suffix, ";");

    assert!(
        formatting
            .rows
            .iter()
            .any(|row| matches!(row, ReviewRow::Current(line) if line.number == 25))
    );
    assert!(
        formatting
            .rows
            .iter()
            .any(|row| matches!(row, ReviewRow::Reflow(line) if line.number == 26))
    );
    assert!(
        formatting
            .rows
            .iter()
            .any(|row| matches!(row, ReviewRow::Current(line) if line.number == 27))
    );
}

#[test]
fn duplicate_definition_names_keep_one_to_one_correspondence() {
    let before = "impl Thing { fn first() { old(); } }\nimpl Thing { fn second() { stable(); } }\n";
    let after = "impl Thing { fn first() { new(); } }\nimpl Thing { fn second() { stable(); } }\n";

    let diff = diff_file("alpha.rs", before, after).expect("source must parse");

    assert_eq!(diff.hunks.len(), 1);
    assert!(hunk_has_added_text(&diff.hunks[0], "new"));
    assert!(diff.hunks[0].rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::Current(line)
                if !line.has_changes() && line_text(line).contains("second")
        )
    }));
}

#[test]
fn unknown_syntax_uses_aligned_line_leaves() {
    let before = "alpha\nold value\nomega\n";
    let after = "alpha\nnew value\nomega\n";

    let diff = diff_file("alpha.txt", before, after).expect("plain diff cannot fail");

    assert!(!diff.generated);
    assert_eq!(diff.hunks.len(), 1);
    assert_eq!(diff.hunks[0].coverage.before, Some(1..4));
    assert_eq!(diff.hunks[0].coverage.after, Some(1..4));
    let before = diff.hunks[0].rows.iter().find_map(|row| match row {
        ReviewRow::Removed(line) => Some(line),
        _ => None,
    });
    let after = diff.hunks[0].rows.iter().find_map(|row| match row {
        ReviewRow::Added(line) => Some(line),
        _ => None,
    });
    let before = before.expect("plain replacement needs its removed source row");
    let after = after.expect("plain replacement needs its added source row");
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
fn line_insertions_and_deletions_keep_their_source_numbers() {
    let before = "one\nremove\ntwo\nthree\n";
    let after = "one\ntwo\nadd\nthree\n";

    let diff = diff_file("notes", before, after).expect("plain diff cannot fail");
    let rows = &diff.hunks[0].rows;

    assert!(rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::Removed(line)
                if line.number == 2 && line_text(line) == "remove"
        )
    }));
    assert!(rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::Added(line)
                if line.number == 3 && line_text(line) == "add"
        )
    }));
}

#[test]
fn exact_nested_transfer_is_presented_once_at_its_destination() {
    let before = concat!(
        "const Card = () => (\n",
        "\t<>\n",
        "\t\t<div>\n",
        "\t\t\t<File />\n",
        "\t\t\t<Keep>\n",
        "\t\t\t\t<One />\n",
        "\t\t\t\t<Two />\n",
        "\t\t\t\t<Three />\n",
        "\t\t\t\t<Four />\n",
        "\t\t\t\t<Five />\n",
        "\t\t\t\t<Six />\n",
        "\t\t\t\t<Seven />\n",
        "\t\t\t</Keep>\n",
        "\t\t</div>\n",
        "\t\t<Portrait />\n",
        "\t\t<Composer />\n",
        "\t\t<Moved />\n",
        "\t</>\n",
        ")\n",
    );
    let after = concat!(
        "const Card = () => (\n",
        "\t<>\n",
        "\t\t<Portrait />\n",
        "\t\t<div>\n",
        "\t\t\t<Composer />\n",
        "\t\t\t<File />\n",
        "\t\t\t<Keep>\n",
        "\t\t\t\t<One />\n",
        "\t\t\t\t<Two />\n",
        "\t\t\t\t<Three />\n",
        "\t\t\t\t<Four />\n",
        "\t\t\t\t<Five />\n",
        "\t\t\t\t<Six />\n",
        "\t\t\t\t<Seven />\n",
        "\t\t\t</Keep>\n",
        "\t\t\t<Moved />\n",
        "\t\t</div>\n",
        "\t</>\n",
        ")\n",
    );

    let diff = diff_file("alpha.tsx", before, after).expect("TSX must diff");

    for (payload, before, after) in [("<Composer />", 16, 5), ("<Moved />", 17, 16)] {
        assert_eq!(
            diff.hunks
                .iter()
                .flat_map(|hunk| &hunk.rows)
                .filter(|row| {
                    matches!(
                        row,
                        ReviewRow::Moved {
                            before: Some(old),
                            after: current,
                        } if *old == before
                            && current.number == after
                            && line_text(current).trim() == payload
                    )
                })
                .count(),
            1,
            "{payload} must have one move producer: {diff:#?}",
        );
        assert_eq!(
            marked_line_occurrences(&diff, payload, DiffMark::Removed),
            0
        );
        assert_eq!(marked_line_occurrences(&diff, payload, DiffMark::Added), 0);
        assert_eq!(
            source_lines(&diff)
                .into_iter()
                .filter(|line| line_text(line).trim() == payload)
                .count(),
            1,
            "{payload} must be rendered once across all hunks: {diff:#?}",
        );
    }
    assert_source_space_invariants("alpha.tsx", &diff);
}

#[test]
fn exact_nested_transfers_follow_their_crossed_destination_order() {
    let before = concat!(
        "const Card = () => (\n",
        "\t<article>\n",
        "\t\t<div>\n",
        "\t\t\t<Anchor />\n",
        "\t\t\t<Middle />\n",
        "\t\t</div>\n",
        "\t\t<Camera />\n",
        "\t\t<Microphone />\n",
        "\t</article>\n",
        ")\n",
    );
    let after = concat!(
        "const Card = () => (\n",
        "\t<article>\n",
        "\t\t<div>\n",
        "\t\t\t<Anchor />\n",
        "\t\t\t<Microphone />\n",
        "\t\t\t<Middle />\n",
        "\t\t\t<Camera />\n",
        "\t\t</div>\n",
        "\t</article>\n",
        ")\n",
    );

    let diff = diff_file("alpha.tsx", before, after).expect("TSX must diff");
    let moves = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .filter_map(|row| {
            let ReviewRow::Moved { before, after } = row else {
                return None;
            };
            let payload = line_text(after);
            matches!(payload.trim(), "<Camera />" | "<Microphone />").then_some((
                *before,
                after.number,
                payload.trim().to_owned(),
            ))
        })
        .collect::<Vec<_>>();

    assert_eq!(
        moves,
        vec![
            (Some(8), 5, "<Microphone />".to_owned()),
            (Some(7), 7, "<Camera />".to_owned()),
        ],
        "relocations must follow current-world geometry: {diff:#?}",
    );
    for payload in ["<Camera />", "<Microphone />"] {
        assert_eq!(
            marked_line_occurrences(&diff, payload, DiffMark::Removed),
            0
        );
        assert_eq!(marked_line_occurrences(&diff, payload, DiffMark::Added), 0);
        assert_eq!(
            source_lines(&diff)
                .into_iter()
                .filter(|line| line_text(line).trim() == payload)
                .count(),
            1,
            "{payload} must be rendered once across all hunks: {diff:#?}",
        );
    }
    assert_source_space_invariants("alpha.tsx", &diff);
}

#[test]
fn exact_multiline_transfer_uses_move_provenance_and_elides_its_body() {
    let before = concat!(
        "const Card = () => (\n",
        "\t<article>\n",
        "\t\t<div class=\"self-controls\">\n",
        "\t\t\t<Screen />\n",
        "\t\t</div>\n",
        "\t\t<Show when={liveMedia()}>\n",
        "\t\t\t<div class=\"self-live-controls\">\n",
        "\t\t\t\t<ToggleButton\n",
        "\t\t\t\t\taccessibleName=\"camera\"\n",
        "\n",
        "\t\t\t\t\tlabel=\"cam\"\n",
        "\t\t\t\t/>\n",
        "\t\t\t</div>\n",
        "\t\t</Show>\n",
        "\t</article>\n",
        ")\n",
    );
    let after = concat!(
        "const Card = () => (\n",
        "\t<article>\n",
        "\t\t<div class=\"self-controls\">\n",
        "\t\t\t<Screen />\n",
        "\t\t\t<Show when={liveMedia()}>\n",
        "\t\t\t\t<div class=\"self-live-controls\">\n",
        "\t\t\t\t\t<ToggleButton\n",
        "\t\t\t\t\t\taccessibleName=\"camera\"\n",
        "\n",
        "\t\t\t\t\t\tlabel=\"cam\"\n",
        "\t\t\t\t\t/>\n",
        "\t\t\t\t</div>\n",
        "\t\t\t</Show>\n",
        "\t\t</div>\n",
        "\t</article>\n",
        ")\n",
    );

    let diff = diff_file("alpha.tsx", before, after).expect("TSX must diff");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    assert_eq!(
        rows.iter()
            .filter(|row| {
                matches!(
                    row,
                    ReviewRow::Moved {
                        before: Some(6),
                        after,
                    } if after.number == 5 && line_text(after).contains("<Show when=")
                )
            })
            .count(),
        1,
    );
    assert!(rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::Moved {
                before: None,
                after,
            } if after.number == 13 && line_text(after).contains("</Show>")
        )
    }));
    assert!(rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::Elision(coverage)
                if coverage.before == Some(7..14) && coverage.after == Some(6..13)
        )
    }));
    for line in ["<Show when={liveMedia()}>", "</Show>"] {
        assert_eq!(marked_line_occurrences(&diff, line, DiffMark::Removed), 0);
        assert_eq!(marked_line_occurrences(&diff, line, DiffMark::Added), 0);
        assert_eq!(
            source_lines(&diff)
                .into_iter()
                .filter(|source| line_text(source).trim() == line)
                .count(),
            1,
            "{line} must be rendered once across all hunks: {diff:#?}",
        );
    }
    assert_source_space_invariants("alpha.tsx", &diff);
}

#[test]
fn duplicate_cross_owner_payload_stays_an_explicit_remove_and_add() {
    let before = concat!(
        "const Card = () => (\n",
        "\t<article>\n",
        "\t\t<Alpha>\n",
        "\t\t\t<AlphaAnchor />\n",
        "\t\t\t<Show when={live}><Controls /></Show>\n",
        "\t\t</Alpha>\n",
        "\t\t<Beta>\n",
        "\t\t\t<BetaAnchor />\n",
        "\t\t\t<Show when={live}><Controls /></Show>\n",
        "\t\t</Beta>\n",
        "\t\t<Target>\n",
        "\t\t\t<TargetAnchor />\n",
        "\t\t</Target>\n",
        "\t</article>\n",
        ")\n",
    );
    let after = concat!(
        "const Card = () => (\n",
        "\t<article>\n",
        "\t\t<Alpha>\n",
        "\t\t\t<AlphaAnchor />\n",
        "\t\t</Alpha>\n",
        "\t\t<Beta>\n",
        "\t\t\t<BetaAnchor />\n",
        "\t\t</Beta>\n",
        "\t\t<Target>\n",
        "\t\t\t<TargetAnchor />\n",
        "\t\t\t<Show when={live}><Controls /></Show>\n",
        "\t\t</Target>\n",
        "\t</article>\n",
        ")\n",
    );

    let diff = diff_file("alpha.tsx", before, after).expect("TSX must diff");

    assert_eq!(
        marked_line_occurrences(&diff, "<Show when={live}", DiffMark::Removed),
        2,
    );
    assert_eq!(
        marked_line_occurrences(&diff, "<Show when={live}", DiffMark::Added),
        1,
    );
    assert!(!diff.hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
        matches!(row, ReviewRow::Moved { after, .. } if line_text(after).contains("<Show"))
    }));
    assert_source_space_invariants("alpha.tsx", &diff);
}

#[test]
fn exact_payload_does_not_relocate_between_paired_sibling_owners() {
    let before = concat!(
        "const Card = () => (\n",
        "\t<article>\n",
        "\t\t<Alpha>\n",
        "\t\t\t<AlphaAnchor />\n",
        "\t\t\t<Show when={live}>\n",
        "\t\t\t\t<Camera />\n",
        "\t\t\t</Show>\n",
        "\t\t</Alpha>\n",
        "\t\t<Beta>\n",
        "\t\t\t<BetaAnchor />\n",
        "\t\t\t<Show when={live}>\n",
        "\t\t\t\t<Microphone />\n",
        "\t\t\t</Show>\n",
        "\t\t</Beta>\n",
        "\t</article>\n",
        ")\n",
    );
    let after = concat!(
        "const Card = () => (\n",
        "\t<article>\n",
        "\t\t<Alpha>\n",
        "\t\t\t<AlphaAnchor />\n",
        "\t\t\t<Show when={live}>\n",
        "\t\t\t\t<Microphone />\n",
        "\t\t\t</Show>\n",
        "\t\t</Alpha>\n",
        "\t\t<Beta>\n",
        "\t\t\t<BetaAnchor />\n",
        "\t\t\t<Show when={live}>\n",
        "\t\t\t\t<Camera />\n",
        "\t\t\t</Show>\n",
        "\t\t</Beta>\n",
        "\t</article>\n",
        ")\n",
    );

    let diff = diff_file("alpha.tsx", before, after).expect("TSX must diff");

    for payload in ["<Camera />", "<Microphone />"] {
        assert_eq!(
            marked_line_occurrences(&diff, payload, DiffMark::Removed),
            1
        );
        assert_eq!(marked_line_occurrences(&diff, payload, DiffMark::Added), 1);
    }
    assert!(!diff.hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
        matches!(row, ReviewRow::Moved { after, .. } if line_text(after).contains("<Show"))
    }));
    assert_source_space_invariants("alpha.tsx", &diff);
}

#[test]
fn exact_payload_cannot_cross_a_nested_definition_owner() {
    let cases = [
        (
            "alpha.ts",
            concat!(
                "function outer() {\n",
                "  inner();\n",
                "  function nested() {\n",
                "    stay();\n",
                "  }\n",
                "}\n",
            ),
            concat!(
                "function outer() {\n",
                "  function nested() {\n",
                "    stay();\n",
                "    inner();\n",
                "  }\n",
                "}\n",
            ),
        ),
        (
            "alpha.rs",
            concat!(
                "fn outer() {\n",
                "    inner();\n",
                "    fn nested() {\n",
                "        stay();\n",
                "    }\n",
                "}\n",
            ),
            concat!(
                "fn outer() {\n",
                "    fn nested() {\n",
                "        stay();\n",
                "        inner();\n",
                "    }\n",
                "}\n",
            ),
        ),
    ];

    for (path, before, after) in cases {
        let diff = diff_file(path, before, after).expect("source must diff");

        assert_eq!(
            marked_line_occurrences(&diff, "inner();", DiffMark::Removed),
            1,
            "{path} must retain the old semantic owner: {diff:#?}",
        );
        assert_eq!(
            marked_line_occurrences(&diff, "inner();", DiffMark::Added),
            1,
            "{path} must retain the new semantic owner: {diff:#?}",
        );
        assert!(!diff.hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
            matches!(row, ReviewRow::Moved { after, .. } if line_text(after).trim() == "inner();")
        }));
        assert_source_space_invariants(path, &diff);
    }
}

#[test]
fn recovered_html_wrapper_preserves_its_payload() {
    let before = "<p>\n \t<img />\n</p>\n";
    let after = concat!(
        "<p>\n",
        "\t<div\n",
        "\t\tid=\"alpha\"\n",
        "\t\tdata-alpha=\"beta\"\t\n",
        "\t>\n",
        " \t\t<img />\n",
        "\t</div>\n",
        "</p>\n",
    );

    let diff = diff_file("alpha.html", before, after).expect("HTML must diff");
    let images = source_lines(&diff)
        .into_iter()
        .filter(|line| line_text(line).trim() == "<img />")
        .collect::<Vec<_>>();

    assert_eq!(
        images.len(),
        1,
        "the retained image must have one display owner"
    );
    assert!(
        images[0]
            .spans
            .iter()
            .filter(|span| !span.text.trim().is_empty())
            .all(|span| span.mark == DiffMark::Context),
        "the retained tag spelling must remain context: {diff:#?}",
    );
    assert!(
        source_lines(&diff)
            .into_iter()
            .flat_map(|line| &line.spans)
            .all(|span| span.mark != DiffMark::Removed),
        "wrapping retained source must be green-only: {diff:#?}",
    );
    for shell in ["<div", "id=\"alpha\"", "data-alpha=\"beta\"", "</div>"] {
        assert_eq!(marked_line_occurrences(&diff, shell, DiffMark::Added), 1);
    }
    assert_eq!(marked_line_occurrences(&diff, "</p>", DiffMark::Added), 0);
    assert_eq!(marked_line_occurrences(&diff, "</p>", DiffMark::Removed), 0);
    assert_source_space_invariants("alpha.html", &diff);
}

#[test]
fn recovered_paragraph_keeps_its_frame_around_a_displaced_sibling() {
    let open = "<main>\n  <p>alpha</p>\n  <div>beta</div>\n</main>\n";
    let closed = "<main>\n  <p>alpha\n  <div>beta</div>\n  </p>\n</main>\n";

    for (before, after) in [(open, closed), (closed, open)] {
        let diff = diff_file("alpha.html", before, after).expect("HTML must diff");
        let blocks = source_lines(&diff)
            .into_iter()
            .filter(|line| line_text(line).trim() == "<div>beta</div>")
            .collect::<Vec<_>>();

        assert_eq!(
            blocks.len(),
            2,
            "both owner-local occurrences must remain visible: {diff:#?}"
        );
        assert_eq!(
            marked_line_occurrences(&diff, "<div>beta</div>", DiffMark::Removed),
            1,
        );
        assert_eq!(
            marked_line_occurrences(&diff, "<div>beta</div>", DiffMark::Added),
            1,
        );
        assert_eq!(marked_line_occurrences(&diff, "alpha", DiffMark::Added), 0);
        assert_eq!(
            marked_line_occurrences(&diff, "alpha", DiffMark::Removed),
            0
        );
        assert_eq!(marked_line_occurrences(&diff, "</p>", DiffMark::Added), 0);
        assert_eq!(marked_line_occurrences(&diff, "</p>", DiffMark::Removed), 0);
        assert_source_space_invariants("alpha.html", &diff);
    }
}

#[test]
fn inline_html_removal_keeps_the_complete_removed_node() {
    let before = "<p>alpha <b>beta</b></p>\n";
    let after = "<p>alpha</p>\n";

    let diff = diff_file("alpha.html", before, after).expect("HTML must diff");
    let before = source_lines(&diff)
        .into_iter()
        .find(|line| line_text(line).contains("<b>beta</b>"))
        .expect("the removed inline node must remain visible");
    let removed = before
        .spans
        .iter()
        .filter(|span| span.mark == DiffMark::Removed)
        .map(|span| span.text.as_str())
        .collect::<String>();

    assert_eq!(removed, " <b>beta</b>");
    assert_eq!(marked_line_occurrences(&diff, "alpha", DiffMark::Added), 0);
    assert_source_space_invariants("alpha.html", &diff);
}

#[test]
fn opaque_sibling_replacements_keep_complete_boundaries() {
    let before = concat!(
        "<style>\n",
        "alpha {\n",
        "  beta: gamma;\n",
        "  delta: epsilon;\n",
        "}\n",
        "zeta {\n",
        "  eta: theta;\n",
        "}\n",
        "rho {\n",
        "  sigma: tau;\n",
        "}\n",
        "</style>\n",
    );
    let after = concat!(
        "<style>\n",
        "alpha {\n",
        "  beta: iota;\n",
        "  delta: kappa;\n",
        "}\n",
        "rho {\n",
        "  sigma: tau;\n",
        "}\n",
        "lambda {\n",
        "  mu: nu;\n",
        "  xi: omicron;\n",
        "}\n",
        "</style>\n",
    );
    let diff = diff_file("alpha.html", before, after).expect("HTML must diff");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    assert_eq!(
        rows.iter()
            .filter(|row| matches!(row, ReviewRow::Removed(line) if line.number == 8))
            .count(),
        1,
        "the removed sibling must retain its closing boundary: {diff:#?}",
    );
    for number in 6..=8 {
        assert!(rows.iter().any(|row| {
            matches!(row, ReviewRow::Current(line) if line.number == number && !line.has_changes())
        }), "the shifted unchanged sibling must remain context: {diff:#?}");
    }
    assert!(
        !rows.iter().any(|row| {
            matches!(row, ReviewRow::Removed(line) if (9..=11).contains(&line.number))
        }),
        "the shifted unchanged sibling must not acquire removal ghosts: {diff:#?}",
    );
    for line_number in 9..=12 {
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, ReviewRow::Added(line) if line.number == line_number))
                .count(),
            1,
            "each added sibling row must be emitted exactly once: {diff:#?}",
        );
    }

    assert_source_space_invariants("alpha.html", &diff);
}

#[test]
fn removed_shorthand_field_stays_inside_its_initializer_frame() {
    let before = concat!(
        "fn alpha() -> Beta {\n",
        "    let gamma = delta();\n",
        "    Beta {\n",
        "        alpha: epsilon(),\n",
        "        gamma,\n",
        "        zeta: None,\n",
        "    }\n",
        "}\n",
    );
    let after = concat!(
        "fn alpha() -> Beta {\n",
        "    Beta {\n",
        "        alpha: epsilon(),\n",
        "        beta: eta(),\n",
        "        zeta: None,\n",
        "    }\n",
        "}\n",
    );
    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    #[derive(Debug, Eq, PartialEq)]
    enum Side {
        Current(usize),
        Removed(usize),
        Added(usize),
    }

    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .flat_map(|row| match row {
            ReviewRow::Current(line) | ReviewRow::Reflow(line) => vec![Side::Current(line.number)],
            ReviewRow::Removed(line) => vec![Side::Removed(line.number)],
            ReviewRow::Added(line) => vec![Side::Added(line.number)],
            ReviewRow::Moved { after, .. } => vec![Side::Current(after.number)],
            ReviewRow::LineEnding { .. }
            | ReviewRow::Wordwise(_)
            | ReviewRow::Elision(_)
            | ReviewRow::FileBoundary => Vec::new(),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            Side::Current(1),
            Side::Removed(2),
            Side::Current(2),
            Side::Current(3),
            Side::Removed(5),
            Side::Added(4),
            Side::Current(5),
            Side::Current(6),
            Side::Current(7),
        ],
    );

    for (number, text, mark) in [
        (5, "        gamma,", DiffMark::Removed),
        (4, "        beta: eta(),", DiffMark::Added),
    ] {
        let line = source_lines(&diff)
            .into_iter()
            .find(|line| line.number == number && line_text(line) == text)
            .unwrap_or_else(|| panic!("missing {mark:?} line {number}: {diff:#?}"));
        assert!(
            line.spans
                .iter()
                .filter(|span| !span.text.trim().is_empty())
                .all(|span| span.mark == mark),
            "the complete field envelope must be {mark:?}: {line:#?}",
        );
    }
    assert_source_space_invariants("alpha.rs", &diff);
}

#[test]
fn changed_owner_is_not_repeated_as_its_own_breadcrumb() {
    let before = concat!(
        "impl<B: Beta> Gamma for Delta<B> {\n",
        "    type Epsilon = Zeta;\n",
        "    type Eta = Theta;\n",
        "\n",
        "    fn alpha(&self) -> Iota {\n",
        "        let kappa = self.lambda();\n",
        "        kappa\n",
        "    }\n",
        "\n",
        "    fn beta(&self) -> Iota {\n",
        "        let kappa = self.lambda();\n",
        "        kappa\n",
        "    }\n",
        "\n",
        "    fn gamma(&self) -> Iota {\n",
        "        let kappa = self.lambda();\n",
        "        kappa\n",
        "    }\n",
        "}\n",
    );
    let after = before
        .replacen("impl<B: Beta>", "impl<B: Beta + 'static>", 1)
        .replacen(
            "fn gamma(&self) -> Iota {\n        let kappa = self.lambda();",
            "fn gamma(&self) -> Iota {\n        let kappa = self.mu();",
            1,
        );
    let diff = diff_file("alpha.rs", before, &after).expect("Rust must diff");
    let mut visible = HashSet::new();

    for row in diff.hunks.iter().flat_map(|hunk| &hunk.rows) {
        if matches!(row, ReviewRow::Elision(_)) {
            visible.clear();
            continue;
        }
        let Some(current) = current_world_coverage(row) else {
            continue;
        };
        for line in current {
            assert!(
                visible.insert(line),
                "current line {line} is repeated without an intervening elision: {diff:#?}",
            );
        }
    }
}

#[test]
fn inserted_test_does_not_drift_across_repeated_neighbor_bodies() {
    let before = concat!(
        "mod alpha {\n",
        "#[test]\n",
        "fn alpha() {\n",
        "    let alpha = beta();\n",
        "    consume(alpha);\n",
        "}\n",
        "#[test]\n",
        "fn beta() {\n",
        "    let alpha = beta();\n",
        "    consume(alpha);\n",
        "}\n",
        "#[test]\n",
        "fn gamma() {\n",
        "    let alpha = beta();\n",
        "    consume(alpha);\n",
        "}\n",
        "}\n",
    );
    let after = before.replace(
        "#[test]\nfn beta()",
        concat!(
            "#[test]\n",
            "fn delta() {\n",
            "    let alpha = beta();\n",
            "    consume(alpha);\n",
            "}\n",
            "#[test]\n",
            "fn beta()",
        ),
    );

    let diff = diff_file("alpha.rs", before, &after).expect("Rust must diff");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    for needle in ["fn beta", "fn gamma"] {
        assert!(
            !rows.iter().any(|row| {
                matches!(
                    row,
                    ReviewRow::Removed(line) | ReviewRow::Added(line)
                        if line_text(line).contains(needle)
                )
            }),
            "stable neighbor {needle:?} must stay context or omitted: {rows:#?}"
        );
    }
}

#[test]
fn inserted_top_level_test_keeps_attributes_with_their_definitions() {
    let before = concat!(
        "#[test]\n",
        "fn alpha() { alpha(); }\n",
        "#[test]\n",
        "fn beta() { beta(); }\n",
    );
    let after = before.replace(
        "#[test]\nfn beta()",
        concat!(
            "#[test]\n",
            "fn gamma() { gamma(); }\n",
            "#[test]\n",
            "fn beta()",
        ),
    );
    let diff = diff_file("alpha.rs", before, &after).expect("Rust must diff");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    assert!(!rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::Removed(line) | ReviewRow::Added(line)
                if line_text(line).contains("fn beta")
        )
    }));
    let removed_attributes = rows
        .iter()
        .filter(|row| {
            matches!(
                row,
                ReviewRow::Removed(line)
                    if line_text(line).trim() == "#[test]"
            )
        })
        .count();
    let added_attributes = rows
        .iter()
        .filter(|row| {
            matches!(
                row,
                ReviewRow::Added(line)
                    if line_text(line).trim() == "#[test]"
            )
        })
        .count();

    assert_eq!(removed_attributes, 0, "{rows:#?}");
    assert_eq!(added_attributes, 1, "{rows:#?}");
}

#[test]
fn reordered_decorated_definitions_do_not_churn_repeated_prefixes() {
    let before = concat!(
        "#[derive(Clone)]\n",
        "// explanatory comment\n",
        "/// Alpha documentation.\n",
        "struct Alpha { value: u8 }\n",
        "\n",
        "#[derive(Clone)]\n",
        "/// Beta documentation.\n",
        "struct Beta { value: u16 }\n",
    );
    let after = concat!(
        "#[derive(Clone)]\n",
        "/// Beta documentation.\n",
        "struct Beta { value: u32 }\n",
        "\n",
        "#[derive(Clone)]\n",
        "// explanatory comment\n",
        "/// Alpha documentation.\n",
        "struct Alpha { value: u8 }\n",
    );
    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    for decoration in [
        "#[derive(Clone)]",
        "/// Alpha documentation.",
        "/// Beta documentation.",
    ] {
        assert_eq!(
            marked_line_occurrences(&diff, decoration, DiffMark::Removed),
            0,
            "exact decoration must follow its semantic owner: {diff:#?}",
        );
        assert_eq!(
            marked_line_occurrences(&diff, decoration, DiffMark::Added),
            0,
            "exact decoration must follow its semantic owner: {diff:#?}",
        );
    }
    assert_eq!(marked_line_occurrences(&diff, "u16", DiffMark::Removed), 1);
    assert_eq!(marked_line_occurrences(&diff, "u32", DiffMark::Added), 1);
}

#[test]
fn nested_test_decorations_follow_their_reordered_functions() {
    let before = concat!(
        "mod tests {\n",
        "    #[test]\n",
        "    /// Alpha contract.\n",
        "    fn alpha() { old(); }\n",
        "    #[test]\n",
        "    /// Beta contract.\n",
        "    fn beta() { stable(); }\n",
        "}\n",
    );
    let after = concat!(
        "mod tests {\n",
        "    #[test]\n",
        "    /// Beta contract.\n",
        "    fn beta() { stable(); }\n",
        "    #[test]\n",
        "    /// Alpha contract.\n",
        "    fn alpha() { new(); }\n",
        "}\n",
    );
    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    for decoration in ["#[test]", "/// Alpha contract.", "/// Beta contract."] {
        assert_eq!(
            marked_line_occurrences(&diff, decoration, DiffMark::Removed),
            0,
            "nested decoration must follow its semantic owner: {diff:#?}",
        );
        assert_eq!(
            marked_line_occurrences(&diff, decoration, DiffMark::Added),
            0,
            "nested decoration must follow its semantic owner: {diff:#?}",
        );
    }
    assert_eq!(marked_line_occurrences(&diff, "old", DiffMark::Removed), 1);
    assert_eq!(marked_line_occurrences(&diff, "new", DiffMark::Added), 1);
}

#[test]
fn bare_html_wrapper_does_not_steal_an_existing_div_anchor() {
    let before =
        "<section>\n  <img src=\"alpha.webp\">\n  <div>\n    <p>alpha</p>\n  </div>\n</section>\n";
    let after = "<section>\n  <div>\n    <img src=\"alpha.webp\">\n  </div>\n  <div>\n    <p>alpha</p>\n  </div>\n</section>\n";

    let diff = diff_file("alpha.html", before, after).expect("HTML syntax must diff");
    let rows = &diff.hunks[0].rows;
    let retained = rows
        .iter()
        .filter(|row| {
            matches!(
                row,
                ReviewRow::Reflow(line) if line_text(line).contains("<img")
            )
        })
        .count();

    assert_eq!(retained, 1, "{diff:#?}");
    assert!(!rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::Removed(line) | ReviewRow::Added(line)
                if line_text(line).contains("<img")
        )
    }));
}

#[test]
fn inline_html_wrapper_keeps_its_surroundings_and_payload_as_context() {
    let before = "<article><img src=\"alpha.webp\"></article>\n";
    let after = "<article><div><img src=\"alpha.webp\"></div></article>\n";

    let diff = diff_file("alpha.html", before, after).expect("HTML syntax must diff");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    let line = rows
        .iter()
        .find_map(|row| match row {
            ReviewRow::Current(line) if line_text(line) == after.trim_end() => Some(line),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing mixed-mark current row: {diff:#?}"));
    let context = line
        .spans
        .iter()
        .filter(|span| span.mark == DiffMark::Context)
        .map(|span| span.text.as_str())
        .collect::<String>();
    let added = line
        .spans
        .iter()
        .filter(|span| span.mark == DiffMark::Added)
        .map(|span| span.text.as_str())
        .collect::<String>();
    assert_eq!(context, before.trim_end());
    assert_eq!(added, "<div></div>");
    assert!(!rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::Reflow(_) | ReviewRow::Removed(_) | ReviewRow::Added(_)
        )
    }));
}

#[test]
fn multiline_html_wrapper_keeps_mixed_indentation_atomic() {
    let before = "<img\ndata-alpha=\"beta\"\n  src=\"alpha.webp\"\n/>\n";
    let after = "<div>\n  <img\ndata-alpha=\"beta\"\n    src=\"alpha.webp\"\n  />\n</div>\n";

    let diff = diff_file("alpha.html", before, after).expect("HTML syntax must diff");
    let rows = &diff.hunks[0].rows;

    for needle in ["<img", "data-alpha=\"beta\"", "src=\"alpha.webp\"", "/>"] {
        let retained = rows
            .iter()
            .filter_map(|row| {
                let line = match row {
                    ReviewRow::Current(line) | ReviewRow::Reflow(line) => line,
                    _ => return None,
                };
                line_text(line).contains(needle).then_some(())
            })
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 1, "{needle:?} must stay in one tag block");
    }
    assert!(rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::Current(line)
                if !line.has_changes() && line_text(line).contains("data-alpha=\"beta\"")
        )
    }));
    assert!(!rows.iter().any(|row| {
        let line = match row {
            ReviewRow::Removed(line) | ReviewRow::Added(line) => line,
            _ => return false,
        };
        ["<img", "data-alpha=\"beta\"", "src=\"alpha.webp\"", "/>"]
            .iter()
            .any(|needle| line_text(line).contains(needle))
    }));
}

#[test]
fn html_literal_regions_do_not_offer_structural_correspondence() {
    let cases = [
        (
            "raw text element",
            "<textarea>\n  <img>\n</textarea>\n",
            "<textarea>\n  <div>\n    <img>\n  </div>\n</textarea>\n",
        ),
        (
            "quoted closing text",
            "<pre>\n  <span title=\"</pre>\">\n  <img>\n</pre>\n",
            "<pre>\n  <span title=\"</pre>\">\n  <div>\n    <img>\n  </div>\n</pre>\n",
        ),
        (
            "raw child inside preformatted content",
            "<pre>\n<textarea>\n</pre>\n  <img>\n</textarea>\n</pre>\n",
            "<pre>\n<textarea>\n</pre>\n  <div>\n    <img>\n  </div>\n</textarea>\n</pre>\n",
        ),
        (
            "noscript content",
            "<noscript>\n  <img>\n</noscript>\n",
            "<noscript>\n  <div>\n    <img>\n  </div>\n</noscript>\n",
        ),
        (
            "plaintext remainder",
            "<plaintext>\n</plaintext>\n  <img>\n",
            "<plaintext>\n</plaintext>\n  <div>\n    <img>\n  </div>\n",
        ),
    ];

    for (case, before, after) in cases {
        let diff = diff_file("alpha.html", before, after).expect("HTML syntax must diff");
        assert_html_line_is_literal(&diff.hunks[0].rows, "<img>", case);
    }
}

#[test]
fn html_multiline_attribute_values_remain_literal_changes() {
    let before = "<img\n  title=\"first line\n    second line\"\n/>\n";
    let after = "<div>\n  <img\n    title=\"first line\n      second line\"\n  />\n</div>\n";

    let diff = diff_file("alpha.html", before, after).expect("HTML syntax must diff");
    let rows = &diff.hunks[0].rows;

    assert!(rows.iter().any(|row| {
        matches!(row, ReviewRow::Removed(line) if line_text(line).contains("second line"))
    }));
    assert!(rows.iter().any(|row| {
        matches!(row, ReviewRow::Added(line) if line_text(line).contains("second line"))
    }));
    assert!(
        !rows
            .iter()
            .any(|row| { matches!(row, ReviewRow::Reflow(_)) })
    );
}

#[test]
fn generated_html_keeps_exact_correspondence() {
    let before = "<!-- @generated -->\n  <img src=\"alpha.webp\" />\n";
    let after = "<!-- @generated -->\n    <img src=\"alpha.webp\" />\n";

    let diff = diff_file("alpha.html", before, after).expect("generated HTML uses line diff");
    let rows = &diff.hunks[0].rows;

    assert!(diff.generated);
    assert!(rows.iter().any(|row| {
        matches!(row, ReviewRow::Removed(line) if line_text(line).contains("<img"))
    }));
    assert!(
        rows.iter().any(|row| {
            matches!(row, ReviewRow::Added(line) if line_text(line).contains("<img"))
        })
    );
    assert!(
        !rows
            .iter()
            .any(|row| { matches!(row, ReviewRow::Reflow(_)) })
    );
}

#[test]
fn one_sided_plain_hunks_end_at_eof() {
    for (before, after) in [("only line\n", ""), ("", "only line\n")] {
        let diff = diff_file("alpha.txt", before, after).expect("plain diff cannot fail");

        assert_eq!(diff.hunks.len(), 1);
        assert!(matches!(
            diff.hunks[0].rows.last(),
            Some(ReviewRow::FileBoundary)
        ));
    }
}

#[test]
fn distant_plain_changes_become_focused_hunks() {
    let before = "one\nold two\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\nold twelve\nthirteen\nfourteen\n";
    let after = "one\nnew two\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\nnew twelve\nthirteen\nfourteen\n";

    let diff = diff_file("alpha.md", before, after).expect("plain diff cannot fail");

    assert_eq!(diff.hunks.len(), 2);
    assert_eq!(diff.hunks[0].coverage.before, Some(1..6));
    assert_eq!(diff.hunks[0].coverage.after, Some(1..6));
    assert_eq!(diff.hunks[1].coverage.before, Some(9..15));
    assert_eq!(diff.hunks[1].coverage.after, Some(9..15));
    assert!(!matches!(
        diff.hunks[0].rows.last(),
        Some(ReviewRow::FileBoundary)
    ));
    assert!(matches!(
        diff.hunks[1].rows.last(),
        Some(ReviewRow::FileBoundary)
    ));
}

#[test]
fn plain_hunks_do_not_hide_one_context_line() {
    let before = "old first\none\ntwo\nthree\nfour\nfive\nsix\nseven\nold last\n";
    let after = "new first\none\ntwo\nthree\nfour\nfive\nsix\nseven\nnew last\n";

    let diff = diff_file("alpha.txt", before, after).expect("plain diff cannot fail");

    assert_eq!(diff.hunks.len(), 1);
    assert!(diff.hunks[0].rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::Current(line)
                if !line.has_changes() && line.number == 5 && line_text(line) == "four"
        )
    }));
}

#[test]
fn line_fallback_retains_end_of_file_newline_changes() {
    let diff = diff_file("alpha.txt", "same\n", "same").expect("plain diff cannot fail");

    assert!(matches!(
        diff.hunks[0].rows.as_slice(),
        [
            ReviewRow::Removed(before),
            ReviewRow::Added(after),
            ReviewRow::LineEnding {
                before: Some(LineEnding::Lf),
                after: Some(LineEnding::Missing),
            },
            ReviewRow::FileBoundary,
        ] if before.number == 1
            && after.number == 1
            && before.spans.iter().all(|span| span.mark == DiffMark::Removed)
            && after.spans.iter().all(|span| span.mark == DiffMark::Added)
    ));
}

#[test]
fn rust_terminator_edits_stay_local_and_explicit() {
    for (before, after, expected_after) in [
        ("fn same() {}\n", "fn same() {}\r\n", LineEnding::CrLf),
        ("fn same() {}\n", "fn same() {}", LineEnding::Missing),
    ] {
        let diff = diff_file("alpha.rs", before, after).expect("Rust input must diff");

        assert!(diff.hunks[0].rows.iter().any(|row| {
            matches!(
                row,
                ReviewRow::LineEnding {
                    before: Some(LineEnding::Lf),
                    after: Some(after),
                } if *after == expected_after
            )
        }));
        assert!(matches!(
            diff.hunks[0].rows.last(),
            Some(ReviewRow::FileBoundary)
        ));
    }
}

#[test]
fn inserted_blank_layout_is_signal_without_flattening_its_definition() {
    let before = "fn run() { old(); }\n";
    let after = "\nfn run() { new(); }\n";
    let diff = diff_file("alpha.rs", before, after).expect("Rust input must diff");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    assert!(rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::Added(line)
                if line.number == 1 && line_text(line).is_empty()
        )
    }));
    let definition = source_lines(&diff)
        .into_iter()
        .find(|line| line.number == 2 && line_text(line).contains("new"))
        .expect("edited definition remains visible in current source");
    assert!(
        definition
            .spans
            .iter()
            .any(|span| span.syntax != SyntaxClass::Plain)
    );
    assert!(
        definition
            .spans
            .iter()
            .any(|span| span.mark == DiffMark::Added && span.text.contains("new"))
    );
}

#[test]
fn neighboring_crlf_edit_does_not_flatten_a_structural_change() {
    let before = concat!(
        "fn alpha() { old(); }\n",
        "fn stable() {\n",
        "    same();\n",
        "}\n",
    );
    let after = concat!(
        "fn alpha() { new(); }\n",
        "fn stable() {\r\n",
        "    same();\n",
        "}\n",
    );
    let diff = diff_file("alpha.rs", before, after).expect("Rust input must diff");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    assert!(rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::LineEnding {
                before: Some(LineEnding::Lf),
                after: Some(LineEnding::CrLf),
            }
        )
    }));
    let definition = source_lines(&diff)
        .into_iter()
        .find(|line| line.number == 1 && line_text(line).contains("new"))
        .expect("unrelated definition retains structural rendering");
    assert!(
        definition
            .spans
            .iter()
            .any(|span| span.syntax != SyntaxClass::Plain)
    );
    assert!(
        definition
            .spans
            .iter()
            .any(|span| span.mark == DiffMark::Added && span.text.contains("new"))
    );
    assert!(!rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::Removed(line) if line.number >= 3
        )
    }));
}

#[test]
fn terminator_fact_survives_an_expanded_local_replacement() {
    let before = concat!("fn run() {\n", "    old();\n", "    stable();\n", "}\n",);
    let after = concat!("fn run() {\r\n", "    new();\n", "    stable();\n", "}\n",);
    let diff = diff_file("alpha.rs", before, after).expect("Rust input must diff");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    assert!(rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::LineEnding {
                before: Some(LineEnding::Lf),
                after: Some(LineEnding::CrLf),
            }
        )
    }));
    assert!(rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::Removed(before)
                if before.number == 2 && line_text(before).contains("old")
        )
    }));
    assert!(rows.iter().any(|row| {
        matches!(
            row,
            ReviewRow::Added(after)
                if after.number == 2 && line_text(after).contains("new")
        )
    }));
}

#[test]
fn generated_rust_is_flagged_and_forced_through_line_syntax() {
    let before = "// @generated by build.rs\nuse crate::old;\n";
    let after = "use crate::new;\n";

    for (before, after, removed, added) in [
        (before, after, "use crate::old;", "use crate::new;"),
        (after, before, "use crate::new;", "use crate::old;"),
    ] {
        let diff = diff_file("alpha.rs", before, after).expect("plain diff cannot fail");
        let rows = diff.hunks.iter().flat_map(|hunk| &hunk.rows);

        assert!(diff.generated);
        assert!(
            rows.clone().any(|row| {
                matches!(row, ReviewRow::Removed(line) if line_text(line) == removed)
            })
        );
        assert!(
            rows.clone()
                .any(|row| { matches!(row, ReviewRow::Added(line) if line_text(line) == added) })
        );
        assert!(
            rows.clone()
                .all(|row| !matches!(row, ReviewRow::Wordwise(_)))
        );
        assert!(
            source_lines(&diff)
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.syntax == SyntaxClass::Plain)
        );
    }
}

#[test]
fn generated_marker_is_exact_and_header_bounded() {
    let mut below_header = "ordinary\n".repeat(20);
    below_header.push_str("// @generated\n");

    for (source, generated) in [
        ("// @generated\ncontent\n", true),
        (
            "# This file is automatically @generated by Generator.\n",
            true,
        ),
        ("// package @generated/client; file @generated\n", true),
        ("// @Generated\ncontent\n", false),
        ("// contact foo@generated.example\n", false),
        ("import client from \"@generated/client\";\n", false),
        ("const PACKAGE: &str = \"@generated\";\n", false),
        (below_header.as_str(), false),
    ] {
        let diff = diff_file("alpha.rs", "", source).expect("source must diff");

        assert_eq!(diff.generated, generated, "source: {source:?}");
    }
}

#[test]
fn malformed_rust_falls_back_without_hiding_source_changes() {
    for (before, after) in [
        ("fn alpha(value: u32 {}\n", "fn alpha(value: u64 {}\n"),
        ("fn alpha(value: u32) {}\n", "fn alpha(value: u64 {}\n"),
        ("fn alpha(value: u32 {}\n", "fn alpha(value: u64) {}\n"),
    ] {
        let diff = diff_file("alpha.rs", before, after).expect("line fallback cannot fail");
        let rows = diff.hunks.iter().flat_map(|hunk| &hunk.rows);

        assert!(rows.clone().any(|row| {
            matches!(row, ReviewRow::Removed(line) if line_text(line) == before.trim_end())
        }));
        assert!(rows.clone().any(|row| {
            matches!(row, ReviewRow::Added(line) if line_text(line) == after.trim_end())
        }));
        assert!(
            source_lines(&diff)
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.syntax == SyntaxClass::Plain),
            "both revisions must use line syntax: {diff:#?}",
        );
    }
}

fn line_text(line: &SourceRow) -> String {
    line.spans.iter().map(|span| span.text.as_str()).collect()
}

/// Numbered source rows materially rendered by one public row.
fn materialized_source_rows(row: &ReviewRow) -> (Option<usize>, Option<usize>) {
    match row {
        ReviewRow::Current(line) | ReviewRow::Reflow(line) => (None, Some(line.number)),
        ReviewRow::Removed(line) => (Some(line.number), None),
        ReviewRow::Added(line) => (None, Some(line.number)),
        ReviewRow::Moved { before, after } => (*before, Some(after.number)),
        ReviewRow::Wordwise(word) => (word.before_line, word.after_line),
        ReviewRow::LineEnding { .. } | ReviewRow::Elision(_) | ReviewRow::FileBoundary => {
            (None, None)
        }
    }
}

/// Current-world coverage represented by one public row; old-only ghosts have none.
fn current_world_coverage(row: &ReviewRow) -> Option<Range<usize>> {
    match row {
        ReviewRow::Current(line)
        | ReviewRow::Reflow(line)
        | ReviewRow::Moved { after: line, .. } => Some(line.number..line.number.saturating_add(1)),
        ReviewRow::Added(line) => Some(line.number..line.number.saturating_add(1)),
        ReviewRow::Wordwise(word) => word.after_line.map(|line| line..line.saturating_add(1)),
        ReviewRow::Elision(coverage) => coverage.after.clone(),
        ReviewRow::Removed(_) | ReviewRow::LineEnding { .. } | ReviewRow::FileBoundary => None,
    }
}

/// Public review rows remain ordered and singly owned inside each visual hunk.
///
/// Ownership is deliberately hunk-local: an ancestor breadcrumb may frame two distant hunks.
fn assert_source_space_invariants(path: &str, diff: &PresentedFile) {
    for (hunk_index, hunk) in diff.hunks.iter().enumerate() {
        let mut before_owners = HashSet::new();
        let mut after_owners = HashSet::new();
        let mut previous_current_end = None;
        for (row_index, row) in hunk.rows.iter().enumerate() {
            if let Some(current) = current_world_coverage(row) {
                if let Some(previous_end) = previous_current_end {
                    assert!(
                        current.start >= previous_end,
                        "{path} hunk {hunk_index} row {row_index} moves backwards or overlaps \
                         current source after {previous_end}: {row:#?}\n{hunk:#?}",
                    );
                }
                previous_current_end = Some(current.end);
            }

            let (before, after) = materialized_source_rows(row);
            if let Some(line) = before {
                assert!(
                    before_owners.insert(line),
                    "{path} hunk {hunk_index} gives before line {line} multiple display owners: \
                     {hunk:#?}",
                );
            }
            if let Some(line) = after {
                assert!(
                    after_owners.insert(line),
                    "{path} hunk {hunk_index} gives current line {line} multiple display owners: \
                     {hunk:#?}",
                );
            }
        }
    }
}

fn source_lines(diff: &PresentedFile) -> Vec<&SourceRow> {
    diff.hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .flat_map(|row| match row {
            ReviewRow::Current(line)
            | ReviewRow::Reflow(line)
            | ReviewRow::Moved { after: line, .. } => {
                vec![line]
            }
            ReviewRow::Removed(line) | ReviewRow::Added(line) => vec![line],
            ReviewRow::LineEnding { .. }
            | ReviewRow::Wordwise(_)
            | ReviewRow::Elision(_)
            | ReviewRow::FileBoundary => Vec::new(),
        })
        .collect()
}

fn marked_line_occurrences(diff: &PresentedFile, needle: &str, mark: DiffMark) -> usize {
    source_lines(diff)
        .into_iter()
        .filter(|line| line_text(line).contains(needle))
        .filter(|line| line.spans.iter().any(|span| span.mark == mark))
        .count()
}

fn assert_html_line_is_literal(rows: &[ReviewRow], needle: &str, case: &str) {
    assert!(
        rows.iter().any(|row| {
            matches!(row, ReviewRow::Removed(line) if line_text(line).contains(needle))
        }),
        "{case} retained the old literal line",
    );
    assert!(
        rows.iter().any(|row| {
            matches!(row, ReviewRow::Added(line) if line_text(line).contains(needle))
        }),
        "{case} omitted the new literal line",
    );
    assert!(
        !rows.iter().any(|row| {
            matches!(
                row,
                ReviewRow::Reflow(line) if line_text(line).contains(needle)
            )
        }),
        "{case} treated literal content as a structural reflow",
    );
}

fn hunk_containing<'a>(diff: &'a PresentedFile, needle: &str) -> &'a ReviewHunk {
    diff.hunks
        .iter()
        .find(|hunk| hunk_has_text(hunk, needle))
        .unwrap_or_else(|| panic!("fixture must contain a hunk for {needle}"))
}

fn hunk_has_text(hunk: &ReviewHunk, needle: &str) -> bool {
    hunk.rows.iter().any(|row| match row {
        ReviewRow::Current(line)
        | ReviewRow::Reflow(line)
        | ReviewRow::Moved { after: line, .. } => line_text(line).contains(needle),
        ReviewRow::Removed(line) | ReviewRow::Added(line) => line_text(line).contains(needle),
        ReviewRow::Wordwise(diff) => [
            diff.prefix.as_str(),
            diff.removed.as_str(),
            diff.added.as_str(),
            diff.suffix.as_str(),
        ]
        .concat()
        .contains(needle),
        ReviewRow::LineEnding { .. } => false,
        ReviewRow::Elision(_) => false,
        ReviewRow::FileBoundary => false,
    })
}

fn hunk_has_added_text(hunk: &ReviewHunk, needle: &str) -> bool {
    hunk.rows.iter().any(|row| match row {
        ReviewRow::Current(line)
        | ReviewRow::Reflow(line)
        | ReviewRow::Moved { after: line, .. } => line
            .spans
            .iter()
            .any(|span| span.mark == DiffMark::Added && span.text.contains(needle)),
        ReviewRow::Removed(_) => false,
        ReviewRow::Added(after) => after
            .spans
            .iter()
            .any(|span| span.mark == DiffMark::Added && span.text.contains(needle)),
        ReviewRow::Wordwise(diff) => diff.added.contains(needle),
        ReviewRow::LineEnding { .. } => false,
        ReviewRow::Elision(_) => false,
        ReviewRow::FileBoundary => false,
    })
}
