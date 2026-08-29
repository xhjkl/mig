use super::*;
use std::path::Path;

#[test]
fn lowered_syntax_is_byte_total() {
    let cases = [
        ("alpha.rs", "fn alpha() -> usize { 7 }\n"),
        ("alpha.css", ".alpha { margin: 7rem; }\n"),
        ("alpha.html", "<alpha><beta>gamma</beta></alpha>\n"),
        ("alpha.ts", "export const alpha = `beta gamma`;\n"),
    ];

    for (path, source) in cases {
        let pair = syntax_pair(Path::new(path), source, source, false).unwrap();
        assert_byte_total(&pair.before);
        assert_byte_total(&pair.after);
    }
}

#[test]
fn lowered_leaves_expose_neutral_correspondence_roles() {
    let source = "fn alpha() -> usize { 7 }\n";
    let pair = syntax_pair(Path::new("alpha.rs"), source, source, false).unwrap();

    assert_eq!(leaf_role(&pair.before, "alpha"), LeafRole::Identifier);
    assert_eq!(leaf_role(&pair.before, "7"), LeafRole::Payload);
    assert_eq!(leaf_role(&pair.before, "fn"), LeafRole::Scaffolding);
}

#[test]
fn shorthand_field_owns_its_trailing_comma_and_complete_row() {
    let source = concat!(
        "fn alpha() -> Beta {\n",
        "    Beta {\n",
        "        alpha: beta(),\n",
        "        gamma,\n",
        "    }\n",
        "}\n",
    );
    let pair = syntax_pair(Path::new("alpha.rs"), source, source, false).unwrap();
    let comma = pair
        .before
        .nodes
        .iter()
        .enumerate()
        .map(|(index, _)| NodeId::new(index))
        .find(|id| {
            pair.before.leaf_text(*id) == Some(",") && pair.before.node(*id).lines.start == 4
        })
        .expect("gamma's trailing comma");
    let owner = pair.before.delimiter_owner(comma).expect("comma owner");

    assert_eq!(
        pair.before
            .source
            .slice(pair.before.node(owner).source_envelope.clone()),
        Some("gamma,")
    );
    assert_eq!(
        pair.before.node(owner).source_envelope.end,
        pair.before.node(comma).bytes.end
    );
    assert!(node_owns_complete_lines(&pair.before, owner));
}

#[test]
fn one_failed_parse_falls_both_revisions_back_to_lines() {
    let pair = syntax_pair(Path::new("alpha.rs"), "fn alpha() {}\n", "fn {\n", false).unwrap();

    assert_eq!(pair.before.language, Language::Lines);
    assert_eq!(pair.after.language, Language::Lines);
}

#[test]
fn generated_source_always_uses_lines() {
    let pair = syntax_pair(Path::new("alpha.rs"), "fn {", "fn alpha() {}", true).unwrap();

    assert_eq!(pair.before.language, Language::Lines);
    assert_eq!(pair.after.language, Language::Lines);
}

#[test]
#[should_panic(expected = "review units must be flat beneath the file root")]
fn nested_file_units_are_rejected_before_correspondence() {
    let tree = line::lower(Source::new("alpha\nbeta\n"));
    let SyntaxTree {
        source,
        language,
        root,
        mut nodes,
        ..
    } = tree;
    nodes[2].parent = Some(NodeId::new(1));

    let _ = SyntaxTree::from_nodes(source, language, root, nodes);
}

#[test]
fn html_recovery_restores_source_authored_containment() {
    let before = "<p>\n  <img />\n</p>\n";
    let after = concat!(
        "<p>\n",
        "  <div id=\"alpha\">\n",
        "    <img />\n",
        "  </div>\n",
        "</p>\n",
    );
    let pair = syntax_pair(Path::new("alpha.html"), before, after, false).unwrap();
    let paragraph = node_with_identity(&pair.after, "p");
    let wrapper = node_with_identity(&pair.after, "div");
    let image = node_with_identity(&pair.after, "img");

    assert_eq!(pair.after.language, Language::Html);
    assert!(pair.after.contains(paragraph, wrapper));
    assert!(pair.after.contains(wrapper, image));
    assert_byte_total(&pair.after);
}

#[test]
fn whitespace_sensitive_html_is_one_opaque_leaf() {
    let source = "<pre>\n  <img src=\"alpha\">\n</pre>\n";
    let pair = syntax_pair(Path::new("alpha.html"), source, source, false).unwrap();
    let pre = pair.before.node(node_with_identity(&pair.before, "pre"));

    assert!(pre.children.is_empty());
    assert_eq!(
        pre.leaf.expect("opaque leaf").channel,
        ContentChannel::Opaque
    );
    assert_eq!(
        pair.before.source.slice(pre.bytes.clone()),
        Some(source.trim_end_matches('\n'))
    );
}

fn node_with_identity(tree: &SyntaxTree<'_>, identity: &str) -> NodeId {
    tree.nodes
        .iter()
        .enumerate()
        .map(|(index, _)| NodeId::new(index))
        .find(|id| tree.identity_text(*id) == Some(identity))
        .unwrap_or_else(|| panic!("missing {identity:?} node"))
}

fn leaf_role(tree: &SyntaxTree<'_>, spelling: &str) -> LeafRole {
    tree.nodes
        .iter()
        .enumerate()
        .map(|(index, _)| NodeId::new(index))
        .find(|id| tree.leaf_text(*id) == Some(spelling))
        .and_then(|id| tree.node(id).leaf)
        .map(|leaf| leaf.role)
        .unwrap_or_else(|| panic!("missing {spelling:?} leaf"))
}

fn assert_byte_total(tree: &SyntaxTree<'_>) {
    for (index, node) in tree.nodes.iter().enumerate() {
        if node.leaf.is_some() {
            assert!(node.children.is_empty(), "leaf {index} has children");
            continue;
        }

        let mut cursor = node.bytes.start;
        for child in &node.children {
            let child = tree.node(*child);
            assert_eq!(child.parent, Some(NodeId::new(index)));
            assert_eq!(
                child.bytes.start, cursor,
                "gap or overlap under node {index}"
            );
            cursor = child.bytes.end;
        }
        assert_eq!(
            cursor, node.bytes.end,
            "uncovered suffix under node {index}"
        );
    }
}
