use super::*;
use crate::diff::projection::{ReviewMode, project_pair};
use std::path::Path;

fn pairs(matches: Vec<OrderedMatch>) -> Vec<(usize, usize)> {
    matches
        .into_iter()
        .map(|edge| (edge.before, edge.after))
        .collect()
}

fn is_descendant(projection: &Projection<'_>, node: NodeId, ancestor: NodeId) -> bool {
    let mut parent = projection.node(node).parent;
    while let Some(candidate) = parent {
        if candidate == ancestor {
            return true;
        }
        parent = projection.node(candidate).parent;
    }
    false
}

fn composite_on_line(projection: &Projection<'_>, root: NodeId, kind: &str, line: usize) -> NodeId {
    descendant_composites(projection, root)
        .into_iter()
        .find(|id| {
            let node = projection.node(*id);
            node.kind == kind && node.lines.start == line
        })
        .unwrap_or_else(|| panic!("missing {kind:?} on line {line}"))
}

fn composite_link(graph: &Correspondence, before: NodeId) -> NodeLink {
    graph
        .composites
        .iter()
        .find(|link| link.before == before)
        .copied()
        .expect("before composite must remain linked")
}

#[test]
fn insertions_and_removals_leave_surrounding_edges_stable() {
    let before = ["a", "gone", "b", "c"];
    let after = ["a", "b", "new", "c"];

    assert_eq!(
        pairs(ordered_matches(&before, &after)),
        vec![(0, 0), (2, 1), (3, 3)]
    );
}

#[test]
fn repeated_values_are_retained_inside_anchored_gaps() {
    let before = ["same", "anchor-a", "same", "anchor-b", "same"];
    let after = ["same", "anchor-a", "anchor-b", "same", "same"];

    assert_eq!(
        pairs(ordered_matches(&before, &after)),
        vec![(0, 0), (1, 1), (3, 2), (4, 3)]
    );
}

#[test]
fn crossing_unique_candidates_choose_a_deterministic_stable_subsequence() {
    let before = ["a", "b", "c", "d"];
    let after = ["b", "c", "a", "d"];

    assert_eq!(
        pairs(ordered_matches(&before, &after)),
        vec![(1, 0), (2, 1), (3, 3)]
    );
}

#[test]
fn large_anchorless_gap_uses_linear_memory_fallback() {
    let before = vec!["same"; 200];
    let after = vec!["same"; 200];

    let matches = ordered_matches(&before, &after);

    assert_eq!(matches.len(), 200);
    assert_eq!(matches.first(), Some(&OrderedMatch::new(0, 0)));
    assert_eq!(matches.last(), Some(&OrderedMatch::new(199, 199)));
}

#[test]
fn greedy_fallback_discards_positions_that_would_cross() {
    let before = ["a", "b", "a", "c"];
    let after = ["b", "a", "c", "a"];

    assert_eq!(pairs(greedy_matches(&before, &after)), vec![(0, 1), (2, 3)]);
}

#[test]
fn every_result_is_equal_unique_and_strictly_ordered() {
    let before = [0, 1, 2, 1, 3, 4, 3, 5];
    let after = [1, 0, 1, 2, 4, 3, 5, 3];
    let matches = ordered_matches(&before, &after);

    for edge in &matches {
        assert_eq!(before[edge.before], after[edge.after]);
    }
    for pair in matches.windows(2) {
        assert!(pair[0].before < pair[1].before);
        assert!(pair[0].after < pair[1].after);
    }
}

#[test]
fn line_projection_is_the_same_ordered_unit_graph() {
    let pair = project_pair(
        Path::new("notes.txt"),
        "alpha\nold\nomega\n",
        "alpha\nnew\nomega\n",
        false,
    )
    .expect("line projection cannot fail");
    let graph = correspond(&pair);

    assert!(matches!(
        graph.units.as_slice(),
        [
            UnitEdit::Matched(MatchedUnit {
                relation: ContentRelation::SourceEqual,
                ..
            }),
            UnitEdit::Matched(MatchedUnit {
                relation: ContentRelation::Modified,
                ..
            }),
            UnitEdit::Matched(MatchedUnit {
                relation: ContentRelation::SourceEqual,
                ..
            })
        ]
    ));
    let UnitEdit::Matched(alpha) = &graph.units[0] else {
        unreachable!();
    };
    let [link] = graph.unit_leaf_links(alpha) else {
        panic!("one line unit must own one leaf link");
    };
    assert_eq!(link.relation, LeafRelation::Exact);
    let before_link =
        graph.before_leaf[link.before.index()].and_then(|index| graph.leaf_links.get(index));
    assert_eq!(before_link, Some(link));
    assert_eq!(graph.after_leaf_link(link.after), Some(link));
    let UnitEdit::Matched(changed) = &graph.units[1] else {
        unreachable!();
    };
    let [link] = graph.unit_leaf_links(changed) else {
        panic!("one changed line unit must own one leaf link");
    };
    assert_eq!(link.relation, LeafRelation::Modified);
}

#[test]
fn fallback_lines_prefer_the_nearest_surviving_run_over_extracted_payload() {
    let before = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta"];
    let after = [
        "alpha", "epsilon", "zeta", "theta", "beta", "gamma", "delta",
    ];

    assert_eq!(
        pairs(locality_first_matches(&before, &after)),
        vec![(0, 0), (4, 1), (5, 2)]
    );
}

#[test]
fn physical_line_links_are_exact_and_keep_absolute_coordinates() {
    let pair = project_pair(
        Path::new("notes.txt"),
        "alpha\nbeta\ngamma\n",
        "new\nalpha\nbeta\ngamma\n",
        false,
    )
    .expect("line projection cannot fail");
    let graph = correspond(&pair);

    assert_eq!(
        graph.line_links,
        [
            LineLink {
                before: 0,
                after: 1,
            },
            LineLink {
                before: 1,
                after: 2,
            },
            LineLink {
                before: 2,
                after: 3,
            },
        ]
    );
    assert_eq!(
        graph.line_links_in(1..3, 2..4).collect::<Vec<_>>(),
        [
            LineLink {
                before: 1,
                after: 2,
            },
            LineLink {
                before: 2,
                after: 3,
            },
        ]
    );
}

#[test]
fn source_exactness_keeps_adjacent_blank_layout_as_local_fallback() {
    let adjacent = project_pair(
        Path::new("lib.rs"),
        "fn run() {}\n",
        "\nfn run() { work(); }\n",
        false,
    )
    .expect("Rust projection must parse");
    assert_eq!(
        correspond(&adjacent).line_fallbacks,
        [LineFallback {
            before: 0..0,
            after: 0..1,
        }]
    );

    let outside_layout = project_pair(
        Path::new("lib.rs"),
        "fn run() {}\n",
        "\n\nfn run() { work(); }\n",
        false,
    )
    .expect("Rust projection must parse");
    assert_eq!(
        correspond(&outside_layout).line_fallbacks,
        [LineFallback {
            before: 0..0,
            after: 0..2,
        }]
    );
}

#[test]
fn equal_blank_separators_travel_with_reordered_definitions() {
    let before = "fn first() {\n    first_body();\n}\n\nfn second() {\n    second_body();\n}\n";
    let after = "fn second() {\n    second_body();\n}\n\nfn first() {\n    first_body();\n}\n";
    let pair = project_pair(Path::new("lib.rs"), before, after, false)
        .expect("Rust projection must parse");

    assert_eq!(correspond(&pair).line_fallbacks, []);
}

#[test]
fn growing_a_shared_blank_separator_remains_local_edit_signal() {
    let pair = project_pair(
        Path::new("lib.rs"),
        "fn first() {}\n\nfn second() {}\n",
        "fn first() {}\n\n\nfn second() {}\n",
        false,
    )
    .expect("Rust projection must parse");
    let graph = correspond(&pair);

    assert!(
        graph
            .line_fallbacks
            .iter()
            .any(|fallback| fallback.after.contains(&2)),
        "the second current blank row must remain source signal: {graph:#?}",
    );
}

#[test]
fn blank_layout_cannot_anchor_across_unrelated_semantic_containers() {
    let before = concat!(
        "fn alpha() {\n",
        "    beta();\n",
        "\n",
        "    gamma();\n",
        "}\n",
    );
    let after = concat!(
        "fn delta() {\n",
        "    beta();\n",
        "\n",
        "    gamma();\n",
        "}\n",
        "\n",
        "fn alpha() {\n",
        "    theta();\n",
        "}\n",
    );
    let pair = project_pair(Path::new("alpha.rs"), before, after, false)
        .expect("Rust projection must parse");
    let graph = correspond(&pair);

    assert!(
        scope_lines(&pair.before)
            .iter()
            .all(|owners| !owners.is_empty()),
        "every previous physical line must carry its nearest semantic scope",
    );
    assert!(
        scope_lines(&pair.after)
            .iter()
            .all(|owners| !owners.is_empty()),
        "every current physical line must carry its nearest semantic scope",
    );
    assert!(
        !graph.line_links.contains(&LineLink {
            before: 2,
            after: 2,
        }),
        "the blank line in the previous alpha body cannot anchor into delta",
    );
}

#[test]
fn a_rejected_context_claim_preserves_the_existing_partner() {
    let mut links = ContextLinks {
        before: HashMap::new(),
        after: HashMap::new(),
        placement: HashMap::new(),
        reparenting: HashMap::new(),
    };
    let before = NodeId::new(0);
    let after = NodeId::new(1);
    let other_before = NodeId::new(2);
    let other_after = NodeId::new(3);

    assert!(link_context(
        before,
        after,
        Placement::Stable,
        None,
        &mut links,
    ));
    assert!(!link_context(
        before,
        other_after,
        Placement::Reordered,
        None,
        &mut links,
    ));
    assert!(!link_context(
        other_before,
        after,
        Placement::Stable,
        Some(Reparenting::Wrapped),
        &mut links,
    ));

    assert_eq!(links.before.get(&before), Some(&after));
    assert_eq!(links.after.get(&after), Some(&before));
    assert!(!links.before.contains_key(&other_before));
    assert!(!links.after.contains_key(&other_after));
}

#[test]
fn fallback_normalization_merges_interleaved_overlap_components() {
    let normalized = normalize_line_fallbacks(vec![
        LineFallback {
            before: 0..2,
            after: 0..1,
        },
        LineFallback {
            before: 10..11,
            after: 1..2,
        },
        LineFallback {
            before: 1..3,
            after: 2..3,
        },
    ]);

    assert_eq!(
        normalized,
        [LineFallback {
            before: 0..11,
            after: 0..3,
        }]
    );
}

#[test]
fn fallback_closure_scales_across_many_disjoint_changed_units() {
    const REGION_COUNT: usize = 2_048;

    let mut fallbacks = (0..REGION_COUNT)
        .map(|index| {
            let start = index * 4;
            LineFallback {
                before: start..start + 1,
                after: start..start + 1,
            }
        })
        .collect::<Vec<_>>();
    let units = (0..REGION_COUNT)
        .map(|index| {
            let start = index * 4;
            UnitLineGeometry {
                before: start..start + 2,
                after: start..start + 2,
                changed: true,
                expands_fallback: true,
            }
        })
        .collect::<Vec<_>>();

    close_fallbacks_over_changed_units(&mut fallbacks, &units);

    assert_eq!(fallbacks.len(), REGION_COUNT);
    for (index, fallback) in fallbacks.iter().enumerate() {
        let start = index * 4;
        assert_eq!(fallback.before, start..start + 2);
        assert_eq!(fallback.after, start..start + 2);
    }
}

#[test]
fn terminator_delta_is_local_to_its_physical_row() {
    let pair = project_pair(
        Path::new("alpha.rs"),
        "fn alpha() { old(); }\nfn stable() {\n    same();\n}\n",
        "fn alpha() { new(); }\nfn stable() {\r\n    same();\n}\n",
        false,
    )
    .expect("Rust projection must parse");
    let graph = correspond(&pair);

    assert_eq!(
        graph.line_ending_edits,
        [LineLink {
            before: 1,
            after: 1,
        }]
    );
    assert_eq!(
        graph.line_fallbacks,
        [LineFallback {
            before: 1..2,
            after: 1..2,
        }]
    );
}

#[test]
fn source_formatting_and_comment_edits_have_distinct_relations() {
    let formatting = project_pair(
        Path::new("lib.rs"),
        "fn run(){work();}\n",
        "fn run() {\n    work();\n}\n",
        false,
    )
    .expect("Rust projection must parse");
    let formatting = correspond(&formatting);
    let formatting = only_matched(&formatting);
    assert_eq!(formatting.relation, ContentRelation::FullEqual);
    assert!(formatting.relation.full_equal());
    assert!(!formatting.relation.source_equal());

    let comment = project_pair(
        Path::new("lib.rs"),
        "fn run() {\n    // old\n    work();\n}\n",
        "fn run() {\n    // new\n    work();\n}\n",
        false,
    )
    .expect("Rust projection must parse");
    let comment = correspond(&comment);
    let unit = only_matched(&comment);
    assert_eq!(unit.relation, ContentRelation::PayloadEqual);
    assert!(unit.relation.payload_equal());
    assert!(!unit.relation.full_equal());
    assert!(
        comment
            .unit_leaf_links(unit)
            .iter()
            .any(|link| link.relation == LeafRelation::Modified)
    );
}

#[test]
fn changed_leaf_payload_is_a_modified_structural_link() {
    let pair = project_pair(
        Path::new("lib.rs"),
        "fn run() { old(); }\n",
        "fn run() { new(); }\n",
        false,
    )
    .expect("Rust projection must parse");
    let graph = correspond(&pair);
    let unit = only_matched(&graph);

    assert_eq!(unit.relation, ContentRelation::Modified);
    let modified = graph
        .unit_leaf_links(unit)
        .iter()
        .find(|link| link.relation == LeafRelation::Modified)
        .expect("same-shaped identifier payloads must remain linked");
    assert_eq!(pair.before.leaf_text(modified.before), Some("old"));
    assert_eq!(pair.after.leaf_text(modified.after), Some("new"));
}

#[test]
fn duplicate_definition_names_retain_ordinal_identity_without_body_voting() {
    let pair = project_pair(
        Path::new("lib.rs"),
        "fn same() { one(); }\nfn same() { two(); }\n",
        "fn same() { two(); }\nfn same() { one(); }\n",
        false,
    )
    .expect("Rust projection must parse");
    let graph = correspond(&pair);
    let matched = graph
        .units
        .iter()
        .filter_map(|edit| {
            let UnitEdit::Matched(unit) = edit else {
                return None;
            };
            Some(unit)
        })
        .collect::<Vec<_>>();

    assert_eq!(matched.len(), 2);
    assert!(
        matched
            .iter()
            .all(|unit| unit.placement == Placement::Stable)
    );
    for unit in matched {
        let before = pair
            .before
            .source
            .slice(pair.before.node(unit.before).bytes.clone())
            .unwrap();
        let after = pair
            .after
            .source
            .slice(pair.after.node(unit.after).bytes.clone())
            .unwrap();
        assert_ne!(before, after);
        assert_eq!(unit.relation, ContentRelation::Modified);
    }
}

#[test]
fn following_attribute_uses_its_definition_to_disambiguate_repeated_source() {
    let before = concat!(
        "#[derive(Clone, Copy)]\n",
        "enum Alpha {\n",
        "    Beta,\n",
        "    Gamma,\n",
        "}\n",
        "#[derive(Clone, Copy)]\n",
        "enum Delta {\n",
        "    Epsilon,\n",
        "}\n",
    );
    let after = concat!(
        "#[derive(Clone, Copy)]\n",
        "enum Zeta {\n",
        "    Gamma,\n",
        "    Beta,\n",
        "}\n",
    );
    let pair = project_pair(Path::new("alpha.rs"), before, after, false)
        .expect("Rust projection must parse");
    let graph = correspond(&pair);
    let review = graph
        .units
        .iter()
        .find_map(|edit| {
            let UnitEdit::Matched(unit) = edit else {
                return None;
            };
            (pair.after.identity_text(unit.after) == Some("Zeta")).then_some(unit)
        })
        .expect("renamed enum must remain paired");
    assert_eq!(pair.before.identity_text(review.before), Some("Alpha"));

    let derive = graph
        .units
        .iter()
        .find_map(|edit| {
            let UnitEdit::Matched(unit) = edit else {
                return None;
            };
            let after = pair.after.node(unit.after);
            (after.kind == "attribute_item" && after.lines.start == 1).then_some(unit)
        })
        .expect("derive attribute must remain paired");
    assert_eq!(pair.before.node(derive.before).lines.start, 1);
}

#[test]
fn following_attributes_respect_established_owner_identity() {
    let before = concat!(
        "#[test]\n",
        "fn alpha() { alpha_body(); }\n",
        "#[test]\n",
        "fn beta() { beta_body(); }\n",
    );
    let after = concat!(
        "#[test]\n",
        "fn alpha() { beta_body(); }\n",
        "#[test]\n",
        "fn beta() { alpha_body(); }\n",
    );
    let pair = project_pair(Path::new("lib.rs"), before, after, false)
        .expect("Rust projection must parse");
    let graph = correspond(&pair);
    let mut attributes = graph
        .units
        .iter()
        .filter_map(|edit| {
            let UnitEdit::Matched(unit) = edit else {
                return None;
            };
            (pair.before.node(unit.before).kind == "attribute_item").then(|| {
                (
                    pair.before.node(unit.before).lines.start,
                    pair.after.node(unit.after).lines.start,
                )
            })
        })
        .collect::<Vec<_>>();
    attributes.sort_unstable();

    assert_eq!(attributes, [(1, 1), (3, 3)]);
}

#[test]
fn top_level_decorations_follow_matched_definition_owners() {
    let before = concat!(
        "#[derive(Clone)]\n",
        "// explanatory comment\n",
        "/// Alpha documentation.\n",
        "struct Alpha { value: u8 }\n",
        "\n",
        "#[derive(Clone)]\n",
        "/// Beta documentation.\n",
        "struct Beta { value: u16 }\n",
    );
    let after = concat!(
        "#[derive(Clone)]\n",
        "/// Beta documentation.\n",
        "struct Beta { value: u32 }\n",
        "\n",
        "#[derive(Clone)]\n",
        "// explanatory comment\n",
        "/// Alpha documentation.\n",
        "struct Alpha { value: u8 }\n",
    );
    let pair = project_pair(Path::new("lib.rs"), before, after, false)
        .expect("Rust projection must parse");
    let graph = correspond(&pair);

    for before_line in [1, 3, 6, 7] {
        let decoration = graph
            .units
            .iter()
            .find_map(|edit| {
                let UnitEdit::Matched(unit) = edit else {
                    return None;
                };
                let node = pair.before.node(unit.before);
                (node.lines.start == before_line && node.decoration_owner.is_some()).then_some(unit)
            })
            .expect("every decoration must remain paired");
        let before_owner = pair
            .before
            .node(decoration.before)
            .decoration_owner
            .expect("before decoration owner");
        let after_owner = pair
            .after
            .node(decoration.after)
            .decoration_owner
            .expect("after decoration owner");
        assert_eq!(
            pair.before.identity_text(before_owner),
            pair.after.identity_text(after_owner),
            "a repeated decoration must not cross semantic owners",
        );
        let owner = graph
            .units
            .iter()
            .find_map(|edit| {
                let UnitEdit::Matched(unit) = edit else {
                    return None;
                };
                (unit.before == before_owner && unit.after == after_owner).then_some(unit)
            })
            .expect("a decoration can pair only through a matched owner");
        assert_eq!(decoration.placement, owner.placement);
    }

    let comment = graph
        .units
        .iter()
        .find_map(|edit| {
            let UnitEdit::Matched(unit) = edit else {
                return None;
            };
            let before = pair.before.node(unit.before);
            (before.kind == "line_comment" && before.lines.start == 2).then_some(unit)
        })
        .expect("ordinary commentary remains independently reviewable");
    assert_eq!(pair.before.node(comment.before).decoration_owner, None);
    assert_eq!(pair.after.node(comment.after).decoration_owner, None);
    assert_eq!(pair.after.node(comment.after).lines.start, 6);
}

#[test]
fn structural_decorations_inherit_movement_without_voting_on_it() {
    let fingerprints = |index| NodeFingerprints {
        full: FingerprintId(10_000 + index),
        payload: Some(FingerprintId(20_000 + index)),
        shape: FingerprintId(30_000 + index),
    };
    let unit = |id: usize, decoration_owner: Option<usize>| UnitRecord {
        id: NodeId::new(id),
        kind: "synthetic",
        identity: None,
        atomic: false,
        decoration_owner: decoration_owner.map(NodeId::new),
        fingerprint: fingerprints(id),
        shape: FingerprintId(30_000 + id),
        mode: ReviewMode::Structural,
    };
    // The decorator crosses the other semantic owner. If it entered the LIS,
    // it would choose a different definition as the apparent move.
    let before = [unit(11, Some(10)), unit(10, None), unit(12, None)];
    let after = [unit(22, None), unit(21, Some(20)), unit(20, None)];
    let before_match = [Some(1), Some(2), Some(0)];
    let after_match = [Some(2), Some(0), Some(1)];

    assert_eq!(
        stable_unit_matches(&before_match, &after_match, &before, &after),
        [false, false, true],
    );
}

#[test]
fn root_owned_inner_doc_stays_ahead_of_a_removed_neighbor() {
    let before = concat!(
        "//! Crate contract.\n",
        "use crate::obsolete;\n",
        "use crate::kept;\n",
    );
    let after = concat!("//! Crate contract.\n", "use crate::kept;\n");
    let pair = project_pair(Path::new("lib.rs"), before, after, false)
        .expect("Rust projection must parse");
    let graph = correspond(&pair);
    let doc = graph
        .units
        .iter()
        .position(|edit| {
            matches!(
                edit,
                UnitEdit::Matched(unit)
                    if pair.before.node(unit.before).kind == "line_comment"
                        && unit.relation == ContentRelation::SourceEqual
            )
        })
        .expect("the unchanged inner doc must remain paired through the source root");
    let removed = graph
        .units
        .iter()
        .position(|edit| {
            matches!(
                edit,
                UnitEdit::Removed { before }
                    if pair.before.identity_text(*before) == Some("crate::obsolete")
            )
        })
        .expect("the obsolete import must remain a removal");

    assert!(
        doc < removed,
        "source-order serialization must retain the root decoration"
    );
}

#[test]
fn repeated_decoration_cannot_escape_a_removed_owner() {
    let before = concat!(
        "#[derive(Clone)]\n",
        "struct Alpha;\n",
        "#[derive(Clone)]\n",
        "struct Beta;\n",
    );
    let after = concat!("#[derive(Clone)]\n", "struct Beta;\n");
    let pair = project_pair(Path::new("lib.rs"), before, after, false)
        .expect("Rust projection must parse");
    let graph = correspond(&pair);
    let matched_attributes = graph
        .units
        .iter()
        .filter_map(|edit| {
            let UnitEdit::Matched(unit) = edit else {
                return None;
            };
            (pair.before.node(unit.before).kind == "attribute_item").then_some(unit)
        })
        .collect::<Vec<_>>();

    assert_eq!(matched_attributes.len(), 1);
    assert_eq!(
        pair.before.node(matched_attributes[0].before).lines.start,
        3
    );
    assert_eq!(pair.after.node(matched_attributes[0].after).lines.start, 1);
    assert!(graph.units.iter().any(|edit| {
        matches!(edit, UnitEdit::Removed { before }
            if pair.before.node(*before).kind == "attribute_item"
                && pair.before.node(*before).lines.start == 1)
    }));
}

#[test]
fn nested_repeated_decorations_inherit_their_function_correspondence() {
    let before = concat!(
        "mod tests {\n",
        "#[test]\n",
        "/// Exercise contract.\n",
        "fn alpha() { old(); }\n",
        "#[test]\n",
        "/// Exercise contract.\n",
        "fn beta() { beta_body(); }\n",
        "}\n",
    );
    let after = concat!(
        "mod tests {\n",
        "#[test]\n",
        "/// Exercise contract.\n",
        "fn beta() { beta_body(); }\n",
        "#[test]\n",
        "/// Exercise contract.\n",
        "fn alpha() { new(); }\n",
        "}\n",
    );
    let pair = project_pair(Path::new("lib.rs"), before, after, false)
        .expect("Rust projection must parse");
    let graph = correspond(&pair);
    let unit = only_matched(&graph);

    for (name, before_attribute_line, before_docs_line, after_attribute_line, after_docs_line) in
        [("alpha", 2, 3, 5, 6), ("beta", 5, 6, 2, 3)]
    {
        let before_attribute = composite_on_line(
            &pair.before,
            unit.before,
            "attribute_item",
            before_attribute_line,
        );
        let after_attribute = composite_on_line(
            &pair.after,
            unit.after,
            "attribute_item",
            after_attribute_line,
        );
        let before_docs = pair
            .before
            .nodes
            .iter()
            .position(|node| node.kind == "line_comment" && node.lines.start == before_docs_line)
            .map(NodeId::new)
            .expect("before documentation leaf");
        let after_docs = pair
            .after
            .nodes
            .iter()
            .position(|node| node.kind == "line_comment" && node.lines.start == after_docs_line)
            .map(NodeId::new)
            .expect("after documentation leaf");
        let attribute = composite_link(&graph, before_attribute);
        let docs = graph
            .before_leaf_link(before_docs)
            .expect("documentation leaf must remain paired");

        assert_eq!(attribute.after, after_attribute);
        assert_eq!(docs.after, after_docs);
        assert_eq!(attribute.placement, docs.placement);
        assert_eq!(
            pair.before
                .identity_text(pair.before.node(before_attribute).decoration_owner.unwrap()),
            Some(name),
        );
        assert_eq!(
            pair.after
                .identity_text(pair.after.node(after_attribute).decoration_owner.unwrap()),
            Some(name),
        );
        assert_eq!(
            pair.before
                .identity_text(pair.before.node(before_docs).decoration_owner.unwrap()),
            Some(name),
        );
        assert_eq!(
            pair.after
                .identity_text(pair.after.node(after_docs).decoration_owner.unwrap()),
            Some(name),
        );
    }
}

#[test]
fn compact_units_stay_stable_and_do_not_vote_in_structural_movement() {
    let pair = project_pair(
        Path::new("view.ts"),
        "function alpha() {}\nimport value from \"pkg\";\nfunction beta() {}\n",
        "function beta() {}\nimport value from \"pkg\";\nfunction alpha() {}\n",
        false,
    )
    .expect("TypeScript projection must parse");
    let graph = correspond(&pair);
    let mut reordered_payload = 0;
    for edit in &graph.units {
        let UnitEdit::Matched(unit) = edit else {
            continue;
        };
        if unit.mode == ReviewMode::Compact {
            assert_eq!(unit.placement, Placement::Stable);
        } else if unit.placement == Placement::Reordered {
            reordered_payload += 1;
        }
    }
    assert_eq!(reordered_payload, 1);
}

#[test]
fn mixed_module_modes_resolve_symmetrically_to_linewise_review() {
    let inline = "mod subject { pub fn payload() {} }\n";
    let bodyless = "mod subject;\n";

    for (before, after) in [(inline, bodyless), (bodyless, inline)] {
        let pair = project_pair(Path::new("lib.rs"), before, after, false)
            .expect("Rust projection must parse");
        let graph = correspond(&pair);
        let unit = only_matched(&graph);

        assert_eq!(unit.mode, ReviewMode::Linewise);
        assert_eq!(unit.placement, Placement::Stable);
        assert_eq!(unit.relation, ContentRelation::Modified);
    }
}

#[test]
fn exact_html_child_survives_an_inserted_parent_as_one_reparented_subtree() {
    let before = "<article>\n  <img src=\"alpha.webp\">\n</article>\n";
    let after =
        "<article>\n  <div class=\"beta\">\n    <img src=\"alpha.webp\">\n  </div>\n</article>\n";
    let pair = project_pair(Path::new("view.html"), before, after, false)
        .expect("HTML projection must parse");
    let graph = correspond(&pair);
    let unit = only_matched(&graph);
    let retained = graph
        .unit_composites(unit)
        .iter()
        .find(|link| {
            pair.before.identity_text(link.before) == Some("img")
                && pair.after.identity_text(link.after) == Some("img")
        })
        .expect("the exact img subtree must remain linked across the wrapper");

    assert!(retained.wrapper.is_some());
    assert!(
        graph
            .unit_leaf_links(unit)
            .iter()
            .any(|link| link.relation == LeafRelation::Exact
                && is_descendant(&pair.before, link.before, retained.before)
                && is_descendant(&pair.after, link.after, retained.after))
    );
}

#[test]
fn same_tag_wrapper_is_detected_from_actual_parent_correspondence() {
    let before = "<div><img src=\"alpha.webp\"></div>\n";
    let after = "<div><div><img src=\"alpha.webp\"></div></div>\n";
    let pair = project_pair(Path::new("view.html"), before, after, false)
        .expect("HTML projection must parse");
    let graph = correspond(&pair);
    let unit = only_matched(&graph);
    let retained = graph
        .unit_composites(unit)
        .iter()
        .find(|link| {
            pair.before.identity_text(link.before) == Some("img")
                && pair.after.identity_text(link.after) == Some("img")
        })
        .expect("the image must retain identity through the inserted div");

    assert!(retained.wrapper.is_some());
}

#[test]
fn repeated_html_siblings_keep_fifo_identity_instead_of_following_payload() {
    let before = "<article><p>alpha</p><p>beta</p></article>\n";
    let after = "<article><p>beta</p><p>alpha</p></article>\n";
    let pair = project_pair(Path::new("view.html"), before, after, false)
        .expect("HTML projection must parse");
    let graph = correspond(&pair);
    let unit = only_matched(&graph);
    assert!(!graph.unit_composites(unit).iter().any(|link| {
        pair.before.node(link.before).kind == "element"
            && pair.before.identity_text(link.before) == Some("p")
    }));
    assert!(!graph.unit_leaf_links(unit).iter().any(|link| {
        link.relation == LeafRelation::Exact
            && pair.before.leaf_text(link.before) == Some("alpha")
            && pair.after.leaf_text(link.after) == Some("alpha")
    }));
}

#[test]
fn nested_html_unwrap_keeps_leaf_links_one_to_one() {
    let pair = project_pair(
        Path::new("view.html"),
        "<div><div><img></div></div>\n",
        "<div><img></div>\n",
        false,
    )
    .expect("HTML projection must parse");
    let graph = correspond(&pair);
    let before = graph
        .leaf_links
        .iter()
        .map(|link| link.before)
        .collect::<HashSet<_>>();
    let after = graph
        .leaf_links
        .iter()
        .map(|link| link.after)
        .collect::<HashSet<_>>();

    assert_eq!(before.len(), graph.leaf_links.len());
    assert_eq!(after.len(), graph.leaf_links.len());
    assert!(graph.composites.iter().any(|link| {
        link.wrapper == Some(Reparenting::Unwrapped)
            && pair.before.identity_text(link.before) == Some("img")
            && pair.after.identity_text(link.after) == Some("img")
    }));
}

#[test]
fn nested_html_unwrap_can_reach_the_matched_document_owner() {
    let pair = project_pair(
        Path::new("alpha.html"),
        "<alpha><beta><img src=\"gamma.webp\"></beta></alpha>\n",
        "<img src=\"gamma.webp\">\n",
        false,
    )
    .expect("HTML projection must parse");
    let graph = correspond(&pair);

    assert!(graph.composites.iter().any(|link| {
        link.wrapper == Some(Reparenting::Unwrapped)
            && pair.before.identity_text(link.before) == Some("img")
            && pair.after.identity_text(link.after) == Some("img")
    }));
}

#[test]
fn nested_type_assertion_reaches_the_matched_declaration_owner() {
    let pair = project_pair(
        Path::new("alpha.ts"),
        "const alpha = await beta({ gamma: true, delta: true });\n",
        "const alpha = (await beta({ gamma: true, delta: true })) as Epsilon | null;\n",
        false,
    )
    .expect("TypeScript projection must parse");
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
        .expect("the exact expression must carry a containment certificate");
    assert_eq!(
        pair.after
            .source
            .slice(pair.after.node(retained.after).bytes.clone()),
        Some("await beta({ gamma: true, delta: true })")
    );
}

#[test]
fn renamed_nested_definition_preserves_parent_links_for_exact_body_leaves() {
    let before = concat!(
        "mod alpha {\n",
        "    fn alpha() {\n",
        "        beta();\n",
        "    }\n",
        "}\n",
    );
    let after = concat!(
        "mod alpha {\n",
        "    fn gamma() {\n",
        "        beta();\n",
        "    }\n",
        "}\n",
    );
    let pair = project_pair(Path::new("alpha.rs"), before, after, false)
        .expect("Rust projection must parse");
    let graph = correspond(&pair);
    let unit = only_matched(&graph);
    let payload = graph
        .unit_leaf_links(unit)
        .iter()
        .find(|link| {
            pair.before.leaf_text(link.before) == Some("beta")
                && pair.after.leaf_text(link.after) == Some("beta")
        })
        .expect("unchanged body leaf must remain linked");
    let name = graph
        .unit_leaf_links(unit)
        .iter()
        .find(|link| {
            pair.before.leaf_text(link.before) == Some("alpha")
                && pair.after.leaf_text(link.after) == Some("gamma")
        })
        .expect("renamed definition leaf must remain linked");

    assert_eq!(payload.relation, LeafRelation::Exact);
    assert_eq!(payload.parent, ParentCorrespondence::Direct);
    assert_eq!(name.relation, LeafRelation::Modified);
    assert_eq!(name.parent, ParentCorrespondence::Direct);
}

#[test]
fn nested_renames_follow_local_sibling_order() {
    let before = concat!(
        "mod tests {\n",
        "    fn old_alpha() { shared(); }\n",
        "    fn old_beta() { shared(); }\n",
        "}\n",
    );
    let after = concat!(
        "mod tests {\n",
        "    fn new_alpha() { shared(); }\n",
        "    fn new_beta() { shared(); }\n",
        "}\n",
    );
    let pair = project_pair(Path::new("lib.rs"), before, after, false)
        .expect("Rust projection must parse");
    let graph = correspond(&pair);
    let unit = only_matched(&graph);
    let renamed = graph
        .unit_leaf_links(unit)
        .iter()
        .filter(|link| {
            pair.before
                .leaf_text(link.before)
                .is_some_and(|text| text.starts_with("old_"))
                && pair
                    .after
                    .leaf_text(link.after)
                    .is_some_and(|text| text.starts_with("new_"))
        })
        .map(|link| {
            (
                pair.before.leaf_text(link.before).expect("before name"),
                pair.after.leaf_text(link.after).expect("after name"),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        renamed,
        [("old_alpha", "new_alpha"), ("old_beta", "new_beta")]
    );
}

#[test]
fn renamed_units_follow_local_order_instead_of_crossing_body_copies() {
    let pair = project_pair(
        Path::new("lib.rs"),
        "fn alpha() { body_a(); }\n\nfn beta() { body_b(); }\n",
        "fn gamma() { body_b(); }\n\nfn delta() { body_a(); }\n",
        false,
    )
    .expect("Rust projection must parse");
    let graph = correspond(&pair);
    let links = graph
        .units
        .iter()
        .filter_map(|edit| {
            let UnitEdit::Matched(unit) = edit else {
                return None;
            };
            Some((
                pair.before.identity_text(unit.before),
                pair.after.identity_text(unit.after),
                unit.placement,
            ))
        })
        .collect::<Vec<_>>();

    assert_eq!(
        links,
        [
            (Some("alpha"), Some("gamma"), Placement::Stable),
            (Some("beta"), Some("delta"), Placement::Stable),
        ]
    );
}

#[test]
fn established_unit_anchors_prevent_unrelated_global_pairing() {
    let pair = project_pair(
        Path::new("lib.rs"),
        "use crate::obsolete;\nfn kept() {}\n",
        "fn kept() {}\nuse crate::unrelated;\n",
        false,
    )
    .expect("Rust projection must parse");
    let graph = correspond(&pair);

    assert!(!graph.units.iter().any(|edit| {
        let UnitEdit::Matched(unit) = edit else {
            return false;
        };
        pair.before.identity_text(unit.before) == Some("crate::obsolete")
            || pair.after.identity_text(unit.after) == Some("crate::unrelated")
    }));
    assert!(
        graph
            .units
            .iter()
            .any(|edit| matches!(edit, UnitEdit::Removed { .. }))
    );
    assert!(
        graph
            .units
            .iter()
            .any(|edit| matches!(edit, UnitEdit::Added { .. }))
    );
}

#[test]
fn inserted_repeated_neighbor_does_not_reorder_an_exact_definition_cover() {
    let before = concat!(
        "mod tests {\n",
        "#[test]\n",
        "fn first() {\n",
        "    let rows = compose();\n",
        "    assert!(rows.contains(\"first contract\"));\n",
        "}\n",
        "#[test]\n",
        "fn second() {\n",
        "    let rows = compose();\n",
        "    assert!(rows.contains(\"second contract\"));\n",
        "}\n",
        "#[test]\n",
        "fn third() {\n",
        "    let rows = compose();\n",
        "    assert!(rows.contains(\"third contract\"));\n",
        "}\n",
        "}\n",
    );
    let after = before.replace(
        "#[test]\nfn second()",
        concat!(
            "#[test]\n",
            "fn inserted() {\n",
            "    let rows = compose();\n",
            "    assert!(rows.contains(\"inserted contract\"));\n",
            "}\n",
            "#[test]\n",
            "fn second()",
        ),
    );
    let pair = project_pair(Path::new("lib.rs"), before, &after, false)
        .expect("Rust projection must parse");
    let graph = correspond(&pair);
    let unit = only_matched(&graph);
    let second = graph
        .unit_composites(unit)
        .iter()
        .find(|link| {
            pair.before.node(link.before).kind == "function_item"
                && pair.before.identity_text(link.before) == Some("second")
                && pair.after.identity_text(link.after) == Some("second")
        })
        .expect("the unchanged second definition must retain its exact cover");
    let after_leaves = descendant_leaves(&pair.after, second.after)
        .into_iter()
        .collect::<HashSet<_>>();
    let links = descendant_leaves(&pair.before, second.before)
        .into_iter()
        .map(|leaf| {
            graph
                .before_leaf_link(leaf)
                .expect("every leaf in an exact cover must remain linked")
        })
        .collect::<Vec<_>>();
    let before_second_attribute = composite_on_line(&pair.before, unit.before, "attribute_item", 7);
    let before_third_attribute = composite_on_line(&pair.before, unit.before, "attribute_item", 12);
    let after_inserted_attribute = composite_on_line(&pair.after, unit.after, "attribute_item", 7);
    let after_second_attribute = composite_on_line(&pair.after, unit.after, "attribute_item", 12);
    let after_third_attribute = composite_on_line(&pair.after, unit.after, "attribute_item", 17);
    let second_attribute = composite_link(&graph, before_second_attribute);
    let third_attribute = composite_link(&graph, before_third_attribute);

    assert_eq!(second.placement, Placement::Stable);
    assert!(!links.is_empty());
    assert!(links.iter().all(|link| {
        link.relation == LeafRelation::Exact
            && link.placement == Placement::Stable
            && link.parent == ParentCorrespondence::Direct
            && after_leaves.contains(&link.after)
    }));
    assert_eq!(second_attribute.after, after_second_attribute);
    assert_eq!(second_attribute.placement, Placement::Stable);
    assert_eq!(third_attribute.after, after_third_attribute);
    assert_eq!(third_attribute.placement, Placement::Stable);
    assert!(
        graph
            .composites
            .iter()
            .all(|link| link.after != after_inserted_attribute)
    );
    assert!(
        descendant_leaves(&pair.after, after_inserted_attribute)
            .into_iter()
            .all(|leaf| graph.after_leaf_link(leaf).is_none())
    );
}

#[test]
fn stacked_attributes_align_outward_from_their_following_definition() {
    let before = concat!(
        "mod tests {\n",
        "#[cfg(feature = \"slow\")]\n",
        "#[test]\n",
        "fn target() { body(); }\n",
        "}\n",
    );
    let after = concat!(
        "mod tests {\n",
        "#[allow(dead_code)]\n",
        "#[cfg(feature = \"slow\")]\n",
        "#[test]\n",
        "fn target() { body(); }\n",
        "}\n",
    );
    let pair = project_pair(Path::new("lib.rs"), before, after, false)
        .expect("Rust projection must parse");
    let graph = correspond(&pair);
    let unit = only_matched(&graph);
    let before_cfg = composite_on_line(&pair.before, unit.before, "attribute_item", 2);
    let before_test = composite_on_line(&pair.before, unit.before, "attribute_item", 3);
    let after_added = composite_on_line(&pair.after, unit.after, "attribute_item", 2);
    let after_cfg = composite_on_line(&pair.after, unit.after, "attribute_item", 3);
    let after_test = composite_on_line(&pair.after, unit.after, "attribute_item", 4);

    assert_eq!(composite_link(&graph, before_cfg).after, after_cfg);
    assert_eq!(composite_link(&graph, before_test).after, after_test);
    assert!(
        graph
            .composites
            .iter()
            .all(|link| link.after != after_added)
    );
}

#[test]
fn moved_annotated_definition_carries_its_anonymous_prefix() {
    let before = concat!(
        "mod tests {\n",
        "#[test]\n",
        "fn first() { first_body(); }\n",
        "#[test]\n",
        "fn second() { second_body(); }\n",
        "}\n",
    );
    let after = concat!(
        "mod tests {\n",
        "#[test]\n",
        "fn second() { second_body(); }\n",
        "#[test]\n",
        "fn first() { first_body(); }\n",
        "}\n",
    );
    let pair = project_pair(Path::new("lib.rs"), before, after, false)
        .expect("Rust projection must parse");
    let graph = correspond(&pair);
    let unit = only_matched(&graph);
    let mut placements = Vec::new();
    for (name, before_line, after_line) in [("first", 2, 4), ("second", 4, 2)] {
        let before_attribute =
            composite_on_line(&pair.before, unit.before, "attribute_item", before_line);
        let after_attribute =
            composite_on_line(&pair.after, unit.after, "attribute_item", after_line);
        let attribute = composite_link(&graph, before_attribute);
        let definition = graph
            .unit_composites(unit)
            .iter()
            .find(|link| pair.before.identity_text(link.before) == Some(name))
            .expect("annotated definition must remain linked");

        assert_eq!(attribute.after, after_attribute);
        assert_eq!(attribute.placement, definition.placement);
        placements.push(definition.placement);
    }
    assert!(placements.contains(&Placement::Reordered));
}

#[test]
fn inner_attribute_remains_with_its_enclosing_module() {
    let before = concat!(
        "mod tests {\n",
        "#![allow(dead_code)]\n",
        "#[test]\n",
        "fn first() { first_body(); }\n",
        "#[test]\n",
        "fn second() { second_body(); }\n",
        "}\n",
    );
    let after = concat!(
        "mod tests {\n",
        "#![allow(dead_code)]\n",
        "#[test]\n",
        "fn second() { second_body(); }\n",
        "#[test]\n",
        "fn first() { first_body(); }\n",
        "}\n",
    );
    let pair = project_pair(Path::new("lib.rs"), before, after, false)
        .expect("Rust projection must parse");
    let graph = correspond(&pair);
    let unit = only_matched(&graph);
    let before_inner = composite_on_line(&pair.before, unit.before, "inner_attribute_item", 2);
    let after_inner = composite_on_line(&pair.after, unit.after, "inner_attribute_item", 2);
    let inner = composite_link(&graph, before_inner);

    assert_eq!(
        pair.before.node(before_inner).decoration_owner,
        Some(unit.before)
    );
    assert_eq!(inner.after, after_inner);
    assert_eq!(inner.placement, Placement::Stable);
}

#[test]
fn reordered_exact_inline_subtree_propagates_placement_to_its_leaves() {
    let pair = project_pair(
        Path::new("lib.rs"),
        "fn run() { first(); second(); }\n",
        "fn run() { second(); first(); }\n",
        false,
    )
    .expect("Rust projection must parse");
    let graph = correspond(&pair);
    let unit = only_matched(&graph);

    assert!(
        graph
            .unit_composites(unit)
            .iter()
            .any(|link| link.placement == Placement::Reordered)
    );
    assert!(graph.unit_leaf_links(unit).iter().any(|link| {
        link.relation == LeafRelation::Exact && link.placement == Placement::Reordered
    }));
}

#[test]
fn exact_leaf_cannot_move_between_sibling_call_parents() {
    let pair = project_pair(
        Path::new("lib.rs"),
        "fn run() { left(alpha); right(beta); }\n",
        "fn run() { left(beta); right(alpha); }\n",
        false,
    )
    .expect("Rust projection must parse");
    let graph = correspond(&pair);
    let unit = only_matched(&graph);
    let alpha = graph.unit_leaf_links(unit).iter().any(|link| {
        pair.before.leaf_text(link.before) == Some("alpha")
            && pair.after.leaf_text(link.after) == Some("alpha")
    });

    assert!(!alpha, "a leaf cannot cross from left(...) into right(...)");
}

#[test]
fn subtree_cannot_move_between_sibling_html_parents() {
    let before = concat!(
        "<section>\n",
        "  <div>\n",
        "    <article class=\"alpha\">\n",
        "      <span>gamma</span>\n",
        "    </article>\n",
        "  </div>\n",
        "  <aside></aside>\n",
        "</section>\n",
    );
    let after = concat!(
        "<section>\n",
        "  <div></div>\n",
        "  <aside>\n",
        "    <article class=\"beta\">\n",
        "      <span>gamma</span>\n",
        "    </article>\n",
        "  </aside>\n",
        "</section>\n",
    );
    let pair = project_pair(Path::new("alpha.html"), before, after, false)
        .expect("HTML projection must parse");
    let graph = correspond(&pair);
    let unit = only_matched(&graph);
    let retained = graph.unit_leaf_links(unit).iter().any(|link| {
        pair.before.leaf_text(link.before) == Some("gamma")
            && pair.after.leaf_text(link.after) == Some("gamma")
    });

    assert!(!retained, "payload cannot cross from div into aside");
}

#[test]
fn shallow_modified_subtree_reparents_on_unique_exact_payload() {
    let before = "<body>\n  <div>alpha</div>\n</body>\n";
    let after = "<div id=\"beta\">alpha</div>\n";

    assert_eq!(html_div_reparent_counts(before, after), (1, 1));
    let pair = project_pair(Path::new("alpha.html"), before, after, false)
        .expect("HTML projection must parse");
    let graph = correspond(&pair);
    assert!(graph.leaf_links.iter().any(|link| {
        pair.before.leaf_text(link.before) == Some("alpha")
            && pair.after.leaf_text(link.after) == Some("alpha")
            && link.relation == LeafRelation::Exact
            && link.wrapper.is_some()
    }));
    assert_eq!(
        html_div_reparent_counts(
            "<body>\n  <div></div>\n</body>\n",
            "<div id=\"beta\"></div>\n",
        ),
        (0, 0),
    );
    assert_eq!(
        html_div_reparent_counts(
            "<body>\n  <div>alpha</div>\n  <div>alpha</div>\n</body>\n",
            after,
        ),
        (0, 0),
    );
}

fn html_div_reparent_counts(before: &str, after: &str) -> (usize, usize) {
    let pair = project_pair(Path::new("alpha.html"), before, after, false)
        .expect("HTML projection must parse");
    let mut interner = FingerprintInterner::default();
    let before_fingerprints = fingerprints(&pair.before, &mut interner);
    let after_fingerprints = fingerprints(&pair.after, &mut interner);
    let links = contextual_links(
        &pair,
        pair.before.root,
        pair.after.root,
        Placement::Stable,
        &before_fingerprints,
        &after_fingerprints,
    );
    let before_divs = pair
        .before
        .nodes
        .iter()
        .enumerate()
        .map(|(index, _)| NodeId::new(index))
        .filter(|id| {
            pair.before.node(*id).kind == "element" && pair.before.identity_text(*id) == Some("div")
        })
        .collect::<Vec<_>>();
    let after_div = pair
        .after
        .nodes
        .iter()
        .enumerate()
        .map(|(index, _)| NodeId::new(index))
        .find(|id| {
            pair.after.node(*id).kind == "element" && pair.after.identity_text(*id) == Some("div")
        })
        .expect("current div");

    let matched = before_divs
        .iter()
        .filter(|before| links.before.get(before).copied() == Some(after_div))
        .count();
    let reparented = before_divs
        .iter()
        .filter(|before| links.reparenting.contains_key(before))
        .count();
    (matched, reparented)
}

#[test]
fn physical_line_analysis_retains_terminator_edits_and_missing_sides() {
    let cases = [
        ("same\n", "same", 1, 0),
        ("old\r\n", "new\n", 1, 0),
        ("same\n", "same\r\n", 1, 0),
        ("", "new\n", 0, 0),
        ("old\n", "", 0, 0),
        ("old", "", 0, 1),
        ("old\n", "new\n", 0, 0),
    ];

    for (before, after, ending_edits, missing_terminators) in cases {
        let pair = project_pair(Path::new("notes.txt"), before, after, false)
            .expect("line projection cannot fail");
        let facts = physical_line_correspondence(&pair);
        assert_eq!(
            facts.ending_edits.len(),
            ending_edits,
            "{before:?} -> {after:?}"
        );
        assert_eq!(
            facts.missing_terminators.len(),
            missing_terminators,
            "{before:?} -> {after:?}",
        );
    }
}

#[test]
fn unequal_physical_gaps_do_not_invent_terminator_pairs() {
    let pair = project_pair(
        Path::new("notes.txt"),
        "head\nold\ntail\n",
        "head\ninserted\r\nnew\ntail\n",
        false,
    )
    .expect("line projection cannot fail");
    let facts = physical_line_correspondence(&pair);

    assert!(facts.ending_edits.is_empty());
}

fn only_matched(graph: &Correspondence) -> &MatchedUnit {
    let mut matched = graph.units.iter().filter_map(|edit| {
        let UnitEdit::Matched(unit) = edit else {
            return None;
        };
        Some(unit)
    });
    let unit = matched.next().expect("expected one matched unit");
    assert!(matched.next().is_none(), "expected only one matched unit");
    unit
}
