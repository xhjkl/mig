use super::*;
use crate::diff::syntax::syntax_pair;
use std::path::Path;

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

    assert!(!graph.tree.unit_leaf_links(unit).iter().any(|link| {
        pair.before.leaf_text(link.before) == Some("gamma")
            && pair.after.leaf_text(link.after) == Some("gamma")
    }));
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

    let physical = physical_line_correspondence_in(
        &pair,
        0..pair.before.source.lines().len(),
        0..pair.after.source.lines().len(),
    );

    assert!(physical.ending_edits.is_empty());
}

fn only_matched(graph: &Correspondence) -> &MatchedUnit {
    let mut matched = graph.tree.units.iter().filter_map(|edit| {
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
