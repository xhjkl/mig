use crate::diff::{MAX_REVISION_LINES, PresentedFile, diff_file};
use anyhow::Result;

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
