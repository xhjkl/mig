use super::*;
use std::path::Path;

#[test]
fn lowered_syntax_is_byte_total() {
    let cases = [
        ("alpha.rs", "fn alpha() -> usize { 7 }\n"),
        (
            "alpha.nix",
            "{ alpha, beta ? 1 }: let gamma = ./delta; in { inherit alpha; epsilon = [ beta \"a ${gamma} b\" ]; zeta = ''a\n  b''; }\n",
        ),
        (
            "alpha.toml",
            "# alpha\n[beta.gamma]\n\"delta epsilon\" = [1, { zeta = 'a b' }]\n[[eta]]\ntheta = \"\"\"a\n  b\"\"\"\n",
        ),
        (
            "alpha.json",
            "{\n  \"alpha\": [1, {\"beta\": \"a b\"}],\n  \"gamma\": null\n}\n",
        ),
        (
            "alpha.cpp",
            "#include <vector>\nnamespace alpha { template<class T> struct Beta { T gamma; T delta() const { return gamma; } }; }\n",
        ),
        ("alpha.C", "namespace alpha { int beta() { return 1; } }\n"),
        ("alpha.H", "template<class T> class Alpha { T beta; };\n"),
        (
            "alpha.go",
            "package alpha\n\nimport \"fmt\"\n\ntype Beta[T any] struct { Gamma T }\nfunc (b Beta[T]) Delta() { fmt.Println(`a b`) }\n",
        ),
        (
            "alpha.py",
            "@beta\ndef alpha():\n    if gamma:\n        return f' x {delta} '\n    return '''a\n    b'''\n",
        ),
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
        assert!(
            pair.before.grammar.is_some(),
            "{path} unexpectedly fell back to lines"
        );
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
