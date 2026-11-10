//! Behavior of the whole pipeline, checked through the rows it presents.

mod languages;
mod preservation;
mod structure;

use super::*;
use crate::fixture::{AFTER, BEFORE, LABEL};

/// Diff and check every row against the sources: numbers exist, texts match, context is equal.
fn diff(path: &str, before: &str, after: &str) -> FileDiff {
    let presented = diff_file(path, before, after).expect("source must diff");
    assert!(
        !matches!(
            presented.comparison,
            Comparison::Lines(LineFallback::VerificationFailure(_))
        ),
        "syntax account failed verification for {path}"
    );
    assert_rows_truthful(before, after, &presented);
    presented
}

fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n')
        .map(|line| {
            line.strip_suffix("\r\n")
                .or_else(|| line.strip_suffix('\n'))
                .unwrap_or(line)
        })
        .collect()
}

fn assert_rows_truthful(before: &str, after: &str, presented: &FileDiff) {
    let before_lines = lines(before);
    let after_lines = lines(after);
    for hunk in &presented.hunks {
        for row in &hunk.excerpt.rows {
            match row {
                Row::Context { before, after } => {
                    assert_eq!(before_lines[before.index()], after.text(), "{row:?}");
                    assert_eq!(after_lines[after.number.index()], after.text(), "{row:?}");
                }
                Row::Crumb { before, after } => {
                    assert_eq!(before_lines[before.index()], after.text(), "{row:?}");
                    assert_eq!(after_lines[after.number.index()], after.text(), "{row:?}");
                }
                Row::Reflow { before, after } => {
                    // Token equality is the engine's own verified invariant; here only the texts.
                    for line in before {
                        assert!(line.get() <= before_lines.len(), "{row:?}");
                    }
                    for line in after {
                        assert_eq!(after_lines[line.number.index()], line.text());
                    }
                }
                Row::Removed(line) => assert_eq!(before_lines[line.number.index()], line.text()),
                Row::Added(line) => assert_eq!(after_lines[line.number.index()], line.text()),
                Row::Changed { before, after } => {
                    assert_eq!(before_lines[before.number.index()], before.text());
                    assert_eq!(after_lines[after.number.index()], after.text());
                    assert_ne!(
                        before.text(),
                        after.text(),
                        "changed row with identical sides"
                    );
                }
                Row::Moved { before, after, .. } => {
                    assert_eq!(after_lines[after.number.index()], after.text());
                    assert_eq!(
                        before_lines[before.index()]
                            .split_whitespace()
                            .collect::<String>(),
                        after.text().split_whitespace().collect::<String>()
                    );
                }
                Row::Ending(_) | Row::Elision(_) | Row::FileEnd => {}
            }
        }
    }
}

fn hunk_text(hunk: &Hunk) -> String {
    let mut text = String::new();
    for row in &hunk.excerpt.rows {
        let line = match row {
            Row::Context { after, .. } | Row::Crumb { after, .. } => format!("  {}", after.text()),
            Row::Reflow { after, .. } => after
                .iter()
                .map(|line| format!("~ {}", line.text()))
                .collect::<Vec<_>>()
                .join("\n"),
            Row::Removed(line) => format!("- {}", line.text()),
            Row::Added(line) => format!("+ {}", line.text()),
            Row::Changed { before, after } => format!("- {}\n+ {}", before.text(), after.text()),
            Row::Moved { before, after, .. } => format!("{before}> {}", after.text()),
            Row::Ending(_) => "⏎".to_owned(),
            Row::Elision(_) => "⋮".to_owned(),
            Row::FileEnd => "EOF".to_owned(),
        };
        text.push_str(&line);
        text.push('\n');
    }
    text
}

fn all_text(presented: &FileDiff) -> String {
    presented
        .hunks
        .iter()
        .map(hunk_text)
        .collect::<Vec<_>>()
        .join("---\n")
}

fn removed(presented: &FileDiff) -> Vec<String> {
    presented
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.excerpt.rows)
        .filter_map(|row| match row {
            Row::Removed(line) | Row::Changed { before: line, .. } => Some(line.text()),
            _ => None,
        })
        .collect()
}

fn added(presented: &FileDiff) -> Vec<String> {
    presented
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.excerpt.rows)
        .filter_map(|row| match row {
            Row::Added(line) | Row::Changed { after: line, .. } => Some(line.text()),
            _ => None,
        })
        .collect()
}

fn hunk_containing<'a>(presented: &'a FileDiff, needle: &str) -> &'a Hunk {
    presented
        .hunks
        .iter()
        .find(|hunk| hunk_text(hunk).contains(needle))
        .unwrap_or_else(|| panic!("no hunk contains {needle:?}:\n{}", all_text(presented)))
}

/// After lines some row shows, or an elision with equal sides vouches for.
fn accounted_after_lines(presented: &FileDiff) -> std::collections::BTreeSet<usize> {
    let mut lines = std::collections::BTreeSet::new();
    for row in presented.hunks.iter().flat_map(|hunk| &hunk.excerpt.rows) {
        match row {
            Row::Context { after, .. } | Row::Crumb { after, .. } | Row::Added(after) => {
                lines.insert(after.number.get());
            }
            Row::Changed { after, .. } | Row::Moved { after, .. } => {
                lines.insert(after.number.get());
            }
            Row::Reflow { after, .. } => lines.extend(after.iter().map(|line| line.number.get())),
            Row::Elision(Elision::Move { after, .. }) => {
                lines.extend(after.clone().into_iter().map(LineNumber::get));
            }
            Row::Elision(Elision::Gap { .. }) => {}
            Row::Removed(_) | Row::Ending(_) | Row::FileEnd => {}
        }
    }
    lines
}

fn unequal_elisions(presented: &FileDiff) -> Vec<String> {
    presented
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.excerpt.rows)
        .filter_map(|row| match row {
            Row::Elision(Elision::Move { before, after }) if before.len() != after.len() => {
                Some(format!("{before:?} / {after:?}"))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn unchanged_input_produces_no_hunks() {
    let presented = diff("alpha.rs", "fn a() {}\n", "fn a() {}\n");
    assert!(presented.hunks.is_empty());
}

#[test]
fn fixture_edits_move_and_wiring_are_separate_hunks() {
    let presented = diff(LABEL, BEFORE, AFTER);
    let text = all_text(&presented);
    // The moved function is presented once, as a move, with its origin line.
    assert!(
        text.contains("16> fn beta(alpha: &Alpha, gamma: u64) -> String {"),
        "{text}"
    );
    assert!(
        !removed(&presented)
            .iter()
            .any(|line| line.contains("fn beta")),
        "{text}"
    );
    // The import change is a one-line edit.
    let wiring = hunk_containing(&presented, "use crate::gamma");
    assert_eq!(wiring.excerpt.priority, Priority::Wiring, "{text}");
    // Nothing is shown as removed and re-added verbatim.
    for line in removed(&presented) {
        assert!(
            !added(&presented).contains(&line) || line.trim().is_empty(),
            "churn: {line}\n{text}"
        );
    }
}

#[test]
fn inserted_statement_keeps_neighbors_as_context() {
    let presented = diff(
        "alpha.rs",
        "fn a() {\n    one();\n    two();\n}\n",
        "fn a() {\n    one();\n    inserted();\n    two();\n}\n",
    );
    assert_eq!(presented.hunks.len(), 1);
    let text = hunk_text(&presented.hunks[0]);
    assert_eq!(
        text,
        "  fn a() {\n  one();\n+ inserted();\n  two();\n  }\nEOF\n"
            .replace("  one", "      one")
            .replace("  two", "      two")
            .replace("+ inserted", "+     inserted"),
        "{text}"
    );
}

#[test]
fn renamed_binding_is_an_inline_edit_not_a_rewrite() {
    let presented = diff(
        "alpha.rs",
        "fn a() {\n    let x = compute(1);\n    use_it(x);\n}\n",
        "fn a() {\n    let y = compute(2);\n    use_it(y);\n}\n",
    );
    let hunk = &presented.hunks[0];
    let changed = hunk
        .excerpt
        .rows
        .iter()
        .filter(|row| matches!(row, Row::Changed { .. }))
        .count();
    assert_eq!(changed, 2, "{}", hunk_text(hunk));
    let Row::Changed { before, after } = &hunk.excerpt.rows[1] else {
        panic!("{}", hunk_text(hunk));
    };
    let marked = |row: &SourceRow| {
        row.spans
            .iter()
            .filter(|span| span.mark != DiffMark::Context)
            .map(|span| span.text.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(marked(before), ["x", "1"]);
    assert_eq!(marked(after), ["y", "2"]);
}

#[test]
fn identical_line_after_an_insertion_is_never_removed_and_re_added() {
    let before = concat!(
        "fn calculate() {\n",
        "    let build_root = build_root(runner);\n",
        "    let cargo_exe = runner.cargo_exe()?;\n",
        "    fingerprint.check(\n",
        "        &build_root,\n",
        "        cargo_exe,\n",
        "    )?;\n",
        "    let fingerprint = Arc::new(fingerprint);\n",
        "}\n",
    );
    let after = concat!(
        "fn calculate() {\n",
        "    let build_root = build_root(runner);\n",
        "    let ws_root = runner.ws.root();\n",
        "    let cargo_exe = runner.cargo_exe()?;\n",
        "    fingerprint.check(\n",
        "        &build_root,\n",
        "        ws_root,\n",
        "        cargo_exe,\n",
        "    )?;\n",
        "    let fingerprint = Arc::new(fingerprint);\n",
        "}\n",
    );
    let presented = diff("alpha.rs", before, after);
    let removed = removed(&presented);
    assert!(
        removed.is_empty(),
        "pure insertion shows removals: {removed:?}\n{}",
        all_text(&presented)
    );
    assert_eq!(added(&presented).len(), 2, "{}", all_text(&presented));
}

#[test]
fn deleted_import_is_shown() {
    let before = "use a::A;\nuse b::B;\nuse c::C;\n\nfn main() { A; B; C; }\n";
    let after = "use a::A;\nuse c::C;\n\nfn main() { A; C; }\n";
    let presented = diff("alpha.rs", before, after);
    assert!(
        removed(&presented).iter().any(|line| line == "use b::B;"),
        "{}",
        all_text(&presented)
    );
}

#[test]
fn reordered_imports_are_reported_not_hidden() {
    let before = "use a::A;\nuse c::C;\nuse b::B;\n\nfn main() {}\n";
    let after = "use a::A;\nuse b::B;\nuse c::C;\n\nfn main() {}\n";
    let presented = diff("alpha.rs", before, after);
    assert!(!presented.hunks.is_empty(), "import reorder hidden");
    assert!(
        presented
            .hunks
            .iter()
            .all(|hunk| hunk.excerpt.priority == Priority::Wiring),
        "{}",
        all_text(&presented)
    );
}

#[test]
fn swapped_functions_are_one_move_without_separator_churn() {
    let before = "fn a() {\n    1;\n}\n\nfn b() {\n    2;\n}\n";
    let after = "fn b() {\n    2;\n}\n\nfn a() {\n    1;\n}\n";
    let presented = diff("alpha.rs", before, after);
    assert!(
        removed(&presented).is_empty() && added(&presented).is_empty(),
        "{}",
        all_text(&presented)
    );
    assert!(
        presented
            .hunks
            .iter()
            .any(|hunk| hunk.excerpt.priority == Priority::Move)
    );
}

#[test]
fn wrapper_insertion_marks_only_the_wrapper() {
    let presented = diff(
        "alpha.rs",
        "struct S {\n    inner: Poll<Sender>,\n}\n",
        "struct S {\n    inner: Option<Poll<Sender>>,\n}\n",
    );
    let hunk = &presented.hunks[0];
    let Some(Row::Changed { before, after }) = hunk
        .excerpt
        .rows
        .iter()
        .find(|row| matches!(row, Row::Changed { .. }))
    else {
        panic!("{}", hunk_text(hunk));
    };
    let before_marks = before
        .spans
        .iter()
        .filter(|span| span.mark == DiffMark::Removed)
        .count();
    let after_marked = after
        .spans
        .iter()
        .filter(|span| span.mark == DiffMark::Added)
        .map(|span| span.text.as_str())
        .collect::<String>();
    assert_eq!(before_marks, 0, "{}", hunk_text(hunk));
    assert_eq!(after_marked, "Option<>");
}

#[test]
fn empty_body_growing_a_body_keeps_the_old_braces_visible() {
    let presented = diff(
        "alpha.rs",
        "impl E for X {}\n",
        "impl E for X {\n    fn source(&self) -> Option<&E> {\n        None\n    }\n}\n",
    );
    assert_eq!(
        removed(&presented),
        ["impl E for X {}"],
        "{}",
        all_text(&presented)
    );
}

#[test]
fn line_mode_handles_unknown_languages() {
    let presented = diff("notes.txt", "alpha\nbeta\ngamma\n", "alpha\ngamma\ndelta\n");
    assert_eq!(removed(&presented), ["beta"]);
    assert_eq!(added(&presented), ["delta"]);
}

#[test]
fn whole_file_addition_is_one_addition_hunk() {
    let presented = diff(
        "alpha.py",
        "",
        "def a():\n    pass\n\n\ndef b():\n    pass\n",
    );
    assert_eq!(presented.hunks.len(), 1);
    assert!(removed(&presented).is_empty());
    assert_eq!(added(&presented).len(), 6);
    assert!(matches!(
        presented.hunks[0].excerpt.rows.last(),
        Some(Row::FileEnd)
    ));
}

#[test]
fn missing_final_newline_change_is_announced() {
    let presented = diff("alpha.rs", "fn a() {}", "fn a() {}\n");
    assert!(
        presented
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.excerpt.rows)
            .any(|row| matches!(row, Row::Ending(_))),
        "{}",
        all_text(&presented)
    );
}

#[test]
fn generated_files_are_flagged_and_still_diffed() {
    let before = "// @generated\nfn a() { 1 }\n";
    let after = "// @generated\nfn a() { 2 }\n";
    let presented = diff("alpha.rs", before, after);
    assert!(presented.generated);
    assert_eq!(removed(&presented), ["fn a() { 1 }"]);
    assert_eq!(added(&presented), ["fn a() { 2 }"]);
}

#[test]
fn python_indentation_only_change_is_reflow() {
    let presented = diff(
        "alpha.py",
        "def a():\n  return 1\n",
        "def a():\n    return 1\n",
    );
    assert!(
        presented
            .hunks
            .iter()
            .all(|hunk| hunk.excerpt.priority == Priority::Reflow),
        "{}",
        all_text(&presented)
    );
}

#[test]
fn python_moving_a_statement_into_a_branch_is_an_edit() {
    let presented = diff(
        "alpha.py",
        "def a():\n    if x:\n        pass\n    log()\n",
        "def a():\n    if x:\n        log()\n",
    );
    assert!(
        presented
            .hunks
            .iter()
            .any(|hunk| hunk.excerpt.priority == Priority::Edit),
        "{}",
        all_text(&presented)
    );
}

#[test]
fn typescript_method_edit_names_its_class_and_method() {
    let before = "class A {\n  run(): number {\n    return 1;\n  }\n}\n";
    let after = "class A {\n  run(): number {\n    return 2;\n  }\n}\n";
    let presented = diff("alpha.ts", before, after);
    let names = presented.hunks[0]
        .attribution
        .symbols()
        .map(|symbol| symbol.name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"run") && names.contains(&"A"), "{names:?}");
}

#[test]
fn deep_edit_shows_enclosing_headings_once() {
    let mut before = String::from("impl Thing {\n    fn outer(&self) {\n");
    for index in 0..30 {
        before.push_str(&format!("        step_{index}();\n"));
    }
    before.push_str("    }\n}\n");
    let after = before.replace("step_20();", "step_twenty();");
    let presented = diff("alpha.rs", &before, &after);
    let text = all_text(&presented);
    assert_eq!(text.matches("impl Thing {").count(), 1, "{text}");
    assert_eq!(text.matches("fn outer(&self) {").count(), 1, "{text}");
    assert!(text.contains("- ") && text.contains("step_20"), "{text}");
}

#[test]
fn rewritten_block_is_shown_whole_not_shredded() {
    let before = concat!(
        "fn run() {\n",
        "    let a = parse(input);\n",
        "    let b = validate(a);\n",
        "    let c = transform(b);\n",
        "    emit(c);\n",
        "}\n",
    );
    let after = concat!(
        "fn run() {\n",
        "    let parsed = Parser::new(input).parse()?;\n",
        "    let checked = Validator::default().check(parsed)?;\n",
        "    let output = Transformer::run(checked);\n",
        "    emit_all(output);\n",
        "}\n",
    );
    let presented = diff("alpha.rs", before, after);
    let text = hunk_text(&presented.hunks[0]);
    let position = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle} missing in {text}"))
    };
    // All removals precede all additions: a before/after block rather than an interleaving.
    assert!(position("- ") < position("+ "), "{text}");
    let last_removed = text.rfind("\n- ").unwrap();
    let first_added = position("\n+ ");
    assert!(last_removed < first_added, "{text}");
}

#[test]
fn moved_block_with_a_rewrapped_line_elides_only_equal_runs() {
    let before = "fn bench(b: &mut Bencher) {
    let rt = rt();

    let task = || async {
        let mut file = open(DEV_ZERO).await.unwrap();
        for _i in 0..BLOCK_COUNT {
            let mut buffer = [0u8; BUFFER_SIZE];
            let mut file_ref = file.as_mut();
            read(&mut file_ref, &mut buffer).await;
        }
    };

    b.iter(|| {
        rt.block_on(task());
    });
}
";
    let after = "fn bench(c: &mut Criterion) {
    let rt = rt();

    c.bench_function(\"bench\", |b| {
        b.iter(|| {
            let task = || async {
                let mut file =
                    open(DEV_ZERO).await.unwrap();
                for _i in 0..BLOCK_COUNT {
                    let mut buffer = [0u8; BUFFER_SIZE];
                    let mut file_ref = file.as_mut();
                    read(&mut file_ref, &mut buffer).await;
                }
            };

            rt.block_on(task());
        });
    });
}
";
    let presented = diff("alpha.rs", before, after);
    let text = all_text(&presented);
    assert!(
        unequal_elisions(&presented).is_empty(),
        "{:?}\n{text}",
        unequal_elisions(&presented)
    );
    // Every line of the re-indented block is shown or vouched for; none is swallowed by an elision.
    let accounted = accounted_after_lines(&presented);
    for line in 6..=14 {
        assert!(
            accounted.contains(&line),
            "after line {line} is unaccounted for\n{text}"
        );
    }
}

#[test]
fn a_call_that_gains_a_try_and_a_method_keeps_its_identical_lines() {
    let before = "fn a() {
    host(|| {
        resolve(
            alpha,
            beta,
        )
    })
}
";
    let after = "fn a() {
    host(|| {
        resolve(
            alpha,
            beta,
        )?
        .into_js(global)
    })
}
";
    let presented = diff("alpha.rs", before, after);
    let text = all_text(&presented);
    assert!(
        !removed(&presented)
            .iter()
            .any(|line| line.contains("resolve(")),
        "{text}"
    );
    assert!(
        !added(&presented)
            .iter()
            .any(|line| line.contains("resolve(")),
        "{text}"
    );
    assert!(
        added(&presented)
            .iter()
            .any(|line| line.contains(".into_js(global)")),
        "{text}"
    );
}

#[test]
fn blank_separators_travel_with_a_moved_function_and_stay_accounted_for() {
    let before = "fn a() {\n    1;\n}\n\nfn b() {\n    2;\n}\n\nfn c() {\n    3;\n}\n";
    let after = "fn b() {\n    2;\n}\n\nfn a() {\n    1;\n}\n\nfn c() {\n    3;\n}\n";
    let presented = diff("alpha.rs", before, after);
    let text = all_text(&presented);
    let starts = presented
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.excerpt.rows)
        .filter_map(|row| match row {
            Row::Moved {
                after,
                edge: MoveEdge::Start | MoveEdge::Only,
                ..
            } => Some(after.text()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        !starts.is_empty() && starts.iter().all(|line| line.starts_with("fn ")),
        "{starts:?}\n{text}"
    );
    let accounted = accounted_after_lines(&presented);
    for line in 1..=lines(after).len() {
        assert!(
            accounted.contains(&line),
            "after line {line} is unaccounted for\n{text}"
        );
    }
}
