//! Context selection and range geometry shared across diff stages.

use std::collections::HashSet;
use std::ops::Range;

/// Base context radius in physical lines; merged halos and breadcrumbs may retain more.
pub const CONTEXT_HALO_RADIUS: usize = 3;

/// Allow halos to meet across one extra context row, avoiding a one-line fold.
pub fn context_gap_fits_halos(context_rows: usize) -> bool {
    context_rows <= CONTEXT_HALO_RADIUS * 2 + 1
}

/// Expand a nonempty signal range, stopping at another signal or the context limit.
pub fn context_halo_range<T>(
    items: &[T],
    signal: Range<usize>,
    is_context: impl Fn(&T) -> bool,
) -> Range<usize> {
    debug_assert!(!signal.is_empty() && signal.end <= items.len());
    let mut start = signal.start;
    let mut retained = 0;
    while start > 0 && retained < CONTEXT_HALO_RADIUS {
        if !is_context(&items[start - 1]) {
            break;
        }
        start -= 1;
        retained += 1;
    }

    let mut end = signal.end;
    let mut retained = 0;
    while end < items.len() && retained < CONTEXT_HALO_RADIUS {
        if !is_context(&items[end]) {
            break;
        }
        end += 1;
        retained += 1;
    }
    start..end
}

/// Select breadcrumbs and their contiguous context as sorted, unique row indices.
pub fn breadcrumb_halo_indices(
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

/// Join the signal halo and breadcrumb rows, filling any one-row context gaps.
pub fn review_selection_ranges(
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

/// Treat a one-row gap as touching to avoid a one-row fold.
pub fn merge_windows_touch(left: &Range<usize>, right: &Range<usize>) -> bool {
    right.start.saturating_sub(left.end) <= 1
}

pub fn include_range(range: &mut Option<Range<usize>>, addition: Option<Range<usize>>) {
    let Some(addition) = addition else {
        return;
    };
    let Some(range) = range else {
        *range = Some(addition);
        return;
    };
    range.start = range.start.min(addition.start);
    range.end = range.end.max(addition.end);
}

pub fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
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
