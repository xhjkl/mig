use super::*;

#[test]
fn overlapping_syntax_on_one_line_has_one_review_owner() {
    let before = "#![allow(alpha)]// beta\nconst ALPHA: u8 = 1;\n";
    let after = "#![allow(alpha)]// beta\nconst ALPHA: u8 = 2;\n";

    let diff = diff_file("alpha.rs", before, after).expect("Rust must plan");

    assert_unchanged_payload(&diff, "#![allow(alpha)]// beta");
    assert_added_payload(&diff, "2");
    assert_removed_payload(&diff, "1");
    assert_source_ownership(&diff);
}

#[test]
fn renamed_declaration_keeps_its_body_as_context() {
    let before = concat!(
        "const Alpha = beta.object({\n",
        "    alpha: beta.string(),\n",
        "    beta: beta.boolean(),\n",
        "})\n",
        "\n",
        "consume(Alpha)\n",
    );
    let after = concat!(
        "const Beta = beta.object({\n",
        "    alpha: beta.string(),\n",
        "    beta: beta.boolean(),\n",
        "})\n",
        "\n",
        "consume(Beta)\n",
    );

    let diff = diff_file("alpha.ts", before, after).expect("TypeScript must plan");

    for payload in ["alpha: beta.string(),", "beta: beta.boolean(),", "})"] {
        assert_unchanged_payload(&diff, payload);
        assert_current_payload_once(&diff, payload);
    }
    assert_added_payload(&diff, "Beta");
    assert_removed_payload(&diff, "Alpha");
    assert_source_ownership(&diff);
}

#[test]
fn edited_opaque_block_keeps_unchanged_physical_lines() {
    let before = concat!(
        "const ALPHA: &str = r#\"\n",
        "alpha\n",
        "beta\n",
        "gamma\n",
        "\"#;\n",
    );
    let after = concat!(
        "const ALPHA: &str = r#\"\n",
        "alpha\n",
        "delta\n",
        "gamma\n",
        "\"#;\n",
    );

    let diff = diff_file("alpha.rs", before, after).expect("Rust must plan");

    for payload in ["const ALPHA: &str", "alpha", "gamma", "\"#;"] {
        assert_unchanged_payload(&diff, payload);
        assert_current_payload_once(&diff, payload);
    }
    assert_added_payload(&diff, "delta");
    assert_removed_payload(&diff, "beta");
    assert_source_ownership(&diff);
}

#[test]
fn stylesheet_edit_keeps_the_rule_frame_as_context() {
    let before = concat!(".alpha {\n", "  color: alpha;\n", "  margin: 0;\n", "}\n",);
    let after = concat!(".alpha {\n", "  color: beta;\n", "  margin: 0;\n", "}\n",);

    let diff = diff_file("alpha.css", before, after).expect("CSS must plan");

    for payload in [".alpha {", "margin: 0;", "}"] {
        assert_unchanged_payload(&diff, payload);
        assert_current_payload_once(&diff, payload);
    }
    assert_added_payload(&diff, "beta");
    assert_removed_payload(&diff, "alpha;");
    assert_source_ownership(&diff);
}

#[test]
fn edited_reparented_subtree_preserves_its_unchanged_child() {
    let before = concat!("<main>\n", "  <section>alpha</section>\n", "</main>\n",);
    let after = "<section class=\"beta\">alpha</section>\n";

    let diff = diff_file("alpha.html", before, after).expect("HTML must plan");

    assert_context_payload(&diff, "alpha");
    assert_current_payload_once(&diff, "alpha</section>");
    assert_added_payload(&diff, "class=\"beta\"");
    assert_removed_payload(&diff, "<main>");
    assert!(
        diff.hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
            matches!(
                row,
                DiffRow::Line(line)
                    if line.has_changes() && line_text(line).contains("alpha</section>")
            )
        }),
        "the retained subtree belongs to one mixed-mark current row: {diff:#?}"
    );
    assert!(
        !diff.hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
            matches!(
                row,
                DiffRow::LineChange {
                    before: Some(line),
                    ..
                } if line_text(line).contains("alpha</section>")
            )
        }),
        "retained payload must not leave an old-world ghost: {diff:#?}"
    );
    assert_source_ownership(&diff);
}

#[test]
fn extracted_block_is_shown_once_in_its_current_home() {
    let before = concat!(
        "fn alpha() {\n",
        "    alpha();\n",
        "    beta();\n",
        "    gamma();\n",
        "}\n",
    );
    let after = concat!(
        "fn alpha() {\n",
        "    alpha();\n",
        "    delta();\n",
        "}\n",
        "\n",
        "fn delta() {\n",
        "    beta();\n",
        "    gamma();\n",
        "}\n",
    );

    let diff = diff_file("alpha.rs", before, after).expect("Rust must plan");

    for payload in ["beta();", "gamma();"] {
        assert_unchanged_payload(&diff, payload);
        assert_current_payload_once(&diff, payload);
    }
    assert_added_payload(&diff, "delta");
    assert_source_ownership(&diff);
}

#[test]
fn isolated_expression_edit_does_not_repeat_distant_breadcrumbs() {
    let before = expression_fixture("let beta = Beta::Alpha;");
    let after = expression_fixture("let beta = self.beta.unwrap_or(Beta::Alpha);");

    let diff = diff_file("alpha.rs", &before, &after).expect("Rust must plan");

    assert_eq!(
        diff.hunks.len(),
        1,
        "one edit needs one focused hunk: {diff:#?}"
    );
    assert_eq!(
        current_payload_count(&diff, "#[cfg(beta)]"),
        0,
        "a distant declaration is not a useful breadcrumb: {diff:#?}"
    );
    assert_added_payload(&diff, "unwrap_or");
    assert_line_fragment_mark(&diff, "unwrap_or", "Beta", DiffMark::Context);
    assert_line_fragment_mark(&diff, "unwrap_or", "Alpha", DiffMark::Context);
    assert_source_ownership(&diff);
}

#[test]
fn one_sided_structural_files_keep_physical_source_order() {
    let source = concat!(
        "fn alpha() {\n",
        "\n",
        "    beta();\n",
        "}\n",
        "\n",
        "fn gamma() {}\n",
    );

    for (before, after, side) in [(source, "", false), ("", source, true)] {
        let diff = diff_file("alpha.rs", before, after).expect("Rust must plan");
        let numbers = source_lines(&diff)
            .filter_map(|(line, current)| (current == side).then_some(line.number))
            .collect::<Vec<_>>();

        assert_eq!(numbers, (1..=6).collect::<Vec<_>>(), "{diff:#?}");
        assert_source_ownership(&diff);
    }
}

#[test]
fn deep_edit_keeps_ancestor_breadcrumbs_and_a_local_halo() {
    let before = concat!(
        "impl Alpha {\n",
        "    fn alpha(&self) {\n",
        "        let alpha = 1;\n",
        "        let beta = self.beta();\n",
        "        let gamma = 3;\n",
        "    }\n",
        "\n",
        "    fn beta(&self) {\n",
        "        beta();\n",
        "    }\n",
        "}\n",
    );
    let after = before.replace("self.beta()", "self.gamma()");

    let diff = diff_file("alpha.rs", before, &after).expect("Rust must plan");
    let hunk = diff.hunks.first().expect("the edit must be visible");

    for payload in [
        "impl Alpha",
        "fn alpha",
        "let alpha = 1",
        "self.gamma()",
        "let gamma = 3",
    ] {
        assert!(
            hunk_contains(hunk, payload),
            "missing {payload:?}: {hunk:#?}"
        );
    }
    assert_source_ownership(&diff);
}

#[test]
fn payload_move_wiring_and_reflow_have_distinct_review_priority() {
    let separation = concat!(
        "const ALPHA_0: u8 = 0;\n",
        "const ALPHA_1: u8 = 0;\n",
        "const ALPHA_2: u8 = 0;\n",
        "const ALPHA_3: u8 = 0;\n",
        "const ALPHA_4: u8 = 0;\n",
        "const ALPHA_5: u8 = 0;\n",
        "const ALPHA_6: u8 = 0;\n",
        "const ALPHA_7: u8 = 0;\n",
    );
    let before = format!(
        concat!(
            "use alpha::Alpha;\n",
            "mod alpha;\n",
            "{separation}",
            "fn beta() {{ beta(); }}\n",
            "{separation}",
            "fn gamma() -> u8 {{ 1 }}\n",
            "{separation}",
            "fn delta() {{ alpha(); }}\n",
            "{separation}",
            "fn epsilon() {{}}\n",
        ),
        separation = separation,
    );
    let after = format!(
        concat!(
            "use beta::Beta;\n",
            "mod alpha;\n",
            "mod beta;\n",
            "{separation}",
            "{separation}",
            "fn gamma() -> u8 {{\n",
            "    1\n",
            "}}\n",
            "{separation}",
            "fn delta() {{ beta(); }}\n",
            "{separation}",
            "fn epsilon() {{}}\n",
            "fn beta() {{ beta(); }}\n",
        ),
        separation = separation,
    );

    let diff = diff_file("alpha.rs", &before, &after).expect("Rust must plan");
    let payload = hunk_position(&diff, "fn delta");
    let movement = diff
        .hunks
        .iter()
        .position(|hunk| {
            hunk.rows
                .iter()
                .any(|row| matches!(row, DiffRow::Moved { .. }))
        })
        .expect("the exact function must be a move");
    let wiring = diff
        .hunks
        .iter()
        .position(|hunk| hunk_contains(hunk, "mod beta;"))
        .expect("the compact declarations must remain visible as wiring");
    let reflow = diff
        .hunks
        .iter()
        .position(|hunk| hunk_contains(hunk, "fn gamma"))
        .expect("the formatting-only function must remain visible");

    assert!(
        payload < movement && movement < wiring && wiring < reflow,
        "review order must follow signal value: {:#?}",
        diff.hunks
    );
    assert!(
        diff.hunks[wiring]
            .rows
            .iter()
            .any(|row| matches!(row, DiffRow::Wordwise(_))),
        "the import replacement belongs in the wiring hunk: {:#?}",
        diff.hunks[wiring]
    );
    assert_source_ownership(&diff);
}

fn expression_fixture(expression: &str) -> String {
    format!(
        concat!(
            "use alpha::Alpha;\n",
            "\n",
            "pub mod alpha;\n",
            "\n",
            "#[cfg(beta)]\n",
            "pub mod beta;\n",
            "\n",
            "pub struct Alpha {{\n",
            "    alpha: usize,\n",
            "    beta: Option<Beta>,\n",
            "}}\n",
            "\n",
            "impl Alpha {{\n",
            "    pub fn alpha(&self) -> Beta {{\n",
            "        let alpha = self.alpha;\n",
            "        {expression}\n",
            "        let gamma = alpha + 1;\n",
            "        beta.with(gamma)\n",
            "    }}\n",
            "}}\n",
            "\n",
            "pub fn beta() {{\n",
            "    beta();\n",
            "}}\n",
        ),
        expression = expression,
    )
}

fn source_lines(diff: &FileDiff) -> impl Iterator<Item = (&DisplayLine, bool)> {
    diff.hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .flat_map(|row| match row {
            DiffRow::Line(line) | DiffRow::Reflow(line) | DiffRow::Moved { after: line, .. } => {
                vec![(line, true)]
            }
            DiffRow::LineChange { before, after } => before
                .iter()
                .map(|line| (line, false))
                .chain(after.iter().map(|line| (line, true)))
                .collect(),
            DiffRow::LineEnding { .. }
            | DiffRow::Wordwise(_)
            | DiffRow::Elision(_)
            | DiffRow::FileBoundary => Vec::new(),
        })
}

fn line_text(line: &DisplayLine) -> String {
    line.spans.iter().map(|span| span.text.as_str()).collect()
}

fn assert_unchanged_payload(diff: &FileDiff, payload: &str) {
    let matching = source_lines(diff)
        .filter(|(line, _)| line_text(line).contains(payload))
        .collect::<Vec<_>>();
    assert!(
        !matching.is_empty(),
        "missing stable payload {payload:?}: {diff:#?}"
    );
    assert!(
        matching.iter().all(|(line, _)| !line.has_changes()),
        "stable payload {payload:?} became a material change: {matching:#?}"
    );
}

fn assert_current_payload_once(diff: &FileDiff, payload: &str) {
    let occurrences = current_payload_count(diff, payload);
    assert_eq!(occurrences, 1, "current payload {payload:?}: {diff:#?}");
}

fn assert_context_payload(diff: &FileDiff, payload: &str) {
    assert!(
        source_lines(diff).any(|(line, current)| {
            current
                && line
                    .spans
                    .iter()
                    .any(|span| span.mark == DiffMark::Context && span.text.contains(payload))
        }),
        "missing retained payload {payload:?}: {diff:#?}"
    );
}

fn assert_line_fragment_mark(diff: &FileDiff, line_payload: &str, fragment: &str, mark: DiffMark) {
    assert!(
        source_lines(diff).any(|(line, current)| {
            current
                && line_text(line).contains(line_payload)
                && line
                    .spans
                    .iter()
                    .any(|span| span.mark == mark && span.text.contains(fragment))
        }),
        "missing {mark:?} fragment {fragment:?} on {line_payload:?}: {diff:#?}"
    );
}

fn current_payload_count(diff: &FileDiff, payload: &str) -> usize {
    source_lines(diff)
        .filter(|(line, current)| *current && line_text(line).contains(payload))
        .count()
}

fn assert_added_payload(diff: &FileDiff, payload: &str) {
    assert!(
        source_lines(diff).any(|(line, current)| {
            current
                && line_text(line).contains(payload)
                && line.spans.iter().any(|span| span.mark == DiffMark::Added)
        }) || diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .any(|row| { matches!(row, DiffRow::Wordwise(word) if word.added.contains(payload)) }),
        "missing added payload {payload:?}: {diff:#?}"
    );
}

fn assert_removed_payload(diff: &FileDiff, payload: &str) {
    assert!(
        source_lines(diff).any(|(line, current)| {
            !current
                && line_text(line).contains(payload)
                && line.spans.iter().any(|span| span.mark == DiffMark::Removed)
        }) || diff.hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
            matches!(row, DiffRow::Wordwise(word) if word.removed.contains(payload))
        }),
        "missing removed payload {payload:?}: {diff:#?}"
    );
}

fn assert_source_ownership(diff: &FileDiff) {
    let mut global_before = std::collections::HashMap::<usize, (usize, bool)>::new();
    let mut global_after = std::collections::HashMap::<usize, (usize, bool)>::new();
    for hunk in &diff.hunks {
        let mut before = std::collections::HashSet::new();
        let mut after = std::collections::HashSet::new();
        for row in &hunk.rows {
            let (before_line, after_line, material) = match row {
                DiffRow::Line(line) => (None, Some(line.number), line.has_changes()),
                DiffRow::Reflow(line) => (None, Some(line.number), true),
                DiffRow::LineChange { before, after } => (
                    before.as_ref().map(|line| line.number),
                    after.as_ref().map(|line| line.number),
                    true,
                ),
                DiffRow::Moved {
                    before,
                    after: line,
                } => (*before, Some(line.number), true),
                DiffRow::Wordwise(word) => (word.before_line, word.after_line, true),
                DiffRow::LineEnding { .. } | DiffRow::Elision(_) | DiffRow::FileBoundary => {
                    (None, None, false)
                }
            };
            if let Some(line) = before_line {
                assert!(
                    before.insert(line),
                    "before line {line} has two owners: {hunk:#?}"
                );
                let global = global_before.entry(line).or_default();
                global.0 += 1;
                global.1 |= material;
            }
            if let Some(line) = after_line {
                assert!(
                    after.insert(line),
                    "current line {line} has two owners: {hunk:#?}"
                );
                let global = global_after.entry(line).or_default();
                global.0 += 1;
                global.1 |= material;
            }
        }
    }

    for (side, owners) in [("before", global_before), ("current", global_after)] {
        for (line, (occurrences, material)) in owners {
            assert!(
                occurrences == 1 || !material,
                "{side} line {line} has {occurrences} owners including material review: {diff:#?}"
            );
        }
    }
}

fn hunk_contains(hunk: &Hunk, payload: &str) -> bool {
    hunk.rows.iter().any(|row| match row {
        DiffRow::Line(line) | DiffRow::Reflow(line) | DiffRow::Moved { after: line, .. } => {
            line_text(line).contains(payload)
        }
        DiffRow::LineChange { before, after } => before
            .iter()
            .chain(after)
            .any(|line| line_text(line).contains(payload)),
        DiffRow::Wordwise(word) => [
            word.prefix.as_str(),
            word.removed.as_str(),
            word.added.as_str(),
            word.suffix.as_str(),
        ]
        .concat()
        .contains(payload),
        DiffRow::LineEnding { .. } | DiffRow::Elision(_) | DiffRow::FileBoundary => false,
    })
}

fn hunk_position(diff: &FileDiff, payload: &str) -> usize {
    diff.hunks
        .iter()
        .position(|hunk| hunk_contains(hunk, payload))
        .unwrap_or_else(|| panic!("missing hunk for {payload:?}: {diff:#?}"))
}
