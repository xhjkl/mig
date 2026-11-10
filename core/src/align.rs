//! Sequence alignment primitives shared by every layer that pairs things in order.
//!
//! Nothing here knows about syntax or lines; callers supply comparable values and
//! read back index pairs.

use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

/// Bound on the quadratic alignment table; larger gaps use the greedy fallback.
const MAX_LOCAL_ALIGNMENT_CELLS: usize = 16_384;

/// Occurrence indices paired by an alignment; whether pairs may cross depends on the algorithm.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Pair {
    pub before: usize,
    pub after: usize,
}

/// Match equal values once per occurrence while preserving order in both sequences.
/// Noncrossing unique values anchor the alignment. Small gaps use a longest common
/// subsequence; large gaps use greedy matching to cap memory use.
pub fn ordered_matches<T: Eq + Hash>(before: &[T], after: &[T]) -> Vec<Pair> {
    ordered_matches_preferring(before, after, |_, _| false)
}

/// Order-preserving matching that breaks alignment ties toward preferred pairs.
/// Preference never adds or removes matches; it only chooses among equal-sized alignments.
pub fn ordered_matches_preferring<T: Eq + Hash>(
    before: &[T],
    after: &[T],
    prefer: impl Fn(usize, usize) -> bool,
) -> Vec<Pair> {
    let mut before_positions = HashMap::<&T, Vec<usize>>::new();
    for (index, value) in before.iter().enumerate() {
        before_positions.entry(value).or_default().push(index);
    }
    let mut after_positions = HashMap::<&T, Vec<usize>>::new();
    for (index, value) in after.iter().enumerate() {
        after_positions.entry(value).or_default().push(index);
    }

    let mut candidates = before_positions
        .iter()
        .filter_map(|(value, before_indices)| {
            let [before] = before_indices.as_slice() else {
                return None;
            };
            let [after] = after_positions.get(value)?.as_slice() else {
                return None;
            };
            Some(Pair {
                before: *before,
                after: *after,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable_by_key(|edge| edge.before);

    let candidate_after = candidates.iter().map(|edge| edge.after).collect::<Vec<_>>();
    let stable = increasing_subsequence_members(&candidate_after);
    let anchors = candidates
        .into_iter()
        .zip(stable)
        .filter_map(|(edge, stable)| stable.then_some(edge));

    let mut matches = Vec::new();
    let mut before_start = 0;
    let mut after_start = 0;
    for anchor in anchors.chain(std::iter::once(Pair {
        before: before.len(),
        after: after.len(),
    })) {
        let gap = align_gap(
            &before[before_start..anchor.before],
            &after[after_start..anchor.after],
            |before, after| prefer(before_start + before, after_start + after),
        );
        matches.extend(gap.into_iter().map(|edge| Pair {
            before: before_start + edge.before,
            after: after_start + edge.after,
        }));
        if anchor.before < before.len() && anchor.after < after.len() {
            matches.push(anchor);
        }
        before_start = anchor.before.saturating_add(1);
        after_start = anchor.after.saturating_add(1);
    }
    matches
}

/// Accept a pair only when each endpoint uniquely prefers the other and exceeds the threshold.
pub fn reciprocal_unique_matches(
    before_len: usize,
    after_len: usize,
    minimum_similarity: u64,
    similarity: impl Fn(usize, usize) -> u64,
) -> Vec<Pair> {
    if before_len == 0 || after_len == 0 {
        return Vec::new();
    }
    let mut similarities = vec![0_u64; before_len * after_len];
    for before in 0..before_len {
        for after in 0..after_len {
            similarities[before * after_len + after] = similarity(before, after);
        }
    }
    let before_best = (0..before_len)
        .map(|before| {
            unique_best(
                (0..after_len).map(|after| (after, similarities[before * after_len + after])),
            )
        })
        .collect::<Vec<_>>();
    let after_best = (0..after_len)
        .map(|after| {
            unique_best(
                (0..before_len).map(|before| (before, similarities[before * after_len + after])),
            )
        })
        .collect::<Vec<_>>();
    before_best
        .into_iter()
        .enumerate()
        .filter_map(|(before, best)| {
            let (after, similarity) = best?;
            (similarity > minimum_similarity
                && after_best[after].map(|(candidate, _)| candidate) == Some(before))
            .then_some(Pair { before, after })
        })
        .collect()
}

fn unique_best(values: impl Iterator<Item = (usize, u64)>) -> Option<(usize, u64)> {
    let mut best = None;
    let mut tied = false;
    for (index, score) in values.filter(|(_, score)| *score > 0) {
        let Some((_, best_score)) = best else {
            best = Some((index, score));
            continue;
        };
        if score > best_score {
            best = Some((index, score));
            tied = false;
        } else if score == best_score {
            tied = true;
        }
    }
    (!tied).then_some(best).flatten()
}

/// Mark one longest strictly increasing subsequence.
/// Ties choose the latest occurrence of an equal tail value.
pub fn increasing_subsequence_members(values: &[usize]) -> Vec<bool> {
    let mut tails = Vec::<usize>::new();
    let mut previous = vec![None; values.len()];
    for (index, value) in values.iter().copied().enumerate() {
        let slot = tails.partition_point(|tail| values[*tail] < value);
        if slot > 0 {
            previous[index] = Some(tails[slot - 1]);
        }
        if slot == tails.len() {
            tails.push(index);
        } else {
            tails[slot] = index;
        }
    }
    let mut members = vec![false; values.len()];
    let Some(mut index) = tails.last().copied() else {
        return members;
    };
    loop {
        members[index] = true;
        let Some(parent) = previous[index] else {
            break;
        };
        index = parent;
    }
    members
}

/// Mark the pairs that keep their relative order; the rest moved. Input is sorted by `before`.
pub fn stable_members(pairs: &[Pair]) -> Vec<bool> {
    let after = pairs.iter().map(|pair| pair.after).collect::<Vec<_>>();
    increasing_subsequence_members(&after)
}

/// Mark an in-order subset of maximum total weight, so that large constructs stay in place
/// and small ones are the ones that moved. Input is sorted by `before`; long inputs fall back
/// to the unweighted choice.
pub fn weighted_stable_members(pairs: &[Pair], weights: &[usize]) -> Vec<bool> {
    const MAX_WEIGHTED: usize = 2_000;
    debug_assert_eq!(pairs.len(), weights.len());
    if pairs.len() > MAX_WEIGHTED {
        return stable_members(pairs);
    }
    let mut best = vec![0_usize; pairs.len()];
    let mut previous = vec![None::<usize>; pairs.len()];
    for index in 0..pairs.len() {
        best[index] = weights[index].max(1);
        for earlier in 0..index {
            if pairs[earlier].after < pairs[index].after {
                let candidate = best[earlier] + weights[index].max(1);
                if candidate > best[index] {
                    best[index] = candidate;
                    previous[index] = Some(earlier);
                }
            }
        }
    }
    let mut members = vec![false; pairs.len()];
    let Some(mut index) =
        (0..pairs.len()).max_by_key(|index| (best[*index], std::cmp::Reverse(*index)))
    else {
        return members;
    };
    loop {
        members[index] = true;
        let Some(parent) = previous[index] else {
            break;
        };
        index = parent;
    }
    members
}

fn align_gap<T: Eq + Hash>(
    before: &[T],
    after: &[T],
    prefer: impl Fn(usize, usize) -> bool,
) -> Vec<Pair> {
    if before.is_empty() || after.is_empty() {
        return Vec::new();
    }
    let cells = before.len().saturating_mul(after.len());
    if cells > MAX_LOCAL_ALIGNMENT_CELLS {
        return greedy_matches(before, after);
    }
    lcs_matches(before, after, prefer)
}

/// Align a large gap in linear memory; the result may retain fewer matches than LCS.
fn greedy_matches<T: Eq + Hash>(before: &[T], after: &[T]) -> Vec<Pair> {
    let mut after_positions = HashMap::<&T, VecDeque<usize>>::new();
    for (index, value) in after.iter().enumerate() {
        after_positions.entry(value).or_default().push_back(index);
    }
    let mut matches = Vec::new();
    let mut after_floor = 0;
    for (before, value) in before.iter().enumerate() {
        let Some(positions) = after_positions.get_mut(value) else {
            continue;
        };
        while positions.front().is_some_and(|after| *after < after_floor) {
            positions.pop_front();
        }
        let Some(after) = positions.pop_front() else {
            continue;
        };
        matches.push(Pair { before, after });
        after_floor = after + 1;
    }
    matches
}

/// Maximize equal-value matches, then preferred pairs, then the smallest positional drift.
/// Callers must bound the gap size because the score table is quadratic.
fn lcs_matches<T: Eq>(
    before: &[T],
    after: &[T],
    prefer: impl Fn(usize, usize) -> bool,
) -> Vec<Pair> {
    #[derive(Clone, Copy, Default, Eq, PartialEq)]
    struct Score {
        retained: usize,
        preferred: usize,
        drift: usize,
    }

    let width = after.len() + 1;
    let mut scores = vec![Score::default(); (before.len() + 1) * width];
    let better = |left: Score, right: Score| {
        (left.retained, left.preferred, Reverse(left.drift))
            > (right.retained, right.preferred, Reverse(right.drift))
    };
    let pair_score = |mut paired: Score, before_index: usize, after_index: usize| {
        paired.retained += 1;
        paired.preferred += usize::from(prefer(before_index, after_index));
        paired.drift = paired
            .drift
            .saturating_add(before_index.abs_diff(after_index));
        paired
    };
    for before_index in (0..before.len()).rev() {
        for after_index in (0..after.len()).rev() {
            let mut score = scores[(before_index + 1) * width + after_index];
            let skip_after = scores[before_index * width + after_index + 1];
            if better(skip_after, score) {
                score = skip_after;
            }
            if before[before_index] == after[after_index] {
                let paired = pair_score(
                    scores[(before_index + 1) * width + after_index + 1],
                    before_index,
                    after_index,
                );
                if better(paired, score) {
                    score = paired;
                }
            }
            scores[before_index * width + after_index] = score;
        }
    }

    let mut matches = Vec::new();
    let mut before_index = 0;
    let mut after_index = 0;
    while before_index < before.len() && after_index < after.len() {
        if before[before_index] == after[after_index] {
            let paired = pair_score(
                scores[(before_index + 1) * width + after_index + 1],
                before_index,
                after_index,
            );
            if paired == scores[before_index * width + after_index] {
                matches.push(Pair {
                    before: before_index,
                    after: after_index,
                });
                before_index += 1;
                after_index += 1;
                continue;
            }
        }
        let target = scores[before_index * width + after_index];
        let skip_before = scores[(before_index + 1) * width + after_index];
        let skip_after = scores[before_index * width + after_index + 1];
        match (skip_before == target, skip_after == target) {
            (true, false) => before_index += 1,
            (false, true) => after_index += 1,
            (true, true) if before.len() - before_index >= after.len() - after_index => {
                before_index += 1;
            }
            (true, true) => after_index += 1,
            (false, false) => unreachable!("alignment score has no reconstructible edge"),
        }
    }
    matches
}

/// Common prefix and suffix lengths, the cheapest useful alignment of two sequences.
pub fn changed_extent<T: Eq>(
    before: &[T],
    after: &[T],
) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
    let prefix = before
        .iter()
        .zip(after)
        .take_while(|(before, after)| before == after)
        .count();
    let suffix = before[prefix..]
        .iter()
        .rev()
        .zip(after[prefix..].iter().rev())
        .take_while(|(before, after)| before == after)
        .count();
    (
        prefix..before.len().saturating_sub(suffix),
        prefix..after.len().saturating_sub(suffix),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_matching_anchors_unique_values_and_fills_gaps() {
        let before = ["a", "x", "b", "c", "d"];
        let after = ["a", "b", "y", "c", "d"];
        let matches = ordered_matches(&before, &after);
        assert_eq!(
            matches,
            [
                Pair {
                    before: 0,
                    after: 0
                },
                Pair {
                    before: 2,
                    after: 1
                },
                Pair {
                    before: 3,
                    after: 3
                },
                Pair {
                    before: 4,
                    after: 4
                },
            ]
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
            Some(&Pair {
                before: 0,
                after: 0
            })
        );
        assert_eq!(
            matches.last(),
            Some(&Pair {
                before: 199,
                after: 199
            })
        );
    }

    #[test]
    fn stable_members_mark_a_longest_in_order_run() {
        let pairs = [
            Pair {
                before: 0,
                after: 2,
            },
            Pair {
                before: 1,
                after: 3,
            },
            Pair {
                before: 2,
                after: 0,
            },
        ];
        assert_eq!(stable_members(&pairs), [true, true, false]);
    }

    #[test]
    fn changed_extent_trims_shared_ends() {
        let before = ['a', 'b', 'c', 'd'];
        let after = ['a', 'x', 'y', 'd'];
        assert_eq!(changed_extent(&before, &after), (1..3, 1..3));
        assert_eq!(changed_extent(&before, &before), (4..4, 4..4));
    }
}
