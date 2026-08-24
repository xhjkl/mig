use super::*;
use crate::diff::SyntaxClass;
use crate::diff::correspondence::correspond;
use crate::diff::projection::project_pair;
use std::collections::HashSet;
use std::path::Path;

fn planned(path: &str, before: &str, after: &str) -> Vec<Hunk> {
    let pair = project_pair(Path::new(path), before, after, false).unwrap();
    let correspondence = correspond(&pair);
    let hunks = plan_hunks(&pair, &correspondence);
    assert_source_space_invariants(path, &hunks);
    assert_local_fallbacks_are_signal(path, &correspondence, &hunks);
    hunks
}

/// Every non-exact row admitted to local line review remains materialized as an edit.
fn assert_local_fallbacks_are_signal(path: &str, correspondence: &Correspondence, hunks: &[Hunk]) {
    let mut before_signal = HashSet::new();
    let mut after_signal = HashSet::new();
    for row in hunks.iter().flat_map(|hunk| &hunk.rows) {
        let DiffRow::LineChange { before, after } = row else {
            continue;
        };
        before_signal.extend(before.as_ref().map(|line| line.number.saturating_sub(1)));
        after_signal.extend(after.as_ref().map(|line| line.number.saturating_sub(1)));
    }
    let before_exact = correspondence
        .line_links
        .iter()
        .map(|link| link.before)
        .collect::<HashSet<_>>();
    let after_exact = correspondence
        .line_links
        .iter()
        .map(|link| link.after)
        .collect::<HashSet<_>>();

    for fallback in &correspondence.line_fallbacks {
        for line in fallback.before.clone() {
            assert!(
                before_exact.contains(&line) || before_signal.contains(&line),
                "{path} hides before line {} inside local fallback {fallback:#?}: {hunks:#?}",
                line + 1,
            );
        }
        for line in fallback.after.clone() {
            assert!(
                after_exact.contains(&line) || after_signal.contains(&line),
                "{path} hides current line {} inside local fallback {fallback:#?}: {hunks:#?}",
                line + 1,
            );
        }
    }
}

#[test]
fn move_merge_window_does_not_borrow_an_edit_halo() {
    let moved = merge_window(1..2, Signal::Move);
    let near_edit = merge_window(6..7, Signal::Edit);
    let distant_edit = merge_window(9..10, Signal::Edit);
    let first_edit = merge_window(1..2, Signal::Edit);

    assert!(merge_windows_touch(&moved, &near_edit));
    assert!(!merge_windows_touch(&moved, &distant_edit));
    assert!(merge_windows_touch(&first_edit, &distant_edit));
}

#[test]
fn fallback_index_ignores_an_empty_counterpart_coordinate() {
    let index = FallbackIndex::new(&[crate::diff::correspondence::LineFallback {
        before: 7..8,
        after: 7..7,
    }]);

    assert!(indexed_ranges_overlap(&index.before, &(2..9)));
    assert!(!indexed_ranges_overlap(&index.after, &(2..10)));
}

fn line_text(line: &DisplayLine) -> String {
    line.spans.iter().map(|span| span.text.as_str()).collect()
}

fn current_line(row: &DiffRow) -> Option<&DisplayLine> {
    match row {
        DiffRow::Line(line) | DiffRow::Reflow(line) | DiffRow::Moved { after: line, .. } => {
            Some(line)
        }
        DiffRow::LineChange { after, .. } => after.as_ref(),
        DiffRow::LineEnding { .. }
        | DiffRow::Wordwise(_)
        | DiffRow::Elision(_)
        | DiffRow::FileBoundary => None,
    }
}

/// Current-world coverage represented by one planned row; old-only ghosts have none.
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

/// Planned rows remain ordered and singly owned inside each visual hunk.
///
/// Ownership is deliberately hunk-local: an ancestor breadcrumb may frame two distant hunks.
fn assert_source_space_invariants(path: &str, hunks: &[Hunk]) {
    for (hunk_index, hunk) in hunks.iter().enumerate() {
        let mut before = HashSet::new();
        let mut after = HashSet::new();
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

            let (before_line, after_line) = match row {
                DiffRow::Line(line) | DiffRow::Reflow(line) => (None, Some(line.number)),
                DiffRow::LineChange { before, after } => (
                    before.as_ref().map(|line| line.number),
                    after.as_ref().map(|line| line.number),
                ),
                DiffRow::Moved { before, after } => (*before, Some(after.number)),
                DiffRow::Wordwise(word) => (word.before_line, word.after_line),
                DiffRow::LineEnding { .. } | DiffRow::Elision(_) | DiffRow::FileBoundary => {
                    continue;
                }
            };
            if let Some(line) = before_line {
                assert!(
                    before.insert(line),
                    "{path} hunk {hunk_index} gives before line {line} multiple display owners: \
                     {hunk:#?}",
                );
            }
            if let Some(line) = after_line {
                assert!(
                    after.insert(line),
                    "{path} hunk {hunk_index} gives current line {line} multiple display owners: \
                     {hunk:#?}",
                );
            }
        }
    }
}

#[test]
fn line_cst_keeps_context_and_the_terminal_boundary() {
    let hunks = planned("notes.txt", "one\nold\nthree\n", "one\nnew\nthree\n");

    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0].coverage.before, Some(1..4));
    assert_eq!(hunks[0].coverage.after, Some(1..4));
    assert!(matches!(hunks[0].rows.last(), Some(DiffRow::FileBoundary)));
    assert!(hunks[0].rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: Some(before),
                after: Some(after),
            } if line_text(before) == "old" && line_text(after) == "new"
        )
    }));
}

#[test]
fn retained_html_child_uses_after_indentation() {
    let before = "<article>\n  <img\n  src=\"ada.webp\"\n  />\n</article>\n";
    let after =
        "<article>\n  <div>\n    <img\n      src=\"ada.webp\"\n    />\n  </div>\n</article>\n";
    let hunks = planned("view.html", before, after);
    let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();

    assert!(rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::Reflow(line) if line_text(line) == "      src=\"ada.webp\""
        )
    }));
    assert!(!rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::LineChange { before, after }
                    if before.iter().chain(after).any(|line| line_text(line).contains("src=\"ada.webp\""))
            )
        }));
}

#[test]
fn display_syntax_does_not_change_structural_anchor_admission() {
    let before = "<article>\n  <img\n  src=\"ada.webp\"\n  />\n</article>\n";
    let after =
        "<article>\n  <div>\n    <img\n      src=\"ada.webp\"\n    />\n  </div>\n</article>\n";
    let pair = project_pair(Path::new("view.html"), before, after, false).unwrap();
    let correspondence = correspond(&pair);
    let baseline_facts = AnchorFacts::new(&pair);
    let baseline = plan_hunks(&pair, &correspondence);

    let mut recolored = pair.clone();
    for projection in [&mut recolored.before, &mut recolored.after] {
        for node in &mut projection.nodes {
            if let Some(leaf) = &mut node.leaf {
                leaf.syntax = SyntaxClass::Punctuation;
            }
        }
    }
    let recolored_facts = AnchorFacts::new(&recolored);
    assert_eq!(baseline_facts.before, recolored_facts.before);
    assert_eq!(baseline_facts.after, recolored_facts.after);

    let correspondence = correspond(&recolored);
    let recolored = plan_hunks(&recolored, &correspondence);
    let reflows = |hunks: &[Hunk]| {
        hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .filter_map(|row| match row {
                DiffRow::Reflow(line) => Some((line.number, line_text(line))),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let baseline = reflows(&baseline);
    assert!(
        baseline
            .iter()
            .any(|(_, text)| text == "      src=\"ada.webp\""),
        "fixture must exercise a retained structural anchor: {baseline:#?}",
    );
    assert_eq!(baseline, reflows(&recolored));
}

#[test]
fn opaque_plaintext_body_stays_literal() {
    let before = "<plaintext>\n</plaintext>\n  <img>\n";
    let after = "<plaintext>\n</plaintext>\n  <div>\n    <img>\n  </div>\n";
    let hunks = planned("view.html", before, after);
    let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();

    assert!(
        rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::LineChange { before: Some(line), .. }
                    if line_text(line).contains("<img>")
            )
        }),
        "{rows:#?}"
    );
    assert!(
        rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::LineChange { after: Some(line), .. }
                    if line_text(line).contains("<img>")
            )
        }),
        "{rows:#?}"
    );
}

#[test]
fn replacement_group_uses_current_position_and_deletion_uses_its_gap() {
    let graph = Correspondence {
        units: Vec::new(),
        line_ending_edits: Vec::new(),
        line_fallbacks: Vec::new(),
        line_links: vec![crate::diff::correspondence::LineLink {
            before: 91,
            after: 91,
        }],
        leaf_links: Vec::new(),
        before_leaf: Vec::new(),
        after_leaf: Vec::new(),
        composites: Vec::new(),
    };
    let alignment = LineAlignment::new(&graph, 104);
    let line = |number| DisplayLine {
        number,
        spans: Vec::new(),
    };
    let replacement = order_replacement_group(vec![
        DiffRow::LineChange {
            before: Some(line(94)),
            after: Some(line(100)),
        },
        DiffRow::LineChange {
            before: Some(line(95)),
            after: Some(line(101)),
        },
    ]);
    let replacement_current = replacement
        .iter()
        .filter_map(row_after_source_line)
        .collect::<Vec<_>>();
    let current = vec![DiffRow::Line(line(97))];
    let deletion = vec![DiffRow::LineChange {
        before: Some(line(94)),
        after: None,
    }];

    assert_eq!(replacement_current, [100, 101]);
    assert_eq!(
        group_source_order(&replacement, &alignment),
        SourceOrder::current(100)
    );
    assert!(
        group_source_order(&current, &alignment) < group_source_order(&replacement, &alignment)
    );
    assert_eq!(
        group_source_order(&deletion, &alignment),
        alignment.before_order(94)
    );
}

#[test]
fn deletion_gaps_are_stable_before_first_between_anchors_at_eof_and_when_empty() {
    let graph = Correspondence {
        units: Vec::new(),
        line_ending_edits: Vec::new(),
        line_fallbacks: Vec::new(),
        line_links: vec![
            crate::diff::correspondence::LineLink {
                before: 1,
                after: 0,
            },
            crate::diff::correspondence::LineLink {
                before: 3,
                after: 1,
            },
        ],
        leaf_links: Vec::new(),
        before_leaf: Vec::new(),
        after_leaf: Vec::new(),
        composites: Vec::new(),
    };
    let alignment = LineAlignment::new(&graph, 2);

    assert_eq!(alignment.before_order(1).after_gap(), AfterGap::new(0));
    assert_eq!(alignment.before_order(3).after_gap(), AfterGap::new(1));
    assert_eq!(alignment.before_order(5).after_gap(), AfterGap::new(2));
    assert!(alignment.before_order(3) < SourceOrder::current(2));

    let empty = Correspondence {
        units: Vec::new(),
        line_ending_edits: Vec::new(),
        line_fallbacks: Vec::new(),
        line_links: Vec::new(),
        leaf_links: Vec::new(),
        before_leaf: Vec::new(),
        after_leaf: Vec::new(),
        composites: Vec::new(),
    };
    let empty = LineAlignment::new(&empty, 0);

    assert_eq!(empty.before_order(1).after_gap(), AfterGap::BEFORE_FIRST);
    assert_eq!(empty.current_anchor(1), None);
}

#[test]
fn modified_multiline_unit_moved_down_stays_in_current_source_order() {
    let before = concat!(
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
    let after = concat!(
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
    let hunks = planned("lib.rs", before, after);

    assert_eq!(hunks.len(), 1, "{hunks:#?}");
    let current = hunks[0]
        .rows
        .iter()
        .filter_map(row_after_source_line)
        .collect::<Vec<_>>();
    assert_eq!(current, (1..=10).collect::<Vec<_>>(), "{hunks:#?}");
    assert!(current.windows(2).all(|lines| lines[0] <= lines[1]));
}

#[test]
fn changed_comment_is_a_physical_line_edit() {
    let hunks = planned(
        "lib.rs",
        "fn run() {\n    // old reason\n    work();\n}\n",
        "fn run() {\n    // new reason\n    work();\n}\n",
    );
    let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();

    assert!(
        rows.iter()
            .any(|row| matches!(row, DiffRow::LineChange { .. }))
    );
    assert!(!rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::Line(line) | DiffRow::Reflow(line) if line_text(line).contains("reason")
        )
    }));
}

#[test]
fn comment_edit_does_not_hide_independent_reflow() {
    let before = "fn run() { // old reason\n    work(); }\n";
    let after = "fn run() {\n    // new reason\n    work();\n}\n";
    let hunks = planned("lib.rs", before, after);
    let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();

    assert!(
        rows.iter()
            .any(|row| matches!(row, DiffRow::LineChange { .. }))
    );
    assert!(rows.iter().any(|row| { matches!(row, DiffRow::Reflow(_)) }));
}

#[test]
fn multiple_changed_comments_on_one_line_render_one_line_change() {
    let before = "fn run() { /* first */ /* second */ work(); }\n";
    let after = "fn run() { /* changed */ /* revised */ work(); }\n";
    let hunks = planned("lib.rs", before, after);
    let line_changes = hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .filter(|row| matches!(row, DiffRow::LineChange { .. }))
        .count();

    assert_eq!(line_changes, 1);
}

#[test]
fn changed_top_level_comment_pairs_both_source_sides() {
    let hunks = planned(
        "lib.rs",
        "// old license\nfn run() {}\n",
        "// new license\nfn run() {}\n",
    );

    assert!(hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: Some(before),
                after: Some(after),
            } if line_text(before).contains("old") && line_text(after).contains("new")
        )
    }));
}

#[test]
fn exact_reordered_unit_is_present_world_move() {
    let before = "fn first() {\n    first();\n}\n\nfn second() {\n    second();\n}\n";
    let after = "fn second() {\n    second();\n}\n\nfn first() {\n    first();\n}\n";
    let hunks = planned("lib.rs", before, after);

    assert!(
        hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .any(|row| matches!(row, DiffRow::Moved { .. })),
        "{hunks:#?}"
    );
}

#[test]
fn reordered_subtree_inside_unit_is_not_rendered_as_all_context() {
    let before = "fn run() {\n    first();\n    second();\n}\n";
    let after = "fn run() {\n    second();\n    first();\n}\n";
    let hunks = planned("lib.rs", before, after);

    assert!(
        hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .filter_map(current_line)
            .flat_map(|line| &line.spans)
            .any(|span| span.mark == DiffMark::Added
                && matches!(span.text.as_str(), "first" | "second"))
    );
}

#[test]
fn exact_leaf_reparented_inside_unit_remains_visible() {
    let before = "fn run() { left(alpha); right(beta); }\n";
    let after = "fn run() { left(beta); right(alpha); }\n";
    let hunks = planned("lib.rs", before, after);

    assert!(hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
        let DiffRow::Line(line) = row else {
            return false;
        };
        line.spans
            .iter()
            .any(|span| span.text == "alpha" && span.mark == DiffMark::Added)
    }));
}

#[test]
fn wiring_units_share_affixes() {
    let hunks = planned("lib.rs", "use crate::old_name;\n", "use crate::new_name;\n");
    let word = hunks.iter().flat_map(|hunk| &hunk.rows).find_map(|row| {
        let DiffRow::Wordwise(word) = row else {
            return None;
        };
        Some(word)
    });

    let word = word.expect("wiring edit");
    assert_eq!(word.prefix, "use crate::");
    assert_eq!(word.removed, "old");
    assert_eq!(word.added, "new");
    assert_eq!(word.suffix, "_name;");
}

#[test]
fn mixed_module_modes_render_linewise_in_both_directions() {
    let inline = "mod subject { pub fn payload() {} }\n";
    let bodyless = "mod subject;\n";

    for (before, after) in [(inline, bodyless), (bodyless, inline)] {
        let hunks = planned("lib.rs", before, after);
        let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();

        assert!(
            !rows.iter().any(|row| matches!(row, DiffRow::Wordwise(_))),
            "{before:?} -> {after:?}: {hunks:#?}"
        );
        assert!(rows.iter().any(|row| {
            matches!(
                row,
                DiffRow::LineChange {
                    before: Some(before_line),
                    after: Some(after_line),
                } if line_text(before_line) == before.trim_end()
                    && line_text(after_line) == after.trim_end()
            )
        }));
    }
}

#[test]
fn removed_syntax_is_never_hidden_in_a_current_only_row() {
    for (path, before, after, removed) in [
        (
            "lib.rs",
            "fn run() { let mut value = 1; }\n",
            "fn run() { let value = 1; }\n",
            "mut",
        ),
        (
            "view.ts",
            "function run() { old(); }\n",
            "function run() {}\n",
            "old",
        ),
        (
            "view.css",
            ".card { margin: 1px 2px; }\n",
            ".card { margin: 1px; }\n",
            "2",
        ),
    ] {
        let hunks = planned(path, before, after);
        assert!(
            hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
                let DiffRow::LineChange {
                    before: Some(line), ..
                } = row
                else {
                    return false;
                };
                line_text(line).contains(removed)
                    && line
                        .spans
                        .iter()
                        .any(|span| span.mark == DiffMark::Removed && span.text.contains(removed))
            }),
            "{path}: {hunks:#?}"
        );
    }
}

#[test]
fn overlapping_syntax_units_review_one_physical_row_locally() {
    let diff = crate::diff::diff_file(
        "lib.rs",
        "fn alpha() {} fn beta() {}\n",
        "fn alpha() { x(); } fn beta() { y(); }\n",
    )
    .expect("overlapping syntax units must review locally");
    let current = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .filter_map(|row| match row {
            DiffRow::Line(line) | DiffRow::Reflow(line) => Some(line),
            DiffRow::LineChange { after, .. } => after.as_ref(),
            _ => None,
        })
        .filter(|line| line.number == 1)
        .collect::<Vec<_>>();

    assert_eq!(current.len(), 1, "{:#?}", diff.hunks);
    assert!(line_text(current[0]).contains("x();"));
    assert!(line_text(current[0]).contains("y();"));
}

#[test]
fn file_boundary_is_unique_and_globally_terminal() {
    let hunks = planned(
        "lib.rs",
        "use crate::old;\n\nfn end() { old(); }\n",
        "use crate::new;\n\nfn end() { new(); }\n",
    );
    let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();

    assert_eq!(
        rows.iter()
            .filter(|row| matches!(row, DiffRow::FileBoundary))
            .count(),
        1
    );
    assert!(matches!(rows.last(), Some(DiffRow::FileBoundary)));
    assert!(matches!(
        hunks.last().and_then(|hunk| hunk.rows.first()),
        Some(DiffRow::Wordwise(_))
    ));
}

#[test]
fn adjacent_structural_hunks_share_one_context_halo() {
    let hunks = planned(
        "lib.rs",
        "fn first() { old(); }\n\nfn second() { old(); }\n",
        "fn first() { new(); }\n\nfn second() { new(); }\n",
    );
    let shared_blank = hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .filter(|row| {
            matches!(
                row,
                DiffRow::Line(line) if !line.has_changes() && line.number == 2
            )
        })
        .count();

    assert_eq!(hunks.len(), 1, "adjacent context halos must coalesce");
    assert_eq!(shared_blank, 1, "{hunks:#?}");
}

#[test]
fn payload_signal_keeps_ancestor_headers_and_local_context() {
    let filler = (0..12)
        .map(|index| format!("        stable_{index}();\n"))
        .collect::<String>();
    let before = format!(
        concat!(
            "pub async fn attempt_turn_on_stream() -> Result<()> {{\n",
            "    prepare();\n",
            "    loop {{\n",
            "        outer_setup();\n",
            "{filler}",
            "        loop {{\n",
            "            settle();\n",
            "            let frame = read().await?;\n",
            "            match frame {{\n",
            "                Frame::Log(line) => show(line),\n",
            "                Frame::ToolCall => handle(),\n",
            "                Frame::Stop => break,\n",
            "                Frame::Request {{ .. }} => {{}}\n",
            "            }}\n",
            "            finish();\n",
            "        }}\n",
            "    }}\n",
            "}}\n",
        ),
        filler = filler,
    );
    let after = before.replace(
        "                Frame::Stop => break,\n",
        concat!(
            "                Frame::Stop => break,\n",
            "                Frame::Error(message) => return Err(eyre!(message)),\n",
        ),
    );

    let hunks = planned("turn.rs", &before, &after);
    let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();
    for context in [
        "pub async fn attempt_turn_on_stream",
        "match frame {",
        "Frame::Log",
        "Frame::ToolCall",
        "Frame::Stop",
        "Frame::Request",
        "finish();",
    ] {
        assert!(
            rows.iter().any(|row| {
                matches!(
                    row,
                    DiffRow::Line(line) | DiffRow::Reflow(line)
                        if line_text(line).contains(context)
                )
            }),
            "missing {context:?}: {hunks:#?}",
        );
    }
    assert_eq!(
        rows.iter()
            .filter(|row| {
                matches!(
                    row,
                    DiffRow::Line(line) | DiffRow::Reflow(line)
                        if line_text(line).trim() == "loop {"
                )
            })
            .count(),
        2,
        "both loop ancestors must be visible: {hunks:#?}",
    );
    assert!(rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: None,
                after: Some(line),
            } if line_text(line).contains("Frame::Error")
        )
    }));
    assert!(rows.iter().any(|row| matches!(row, DiffRow::Elision(_))));
    assert!(!rows.iter().any(|row| {
            matches!(row, DiffRow::Line(line) | DiffRow::Reflow(line) if line_text(line).contains("stable_5"))
        }));

    let position = |number| {
        rows.iter()
            .position(|row| current_line(row).is_some_and(|line| line.number == number))
            .unwrap_or_else(|| panic!("missing current line {number}: {hunks:#?}"))
    };
    let hierarchy = [1, 3, 17, 20].map(position);
    assert!(
        hierarchy.windows(2).all(|pair| pair[0] < pair[1]),
        "hierarchy breadcrumbs must remain source ordered: {hunks:#?}",
    );
    for halo in [(1..=6).collect::<Vec<_>>(), (14..=27).collect::<Vec<_>>()] {
        let halo = halo.into_iter().map(position).collect::<Vec<_>>();
        assert!(
            halo.windows(2).all(|pair| pair[0] + 1 == pair[1]),
            "each hierarchy step needs its own contiguous context halo: {hunks:#?}",
        );
    }
    let local = [21, 22, 23, 24, 25, 26, 27].map(position);
    assert_eq!(hierarchy[3] + 1, local[0], "{hunks:#?}");
    assert!(
        local.windows(2).all(|pair| pair[0] + 1 == pair[1]),
        "three rows on either side of the signal must stay contiguous: {hunks:#?}",
    );
}

#[test]
fn distant_payload_windows_repeat_their_callable_hierarchy() {
    let review = |context| {
        let stable = (0..context)
            .map(|index| format!("    stable_{index}();\n"))
            .collect::<String>();
        let before = format!(
            concat!(
                "fn run() {{\n",
                "    first(old_alpha);\n",
                "{stable}",
                "    second(old_beta);\n",
                "}}\n",
            ),
            stable = stable,
        );
        let after = before
            .replace("old_alpha", "new_alpha")
            .replace("old_beta", "new_beta");
        planned("lib.rs", &before, &after)
    };

    assert_eq!(review(7).len(), 1, "touching payload halos must coalesce");
    assert_eq!(review(8).len(), 2, "distant payload halos must split");
    let hunks = review(18);

    assert_eq!(hunks.len(), 2, "{hunks:#?}");
    for (index, (present, absent)) in [("new_alpha", "new_beta"), ("new_beta", "new_alpha")]
        .into_iter()
        .enumerate()
    {
        assert!(hunks[index].rows.iter().any(|row| {
            current_line(row).is_some_and(|line| line_text(line).contains("fn run()"))
        }));
        assert!(hunks[index].rows.iter().any(|row| {
            current_line(row).is_some_and(|line| line_text(line).contains(present))
        }));
        assert!(
            !hunks[index].rows.iter().any(|row| {
                current_line(row).is_some_and(|line| line_text(line).contains(absent))
            })
        );
    }
}

#[test]
fn multiline_replacement_renders_each_revision_as_one_run() {
    let hunks = planned(
        "notes.txt",
        "header\nthis\nwent\naway\ntail\n",
        "header\nand then\nthis came in\nno meatgrinder\ntail\n",
    );
    let sides = hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .filter_map(|row| match row {
            DiffRow::LineChange {
                before: Some(_),
                after: None,
            } => Some('-'),
            DiffRow::LineChange {
                before: None,
                after: Some(_),
            } => Some('+'),
            DiffRow::LineChange {
                before: Some(_),
                after: Some(_),
            } => Some('±'),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(sides, ['-', '-', '-', '+', '+', '+']);
}

#[test]
fn unrelated_delimiter_cannot_split_a_structural_replacement() {
    let before = concat!(
        "fn run() {\n",
        "    let stdin = read();\n",
        "    let mut history = make_history(stdin);\n",
        "\n",
        "    // Build prompt from the arguments.\n",
        "    // Collect positional arguments.\n",
        "    let prompt = {\n",
        "        let mut args = std::env::args();\n",
        "        let _ = args.next();\n",
        "        args.collect::<Vec<_>>().join(\" \")\n",
        "    };\n",
        "    connect();\n",
        "}\n",
    );
    let after = concat!(
        "fn run() {\n",
        "    let stdin = read();\n",
        "    let system = match backend {\n",
        "        Backend::Old => OLD,\n",
        "        Backend::New => NEW,\n",
        "    };\n",
        "    let mut history = make_history(stdin, system);\n",
        "    connect();\n",
        "}\n",
    );

    let hunks = planned("run.rs", before, after);
    let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();
    let delimiter_sides = rows
        .iter()
        .filter_map(|row| match row {
            DiffRow::LineChange {
                before: Some(line),
                after: None,
            } if line_text(line).trim() == "};" => Some('-'),
            DiffRow::LineChange {
                before: None,
                after: Some(line),
            } if line_text(line).trim() == "};" => Some('+'),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(delimiter_sides, ['-', '+'], "{hunks:#?}");

    let replacement = rows
        .iter()
        .filter_map(|row| match row {
            DiffRow::LineChange {
                before: Some(line),
                after: None,
            } => Some(('-', line_text(line).trim().to_string())),
            DiffRow::LineChange {
                before: None,
                after: Some(line),
            } => Some(('+', line_text(line).trim().to_string())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        replacement,
        [
            ('-', "let mut history = make_history(stdin);".to_string()),
            ('-', "".to_string()),
            ('-', "// Build prompt from the arguments.".to_string()),
            ('-', "// Collect positional arguments.".to_string()),
            ('-', "let prompt = {".to_string()),
            ('-', "let mut args = std::env::args();".to_string()),
            ('-', "let _ = args.next();".to_string()),
            ('-', "args.collect::<Vec<_>>().join(\" \")".to_string()),
            ('-', "};".to_string()),
            ('+', "let system = match backend {".to_string()),
            ('+', "Backend::Old => OLD,".to_string()),
            ('+', "Backend::New => NEW,".to_string()),
            ('+', "};".to_string()),
            (
                '+',
                "let mut history = make_history(stdin, system);".to_string()
            ),
        ]
    );

    let mut current_started = false;
    for row in rows {
        let DiffRow::LineChange { before, after } = row else {
            continue;
        };
        if after.is_some() {
            current_started = true;
        }
        assert!(
            !current_started || before.is_none(),
            "before rows must not resume after current rows: {hunks:#?}",
        );
    }
}

#[test]
fn changed_chain_limits_replacement_to_linked_parent_context() {
    let before = concat!(
        "fn annotate(node: Node) {\n",
        "    if node.kind() == \"use_declaration\" {\n",
        "        let identity = node\n",
        "            .child_by_field_name(\"argument\")\n",
        "            .map(|argument| argument.byte_range());\n",
        "        consume(identity);\n",
        "    }\n",
        "}\n",
    );
    let after = concat!(
        "fn annotate(node: Node) {\n",
        "    if node.kind() == \"use_declaration\" || is_bodyless_module(node) {\n",
        "        let identity = node\n",
        "            .child_by_field_name(\"argument\")\n",
        "            .or_else(|| node.child_by_field_name(\"name\"))\n",
        "            .map(|identity| identity.byte_range());\n",
        "        consume(identity);\n",
        "    }\n",
        "}\n",
    );
    let hunks = planned("rust.rs", before, after);
    assert!(hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
        matches!(
            row,
            DiffRow::Line(line)
                if line.number == 2
                    && line.has_changes()
                    && line_text(line).contains("is_bodyless_module")
        )
    }));
    let replacement = hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .filter_map(|row| match row {
            DiffRow::LineChange {
                before: Some(line),
                after: None,
            } => Some(('-', line.number, line_text(line).trim().to_owned())),
            DiffRow::LineChange {
                before: None,
                after: Some(line),
            } => Some(('+', line.number, line_text(line).trim().to_owned())),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        replacement,
        [
            ('-', 4, ".child_by_field_name(\"argument\")".to_owned()),
            ('-', 5, ".map(|argument| argument.byte_range());".to_owned(),),
            ('+', 4, ".child_by_field_name(\"argument\")".to_owned()),
            (
                '+',
                5,
                ".or_else(|| node.child_by_field_name(\"name\"))".to_owned(),
            ),
            ('+', 6, ".map(|identity| identity.byte_range());".to_owned(),),
        ],
        "{hunks:#?}",
    );
}

#[test]
fn changed_loop_pattern_does_not_pair_with_an_inserted_parameter() {
    let before = concat!(
        "fn contextual_links(\n",
        "    pair: &ProjectionPair<'_, '_>,\n",
        "    before_unit: NodeId,\n",
        "    after_unit: NodeId,\n",
        "    before_fingerprints: &[NodeFingerprints],\n",
        "    after_fingerprints: &[NodeFingerprints],\n",
        ") -> ContextLinks {\n",
        "    let mut pending = VecDeque::from([(before_unit, after_unit)]);\n",
        "    while let Some((before_parent, after_parent)) = pending.pop_front() {\n",
        "        let pairs = contextual_child_matches(pair, before_parent, after_parent);\n",
        "        for edge in pairs {\n",
        "            link_context(edge);\n",
        "        }\n",
        "    }\n",
        "}\n",
    );
    let after = concat!(
        "fn contextual_links(\n",
        "    pair: &ProjectionPair<'_, '_>,\n",
        "    before_unit: NodeId,\n",
        "    after_unit: NodeId,\n",
        "    unit_placement: Placement,\n",
        "    before_fingerprints: &[NodeFingerprints],\n",
        "    after_fingerprints: &[NodeFingerprints],\n",
        ") -> ContextLinks {\n",
        "    let mut pending = VecDeque::from([(before_unit, after_unit)]);\n",
        "    while let Some((before_parent, after_parent)) = pending.pop_front() {\n",
        "        let pairs = contextual_child_matches(pair, before_parent, after_parent);\n",
        "        let placements = contextual_match_placements(&pairs);\n",
        "        for (edge, placement) in pairs.into_iter().zip(placements) {\n",
        "            link_context(edge, placement);\n",
        "        }\n",
        "    }\n",
        "}\n",
    );
    let hunks = planned("correspondence.rs", before, after);
    let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();
    let old_loop = rows
        .iter()
        .position(|row| {
            matches!(
                row,
                DiffRow::LineChange {
                    before: Some(line),
                    ..
                } if line_text(line).contains("for edge in pairs")
            )
        })
        .expect("old loop header remains visible");
    let parameter = rows
        .iter()
        .position(|row| {
            current_line(row)
                .is_some_and(|line| line_text(line).contains("unit_placement: Placement"))
        })
        .expect("inserted parameter remains visible");

    assert!(parameter < old_loop, "{hunks:#?}");
    assert!(!rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: Some(before),
                after: Some(after),
            } if line_text(before).contains("for edge in pairs")
                && line_text(after).contains("unit_placement: Placement")
        )
    }));
}

#[test]
fn reordered_links_cannot_veto_stable_display_anchors() {
    let before = "fn run() {\n    alpha();\n    beta();\n}\n";
    let after = "fn run() {\n    beta();\n    alpha();\n}\n";
    let pair = project_pair(Path::new("run.rs"), before, after, false).unwrap();
    let node_on_line = |projection: &Projection<'_>, line: usize| {
        projection
            .nodes
            .iter()
            .position(|node| node.kind == "expression_statement" && node.lines.start == line)
            .map(NodeId::new)
            .expect("statement on source line")
    };
    let links = [
        NodeLink {
            before: node_on_line(&pair.before, 2),
            after: node_on_line(&pair.after, 3),
            placement: Placement::Stable,
            reparented: false,
        },
        NodeLink {
            before: node_on_line(&pair.before, 3),
            after: node_on_line(&pair.after, 2),
            placement: Placement::Reordered,
            reparented: false,
        },
    ];
    let facts = AnchorFacts::new(&pair);

    let retained = retained_regions(&pair, &facts, &links, &(0..4), &(0..4));

    assert!(matches!(
        retained.as_slice(),
        [RetainedRegion {
            before,
            after,
            retention: Retention::Exact,
        }] if *before == (1..2) && *after == (2..3)
    ));
}

#[test]
fn nearby_soft_continuation_repeats_inside_the_revision_runs() {
    let before = concat!(
        "fn chain() {\n",
        "    let value = source\n",
        "        .old_first()\n",
        "        .shared()\n",
        "        .old_last();\n",
        "}\n",
    );
    let after = before
        .replace("old_first", "new_first")
        .replace("old_last", "new_last");
    let hunks = planned("chain.rs", before, &after);
    let shared = hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .filter_map(|row| match row {
            DiffRow::LineChange {
                before: Some(line),
                after: None,
            } if line_text(line).contains(".shared()") => Some('-'),
            DiffRow::LineChange {
                before: None,
                after: Some(line),
            } if line_text(line).contains(".shared()") => Some('+'),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(shared, ['-', '+'], "{hunks:#?}");
    assert!(!hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
        matches!(row, DiffRow::Line(line) | DiffRow::Reflow(line) if line_text(line).contains(".shared()"))
    }));
}

#[test]
fn distant_soft_continuations_remain_bounded_context() {
    let shared = (0..24)
        .map(|index| format!("        .shared_{index}()\n"))
        .collect::<String>();
    let before = format!(
        concat!(
            "fn chain() {{\n",
            "    let value = source\n",
            "        .old_first()\n",
            "{shared}",
            "        .old_last();\n",
            "}}\n",
        ),
        shared = shared,
    );
    let after = before
        .replace("old_first", "new_first")
        .replace("old_last", "new_last");
    let hunks = planned("chain.rs", &before, &after);
    let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();
    let displayed_shared = rows
        .iter()
        .filter(|row| current_line(row).is_some_and(|line| line_text(line).contains(".shared_")))
        .count();

    assert!(hunks.len() >= 2, "{hunks:#?}");
    assert!(rows.iter().any(|row| matches!(row, DiffRow::Elision(_))));
    assert!(displayed_shared < 24, "{hunks:#?}");
    assert!(!rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::LineChange { before, after }
                if before
                    .iter()
                    .chain(after)
                    .any(|line| line_text(line).contains(".shared_"))
        )
    }));
}

#[test]
fn crossed_exact_children_repeat_instead_of_anchoring_an_outer_deletion() {
    let before = concat!(
        "enum ReviewTreatment {\n",
        "    Inline,\n",
        "    Linewise,\n",
        "    Compact,\n",
        "}\n",
        "\n",
        "enum Movement {\n",
        "    Track,\n",
        "    Ignore,\n",
        "}\n",
        "\n",
        "fn stable_tail() {}\n",
    );
    let after = concat!(
        "enum ReviewMode {\n",
        "    Structural,\n",
        "    Compact,\n",
        "    Linewise,\n",
        "}\n",
        "\n",
        "fn stable_tail() {}\n",
    );
    let hunks = planned("projection.rs", before, after);
    let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();
    let position = |needle: &str, mark: DiffMark| {
        rows.iter().position(|row| match row {
            DiffRow::LineChange {
                before: Some(line),
                after: None,
            } if mark == DiffMark::Removed => line_text(line).contains(needle),
            DiffRow::LineChange {
                before: None,
                after: Some(line),
            } if mark == DiffMark::Added => line_text(line).contains(needle),
            _ => false,
        })
    };

    let old_review = position("enum ReviewTreatment", DiffMark::Removed).expect("old enum");
    let old_compact = position("Compact,", DiffMark::Removed).expect("old Compact variant");
    let new_review = position("enum ReviewMode", DiffMark::Added).expect("new enum");
    let new_linewise = position("Linewise,", DiffMark::Added).expect("new Linewise variant");
    let current_close = rows
        .iter()
        .position(|row| {
            matches!(
                row,
                DiffRow::LineChange {
                    before: None,
                    after: Some(line),
                } if line.number == 5 && line_text(line) == "}"
            )
        })
        .expect("current enum closing brace");
    let movement = position("enum Movement", DiffMark::Removed).expect("removed Movement enum");

    let replacement = rows
        .iter()
        .filter_map(|row| match row {
            DiffRow::LineChange {
                before: Some(line),
                after: None,
            } if !line_text(line).trim().is_empty() => {
                Some(('-', line_text(line).trim().to_owned()))
            }
            DiffRow::LineChange {
                before: None,
                after: Some(line),
            } if !line_text(line).trim().is_empty() => {
                Some(('+', line_text(line).trim().to_owned()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        replacement,
        [
            ('-', "enum ReviewTreatment {".to_owned()),
            ('-', "Inline,".to_owned()),
            ('-', "Linewise,".to_owned()),
            ('-', "Compact,".to_owned()),
            ('-', "}".to_owned()),
            ('+', "enum ReviewMode {".to_owned()),
            ('+', "Structural,".to_owned()),
            ('+', "Compact,".to_owned()),
            ('+', "Linewise,".to_owned()),
            ('+', "}".to_owned()),
            ('-', "enum Movement {".to_owned()),
            ('-', "Track,".to_owned()),
            ('-', "Ignore,".to_owned()),
            ('-', "}".to_owned()),
        ],
        "{hunks:#?}",
    );

    assert!(old_review < old_compact && old_compact < new_review);
    assert!(new_review < new_linewise && new_linewise < current_close);
    assert!(current_close < movement, "{hunks:#?}");
    for variant in ["Compact,", "Linewise,"] {
        assert_eq!(
            rows.iter()
                .filter(|row| {
                    matches!(
                        row,
                        DiffRow::LineChange {
                            before: Some(line),
                            after: None,
                        } if line_text(line).contains(variant)
                    )
                })
                .count(),
            1,
            "{variant} must occur once in the old run: {hunks:#?}",
        );
        assert_eq!(
            rows.iter()
                .filter(|row| {
                    matches!(
                        row,
                        DiffRow::LineChange {
                            before: None,
                            after: Some(line),
                        } if line_text(line).contains(variant)
                    )
                })
                .count(),
            1,
            "{variant} must occur once in the current run: {hunks:#?}",
        );
        assert!(!rows.iter().any(|row| {
            matches!(row, DiffRow::Line(line) | DiffRow::Reflow(line) if line_text(line).contains(variant))
        }));
    }
}

#[test]
fn replaced_owner_repeats_its_shared_delimiter() {
    let hunks = planned(
        "projection.rs",
        concat!(
            "enum Old {\n",
            "    Gone,\n",
            "    Compact,\n",
            "}\n",
            "fn tail() {}\n",
        ),
        concat!(
            "enum New {\n",
            "    Added,\n",
            "    Compact,\n",
            "}\n",
            "fn tail() {}\n",
        ),
    );
    let replacement = hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .filter_map(|row| match row {
            DiffRow::LineChange {
                before: Some(line),
                after: None,
            } => Some(('-', line_text(line).trim().to_owned())),
            DiffRow::LineChange {
                before: None,
                after: Some(line),
            } => Some(('+', line_text(line).trim().to_owned())),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        replacement,
        [
            ('-', "enum Old {".to_owned()),
            ('-', "Gone,".to_owned()),
            ('-', "Compact,".to_owned()),
            ('-', "}".to_owned()),
            ('+', "enum New {".to_owned()),
            ('+', "Added,".to_owned()),
            ('+', "Compact,".to_owned()),
            ('+', "}".to_owned()),
        ],
        "{hunks:#?}",
    );
}

#[test]
fn documentation_does_not_turn_crossing_variants_into_display_anchors() {
    let before = concat!(
        "/// Planner treatment requested by a semantic review boundary.\n",
        "#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]\n",
        "pub(crate) enum ReviewTreatment {\n",
        "    Inline,\n",
        "    Linewise,\n",
        "    Compact,\n",
        "}\n",
        "\n",
        "/// Whether stable-order analysis may classify a review boundary as moved.\n",
        "#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]\n",
        "pub(crate) enum Movement {\n",
        "    Track,\n",
        "    Ignore,\n",
        "}\n",
    );
    let after = concat!(
        "/// How one independently matched review unit is compared and presented.\n",
        "#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]\n",
        "pub(crate) enum ReviewMode {\n",
        "    /// Structure-aware content eligible for move and reflow detection.\n",
        "    Structural,\n",
        "    /// Low-signal content kept compact when its replacement fits one line.\n",
        "    Compact,\n",
        "    /// Content compared and presented through physical lines.\n",
        "    Linewise,\n",
        "}\n",
    );
    let hunks = planned("projection.rs", before, after);
    assert_eq!(hunks.len(), 1, "adjacent review facts must stay together");
    assert!(
        !hunks[0]
            .rows
            .iter()
            .any(|row| matches!(row, DiffRow::Elision(_)))
    );
    let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();
    assert!(rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: Some(before),
                after: Some(after),
            } if before.number == 1
                && after.number == 1
                && line_text(before).contains("Planner treatment")
                && line_text(after).contains("independently matched review unit")
        )
    }));
    let replacement = rows
        .iter()
        .filter_map(|row| match row {
            DiffRow::LineChange {
                before: Some(line),
                after: None,
            } if !line_text(line).trim().is_empty() => {
                Some(('-', line.number, line_text(line).trim().to_owned()))
            }
            DiffRow::LineChange {
                before: None,
                after: Some(line),
            } if !line_text(line).trim().is_empty() => {
                Some(('+', line.number, line_text(line).trim().to_owned()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        replacement,
        [
            ('-', 3, "pub(crate) enum ReviewTreatment {".to_owned()),
            ('-', 4, "Inline,".to_owned()),
            ('-', 5, "Linewise,".to_owned()),
            ('-', 6, "Compact,".to_owned()),
            ('-', 7, "}".to_owned()),
            ('+', 3, "pub(crate) enum ReviewMode {".to_owned()),
            (
                '+',
                4,
                "/// Structure-aware content eligible for move and reflow detection.".to_owned(),
            ),
            ('+', 5, "Structural,".to_owned()),
            (
                '+',
                6,
                "/// Low-signal content kept compact when its replacement fits one line."
                    .to_owned(),
            ),
            ('+', 7, "Compact,".to_owned()),
            (
                '+',
                8,
                "/// Content compared and presented through physical lines.".to_owned(),
            ),
            ('+', 9, "Linewise,".to_owned()),
            ('+', 10, "}".to_owned()),
            (
                '-',
                9,
                "/// Whether stable-order analysis may classify a review boundary as moved."
                    .to_owned(),
            ),
            (
                '-',
                10,
                "#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]".to_owned(),
            ),
            ('-', 11, "pub(crate) enum Movement {".to_owned()),
            ('-', 12, "Track,".to_owned()),
            ('-', 13, "Ignore,".to_owned()),
            ('-', 14, "}".to_owned()),
        ],
        "{hunks:#?}",
    );
    assert!(rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::Line(line)
                if line.number == 2
                    && line_text(line) == "#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]"
        )
    }));
    for variant in ["Compact,", "Linewise,"] {
        assert!(!rows.iter().any(|row| {
            matches!(row, DiffRow::Line(line) | DiffRow::Reflow(line) if line_text(line).contains(variant))
        }));
    }
}

#[test]
fn stable_attribute_subtrees_cannot_become_structural_checkpoints() {
    let before = concat!(
        "mod tests {\n",
        "    #[test]\n",
        "    fn alpha() { old(); }\n",
        "}\n",
    );
    let after = concat!(
        "mod tests {\n",
        "    #[test]\n",
        "    fn alpha() { new(); }\n",
        "}\n",
    );
    let pair = project_pair(Path::new("lib.rs"), before, after, false)
        .expect("Rust projection must parse");
    let graph = correspond(&pair);
    let attribute = graph
        .composites
        .iter()
        .copied()
        .find(|link| pair.before.node(link.before).kind == "attribute_item")
        .expect("the stable test attribute must remain structurally paired");
    let descendants = pair
        .before
        .descendants(attribute.before)
        .collect::<HashSet<_>>();
    let nested = graph
        .composites
        .iter()
        .copied()
        .find(|link| descendants.contains(&link.before))
        .expect("the attribute must retain a paired nested subtree");
    let before_region = 0..pair.before.source.lines().len();
    let after_region = 0..pair.after.source.lines().len();
    let anchor_facts = AnchorFacts::new(&pair);

    assert!(link_belongs_to_decoration(&pair, attribute));
    assert_eq!(pair.before.node(nested.before).decoration_owner, None);
    assert!(link_belongs_to_decoration(&pair, nested));
    assert!(
        retained_region(
            &pair,
            &anchor_facts,
            attribute,
            &before_region,
            &after_region,
        )
        .is_none()
    );
    assert!(
        retained_regions(
            &pair,
            &anchor_facts,
            &[attribute, nested],
            &before_region,
            &after_region,
        )
        .is_empty()
    );
}

#[test]
fn modified_expression_keeps_both_sides_and_its_owner() {
    let before = concat!(
        "fn make_history() {\n",
        "    let reasoning = reasoning();\n",
        "    let mut history = vec![Message::System(\n",
        "        SYSTEM_PREAMBLE\n",
        "            .replace(\"cutoff\", \"old\")\n",
        "            .replace(\"reasoning\", &reasoning),\n",
        "    )];\n",
        "}\n",
    );
    let after = before.replace("SYSTEM_PREAMBLE", "system_preamble");

    let hunks = planned("history.rs", before, &after);
    assert!(hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: Some(before),
                after: Some(after),
            } if line_text(before).contains("SYSTEM_PREAMBLE")
                && line_text(after).contains("system_preamble")
        )
    }));
    for context in [
        "fn make_history",
        "let mut history = vec!",
        ".replace(\"cutoff\"",
        ".replace(\"reasoning\"",
    ] {
        assert!(
                hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
                    matches!(row, DiffRow::Line(line) | DiffRow::Reflow(line) if line_text(line).contains(context))
                }),
                "missing {context:?}: {hunks:#?}",
            );
    }
}

#[test]
fn unmatched_units_at_one_edit_gap_render_before_then_current() {
    let hunks = planned(
        "lib.rs",
        "use crate::old;\nfn stable() {}\n",
        "const NEW: u8 = 1;\nfn stable() {}\n",
    );
    let changes = hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .filter_map(|row| match row {
            DiffRow::LineChange {
                before: Some(line),
                after: None,
            } => Some(('-', line_text(line))),
            DiffRow::LineChange {
                before: None,
                after: Some(line),
            } => Some(('+', line_text(line))),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(hunks.len(), 1, "{hunks:#?}");
    assert_eq!(
        changes,
        [
            ('-', "use crate::old;".to_string()),
            ('+', "const NEW: u8 = 1;".to_string()),
        ]
    );
}

#[test]
fn one_edit_gap_preserves_old_source_order_before_current() {
    let hunks = planned(
        "lib.rs",
        "use crate::old;\nfn run() {}\n",
        "const KEEP: u8 = 1;\n",
    );
    let changes = hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .filter_map(|row| match row {
            DiffRow::LineChange {
                before: Some(line),
                after: None,
            } => Some(('-', line.number, line_text(line))),
            DiffRow::LineChange {
                before: None,
                after: Some(line),
            } => Some(('+', line.number, line_text(line))),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(hunks.len(), 1, "{hunks:#?}");
    assert_eq!(
        changes,
        [
            ('-', 1, "use crate::old;".to_string()),
            ('-', 2, "fn run() {}".to_string()),
            ('+', 1, "const KEEP: u8 = 1;".to_string()),
        ],
    );
}

#[test]
fn one_sided_wiring_removal_keeps_current_file_context() {
    let hunks = planned(
        "lib.rs",
        concat!(
            "use crate::old;\n",
            "use crate::kept;\n",
            "\n",
            "fn run() {}\n",
            "fn nearby() {}\n",
            "fn far_one() {}\n",
            "fn far_two() {}\n",
        ),
        concat!(
            "use crate::kept;\n",
            "\n",
            "fn run() {}\n",
            "fn nearby() {}\n",
            "fn far_one() {}\n",
            "fn far_two() {}\n",
        ),
    );
    let rows = &hunks[0].rows;
    let removed = rows
        .iter()
        .position(|row| {
            matches!(
                row,
                DiffRow::LineChange {
                    before: Some(line),
                    after: None,
                } if line_text(line).contains("crate::old")
            )
        })
        .expect("removed import");
    let kept = rows
        .iter()
        .position(|row| {
            matches!(
                row,
                DiffRow::Line(line)
                    if !line.has_changes()
                        && line.number == 1
                        && line_text(line).contains("crate::kept")
            )
        })
        .expect("remaining import context");

    assert!(removed < kept, "{hunks:#?}");
    assert!(rows.iter().any(|row| {
        matches!(row, DiffRow::Line(line) if line.number == 2 && line_text(line).is_empty())
    }));
    assert!(rows.iter().any(|row| {
        matches!(row, DiffRow::Line(line) if line.number == 3 && line_text(line).contains("fn run"))
    }));
    assert!(rows.iter().any(|row| {
            matches!(row, DiffRow::Line(line) if line.number == 4 && line_text(line).contains("fn nearby"))
        }));
    assert!(!rows.iter().any(|row| {
        current_line(row).is_some_and(|line| {
            line.number >= 5
                && (line_text(line).contains("far_one") || line_text(line).contains("far_two"))
        })
    }));
}

#[test]
fn exact_stable_statement_remains_context_between_replacements() {
    let hunks = planned(
        "lib.rs",
        concat!(
            "fn run() {\n",
            "    old_before();\n",
            "    stable();\n",
            "    old_after();\n",
            "}\n",
        ),
        concat!(
            "fn run() {\n",
            "    new_before();\n",
            "    stable();\n",
            "    new_after();\n",
            "}\n",
        ),
    );
    let stable = hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .filter(|row| match row {
            DiffRow::Line(line) if !line.has_changes() => line_text(line).contains("stable();"),
            DiffRow::LineChange { before, after } => before
                .as_ref()
                .into_iter()
                .chain(after)
                .any(|line| line_text(line).contains("stable();")),
            _ => false,
        })
        .collect::<Vec<_>>();

    assert!(matches!(stable.as_slice(), [DiffRow::Line(_)]));
}

#[test]
fn stable_delimiter_only_leaf_is_weak_in_a_parsed_cst() {
    let hunks = planned(
        "lib.rs",
        concat!(
            "fn run() {\n",
            "    let value = {\n",
            "        old_one();\n",
            "    };\n",
            "    old_two();\n",
            "}\n",
        ),
        concat!(
            "fn run() {\n",
            "    let value = {\n",
            "        new_one();\n",
            "    };\n",
            "    new_two();\n",
            "}\n",
        ),
    );
    let replacement = hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .filter_map(|row| match row {
            DiffRow::LineChange {
                before: Some(line),
                after: None,
            } => Some(('-', line_text(line).trim().to_string())),
            DiffRow::LineChange {
                before: None,
                after: Some(line),
            } => Some(('+', line_text(line).trim().to_string())),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        replacement,
        [
            ('-', "old_one();".to_string()),
            ('-', "};".to_string()),
            ('-', "old_two();".to_string()),
            ('+', "new_one();".to_string()),
            ('+', "};".to_string()),
            ('+', "new_two();".to_string()),
        ],
        "{hunks:#?}",
    );
}

#[test]
fn multiline_leaf_checkpoint_strength_is_physical_line_local() {
    let source = "/* label\n};\n*/\nfn run() {}\n";
    let pair = project_pair(Path::new("lib.rs"), source, source, false).unwrap();

    assert!(line_link_is_display_checkpoint(&pair, 0, 0));
    assert!(!line_link_is_display_checkpoint(&pair, 1, 1));
    assert!(!line_link_is_display_checkpoint(&pair, 2, 2));
}

#[test]
fn weak_layout_rows_group_locally_but_never_unboundedly() {
    for (path, before, after) in [
        (
            "lib.rs",
            "fn run() {\n    old_one();\n\n    old_two();\n}\n",
            "fn run() {\n    new_one();\n\n    new_two();\n}\n",
        ),
        (
            "notes.txt",
            "old_one();\n\nold_two();\n",
            "new_one();\n\nnew_two();\n",
        ),
    ] {
        let hunks = planned(path, before, after);
        let blank = hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .filter_map(|row| match row {
                DiffRow::LineChange {
                    before: Some(line),
                    after: None,
                } => Some(('-', line_text(line).trim().to_string())),
                DiffRow::LineChange {
                    before: None,
                    after: Some(line),
                } => Some(('+', line_text(line).trim().to_string())),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            blank,
            [
                ('-', "old_one();".to_string()),
                ('-', "".to_string()),
                ('-', "old_two();".to_string()),
                ('+', "new_one();".to_string()),
                ('+', "".to_string()),
                ('+', "new_two();".to_string()),
            ],
            "{path}: {hunks:#?}",
        );
    }

    let delimiter = planned(
        "notes.txt",
        "old_one();\n};\nold_two();\n",
        "new_one();\n};\nnew_two();\n",
    );
    let delimiter = delimiter
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .filter_map(|row| match row {
            DiffRow::LineChange {
                before: Some(line),
                after: None,
            } => Some(('-', line_text(line))),
            DiffRow::LineChange {
                before: None,
                after: Some(line),
            } => Some(('+', line_text(line))),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        delimiter,
        [
            ('-', "old_one();".to_string()),
            ('-', "};".to_string()),
            ('-', "old_two();".to_string()),
            ('+', "new_one();".to_string()),
            ('+', "};".to_string()),
            ('+', "new_two();".to_string()),
        ]
    );

    for weak in ["};\n".repeat(30), "\n".repeat(30)] {
        let before = format!("old_one();\n{weak}old_two();\n");
        let after = format!("new_one();\n{weak}new_two();\n");
        let focused = planned("notes.txt", &before, &after);
        let replacements = focused
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .filter(|row| matches!(row, DiffRow::LineChange { .. }))
            .count();

        assert_eq!(focused.len(), 2, "{focused:#?}");
        assert!(replacements < 30, "weak context expanded without bound");
    }

    for (weak_rows, expected_hunks) in [(7, 1), (8, 2)] {
        let weak = "};\n".repeat(weak_rows);
        let before = format!("old_one();\n{weak}old_two();\n");
        let after = format!("new_one();\n{weak}new_two();\n");
        let focused = planned("notes.txt", &before, &after);
        assert_eq!(focused.len(), expected_hunks, "{focused:#?}");
    }
}

#[test]
fn mixed_move_hunk_still_completes_payload_edit_context() {
    let hunks = planned(
        "lib.rs",
        "fn first() { one(); }\nfn second() { two(); }\n",
        concat!(
            "fn second() { two(); }\n",
            "fn first() { one(); }\n",
            "use crate::new;\n",
        ),
    );
    assert_eq!(hunks.len(), 1, "{hunks:#?}");
    let rows = &hunks[0].rows;
    assert!(rows.iter().any(|row| matches!(
        row,
        DiffRow::Moved {
            before: Some(1),
            after,
        } if after.number == 2
    )));
    assert!(rows.iter().any(|row| matches!(
        row,
        DiffRow::LineChange {
            before: None,
            after: Some(line),
        } if line.number == 3 && line_text(line).contains("crate::new")
    )));
    assert!(
        rows.iter().any(|row| matches!(
            row,
            DiffRow::Line(line)
                if !line.has_changes()
                    && line.number == 1
                    && line_text(line).contains("fn second")
        )),
        "mixed move hunk lost current-file context: {hunks:#?}"
    );
}

#[test]
fn current_bearing_move_and_replacement_groups_follow_current_source() {
    let hunks = planned(
        "lib.rs",
        concat!(
            "fn first() { one(); }\n",
            "fn second() { two(); }\n",
            "fn third() { three(); }\n",
        ),
        concat!(
            "fn third() { changed_three(); }\n",
            "fn second() { two(); }\n",
            "fn first() { one(); }\n",
        ),
    );
    let signals = hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .filter_map(|row| match row {
            DiffRow::Moved { after, .. }
            | DiffRow::LineChange {
                after: Some(after), ..
            } => Some(after.number),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(signals, [1, 2, 3], "{hunks:#?}");
}

#[test]
fn changed_callable_header_is_still_a_distant_body_breadcrumb() {
    let body = (0..12)
        .map(|index| format!("    stable_{index}();\n"))
        .collect::<String>();
    let before = format!("fn run(value: u8) {{\n{body}    old();\n}}\n");
    let after = format!("fn run() {{\n{body}    new();\n}}\n");
    let hunks = planned("lib.rs", &before, &after);
    let body = hunks
        .iter()
        .find(|hunk| {
            hunk.rows.iter().any(|row| {
                matches!(
                    row,
                    DiffRow::LineChange {
                        after: Some(line),
                        ..
                    } if line_text(line).contains("new();")
                )
            })
        })
        .expect("body replacement hunk");

    assert!(body.rows.iter().any(|row| {
        matches!(
            row,
            DiffRow::Line(line)
                if !line.has_changes()
                    && line.number == 1
                    && line_text(line).contains("fn run()")
        )
    }));
    assert!(
        body.rows
            .iter()
            .any(|row| matches!(row, DiffRow::Elision(_)))
    );
    for local in ["stable_9", "stable_10", "stable_11"] {
        assert!(body.rows.iter().any(|row| {
                matches!(row, DiffRow::Line(line) | DiffRow::Reflow(line) if line_text(line).contains(local))
            }));
    }
    for breadcrumb_context in ["stable_0", "stable_1", "stable_2"] {
        assert!(body.rows.iter().any(|row| {
                matches!(row, DiffRow::Line(line) | DiffRow::Reflow(line) if line_text(line).contains(breadcrumb_context))
            }));
    }
    assert!(!body.rows.iter().any(|row| {
            matches!(row, DiffRow::Line(line) | DiffRow::Reflow(line) if line_text(line).contains("stable_3"))
        }));
}

#[test]
fn history_shape_crosses_unit_boundaries_without_losing_source_order() {
    let before = concat!(
        "//! Extensions to handle lists of messages.\n",
        "use crate::prompting::SYSTEM_PREAMBLE;\n",
        "use crate::protocol::Message;\n",
        "\n",
        "/// Compose a full session history from the default preamble\n",
        "/// and optional stdin/extra contexts in the canonical order.\n",
        "pub fn make_history(\n",
        "    stdin_content: Option<String>,\n",
        "    stdout_redirection_path: Option<String>,\n",
        ") -> Vec<Message> {\n",
        "    let now = time::OffsetDateTime::now_local();\n",
        "    let now = now.date().to_string();\n",
        "    let reasoning = std::env::var(\"PLEASE_TRY\")\n",
        "        .ok()\n",
        "        .map(|v| v.trim().to_lowercase())\n",
        "        .and_then(|v| match v.as_str() {\n",
        "            _ if v.starts_with(\"h\") => Some(\"high\".to_string()),\n",
        "            _ if v.starts_with(\"m\") => Some(\"medium\".to_string()),\n",
        "            _ => None,\n",
        "        })\n",
        "        .unwrap_or_else(|| \"medium\".to_string());\n",
        "    let mut history = vec![Message::System(\n",
        "        SYSTEM_PREAMBLE\n",
        "            .replace(\"cutoff\", \"old\")\n",
        "            .replace(\"today\", &now)\n",
        "            .replace(\"reasoning\", &reasoning),\n",
        "    )];\n",
        "}\n",
    );
    let after = before
        .replace("use crate::prompting::SYSTEM_PREAMBLE;\n", "")
        .replace("default preamble", "selected backend preamble")
        .replace(
            "    stdout_redirection_path: Option<String>,\n",
            concat!(
                "    stdout_redirection_path: Option<String>,\n",
                "    system_preamble: &str,\n",
            ),
        )
        .replace("        SYSTEM_PREAMBLE\n", "        system_preamble\n");
    let hunks = planned("history.rs", before, &after);
    assert_eq!(
        hunks.len(),
        2,
        "adjacent top edits coalesce, while the distant use-site repeats its scope"
    );
    let rows = hunks.iter().flat_map(|hunk| &hunk.rows).collect::<Vec<_>>();
    let position = |predicate: &dyn Fn(&DiffRow) -> bool| {
        rows.iter()
            .position(|row| predicate(row))
            .unwrap_or_else(|| panic!("expected history row: {hunks:#?}"))
    };
    let module = position(&|row| matches!(row, DiffRow::Line(line) if line.number == 1));
    let removed_import = position(&|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: Some(line),
                after: None,
            } if line_text(line).contains("SYSTEM_PREAMBLE;")
        )
    });
    let remaining_import = position(
        &|row| matches!(row, DiffRow::Line(line) if line.number == 2 && line_text(line).contains("protocol::Message")),
    );
    let blank = position(
        &|row| matches!(row, DiffRow::Line(line) if line.number == 3 && line_text(line).is_empty()),
    );
    let doc = position(&|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: Some(before),
                after: Some(after),
            } if line_text(before).contains("default preamble")
                && line_text(after).contains("selected backend preamble")
        )
    });
    let continuation = position(
        &|row| matches!(row, DiffRow::Line(line) if line.number == 5 && line_text(line).contains("optional stdin")),
    );
    let definition = position(
        &|row| matches!(row, DiffRow::Line(line) if line.number == 6 && line_text(line).contains("make_history")),
    );
    let stdout = position(
        &|row| matches!(row, DiffRow::Line(line) if line.number == 8 && line_text(line).contains("stdout_redirection_path")),
    );
    let parameter = position(&|row| {
        matches!(
            row,
            DiffRow::LineChange {
                before: None,
                after: Some(line),
            } if line.number == 9 && line_text(line).contains("system_preamble: &str")
        )
    });
    let signature_end = position(
        &|row| matches!(row, DiffRow::Line(line) if line.number == 10 && line_text(line).contains(") -> Vec<Message>")),
    );

    assert!(module < removed_import);
    assert!(removed_import < remaining_import);
    assert!(remaining_import < blank);
    assert!(blank < doc);
    assert!(doc < continuation);
    assert!(continuation < definition);
    assert!(definition < stdout);
    assert_eq!(stdout + 1, parameter, "added parameter must follow stdout");
    assert_eq!(
        parameter + 1,
        signature_end,
        "signature must stay contiguous"
    );
    assert_eq!(
        rows.iter()
            .filter(|row| {
                matches!(
                    row,
                    DiffRow::Line(line)
                        if line_text(line).contains("pub fn make_history")
                )
            })
            .count(),
        2,
        "each distant window needs its callable breadcrumb: {hunks:#?}",
    );

    let local_position = |number| {
        rows.iter()
            .position(|row| current_line(row).is_some_and(|line| line.number == number))
            .unwrap_or_else(|| panic!("missing history line {number}: {hunks:#?}"))
    };
    let local = [20, 21, 22, 23, 24, 25, 26].map(local_position);
    assert!(
        local.windows(2).all(|pair| pair[0] + 1 == pair[1]),
        "expression owner and chain must stay contiguous: {hunks:#?}",
    );
    assert!(matches!(
        rows[local[3]],
        DiffRow::LineChange {
            before: Some(_),
            after: Some(_),
        }
    ));
    let elision = rows
        .iter()
        .position(|row| matches!(row, DiffRow::Elision(_)))
        .expect("distant history context must remain folded");
    assert!(signature_end < elision && elision < local[0], "{hunks:#?}");
}

#[test]
fn context_halo_coalescing_has_an_exact_seven_line_boundary() {
    let review = |context: usize| {
        let stable = (0..context)
            .map(|index| format!("// stable {index}\n"))
            .collect::<String>();
        planned(
            "lib.rs",
            &format!("fn first() {{ old(); }}\n{stable}fn second() {{ old(); }}\n"),
            &format!("fn first() {{ new(); }}\n{stable}fn second() {{ new(); }}\n"),
        )
    };

    let touching = review(7);
    assert_eq!(touching.len(), 1, "{touching:#?}");
    assert!(
        touching[0]
            .rows
            .iter()
            .all(|row| !matches!(row, DiffRow::Elision(_)))
    );

    let separate = review(8);
    assert_eq!(separate.len(), 2, "{separate:#?}");
}

#[test]
fn multiline_wiring_units_keep_physical_rows() {
    for (path, before, after) in [
        (
            "lib.rs",
            "use crate::{\n    Alpha,\n    Beta,\n};\n",
            "use crate::{\n    Alpha,\n    Beta,\n    Gamma,\n};\n",
        ),
        (
            "view.ts",
            "import {\n  Alpha,\n  Beta,\n} from \"pkg\";\n",
            "import {\n  Alpha,\n  Beta,\n  Gamma,\n} from \"pkg\";\n",
        ),
    ] {
        let hunks = planned(path, before, after);
        assert!(
            !hunks
                .iter()
                .flat_map(|hunk| &hunk.rows)
                .any(|row| matches!(row, DiffRow::Wordwise(_)))
        );
        assert!(
            hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
                matches!(
                    row,
                    DiffRow::LineChange {
                        after: Some(line),
                        ..
                    } if line_text(line).contains("Gamma")
                )
            }),
            "{path}: {hunks:#?}"
        );
    }
}

#[test]
fn jsx_wrapper_reuses_generic_reparented_subtree_handling() {
    let before = "function View() {\n  return (\n    <article>\n      <img src=\"x\" />\n    </article>\n  );\n}\n";
    let after = "function View() {\n  return (\n    <article>\n      <div>\n        <img src=\"x\" />\n      </div>\n    </article>\n  );\n}\n";
    let hunks = planned("view.tsx", before, after);

    assert!(
        hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
            matches!(
                row,
                DiffRow::Reflow(line) if line_text(line).trim_start().starts_with("<img")
            )
        }),
        "{hunks:#?}"
    );
    assert!(!hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
        matches!(
            row,
            DiffRow::LineChange { before, after }
                if before.iter().chain(after).any(|line| line_text(line).contains("<img"))
        )
    }));
}
