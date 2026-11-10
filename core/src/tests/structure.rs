//! Behaviors carried over from the previous engine's suite, restated for the row model.

use super::*;

fn marked(row: &SourceRow, mark: DiffMark) -> Vec<String> {
    row.spans
        .iter()
        .filter(|span| span.mark == mark)
        .map(|span| span.text.clone())
        .collect()
}

fn rows(presented: &FileDiff) -> impl Iterator<Item = &Row> {
    presented.hunks.iter().flat_map(|hunk| &hunk.excerpt.rows)
}

/// Texts of every row that shows an after-side line.
fn current_texts(presented: &FileDiff) -> Vec<String> {
    rows(presented)
        .flat_map(|row| match row {
            Row::Context { after, .. }
            | Row::Crumb { after, .. }
            | Row::Added(after)
            | Row::Changed { after, .. }
            | Row::Moved { after, .. } => vec![after.text()],
            Row::Reflow { after, .. } => after.iter().map(SourceRow::text).collect(),
            _ => Vec::new(),
        })
        .collect()
}

fn added_numbers(presented: &FileDiff) -> Vec<usize> {
    let mut numbers = rows(presented)
        .filter_map(|row| match row {
            Row::Added(line) | Row::Changed { after: line, .. } => Some(line.number.get()),
            _ => None,
        })
        .collect::<Vec<_>>();
    numbers.sort_unstable();
    numbers
}

fn has_context(presented: &FileDiff, payload: &str) -> bool {
    rows(presented)
        .any(|row| matches!(row, Row::Context { after, .. } if after.text().contains(payload)))
}

#[test]
fn python_layout_changes_are_reflow_only() {
    let triple = "'''";
    let cases = [
        (
            "def alpha(): pass\n".to_owned(),
            "def alpha():\n\tpass\n".to_owned(),
        ),
        (
            "def alpha(): return f'{beta+gamma}'\n".to_owned(),
            "def alpha(): return f'{ beta + gamma }'\n".to_owned(),
        ),
        (
            "def alpha():\n    return 1\n".to_owned(),
            "def alpha():\n\treturn 1\n".to_owned(),
        ),
        (
            "def alpha():\n    if beta:\n        gamma()\n    delta()\n".to_owned(),
            "def alpha():\n  if beta:\n    gamma()\n  delta()\n".to_owned(),
        ),
        (
            "def alpha():\n    return (1 + 2)\n".to_owned(),
            "def alpha():\n    return (1\n      + 2)\n".to_owned(),
        ),
        (
            "def alpha():\n    return 1 + 2\n".to_owned(),
            "def alpha():\n    return 1 + \\\n        2\n".to_owned(),
        ),
    ];
    let _ = triple;
    for (before, after) in &cases {
        for (before, after) in [(before, after), (after, before)] {
            let presented = diff("alpha.py", before, after);
            assert!(
                !presented.hunks.is_empty(),
                "layout change hidden: {before:?} -> {after:?}"
            );
            assert!(
                presented
                    .hunks
                    .iter()
                    .all(|hunk| hunk.excerpt.priority == Priority::Reflow),
                "layout change presented as an edit: {before:?} -> {after:?}\n{}",
                all_text(&presented)
            );
            assert!(
                removed(&presented).is_empty() && added(&presented).is_empty(),
                "{}",
                all_text(&presented)
            );
        }
    }
}

#[test]
fn python_indentation_that_changes_scope_is_a_material_edit() {
    for (before, after, affected) in [
        (
            "def alpha():\n    if beta:\n        gamma()\n    delta()\n",
            "def alpha():\n    if beta:\n        gamma()\n        delta()\n",
            "delta()",
        ),
        (
            "def alpha():\n    for beta in gamma:\n        delta()\n    epsilon()\n",
            "def alpha():\n    for beta in gamma:\n        delta()\n        epsilon()\n",
            "epsilon()",
        ),
        (
            "def alpha():\n    beta()\ngamma()\n",
            "def alpha():\n    beta()\n    gamma()\n",
            "gamma()",
        ),
    ] {
        for (before, after) in [(before, after), (after, before)] {
            let presented = diff("alpha.py", before, after);
            let shown_as_change = rows(&presented).any(|row| match row {
                Row::Removed(line) | Row::Added(line) => line.text().contains(affected),
                Row::Changed { before, after } => {
                    before.text().contains(affected) || after.text().contains(affected)
                }
                Row::Moved { after, .. } => after.text().contains(affected),
                _ => false,
            });
            assert!(
                shown_as_change,
                "scope change disappeared: {before:?} -> {after:?}\n{}",
                all_text(&presented)
            );
        }
    }
}

#[test]
fn python_decorated_definitions_move_with_their_decorators() {
    let alpha = "@beta\ndef alpha():\n    return 1\n";
    let gamma = "@delta\ndef gamma():\n    return 2\n";
    let before = format!("{alpha}\n{gamma}");
    let after = format!("{gamma}\n{alpha}");
    let presented = diff("alpha.py", &before, &after);
    assert!(
        rows(&presented)
            .any(|row| matches!(row, Row::Moved { after, .. } if after.text().starts_with('@'))),
        "{}",
        all_text(&presented)
    );
    assert!(
        removed(&presented).is_empty() && added(&presented).is_empty(),
        "{}",
        all_text(&presented)
    );
}

#[test]
fn python_literal_and_debug_fstring_whitespace_remains_material() {
    let triple = "'''";
    let cases = [
        (
            "def alpha(): return 'a b'\n".to_owned(),
            "def alpha(): return 'a  b'\n".to_owned(),
        ),
        (
            format!("def alpha():\n    return {triple}a\n    b{triple}\n"),
            format!("def alpha():\n    return {triple}a\n        b{triple}\n"),
        ),
        (
            "def alpha(): return f'{beta=}'\n".to_owned(),
            "def alpha(): return f'{ beta = }'\n".to_owned(),
        ),
    ];
    for (before, after) in &cases {
        let presented = diff("alpha.py", before, after);
        assert!(
            !removed(&presented).is_empty() || !added(&presented).is_empty(),
            "literal edit disappeared: {before:?}\n{}",
            all_text(&presented)
        );
        assert!(
            !rows(&presented).any(|row| matches!(row, Row::Reflow { .. })),
            "literal edit became reflow: {before:?}\n{}",
            all_text(&presented)
        );
    }
}

#[test]
fn extracted_function_is_fully_added_and_its_old_body_removed() {
    let before = concat!(
        "fn alpha(beta: u64) -> bool {\n",
        "    let gamma = beta + 1;\n",
        "    (gamma > ALPHA)\n",
        "        && delta(gamma)\n",
        "        && epsilon(beta)\n",
        "}\n",
    );
    let after = concat!(
        "fn alpha(beta: u64) -> bool {\n",
        "    zeta(beta)\n",
        "}\n",
        "fn zeta(beta: u64) -> bool {\n",
        "    let gamma = beta + 1;\n",
        "    (gamma > BETA)\n",
        "        && delta(gamma)\n",
        "        && epsilon(beta)\n",
        "}\n",
    );
    let presented = diff("alpha.rs", before, after);
    assert_eq!(
        added_numbers(&presented),
        [2, 4, 5, 6, 7, 8, 9],
        "{}",
        all_text(&presented)
    );
    for payload in [
        "let gamma = beta + 1;",
        "(gamma > ALPHA)",
        "&& delta(gamma)",
        "&& epsilon(beta)",
    ] {
        assert!(
            removed(&presented)
                .iter()
                .any(|line| line.contains(payload)),
            "{payload} not removed\n{}",
            all_text(&presented)
        );
    }
    assert_eq!(
        current_texts(&presented)
            .iter()
            .filter(|line| line.contains("let gamma = beta + 1;"))
            .count(),
        1,
        "{}",
        all_text(&presented)
    );
}

#[test]
fn copied_multiline_call_keeps_its_edited_source_complete() {
    let before = concat!(
        "fn alpha() {\n",
        "    beta(\n",
        "        alpha,\n",
        "        gamma,\n",
        "        delta,\n",
        "    );\n",
        "}\n",
    );
    let after = concat!(
        "fn alpha() {\n",
        "    beta(epsilon);\n",
        "}\n",
        "fn zeta() {\n",
        "    beta(\n",
        "        alpha,\n",
        "        gamma,\n",
        "        delta,\n",
        "    );\n",
        "}\n",
    );
    let presented = diff("alpha.rs", before, after);
    assert!(
        added(&presented)
            .iter()
            .any(|line| line.contains("beta(epsilon)")),
        "{}",
        all_text(&presented)
    );
    assert_eq!(
        added_numbers(&presented),
        [2, 4, 5, 6, 7, 8, 9, 10],
        "{}",
        all_text(&presented)
    );
    for payload in ["beta(", "alpha,", "gamma,", "delta,", ");"] {
        assert!(
            removed(&presented)
                .iter()
                .any(|line| line.contains(payload)),
            "{payload} not removed\n{}",
            all_text(&presented)
        );
    }
}

#[test]
fn multiline_expansion_replaces_a_complete_statement() {
    let before = concat!(
        "fn alpha() {\n",
        "    let beta = gamma(delta, epsilon);\n",
        "}\n"
    );
    let after = concat!(
        "fn alpha() {\n",
        "    let beta = gamma(\n",
        "        delta,\n",
        "        zeta,\n",
        "    );\n",
        "}\n",
    );
    let presented = diff("alpha.rs", before, after);
    assert_eq!(
        removed(&presented),
        ["    let beta = gamma(delta, epsilon);"],
        "{}",
        all_text(&presented)
    );
    assert_eq!(
        added_numbers(&presented),
        [2, 3, 4, 5],
        "{}",
        all_text(&presented)
    );
}

#[test]
fn statement_wrapped_in_a_new_branch_stays_visible_as_kept_text() {
    let before = concat!("fn alpha() {\n", "    gamma();\n", "}\n");
    let after = concat!(
        "fn alpha() {\n",
        "    if beta {\n",
        "        gamma();\n",
        "    }\n",
        "}\n",
    );
    let presented = diff("alpha.rs", before, after);
    assert!(
        !removed(&presented)
            .iter()
            .any(|line| line.contains("gamma();")),
        "{}",
        all_text(&presented)
    );
    assert!(
        added(&presented)
            .iter()
            .any(|line| line.contains("if beta {")),
        "{}",
        all_text(&presented)
    );
    assert!(
        rows(&presented).any(|row| matches!(
            row,
            Row::Reflow { after, .. } if after.iter().any(|line| line.text().contains("gamma();"))
        )),
        "{}",
        all_text(&presented)
    );
}

#[test]
fn reordered_multiline_arguments_keep_their_trailing_commas() {
    for (first, second) in [("gamma", "delta"), ("1", "2")] {
        let before =
            format!("fn alpha() {{\n    beta(\n        {first},\n        {second},\n    );\n}}\n");
        let after =
            format!("fn alpha() {{\n    beta(\n        {second},\n        {first},\n    );\n}}\n");
        let presented = diff("alpha.rs", &before, &after);
        let reordered = rows(&presented).any(|row| match row {
            Row::Moved { after, .. } | Row::Added(after) => {
                after.text().trim() == format!("{first},")
                    || after.text().trim() == format!("{second},")
            }
            _ => false,
        });
        assert!(reordered, "{}", all_text(&presented));
        for value in [first, second] {
            assert_eq!(
                current_texts(&presented)
                    .iter()
                    .filter(|line| line.trim() == format!("{value},"))
                    .count(),
                1,
                "{}",
                all_text(&presented)
            );
        }
    }
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
    let after = before.replace("Alpha", "Beta");
    let presented = diff("alpha.ts", before, &after);
    for payload in ["alpha: beta.string(),", "beta: beta.boolean(),", "})"] {
        assert!(
            has_context(&presented, payload),
            "{payload} not kept\n{}",
            all_text(&presented)
        );
    }
    let Some(Row::Changed { before, after }) =
        rows(&presented).find(|row| matches!(row, Row::Changed { .. }))
    else {
        panic!("{}", all_text(&presented));
    };
    assert_eq!(marked(before, DiffMark::Removed), ["Alpha"]);
    assert_eq!(marked(after, DiffMark::Added), ["Beta"]);
}

#[test]
fn new_function_is_added_whole_even_when_it_copies_an_old_body() {
    let before = concat!(
        "fn alpha() {\n",
        "    beta();\n",
        "    gamma(ALPHA);\n",
        "}\n"
    );
    let after = concat!(
        "fn alpha() {\n",
        "    delta();\n",
        "}\n",
        "fn delta() {\n",
        "    beta();\n",
        "    gamma(BETA);\n",
        "}\n",
    );
    let presented = diff("alpha.rs", before, after);
    assert_eq!(
        added_numbers(&presented),
        [2, 4, 5, 6, 7],
        "{}",
        all_text(&presented)
    );
    assert!(
        removed(&presented)
            .iter()
            .any(|line| line.contains("beta();")),
        "{}",
        all_text(&presented)
    );
    assert_eq!(
        current_texts(&presented)
            .iter()
            .filter(|line| line.contains("beta();"))
            .count(),
        1
    );
}

#[test]
fn extracted_c_body_is_removed_locally_and_the_new_function_is_fully_added() {
    let before = concat!(
        "static int alpha(struct Beta *beta)\n",
        "{\n",
        "    int gamma = beta->gamma;\n",
        "    gamma += delta(beta);\n",
        "    if (gamma < 0)\n",
        "        return -1;\n",
        "    beta->gamma = gamma;\n",
        "    return 0;\n",
        "}\n",
    );
    let after = concat!(
        "static int alpha(struct Beta *beta)\n",
        "{\n",
        "    if (epsilon(beta) < 0)\n",
        "        return -1;\n",
        "    return 0;\n",
        "}\n",
        "\n",
        "static int epsilon(struct Beta *beta)\n",
        "{\n",
        "    int gamma = beta->gamma;\n",
        "    gamma += delta(beta);\n",
        "    if (gamma < 0)\n",
        "        return -1;\n",
        "    beta->gamma = gamma;\n",
        "    return 0;\n",
        "}\n",
    );
    let presented = diff("alpha.c", before, after);
    let numbers = added_numbers(&presented);
    assert!(
        (8..=16).all(|line| numbers.contains(&line)),
        "{numbers:?}\n{}",
        all_text(&presented)
    );
    for payload in [
        "int gamma = beta->gamma;",
        "gamma += delta(beta);",
        "beta->gamma = gamma;",
    ] {
        assert!(
            removed(&presented)
                .iter()
                .any(|line| line.contains(payload)),
            "{payload}\n{}",
            all_text(&presented)
        );
    }
}

#[test]
fn added_nested_function_is_closed_to_sibling_body_anchors() {
    let before = concat!(
        "impl Alpha {\n",
        "    fn alpha() {\n",
        "        beta();\n",
        "        gamma(ALPHA);\n",
        "    }\n",
        "}\n",
    );
    let after = concat!(
        "impl Alpha {\n",
        "    fn alpha() {\n",
        "        delta();\n",
        "    }\n",
        "    fn delta() {\n",
        "        beta();\n",
        "        gamma(BETA);\n",
        "    }\n",
        "}\n",
    );
    let presented = diff("alpha.rs", before, after);
    assert_eq!(
        added_numbers(&presented),
        [3, 5, 6, 7, 8],
        "{}",
        all_text(&presented)
    );
    assert!(
        removed(&presented)
            .iter()
            .any(|line| line.contains("beta();"))
    );
}

#[test]
fn field_moved_to_another_structure_is_removed_and_added() {
    let before = concat!(
        "struct Alpha {\n",
        "    beta: Beta,\n",
        "    gamma: Alpha,\n",
        "}\n",
        "struct Delta {\n",
        "    epsilon: Epsilon,\n",
        "}\n",
    );
    let after = concat!(
        "struct Alpha {\n",
        "    gamma: Alpha,\n",
        "}\n",
        "struct Delta {\n",
        "    beta: Beta,\n",
        "    epsilon: Epsilon,\n",
        "}\n",
    );
    let presented = diff("alpha.rs", before, after);
    assert_eq!(
        removed(&presented),
        ["    beta: Beta,"],
        "{}",
        all_text(&presented)
    );
    assert_eq!(
        added(&presented),
        ["    beta: Beta,"],
        "{}",
        all_text(&presented)
    );
}

#[test]
fn multiline_wrapper_marks_only_the_wrapper() {
    let before = concat!("fn alpha() {\n", "    let beta = x;\n", "}\n");
    let after = concat!(
        "fn alpha() {\n",
        "    let beta = Some(\n",
        "        x\n",
        "    );\n",
        "}\n",
    );
    let presented = diff("alpha.rs", before, after);
    let added_marks = rows(&presented)
        .filter_map(|row| match row {
            Row::Added(line) => Some(marked(line, DiffMark::Added).concat()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(added_marks, ["Some(", "", ")"], "{}", all_text(&presented));
    let removed_marks = rows(&presented)
        .filter_map(|row| match row {
            Row::Removed(line) => Some(marked(line, DiffMark::Removed).concat()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(removed_marks, [""], "{}", all_text(&presented));
}

#[test]
fn plain_lines_align_by_longest_common_run_and_account_for_every_line() {
    let before = concat!(
        "static int alpha(void)\n",
        "{\n",
        "    beta();\n",
        "    while (gamma()) {\n",
        "        delta();\n",
        "    }\n",
        "    epsilon();\n",
        "    zeta();\n",
        "    eta();\n",
        "}\n",
    );
    let after = concat!(
        "static int alpha(void)\n",
        "{\n",
        "    theta();\n",
        "    zeta();\n",
        "    eta();\n",
        "}\n",
        "\n",
        "static int theta(void)\n",
        "{\n",
        "    beta();\n",
        "    while (gamma()) {\n",
        "        delta();\n",
        "    }\n",
        "    epsilon();\n",
        "}\n",
    );
    let presented = diff("alpha.txt", before, after);
    assert_eq!(
        removed(&presented),
        ["    zeta();", "    eta();"],
        "{}",
        all_text(&presented)
    );
    assert!(
        added(&presented)
            .iter()
            .any(|line| line.contains("theta();")),
        "{}",
        all_text(&presented)
    );
    for payload in ["beta();", "epsilon();"] {
        assert!(
            has_context(&presented, payload),
            "{payload}\n{}",
            all_text(&presented)
        );
    }
}

#[test]
fn structural_context_can_carry_a_hunk_to_eof() {
    let presented = diff(
        "alpha.rs",
        "fn run() { old(); }\n\n",
        "fn run() { new(); }\n\n",
    );
    let hunk = &presented.hunks[0];
    assert_eq!(
        hunk.excerpt.coverage.after,
        Some(LineSpan::new(
            LineNumber::new(1).unwrap(),
            LineNumber::new(3).unwrap()
        )),
        "{}",
        hunk_text(hunk)
    );
    assert!(
        matches!(hunk.excerpt.rows.iter().rev().nth(1), Some(Row::Context { after, .. }) if after.number.get() == 2 && after.text().is_empty()),
        "{}",
        hunk_text(hunk)
    );
    assert!(matches!(hunk.excerpt.rows.last(), Some(Row::FileEnd)));
}

#[test]
fn two_sided_hunk_uses_the_current_file_boundary() {
    let presented = diff(
        "alpha.rs",
        "fn run() { old(); }\n",
        "fn run() { new(); }\n\nfn later() {}\n",
    );
    assert_eq!(presented.hunks.len(), 1, "{}", all_text(&presented));
    let hunk = &presented.hunks[0];
    assert_eq!(
        hunk.excerpt.coverage.before,
        Some(LineSpan::new(
            LineNumber::new(1).unwrap(),
            LineNumber::new(2).unwrap()
        ))
    );
    assert_eq!(
        hunk.excerpt.coverage.after,
        Some(LineSpan::new(
            LineNumber::new(1).unwrap(),
            LineNumber::new(4).unwrap()
        ))
    );
    assert!(hunk_text(hunk).contains("fn later"));
    assert!(matches!(hunk.excerpt.rows.last(), Some(Row::FileEnd)));
}

#[test]
fn duplicate_definition_names_keep_one_to_one_correspondence() {
    let before = "impl Thing { fn first() { old(); } }\nimpl Thing { fn second() { stable(); } }\n";
    let after = "impl Thing { fn first() { new(); } }\nimpl Thing { fn second() { stable(); } }\n";
    let presented = diff("alpha.rs", before, after);
    assert_eq!(presented.hunks.len(), 1, "{}", all_text(&presented));
    assert!(added(&presented).iter().any(|line| line.contains("new")));
    assert!(
        has_context(&presented, "second"),
        "{}",
        all_text(&presented)
    );
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
    let presented = diff("alpha.tsx", before, after);
    for (payload, origin, destination) in [("<Composer />", 16, 5), ("<Moved />", 17, 16)] {
        let moves = rows(&presented)
            .filter(|row| matches!(row, Row::Moved { before, after, .. } if before.get() == origin && after.number.get() == destination && after.text().trim() == payload))
            .count();
        assert_eq!(
            moves,
            1,
            "{payload} must be one move\n{}",
            all_text(&presented)
        );
        assert!(
            !removed(&presented)
                .iter()
                .any(|line| line.contains(payload)),
            "{}",
            all_text(&presented)
        );
        assert!(
            !added(&presented).iter().any(|line| line.contains(payload)),
            "{}",
            all_text(&presented)
        );
        assert_eq!(
            current_texts(&presented)
                .iter()
                .filter(|line| line.trim() == payload)
                .count(),
            1,
            "{payload} rendered more than once\n{}",
            all_text(&presented)
        );
    }
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
    let presented = diff("alpha.tsx", before, after);
    assert_eq!(
        removed(&presented)
            .iter()
            .filter(|line| line.contains("<Show when={live}"))
            .count(),
        2,
        "{}",
        all_text(&presented)
    );
    assert_eq!(
        added(&presented)
            .iter()
            .filter(|line| line.contains("<Show when={live}"))
            .count(),
        1,
        "{}",
        all_text(&presented)
    );
    assert!(
        !rows(&presented)
            .any(|row| matches!(row, Row::Moved { after, .. } if after.text().contains("<Show"))),
        "{}",
        all_text(&presented)
    );
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
            "fn beta()"
        ),
    );
    let presented = diff("alpha.rs", before, &after);
    for needle in ["fn beta", "fn gamma"] {
        assert!(
            !removed(&presented)
                .iter()
                .chain(added(&presented).iter())
                .any(|line| line.contains(needle)),
            "stable neighbor {needle:?} must stay context or omitted\n{}",
            all_text(&presented)
        );
    }
    assert!(
        added(&presented)
            .iter()
            .any(|line| line.contains("fn delta")),
        "{}",
        all_text(&presented)
    );
}

#[test]
fn inserted_top_level_test_keeps_attributes_with_their_definitions() {
    let before = concat!(
        "#[test]\n",
        "fn alpha() { alpha(); }\n",
        "#[test]\n",
        "fn beta() { beta(); }\n"
    );
    let after = before.replace(
        "#[test]\nfn beta()",
        concat!(
            "#[test]\n",
            "fn gamma() { gamma(); }\n",
            "#[test]\n",
            "fn beta()"
        ),
    );
    let presented = diff("alpha.rs", before, &after);
    assert!(
        !removed(&presented)
            .iter()
            .chain(added(&presented).iter())
            .any(|line| line.contains("fn beta")),
        "{}",
        all_text(&presented)
    );
    assert_eq!(
        removed(&presented)
            .iter()
            .filter(|line| line.trim() == "#[test]")
            .count(),
        0,
        "{}",
        all_text(&presented)
    );
    assert_eq!(
        added(&presented)
            .iter()
            .filter(|line| line.trim() == "#[test]")
            .count(),
        1,
        "{}",
        all_text(&presented)
    );
}

#[test]
fn inserted_blank_layout_is_signal_without_flattening_its_definition() {
    let presented = diff(
        "alpha.rs",
        "fn run() { old(); }\n",
        "\nfn run() { new(); }\n",
    );
    assert!(
        rows(&presented).any(|row| matches!(row, Row::Added(line) if line.number.get() == 1 && line.text().is_empty())),
        "{}",
        all_text(&presented)
    );
    let definition = rows(&presented)
        .find_map(|row| match row {
            Row::Changed { after, .. } | Row::Added(after)
                if after.number.get() == 2 && after.text().contains("new") =>
            {
                Some(after)
            }
            _ => None,
        })
        .expect("edited definition remains visible");
    assert!(
        definition
            .spans
            .iter()
            .any(|span| span.highlight != Highlight::Plain)
    );
    assert!(
        definition
            .spans
            .iter()
            .any(|span| span.mark == DiffMark::Added && span.text.contains("new"))
    );
}

#[test]
fn rust_terminator_edits_stay_local_and_explicit() {
    for (before, after, expected) in [
        ("fn same() {}\n", "fn same() {}\r\n", LineEnding::CrLf),
        ("fn same() {}\n", "fn same() {}", LineEnding::Missing),
    ] {
        let presented = diff("alpha.rs", before, after);
        assert!(
            rows(&presented).any(|row| matches!(row, Row::Ending(EndingChange::Replaced { before, after }) if before.ending == LineEnding::Lf && after.ending == expected)),
            "{}",
            all_text(&presented)
        );
        assert!(
            matches!(presented.hunks[0].excerpt.rows.last(), Some(Row::FileEnd)),
            "{}",
            all_text(&presented)
        );
    }
}

#[test]
fn malformed_rust_falls_back_without_hiding_source_changes() {
    for (before, after) in [
        ("fn alpha(value: u32 {}\n", "fn alpha(value: u64 {}\n"),
        ("fn alpha(value: u32) {}\n", "fn alpha(value: u64 {}\n"),
        ("fn alpha(value: u32 {}\n", "fn alpha(value: u64) {}\n"),
    ] {
        let presented = diff("alpha.rs", before, after);
        assert_eq!(
            removed(&presented),
            [before.trim_end()],
            "{}",
            all_text(&presented)
        );
        assert_eq!(
            added(&presented),
            [after.trim_end()],
            "{}",
            all_text(&presented)
        );
        let plain = rows(&presented).all(|row| match row {
            Row::Removed(line) | Row::Added(line) => line
                .spans
                .iter()
                .all(|span| span.highlight == Highlight::Plain),
            Row::Changed { before, after } => before
                .spans
                .iter()
                .chain(&after.spans)
                .all(|span| span.highlight == Highlight::Plain),
            _ => true,
        });
        assert!(
            plain,
            "both revisions must use line syntax\n{}",
            all_text(&presented)
        );
    }
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
    let presented = diff("alpha.rs", before, &after);
    assert!(
        !rows(&presented)
            .any(|row| matches!(row, Row::Crumb { after, .. } if after.text().contains("impl<B"))),
        "{}",
        all_text(&presented)
    );
    assert_eq!(
        rows(&presented)
            .filter(
                |row| matches!(row, Row::Changed { after, .. } if after.text().contains("impl<B"))
            )
            .count(),
        1,
        "{}",
        all_text(&presented)
    );
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
        "        delta();\n",
        "    }\n",
        "}\n",
    );
    let after = before.replace("self.beta()", "self.gamma()");
    let presented = diff("alpha.rs", before, &after);
    assert_eq!(presented.hunks.len(), 1, "{}", all_text(&presented));
    let hunk = &presented.hunks[0];
    let text = hunk_text(hunk);
    assert!(
        hunk.excerpt
            .coverage
            .after
            .as_ref()
            .is_some_and(|coverage| coverage.end.get() <= 8),
        "{text}"
    );
    for payload in [
        "impl Alpha",
        "fn alpha",
        "let alpha = 1",
        "self.gamma()",
        "let gamma = 3",
    ] {
        assert!(text.contains(payload), "missing {payload:?}:\n{text}");
    }
    for payload in ["fn beta", "delta();"] {
        assert!(
            !text.contains(payload),
            "distant sibling {payload:?} leaked:\n{text}"
        );
    }
}

#[test]
fn adjacent_removed_definitions_keep_intervening_layout_in_source_order() {
    let before = concat!(
        "fn alpha() {}\n",
        "\n",
        "fn beta() {\n",
        "    gamma();\n",
        "}\n",
        "\n",
        "fn delta() {\n",
        "    epsilon();\n",
        "}\n",
        "\n",
        "fn zeta() {}\n",
    );
    for (before, after, removed_lines) in [
        (before, "fn alpha() {}\n\nfn zeta() {}\n", 3..11),
        (
            "\nfn alpha() {}\n\nfn beta() {}\n\nfn gamma() {}\n",
            "fn gamma() {}\n",
            1..6,
        ),
        (
            "fn alpha() {}\n\nfn beta() {}\n\nfn gamma() {}\n",
            "fn alpha() {}\n",
            2..6,
        ),
    ] {
        let presented = diff("alpha.rs", before, after);
        let numbers = rows(&presented)
            .filter_map(|row| match row {
                Row::Removed(line) => Some(line.number.get()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            numbers,
            removed_lines.collect::<Vec<_>>(),
            "{}",
            all_text(&presented)
        );
        for line in after.lines().filter(|line| !line.is_empty()) {
            assert!(
                has_context(&presented, line),
                "{line}\n{}",
                all_text(&presented)
            );
        }
    }
}

#[test]
fn separate_leaf_edits_stay_with_their_own_expressions() {
    let before = concat!(
        "fn alpha() {\n",
        "    beta(&[\"gamma\", \"ALPHA\"]);\n",
        "    beta(\n",
        "        &[\"gamma\", \"ALPHA\"],\n",
        "    );\n",
        "}\n",
    );
    let after = before.replace("ALPHA", "BETA");
    let presented = diff("alpha.rs", before, &after);
    for (line, suffix) in [(2, "]);"), (4, "],")] {
        let Some(Row::Changed { before, after }) = rows(&presented)
            .find(|row| matches!(row, Row::Changed { after, .. } if after.number.get() == line))
        else {
            panic!(
                "line {line} is not an inline edit\n{}",
                all_text(&presented)
            );
        };
        assert_eq!(
            marked(before, DiffMark::Removed),
            ["ALPHA"],
            "{}",
            all_text(&presented)
        );
        assert_eq!(
            marked(after, DiffMark::Added),
            ["BETA"],
            "{}",
            all_text(&presented)
        );
        for row in [before, after] {
            assert!(
                row.spans
                    .iter()
                    .any(|span| span.mark == DiffMark::Context && span.text.contains(suffix)),
                "{suffix} lost its context mark\n{}",
                all_text(&presented)
            );
        }
    }
}

#[test]
fn changed_chain_marks_the_inserted_link_and_keeps_outer_context() {
    let before = concat!(
        "fn alpha(beta: Beta) {\n",
        "    if beta.gamma() == \"alpha\" {\n",
        "        let delta = beta\n",
        "            .epsilon(\"beta\")\n",
        "            .map(|zeta| zeta.eta());\n",
        "        theta(delta);\n",
        "    }\n",
        "}\n",
    );
    let after = concat!(
        "fn alpha(beta: Beta) {\n",
        "    if beta.gamma() == \"alpha\" || gamma(&beta) {\n",
        "        let delta = beta\n",
        "            .epsilon(\"beta\")\n",
        "            .or_else(|| beta.epsilon(\"gamma\"))\n",
        "            .map(|zeta| zeta.eta());\n",
        "        theta(delta);\n",
        "    }\n",
        "}\n",
    );
    let presented = diff("alpha.rs", before, after);
    let Some(Row::Changed {
        after: condition, ..
    }) = rows(&presented)
        .find(|row| matches!(row, Row::Changed { after, .. } if after.number.get() == 2))
    else {
        panic!("{}", all_text(&presented));
    };
    assert!(
        marked(condition, DiffMark::Added)
            .concat()
            .contains("gamma(&beta)"),
        "{}",
        all_text(&presented)
    );
    assert!(
        has_context(&presented, ".epsilon(\"beta\")"),
        "{}",
        all_text(&presented)
    );
    assert!(
        !removed(&presented)
            .iter()
            .any(|line| line.contains(".epsilon(\"beta\")")),
        "{}",
        all_text(&presented)
    );
    assert_eq!(
        added(&presented)
            .iter()
            .filter(|line| line.contains("or_else"))
            .count(),
        1,
        "{}",
        all_text(&presented)
    );
    assert!(
        has_context(&presented, ".map(|zeta| zeta.eta());"),
        "{}",
        all_text(&presented)
    );
}

#[test]
fn swapped_calls_cannot_tunnel_between_function_owners() {
    let before = concat!(
        "fn alpha() {\n",
        "    gamma();\n",
        "}\n",
        "fn beta() {\n",
        "    delta();\n",
        "}\n"
    );
    let after = concat!(
        "fn alpha() {\n",
        "    delta();\n",
        "}\n",
        "fn beta() {\n",
        "    gamma();\n",
        "}\n"
    );
    let presented = diff("alpha.rs", before, after);
    for payload in ["gamma();", "delta();"] {
        assert!(
            removed(&presented)
                .iter()
                .any(|line| line.contains(payload)),
            "{}",
            all_text(&presented)
        );
        assert!(
            added(&presented).iter().any(|line| line.contains(payload)),
            "{}",
            all_text(&presented)
        );
    }
    assert!(
        !rows(&presented).any(|row| matches!(row, Row::Moved { .. })),
        "{}",
        all_text(&presented)
    );
}

#[test]
fn a_new_local_function_is_not_a_wrapper_around_an_old_statement() {
    let before = concat!("fn alpha() {\n", "    beta();\n", "}\n");
    let after = concat!(
        "fn alpha() {\n",
        "    fn gamma() {\n",
        "        beta();\n",
        "    }\n",
        "}\n"
    );
    let presented = diff("alpha.rs", before, after);
    assert_eq!(
        removed(&presented),
        ["    beta();"],
        "{}",
        all_text(&presented)
    );
    assert_eq!(
        added_numbers(&presented),
        [2, 3, 4],
        "{}",
        all_text(&presented)
    );
}

#[test]
fn an_inserted_sibling_cannot_consume_the_following_exact_sibling() {
    let cases = [
        (
            concat!("fn alpha() {\n", "    beta();\n", "}\n"),
            concat!("fn alpha() {\n", "    gamma();\n", "    beta();\n", "}\n"),
            "gamma();",
            "beta();",
        ),
        (
            concat!("fn alpha(\n", "    beta: Beta,\n", ") {}\n"),
            concat!(
                "fn alpha(\n",
                "    gamma: Gamma,\n",
                "    beta: Beta,\n",
                ") {}\n"
            ),
            "gamma: Gamma,",
            "beta: Beta,",
        ),
        (
            concat!(
                "fn alpha(beta: bool) {\n",
                "    if beta {\n",
                "        gamma();\n",
                "    } else {\n",
                "        delta();\n",
                "    }\n",
                "}\n",
            ),
            concat!(
                "fn alpha(beta: bool) {\n",
                "    if beta {\n",
                "        gamma();\n",
                "    } else {\n",
                "        epsilon();\n",
                "        delta();\n",
                "    }\n",
                "}\n",
            ),
            "epsilon();",
            "delta();",
        ),
    ];
    for (before, after, inserted, retained) in cases {
        let presented = diff("alpha.rs", before, after);
        assert!(
            added(&presented).iter().any(|line| line.contains(inserted)),
            "{}",
            all_text(&presented)
        );
        assert!(
            has_context(&presented, retained),
            "{}",
            all_text(&presented)
        );
        assert!(
            !removed(&presented)
                .iter()
                .any(|line| line.contains(retained)),
            "{}",
            all_text(&presented)
        );
        assert_eq!(
            current_texts(&presented)
                .iter()
                .filter(|line| line.contains(retained))
                .count(),
            1
        );
    }
}

#[test]
fn replaced_block_members_stay_inside_their_anonymous_block() {
    let before = concat!(
        "package main\n",
        "\n",
        "var x = 0\n",
        "\n",
        "const (\n",
        "\talpha = 1\n",
        "\tgamma = 3\n",
        ")\n"
    );
    let after = before.replace("alpha = 1", "delta = 4");
    let presented = diff("alpha.go", before, &after);
    let text = all_text(&presented);
    let opener = text.find("  const (").unwrap_or_else(|| panic!("{text}"));
    let removed_member = text
        .find("- \talpha = 1")
        .unwrap_or_else(|| panic!("{text}"));
    assert!(opener < removed_member, "{text}");
    assert!(has_context(&presented, "gamma = 3"), "{text}");
}

#[test]
fn unchanged_anonymous_sibling_keeps_its_partner_over_a_nearby_edit() {
    let before = concat!(
        "package main\n",
        "\n",
        "func run(sc *ctx) error {\n",
        "\tquickTunnelURL := data.Result.Hostname\n",
        "\tif !strings.HasPrefix(quickTunnelURL, \"https://\") {\n",
        "\t\tquickTunnelURL = \"https://\" + quickTunnelURL\n",
        "\t}\n",
        "\n",
        "\tif !sc.c.IsSet(flags.Protocol) {\n",
        "\t\t_ = sc.c.Set(flags.Protocol, \"quic\")\n",
        "\t}\n",
        "\treturn StartServer(sc)\n",
        "}\n",
    );
    let after = concat!(
        "package main\n",
        "\n",
        "func run(sc *ctx) error {\n",
        "\tquickTunnelURL := normalizeQuickTunnelURL(data.Result.Hostname)\n",
        "\n",
        "\tif !sc.c.IsSet(flags.Protocol) {\n",
        "\t\t_ = sc.c.Set(flags.Protocol, \"quic\")\n",
        "\t}\n",
        "\treturn StartServer(sc)\n",
        "}\n",
    );
    let presented = diff("alpha.go", before, after);
    assert!(
        has_context(&presented, "if !sc.c.IsSet(flags.Protocol) {"),
        "{}",
        all_text(&presented)
    );
    assert!(
        has_context(&presented, "_ = sc.c.Set(flags.Protocol, \"quic\")"),
        "{}",
        all_text(&presented)
    );
    assert!(
        removed(&presented)
            .iter()
            .any(|line| line.contains("strings.HasPrefix")),
        "{}",
        all_text(&presented)
    );
}

#[test]
fn edits_moves_wiring_and_reflow_rank_in_that_order() {
    let separation = "const ALPHA_0: u8 = 0;\nconst ALPHA_1: u8 = 0;\nconst ALPHA_2: u8 = 0;\nconst ALPHA_3: u8 = 0;\nconst ALPHA_4: u8 = 0;\nconst ALPHA_5: u8 = 0;\nconst ALPHA_6: u8 = 0;\nconst ALPHA_7: u8 = 0;\n";
    let before = format!(
        "use alpha::Alpha;\nmod alpha;\n{separation}fn beta() {{ beta(); }}\n{separation}fn gamma() -> u8 {{ 1 }}\n{separation}fn delta() {{ alpha(); }}\n{separation}fn epsilon() {{}}\n"
    );
    let after = format!(
        "use beta::Beta;\nmod alpha;\nmod beta;\n{separation}{separation}fn gamma() -> u8 {{\n    1\n}}\n{separation}fn delta() {{ beta(); }}\n{separation}fn epsilon() {{}}\nfn beta() {{ beta(); }}\n"
    );
    let presented = diff("alpha.rs", &before, &after);
    let natures = presented
        .hunks
        .iter()
        .map(|hunk| hunk.excerpt.priority)
        .collect::<Vec<_>>();
    for priority in [
        Priority::Edit,
        Priority::Move,
        Priority::Wiring,
        Priority::Reflow,
    ] {
        assert!(
            natures.contains(&priority),
            "{natures:?}\n{}",
            all_text(&presented)
        );
    }
    assert!(
        natures.windows(2).all(|pair| pair[0] >= pair[1]),
        "{natures:?}"
    );
}
