use super::*;

#[test]
fn token_boundaries_and_literal_newlines_are_material() {
    for (path, before, after) in [
        (
            "a.c",
            "int f(int x, int y) {\n    return x + ++y;\n}\n",
            "int f(int x, int y) {\n    return x++ + y;\n}\n",
        ),
        (
            "a.rs",
            "fn f() {\n    m!(foo bar);\n}\n",
            "fn f() {\n    m!(foobar);\n}\n",
        ),
        (
            "a.rs",
            "fn f() {\n    let s = \"a\nb\";\n}\n",
            "fn f() {\n    let s = \"ab\";\n}\n",
        ),
        (
            "a.py",
            "def f():\n    if ready:\n        a()\n    b()\n    c()\n",
            "def f():\n    if ready:\n        a()\n        b()\n        c()\n",
        ),
        ("a.txt", "ab\ncd\n", "abcd\n"),
    ] {
        let presented = diff(path, before, after);
        assert!(
            !removed(&presented).is_empty(),
            "{path}: {}",
            all_text(&presented)
        );
        assert!(
            !added(&presented).is_empty(),
            "{path}: {}",
            all_text(&presented)
        );
        assert!(
            presented
                .hunks
                .iter()
                .any(|hunk| hunk.excerpt.priority == Priority::Edit)
        );
    }
}

#[test]
fn a_final_newline_change_survives_reflow() {
    let presented = diff("a.rs", "fn f() { call(); }", "fn f() {\n    call();\n}\n");
    assert!(presented.hunks.iter().flat_map(|hunk| &hunk.excerpt.rows).any(|row| {
        matches!(row, Row::Ending(EndingChange::Replaced { before, after }) if before.line.get() == 1 && before.ending == LineEnding::Missing && after.line.get() == 3 && after.ending == LineEnding::Lf)
    }), "{}", all_text(&presented));
}

#[test]
fn formatting_and_relocation_do_not_change_a_contract() {
    for (before, after) in [
        ("fn f() { call(); }\n", "fn f() {\n    call();\n}\n"),
        (
            "fn a() { one(); }\nfn b() { two(); }\n",
            "fn b() { two(); }\nfn a() { one(); }\n",
        ),
    ] {
        let presented = diff("a.rs", before, after);
        assert!(
            presented
                .hunks
                .iter()
                .all(|hunk| !hunk.attribution.changes.contract),
            "{presented:#?}"
        );
        assert!(
            presented
                .hunks
                .iter()
                .any(|hunk| hunk.attribution.symbols().next().is_some())
        );
    }
}

#[test]
fn excessive_nesting_uses_exact_lines() {
    let before = format!("{}0{}\n", "[".repeat(10_000), "]".repeat(10_000));
    let after = before.replacen('0', "1", 1);
    let presented = diff("a.json", &before, &after);
    assert!(!removed(&presented).is_empty());
    assert!(!added(&presented).is_empty());
    assert_eq!(
        presented.comparison,
        Comparison::Lines(LineFallback::DepthLimit)
    );
}

#[test]
fn equal_subtrees_can_supersede_a_wrapper_recovery() {
    // Capturing `src/diff/syntax/lower.rs` around `b513f5e` with `git cat-file blob`;
    // the fixture OIDs are `70b251736327c3983068feb141b209d1c313e56c` and
    // `fcaf5bd87352b3cd1648f511ac6ee81cd3e9b021`.
    let before = include_str!("cases/recovery.before.rs");
    let after = include_str!("cases/recovery.after.rs");
    let presented = diff("lower.rs", before, after);
    assert!(!presented.hunks.is_empty());
}

#[test]
fn terminator_changes_inside_a_move_keep_their_source_rows_visible() {
    let moved = format!(
        "fn moved() {{\n{}}}\n",
        (0..8)
            .map(|index| format!("    step_{index}();\n"))
            .collect::<String>()
    );
    let stable = format!(
        "fn stable() {{\n{}}}\n",
        (0..30)
            .map(|index| format!("    stable_{index}();\n"))
            .collect::<String>()
    );
    let before = format!("{moved}\n{stable}");
    let moved = moved.replace("step_3();\n", "step_3();\r\n");
    let after = format!("{stable}\n{moved}");
    let presented = diff("a.rs", &before, &after);
    let rows = presented
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.excerpt.rows)
        .collect::<Vec<_>>();
    assert!(
        rows.iter()
            .any(|row| matches!(row, Row::Moved { after, .. } if after.text().contains("step_3"))),
        "{presented:#?}"
    );
    assert!(rows.iter().any(|row| matches!(row, Row::Ending(EndingChange::Replaced { after, .. }) if after.ending == LineEnding::CrLf)));
}

#[test]
fn same_named_methods_retain_their_distinct_owners() {
    let before = "class A { run() { return 1; } }\nclass B { run() { return 1; } }\n";
    let after = before.replace("return 1", "return 2");
    let presented = diff("a.ts", before, &after);
    let symbols = presented
        .hunks
        .iter()
        .flat_map(|hunk| hunk.attribution.symbols())
        .collect::<Vec<_>>();
    for owner in ["A", "B"] {
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == "run" && symbol.owner.as_deref() == Some(owner)),
            "{symbols:?}"
        );
    }
}

#[test]
fn a_decorated_definition_has_one_declaration_identity() {
    let before = "@decorator\ndef f():\n    return 1\n";
    let after = before.replace("return 1", "return 2");
    let presented = diff("a.py", before, &after);
    let symbols = presented
        .hunks
        .iter()
        .flat_map(|hunk| hunk.attribution.symbols())
        .collect::<Vec<_>>();
    assert_eq!(symbols.len(), 1, "{symbols:?}");
    assert_eq!(symbols[0].name, "f");
    assert_eq!(symbols[0].owner, None);
}
