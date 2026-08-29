use super::*;

#[test]
fn line_projection_is_an_exact_leaf_cst_including_terminators() {
    let pair = project_pair(Path::new("notes.txt"), "a\r\n\nlast", "", false).unwrap();

    assert_eq!(pair.before.language, Language::Lines);
    let root = pair.before.node(pair.before.root);
    let payloads = root
        .children
        .iter()
        .map(|id| pair.before.leaf_text(*id).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(payloads, ["a\r\n", "\n", "last"]);
    assert!(root.children.iter().all(|id| {
        pair.before
            .node(*id)
            .review
            .as_ref()
            .is_some_and(|unit| unit.mode == ReviewMode::Linewise)
    }));
}

#[test]
fn one_bad_parse_falls_both_revisions_back_to_lines() {
    let pair = project_pair(Path::new("lib.rs"), "fn good() {}\n", "fn {\n", false).unwrap();

    assert_eq!(pair.before.language, Language::Lines);
    assert_eq!(pair.after.language, Language::Lines);
}

#[test]
fn recovered_html_places_the_image_under_the_parser_created_div() {
    let before = "<p>\n \t<img />\n</p>\n";
    let after = concat!(
        "<p>\n",
        "\t<div\n",
        "\t\tid=\"alpha\"\n",
        "\t\tdata-beta=\"gamma\"\t\n",
        "\t>\n",
        " \t\t<img />\n",
        "\t</div>\n",
        "</p>\n",
    );
    let pair = project_pair(Path::new("alpha.html"), before, after, false).unwrap();

    assert_eq!(pair.before.language, Language::Html);
    assert_eq!(pair.after.language, Language::Html);
    let before_img = node_with_identity(&pair.before, "img");
    let before_paragraph = node_with_identity(&pair.before, "p");
    let after_img = node_with_identity(&pair.after, "img");
    let after_paragraph = node_with_identity(&pair.after, "p");
    let after_div = node_with_identity(&pair.after, "div");

    assert!(is_descendant(&pair.before, before_img, before_paragraph));
    assert!(is_descendant(&pair.after, after_img, after_div));
    assert!(!is_descendant(&pair.after, after_img, after_paragraph));
}

#[test]
fn html_nonlocal_raw_text_errors_remain_line_fallback() {
    let before = "<pre>\n<textarea>\n</pre>\n  <img>\n</textarea>\n</pre>\n";
    let after = concat!(
        "<pre>\n",
        "<textarea>\n",
        "</pre>\n",
        "  <div>\n",
        "    <img>\n",
        "  </div>\n",
        "</textarea>\n",
        "</pre>\n",
    );
    let pair = project_pair(Path::new("view.html"), before, after, false).unwrap();

    assert_eq!(pair.before.language, Language::Lines);
    assert_eq!(pair.after.language, Language::Lines);
}

#[test]
fn generated_source_never_enters_a_language_grammar() {
    let pair = project_pair(Path::new("broken.rs"), "fn {", "fn good() {}", true).unwrap();

    assert_eq!(pair.before.language, Language::Lines);
    assert_eq!(pair.after.language, Language::Lines);
}

#[test]
fn web_extensions_select_real_grammars() {
    let cases = [
        ("view.html", "<main>ok</main>", Language::Html),
        ("view.css", ".card { color: red; }", Language::Css),
        ("view.ts", "const n: number = 1;", Language::TypeScript),
        ("view.tsx", "const x = <Card />;", Language::Tsx),
        ("view.js", "const n = 1;", Language::JavaScript),
        ("view.jsx", "const x = <Card />;", Language::Jsx),
    ];

    for (path, source, language) in cases {
        let pair = project_pair(Path::new(path), source, source, false).unwrap();
        assert_eq!(pair.before.language, language, "{path}");
        assert_eq!(pair.after.language, language, "{path}");
    }
}

#[test]
fn c_extensions_select_structural_function_owners() {
    let source = concat!(
        "static int alpha(void) { return 0; }\n",
        "static int beta(void) { return alpha(); }\n",
    );

    for path in ["alpha.c", "alpha.h"] {
        let pair = project_pair(Path::new(path), source, source, false).unwrap();
        assert_eq!(pair.before.language, Language::C, "{path}");
        let functions = pair
            .before
            .review_units()
            .filter(|(_, node)| node.kind == "function_definition")
            .map(|(id, node)| {
                (
                    pair.before.identity_text(id),
                    node.correspondence,
                    node.review.unwrap().mode,
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            functions,
            [
                (
                    Some("alpha"),
                    CorrespondenceRole::HardOwner,
                    ReviewMode::Structural,
                ),
                (
                    Some("beta"),
                    CorrespondenceRole::HardOwner,
                    ReviewMode::Structural,
                ),
            ],
            "{path}"
        );
    }
}

#[test]
fn html_wrapper_and_child_are_distinct_trackable_nodes() {
    let before = "<article>\n  <img src=\"alpha.webp\">\n</article>\n";
    let after =
        "<article>\n  <div class=\"beta\">\n    <img src=\"alpha.webp\">\n  </div>\n</article>\n";
    let pair = project_pair(Path::new("view.html"), before, after, false).unwrap();

    let before_img = node_with_identity(&pair.before, "img");
    let after_img = node_with_identity(&pair.after, "img");
    let after_div = node_with_identity(&pair.after, "div");
    assert_eq!(pair.before.node(before_img).kind, "element");
    assert_eq!(pair.after.node(after_img).kind, "element");
    assert!(is_descendant(&pair.after, after_img, after_div));
    assert_eq!(pair.after.review_units().count(), 1);
    assert_eq!(pair.after.review_units().next().unwrap().0, pair.after.root);
}

#[test]
fn whitespace_sensitive_html_is_one_exact_opaque_payload() {
    let source = "<pre>\n  <img src=\"literal\">\n</pre>\n";
    let pair = project_pair(Path::new("view.html"), source, source, false).unwrap();
    let pre = node_with_identity(&pair.before, "pre");
    let pre = pair.before.node(pre);

    assert!(pre.children.is_empty());
    assert_eq!(pre.leaf.unwrap().channel, ContentChannel::Opaque);
    assert_eq!(
        pair.before.source.slice(pre.bytes.clone()),
        Some(source.trim_end_matches('\n'))
    );
}

#[test]
fn html_plaintext_absorbs_apparent_closing_tags_through_eof() {
    let source = "<p>markup</p>\n<plaintext>\n</plaintext>\n<div>still literal</div>\n";
    let pair = project_pair(Path::new("view.html"), source, source, false).unwrap();
    let plaintext = node_with_identity(&pair.before, "plaintext");
    let plaintext = pair.before.node(plaintext);

    assert!(plaintext.children.is_empty());
    assert_eq!(plaintext.leaf.unwrap().channel, ContentChannel::Opaque);
    assert_eq!(plaintext.bytes.end, source.len());
    assert_eq!(
        pair.before.source.slice(plaintext.bytes.clone()),
        Some("<plaintext>\n</plaintext>\n<div>still literal</div>\n")
    );
    assert!(
        !pair
            .before
            .nodes
            .iter()
            .enumerate()
            .map(|(index, _)| NodeId::new(index))
            .any(|id| pair.before.identity_text(id) == Some("div"))
    );
}

#[test]
fn language_review_units_are_non_overlapping_top_level_roots() {
    let cases = [
        (
            "lib.rs",
            "use crate::A;\nfn outer() {\n    // nested\n}\n// top\n",
            3,
        ),
        (
            "view.css",
            ".a {\n  /* nested */\n  color: red;\n}\n/* top */\n",
            2,
        ),
        (
            "view.ts",
            "import { a } from './a';\nfunction outer() {\n  // nested\n}\n// top\n",
            3,
        ),
    ];

    for (path, source, expected) in cases {
        let pair = project_pair(Path::new(path), source, source, false).unwrap();
        let units = pair
            .before
            .review_units()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        assert_eq!(units.len(), expected, "{path}");
        for (index, unit) in units.iter().copied().enumerate() {
            for other in units.iter().copied().skip(index + 1) {
                assert!(!is_descendant(&pair.before, unit, other), "{path}");
                assert!(!is_descendant(&pair.before, other, unit), "{path}");
            }
        }
    }
}

#[test]
fn typescript_declarator_identities_survive_edits_and_reordering() {
    let before = "export const alpha = 1;\nexport let beta = 2;\nvar gamma = 3;\n";
    let after = "var gamma = 30;\nexport let beta = 20;\nexport const alpha = 10;\n";
    let pair = project_pair(Path::new("values.ts"), before, after, false).unwrap();

    assert_eq!(review_identities(&pair.before), ["alpha", "beta", "gamma"]);
    assert_eq!(review_identities(&pair.after), ["gamma", "beta", "alpha"]);
}

#[test]
fn wiring_identities_survive_reordering() {
    let rust_before = "use crate::alpha;\npub use crate::beta::Thing;\n";
    let rust_after = "pub use crate::beta::Thing;\nuse crate::alpha;\n";
    let rust = project_pair(Path::new("lib.rs"), rust_before, rust_after, false).unwrap();
    assert_eq!(
        review_identities(&rust.before),
        ["crate::alpha", "crate::beta::Thing"]
    );
    assert_eq!(
        review_identities(&rust.after),
        ["crate::beta::Thing", "crate::alpha"]
    );

    let css_before = "@import \"alpha.css\" screen;\n@import url(\"beta.css\");\n";
    let css_after = "@import url(\"beta.css\");\n@import \"alpha.css\" print;\n";
    let css = project_pair(Path::new("view.css"), css_before, css_after, false).unwrap();
    assert_eq!(
        review_identities(&css.before),
        ["\"alpha.css\"", "url(\"beta.css\")"]
    );
    assert_eq!(
        review_identities(&css.after),
        ["url(\"beta.css\")", "\"alpha.css\""]
    );
}

#[test]
fn css_group_statements_keep_payload_header_identities_across_body_edits() {
    let before = "@media (min-width: 1px) { .a { color: red; } }\n@supports (display: grid) { .b { display: grid; } }\n@keyframes fade { from { opacity: 0; } }\n@scope (.card) { .title { color: blue; } }\n";
    let after = "@scope (.card) { .title { color: red; } }\n@keyframes fade { to { opacity: 1; } }\n@supports (display: grid) { .b { display: block; } }\n@media (min-width: 1px) { .a { color: green; } }\n";
    let pair = project_pair(Path::new("view.css"), before, after, false).unwrap();

    assert_eq!(
        review_identities(&pair.before),
        [
            "@media (min-width: 1px)",
            "@supports (display: grid)",
            "fade",
            "@scope (.card)",
        ]
    );
    assert_eq!(
        review_identities(&pair.after),
        [
            "@scope (.card)",
            "fade",
            "@supports (display: grid)",
            "@media (min-width: 1px)",
        ]
    );
    assert!(
        pair.before
            .review_units()
            .all(|(_, node)| { node.review.as_ref().unwrap().mode == ReviewMode::Structural })
    );
}

#[test]
fn tree_sitter_arenas_are_byte_total_and_retain_intrinsic_numeric_prefixes() {
    let cases = [
        ("value.css", ".card { margin: 7rem; }\n"),
        ("value.html", "<div class=\"a b\">\n  <img>\n</div>\n"),
        ("value.rs", "fn value() -> usize { 7 }\n"),
        ("value.ts", "export const value = `a b`;\n"),
    ];
    for (path, source) in cases {
        let pair = project_pair(Path::new(path), source, source, false).unwrap();
        assert_byte_total(&pair.before);
    }

    let pair = project_pair(
        Path::new("value.css"),
        ".card { margin: 7rem; }\n",
        ".card { margin: 7rem; }\n",
        false,
    )
    .unwrap();
    let digit = leaf_with_text(&pair.before, "7");
    assert_eq!(pair.before.node(digit).kind, "source_fragment");
    assert_eq!(
        pair.before.node(digit).leaf,
        Some(Leaf {
            syntax: SyntaxClass::Literal,
            channel: ContentChannel::Syntax,
            delimiter: false,
        })
    );
    let parent = pair.before.node(digit).parent.unwrap();
    assert_eq!(pair.before.node(parent).kind, "integer_value");
}

#[test]
fn intrinsic_css_digits_link_as_modified_and_render_as_a_replacement() {
    use crate::diff::correspondence::{LeafRelation, correspond};
    use crate::diff::{DiffMark, DiffRow};

    let before = ".card { margin: 6rem; }\n";
    let after = ".card { margin: 7rem; }\n";
    let pair = project_pair(Path::new("value.css"), before, after, false).unwrap();
    let six = leaf_with_text(&pair.before, "6");
    let seven = leaf_with_text(&pair.after, "7");
    let graph = correspond(&pair);
    let link = graph.before_leaf[six.index()]
        .and_then(|index| graph.leaf_links.get(index))
        .expect("numeric leaf must link");
    assert_eq!(link.after, seven);
    assert_eq!(link.relation, LeafRelation::Modified);

    let diff = crate::diff::diff_file("value.css", before, after).unwrap();
    assert!(diff.hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
        let DiffRow::LineChange {
            before: Some(before),
            after: Some(after),
        } = row
        else {
            return false;
        };
        before
            .spans
            .iter()
            .any(|span| span.mark == DiffMark::Removed && span.text == "6")
            && after
                .spans
                .iter()
                .any(|span| span.mark == DiffMark::Added && span.text == "7")
    }));
}

#[test]
fn layout_and_significant_whitespace_use_distinct_channels() {
    let css = project_pair(
        Path::new("selector.css"),
        ".a .b { content: \"a b\"; }\n",
        ".a .b { content: \"a b\"; }\n",
        false,
    )
    .unwrap();
    let selector_space = css
        .before
        .nodes
        .iter()
        .enumerate()
        .map(|(index, _)| NodeId::new(index))
        .find(|id| {
            css.before.leaf_text(*id) == Some(" ")
                && css
                    .before
                    .node(*id)
                    .parent
                    .is_some_and(|parent| css.before.node(parent).kind == "descendant_selector")
        })
        .expect("descendant combinator whitespace must be projected");
    assert_eq!(
        css.before.node(selector_space).leaf.unwrap().channel,
        ContentChannel::Syntax
    );
    let string_content = leaf_with_text(&css.before, "a b");
    assert_eq!(
        css.before.node(string_content).leaf.unwrap().channel,
        ContentChannel::Syntax
    );

    let html = project_pair(
        Path::new("space.html"),
        "<div>\n  <span>a</span> <span>b</span>\n</div>\n",
        "<div>\n  <span>a</span> <span>b</span>\n</div>\n",
        false,
    )
    .unwrap();
    assert!(html.before.nodes.iter().enumerate().any(|(index, _)| {
        let id = NodeId::new(index);
        html.before
            .leaf_text(id)
            .is_some_and(|text| text.contains('\n') && text.trim().is_empty())
            && html.before.node(id).leaf.unwrap().channel == ContentChannel::Layout
    }));
    assert!(html.before.nodes.iter().enumerate().any(|(index, _)| {
        let id = NodeId::new(index);
        html.before.leaf_text(id) == Some(" ")
            && html.before.node(id).leaf.unwrap().channel == ContentChannel::Syntax
    }));
}

fn node_with_identity(projection: &Projection<'_>, identity: &str) -> NodeId {
    projection
        .nodes
        .iter()
        .enumerate()
        .map(|(index, _)| NodeId::new(index))
        .find(|id| projection.identity_text(*id) == Some(identity))
        .unwrap_or_else(|| panic!("missing {identity:?} graph node"))
}

fn review_identities<'source>(projection: &Projection<'source>) -> Vec<&'source str> {
    projection
        .review_units()
        .map(|(id, _)| {
            projection.identity_text(id).unwrap_or_else(|| {
                panic!(
                    "review unit {:?} lacks an identity",
                    projection.node(id).kind
                )
            })
        })
        .collect()
}

fn leaf_with_text(projection: &Projection<'_>, text: &str) -> NodeId {
    projection
        .nodes
        .iter()
        .enumerate()
        .map(|(index, _)| NodeId::new(index))
        .find(|id| projection.leaf_text(*id) == Some(text))
        .unwrap_or_else(|| panic!("missing leaf {text:?}"))
}

fn assert_byte_total(projection: &Projection<'_>) {
    for (index, node) in projection.nodes.iter().enumerate() {
        if node.leaf.is_some() {
            assert!(
                node.children.is_empty(),
                "concrete leaf {:?} node {} grew projected children",
                node.kind,
                index
            );
            continue;
        }

        let mut cursor = node.bytes.start;
        for child in &node.children {
            let child = projection.node(*child);
            assert_eq!(
                child.bytes.start, cursor,
                "gap or overlap under {:?} node {}",
                node.kind, index
            );
            cursor = child.bytes.end;
        }
        assert_eq!(
            cursor, node.bytes.end,
            "uncovered suffix under {:?} node {}",
            node.kind, index
        );
    }
}

fn is_descendant(projection: &Projection<'_>, child: NodeId, ancestor: NodeId) -> bool {
    let mut parent = projection.node(child).parent;
    while let Some(candidate) = parent {
        if candidate == ancestor {
            return true;
        }
        parent = projection.node(candidate).parent;
    }
    false
}
