mod input;
mod worktree;

use crate::input::{BoundedBytes, OpenFile};
use anyhow::{Context, Result, bail};
use clap::Parser;
use mig::review::{FileNotice, FileReview, MAX_REVISION_BYTES};
use mig::{diff::diff_file, ui};
use std::env;
use std::path::{Path, PathBuf};

/// Current worktree changes or two concrete text-file revisions.
#[derive(Parser)]
#[command(
    name = "m",
    version,
    about = "Review source changes with structural Rust diffs"
)]
struct Cli {
    /// Previous version; omit both paths to scan the current directory.
    #[arg(requires = "after")]
    before: Option<PathBuf>,

    /// Current version of the source file.
    after: Option<PathBuf>,

    /// Display path and syntax hint to use when the inputs are temporary files.
    #[arg(long, requires = "before")]
    path: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let Some(before) = cli.before else {
        return review_current_directory();
    };
    let Some(after) = cli.after else {
        bail!("BEFORE and AFTER must be provided together");
    };

    review_file_pair(before, after, cli.path)
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
    let diff = diff_file(&path, &before_source, &after_source)?;
    if diff.windows.is_empty() {
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
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn cli_accepts_a_directory_scan_or_a_complete_pair() {
        let scan = Cli::try_parse_from(["m"]);
        let pair = Cli::try_parse_from(["m", "old.rs", "current.rs"]);
        let incomplete = Cli::try_parse_from(["m", "old.rs"]);
        let path_without_pair = Cli::try_parse_from(["m", "--path", "src/lib.rs"]);

        assert!(scan.is_ok());
        assert!(pair.is_ok());
        assert!(incomplete.is_err());
        assert!(path_without_pair.is_err());
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
            FileReview::Diff(diff) if diff.path == "input.txt" && !diff.windows.is_empty()
        ));
    }
}
