use crate::diff::{
    ChangeNature, MAX_REVISION_LINES, PresentedFile, ReviewRow, TouchedSymbol, diff_file,
};
use anyhow::Result;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Per-revision byte limit checked before loading source for review.
pub const MAX_REVISION_BYTES: u64 = 16 * 1024 * 1024;

/// Keep files over the line limit in navigation as notices; omit pairs with no visible changes.
pub fn review_source_pair(
    path: &str,
    before: Option<&str>,
    after: Option<&str>,
) -> Result<Option<ReviewEntry>> {
    let before_lines = before.map(|source| source.lines().count());
    let after_lines = after.map(|source| source.lines().count());
    if before_lines.is_some_and(|lines| lines > MAX_REVISION_LINES)
        || after_lines.is_some_and(|lines| lines > MAX_REVISION_LINES)
    {
        return Ok(Some(ReviewEntry::Notice(FileNotice::TooManyLines {
            path: path.to_owned(),
            before_lines,
            after_lines,
            limit_lines: MAX_REVISION_LINES,
        })));
    }

    let before = before.unwrap_or_default();
    let after = after.unwrap_or_default();
    let diff = diff_file(path, before, after)?;
    if diff.hunks.is_empty() {
        return Ok(None);
    }

    Ok(Some(ReviewEntry::Diff(diff)))
}

/// Order files and hunks so the changes other changes depend on come first.
///
/// A file is upstream of another reviewed file when that file spells a name this
/// file's changes declare, and does not declare that name itself. Files with more
/// such consumers come first; tests, documentation, and generated files follow
/// the sources they exercise. Within a file, edits keep precedence over moves,
/// wiring, and formatting; among edits, consumed declarations lead, then heading
/// changes such as signatures and fields, then source order. `tie_break` orders
/// files the ranking leaves equal.
pub fn rank_reviews(reviews: &mut [ReviewEntry], mut tie_break: impl FnMut(&ReviewEntry) -> usize) {
    let mut consumers_by_file = vec![HashSet::<usize>::new(); reviews.len()];
    let mut consumers_by_hunk = reviews
        .iter()
        .map(|review| match review {
            ReviewEntry::Diff(diff) => vec![HashSet::<usize>::new(); diff.hunks.len()],
            ReviewEntry::Notice(_) => Vec::new(),
        })
        .collect::<Vec<_>>();
    // A name is attributable only when exactly one reviewed source file defines it;
    // mocks in tests and conventional names such as `new` or `tests` stay unattributed.
    let mut defining = HashMap::<&str, usize>::new();
    for review in reviews.iter() {
        let ReviewEntry::Diff(diff) = review else {
            continue;
        };
        if review_tier(review) != 0 {
            continue;
        }
        for name in &diff.impact.defined {
            *defining.entry(name.as_str()).or_default() += 1;
        }
    }
    let mut consumers_by_symbol = HashMap::<&TouchedSymbol, Vec<usize>>::new();
    for (index, review) in reviews.iter().enumerate() {
        let ReviewEntry::Diff(diff) = review else {
            continue;
        };
        if review_tier(review) != 0 {
            continue;
        }
        for (hunk_index, hunk) in diff.hunks.iter().enumerate() {
            for symbol in &hunk.impact.symbols {
                // Credit goes to the file that defines the name, not one that merely
                // touches a same-named mock or local.
                if !diff.impact.defined.contains(&symbol.name)
                    || defining.get(symbol.name.as_str()) != Some(&1)
                {
                    continue;
                }
                let consumers = consumers_by_symbol.entry(symbol).or_insert_with(|| {
                    reviews
                        .iter()
                        .enumerate()
                        .filter(|(_, review)| {
                            matches!(review, ReviewEntry::Diff(diff) if uses_symbol(diff, symbol))
                        })
                        .map(|(consumer, _)| consumer)
                        .collect()
                });
                consumers_by_hunk[index][hunk_index].extend(consumers.iter());
                consumers_by_file[index].extend(consumers.iter());
            }
        }
    }

    for (index, review) in reviews.iter_mut().enumerate() {
        let ReviewEntry::Diff(diff) = review else {
            continue;
        };
        for (hunk, consumers) in diff.hunks.iter_mut().zip(&consumers_by_hunk[index]) {
            hunk.consumers = consumers.len();
        }
        diff.consumers = consumers_by_file[index].len();
        rank_hunks(diff);
    }

    let mut order = (0..reviews.len()).collect::<Vec<_>>();
    let keys = reviews
        .iter()
        .enumerate()
        .map(|(index, review)| {
            let contract = match review {
                ReviewEntry::Diff(diff) => diff.hunks.iter().any(|hunk| hunk.impact.contract),
                ReviewEntry::Notice(_) => false,
            };
            (
                review_tier(review),
                Reverse(consumers_by_file[index].len()),
                Reverse(contract),
                tie_break(review),
                review.path().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    order.sort_by(|left, right| keys[*left].cmp(&keys[*right]));
    let mut ranked = order
        .into_iter()
        .map(|index| std::mem::replace(&mut reviews[index], placeholder()))
        .collect::<Vec<_>>();
    reviews.swap_with_slice(&mut ranked);
}

/// Spelling the name, and the owner of a nested definition, without defining the name itself.
fn uses_symbol(diff: &PresentedFile, symbol: &TouchedSymbol) -> bool {
    let references = &diff.impact.references;
    references.contains(&symbol.name)
        && symbol
            .owner
            .as_ref()
            .is_none_or(|owner| references.contains(owner))
        && !diff.impact.defined.contains(&symbol.name)
}

fn placeholder() -> ReviewEntry {
    ReviewEntry::Notice(FileNotice::TooLarge {
        path: String::new(),
        before_bytes: None,
        after_bytes: None,
        limit_bytes: 0,
    })
}

/// Stable-sort hunks by review priority, keeping the file boundary on the last hunk.
fn rank_hunks(diff: &mut PresentedFile) {
    let boundary = diff.hunks.iter_mut().find_map(|hunk| {
        let last = hunk.rows.last()?;
        matches!(last, ReviewRow::FileBoundary).then(|| hunk.rows.pop())
    });
    diff.hunks.sort_by_key(|hunk| {
        (
            Reverse(nature_priority(hunk.nature)),
            Reverse(hunk.consumers),
            Reverse(hunk.impact.contract),
        )
    });
    if let (Some(Some(boundary)), Some(last)) = (boundary, diff.hunks.last_mut()) {
        last.rows.push(boundary);
    }
}

fn nature_priority(nature: ChangeNature) -> u8 {
    match nature {
        ChangeNature::Reflow => 0,
        ChangeNature::Wiring => 1,
        ChangeNature::Move => 2,
        ChangeNature::Edit => 3,
    }
}

/// Sources, then tests, then documentation, then generated files.
fn review_tier(review: &ReviewEntry) -> u8 {
    if review.is_generated() {
        return 3;
    }
    let path = Path::new(review.path());
    if is_documentation_path(path) {
        return 2;
    }
    if is_test_path(path) {
        return 1;
    }
    0
}

fn is_documentation_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "md" | "markdown" | "rst" | "adoc"
            )
        })
}

/// Recognize test code from conventional directory and file names.
pub fn is_test_path(path: &Path) -> bool {
    let directory_names = path
        .parent()
        .into_iter()
        .flat_map(|parent| parent.iter())
        .filter_map(|segment| segment.to_str())
        .map(str::to_ascii_lowercase);
    for name in directory_names {
        if matches!(
            name.as_str(),
            "test"
                | "tests"
                | "__tests__"
                | "testing"
                | "spec"
                | "specs"
                | "testdata"
                | "fixtures"
                | "e2e"
                | "benches"
                | "benchmarks"
        ) {
            return true;
        }
    }
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    let stem = stem.to_ascii_lowercase();
    matches!(stem.as_str(), "test" | "tests" | "conftest")
        || stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with(".test")
        || stem.ends_with("_spec")
        || stem.ends_with(".spec")
        || stem.ends_with("_tests")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewEntry {
    Diff(PresentedFile),
    Notice(FileNotice),
}

impl ReviewEntry {
    pub fn path(&self) -> &str {
        match self {
            Self::Diff(diff) => &diff.path,
            Self::Notice(FileNotice::TooLarge { path, .. })
            | Self::Notice(FileNotice::TooManyLines { path, .. }) => path,
        }
    }

    /// Classify only inspected diffs as generated; notices keep normal review priority.
    pub fn is_generated(&self) -> bool {
        match self {
            Self::Diff(diff) => diff.generated,
            Self::Notice(_) => false,
        }
    }
}

/// A changed path kept in navigation after a resource limit prevents diffing.
/// Missing sizes or line counts denote an absent revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileNotice {
    TooLarge {
        path: String,
        before_bytes: Option<u64>,
        after_bytes: Option<u64>,
        limit_bytes: u64,
    },
    TooManyLines {
        path: String,
        before_lines: Option<usize>,
        after_lines: Option<usize>,
        limit_lines: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff(path: &str, before: &str, after: &str) -> ReviewEntry {
        review_source_pair(path, Some(before), Some(after))
            .expect("source diffs")
            .expect("changed source stays visible")
    }

    fn paths(reviews: &[ReviewEntry]) -> Vec<&str> {
        reviews.iter().map(ReviewEntry::path).collect()
    }

    #[test]
    fn files_whose_changed_definitions_other_files_use_come_first() {
        let mut reviews = vec![
            diff(
                "src/zeta.rs",
                "fn zeta() -> u8 { 1 }\n",
                "fn zeta() -> u8 { 2 }\n",
            ),
            diff(
                "src/beta.rs",
                "pub fn beta() -> u8 { alpha() }\n",
                "pub fn beta() -> u8 { alpha() + 1 }\n",
            ),
            diff(
                "src/alpha.rs",
                "pub fn alpha() -> u8 { 1 }\n",
                "pub fn alpha() -> u8 { 2 }\n",
            ),
            diff(
                "src/gamma.rs",
                "fn gamma() -> u8 { alpha() }\n",
                "fn gamma() -> u8 { alpha() * 2 }\n",
            ),
        ];

        rank_reviews(&mut reviews, |_| 0);

        assert_eq!(
            paths(&reviews),
            ["src/alpha.rs", "src/beta.rs", "src/gamma.rs", "src/zeta.rs"]
        );
        let ReviewEntry::Diff(alpha) = &reviews[0] else {
            panic!("alpha is a diff");
        };
        assert_eq!(alpha.consumers, 2);
        assert_eq!(alpha.hunks[0].consumers, 2);
    }

    #[test]
    fn tests_follow_sources_and_mocks_do_not_claim_names() {
        let mut reviews = vec![
            diff(
                "tests/alpha_test.rs",
                "fn alpha() -> u8 { 0 }\nfn check() { alpha(); beta(); }\n",
                "fn alpha() -> u8 { 9 }\nfn check() { alpha(); beta(); gamma(); }\n",
            ),
            diff(
                "src/beta.rs",
                "pub fn beta() -> u8 { 1 }\n",
                "pub fn beta() -> u8 { 2 }\n",
            ),
            diff(
                "src/alpha.rs",
                "pub fn alpha() -> u8 { 1 }\n",
                "pub fn alpha() -> u8 { 2 }\n",
            ),
        ];

        rank_reviews(&mut reviews, |_| 0);

        // The test calls its own `alpha`, so only `beta` has a consumer.
        assert_eq!(
            paths(&reviews),
            ["src/beta.rs", "src/alpha.rs", "tests/alpha_test.rs"]
        );
        let consumers = reviews
            .iter()
            .map(|review| match review {
                ReviewEntry::Diff(diff) => diff.consumers,
                ReviewEntry::Notice(_) => usize::MAX,
            })
            .collect::<Vec<_>>();
        assert_eq!(consumers, [1, 0, 0]);
    }

    #[test]
    fn hunks_touching_used_definitions_lead_and_the_file_boundary_stays_last() {
        let mut reviews = vec![
            diff(
                "src/alpha.rs",
                concat!(
                    "pub fn used() -> u8 { 1 }\n",
                    "\n",
                    "fn local() -> u8 { 1 }\n",
                    "\n",
                    "fn tail() -> u8 { 1 }\n",
                ),
                concat!(
                    "pub fn used() -> u8 { 2 }\n",
                    "\n",
                    "fn local() -> u8 { 2 }\n",
                    "\n",
                    "fn tail() -> u8 { 2 }\n",
                ),
            ),
            diff(
                "src/beta.rs",
                "pub fn beta() -> u8 { used() }\n",
                "pub fn beta() -> u8 { used() + 1 }\n",
            ),
        ];

        rank_reviews(&mut reviews, |_| 0);

        let ReviewEntry::Diff(alpha) = &reviews[0] else {
            panic!("alpha is a diff");
        };
        assert_eq!(alpha.path, "src/alpha.rs");
        let names = alpha
            .hunks
            .iter()
            .map(|hunk| hunk.impact.symbols[0].name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names[0], "used", "{names:?}");
        assert_eq!(alpha.hunks[0].consumers, 1);
        assert!(alpha.hunks[1..].iter().all(|hunk| hunk.consumers == 0));
        let boundaries = alpha
            .hunks
            .iter()
            .enumerate()
            .filter(|(_, hunk)| {
                hunk.rows
                    .iter()
                    .any(|row| matches!(row, ReviewRow::FileBoundary))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        assert_eq!(boundaries, [alpha.hunks.len() - 1]);
    }

    #[test]
    fn test_paths_follow_conventions() {
        for path in [
            "tests/alpha.rs",
            "src/__tests__/alpha.ts",
            "alpha_test.go",
            "test_alpha.py",
            "alpha.spec.ts",
            "alpha.test.tsx",
            "src/diff/tests.rs",
        ] {
            assert!(is_test_path(Path::new(path)), "{path}");
        }
        for path in ["src/alpha.rs", "testament.rs", "contest/alpha.go"] {
            assert!(!is_test_path(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn source_pair_notice_preserves_an_absent_revision() {
        let after = "\n".repeat(MAX_REVISION_LINES + 1);

        let review = review_source_pair("alpha.txt", None, Some(&after))
            .expect("inspect added source")
            .expect("line-dense source stays visible");

        assert!(matches!(
            review,
            ReviewEntry::Notice(FileNotice::TooManyLines {
                before_lines: None,
                after_lines: Some(lines),
                ..
            }) if lines == MAX_REVISION_LINES + 1
        ));
    }
}
