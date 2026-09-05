use crate::commit;
use crate::input::{BoundedBytes, OpenFile};
use crate::review::{FileNotice, MAX_REVISION_BYTES, ReviewEntry, review_source_pair};
use crate::{ui, worktree};
use anyhow::{Context, Result, bail};
use clap::Parser;
use std::env;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "m",
    version,
    about = "Review source changes with syntax-aware diffs"
)]
struct Cli {
    /// Commit to show, or previous file when AFTER is also provided.
    #[arg(value_name = "COMMITISH_OR_BEFORE")]
    commitish_or_before: Option<PathBuf>,

    /// Current file paired with BEFORE.
    after: Option<PathBuf>,
}

/// Run Mig from the process arguments and current directory.
pub fn run() -> Result<()> {
    let cli = Cli::parse();

    let reviews = match (cli.commitish_or_before, cli.after) {
        (None, Some(_)) => bail!("AFTER cannot be provided without BEFORE"),
        (None, None) => {
            let directory = env::current_dir();
            let directory = directory.context("failed to locate the current directory")?;
            worktree::diff_directory(&directory, MAX_REVISION_BYTES)?
        }
        (Some(commitish), None) => {
            let directory = env::current_dir();
            let directory = directory.context("failed to locate the current directory")?;
            commit::diff_commit(&directory, &commitish, MAX_REVISION_BYTES)?
        }
        (Some(before), Some(after)) => {
            let review = load_file_pair(&before, &after, MAX_REVISION_BYTES)?;
            review.into_iter().collect()
        }
    };
    if reviews.is_empty() {
        return Ok(());
    }

    ui::run(reviews)
}

fn load_file_pair(before: &Path, after: &Path, limit: u64) -> Result<Option<ReviewEntry>> {
    let before_file = OpenFile::open(before)?;
    let after_file = OpenFile::open(after)?;
    let before_bytes = before_file.bytes();
    let after_bytes = after_file.bytes();
    let path = display_pair_path(before, after);
    let path = path.to_string_lossy().into_owned();

    if before_bytes > limit || after_bytes > limit {
        return Ok(Some(ReviewEntry::Notice(FileNotice::TooLarge {
            path,
            before_bytes: Some(before_bytes),
            after_bytes: Some(after_bytes),
            limit_bytes: limit,
        })));
    }

    let before_source = before_file.read(limit)?;
    let before_source = match before_source {
        BoundedBytes::TooLarge(bytes) => {
            return Ok(Some(ReviewEntry::Notice(FileNotice::TooLarge {
                path,
                before_bytes: Some(bytes),
                after_bytes: Some(after_bytes),
                limit_bytes: limit,
            })));
        }
        BoundedBytes::Contents(source) => source,
    };
    let after_source = after_file.read(limit)?;
    let after_source = match after_source {
        BoundedBytes::TooLarge(bytes) => {
            return Ok(Some(ReviewEntry::Notice(FileNotice::TooLarge {
                path,
                before_bytes: Some(before_bytes),
                after_bytes: Some(bytes),
                limit_bytes: limit,
            })));
        }
        BoundedBytes::Contents(source) => source,
    };
    let before_source = String::from_utf8(before_source)
        .with_context(|| format!("file is not UTF-8: {}", before.display()))?;
    let after_source = String::from_utf8(after_source)
        .with_context(|| format!("file is not UTF-8: {}", after.display()))?;

    review_source_pair(&path, Some(&before_source), Some(&after_source))
}

/// Use a shared filename to hide temporary directories in Git difftool labels.
fn display_pair_path(before: &Path, after: &Path) -> PathBuf {
    if before.file_name() != after.file_name() {
        return after.into();
    }

    let Some(file_name) = after.file_name() else {
        return after.into();
    };

    file_name.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{MAX_REVISION_LINES, ReviewRow};
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn cli_accepts_a_directory_scan_commit_or_complete_pair() {
        let scan = Cli::try_parse_from(["m"]);
        let commit = Cli::try_parse_from(["m", "HEAD~2"]);
        let pair = Cli::try_parse_from(["m", "old.rs", "current.rs"]);

        assert!(scan.is_ok());
        assert!(commit.is_ok());
        assert!(pair.is_ok());
    }

    #[test]
    fn shared_file_names_hide_temporary_directories() {
        let path = display_pair_path(Path::new("/alpha/beta.rs"), Path::new("/gamma/beta.rs"));

        assert_eq!(path, Path::new("beta.rs"));
    }

    #[test]
    fn distinct_file_names_preserve_the_current_path() {
        let path = display_pair_path(Path::new("old.rs"), Path::new("src/current.rs"));

        assert_eq!(path, Path::new("src/current.rs"));
    }

    #[test]
    fn oversized_pair_is_retained_without_decoding_the_other_revision() {
        let directory = TempDir::new().expect("temporary directory");
        let before = directory.path().join("before.txt");
        let after = directory.path().join("after.txt");
        fs::write(&before, [0xff]).expect("write invalid UTF-8 before input");
        fs::write(&after, b"12345").expect("write oversized after input");

        let review = load_file_pair(&before, &after, 4)
            .expect("inspect oversized pair")
            .expect("oversized pair stays visible");

        assert!(matches!(
            review,
            ReviewEntry::Notice(FileNotice::TooLarge {
                path,
                before_bytes: Some(1),
                after_bytes: Some(5),
                limit_bytes: 4,
            }) if Path::new(&path) == after
        ));
    }

    #[test]
    fn pair_at_the_exact_limit_is_diffed() {
        let directory = TempDir::new().expect("temporary directory");
        let before = directory.path().join("before.txt");
        let after = directory.path().join("after.txt");
        fs::write(&before, b"1234").expect("write before input");
        fs::write(&after, b"5678").expect("write after input");

        let review = load_file_pair(&before, &after, 4)
            .expect("diff exact-limit pair")
            .expect("changed pair stays visible");

        let ReviewEntry::Diff(diff) = review else {
            panic!("an exact-limit text pair must remain a diff");
        };
        assert_eq!(Path::new(&diff.path), after);
        let rows = diff.hunks.iter().flat_map(|hunk| &hunk.rows);
        for (before, expected) in [(true, "1234"), (false, "5678")] {
            assert!(rows.clone().any(|row| {
                let line = match (before, row) {
                    (true, ReviewRow::Removed(line)) | (false, ReviewRow::Added(line)) => line,
                    _ => return false,
                };
                let text = line
                    .spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>();
                line.number == 1 && text == expected
            }));
        }
    }

    #[test]
    fn line_dense_pair_stays_visible_as_a_notice() {
        let directory = TempDir::new().expect("temporary directory");
        let before = directory.path().join("before.txt");
        let after = directory.path().join("after.txt");
        fs::write(&before, b"before\n").expect("write before input");
        fs::write(&after, "\n".repeat(MAX_REVISION_LINES + 1))
            .expect("write line-dense after input");

        let review = load_file_pair(&before, &after, MAX_REVISION_BYTES)
            .expect("inspect line-dense pair")
            .expect("line-dense pair stays visible");

        assert!(matches!(
            review,
            ReviewEntry::Notice(FileNotice::TooManyLines {
                path,
                before_lines: Some(1),
                after_lines: Some(lines),
                limit_lines: MAX_REVISION_LINES,
            }) if Path::new(&path) == after && lines == MAX_REVISION_LINES + 1
        ));
    }
}
