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
        let presented = diff(path, before, after);
        assert!(
            presented
                .hunks
                .iter()
                .all(|hunk| hunk.excerpt.priority == Priority::Reflow),
            "{path}: {presented:#?}"
        );
        assert!(
            removed(&presented).is_empty() && added(&presented).is_empty(),
            "{path}: {presented:#?}"
        );
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
        let presented = diff(path, before, after);
        assert!(
            !removed(&presented).is_empty() && !added(&presented).is_empty(),
            "{path}: {presented:#?}"
        );
        assert!(
            presented
                .hunks
                .iter()
                .all(|hunk| hunk.excerpt.priority != Priority::Reflow),
            "{path}: {presented:#?}"
        );
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
        let presented = diff(path, before, after);
        assert!(
            removed(&presented).iter().any(|line| line.contains('2')),
            "{path}: {presented:#?}"
        );
        assert!(
            added(&presented).iter().any(|line| line.contains('3')),
            "{path}: {presented:#?}"
        );
        let marked_removed = presented
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.excerpt.rows)
            .filter_map(|row| match row {
                Row::Removed(line) | Row::Changed { before: line, .. } => Some(line),
                _ => None,
            })
            .flat_map(|line| &line.spans)
            .filter(|span| span.mark == DiffMark::Removed)
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert!(!marked_removed.contains('1'), "{path}: {presented:#?}");
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
        let presented = diff(path, before, after);
        assert!(
            presented
                .hunks
                .iter()
                .any(|hunk| hunk.excerpt.priority != Priority::Reflow),
            "{path}: {presented:#?}"
        );
    }
}

#[test]
fn web_text_spacing_and_stylesheet_values_remain_material() {
    for (path, before, after) in [
        (
            "a.html",
            "<p>one <em>two</em></p>\n",
            "<p>one<em>two</em></p>\n",
        ),
        ("a.html", "<pre>one\ntwo</pre>\n", "<pre>onetwo</pre>\n"),
        (
            "a.css",
            ".a {\n  color: red;\n  margin: 0;\n}\n",
            ".a {\n  color: blue;\n  margin: 0;\n}\n",
        ),
    ] {
        let presented = diff(path, before, after);
        assert!(
            !removed(&presented).is_empty() && !added(&presented).is_empty(),
            "{path}: {presented:#?}"
        );
        assert!(
            presented
                .hunks
                .iter()
                .any(|hunk| hunk.excerpt.priority == Priority::Edit)
        );
    }
}
