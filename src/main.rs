mod commit;
mod input;
mod worktree;

use crate::input::{BoundedBytes, OpenFile};
use anyhow::{Context, Result, bail};
use clap::Parser;
use mig::review::{
    FileNotice, FileReview, MAX_REVISION_BYTES, MAX_REVISION_LINES, revision_line_count,
};
use mig::{diff::diff_file, ui};
use std::env;
use std::path::{Path, PathBuf};

/// Current worktree changes, one commit, or two concrete text-file revisions.
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

    /// Display path and syntax hint to use when the inputs are temporary files.
    #[arg(long, requires = "after")]
    path: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match (cli.commitish_or_before, cli.after) {
        (None, None) => review_current_directory(),
        (Some(commitish), None) => review_commit(&commitish),
        (Some(before), Some(after)) => review_file_pair(before, after, cli.path),
        (None, Some(_)) => bail!("AFTER cannot be provided without BEFORE"),
    }
}

/// All current Git changes rooted beneath the invocation directory.
fn review_current_directory() -> Result<()> {
    let directory = env::current_dir();
    let directory = directory.context("failed to locate the current directory")?;
    let diffs = worktree::diff_directory(&directory)?;
    if diffs.is_empty() {
        return Ok(());
    }

    ui::run(diffs)
}

/// One commit against its first parent, or the empty tree for a root commit.
fn review_commit(commitish: &Path) -> Result<()> {
    let directory = env::current_dir();
    let directory = directory.context("failed to locate the current directory")?;
    let diffs = commit::diff_commit(&directory, commitish)?;
    if diffs.is_empty() {
        return Ok(());
    }

    ui::run(diffs)
}

/// One explicit pair, independent of any surrounding Git worktree.
fn review_file_pair(before: PathBuf, after: PathBuf, path: Option<PathBuf>) -> Result<()> {
    let review = plan_file_pair(&before, &after, path.as_deref(), MAX_REVISION_BYTES)?;
    let Some(review) = review else {
        return Ok(());
    };

    ui::run(vec![review])
}

fn plan_file_pair(
    before: &Path,
    after: &Path,
    path: Option<&Path>,
    limit: u64,
) -> Result<Option<FileReview>> {
    let before_file = OpenFile::open(before)?;
    let after_file = OpenFile::open(after)?;
    let before_bytes = before_file.bytes();
    let after_bytes = after_file.bytes();
    let path = path
        .map(Path::to_owned)
        .unwrap_or_else(|| display_pair_path(before, after));
    let path = path.to_string_lossy().into_owned();

    if before_bytes > limit || after_bytes > limit {
        return Ok(Some(oversized_pair(path, before_bytes, after_bytes, limit)));
    }

    let before_source = before_file.read(limit)?;
    let before_source = match before_source {
        BoundedBytes::TooLarge(bytes) => {
            return Ok(Some(oversized_pair(path, bytes, after_bytes, limit)));
        }
        BoundedBytes::Contents(source) => source,
    };
    let after_source = after_file.read(limit)?;
    let after_source = match after_source {
        BoundedBytes::TooLarge(bytes) => {
            return Ok(Some(oversized_pair(path, before_bytes, bytes, limit)));
        }
        BoundedBytes::Contents(source) => source,
    };
    let before_source = String::from_utf8(before_source)
        .with_context(|| format!("file is not UTF-8: {}", before.display()))?;
    let after_source = String::from_utf8(after_source)
        .with_context(|| format!("file is not UTF-8: {}", after.display()))?;
    let before_lines = revision_line_count(&before_source);
    let after_lines = revision_line_count(&after_source);
    if before_lines > MAX_REVISION_LINES || after_lines > MAX_REVISION_LINES {
        let notice = FileNotice::too_many_lines(
            path,
            Some(before_lines),
            Some(after_lines),
            MAX_REVISION_LINES,
        );
        return Ok(Some(FileReview::Notice(notice)));
    }
    let diff = diff_file(&path, &before_source, &after_source)?;
    if diff.hunks.is_empty() {
        return Ok(None);
    }

    Ok(Some(FileReview::from(diff)))
}

fn oversized_pair(path: String, before_bytes: u64, after_bytes: u64, limit: u64) -> FileReview {
    let notice = FileNotice::too_large(path, Some(before_bytes), Some(after_bytes), limit);
    FileReview::Notice(notice)
}

/// Stable UI label for Git difftool pairs whose directories are temporary.
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
    use mig::diff::{CodeRole, DiffRow};
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn cli_accepts_a_directory_scan_commit_or_complete_pair() {
        let scan = Cli::try_parse_from(["m"]);
        let commit = Cli::try_parse_from(["m", "HEAD~2"]);
        let pair = Cli::try_parse_from(["m", "old.rs", "current.rs"]);
        let path_without_pair = Cli::try_parse_from(["m", "--path", "src/lib.rs"]);
        let path_with_commit = Cli::try_parse_from(["m", "HEAD", "--path", "src/lib.rs"]);

        assert!(scan.is_ok());
        assert!(commit.is_ok());
        assert!(pair.is_ok());
        assert!(path_without_pair.is_err());
        assert!(path_with_commit.is_err());
    }

    #[test]
    fn shared_file_names_hide_temporary_directories() {
        let path = display_pair_path(
            Path::new("/tmp/mig-before/src/lib.rs"),
            Path::new("/work/project/src/lib.rs"),
        );

        assert_eq!(path, Path::new("lib.rs"));
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

        let review = plan_file_pair(&before, &after, Some(Path::new("input.txt")), 4)
            .expect("plan oversized pair")
            .expect("oversized pair stays visible");

        assert!(matches!(
            review,
            FileReview::Notice(FileNotice::TooLarge {
                path,
                before_bytes: Some(1),
                after_bytes: Some(5),
                limit_bytes: 4,
            }) if path == "input.txt"
        ));
    }

    #[test]
    fn pair_at_the_exact_limit_is_diffed() {
        let directory = TempDir::new().expect("temporary directory");
        let before = directory.path().join("before.txt");
        let after = directory.path().join("after.txt");
        fs::write(&before, b"1234").expect("write before input");
        fs::write(&after, b"5678").expect("write after input");

        let review = plan_file_pair(&before, &after, Some(Path::new("input.txt")), 4)
            .expect("plan exact-limit pair")
            .expect("changed pair stays visible");

        assert!(matches!(
            review,
            FileReview::Diff(diff) if diff.path == "input.txt" && !diff.hunks.is_empty()
        ));
    }

    #[test]
    fn line_dense_pair_stays_visible_as_a_notice() {
        let directory = TempDir::new().expect("temporary directory");
        let before = directory.path().join("before.txt");
        let after = directory.path().join("after.txt");
        fs::write(&before, b"before\n").expect("write before input");
        fs::write(&after, "\n".repeat(MAX_REVISION_LINES + 1))
            .expect("write line-dense after input");

        let review = plan_file_pair(
            &before,
            &after,
            Some(Path::new("dense.txt")),
            MAX_REVISION_BYTES,
        )
        .expect("plan line-dense pair")
        .expect("line-dense pair stays visible");

        assert!(matches!(
            review,
            FileReview::Notice(FileNotice::TooManyLines {
                path,
                before_lines: Some(1),
                after_lines: Some(lines),
                limit_lines: MAX_REVISION_LINES,
            }) if path == "dense.txt" && lines == MAX_REVISION_LINES + 1
        ));
    }

    #[test]
    fn explicit_html_pair_keeps_a_whitespace_noisy_wrapped_tag_atomic() {
        let directory = TempDir::new().expect("temporary directory");
        let before_path = directory.path().join("before.html");
        let after_path = directory.path().join("after.html");
        let before = "<article>\n\t<img\n\t\tsrc=\"avatar.webp\"\n\t/>\n</article>\n";
        let after = concat!(
            "<article>\n",
            "\t<div>\n",
            "\t\t<img",
            "                           ",
            "\n",
            "\t\t\tsrc=\"avatar.webp\"\n",
            "\t\t/>\n",
            "\t</div>\n",
            "</article>\n",
        );
        fs::write(&before_path, before).expect("write before HTML");
        fs::write(&after_path, after).expect("write after HTML");

        let review = plan_file_pair(&before_path, &after_path, None, MAX_REVISION_BYTES)
            .expect("plan explicit HTML pair")
            .expect("changed pair stays visible");
        let FileReview::Diff(diff) = review else {
            panic!("HTML pair needs a concrete diff");
        };
        assert!(diff.path.ends_with("after.html"));
        let reflow = diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .filter_map(|row| {
                let DiffRow::Code {
                    line,
                    role: CodeRole::Reflow,
                } = row
                else {
                    return None;
                };
                let text = line
                    .spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>();
                Some((line.number, text))
            })
            .collect::<Vec<_>>();
        for expected in [
            (3, "\t\t<img                           "),
            (4, "\t\t\tsrc=\"avatar.webp\""),
            (5, "\t\t/>"),
        ] {
            assert!(
                reflow
                    .iter()
                    .any(|(number, text)| *number == expected.0 && text == expected.1),
                "missing exact current row {expected:?}",
            );
        }
        for needle in ["<img", "src=\"avatar.webp\"", "/>"] {
            let retained = diff
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.rows)
                .filter(|row| {
                    matches!(
                        row,
                        DiffRow::Code {
                            line,
                            role: CodeRole::Reflow,
                        } if line
                            .spans
                            .iter()
                            .map(|span| span.text.as_str())
                            .collect::<String>()
                            .contains(needle)
                    )
                })
                .count();
            assert_eq!(retained, 1, "{needle:?} must stay in the retained tag");
            assert!(!diff.hunks.iter().flat_map(|hunk| &hunk.rows).any(|row| {
                let DiffRow::Linewise { before, after } = row else {
                    return false;
                };
                before.iter().chain(after).any(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.text.as_str())
                        .collect::<String>()
                        .contains(needle)
                })
            }));
        }
    }
}
