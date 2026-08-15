mod correspondence;
mod plan;
mod projection;
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
pub struct CodeSpan {
    pub text: String,
    pub syntax: SyntaxClass,
    pub mark: DiffMark,
}

/// Original source line retained inside a bounded diff hunk.
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

/// One-based, half-open before/after bounds covered by a review row or hunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineCoverage {
    pub before: Option<Range<usize>>,
    pub after: Option<Range<usize>>,
}

/// How one current-world source line communicates its role in the hunk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodeRole {
    Context,
    Inline,
    Reflow,
}

/// Presentation-ready row chosen while planning a bounded diff hunk.
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

/// Project both revisions, find graph correspondence, then plan a bounded review.
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
    let (pair, correspondence) = if correspondence.requires_line_fallback
        && pair.before.language != projection::Language::Lines
    {
        let pair =
            projection::line_pair(before, after, projection::FallbackReason::SourceExactness);
        let correspondence = correspondence::correspond(&pair);
        (pair, correspondence)
    } else {
        (pair, correspondence)
    };

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
mod tests {
    use super::*;
    use crate::fixture::{AFTER, BEFORE, LABEL, web};

    #[test]
    fn fixture_becomes_an_ordered_stream_of_hunks() {
        let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");

        assert_eq!(diff.hunks.len(), 6);
        assert!(matches!(
            diff.hunks[4].rows.as_slice(),
            [DiffRow::Wordwise(_)]
        ));
        assert!(matches!(
            diff.hunks[5].rows.as_slice(),
            [
                DiffRow::Code { .. },
                DiffRow::Code { .. },
                DiffRow::Code { .. }
            ]
        ));
    }

    #[test]
    fn definition_hunk_groups_treatments_and_elides_distant_context() {
        let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");
        let hunk = hunk_containing(&diff, "fn load_profile");

        assert_eq!(hunk.coverage.before, Some(23..31));
        assert_eq!(hunk.coverage.after, Some(16..24));
        assert_eq!(hunk.rows.len(), 8);
        assert_eq!(
            hunk.rows
                .iter()
                .filter(|row| matches!(row, DiffRow::Elision(_)))
                .count(),
            2
        );

        let linewise = hunk.rows.iter().find_map(|row| {
            let DiffRow::Linewise { before, after } = row else {
                return None;
            };
            Some((before.as_ref()?, after.as_ref()?))
        });
        let Some((before_comment, after_comment)) = linewise else {
            panic!("comment edit must stay inside its definition hunk");
        };
        assert!(line_text(before_comment).contains("already trusted"));
        assert!(line_text(after_comment).contains("must be revalidated"));
        assert!(line_text(after_comment).starts_with("    //"));

        let inline = hunk.rows.iter().find_map(|row| {
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
    fn structural_frame_can_carry_a_hunk_to_eof() {
        let before = "fn run() { old(); }\n\n";
        let after = "fn run() { new(); }\n\n";

        let diff = diff_file("src/run.rs", before, after).expect("source must parse");
        let hunk = &diff.hunks[0];

        assert_eq!(hunk.coverage.after, Some(1..2));
        assert!(matches!(
            hunk.rows.iter().rev().nth(1),
            Some(DiffRow::Code { line, .. }) if line.number == 2 && line_text(line).is_empty()
        ));
        assert!(matches!(hunk.rows.last(), Some(DiffRow::FileBoundary)));
    }

    #[test]
    fn two_sided_hunk_uses_the_current_file_boundary() {
        let before = "fn run() { old(); }\n";
        let after = "fn run() { new(); }\n\nfn later() {}\n";

        let diff = diff_file("src/run.rs", before, after).expect("source must parse");
        let run = hunk_containing(&diff, "fn run");
        let later = hunk_containing(&diff, "fn later");

        assert_eq!(run.coverage.before, Some(1..2));
        assert_eq!(run.coverage.after, Some(1..2));
        assert!(!matches!(run.rows.last(), Some(DiffRow::FileBoundary)));
        assert!(matches!(later.rows.last(), Some(DiffRow::FileBoundary)));
    }

    #[test]
    fn one_context_line_between_edits_stays_visible() {
        let before = "fn run() {\n    before_one();\n\n    before_two();\n}\n";
        let after = "fn run() {\n    after_one();\n\n    after_two();\n}\n";
        let diff = diff_file("src/run.rs", before, after).expect("source must parse");
        let hunk = hunk_containing(&diff, "fn run");

        assert!(
            hunk.rows
                .iter()
                .all(|row| !matches!(row, DiffRow::Elision(_)))
        );
        assert!(hunk.rows.iter().any(|row| {
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
    fn move_hunk_lives_in_the_present_and_elides_its_unchanged_body() {
        let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");
        let hunk = hunk_containing(&diff, "fn cache_key");

        assert_eq!(hunk.coverage.before, Some(16..22));
        assert_eq!(hunk.coverage.after, Some(38..44));
        assert_eq!(hunk.rows.len(), 3);

        let DiffRow::Moved {
            before: Some(before),
            after,
        } = &hunk.rows[0]
        else {
            panic!("move must begin with a before-to-after line correspondence");
        };
        assert_eq!((*before, after.number), (16, 38));
        assert!(line_text(after).contains("fn cache_key"));

        let DiffRow::Elision(coverage) = &hunk.rows[1] else {
            panic!("unchanged moved body must be abbreviated");
        };
        assert_eq!(coverage.before, Some(17..21));
        assert_eq!(coverage.after, Some(39..43));

        let DiffRow::Moved {
            before: None,
            after,
        } = &hunk.rows[2]
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
            .hunks
            .iter()
            .find(|hunk| matches!(hunk.rows.first(), Some(DiffRow::Moved { .. })))
            .expect("one definition must be presented as moved");

        assert!(matches!(
            moved.rows.as_slice(),
            [
                DiffRow::Moved { .. },
                DiffRow::Code {
                    role: CodeRole::Context,
                    ..
                },
                DiffRow::Moved { .. },
                DiffRow::FileBoundary
            ]
        ));
    }

    #[test]
    fn expanded_fixture_adds_distinct_policy_and_payload_hunks() {
        let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");

        let policy = hunk_containing(&diff, "fn should_refresh");
        assert!(hunk_has_text(policy, "Only stale profiles"));
        assert!(hunk_has_text(policy, "Stale and legacy profiles"));
        assert!(hunk_has_text(
            policy,
            "profile.schema < 4 || age > Duration::from_secs(300)"
        ));
        assert!(hunk_has_added_text(policy, "schema"));

        let normalization = hunk_containing(&diff, "fn display_label");
        assert!(hunk_has_added_text(normalization, "replace"));
        assert!(hunk_has_text(
            normalization,
            "profile.display_name.trim().to_owned().replace('\\n', \" \")"
        ));
    }

    #[test]
    fn imports_and_reflow_use_distinct_late_hunks() {
        let diff = diff_file(LABEL, BEFORE, AFTER).expect("fixture must parse");

        let import = diff
            .hunks
            .iter()
            .find(|hunk| matches!(hunk.rows.as_slice(), [DiffRow::Wordwise(_)]))
            .expect("fixture must include a wordwise import hunk");
        let formatting = hunk_containing(&diff, "fn render_response");
        let [DiffRow::Wordwise(import)] = import.rows.as_slice() else {
            panic!("import replacement must be a wordwise row");
        };
        assert_eq!(import.prefix, "use crate::telemetry::");
        assert_eq!(import.removed, "legacy_counter");
        assert_eq!(import.added, "{Metric, ReviewMeter}");
        assert_eq!(import.suffix, ";");

        assert_eq!(formatting.coverage.before, Some(32..38));
        assert_eq!(formatting.coverage.after, Some(25..28));
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

        assert!(diff.hunks.is_empty());
    }

    #[test]
    fn identical_recovered_source_has_no_review_work() {
        let source = "fn broken(value: u32 {}\n";

        let diff = diff_file("src/broken.rs", source, source).expect("parser must recover");

        assert!(diff.hunks.is_empty());
    }

    #[test]
    fn duplicate_definition_names_keep_one_to_one_correspondence() {
        let before =
            "impl Thing { fn first() { old(); } }\nimpl Thing { fn second() { stable(); } }\n";
        let after =
            "impl Thing { fn first() { new(); } }\nimpl Thing { fn second() { stable(); } }\n";

        let diff = diff_file("src/thing.rs", before, after).expect("source must parse");

        assert_eq!(diff.hunks.len(), 1);
        assert!(hunk_has_added_text(&diff.hunks[0], "new"));
        assert!(!hunk_has_text(&diff.hunks[0], "second"));
    }

    #[test]
    fn inserted_and_removed_comments_keep_their_source_side() {
        let plain = "fn run() {\n    work();\n}\n";
        let commented = "fn run() {\n    // explain why\n    work();\n}\n";

        let added = diff_file("src/run.rs", plain, commented).expect("source must parse");
        let removed = diff_file("src/run.rs", commented, plain).expect("source must parse");

        assert!(matches!(
            added.hunks[0].rows.as_slice(),
            [DiffRow::Linewise {
                before: None,
                after: Some(_)
            }]
        ));
        assert!(matches!(
            removed.hunks[0].rows.as_slice(),
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
    fn line_insertions_and_deletions_keep_their_source_numbers() {
        let before = "one\nremove\ntwo\nthree\n";
        let after = "one\ntwo\nadd\nthree\n";

        let diff = diff_file("notes", before, after).expect("plain diff cannot fail");
        let rows = &diff.hunks[0].rows;

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
    fn html_wrapper_preserves_the_reindented_image_subtree() {
        let fixture = web::HTML;
        let diff = diff_file(fixture.path, fixture.before, fixture.after)
            .expect("HTML projection must plan");
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
                    let DiffRow::Code { line, role } = row else {
                        return None;
                    };
                    line_text(line).contains(needle).then_some((line, role))
                })
                .collect::<Vec<_>>();
            let [(line, role)] = retained.as_slice() else {
                panic!("{needle:?} must appear once as retained current content");
            };
            assert_eq!(**role, CodeRole::Reflow);
            assert!(line.spans.iter().all(|span| span.mark == DiffMark::Context));
            assert!(!rows.iter().any(|row| {
                let DiffRow::Linewise { before, after } = row else {
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
                let DiffRow::Linewise {
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
                    DiffRow::Code {
                        line,
                        role: CodeRole::Reflow,
                    } if line_text(line).contains("<img")
                )
            })
            .count();

        assert_eq!(retained, 1, "{diff:#?}");
        assert!(!rows.iter().any(|row| {
            let DiffRow::Linewise { before, after } = row else {
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
                DiffRow::Linewise {
                    before: Some(before),
                    after: Some(after),
                } if line_text(before).contains("<article><img")
                    && line_text(after).contains("<article><div><img")
            )
        }));
        assert!(!rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Code {
                    role: CodeRole::Reflow,
                    ..
                }
            )
        }));
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
                    let DiffRow::Code { line, role } = row else {
                        return None;
                    };
                    line_text(line).contains(needle).then_some(*role)
                })
                .collect::<Vec<_>>();
            assert_eq!(retained.len(), 1, "{needle:?} must stay in one tag block");
        }
        assert!(rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Code {
                    line,
                    role: CodeRole::Context,
                } if line_text(line).contains("fixed=\"yes\"")
            )
        }));
        assert!(!rows.iter().any(|row| {
            let DiffRow::Linewise { before, after } = row else {
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
    fn quoted_pre_closing_text_does_not_end_literal_treatment() {
        let before = "<pre>\n  <span title=\"</pre>\">\n  <img>\n</pre>\n";
        let after = "<pre>\n  <span title=\"</pre>\">\n  <div>\n    <img>\n  </div>\n</pre>\n";

        let diff = diff_file("index.html", before, after).expect("HTML uses the line planner");
        let rows = &diff.hunks[0].rows;

        assert_html_line_is_literal(rows, "<img>");
    }

    #[test]
    fn raw_child_does_not_close_its_preformatted_parent() {
        let before = "<pre>\n<textarea>\n</pre>\n  <img>\n</textarea>\n</pre>\n";
        let after =
            "<pre>\n<textarea>\n</pre>\n  <div>\n    <img>\n  </div>\n</textarea>\n</pre>\n";

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
            let DiffRow::Linewise { before, .. } = row else {
                return false;
            };
            before
                .as_ref()
                .is_some_and(|line| line_text(line).contains("second line"))
        }));
        assert!(rows.iter().any(|row| {
            let DiffRow::Linewise { after, .. } = row else {
                return false;
            };
            after
                .as_ref()
                .is_some_and(|line| line_text(line).contains("second line"))
        }));
        assert!(!rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Code {
                    role: CodeRole::Reflow,
                    ..
                }
            )
        }));
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
                DiffRow::Linewise {
                    before: Some(before),
                    after: Some(after),
                } if line_text(before).contains("<img") && line_text(after).contains("<img")
            )
        }));
        assert!(!rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Code {
                    role: CodeRole::Reflow,
                    ..
                }
            )
        }));
    }

    #[test]
    fn every_web_fixture_produces_review_work() {
        let paths = web::ALL
            .iter()
            .map(|fixture| fixture.path)
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            [
                "web/profile-card.css",
                "web/profile-card.html",
                "web/profile-card.ts",
            ]
        );

        for fixture in web::ALL {
            let diff = diff_file(fixture.path, fixture.before, fixture.after)
                .expect("web fixture uses a supported fallback");

            assert!(
                !diff.hunks.is_empty(),
                "{} needs a visible diff",
                fixture.path
            );
        }
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
        assert!(!has_two_sided_linewise_rows(&diff));

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
                DiffRow::Code {
                    line,
                    role: CodeRole::Inline,
                } if line_text(line).contains("grid-template-columns: 7rem")
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
                DiffRow::Code {
                    line,
                    role: CodeRole::Context,
                } if line.number == 5 && line_text(line) == "four"
            )
        }));
    }

    #[test]
    fn line_projection_retains_end_of_file_newline_changes() {
        let diff = diff_file("notes.txt", "same\n", "same").expect("plain diff cannot fail");

        assert!(matches!(
            diff.hunks[0].rows.as_slice(),
            [
                DiffRow::Linewise {
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
    fn rust_terminator_edits_reproject_both_revisions_as_line_leaves() {
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
    fn generated_rust_is_flagged_and_forced_through_line_projection() {
        let before = "// @generated by build.rs\nuse crate::old;\n";
        let after = "use crate::new;\n";

        let diff = diff_file("src/bindings.rs", before, after).expect("plain diff cannot fail");
        let marker_added =
            diff_file("src/bindings.rs", after, before).expect("plain diff cannot fail");

        assert!(diff.generated);
        assert!(marker_added.generated);
        assert!(
            diff.hunks[0]
                .rows
                .iter()
                .any(|row| matches!(row, DiffRow::Linewise { .. }))
        );
        assert!(
            diff.hunks[0]
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

            assert!(!diff.hunks.is_empty());
            assert!(
                diff.hunks
                    .iter()
                    .flat_map(|hunk| &hunk.rows)
                    .any(|row| matches!(row, DiffRow::Linewise { .. }))
            );
        }
    }

    #[test]
    fn identical_line_projected_source_has_no_review_work() {
        let source = "plain text\nwith no grammar\n";

        let diff = diff_file("README", source, source).expect("plain diff cannot fail");

        assert!(diff.hunks.is_empty());
    }

    #[test]
    fn large_anchorless_alignment_uses_the_bounded_fallback() {
        let before = vec!["same"; 200];
        let after = vec!["same"; 200];

        let matches = correspondence::ordered_matches(&before, &after);

        assert_eq!(matches.len(), 200);
        assert_eq!(
            matches.first().map(|edge| (edge.before, edge.after)),
            Some((0, 0))
        );
        assert_eq!(
            matches.last().map(|edge| (edge.before, edge.after)),
            Some((199, 199))
        );
    }

    fn line_text(line: &CodeLine) -> String {
        line.spans.iter().map(|span| span.text.as_str()).collect()
    }

    fn source_lines(diff: &FileDiff) -> Vec<&CodeLine> {
        diff.hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .flat_map(|row| match row {
                DiffRow::Code { line, .. } | DiffRow::Moved { after: line, .. } => vec![line],
                DiffRow::Linewise { before, after } => {
                    before.iter().chain(after).collect::<Vec<_>>()
                }
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

    fn has_two_sided_linewise_rows(diff: &FileDiff) -> bool {
        diff.hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
            matches!(
                row,
                DiffRow::Linewise {
                    before: Some(_),
                    after: Some(_),
                }
            )
        })
    }

    fn assert_html_line_is_literal(rows: &[DiffRow], needle: &str) {
        assert!(rows.iter().any(|row| {
            let DiffRow::Linewise { before, .. } = row else {
                return false;
            };
            before
                .as_ref()
                .is_some_and(|line| line_text(line).contains(needle))
        }));
        assert!(rows.iter().any(|row| {
            let DiffRow::Linewise { after, .. } = row else {
                return false;
            };
            after
                .as_ref()
                .is_some_and(|line| line_text(line).contains(needle))
        }));
        assert!(!rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::Code {
                    line,
                    role: CodeRole::Reflow,
                } if line_text(line).contains(needle)
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
            DiffRow::FileBoundary => false,
        })
    }

    fn hunk_has_added_text(hunk: &Hunk, needle: &str) -> bool {
        hunk.rows.iter().any(|row| match row {
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
            DiffRow::FileBoundary => false,
        })
    }
}
