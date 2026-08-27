//! Render-neutral geometry for source context halos and hierarchy breadcrumbs.

use std::collections::HashSet;
use std::ops::Range;

/// Physical lines retained on either side of an ordinary signal.
pub(super) const CONTEXT_HALO_RADIUS: usize = 3;

/// Whether two three-line halos touch or would hide only one physical row.
pub(super) fn context_gap_fits_halos(context_rows: usize) -> bool {
    context_rows <= CONTEXT_HALO_RADIUS * 2 + 1
}

/// First retained item after walking through at most one leading context halo.
pub(super) fn context_halo_start<T>(
    items: &[T],
    signal: usize,
    is_context: impl Fn(&T) -> bool,
) -> usize {
    let mut index = signal;
    let mut retained = 0;
    while index > 0 && retained < CONTEXT_HALO_RADIUS {
        if !is_context(&items[index - 1]) {
            break;
        }
        index -= 1;
        retained += 1;
    }
    index
}

/// Exclusive retained end after walking through at most one trailing context halo.
pub(super) fn context_halo_end<T>(
    items: &[T],
    signal: usize,
    is_context: impl Fn(&T) -> bool,
) -> usize {
    let mut index = signal + 1;
    let mut retained = 0;
    while index < items.len() && retained < CONTEXT_HALO_RADIUS {
        if !is_context(&items[index]) {
            break;
        }
        index += 1;
        retained += 1;
    }
    index
}

/// Contiguous unchanged rows around each display-only hierarchy step.
pub(super) fn breadcrumb_halo_indices(
    breadcrumbs: &HashSet<usize>,
    mut context_index: impl FnMut(usize) -> Option<usize>,
) -> Vec<usize> {
    let mut indices = Vec::new();
    for breadcrumb in breadcrumbs {
        indices.extend(context_index(*breadcrumb));

        let mut before = *breadcrumb;
        for _ in 0..CONTEXT_HALO_RADIUS {
            let Some(line) = before.checked_sub(1).filter(|line| *line > 0) else {
                break;
            };
            let Some(index) = context_index(line) else {
                break;
            };
            indices.push(index);
            before = line;
        }

        let mut after = *breadcrumb;
        for _ in 0..CONTEXT_HALO_RADIUS {
            let line = after.saturating_add(1);
            let Some(index) = context_index(line) else {
                break;
            };
            indices.push(index);
            after = line;
        }
    }
    indices.sort_unstable();
    indices.dedup();
    indices
}

/// Merge signal and breadcrumb context halos without widening hunk merge focus.
pub(super) fn review_selection_ranges(
    context_halo: Range<usize>,
    breadcrumbs: impl IntoIterator<Item = usize>,
    is_context: impl Fn(usize) -> bool,
) -> Vec<Range<usize>> {
    let mut selected = vec![context_halo];
    selected.extend(breadcrumbs.into_iter().map(|index| index..index + 1));
    selected.sort_by_key(|range| range.start);

    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in selected {
        let Some(previous) = merged.last_mut() else {
            merged.push(range);
            continue;
        };
        let one_context_gap =
            range.start == previous.end.saturating_add(1) && is_context(previous.end);
        if range.start <= previous.end || one_context_gap {
            previous.end = previous.end.max(range.end);
            continue;
        }
        merged.push(range);
    }
    merged
}

/// Whether two pre-expanded merge windows touch or would hide only one row.
pub(super) fn merge_windows_touch(left: &Range<usize>, right: &Range<usize>) -> bool {
    right.start.saturating_sub(left.end) <= 1
}

/// Whether two half-open source ranges share at least one coordinate.
pub(super) fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breadcrumb_selection_normalizes_order_duplicates_touching_ranges_and_one_context_gap() {
        let ranges = review_selection_ranges(4..7, [9, 3, 2, 7, 9, 12], |index| index == 8);

        assert_eq!(ranges, [2..10, 12..13]);
    }
}
