use super::*;
use crate::fixture::{AFTER, BEFORE, LABEL, web};
use std::collections::HashSet;

#[test]
fn fixture_keeps_payload_edits_ahead_of_pure_imports() {
    let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");

    assert_eq!(diff.hunks.len(), 2, "adjacent focus windows must coalesce");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();
    let import = rows
        .iter()
        .position(|row| matches!(row, DiffRow::Wordwise(_)))
        .expect("fixture must include its import replacement");
    let definition = rows
        .iter()
        .position(
            |row| matches!(row, DiffRow::Line(line) if line_text(line).contains("fn load_profile")),
        )
        .expect("fixture must include its first changed definition");
    let moved = rows
        .iter()
        .position(|row| {
            matches!(
                row,
                DiffRow::Moved { after, .. }
                    if after.number == 38 && line_text(after).contains("fn cache_key")
            )
        })
        .expect("fixture must include its moved definition");
    assert!(
        definition < moved,
        "source order must survive coalescing: {rows:#?}"
    );
    assert!(
        moved < import,
        "pure import churn belongs below payload-edit hunks: {rows:#?}"
    );
}

#[test]
fn pure_import_and_reflow_hunks_follow_payload_edits() {
    let stable = concat!(
        "const A: u8 = 0;\n",
        "const B: u8 = 0;\n",
        "const C: u8 = 0;\n",
        "const D: u8 = 0;\n",
        "const E: u8 = 0;\n",
        "const F: u8 = 0;\n",
        "const G: u8 = 0;\n",
        "const H: u8 = 0;\n",
    );
    let before = format!(
        "use crate::old;\n{stable}fn formatted() -> u8 {{ 1 }}\n{stable}fn logic() {{ old(); }}\n"
    );
    let after = format!(
        "use crate::new;\n{stable}fn formatted() -> u8 {{\n    1\n}}\n{stable}fn logic() {{ new(); }}\n"
    );
    let diff = diff_file("src/buoyancy.rs", &before, &after).expect("source must parse");

    let payload = diff
        .hunks
        .iter()
        .position(|hunk| hunk_has_text(hunk, "new();"))
        .expect("payload-edit hunk");
    let import = diff
        .hunks
        .iter()
        .position(|hunk| {
            hunk.rows
                .iter()
                .any(|row| matches!(row, DiffRow::Wordwise(_)))
        })
        .expect("wiring import hunk");
    let reflow = diff
        .hunks
        .iter()
        .position(|hunk| hunk_has_text(hunk, "fn formatted"))
        .expect("reflow hunk");

    assert!(payload < import && import < reflow, "{:#?}", diff.hunks);
}

#[test]
fn bodyless_module_wiring_follows_payload_edits() {
    let stable = concat!(
        "const A: u8 = 0;\n",
        "const B: u8 = 0;\n",
        "const C: u8 = 0;\n",
        "const D: u8 = 0;\n",
        "const E: u8 = 0;\n",
        "const F: u8 = 0;\n",
        "const G: u8 = 0;\n",
        "const H: u8 = 0;\n",
    );
    let before = format!("mod stable;\n{stable}fn logic() {{ old(); }}\n");
    let after =
        format!("mod stable;\nmod change;\nmod context;\n{stable}fn logic() {{ new(); }}\n");
    let diff = diff_file("src/diff.rs", &before, &after).expect("Rust must plan");
    let payload = diff
        .hunks
        .iter()
        .position(|hunk| hunk_has_text(hunk, "new();"))
        .expect("payload-edit hunk");
    let wiring = diff
        .hunks
        .iter()
        .position(|hunk| hunk_has_text(hunk, "mod change;"))
        .expect("module-wiring hunk");

    assert!(payload < wiring, "{:#?}", diff.hunks);
}

#[test]
fn mixed_module_mode_keeps_edit_buoyancy_in_both_directions() {
    let separation = concat!(
        "const A: u8 = 0;\n",
        "const B: u8 = 0;\n",
        "const C: u8 = 0;\n",
        "const D: u8 = 0;\n",
        "const E: u8 = 0;\n",
        "const F: u8 = 0;\n",
        "const G: u8 = 0;\n",
        "const H: u8 = 0;\n",
    );
    let inline = "mod subject { pub fn payload() {} }\n";
    let bodyless = "mod subject;\n";

    for (before_module, after_module) in [(inline, bodyless), (bodyless, inline)] {
        let before = format!("use crate::old;\n{separation}{before_module}");
        let after = format!("use crate::new;\n{separation}{after_module}");
        let diff = diff_file("src/lib.rs", &before, &after).expect("Rust must plan");
        let module = diff
            .hunks
            .iter()
            .position(|hunk| hunk_has_text(hunk, "pub fn payload"))
            .expect("module transition hunk");
        let import = diff
            .hunks
            .iter()
            .position(|hunk| {
                hunk.rows
                    .iter()
                    .any(|row| matches!(row, DiffRow::Wordwise(_)))
            })
            .expect("compact import hunk");

        assert!(module < import, "{:#?}", diff.hunks);
    }
}

#[test]
fn definition_hunk_keeps_hierarchy_local_context_and_distant_elision() {
    let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");
    let hunk = hunk_containing(&diff, "fn load_profile");

    assert!(
        hunk.rows
            .iter()
            .any(|row| matches!(row, DiffRow::Elision(_)))
    );
    for context in ["fn load_profile", "let cached", "profile.filter"] {
        assert!(hunk_has_text(hunk, context), "missing {context:?}");
    }

    let line_change = hunk.rows.iter().find_map(|row| {
        let DiffRow::LineChange { before, after } = row else {
            return None;
        };
        Some((before.as_ref()?, after.as_ref()?))
    });
    let Some((before_comment, after_comment)) = line_change else {
        panic!("comment edit must stay inside its definition hunk");
    };
    assert!(line_text(before_comment).contains("already trusted"));
    assert!(line_text(after_comment).contains("must be revalidated"));
    assert!(line_text(after_comment).starts_with("    //"));

    let payload = hunk.rows.iter().find_map(|row| {
        let DiffRow::Line(line) = row else {
            return None;
        };
        line.has_changes().then_some(line)
    });
    let Some(payload) = payload else {
        panic!("execution-point edit must remain a marked payload row");
    };
    assert!(line_text(payload).contains("cached.and_then(validate_profile)"));
    assert!(
        payload
            .spans
            .iter()
            .any(|span| { span.text.contains("and_then") && span.mark == DiffMark::Added })
    );
}

#[test]
fn structural_context_can_carry_a_hunk_to_eof() {
    let before = "fn run() { old(); }\n\n";
    let after = "fn run() { new(); }\n\n";

    let diff = diff_file("src/run.rs", before, after).expect("source must parse");
    let hunk = &diff.hunks[0];

    assert_eq!(hunk.coverage.after, Some(1..3));
    assert!(matches!(
        hunk.rows.iter().rev().nth(1),
        Some(DiffRow::Line(line)) if line.number == 2 && line_text(line).is_empty()
    ));
    assert!(matches!(hunk.rows.last(), Some(DiffRow::FileBoundary)));
}

#[test]
fn two_sided_hunk_uses_the_current_file_boundary() {
    let before = "fn run() { old(); }\n";
    let after = "fn run() { new(); }\n\nfn later() {}\n";

    let diff = diff_file("src/run.rs", before, after).expect("source must parse");
    let hunk = hunk_containing(&diff, "fn run");

    assert_eq!(diff.hunks.len(), 1);
    assert_eq!(hunk.coverage.before, Some(1..2));
    assert_eq!(hunk.coverage.after, Some(1..4));
    assert!(hunk_has_text(hunk, "fn later"));
    assert!(matches!(hunk.rows.last(), Some(DiffRow::FileBoundary)));
}

#[test]
fn move_hunk_lives_in_the_present_and_elides_its_unchanged_body() {
    let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");
    let hunk = hunk_containing(&diff, "fn cache_key");

    assert!(hunk.rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::Moved {
                before: Some(16),
                after,
            } if after.number == 38 && line_text(after).contains("fn cache_key")
        )
    }));
    assert!(
        hunk.rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Elision(coverage)
                    if coverage.before == Some(17..21) && coverage.after == Some(39..43)
            )
        }),
        "{hunk:#?}"
    );
    assert!(hunk.rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::Moved {
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
    let diff = diff_file("src/run.rs", before, after).expect("source must parse");
    let moved = diff
        .hunks
        .iter()
        .find(|hunk| matches!(hunk.rows.first(), Some(DiffRow::Moved { .. })))
        .expect("one definition must be presented as moved");

    assert!(matches!(
        moved.rows.as_slice(),
        [
            DiffRow::Moved { .. },
            DiffRow::Line(_),
            DiffRow::Moved { .. },
            DiffRow::FileBoundary
        ]
    ));
}

#[test]
fn moved_definition_keeps_its_terminator_edit_visible() {
    let before = "fn alpha() { alpha(); }\nfn beta() { beta(); }\n";
    let after = "fn beta() { beta(); }\nfn alpha() { alpha(); }\r\n";
    let diff = diff_file("src/run.rs", before, after).expect("source must parse");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    assert!(
        rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Moved {
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
            DiffRow::LineEnding {
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
    let diff = diff_file("src/move.rs", before, after).expect("source must parse");
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
                    DiffRow::Moved {
                        before: Some(1),
                        after,
                    },
                    DiffRow::LineEnding {
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
                DiffRow::LineChange { before, after }
                    if before.as_ref().is_some_and(|line| line.number == 1)
                        || after.as_ref().is_some_and(|line| line.number == 2)
            )
        }),
        "the move must remain the sole producer: {rows:#?}"
    );
    assert_source_space_invariants("src/move.rs", &diff);
}

#[test]
fn moved_reflow_owns_an_unmatched_missing_terminator() {
    let before = "fn alpha(){alpha();}\nfn beta(){beta();}\n";
    let after = "fn beta(){beta();}\nfn alpha() {\n    alpha();\n}";
    let diff = diff_file("src/move.rs", before, after).expect("source must parse");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    assert!(
        rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Moved { after, .. } if line_text(after).contains("fn alpha")
            )
        }),
        "{rows:#?}"
    );
    assert!(
        rows.windows(2).any(|rows| {
            matches!(
                rows,
                [
                    DiffRow::Moved { after, .. },
                    DiffRow::LineEnding {
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
            .any(|row| matches!(row, DiffRow::LineChange { .. })),
        "the move must remain the sole producer: {rows:#?}"
    );
    assert_source_space_invariants("src/move.rs", &diff);
}

#[test]
fn moved_reflow_keeps_an_unmatched_crlf_visible() {
    let before = "fn alpha(){alpha();}\nfn beta(){beta();}\n";
    let after = "fn beta(){beta();}\nfn alpha() {\r\n    alpha();\n}\n";
    let diff = diff_file("src/run.rs", before, after).expect("source must parse");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    assert!(
        rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Moved { after, .. }
                    if after.number == 2 && line_text(after).contains("fn alpha")
            )
        }),
        "{rows:#?}"
    );
    assert!(
        rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::LineEnding {
                    before: None,
                    after: Some(LineEnding::CrLf),
                }
            )
        }),
        "{rows:#?}"
    );
    assert_source_space_invariants("src/run.rs", &diff);
}

#[test]
fn imports_and_reflow_keep_their_signals_and_local_context() {
    let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");

    let import = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .find_map(|row| match row {
            DiffRow::Wordwise(import) => Some(import),
            _ => None,
        })
        .expect("fixture must include a wordwise import hunk");
    let formatting = hunk_containing(&diff, "fn render_response");
    assert_eq!(import.prefix, "use crate::telemetry::");
    assert_eq!(import.removed, "legacy_counter");
    assert_eq!(import.added, "{Metric, ReviewMeter}");
    assert_eq!(import.suffix, ";");

    assert!(
        formatting
            .rows
            .iter()
            .any(|row| matches!(row, DiffRow::Line(line) if line.number == 25))
    );
    assert!(
        formatting
            .rows
            .iter()
            .any(|row| matches!(row, DiffRow::Reflow(line) if line.number == 26))
    );
    assert!(
        formatting
            .rows
            .iter()
            .any(|row| matches!(row, DiffRow::Line(line) if line.number == 27))
    );
}

#[test]
fn identical_source_has_no_review_work() {
    let source = "use std::fmt;\n\nfn stable() { fmt::write(); }\n";
    let diff = diff_file("src/stable.rs", source, source).expect("source must parse");

    assert!(diff.hunks.is_empty());
}

#[test]
fn duplicate_definition_names_keep_one_to_one_correspondence() {
    let before = "impl Thing { fn first() { old(); } }\nimpl Thing { fn second() { stable(); } }\n";
    let after = "impl Thing { fn first() { new(); } }\nimpl Thing { fn second() { stable(); } }\n";

    let diff = diff_file("src/thing.rs", before, after).expect("source must parse");

    assert_eq!(diff.hunks.len(), 1);
    assert!(hunk_has_added_text(&diff.hunks[0], "new"));
    assert!(diff.hunks[0].rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::Line(line)
                if !line.has_changes() && line_text(line).contains("second")
        )
    }));
}

#[test]
fn inserted_call_wrapper_does_not_mark_unchanged_empty_calls() {
    let before = r#"async fn serve_one_turn() -> Result<()> {
    let harmony = HarmonyAdapter::gpt_oss()?;
    let mut parser = harmony.output_parser()?;
    let (generated_tx, mut generated_rx) = unbounded_channel();
    let history = history.to_owned();
    let also_hub = hub.clone();
    while let Some(event) = generated_rx.recv().await {}
    let calls = parser.finish();
    let message = error.to_string();
    Ok(())
}
"#;
    let after = r#"async fn serve_one_turn() -> Result<()> {
    // The constructor may populate a cache through a blocking client.
    let harmony = tokio::task::spawn_blocking(HarmonyAdapter::gpt_oss)
        .await
        .map_err(|error| eyre!(error))??;
    let mut parser = harmony.output_parser()?;
    let (generated_tx, mut generated_rx) = unbounded_channel();
    let history = history.to_owned();
    let also_hub = hub.clone();
    while let Some(event) = generated_rx.recv().await {}
    let calls = parser.finish();
    let message = error.to_string();
    Ok(())
}
"#;

    let diff = diff_file("src/hub.rs", before, after).expect("source must parse");

    assert!(hunk_has_added_text(&diff.hunks[0], "spawn_blocking"));
    assert_eq!(current_line_occurrences(&diff, "output_parser()"), 1);
    for unchanged in [
        "output_parser()",
        "unbounded_channel()",
        "to_owned()",
        "clone()",
        "recv()",
        "finish()",
        "to_string()",
    ] {
        assert_eq!(
            marked_line_occurrences(&diff, unchanged, DiffMark::Added),
            0,
            "unchanged call became added: {unchanged}"
        );
    }
}

#[test]
fn inserted_and_removed_comments_keep_their_source_side() {
    let plain = "fn run() {\n    work();\n}\n";
    let commented = "fn run() {\n    // explain why\n    work();\n}\n";

    let added = diff_file("src/run.rs", plain, commented).expect("source must parse");
    let removed = diff_file("src/run.rs", commented, plain).expect("source must parse");

    assert!(added.hunks[0].rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: None,
                after: Some(after),
            } if line_text(after).contains("explain why")
        )
    }));
    assert!(removed.hunks[0].rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: Some(before),
                after: None,
            } if line_text(before).contains("explain why")
        )
    }));
}

#[test]
fn comments_inside_an_added_definition_are_added_content() {
    let after = "fn run() {\n    // explain why\n    work();\n}\n";

    let diff = diff_file("src/run.rs", "", after).expect("source must parse");

    assert!(hunk_has_added_text(&diff.hunks[0], "explain why"));
}

#[test]
fn unknown_syntax_uses_aligned_line_leaves() {
    let before = "alpha\nold value\nomega\n";
    let after = "alpha\nnew value\nomega\n";

    let diff = diff_file("notes.txt", before, after).expect("plain diff cannot fail");

    assert!(!diff.generated);
    assert_eq!(diff.hunks.len(), 1);
    assert_eq!(diff.hunks[0].coverage.before, Some(1..4));
    assert_eq!(diff.hunks[0].coverage.after, Some(1..4));
    let changed = diff.hunks[0].rows.iter().find_map(|row| {
        let DiffRow::LineChange {
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
fn line_insertions_and_deletions_keep_their_source_numbers() {
    let before = "one\nremove\ntwo\nthree\n";
    let after = "one\ntwo\nadd\nthree\n";

    let diff = diff_file("notes", before, after).expect("plain diff cannot fail");
    let rows = &diff.hunks[0].rows;

    assert!(rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: Some(line),
                after: None,
            } if line.number == 2 && line_text(line) == "remove"
        )
    }));
    assert!(rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: None,
                after: Some(line),
            } if line.number == 3 && line_text(line) == "add"
        )
    }));
}

#[test]
fn html_wrapper_preserves_the_reindented_image_subtree() {
    let fixture = web::HTML;
    let diff =
        diff_file(fixture.path, fixture.before, fixture.after).expect("HTML projection must plan");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    for needle in [
        "<img",
        "class=\"profile-card__avatar\"",
        "src=\"/avatars/ada.webp\"",
        "alt=\"Ada Lovelace\"",
        "/>",
    ] {
        let retained = rows
            .iter()
            .filter_map(|row| {
                let DiffRow::Reflow(line) = row else {
                    return None;
                };
                line_text(line).contains(needle).then_some(line)
            })
            .collect::<Vec<_>>();
        let [line] = retained.as_slice() else {
            panic!("{needle:?} must appear once as retained current content");
        };
        assert!(line.spans.iter().all(|span| span.mark == DiffMark::Context));
        assert!(!rows.iter().any(|row| {
            let DiffRow::LineChange { before, after } = row else {
                return false;
            };
            before
                .iter()
                .chain(after)
                .any(|line| line_text(line).contains(needle))
        }));
    }

    for wrapper in ["<div class=\"profile-card__portrait\">", "</div>"] {
        assert!(rows.iter().any(|row| {
            let DiffRow::LineChange {
                before: None,
                after: Some(line),
            } = row
            else {
                return false;
            };
            line_text(line).trim() == wrapper
                && line.spans.iter().all(|span| span.mark == DiffMark::Added)
        }));
    }
}

#[test]
fn recovered_html_still_anchors_a_reindented_child() {
    // Browsers accept this common authoring state even though a `div` cannot
    // formally remain inside a `p`; the recovered CST still knows the image.
    let before = "<p>\n \t<img />\n</p>\n";
    let after = concat!(
        "<p>\n",
        "\t<div\n",
        "\t\tid=\"gege\"\n",
        "\t\tx-aria=\"takogoNet\"\t\n",
        "\t>\n",
        " \t\t<img />\n",
        "\t</div>\n",
        "</p>\n",
    );

    let diff = diff_file("sample.html", before, after).expect("HTML must plan");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();
    let image = rows
        .iter()
        .filter_map(|row| {
            let DiffRow::Reflow(line) = row else {
                return None;
            };
            (line_text(line).trim() == "<img />").then_some(line)
        })
        .collect::<Vec<_>>();

    assert_eq!(
        image.len(),
        1,
        "the recovered image must be one reflow anchor"
    );
    assert!(
        image[0]
            .spans
            .iter()
            .all(|span| span.mark == DiffMark::Context)
    );
    assert!(!rows.iter().any(|row| {
        let DiffRow::LineChange { before, after } = row else {
            return false;
        };
        before
            .iter()
            .chain(after)
            .any(|line| line_text(line).trim() == "<img />")
    }));
}

#[test]
fn renamed_definition_keeps_exact_body_lines_as_context() {
    let before = concat!(
        "mod tests {\n",
        "#[test]\n",
        "fn fixture_keeps_primary_logic_ahead_of_pure_imports() {\n",
        "    let diff = diff_file(LABEL, BEFORE, AFTER).expect(\"fixture must parse\");\n",
        "\n",
        "    assert_eq!(diff.hunks.len(), 2, \"adjacent focus windows must coalesce\");\n",
        "    let rows = diff\n",
        "        .hunks\n",
        "        .iter()\n",
        "        .flat_map(|hunk| &hunk.rows)\n",
        "        .collect::<Vec<_>>();\n",
        "    let import = rows\n",
        "        .iter()\n",
        "        .position(|row| matches!(row, DiffRow::Wordwise(_)))\n",
        "        .expect(\"fixture must include its import replacement\");\n",
        "}\n",
        "}\n",
    );
    let after = before
        .replace(
            "fixture_keeps_primary_logic_ahead_of_pure_imports",
            "fixture_keeps_payload_edits_ahead_of_pure_imports",
        )
        .replace("primary logic", "payload edits");

    let diff = diff_file("src/diff.rs", before, &after).expect("Rust must plan");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    for needle in [".hunks", ".flat_map", "let import = rows"] {
        assert!(
            !rows.iter().any(|row| {
                let DiffRow::LineChange { before, after } = row else {
                    return false;
                };
                before
                    .iter()
                    .chain(after)
                    .any(|line| line_text(line).contains(needle))
            }),
            "unchanged {needle:?} must be context or omitted, never a ghost/addition pair: {rows:#?}"
        );
    }
}

#[test]
fn inserted_test_does_not_drift_across_repeated_neighbor_bodies() {
    let before = concat!(
        "mod tests {\n",
        "#[test]\n",
        "fn first() {\n",
        "    let rows = compose();\n",
        "    assert!(rows.contains(\"first contract\"));\n",
        "}\n",
        "#[test]\n",
        "fn second() {\n",
        "    let rows = compose();\n",
        "    assert!(rows.contains(\"second contract\"));\n",
        "}\n",
        "#[test]\n",
        "fn third() {\n",
        "    let rows = compose();\n",
        "    assert!(rows.contains(\"third contract\"));\n",
        "}\n",
        "}\n",
    );
    let after = before.replace(
        "#[test]\nfn second()",
        concat!(
            "#[test]\n",
            "fn inserted() {\n",
            "    let rows = compose();\n",
            "    assert!(rows.contains(\"inserted contract\"));\n",
            "}\n",
            "#[test]\n",
            "fn second()",
        ),
    );

    let diff = diff_file("src/ui.rs", before, &after).expect("Rust must plan");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    for needle in ["fn second", "second contract", "fn third", "third contract"] {
        assert!(
            !rows.iter().any(|row| {
                let DiffRow::LineChange { before, after } = row else {
                    return false;
                };
                before
                    .iter()
                    .chain(after)
                    .any(|line| line_text(line).contains(needle))
            }),
            "stable neighbor {needle:?} must stay context or omitted: {rows:#?}"
        );
    }
}

#[test]
fn inserted_top_level_test_keeps_attributes_with_their_definitions() {
    let before = concat!(
        "#[test]\n",
        "fn first() { assert!(\"first contract\"); }\n",
        "#[test]\n",
        "fn second() { assert!(\"second contract\"); }\n",
    );
    let after = before.replace(
        "#[test]\nfn second()",
        concat!(
            "#[test]\n",
            "fn inserted() { assert!(\"inserted contract\"); }\n",
            "#[test]\n",
            "fn second()",
        ),
    );
    let diff = diff_file("src/tests.rs", before, &after).expect("Rust must plan");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    assert!(!rows.iter().any(|row| {
        let DiffRow::LineChange { before, after } = row else {
            return false;
        };
        before
            .iter()
            .chain(after)
            .any(|line| line_text(line).contains("fn second"))
    }));
    let removed_attributes = rows
        .iter()
        .filter(|row| {
            matches!(
                row,
                DiffRow::LineChange { before: Some(line), .. }
                    if line_text(line).trim() == "#[test]"
            )
        })
        .count();
    let added_attributes = rows
        .iter()
        .filter(|row| {
            matches!(
                row,
                DiffRow::LineChange { after: Some(line), .. }
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
    let diff = diff_file("src/decorated.rs", before, after).expect("Rust must plan");

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
    let diff = diff_file("src/nested.rs", before, after).expect("Rust must plan");

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
    let before = "<section>\n  <img src=\"avatar.webp\">\n  <div>\n    <p>Existing</p>\n  </div>\n</section>\n";
    let after = "<section>\n  <div>\n    <img src=\"avatar.webp\">\n  </div>\n  <div>\n    <p>Existing</p>\n  </div>\n</section>\n";

    let diff = diff_file("index.html", before, after).expect("HTML uses the line planner");
    let rows = &diff.hunks[0].rows;
    let retained = rows
        .iter()
        .filter(|row| {
            matches!(
                row,
                DiffRow::Reflow(line) if line_text(line).contains("<img")
            )
        })
        .count();

    assert_eq!(retained, 1, "{diff:#?}");
    assert!(!rows.iter().any(|row| {
        let DiffRow::LineChange { before, after } = row else {
            return false;
        };
        before
            .iter()
            .chain(after)
            .any(|line| line_text(line).contains("<img"))
    }));
}

#[test]
fn inline_html_wrapper_cannot_hide_unrelated_bytes_on_the_same_line() {
    let before = "<article><img src=\"avatar.webp\"></article>\n";
    let after = "<article><div><img src=\"avatar.webp\"></div></article>\n";

    let diff = diff_file("index.html", before, after).expect("HTML projection must plan");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    assert!(rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: Some(before),
                after: Some(after),
            } if line_text(before).contains("<article><img")
                && line_text(after).contains("<article><div><img")
        )
    }));
    assert!(!rows.iter().any(|row| { matches!(row, DiffRow::Reflow(_)) }));
}

#[test]
fn multiline_html_wrapper_keeps_mixed_indentation_atomic() {
    let before = "<img\nfixed=\"yes\"\n  src=\"avatar.webp\"\n/>\n";
    let after = "<div>\n  <img\nfixed=\"yes\"\n    src=\"avatar.webp\"\n  />\n</div>\n";

    let diff = diff_file("index.html", before, after).expect("HTML uses the line planner");
    let rows = &diff.hunks[0].rows;

    for needle in ["<img", "fixed=\"yes\"", "src=\"avatar.webp\"", "/>"] {
        let retained = rows
            .iter()
            .filter_map(|row| {
                let line = match row {
                    DiffRow::Line(line) | DiffRow::Reflow(line) => line,
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
            DiffRow::Line(line)
                if !line.has_changes() && line_text(line).contains("fixed=\"yes\"")
        )
    }));
    assert!(!rows.iter().any(|row| {
        let DiffRow::LineChange { before, after } = row else {
            return false;
        };
        before.iter().chain(after).any(|line| {
            ["<img", "fixed=\"yes\"", "src=\"avatar.webp\"", "/>"]
                .iter()
                .any(|needle| line_text(line).contains(needle))
        })
    }));
}

#[test]
fn html_raw_text_wrapper_remains_a_literal_change() {
    let before = "<textarea>\n  <img>\n</textarea>\n";
    let after = "<textarea>\n  <div>\n    <img>\n  </div>\n</textarea>\n";

    let diff = diff_file("index.html", before, after).expect("HTML uses the line planner");
    let rows = &diff.hunks[0].rows;

    assert_html_line_is_literal(rows, "<img>");
}

#[test]
fn quoted_pre_closing_text_does_not_end_literal_interpretation() {
    let before = "<pre>\n  <span title=\"</pre>\">\n  <img>\n</pre>\n";
    let after = "<pre>\n  <span title=\"</pre>\">\n  <div>\n    <img>\n  </div>\n</pre>\n";

    let diff = diff_file("index.html", before, after).expect("HTML uses the line planner");
    let rows = &diff.hunks[0].rows;

    assert_html_line_is_literal(rows, "<img>");
}

#[test]
fn raw_child_does_not_close_its_preformatted_parent() {
    let before = "<pre>\n<textarea>\n</pre>\n  <img>\n</textarea>\n</pre>\n";
    let after = "<pre>\n<textarea>\n</pre>\n  <div>\n    <img>\n  </div>\n</textarea>\n</pre>\n";

    let diff = diff_file("index.html", before, after).expect("HTML uses the line planner");
    let rows = &diff.hunks[0].rows;

    assert_html_line_is_literal(rows, "<img>");
}

#[test]
fn noscript_content_keeps_literal_correspondence() {
    let before = "<noscript>\n  <img>\n</noscript>\n";
    let after = "<noscript>\n  <div>\n    <img>\n  </div>\n</noscript>\n";

    let diff = diff_file("index.html", before, after).expect("HTML uses the line planner");
    let rows = &diff.hunks[0].rows;

    assert_html_line_is_literal(rows, "<img>");
}

#[test]
fn plaintext_never_resumes_html_correspondence() {
    let before = "<plaintext>\n</plaintext>\n  <img>\n";
    let after = "<plaintext>\n</plaintext>\n  <div>\n    <img>\n  </div>\n";

    let diff = diff_file("index.html", before, after).expect("HTML uses the line planner");
    let rows = &diff.hunks[0].rows;

    assert_html_line_is_literal(rows, "<img>");
}

#[test]
fn html_multiline_attribute_values_remain_literal_changes() {
    let before = "<img\n  title=\"first line\n    second line\"\n/>\n";
    let after = "<div>\n  <img\n    title=\"first line\n      second line\"\n  />\n</div>\n";

    let diff = diff_file("index.html", before, after).expect("HTML uses the line planner");
    let rows = &diff.hunks[0].rows;

    assert!(rows.iter().any(|row| {
        let DiffRow::LineChange { before, .. } = row else {
            return false;
        };
        before
            .as_ref()
            .is_some_and(|line| line_text(line).contains("second line"))
    }));
    assert!(rows.iter().any(|row| {
        let DiffRow::LineChange { after, .. } = row else {
            return false;
        };
        after
            .as_ref()
            .is_some_and(|line| line_text(line).contains("second line"))
    }));
    assert!(!rows.iter().any(|row| { matches!(row, DiffRow::Reflow(_)) }));
}

#[test]
fn generated_html_keeps_exact_correspondence() {
    let before = "<!-- @generated -->\n  <img src=\"avatar.webp\" />\n";
    let after = "<!-- @generated -->\n    <img src=\"avatar.webp\" />\n";

    let diff = diff_file("index.html", before, after).expect("generated HTML uses line diff");
    let rows = &diff.hunks[0].rows;

    assert!(diff.generated);
    assert!(rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: Some(before),
                after: Some(after),
            } if line_text(before).contains("<img") && line_text(after).contains("<img")
        )
    }));
    assert!(!rows.iter().any(|row| { matches!(row, DiffRow::Reflow(_)) }));
}

#[test]
fn typescript_fixture_keeps_declarations_and_syntax_in_the_graph_plan() {
    let fixture = web::TYPESCRIPT;
    let diff = diff_file(fixture.path, fixture.before, fixture.after)
        .expect("TypeScript fixture must parse");

    for declaration in [
        "export interface Profile",
        "export function cardTitle",
        "export function avatarAlt",
        "export function avatarSource",
    ] {
        assert_eq!(
            current_line_occurrences(&diff, declaration),
            1,
            "{declaration}"
        );
    }
    assert_eq!(current_line_occurrences(&diff, "string | null"), 1);
    assert_eq!(current_line_occurrences(&diff, " · "), 1);
    assert_eq!(current_line_occurrences(&diff, "Portrait of"), 1);
    assert_eq!(
        marked_line_occurrences(&diff, "avatarSource", DiffMark::Added),
        1
    );
    assert!(has_two_sided_line_changes(&diff));

    let syntax = displayed_syntax_classes(&diff);
    for expected in [
        SyntaxClass::Keyword,
        SyntaxClass::Type,
        SyntaxClass::String,
        SyntaxClass::Punctuation,
    ] {
        assert!(syntax.contains(&expected), "missing {expected:?} styling");
    }
}

#[test]
fn css_fixture_keeps_rules_and_declarations_in_the_graph_plan() {
    let fixture = web::CSS;
    let diff =
        diff_file(fixture.path, fixture.before, fixture.after).expect("CSS fixture must parse");
    assert_eq!(current_line_occurrences(&diff, ".profile-card {"), 1);
    assert_eq!(
        current_line_occurrences(&diff, "grid-template-columns: 7rem"),
        1
    );
    assert_eq!(
        current_line_occurrences(&diff, ".profile-card__portrait {"),
        1
    );
    assert_eq!(
        current_line_occurrences(&diff, ".profile-card__portrait img {"),
        1
    );
    assert_eq!(marked_line_occurrences(&diff, "7rem", DiffMark::Added), 1);
    assert_eq!(
        marked_line_occurrences(&diff, ".profile-card__avatar {", DiffMark::Removed),
        1,
        "{diff:#?}"
    );
    assert_eq!(current_line_occurrences(&diff, "aspect-ratio: 1;"), 1);
    assert_eq!(current_line_occurrences(&diff, "object-fit: cover;"), 1);
    assert!(diff.hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: Some(before),
                after: Some(after),
            } if line_text(before).contains("grid-template-columns: 6rem")
                && line_text(after).contains("grid-template-columns: 7rem")
        )
    }));

    let syntax = displayed_syntax_classes(&diff);
    for expected in [
        SyntaxClass::Identifier,
        SyntaxClass::Literal,
        SyntaxClass::Punctuation,
    ] {
        assert!(syntax.contains(&expected), "missing {expected:?} styling");
    }
}

#[test]
fn one_sided_plain_hunks_end_at_eof() {
    for (before, after) in [("only line\n", ""), ("", "only line\n")] {
        let diff = diff_file("notes.txt", before, after).expect("plain diff cannot fail");

        assert_eq!(diff.hunks.len(), 1);
        assert!(matches!(
            diff.hunks[0].rows.last(),
            Some(DiffRow::FileBoundary)
        ));
    }
}

#[test]
fn distant_plain_changes_become_focused_hunks() {
    let before = "one\nold two\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\nold twelve\nthirteen\nfourteen\n";
    let after = "one\nnew two\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\nnew twelve\nthirteen\nfourteen\n";

    let diff = diff_file("notes.md", before, after).expect("plain diff cannot fail");

    assert_eq!(diff.hunks.len(), 2);
    assert_eq!(diff.hunks[0].coverage.before, Some(1..6));
    assert_eq!(diff.hunks[0].coverage.after, Some(1..6));
    assert_eq!(diff.hunks[1].coverage.before, Some(9..15));
    assert_eq!(diff.hunks[1].coverage.after, Some(9..15));
    assert!(!matches!(
        diff.hunks[0].rows.last(),
        Some(DiffRow::FileBoundary)
    ));
    assert!(matches!(
        diff.hunks[1].rows.last(),
        Some(DiffRow::FileBoundary)
    ));
}

#[test]
fn plain_hunks_do_not_hide_one_context_line() {
    let before = "old first\none\ntwo\nthree\nfour\nfive\nsix\nseven\nold last\n";
    let after = "new first\none\ntwo\nthree\nfour\nfive\nsix\nseven\nnew last\n";

    let diff = diff_file("notes.txt", before, after).expect("plain diff cannot fail");

    assert_eq!(diff.hunks.len(), 1);
    assert!(diff.hunks[0].rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::Line(line)
                if !line.has_changes() && line.number == 5 && line_text(line) == "four"
        )
    }));
}

#[test]
fn line_projection_retains_end_of_file_newline_changes() {
    let diff = diff_file("notes.txt", "same\n", "same").expect("plain diff cannot fail");

    assert!(matches!(
        diff.hunks[0].rows.as_slice(),
        [
            DiffRow::LineChange {
                before: Some(before),
                after: Some(after),
            },
            DiffRow::LineEnding {
                before: Some(LineEnding::Lf),
                after: Some(LineEnding::Missing),
            },
            DiffRow::FileBoundary,
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
        let diff = diff_file("src/same.rs", before, after).expect("Rust input must plan");

        assert!(diff.hunks[0].rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::LineEnding {
                    before: Some(LineEnding::Lf),
                    after: Some(after),
                } if *after == expected_after
            )
        }));
        assert!(matches!(
            diff.hunks[0].rows.last(),
            Some(DiffRow::FileBoundary)
        ));
    }
}

#[test]
fn inserted_blank_layout_is_signal_without_flattening_its_definition() {
    let before = "fn run() { old(); }\n";
    let after = "\nfn run() { new(); }\n";
    let diff = diff_file("src/run.rs", before, after).expect("Rust input must plan");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    assert!(rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: None,
                after: Some(line),
            } if line.number == 1 && line_text(line).is_empty()
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
    let diff = diff_file("src/local.rs", before, after).expect("Rust input must plan");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    assert!(rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::LineEnding {
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
            DiffRow::LineChange {
                before: Some(line),
                after: None,
            } if line.number >= 3
        )
    }));
}

#[test]
fn terminator_fact_survives_an_expanded_local_replacement() {
    let before = concat!("fn run() {\n", "    old();\n", "    stable();\n", "}\n",);
    let after = concat!("fn run() {\r\n", "    new();\n", "    stable();\n", "}\n",);
    let diff = diff_file("src/local.rs", before, after).expect("Rust input must plan");
    let rows = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .collect::<Vec<_>>();

    assert!(rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::LineEnding {
                before: Some(LineEnding::Lf),
                after: Some(LineEnding::CrLf),
            }
        )
    }));
    assert!(rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: Some(before),
                after: None,
            } if before.number == 2 && line_text(before).contains("old")
        )
    }));
    assert!(rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: None,
                after: Some(after),
            } if after.number == 2 && line_text(after).contains("new")
        )
    }));
}

#[test]
fn generated_rust_is_flagged_and_forced_through_line_projection() {
    let before = "// @generated by build.rs\nuse crate::old;\n";
    let after = "use crate::new;\n";

    let diff = diff_file("src/bindings.rs", before, after).expect("plain diff cannot fail");
    let marker_added = diff_file("src/bindings.rs", after, before).expect("plain diff cannot fail");

    assert!(diff.generated);
    assert!(marker_added.generated);
    assert!(
        diff.hunks[0]
            .rows
            .iter()
            .any(|row| matches!(row, DiffRow::LineChange { .. }))
    );
    assert!(
        diff.hunks[0]
            .rows
            .iter()
            .all(|row| !matches!(row, DiffRow::Wordwise(_)))
    );
}

#[test]
fn representative_public_rows_obey_source_space_invariants() {
    let moved_before = concat!(
        "fn first() {\n",
        "    one();\n",
        "}\n",
        "fn second() {\n",
        "    two();\n",
        "}\n",
        "fn third() {\n",
        "    three();\n",
        "}\n",
    );
    let moved_after = concat!(
        "fn third() {\n",
        "    three();\n",
        "}\n",
        "fn second() {\n",
        "    two();\n",
        "}\n",
        "fn first() {\n",
        "    changed_one();\n",
        "    extra();\n",
        "}\n",
    );
    let distant_before = concat!(
        "one\n",
        "old two\n",
        "three\n",
        "four\n",
        "five\n",
        "six\n",
        "seven\n",
        "eight\n",
        "nine\n",
        "ten\n",
        "eleven\n",
        "old twelve\n",
        "thirteen\n",
        "fourteen\n",
    );
    let distant_after = concat!(
        "one\n",
        "new two\n",
        "three\n",
        "four\n",
        "five\n",
        "six\n",
        "seven\n",
        "eight\n",
        "nine\n",
        "ten\n",
        "eleven\n",
        "new twelve\n",
        "thirteen\n",
        "fourteen\n",
    );
    let edge_cases = [
        ("src/order.rs", moved_before, moved_after),
        ("notes.md", distant_before, distant_after),
        ("notes.txt", "same\n", "same"),
        ("src/removed.rs", "fn removed() {}\n", ""),
        ("src/added.rs", "", "fn added() {}\n"),
    ];

    let fixtures = [(LABEL, BEFORE, AFTER)]
        .into_iter()
        .chain(
            web::ALL
                .iter()
                .map(|fixture| (fixture.path, fixture.before, fixture.after)),
        )
        .chain(edge_cases);
    let mut observed = ObservedRowKinds::default();
    for (path, before, after) in fixtures {
        let diff = diff_file(path, before, after).expect("source must plan");

        assert_source_space_invariants(path, &diff);
        observed.extend(&diff);
    }
    assert!(observed.line, "matrix must exercise ordinary source rows");
    assert!(observed.reflow, "matrix must exercise reflow rows");
    assert!(observed.line_change, "matrix must exercise line changes");
    assert!(observed.line_ending, "matrix must exercise line endings");
    assert!(observed.moved, "matrix must exercise moves");
    assert!(observed.wordwise, "matrix must exercise compact changes");
    assert!(observed.elision, "matrix must exercise elisions");
    assert!(
        observed.file_boundary,
        "matrix must exercise file boundaries"
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
fn malformed_or_linewise_rust_still_materializes_source_changes() {
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

        assert!(!diff.hunks.is_empty());
        assert!(
            diff.hunks
                .iter()
                .flat_map(|hunk| &hunk.rows)
                .any(|row| matches!(row, DiffRow::LineChange { .. }))
        );
    }
}

fn line_text(line: &DisplayLine) -> String {
    line.spans.iter().map(|span| span.text.as_str()).collect()
}

/// Numbered source rows materially rendered by one public row.
fn materialized_source_rows(row: &DiffRow) -> (Option<usize>, Option<usize>) {
    match row {
        DiffRow::Line(line) | DiffRow::Reflow(line) => (None, Some(line.number)),
        DiffRow::LineChange { before, after } => (
            before.as_ref().map(|line| line.number),
            after.as_ref().map(|line| line.number),
        ),
        DiffRow::Moved { before, after } => (*before, Some(after.number)),
        DiffRow::Wordwise(word) => (word.before_line, word.after_line),
        DiffRow::LineEnding { .. } | DiffRow::Elision(_) | DiffRow::FileBoundary => (None, None),
    }
}

/// Current-world coverage represented by one public row; old-only ghosts have none.
fn current_world_coverage(row: &DiffRow) -> Option<Range<usize>> {
    match row {
        DiffRow::Line(line) | DiffRow::Reflow(line) | DiffRow::Moved { after: line, .. } => {
            Some(line.number..line.number.saturating_add(1))
        }
        DiffRow::LineChange {
            after: Some(line), ..
        } => Some(line.number..line.number.saturating_add(1)),
        DiffRow::Wordwise(word) => word.after_line.map(|line| line..line.saturating_add(1)),
        DiffRow::Elision(coverage) => coverage.after.clone(),
        DiffRow::LineChange { after: None, .. }
        | DiffRow::LineEnding { .. }
        | DiffRow::FileBoundary => None,
    }
}

/// Public review rows remain ordered and singly owned inside each visual hunk.
///
/// Ownership is deliberately hunk-local: an ancestor breadcrumb may frame two distant hunks.
fn assert_source_space_invariants(path: &str, diff: &FileDiff) {
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

#[derive(Default)]
struct ObservedRowKinds {
    line: bool,
    reflow: bool,
    line_change: bool,
    line_ending: bool,
    moved: bool,
    wordwise: bool,
    elision: bool,
    file_boundary: bool,
}

impl ObservedRowKinds {
    fn extend(&mut self, diff: &FileDiff) {
        for row in diff.hunks.iter().flat_map(|hunk| &hunk.rows) {
            match row {
                DiffRow::Line(_) => self.line = true,
                DiffRow::Reflow(_) => self.reflow = true,
                DiffRow::LineChange { .. } => self.line_change = true,
                DiffRow::LineEnding { .. } => self.line_ending = true,
                DiffRow::Moved { .. } => self.moved = true,
                DiffRow::Wordwise(_) => self.wordwise = true,
                DiffRow::Elision(_) => self.elision = true,
                DiffRow::FileBoundary => self.file_boundary = true,
            }
        }
    }
}

fn source_lines(diff: &FileDiff) -> Vec<&DisplayLine> {
    diff.hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .flat_map(|row| match row {
            DiffRow::Line(line) | DiffRow::Reflow(line) | DiffRow::Moved { after: line, .. } => {
                vec![line]
            }
            DiffRow::LineChange { before, after } => before.iter().chain(after).collect::<Vec<_>>(),
            DiffRow::LineEnding { .. }
            | DiffRow::Wordwise(_)
            | DiffRow::Elision(_)
            | DiffRow::FileBoundary => Vec::new(),
        })
        .collect()
}

fn current_line_occurrences(diff: &FileDiff, needle: &str) -> usize {
    source_lines(diff)
        .into_iter()
        .filter(|line| !line.spans.iter().any(|span| span.mark == DiffMark::Removed))
        .filter(|line| line_text(line).contains(needle))
        .count()
}

fn marked_line_occurrences(diff: &FileDiff, needle: &str, mark: DiffMark) -> usize {
    source_lines(diff)
        .into_iter()
        .filter(|line| line_text(line).contains(needle))
        .filter(|line| line.spans.iter().any(|span| span.mark == mark))
        .count()
}

fn displayed_syntax_classes(diff: &FileDiff) -> Vec<SyntaxClass> {
    source_lines(diff)
        .into_iter()
        .flat_map(|line| line.spans.iter().map(|span| span.syntax))
        .collect()
}

fn has_two_sided_line_changes(diff: &FileDiff) -> bool {
    diff.hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: Some(_),
                after: Some(_),
            }
        )
    })
}

fn assert_html_line_is_literal(rows: &[DiffRow], needle: &str) {
    assert!(rows.iter().any(|row| {
        let DiffRow::LineChange { before, .. } = row else {
            return false;
        };
        before
            .as_ref()
            .is_some_and(|line| line_text(line).contains(needle))
    }));
    assert!(rows.iter().any(|row| {
        let DiffRow::LineChange { after, .. } = row else {
            return false;
        };
        after
            .as_ref()
            .is_some_and(|line| line_text(line).contains(needle))
    }));
    assert!(!rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::Reflow(line) if line_text(line).contains(needle)
        )
    }));
}

fn hunk_containing<'a>(diff: &'a FileDiff, needle: &str) -> &'a Hunk {
    diff.hunks
        .iter()
        .find(|hunk| hunk_has_text(hunk, needle))
        .unwrap_or_else(|| panic!("fixture must contain a hunk for {needle}"))
}

fn hunk_has_text(hunk: &Hunk, needle: &str) -> bool {
    hunk.rows.iter().any(|row| match row {
        DiffRow::Line(line) | DiffRow::Reflow(line) | DiffRow::Moved { after: line, .. } => {
            line_text(line).contains(needle)
        }
        DiffRow::LineChange { before, after } => {
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
        DiffRow::FileBoundary => false,
    })
}

fn hunk_has_added_text(hunk: &Hunk, needle: &str) -> bool {
    hunk.rows.iter().any(|row| match row {
        DiffRow::Line(line) | DiffRow::Reflow(line) | DiffRow::Moved { after: line, .. } => line
            .spans
            .iter()
            .any(|span| span.mark == DiffMark::Added && span.text.contains(needle)),
        DiffRow::LineChange { after, .. } => after.as_ref().is_some_and(|after| {
            after
                .spans
                .iter()
                .any(|span| span.mark == DiffMark::Added && span.text.contains(needle))
        }),
        DiffRow::Wordwise(diff) => diff.added.contains(needle),
        DiffRow::LineEnding { .. } => false,
        DiffRow::Elision(_) => false,
        DiffRow::FileBoundary => false,
    })
}
