use super::*;

#[test]
fn insignificant_layout_reflows_without_changing_payload() {
    for (path, before, after) in [
        (
            "alpha.nix",
            "{alpha=[1 2];}\n",
            "{\n  alpha = [\n    1\n    2\n  ];\n}\n",
        ),
        ("alpha.nix", "\"a ${beta} b\"\n", "\"a ${ beta } b\"\n"),
        (
            "alpha.toml",
            "[alpha]\nbeta=[1,2]\n",
            "[ alpha ]\nbeta = [\n  1,\n  2\n]\n",
        ),
        (
            "alpha.json",
            "{\"alpha\":[1,2]}\n",
            "{\n  \"alpha\": [1, 2]\n}\n",
        ),
        (
            "alpha.cpp",
            "template<class T> T alpha(T beta) { return beta; }\n",
            "template<class T>\nT alpha(T beta) {\n\treturn beta;\n}\n",
        ),
        (
            "alpha.rs",
            "fn alpha() { beta(); }\n",
            "fn alpha() {\n\tbeta();\n}\n",
        ),
        (
            "alpha.c",
            "int alpha() { return 1; }\n",
            "int alpha() {\n\treturn 1;\n}\n",
        ),
        (
            "alpha.js",
            "function alpha() { beta(); }\n",
            "function alpha() {\n\tbeta();\n}\n",
        ),
        (
            "alpha.go",
            "package alpha\nfunc beta() int { return 1 }\n",
            "package alpha\nfunc beta() int {\n\treturn 1\n}\n",
        ),
    ] {
        let diff = diff_file(path, before, after).expect("source must diff");
        let rows = diff.hunks.iter().flat_map(|hunk| &hunk.rows);
        assert!(
            rows.clone().any(|row| matches!(row, ReviewRow::Reflow(_))),
            "{diff:#?}"
        );
        assert!(
            rows.clone().all(|row| match row {
                ReviewRow::Current(line) | ReviewRow::Reflow(line) => !line.has_changes(),
                ReviewRow::Elision(_) | ReviewRow::FileBoundary => true,
                _ => false,
            }),
            "layout changed payload: {diff:#?}"
        );
        assert_source_ownership(&diff);
    }
}

#[test]
fn literal_whitespace_is_not_layout() {
    for (path, before, after) in [
        ("alpha.nix", "\"a ${beta} b\"\n", "\"a  ${beta} b\"\n"),
        ("alpha.nix", "''\n  a\n  b\n''\n", "''\n  a\n    b\n''\n"),
        ("alpha.toml", "alpha = '''a b'''\n", "alpha = '''a  b'''\n"),
        ("alpha.toml", "\"a b\" = 1\n", "\"a  b\" = 1\n"),
        (
            "alpha.json",
            "{\"alpha\":\"a b\"}\n",
            "{\"alpha\":\"a  b\"}\n",
        ),
        (
            "alpha.cpp",
            "const char* alpha = R\"(a b)\";\n",
            "const char* alpha = R\"(a  b)\";\n",
        ),
        (
            "alpha.go",
            "package alpha\nvar beta = `a b`\n",
            "package alpha\nvar beta = `a  b`\n",
        ),
    ] {
        let diff = diff_file(path, before, after).expect("source must diff");
        assert!(
            source_lines(&diff).any(|(line, _)| line.has_changes()),
            "literal edit disappeared: {diff:#?}"
        );
        assert!(
            !diff
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.rows)
                .any(|row| matches!(row, ReviewRow::Reflow(_))),
            "literal edit became reflow: {diff:#?}"
        );
        assert_source_ownership(&diff);
    }
}

#[test]
fn configuration_values_stay_with_their_named_owner() {
    for (path, before, after) in [
        (
            "alpha.nix",
            "{\n  alpha = { value = 1; };\n  beta = { value = 2; };\n}\n",
            "{\n  beta = { value = 3; };\n  alpha = { value = 1; };\n}\n",
        ),
        (
            "alpha.nix",
            "let\n  alpha = 1;\n  beta = 2;\nin alpha + beta\n",
            "let\n  beta = 3;\n  alpha = 1;\nin alpha + beta\n",
        ),
        (
            "alpha.json",
            "{\n  \"alpha\": {\"value\": 1},\n  \"beta\": {\"value\": 2}\n}\n",
            "{\n  \"beta\": {\"value\": 3},\n  \"alpha\": {\"value\": 1}\n}\n",
        ),
        (
            "alpha.toml",
            "[alpha]\nvalue = 1\n\n[beta]\nvalue = 2\n",
            "[beta]\nvalue = 3\n\n[alpha]\nvalue = 1\n",
        ),
    ] {
        let diff = diff_file(path, before, after).expect("configuration must diff");
        assert_removed_payload(&diff, "2");
        assert_added_payload(&diff, "3");
        assert_no_removed_payload(&diff, "1");
        assert_context_payload(&diff, "1");
        assert_source_ownership(&diff);
    }
}

#[test]
fn configuration_list_order_remains_a_material_change() {
    for (path, before, after) in [
        ("alpha.nix", "[1 2 3]\n", "[2 1 3]\n"),
        ("alpha.json", "[1, 2, 3]\n", "[2, 1, 3]\n"),
        ("alpha.toml", "alpha = [1, 2, 3]\n", "alpha = [2, 1, 3]\n"),
        (
            "alpha.toml",
            "[[alpha]]\nvalue = 1\n[[alpha]]\nvalue = 2\n",
            "[[alpha]]\nvalue = 2\n[[alpha]]\nvalue = 1\n",
        ),
    ] {
        let diff = diff_file(path, before, after).expect("configuration must diff");
        assert!(
            source_lines(&diff).any(|(line, _)| line.has_changes())
                || diff
                    .hunks
                    .iter()
                    .flat_map(|hunk| &hunk.rows)
                    .any(|row| matches!(row, ReviewRow::Moved { .. })),
            "list order disappeared: {diff:#?}"
        );
        assert_source_ownership(&diff);
    }
}

#[test]
fn cpp_overloads_keep_their_signatures_when_reordered_and_edited() {
    let before = "int alpha(int beta) { return 1; }\n\nint alpha(double beta) { return 2; }\n";
    let after = "int alpha(double beta) { return 3; }\n\nint alpha(int beta) { return 1; }\n";
    for path in ["alpha.cpp", "alpha.h"] {
        let diff = diff_file(path, before, after).expect("C++ must diff");
        assert_removed_payload(&diff, "2");
        assert_added_payload(&diff, "3");
        assert_no_removed_payload(&diff, "1");
        assert!(
            diff.hunks
                .iter()
                .flat_map(|hunk| &hunk.rows)
                .any(|row| matches!(row,
                    ReviewRow::Moved { before: Some(1), after } if line_text(after).contains("int beta")
                )),
            "{diff:#?}"
        );
        assert_source_ownership(&diff);
    }
}

#[test]
fn go_methods_keep_receiver_identity_when_reordered_and_edited() {
    let before = "package alpha\n\nfunc (b Beta) Value() int { return 1 }\n\nfunc (g Gamma) Value() int { return 2 }\n";
    let after = "package alpha\n\nfunc (g Gamma) Value() int { return 3 }\n\nfunc (b Beta) Value() int { return 1 }\n";
    let diff = diff_file("alpha.go", before, after).expect("Go must diff");
    assert_removed_payload(&diff, "2");
    assert_added_payload(&diff, "3");
    assert_no_removed_payload(&diff, "1");
    assert!(
        diff.hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .any(|row| matches!(row,
                ReviewRow::Moved { before: Some(3), after } if line_text(after).contains("Beta")
            )),
        "{diff:#?}"
    );
    assert_source_ownership(&diff);
}

#[test]
fn python_layout_changes_preserve_the_same_block_structure() {
    for (before, after) in [
        ("def alpha(): pass\n", "def alpha():\n\tpass\n"),
        (
            "def alpha(): return f'{beta+gamma}'\n",
            "def alpha(): return f'{ beta + gamma }'\n",
        ),
        ("def alpha():\n    return 1\n", "def alpha():\n\treturn 1\n"),
        (
            "def alpha():\n    if beta:\n        gamma()\n    delta()\n",
            "def alpha():\n  if beta:\n    gamma()\n  delta()\n",
        ),
        (
            "def alpha():\n    return (1 + 2)\n",
            "def alpha():\n    return (1\n      + 2)\n",
        ),
        (
            "def alpha():\n    return 1 + 2\n",
            "def alpha():\n    return 1 + \\\n        2\n",
        ),
    ] {
        for (before, after) in [(before, after), (after, before)] {
            let diff = diff_file("alpha.py", before, after).expect("Python must diff");
            let rows = diff.hunks.iter().flat_map(|hunk| &hunk.rows);
            assert!(
                rows.clone().any(|row| matches!(row, ReviewRow::Reflow(_))),
                "{diff:#?}"
            );
            assert!(
                rows.clone().all(|row| match row {
                    ReviewRow::Current(line) | ReviewRow::Reflow(line) => !line.has_changes(),
                    ReviewRow::Elision(_) | ReviewRow::FileBoundary => true,
                    _ => false,
                }),
                "layout changed payload: {diff:#?}"
            );
            assert_source_ownership(&diff);
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
            let diff = diff_file("alpha.py", before, after).expect("Python must diff");
            assert!(
                diff.hunks
                    .iter()
                    .flat_map(|hunk| &hunk.rows)
                    .any(|row| match row {
                        ReviewRow::Current(line) =>
                            line_text(line).contains(affected) && line.has_changes(),
                        ReviewRow::Removed(line)
                        | ReviewRow::Added(line)
                        | ReviewRow::Moved { after: line, .. } =>
                            line_text(line).contains(affected),
                        _ => false,
                    }),
                "scope change disappeared into reflow: {diff:#?}"
            );
            assert_source_ownership(&diff);
        }
    }
}

#[test]
fn python_decorated_definitions_move_with_their_decorators() {
    let alpha = "@beta\ndef alpha():\n    return 1\n";
    let gamma = "@delta\ndef gamma():\n    return 2\n";
    let before = format!("{alpha}\n{gamma}");
    let after = format!("{gamma}\n{alpha}");
    let diff = diff_file("alpha.py", &before, &after).expect("Python must diff");
    assert!(
        diff.hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .any(|row| matches!(row,
                ReviewRow::Moved { before: Some(_), after } if line_text(after).starts_with('@')
            )),
        "{diff:#?}"
    );
    assert!(
        !source_lines(&diff).any(|(line, _)| line.has_changes()),
        "movement changed payload: {diff:#?}"
    );
    assert_source_ownership(&diff);
}

#[test]
fn python_literal_and_debug_fstring_whitespace_remains_material() {
    for (before, after) in [
        (
            "def alpha(): return 'a b'\n",
            "def alpha(): return 'a  b'\n",
        ),
        (
            "def alpha():\n    return '''a\n    b'''\n",
            "def alpha():\n    return '''a\n        b'''\n",
        ),
        (
            "def alpha(): return f'{beta=}'\n",
            "def alpha(): return f'{ beta = }'\n",
        ),
    ] {
        let diff = diff_file("alpha.py", before, after).expect("Python must diff");
        assert!(
            source_lines(&diff).any(|(line, _)| line.has_changes()),
            "literal edit disappeared: {diff:#?}"
        );
        assert!(
            !diff
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.rows)
                .any(|row| matches!(row, ReviewRow::Reflow(_))),
            "literal edit became reflow: {diff:#?}"
        );
        assert_source_ownership(&diff);
    }
}

#[test]
fn overlapping_syntax_on_one_line_has_one_review_owner() {
    let before = "#![allow(alpha)]// beta\nconst ALPHA: u8 = 1;\n";
    let after = "#![allow(alpha)]// beta\nconst ALPHA: u8 = 2;\n";

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_context_payload(&diff, "#![allow(alpha)]// beta");
    assert_added_payload(&diff, "2");
    assert_removed_payload(&diff, "1");
    assert_source_ownership(&diff);
}

#[test]
fn decorated_module_insertion_preserves_an_unchanged_boolean_chain() {
    // Four operands retain the nested repeated wrapper that exposed this boundary conflict.
    let stable = concat!(
        "fn alpha(beta: &str) -> bool {\n",
        "    let gamma = beta.starts_with(\"alpha\")\n",
        "        || beta.starts_with(\"beta\")\n",
        "        || beta.starts_with(\"gamma\")\n",
        "        || beta.starts_with(\"delta\");\n",
        "    if !gamma {\n",
        "        return false;\n",
        "    }\n",
        "    true\n",
        "}\n",
    );
    let before = format!("{stable}#[cfg(test)]\nmod alpha;\n");
    let after = format!("{stable}#[cfg(test)]\nmod beta;\n#[cfg(test)]\nmod alpha;\n");
    let inserted = stable.lines().count() + 1;

    let diff = diff_file("alpha.rs", &before, &after).expect("Rust must diff");

    assert_added_subtree(&diff, inserted..inserted + 2);
    for number in inserted + 2..=inserted + 3 {
        assert!(
            source_lines(&diff)
                .any(|(line, current)| { current && line.number == number && !line.has_changes() }),
            "existing decorated module line {number} lost its stable scope: {diff:#?}"
        );
    }
    assert_eq!(current_payload_count(&diff, "#[cfg(test)]"), 2, "{diff:#?}");
    assert_current_payload_once(&diff, "mod alpha;");
    assert_no_removed_payload(&diff, "mod alpha;");
    assert_source_ownership(&diff);
}

#[test]
fn extracted_multiline_expression_keeps_its_removed_syntax_complete() {
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

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_added_subtree(&diff, 4..10);
    for payload in [
        "let gamma = beta + 1;",
        "(gamma > ALPHA)",
        "&& delta(gamma)",
        "&& epsilon(beta)",
    ] {
        assert_removed_payload(&diff, payload);
    }
    assert_current_payload_once(&diff, "let gamma = beta + 1;");
    assert_source_ownership(&diff);
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

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_added_payload(&diff, "beta(epsilon)");
    assert_added_subtree(&diff, 4..11);
    for payload in ["beta(", "alpha,", "gamma,", "delta,", ");"] {
        assert_removed_payload(&diff, payload);
    }
    assert_current_payload_once(&diff, "gamma,");
    assert_source_ownership(&diff);
}

#[test]
fn multiline_expansion_replaces_a_complete_parseable_statement() {
    let before = concat!(
        "fn alpha() {\n",
        "    let beta = gamma(delta, epsilon);\n",
        "}\n",
    );
    let after = concat!(
        "fn alpha() {\n",
        "    let beta = gamma(\n",
        "        delta,\n",
        "        zeta,\n",
        "    );\n",
        "}\n",
    );

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert!(
        diff.hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
            matches!(
                row,
                ReviewRow::Removed(line) if line.number == 2
                    && line.spans.iter().all(|span| span.mark == DiffMark::Removed)
            )
        }),
        "the old statement must remain one complete removal: {diff:#?}"
    );
    assert_added_subtree(&diff, 2..6);
    assert_source_ownership(&diff);
}

#[test]
fn multiline_wrapper_retains_the_complete_nested_statement() {
    let before = concat!("fn alpha() {\n", "    gamma();\n", "}\n");
    let after = concat!(
        "fn alpha() {\n",
        "    if beta {\n",
        "        gamma();\n",
        "    }\n",
        "}\n",
    );

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert!(
        !source_lines(&diff).any(|(line, current)| !current && line.number == 2),
        "a certified wrapper left an old statement ghost: {diff:#?}"
    );
    assert_line_fragment_mark(&diff, "gamma();", "gamma();", DiffMark::Context);
    assert_added_payload(&diff, "if beta {");
    assert_source_ownership(&diff);
}

#[test]
fn reordered_multiline_leaf_arguments_keep_their_trailing_commas() {
    for (first, second) in [("gamma", "delta"), ("1", "2")] {
        let before =
            format!("fn alpha() {{\n    beta(\n        {first},\n        {second},\n    );\n}}\n");
        let after =
            format!("fn alpha() {{\n    beta(\n        {second},\n        {first},\n    );\n}}\n");

        let diff = diff_file("alpha.rs", &before, &after).expect("Rust must diff");

        for value in [first, second] {
            let payload = format!("{value},");
            let lines = source_lines(&diff)
                .filter(|(line, _)| line_text(line).trim() == payload)
                .collect::<Vec<_>>();
            assert!(!lines.is_empty(), "missing argument {payload:?}: {diff:#?}");
            for (line, _) in lines {
                let owner = line
                    .spans
                    .iter()
                    .find(|span| span.text.contains(value))
                    .expect("the argument line contains its semantic owner");
                let delimiter = line
                    .spans
                    .iter()
                    .find(|span| span.text.contains(','))
                    .expect("the argument line contains its trailing comma");
                assert_eq!(
                    owner.mark, delimiter.mark,
                    "trailing comma split from {value:?}: {diff:#?}"
                );
            }
            assert_current_payload_once(&diff, &payload);
        }
        assert!(
            [first, second].into_iter().any(|value| {
                let payload = format!("{value},");
                source_lines(&diff).any(|(line, current)| {
                    !current && line_has_marked_payload(line, &payload, DiffMark::Removed)
                }) && source_lines(&diff).any(|(line, current)| {
                    current && line_has_marked_payload(line, &payload, DiffMark::Added)
                })
            }),
            "the reordered argument was not shown atomically: {diff:#?}"
        );
        assert_source_ownership(&diff);
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
    let after = concat!(
        "const Beta = beta.object({\n",
        "    alpha: beta.string(),\n",
        "    beta: beta.boolean(),\n",
        "})\n",
        "\n",
        "consume(Beta)\n",
    );

    let diff = diff_file("alpha.ts", before, after).expect("TypeScript must diff");

    for payload in ["alpha: beta.string(),", "beta: beta.boolean(),", "})"] {
        assert_context_payload(&diff, payload);
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

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    for payload in ["const ALPHA: &str", "alpha", "gamma", "\"#;"] {
        assert_context_payload(&diff, payload);
        assert_current_payload_once(&diff, payload);
    }
    assert_added_payload(&diff, "del");
    assert_removed_payload(&diff, "be");
    assert_source_ownership(&diff);
}

#[test]
fn stylesheet_edit_keeps_the_rule_frame_as_context() {
    let before = concat!(".alpha {\n", "  color: alpha;\n", "  margin: 0;\n", "}\n",);
    let after = concat!(".alpha {\n", "  color: beta;\n", "  margin: 0;\n", "}\n",);

    let diff = diff_file("alpha.css", before, after).expect("CSS must diff");

    for payload in [".alpha {", "margin: 0;", "}"] {
        assert_context_payload(&diff, payload);
        assert_current_payload_once(&diff, payload);
    }
    assert_added_payload(&diff, "bet");
    assert_removed_payload(&diff, "alph");
    assert_source_ownership(&diff);
}

#[test]
fn edited_reparented_subtree_preserves_its_unchanged_child() {
    let before = concat!("<main>\n", "  <section>alpha</section>\n", "</main>\n",);
    let after = "<section class=\"beta\">alpha</section>\n";

    let diff = diff_file("alpha.html", before, after).expect("HTML must diff");

    assert_context_payload(&diff, "alpha");
    assert_current_payload_once(&diff, "alpha</section>");
    assert_added_payload(&diff, "class=\"beta\"");
    assert_removed_payload(&diff, "<main>");
    assert!(
        diff.hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
            matches!(
                row,
                ReviewRow::Current(line)
                    if line.has_changes() && line_text(line).contains("alpha</section>")
            )
        }),
        "the retained subtree belongs to one mixed-mark current row: {diff:#?}"
    );
    assert!(
        !diff.hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
            matches!(
                row,
                ReviewRow::Removed(line)
                    if line_text(line).contains("alpha</section>")
            )
        }),
        "retained payload must not leave an old-world ghost: {diff:#?}"
    );
    assert_source_ownership(&diff);
}

#[test]
fn added_function_is_closed_to_anchors_from_an_existing_function() {
    let before = concat!(
        "fn alpha() {\n",
        "    beta();\n",
        "    gamma(ALPHA);\n",
        "}\n",
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

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_line_predecessor(&diff, 3, 2, "gamma(ALPHA)", "delta();");
    assert_added_subtree(&diff, 4..8);
    assert_current_payload_once(&diff, "beta();");
    assert_removed_payload(&diff, "beta();");
    assert_source_ownership(&diff);
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

    let diff = diff_file("alpha.c", before, after).expect("C must diff");

    assert_added_subtree(&diff, 8..17);
    for payload in [
        "int gamma = beta->gamma;",
        "gamma += delta(beta);",
        "beta->gamma = gamma;",
    ] {
        assert_removed_payload(&diff, payload);
    }
    assert_current_payload_once(&diff, "gamma += delta(beta);");
    assert_source_ownership(&diff);
}

#[test]
fn line_fallback_prefers_nearby_survivors_to_a_farther_copied_run() {
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

    let diff = diff_file("alpha.txt", before, after).expect("line fallback must diff");

    assert_added_subtree(&diff, 8..16);
    for payload in ["beta();", "while (gamma())", "delta();", "epsilon();"] {
        assert_removed_payload(&diff, payload);
        assert_current_payload_once(&diff, payload);
    }
    for (payload, before_line) in [("zeta();", 8), ("eta();", 9)] {
        assert_context_payload(&diff, payload);
        assert!(
            !source_lines(&diff).any(|(line, current)| !current && line.number == before_line),
            "a nearby survivor was removed in favor of a farther copy: {diff:#?}"
        );
    }
    assert_source_ownership(&diff);
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

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_line_predecessor(&diff, 4, 3, "gamma(ALPHA)", "delta();");
    assert_added_subtree(&diff, 5..9);
    assert_current_payload_once(&diff, "beta();");
    assert_removed_payload(&diff, "beta();");
    assert_source_ownership(&diff);
}

#[test]
fn field_added_to_another_structure_cannot_borrow_its_old_owner() {
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

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_added_payload(&diff, "beta: Beta");
    assert_removed_payload(&diff, "beta: Beta");
    assert_added_subtree(&diff, 5..6);
    assert_context_payload(&diff, "epsilon: Epsilon");
    assert_current_payload_once(&diff, "beta: Beta");
    assert_source_ownership(&diff);
}

#[test]
fn field_edit_stays_inside_its_structure() {
    let before = concat!("struct Alpha {\n", "    beta: Alpha,\n", "}\n");
    let after = concat!("struct Alpha {\n", "    beta: Beta,\n", "}\n");

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_removed_payload(&diff, "Alph");
    assert_added_payload(&diff, "Bet");
    assert_context_payload(&diff, "struct Alpha");
    assert_line_fragment_mark(&diff, "beta: Beta", "beta", DiffMark::Context);
    assert_source_ownership(&diff);
}

#[test]
fn copied_body_cannot_choose_a_later_renamed_function() {
    let before = concat!("fn alpha() {\n", "    beta();\n", "}\n");
    let after = concat!(
        "fn gamma() {\n",
        "    delta();\n",
        "}\n",
        "fn delta() {\n",
        "    beta();\n",
        "}\n",
    );

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_added_subtree(&diff, 4..7);
    assert_current_payload_once(&diff, "beta();");
    assert_removed_payload(&diff, "beta();");
    assert_removed_payload(&diff, "alpha");
    assert_added_payload(&diff, "gamma");
    assert_source_ownership(&diff);
}

#[test]
fn a_nested_owner_cannot_veto_its_containing_sibling_match() {
    let before = concat!(
        "mod alpha {\n",
        "    fn beta() {\n",
        "        fn delta() {}\n",
        "    }\n",
        "}\n",
    );
    let after = concat!(
        "mod alpha {\n",
        "    fn epsilon() {}\n",
        "    fn delta() {}\n",
        "}\n",
    );

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_removed_payload(&diff, "beta");
    assert_added_payload(&diff, "epsilon");
    assert_removed_payload(&diff, "fn delta() {}");
    assert_added_payload(&diff, "fn delta() {}");
    assert_source_ownership(&diff);
}

#[test]
fn copied_closure_payload_cannot_steal_the_local_closure() {
    let before = concat!(
        "fn alpha() {\n",
        "    consume(\n",
        "        || gamma(ALPHA),\n",
        "    );\n",
        "}\n",
    );
    let after = concat!(
        "fn alpha() {\n",
        "    consume(\n",
        "        || gamma(BETA),\n",
        "        || gamma(ALPHA),\n",
        "    );\n",
        "}\n",
    );

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_removed_payload(&diff, "ALPH");
    assert_added_payload(&diff, "BET");
    assert_line_fragment_mark(&diff, "gamma(BETA)", "gamma", DiffMark::Context);
    assert_added_subtree(&diff, 4..5);
    assert_current_payload_once(&diff, "gamma(ALPHA)");
    assert_source_ownership(&diff);
}

#[test]
fn sibling_callable_bodies_pair_locally_before_their_payloads() {
    let cases = [
        (
            "alpha.rs",
            concat!(
                "fn alpha() {\n",
                "    let beta = [\n",
                "        || { gamma(); },\n",
                "        || {},\n",
                "    ];\n",
                "}\n",
            ),
            concat!(
                "fn alpha() {\n",
                "    let beta = [\n",
                "        || {},\n",
                "        || { gamma(); },\n",
                "    ];\n",
                "}\n",
            ),
        ),
        (
            "alpha.ts",
            concat!(
                "const alpha = [\n",
                "    () => { gamma(); },\n",
                "    () => {},\n",
                "];\n",
            ),
            concat!(
                "const alpha = [\n",
                "    () => {},\n",
                "    () => { gamma(); },\n",
                "];\n",
            ),
        ),
        (
            "alpha.ts",
            concat!(
                "const alpha = [\n",
                "    beta => { gamma(); },\n",
                "    beta => {},\n",
                "];\n",
            ),
            concat!(
                "const alpha = [\n",
                "    beta => {},\n",
                "    beta => { gamma(); },\n",
                "];\n",
            ),
        ),
    ];

    for (path, before, after) in cases {
        let diff = diff_file(path, before, after).expect("callable bodies must diff");

        assert_removed_payload(&diff, "gamma();");
        assert_added_payload(&diff, "gamma();");
        assert_source_ownership(&diff);
    }
}

#[test]
fn repeated_named_owners_pair_fifo_before_their_bodies() {
    let before = concat!(
        "mod alpha {\n",
        "    impl Beta { fn gamma() { delta(); } }\n",
        "    impl Beta {}\n",
        "}\n",
    );
    let after = concat!(
        "mod alpha {\n",
        "    impl Beta {}\n",
        "    impl Beta { fn gamma() { delta(); } }\n",
        "}\n",
    );

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_removed_payload(&diff, "fn gamma() { delta(); }");
    assert_added_payload(&diff, "fn gamma() { delta(); }");
    assert_source_ownership(&diff);
}

#[test]
fn copied_object_payload_cannot_steal_the_local_object() {
    let before = concat!("const alpha = [\n", "  { beta: ALPHA },\n", "];\n",);
    let after = concat!(
        "const alpha = [\n",
        "  { beta: BETA },\n",
        "  { beta: ALPHA },\n",
        "];\n",
    );

    let diff = diff_file("alpha.ts", before, after).expect("TypeScript must diff");

    assert_removed_payload(&diff, "ALPH");
    assert_added_payload(&diff, "BET");
    assert_line_fragment_mark(&diff, "beta: BETA", "beta", DiffMark::Context);
    assert_added_subtree(&diff, 3..4);
    assert_current_payload_once(&diff, "beta: ALPHA");
    assert_source_ownership(&diff);
}

#[test]
fn sibling_object_fields_cannot_choose_another_object_occurrence() {
    let before = concat!(
        "const alpha = [\n",
        "    { beta: gamma() },\n",
        "    { beta: null },\n",
        "];\n",
    );
    let after = concat!(
        "const alpha = [\n",
        "    { beta: null },\n",
        "    { beta: gamma() },\n",
        "];\n",
    );

    let diff = diff_file("alpha.ts", before, after).expect("TypeScript must diff");

    for payload in ["gamma", "null"] {
        assert_removed_payload(&diff, payload);
        assert_added_payload(&diff, payload);
    }
    assert_source_ownership(&diff);
}

#[test]
fn copied_if_payload_cannot_steal_the_local_branch() {
    let before = concat!(
        "fn alpha() {\n",
        "    if beta() {\n",
        "        gamma(ALPHA);\n",
        "    }\n",
        "}\n",
    );
    let after = concat!(
        "fn alpha() {\n",
        "    if beta() {\n",
        "        gamma(BETA);\n",
        "    }\n",
        "    if delta() {\n",
        "        gamma(ALPHA);\n",
        "    }\n",
        "}\n",
    );

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_removed_payload(&diff, "ALPH");
    assert_added_payload(&diff, "BET");
    assert_line_fragment_mark(&diff, "gamma(BETA)", "gamma", DiffMark::Context);
    assert_added_subtree(&diff, 5..8);
    assert_current_payload_once(&diff, "gamma(ALPHA)");
    assert_source_ownership(&diff);
}

#[test]
fn repeated_conditions_cannot_let_branch_payload_choose_their_occurrence() {
    let before = concat!(
        "fn alpha() {\n",
        "    if beta() { gamma(); }\n",
        "    if beta() {}\n",
        "}\n",
    );
    let after = concat!(
        "fn alpha() {\n",
        "    if beta() {}\n",
        "    if beta() { gamma(); }\n",
        "}\n",
    );

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_removed_payload(&diff, "gamma();");
    assert_added_payload(&diff, "gamma();");
    assert_source_ownership(&diff);
}

#[test]
fn decorated_payload_cannot_cross_into_an_inserted_branch() {
    let before = concat!("fn alpha() {\n", "    //! gamma\n", "    beta();\n", "}\n",);
    let after = concat!(
        "fn alpha() {\n",
        "    if true {\n",
        "        //! gamma\n",
        "        beta();\n",
        "    }\n",
        "}\n",
    );

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_added_subtree(&diff, 2..6);
    assert_current_payload_once(&diff, "//! gamma");
    assert_current_payload_once(&diff, "beta();");
    assert_removed_payload(&diff, "//! gamma");
    assert_removed_payload(&diff, "beta();");
    assert_source_ownership(&diff);
}

#[test]
fn repeated_lines_cannot_be_consumed_by_an_inserted_function() {
    let before = concat!(
        "fn alpha() {\n",
        "    gamma();\n",
        "}\n",
        "fn beta() {\n",
        "    gamma();\n",
        "}\n",
    );
    let after = concat!(
        "fn alpha() {\n",
        "    delta();\n",
        "}\n",
        "fn gamma() {\n",
        "    gamma();\n",
        "}\n",
        "fn beta() {\n",
        "    gamma();\n",
        "}\n",
    );

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_added_subtree(&diff, 4..7);
    assert_line_fragment_mark(&diff, "gamma();", "gamma", DiffMark::Context);
    assert_eq!(current_payload_count(&diff, "gamma();"), 2, "{diff:#?}");
    assert_source_ownership(&diff);
}

#[test]
fn isolated_expression_edit_does_not_repeat_distant_breadcrumbs() {
    let before = expression_fixture("let beta = Beta::Alpha;");
    let after = expression_fixture("let beta = self.beta.unwrap_or(Beta::Alpha);");

    let diff = diff_file("alpha.rs", &before, &after).expect("Rust must diff");

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
fn duplicated_wrapper_payload_keeps_fifo_sibling_identity() {
    let before = concat!(
        "fn alpha() {\n",
        "    consume(x);\n",
        "    consume(y.unwrap_or(x));\n",
        "}\n",
    );
    let after = concat!(
        "fn alpha() {\n",
        "    consume(y.unwrap_or(x));\n",
        "    consume(y.unwrap_or(x));\n",
        "}\n",
    );

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_line_fragment_mark(&diff, "unwrap_or", "y.unwrap_or(", DiffMark::Added);
    assert_line_fragment_mark(&diff, "unwrap_or", "x", DiffMark::Context);
    assert_no_removed_payload(&diff, "x");
    let wrapped = source_lines(&diff).any(|(line, current)| {
        current && line.number == 2 && line.has_changes() && line_text(line).contains("unwrap_or")
    });
    assert!(
        wrapped,
        "the first sibling did not receive its local wrapper: {diff:#?}"
    );
    assert_eq!(
        current_payload_count(&diff, "consume(y.unwrap_or(x));"),
        2,
        "both current siblings must remain visible: {diff:#?}"
    );
    let retained = source_lines(&diff).any(|(line, current)| {
        current && line.number == 3 && !line.has_changes() && line_text(line).contains("unwrap_or")
    });
    assert!(
        retained,
        "the existing second sibling lost its identity: {diff:#?}"
    );
    assert_source_ownership(&diff);
}

#[test]
fn nested_closures_retain_their_unique_payload_at_any_depth() {
    let before = concat!("fn alpha() {\n", "    let beta = x;\n", "}\n");
    let after = concat!("fn alpha() {\n", "    let beta = || || || x;\n", "}\n");

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_added_payload(&diff, "|| || ||");
    assert_line_fragment_mark(&diff, "|| || || x", "x", DiffMark::Context);
    assert_no_removed_payload(&diff, "x");
    assert_source_ownership(&diff);
}

#[test]
fn multiline_wrapper_is_one_green_only_structural_edit() {
    let before = concat!("fn alpha() {\n", "    let beta = x;\n", "}\n");
    let after = concat!(
        "fn alpha() {\n",
        "    let beta = Some(\n",
        "        x\n",
        "    );\n",
        "}\n",
    );

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_line_fragment_mark(&diff, "let beta = Some(", "let beta = ", DiffMark::Context);
    assert_line_fragment_mark(&diff, "let beta = Some(", "Some(", DiffMark::Added);
    assert_line_fragment_mark(&diff, "        x", "x", DiffMark::Context);
    assert_line_fragment_mark(&diff, "    );", ")", DiffMark::Added);
    assert!(
        source_lines(&diff).all(|(_, current)| current),
        "a green-only wrapper retained an old-world ghost: {diff:#?}"
    );
    let current = source_lines(&diff)
        .filter_map(|(line, current)| current.then_some(line.number))
        .collect::<Vec<_>>();
    assert!(
        current.windows(2).all(|pair| pair[0] <= pair[1]),
        "one wrapper transition reordered its own rows: {diff:#?}"
    );
    assert_source_ownership(&diff);
}

#[test]
fn multiline_unwrapper_is_one_red_only_structural_edit() {
    let before = concat!(
        "fn alpha() {\n",
        "    let beta = Some(\n",
        "        x\n",
        "    );\n",
        "}\n",
    );
    let after = concat!("fn alpha() {\n", "    let beta = x;\n", "}\n");

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_removed_payload(&diff, "Some(");
    assert_removed_payload(&diff, ")");
    assert_context_payload(&diff, "let beta = x;");
    assert_source_ownership(&diff);
}

#[test]
fn nested_type_assertion_retains_its_unique_payload_at_any_depth() {
    let before = "const alpha = await beta({ gamma: true, delta: true });\n";
    let after = "const alpha = (await beta({ gamma: true, delta: true })) as Epsilon | null;\n";

    let diff = diff_file("alpha.ts", before, after).expect("TypeScript must diff");

    assert_line_fragment_mark(
        &diff,
        "as Epsilon | null",
        "await beta({ gamma: true, delta: true })",
        DiffMark::Context,
    );
    for fragment in ["(", ")", "as", "Epsilon", "|", "null"] {
        assert_line_fragment_mark(&diff, "as Epsilon | null", fragment, DiffMark::Added);
    }
    assert_no_removed_payload(&diff, "await beta({ gamma: true, delta: true })");
    assert_source_ownership(&diff);
}

#[test]
fn removing_nested_elements_retains_their_unique_payload_at_any_depth() {
    let before = "<alpha><beta><img src=\"gamma.webp\"></beta></alpha>\n";
    let after = "<img src=\"gamma.webp\">\n";

    let diff = diff_file("alpha.html", before, after).expect("HTML must diff");

    assert_context_payload(&diff, "<img src=\"gamma.webp\">");
    assert_no_removed_payload(&diff, "<img src=\"gamma.webp\">");
    assert_removed_payload(&diff, "<alpha><beta>");
    assert_removed_payload(&diff, "</beta></alpha>");
    assert_source_ownership(&diff);
}

#[test]
fn nested_elements_retain_their_unique_payload_at_any_depth() {
    let before = "<img src=\"alpha.webp\">\n";
    let after = concat!(
        "<div>\n",
        "  <section>\n",
        "    <img src=\"alpha.webp\">\n",
        "  </section>\n",
        "</div>\n",
    );

    let diff = diff_file("alpha.html", before, after).expect("HTML must diff");

    assert_context_payload(&diff, "<img src=\"alpha.webp\">");
    assert_no_removed_payload(&diff, "<img src=\"alpha.webp\">");
    for payload in ["<div>", "<section>", "</section>", "</div>"] {
        assert_added_payload(&diff, payload);
    }
    assert_source_ownership(&diff);
}

#[test]
fn repeated_markup_elements_cannot_choose_an_occurrence_by_their_payload() {
    let cases = [
        (
            "alpha.html",
            concat!("<div><span></span></div>\n", "<div></div>\n",),
            concat!("<div></div>\n", "<div><span></span></div>\n",),
            "<span></span>",
        ),
        (
            "alpha.tsx",
            concat!(
                "const alpha = [\n",
                "  <div><span /></div>,\n",
                "  <div></div>,\n",
                "];\n",
            ),
            concat!(
                "const alpha = [\n",
                "  <div></div>,\n",
                "  <div><span /></div>,\n",
                "];\n",
            ),
            "<span />",
        ),
    ];

    for (path, before, after, payload) in cases {
        let diff = diff_file(path, before, after).expect("markup syntax must diff");

        assert_removed_payload(&diff, payload);
        assert_added_payload(&diff, payload);
        assert_source_ownership(&diff);
    }
}

#[test]
fn repeated_declarations_cannot_choose_an_occurrence_by_their_value() {
    let before = concat!(".alpha {\n", "  color: alpha;\n", "  color: beta;\n", "}\n",);
    let after = concat!(".alpha {\n", "  color: beta;\n", "  color: alpha;\n", "}\n",);

    let diff = diff_file("alpha.css", before, after).expect("CSS must diff");

    assert_removed_payload(&diff, "alph");
    assert_added_payload(&diff, "alph");
    assert_removed_payload(&diff, "bet");
    assert_added_payload(&diff, "bet");
    assert_source_ownership(&diff);
}

#[test]
fn duplicated_payload_does_not_certify_a_wrapper_spine() {
    let before = concat!("fn alpha() {\n", "    let beta = x;\n", "}\n");
    let after = concat!("fn alpha() {\n", "    let beta = (x, x);\n", "}\n");

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_removed_payload(&diff, "x");
    assert_line_fragment_mark(&diff, "(x, x)", "(x, x)", DiffMark::Added);
    assert_source_ownership(&diff);
}

#[test]
fn a_new_local_function_is_not_a_wrapper_around_an_old_statement() {
    let before = concat!("fn alpha() {\n", "    beta();\n", "}\n");
    let after = concat!(
        "fn alpha() {\n",
        "    fn gamma() {\n",
        "        beta();\n",
        "    }\n",
        "}\n",
    );

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert_removed_payload(&diff, "beta();");
    assert_added_subtree(&diff, 2..5);
    assert_source_ownership(&diff);
}

#[test]
fn payload_moved_between_arrow_bodies_is_not_a_wrapper_transition() {
    let before = concat!(
        "const alpha = () => {\n",
        "    gamma();\n",
        "};\n",
        "const beta = () => {};\n",
    );
    let after = concat!(
        "const alpha = () => {};\n",
        "const beta = () => {\n",
        "    gamma();\n",
        "};\n",
    );

    let diff = diff_file("alpha.ts", before, after).expect("TypeScript must diff");

    assert_removed_payload(&diff, "gamma();");
    assert_added_payload(&diff, "gamma();");
    assert_source_ownership(&diff);
}

#[test]
fn swapped_calls_cannot_tunnel_between_function_owners() {
    let before = concat!(
        "fn alpha() {\n",
        "    gamma();\n",
        "}\n",
        "fn beta() {\n",
        "    delta();\n",
        "}\n",
    );
    let after = concat!(
        "fn alpha() {\n",
        "    delta();\n",
        "}\n",
        "fn beta() {\n",
        "    gamma();\n",
        "}\n",
    );

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    for payload in ["gamm", "delt"] {
        assert_removed_payload(&diff, payload);
        assert_added_payload(&diff, payload);
    }
    assert_line_predecessor(&diff, 2, 2, "gamma();", "delta();");
    assert_line_predecessor(&diff, 5, 5, "delta();", "gamma();");
    assert_source_ownership(&diff);
}

#[test]
fn an_inserted_sibling_cannot_consume_the_following_exact_sibling() {
    let cases = [
        (
            concat!("fn alpha() {\n", "    beta();\n", "}\n"),
            concat!("fn alpha() {\n", "    gamma();\n", "    beta();\n", "}\n",),
            "gamma();",
            "beta();",
        ),
        (
            concat!("fn alpha(\n", "    beta: Beta,\n", ") {}\n"),
            concat!(
                "fn alpha(\n",
                "    gamma: Gamma,\n",
                "    beta: Beta,\n",
                ") {}\n",
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

    for (before, after, added, retained) in cases {
        let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

        assert_added_payload(&diff, added);
        assert_context_payload(&diff, retained);
        assert_current_payload_once(&diff, retained);
        assert_no_removed_payload(&diff, retained);
        assert_source_ownership(&diff);
    }
}

#[test]
fn a_breadcrumb_and_context_share_one_physical_row() {
    let before = concat!(
        "struct Alpha {\n",
        "    beta: Old,\n",
        "}\n",
        "\n",
        "impl Alpha {\n",
        "    fn gamma() {\n",
        "        let a = 1;\n",
        "        let b = 2;\n",
        "        let c = 3;\n",
        "        let d = 4;\n",
        "        old();\n",
        "    }\n",
        "}\n",
    );
    let after = before.replace("Old", "New").replace("old();", "new();");

    let diff = diff_file("alpha.rs", before, &after).expect("Rust must diff");

    assert_eq!(
        current_payload_count(&diff, "impl Alpha {"),
        1,
        "one physical context row was emitted twice: {diff:#?}"
    );
    for payload in ["Old", "old"] {
        assert_removed_payload(&diff, payload);
    }
    for payload in ["New", "new"] {
        assert_added_payload(&diff, payload);
    }
    assert_source_ownership(&diff);
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
    let after = concat!(
        "fn alpha() {\n",
        "    beta(&[\"gamma\", \"BETA\"]);\n",
        "    beta(\n",
        "        &[\"gamma\", \"BETA\"],\n",
        "    );\n",
        "}\n",
    );

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    for line in [2, 4] {
        assert_line_predecessor(&diff, line, line, "ALPHA", "BETA");
        let displays = source_lines(&diff)
            .filter(|(display, _)| {
                let text = line_text(display);
                display.number == line && (text.contains("ALPHA") || text.contains("BETA"))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            displays.len(),
            2,
            "both revisions must stay local: {diff:#?}"
        );
        for (display, current) in displays {
            let suffix = if line == 2 { "]);" } else { "]," };
            assert!(
                display
                    .spans
                    .iter()
                    .any(|span| span.mark == DiffMark::Context && span.text.contains(suffix)),
                "the {:?} suffix became {:?} syntax: {diff:#?}",
                if current { "current" } else { "before" },
                if current { "added" } else { "removed" },
            );
        }
    }
    assert_source_ownership(&diff);
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

    let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");

    assert!(source_lines(&diff).any(|(line, current)| {
        current
            && line.number == 2
            && line.has_changes()
            && line_text(line).contains("gamma(&beta)")
    }));
    assert_context_payload(&diff, ".epsilon(\"beta\")");
    assert_current_payload_once(&diff, ".epsilon(\"beta\")");
    assert_no_removed_payload(&diff, ".epsilon(\"beta\")");
    for (line_number, payload, mark) in [
        (5, "or_else", DiffMark::Added),
        (6, ".map", DiffMark::Context),
    ] {
        let line = source_lines(&diff)
            .find_map(|(line, current)| {
                (current && line.number == line_number && line_text(line).contains(payload))
                    .then_some(line)
            })
            .unwrap_or_else(|| panic!("missing chain link {payload:?}: {diff:#?}"));
        assert!(
            line.spans
                .iter()
                .filter(|span| !span.text.trim().is_empty())
                .all(|span| span.mark == mark),
            "chain link {payload:?} contains a mark other than {mark:?}: {diff:#?}"
        );
    }
    assert_source_ownership(&diff);
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
    let after = "fn alpha() {}\n\nfn zeta() {}\n";

    for (before, after, removed_lines) in [
        (before, after, 3..10),
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
        let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");
        let removed = source_lines(&diff)
            .filter_map(|(line, current)| (!current).then_some(line.number))
            .collect::<Vec<_>>();

        assert_eq!(removed, removed_lines.collect::<Vec<_>>(), "{diff:#?}");
        for line in after.lines().filter(|line| !line.is_empty()) {
            assert_context_payload(&diff, line);
        }
        assert_source_ownership(&diff);
    }
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
        let diff = diff_file("alpha.rs", before, after).expect("Rust must diff");
        let numbers = source_lines(&diff)
            .filter_map(|(line, current)| (current == side).then_some(line.number))
            .collect::<Vec<_>>();

        assert_eq!(numbers, (1..=6).collect::<Vec<_>>(), "{diff:#?}");
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
        "        delta();\n",
        "    }\n",
        "}\n",
    );
    let after = before.replace("self.beta()", "self.gamma()");
    let sibling_line = before
        .lines()
        .position(|line| line.contains("fn beta"))
        .expect("fixture has a distant sibling")
        + 1;

    let diff = diff_file("alpha.rs", before, &after).expect("Rust must diff");
    let hunk = diff.hunks.first().expect("the edit must be visible");

    assert_eq!(diff.hunks.len(), 1, "one edit needs one focused hunk");
    assert!(
        hunk.coverage
            .after
            .as_ref()
            .is_some_and(|coverage| coverage.end <= sibling_line),
        "the local halo reached the distant sibling: {hunk:#?}"
    );
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
    for payload in ["fn beta", "delta();"] {
        assert!(
            !hunk_contains(hunk, payload),
            "distant sibling payload {payload:?} leaked into the local halo: {hunk:#?}"
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

    let diff = diff_file("alpha.rs", &before, &after).expect("Rust must diff");
    let payload = hunk_position(&diff, "fn delta");
    let movement = diff
        .hunks
        .iter()
        .position(|hunk| {
            hunk.rows
                .iter()
                .any(|row| matches!(row, ReviewRow::Moved { .. }))
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
            .any(|row| matches!(row, ReviewRow::Wordwise(_))),
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

fn source_lines(diff: &PresentedFile) -> impl Iterator<Item = (&SourceRow, bool)> {
    diff.hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .flat_map(|row| match row {
            ReviewRow::Current(line)
            | ReviewRow::Reflow(line)
            | ReviewRow::Moved { after: line, .. } => {
                vec![(line, true)]
            }
            ReviewRow::Removed(line) => vec![(line, false)],
            ReviewRow::Added(line) => vec![(line, true)],
            ReviewRow::LineEnding { .. }
            | ReviewRow::Wordwise(_)
            | ReviewRow::Elision(_)
            | ReviewRow::FileBoundary => Vec::new(),
        })
}

fn line_text(line: &SourceRow) -> String {
    line.spans.iter().map(|span| span.text.as_str()).collect()
}

fn assert_current_payload_once(diff: &PresentedFile, payload: &str) {
    let occurrences = current_payload_count(diff, payload);
    assert_eq!(occurrences, 1, "current payload {payload:?}: {diff:#?}");
}

fn assert_context_payload(diff: &PresentedFile, payload: &str) {
    assert!(
        source_lines(diff).any(|(line, current)| {
            current && line_has_marked_payload(line, payload, DiffMark::Context)
        }),
        "missing retained payload {payload:?}: {diff:#?}"
    );
}

fn assert_line_fragment_mark(
    diff: &PresentedFile,
    line_payload: &str,
    fragment: &str,
    mark: DiffMark,
) {
    assert!(
        source_lines(diff).any(|(line, current)| {
            current
                && line_text(line).contains(line_payload)
                && line_has_marked_payload(line, fragment, mark)
        }),
        "missing {mark:?} fragment {fragment:?} on {line_payload:?}: {diff:#?}"
    );
}

fn current_payload_count(diff: &PresentedFile, payload: &str) -> usize {
    source_lines(diff)
        .filter(|(line, current)| *current && line_text(line).contains(payload))
        .count()
}

fn assert_added_payload(diff: &PresentedFile, payload: &str) {
    assert!(
        source_lines(diff).any(|(line, current)| {
            current && line_has_marked_payload(line, payload, DiffMark::Added)
        }) || diff.hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
            matches!(row, ReviewRow::Wordwise(word) if word.added.contains(payload))
        }),
        "missing added payload {payload:?}: {diff:#?}"
    );
}

fn assert_added_subtree(diff: &PresentedFile, lines: Range<usize>) {
    let expected = lines.clone().collect::<Vec<_>>();
    let mut seen = Vec::new();
    for (hunk_index, hunk) in diff.hunks.iter().enumerate() {
        for (row_index, row) in hunk.rows.iter().enumerate() {
            let current = match row {
                ReviewRow::Added(after) => {
                    if !lines.contains(&after.number) {
                        continue;
                    }
                    after
                }
                ReviewRow::Removed(_) => continue,
                ReviewRow::Current(line)
                | ReviewRow::Reflow(line)
                | ReviewRow::Moved { after: line, .. }
                    if lines.contains(&line.number) =>
                {
                    panic!(
                        "added subtree line {} was retained instead of added: {diff:#?}",
                        line.number
                    );
                }
                ReviewRow::Wordwise(word)
                    if word.after_line.is_some_and(|line| lines.contains(&line)) =>
                {
                    panic!("added subtree borrowed a wordwise predecessor: {diff:#?}");
                }
                _ => continue,
            };
            assert!(
                current
                    .spans
                    .iter()
                    .all(|span| span.mark == DiffMark::Added),
                "added subtree line {} contains a retained span: {diff:#?}",
                current.number
            );
            seen.push((hunk_index, row_index, current.number));
        }
    }

    assert_eq!(
        seen.iter().map(|(_, _, line)| *line).collect::<Vec<_>>(),
        expected,
        "added subtree coverage: {diff:#?}"
    );
    assert!(
        seen.iter().all(|(hunk, _, _)| *hunk == seen[0].0),
        "added subtree was split across hunks: {diff:#?}"
    );
    assert!(
        seen.windows(2)
            .all(|rows| rows[0].1.checked_add(1) == Some(rows[1].1)),
        "added subtree rows were interleaved: {diff:#?}"
    );
}

fn assert_line_predecessor(
    diff: &PresentedFile,
    before_number: usize,
    after_number: usize,
    before_payload: &str,
    after_payload: &str,
) {
    let wordwise = diff
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.rows)
        .any(|row| match row {
            ReviewRow::Wordwise(word) => {
                word.before_line == Some(before_number)
                    && word.after_line == Some(after_number)
                    && format!("{}{}{}", word.prefix, word.removed, word.suffix)
                        .contains(before_payload)
                    && format!("{}{}{}", word.prefix, word.added, word.suffix)
                        .contains(after_payload)
            }
            _ => false,
        });
    let adjacent = diff.hunks.iter().any(|hunk| {
        hunk.rows.windows(2).any(|rows| {
            matches!(
                rows,
                [
                    ReviewRow::Removed(before),
                    ReviewRow::Added(after),
                ] if before.number == before_number
                    && after.number == after_number
                    && line_text(before).contains(before_payload)
                    && line_text(after).contains(after_payload)
            )
        })
    });
    assert!(
        wordwise || adjacent,
        "before line {before_number} did not directly precede current line {after_number}: {diff:#?}"
    );
}

fn assert_no_removed_payload(diff: &PresentedFile, payload: &str) {
    assert!(
        !source_lines(diff).any(|(line, current)| {
            !current && line_has_marked_payload(line, payload, DiffMark::Removed)
        }) && !diff.hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
            matches!(row, ReviewRow::Wordwise(word) if word.removed.contains(payload))
        }),
        "redundant old payload {payload:?} remained visible: {diff:#?}"
    );
}

fn assert_removed_payload(diff: &PresentedFile, payload: &str) {
    assert!(
        source_lines(diff).any(|(line, current)| {
            !current && line_has_marked_payload(line, payload, DiffMark::Removed)
        }) || diff.hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
            matches!(row, ReviewRow::Wordwise(word) if word.removed.contains(payload))
        }),
        "missing removed payload {payload:?}: {diff:#?}"
    );
}

fn line_has_marked_payload(line: &SourceRow, payload: &str, mark: DiffMark) -> bool {
    let mut run = String::new();
    for span in &line.spans {
        if span.mark != mark {
            run.clear();
            continue;
        }
        run.push_str(&span.text);
        if run.contains(payload) {
            return true;
        }
    }
    false
}

fn assert_source_ownership(diff: &PresentedFile) {
    let mut global_before = std::collections::HashMap::<usize, (usize, bool)>::new();
    let mut global_after = std::collections::HashMap::<usize, (usize, bool)>::new();
    for hunk in &diff.hunks {
        let mut before = std::collections::HashSet::new();
        let mut after = std::collections::HashSet::new();
        for row in &hunk.rows {
            let (before_line, after_line, material) = match row {
                ReviewRow::Current(line) => (None, Some(line.number), line.has_changes()),
                ReviewRow::Reflow(line) => (None, Some(line.number), true),
                ReviewRow::Removed(line) => (Some(line.number), None, true),
                ReviewRow::Added(line) => (None, Some(line.number), true),
                ReviewRow::Moved {
                    before,
                    after: line,
                } => (*before, Some(line.number), true),
                ReviewRow::Wordwise(word) => (word.before_line, word.after_line, true),
                ReviewRow::LineEnding { .. } | ReviewRow::Elision(_) | ReviewRow::FileBoundary => {
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

fn hunk_contains(hunk: &ReviewHunk, payload: &str) -> bool {
    hunk.rows.iter().any(|row| match row {
        ReviewRow::Current(line)
        | ReviewRow::Reflow(line)
        | ReviewRow::Moved { after: line, .. } => line_text(line).contains(payload),
        ReviewRow::Removed(line) | ReviewRow::Added(line) => line_text(line).contains(payload),
        ReviewRow::Wordwise(word) => [
            word.prefix.as_str(),
            word.removed.as_str(),
            word.added.as_str(),
            word.suffix.as_str(),
        ]
        .concat()
        .contains(payload),
        ReviewRow::LineEnding { .. } | ReviewRow::Elision(_) | ReviewRow::FileBoundary => false,
    })
}

fn hunk_position(diff: &PresentedFile, payload: &str) -> usize {
    diff.hunks
        .iter()
        .position(|hunk| hunk_contains(hunk, payload))
        .unwrap_or_else(|| panic!("missing hunk for {payload:?}: {diff:#?}"))
}
