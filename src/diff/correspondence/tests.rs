use super::*;
use crate::diff::syntax::{SyntaxTree, syntax_pair};
use std::path::Path;

#[test]
fn ordered_matching_is_one_to_one_and_noncrossing() {
    let before = ["alpha", "beta", "alpha", "gamma", "delta"];
    let after = ["beta", "alpha", "gamma", "alpha", "delta"];
    let matches = ordered_matches(&before, &after);

    assert!(
        matches
            .iter()
            .all(|edge| before[edge.before] == after[edge.after])
    );
    assert!(
        matches
            .windows(2)
            .all(|pair| { pair[0].before < pair[1].before && pair[0].after < pair[1].after })
    );
}

#[test]
fn large_anchorless_gap_uses_the_bounded_fallback() {
    let before = vec!["alpha"; 200];
    let after = vec!["alpha"; 200];
    let matches = ordered_matches(&before, &after);

    assert_eq!(matches.len(), 200);
    assert_eq!(
        matches.first(),
        Some(&OrderedMatch {
            before: 0,
            after: 0,
        })
    );
    assert_eq!(
        matches.last(),
        Some(&OrderedMatch {
            before: 199,
            after: 199,
        })
    );
}

#[test]
fn fallback_prefers_the_nearest_surviving_run() {
    let before = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta"];
    let after = ["alpha", "epsilon", "zeta", "eta", "beta", "gamma", "delta"];

    assert_eq!(
        pairs(locality_first_matches(&before, &after)),
        [(0, 0), (4, 1), (5, 2)]
    );
}

#[test]
fn scoped_physical_alignment_cannot_cross_opaque_sibling_boundaries() {
    let before = concat!(
        "<style>\n",
        "alpha {\n",
        "  beta: gamma;\n",
        "}\n",
        "delta {\n",
        "  epsilon: zeta;\n",
        "}\n",
        "eta {\n",
        "  theta: iota;\n",
        "}\n",
        "</style>\n",
    );
    let after = concat!(
        "<style>\n",
        "alpha {\n",
        "  beta: gamma;\n",
        "}\n",
        "eta {\n",
        "  theta: iota;\n",
        "}\n",
        "kappa {\n",
        "  lambda: mu;\n",
        "}\n",
        "</style>\n",
    );
    let pair = syntax_pair(Path::new("alpha.html"), before, after, false).expect("HTML syntax");
    let graph = correspond(&pair);
    let mut links = graph
        .line_links
        .iter()
        .chain(&graph.line_ending_edits)
        .copied()
        .collect::<Vec<_>>();
    links.sort_unstable_by_key(|link| (link.before, link.after));

    assert!(
        links
            .windows(2)
            .all(|links| { links[0].before < links[1].before && links[0].after < links[1].after })
    );
    assert!(links.contains(&LineLink {
        before: 3,
        after: 3
    }));
    assert!(links.contains(&LineLink {
        before: 9,
        after: 6
    }));
    assert!(!links.iter().any(|link| link.before == 6));
    assert!(!links.iter().any(|link| link.after == 9));
}

#[test]
fn exact_leaf_cannot_cross_between_sealed_sibling_parents() {
    let pair = syntax_pair(
        Path::new("alpha.rs"),
        "fn alpha() { beta(gamma); delta(epsilon); }\n",
        "fn alpha() { beta(epsilon); delta(gamma); }\n",
        false,
    )
    .expect("Rust syntax");
    let graph = correspond(&pair);
    let unit = only_matched(&graph);

    assert!(!graph.unit_leaf_links(unit).iter().any(|link| {
        pair.before.leaf_text(link.before) == Some("gamma")
            && pair.after.leaf_text(link.after) == Some("gamma")
    }));
}

#[test]
fn unique_wrapper_path_can_cross_multiple_transparent_nodes() {
    let before = "const alpha = await beta({ gamma: true, delta: true });\n";
    let after = "const alpha = (await beta({ gamma: true, delta: true })) as Epsilon | null;\n";
    let pair = syntax_pair(Path::new("alpha.ts"), before, after, false).expect("TypeScript syntax");
    let graph = correspond(&pair);
    let retained = graph
        .composites
        .iter()
        .find(|link| {
            link.wrapper == Some(Reparenting::Wrapped)
                && pair
                    .before
                    .source
                    .slice(pair.before.node(link.before).bytes.clone())
                    == Some("await beta({ gamma: true, delta: true })")
        })
        .expect("wrapped expression correspondence");

    assert_eq!(
        pair.after
            .source
            .slice(pair.after.node(retained.after).bytes.clone()),
        Some("await beta({ gamma: true, delta: true })")
    );
}

#[test]
fn trailing_delimiter_correspondence_stays_with_its_field() {
    let before = concat!(
        "fn alpha() -> Beta {\n",
        "    Beta {\n",
        "        alpha: delta(),\n",
        "        gamma,\n",
        "        epsilon: None,\n",
        "    }\n",
        "}\n",
    );
    let after = before.replace("alpha: delta()", "alpha: eta()");
    let pair = syntax_pair(Path::new("alpha.rs"), before, &after, false).expect("Rust syntax");
    let graph = correspond(&pair);
    let before_comma = comma_owned_on_line(&pair.before, 3);
    let after_comma = comma_owned_on_line(&pair.after, 3);
    let link = graph
        .before_leaf_link(before_comma)
        .expect("gamma's comma correspondence");

    assert_eq!(link.after, after_comma);
    assert_eq!(link.parent, ParentCorrespondence::Direct);
}

#[test]
fn unequal_physical_gaps_do_not_invent_line_ending_pairs() {
    let pair = syntax_pair(
        Path::new("alpha.txt"),
        "alpha\nbeta\ngamma\n",
        "alpha\ndelta\r\nepsilon\ngamma\n",
        false,
    )
    .expect("line syntax");

    assert!(physical_line_correspondence(&pair).ending_edits.is_empty());
}

fn pairs(matches: Vec<OrderedMatch>) -> Vec<(usize, usize)> {
    matches
        .into_iter()
        .map(|edge| (edge.before, edge.after))
        .collect()
}

fn comma_owned_on_line(tree: &SyntaxTree<'_>, line: usize) -> NodeId {
    tree.nodes
        .iter()
        .enumerate()
        .map(|(index, _)| NodeId::new(index))
        .find(|id| {
            tree.leaf_text(*id) == Some(",")
                && tree.node(*id).lines.start == line
                && tree.delimiter_owner(*id).is_some()
        })
        .unwrap_or_else(|| panic!("missing owned comma on line {line}"))
}

fn only_matched(graph: &Correspondence) -> &MatchedUnit {
    let mut matched = graph.units.iter().filter_map(|edit| {
        let UnitEdit::Matched(unit) = edit else {
            return None;
        };
        Some(unit)
    });
    let unit = matched.next().expect("one matched unit");
    assert!(
        matched.next().is_none(),
        "expected exactly one matched unit"
    );
    unit
}
