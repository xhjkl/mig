use super::*;
use std::path::Path;

#[test]
fn lowered_syntax_is_byte_total() {
    let cases = [
        ("alpha.rs", "fn alpha() -> usize { 7 }\n"),
        ("alpha.css", ".alpha { margin: 7rem; }\n"),
        ("alpha.html", "<alpha><beta>gamma</beta></alpha>\n"),
        (
            "alpha.html",
            "<p>\n  <div id=\"alpha\">\n    <img />\n  </div>\n</p>\n",
        ),
        ("alpha.ts", "export const alpha = `beta gamma`;\n"),
    ];

    for (path, source) in cases {
        let pair = syntax_pair(Path::new(path), source, source, false).unwrap();
        assert_byte_total(&pair.before);
        assert_byte_total(&pair.after);
    }
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
