use crate::input::{BoundedBytes, OpenFile};
use anyhow::{Context, Result, bail};
use gix::ObjectId;
use gix::bstr::{BStr, BString, ByteSlice};
use mig::diff::diff_file;
use mig::review::{
    FileNotice, FileReview, MAX_REVISION_BYTES, MAX_REVISION_LINES, revision_line_count,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Pinned baseline state for one status candidate.
enum HeadRevision {
    Absent,
    Blob(HeadBlob),
    Unsupported,
}

/// Current filesystem state without following links or reviewing special entries.
enum WorktreeRevision {
    Absent,
    File(OpenFile),
    Unsupported,
}

/// Git provenance retained until the review ribbon is ordered.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ChangeClass {
    Dirty,
    Staged,
    Untracked,
}

/// Visible ribbon cadence, with inspected generated files deferred globally.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum RibbonClass {
    Dirty,
    Staged,
    Untracked,
    Generated,
}

/// Planned text reviews in dirty, staged, untracked, then generated cadence.
pub fn diff_directory(directory: &Path) -> Result<Vec<FileReview>> {
    diff_directory_with_limit(directory, MAX_REVISION_BYTES)
}

fn diff_directory_with_limit(directory: &Path, limit: u64) -> Result<Vec<FileReview>> {
    let repo = gix::discover(directory).with_context(|| {
        format!(
            "failed to discover a Git repository from {}",
            directory.display()
        )
    })?;
    let root = repo
        .workdir()
        .context("cannot review changes in a bare Git repository")?
        .to_owned();
    let head_tree_id = repo
        .head_tree_id_or_empty()
        .context("failed to resolve the HEAD tree")?
        .detach();
    let head_tree = repo
        .find_tree(head_tree_id)
        .context("failed to read the pinned HEAD tree")?;
    let changes = changed_paths(&repo, directory, head_tree_id)?;
    let mut reviews = Vec::new();

    for (class, path) in changes {
        let before = head_revision(&repo, &head_tree, &path)?;
        let before = match before {
            HeadRevision::Absent => None,
            HeadRevision::Blob(before) => Some(before),
            HeadRevision::Unsupported => continue,
        };
        let after = worktree_revision(&root, &path)?;
        let after = match after {
            WorktreeRevision::Absent => None,
            WorktreeRevision::File(after) => Some(after),
            WorktreeRevision::Unsupported => continue,
        };

        let before_bytes = before.as_ref().map(|blob| blob.bytes);
        let after_bytes = after.as_ref().map(OpenFile::bytes);
        if before_bytes.is_some_and(|bytes| bytes > limit)
            || after_bytes.is_some_and(|bytes| bytes > limit)
        {
            reviews.push((
                class,
                oversized_review(&path, before_bytes, after_bytes, limit),
            ));
            continue;
        }

        // Read the mutable side first so post-stat growth cannot force a HEAD allocation.
        let after = match after {
            None => Vec::new(),
            Some(after) => match after.read(limit)? {
                BoundedBytes::Contents(after) => after,
                BoundedBytes::TooLarge(bytes) => {
                    reviews.push((
                        class,
                        oversized_review(&path, before_bytes, Some(bytes), limit),
                    ));
                    continue;
                }
            },
        };
        let before = match before {
            None => Vec::new(),
            Some(before) => read_head_blob(&repo, before)?,
        };
        let Some(before) = decode_text(before) else {
            continue;
        };
        let Some(after) = decode_text(after) else {
            continue;
        };
        if before == after {
            continue;
        }
        let before_lines = before_bytes.map(|_| revision_line_count(&before));
        let after_lines = after_bytes.map(|_| revision_line_count(&after));
        if before_lines.is_some_and(|lines| lines > MAX_REVISION_LINES)
            || after_lines.is_some_and(|lines| lines > MAX_REVISION_LINES)
        {
            reviews.push((class, complexity_review(&path, before_lines, after_lines)));
            continue;
        }

        let label = path.to_string_lossy();
        let diff = diff_file(&label, &before, &after)?;
        reviews.push((class, FileReview::from(diff)));
    }

    reviews.sort_by(|(left_class, left), (right_class, right)| {
        ribbon_class(*left_class, left)
            .cmp(&ribbon_class(*right_class, right))
            .then_with(|| left.path().cmp(right.path()))
    });
    Ok(reviews.into_iter().map(|(_, review)| review).collect())
}

fn ribbon_class(class: ChangeClass, review: &FileReview) -> RibbonClass {
    if review.is_generated() {
        return RibbonClass::Generated;
    }

    match class {
        ChangeClass::Dirty => RibbonClass::Dirty,
        ChangeClass::Staged => RibbonClass::Staged,
        ChangeClass::Untracked => RibbonClass::Untracked,
    }
}

fn oversized_review(
    path: &Path,
    before_bytes: Option<u64>,
    after_bytes: Option<u64>,
    limit: u64,
) -> FileReview {
    let path = path.to_string_lossy().into_owned();
    let notice = FileNotice::too_large(path, before_bytes, after_bytes, limit);
    FileReview::Notice(notice)
}

fn complexity_review(
    path: &Path,
    before_lines: Option<usize>,
    after_lines: Option<usize>,
) -> FileReview {
    let path = path.to_string_lossy().into_owned();
    let notice = FileNotice::too_many_lines(path, before_lines, after_lines, MAX_REVISION_LINES);
    FileReview::Notice(notice)
}

/// Status candidates from the pinned tree, index, and regular worktree scope.
fn changed_paths(
    repo: &gix::Repository,
    directory: &Path,
    head_tree_id: ObjectId,
) -> Result<Vec<(ChangeClass, PathBuf)>> {
    let patterns = scope_patterns(repo, directory)?;
    let status = repo
        .status(gix::progress::Discard)
        .context("failed to prepare Git status")?
        .untracked_files(gix::status::UntrackedFiles::Files)
        .index_worktree_submodules(None)
        .tree_index_track_renames(gix::status::tree_index::TrackRenames::Disabled)
        .index_worktree_rewrites(None)
        .head_tree(head_tree_id);
    let items = status
        .into_iter(patterns)
        .context("failed to start Git status")?;
    let mut paths: BTreeMap<PathBuf, ChangeClass> = BTreeMap::new();

    for item in items {
        let item = item.context("failed while reading Git status")?;
        let Some(class) = status_change_class(&item) else {
            continue;
        };
        let Some(path) = decode_git_path(item.location()) else {
            continue;
        };
        paths
            .entry(path)
            .and_modify(|existing| *existing = (*existing).min(class))
            .or_insert(class);
    }

    let mut paths = paths
        .into_iter()
        .map(|(path, class)| (class, path))
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

/// Strongest visible status for one emitted tree/index/worktree fact.
fn status_change_class(item: &gix::status::Item) -> Option<ChangeClass> {
    match item {
        gix::status::Item::TreeIndex(_) => Some(ChangeClass::Staged),
        gix::status::Item::IndexWorktree(change) => match change {
            gix::status::index_worktree::Item::DirectoryContents { entry, .. }
                if matches!(entry.status, gix::dir::entry::Status::Untracked) =>
            {
                Some(ChangeClass::Untracked)
            }
            // `NeedsUpdate` is bookkeeping, not an unstaged content change.
            _ if change.summary().is_some() => Some(ChangeClass::Dirty),
            _ => None,
        },
    }
}

/// Root-anchored literal scope independent of the process current directory.
fn scope_patterns(repo: &gix::Repository, directory: &Path) -> Result<Vec<BString>> {
    let directory = gix::path::try_into_bstr(directory).with_context(|| {
        format!(
            "repository scope is not representable: {}",
            directory.display()
        )
    })?;
    let scope = repo
        .normalize_path(directory.as_ref())
        .with_context(|| format!("failed to normalize repository scope {directory}"))?;
    if scope.is_empty() {
        return Ok(Vec::new());
    }

    // `top` prevents gix from applying the process CWD as an additional prefix.
    let mut pattern = b":(top,literal)".to_vec();
    pattern.extend_from_slice(&scope);
    Ok(vec![BString::from(pattern)])
}

/// UI labels are UTF-8, so an undecodable Git path is outside this review.
fn decode_git_path(path: &BStr) -> Option<PathBuf> {
    let path = path.to_str().ok()?;
    Some(PathBuf::from(path))
}

/// Immutable blob identity and size, resolved before its contents are requested.
struct HeadBlob {
    object: ObjectId,
    bytes: u64,
}

fn head_revision(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
    path: &Path,
) -> Result<HeadRevision> {
    let entry = tree
        .lookup_entry_by_path(path)
        .with_context(|| format!("failed to look up {} in HEAD", path.display()))?;
    let Some(entry) = entry else {
        return Ok(HeadRevision::Absent);
    };
    if !entry.mode().is_blob() {
        return Ok(HeadRevision::Unsupported);
    }

    let object = entry.object_id();
    let header = repo
        .find_header(object)
        .with_context(|| format!("failed to inspect the HEAD blob for {}", path.display()))?;
    if header.kind() != gix::objs::Kind::Blob {
        return Ok(HeadRevision::Unsupported);
    }

    Ok(HeadRevision::Blob(HeadBlob {
        object,
        bytes: header.size(),
    }))
}

fn read_head_blob(repo: &gix::Repository, blob: HeadBlob) -> Result<Vec<u8>> {
    let mut contents = repo
        .find_blob(blob.object)
        .context("failed to read a pinned HEAD blob")?;
    if u64::try_from(contents.data.len()).unwrap_or(u64::MAX) != blob.bytes {
        bail!(
            "Git returned {} bytes for a blob reported as {} bytes",
            contents.data.len(),
            blob.bytes
        );
    }

    Ok(std::mem::take(&mut contents.data))
}

/// Current regular-file handle, absence, or an unsupported filesystem entry.
fn worktree_revision(root: &Path, path: &Path) -> Result<WorktreeRevision> {
    let full_path = root.join(path);
    let metadata = fs::symlink_metadata(&full_path);
    if metadata
        .as_ref()
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(WorktreeRevision::Absent);
    }
    let metadata =
        metadata.with_context(|| format!("failed to inspect file {}", full_path.display()))?;
    if !metadata.file_type().is_file() {
        return Ok(WorktreeRevision::Unsupported);
    }

    let source = OpenFile::open(&full_path)?;
    Ok(WorktreeRevision::File(source))
}

/// UTF-8 source without NUL bytes; other content is outside the terminal text review.
fn decode_text(source: Vec<u8>) -> Option<String> {
    if source.contains(&0) {
        return None;
    }

    String::from_utf8(source).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn real_git_scan_covers_the_net_recursive_worktree() {
        let repository = TempDir::new().expect("temporary repository");
        git(repository.path(), &["init", "--quiet"]);
        git(repository.path(), &["config", "user.name", "Mig Test"]);
        git(
            repository.path(),
            &["config", "user.email", "mig@example.invalid"],
        );

        write(repository.path(), ".gitignore", "ignored.rs\n");
        write(
            repository.path(),
            "changed.rs",
            "fn changed() -> u8 { 1 }\n",
        );
        write(
            repository.path(),
            "also-modified.rs",
            "fn also_modified() -> u8 { 1 }\n",
        );
        write(
            repository.path(),
            "a-generated.txt",
            "# @generated\nold generated text\n",
        );
        write(
            repository.path(),
            "extensionless",
            "old extensionless text\n",
        );
        write_bytes(repository.path(), "invalid.dat", &[0xff, b'\n']);
        write_bytes(repository.path(), "nul.dat", b"old\0binary\n");
        write(repository.path(), "deleted.rs", "fn deleted() {}\n");
        write(
            repository.path(),
            "nested/staged.rs",
            "fn staged() -> u8 { 1 }\n",
        );
        write(repository.path(), "stable.rs", "fn stable() {}\n");
        write(repository.path(), "notes.txt", "before\n");
        write(
            repository.path(),
            "z-generated.txt",
            "# @generated\nold generated tail\n",
        );
        git(repository.path(), &["add", "."]);
        git(repository.path(), &["commit", "--quiet", "-m", "baseline"]);

        let clean = diff_directory(repository.path()).expect("clean scan");
        assert!(clean.is_empty());

        write(
            repository.path(),
            "changed.rs",
            "fn changed() -> u8 { 2 }\n",
        );
        write(
            repository.path(),
            "also-modified.rs",
            "fn also_modified() -> u8 { 2 }\n",
        );
        git(repository.path(), &["add", "also-modified.rs"]);
        write(
            repository.path(),
            "also-modified.rs",
            "fn also_modified() -> u8 { 3 }\n",
        );
        write(
            repository.path(),
            "a-generated.txt",
            "# @generated\nnew generated text\n",
        );
        write(
            repository.path(),
            "extensionless",
            "new extensionless text\n",
        );
        write_bytes(repository.path(), "invalid.dat", &[0xfe, b'\n']);
        write_bytes(repository.path(), "nul.dat", b"new\0binary\n");
        fs::remove_file(repository.path().join("deleted.rs")).expect("delete tracked Rust file");
        write(
            repository.path(),
            "nested/staged.rs",
            "fn staged() -> u8 { 2 }\n",
        );
        git(repository.path(), &["add", "nested/staged.rs"]);
        write(
            repository.path(),
            "nested/untracked.rs",
            "fn untracked() {}\n",
        );
        write(repository.path(), "ignored.rs", "fn ignored() {}\n");
        write(repository.path(), "notes.txt", "after\n");
        write(
            repository.path(),
            "z-generated.txt",
            "# @generated\nnew generated tail\n",
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            symlink("changed.rs", repository.path().join("linked.txt"))
                .expect("create untracked symlink");
        }

        let diffs = diff_directory(repository.path()).expect("changed scan");
        let paths = diffs.iter().map(FileReview::path).collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                "also-modified.rs",
                "changed.rs",
                "deleted.rs",
                "extensionless",
                "notes.txt",
                "nested/staged.rs",
                "nested/untracked.rs",
                "a-generated.txt",
                "z-generated.txt"
            ]
        );
        assert!(
            diffs.iter().all(|review| {
                matches!(review, FileReview::Diff(diff) if !diff.hunks.is_empty())
            })
        );
        assert!(diffs[..7].iter().all(|review| !review.is_generated()));
        assert!(diffs[7..].iter().all(FileReview::is_generated));

        let nested = diff_directory(&repository.path().join("nested")).expect("nested scan");
        let nested_paths = nested.iter().map(FileReview::path).collect::<Vec<_>>();
        assert_eq!(
            nested_paths,
            vec!["nested/staged.rs", "nested/untracked.rs"]
        );
    }

    #[test]
    fn undecodable_git_paths_are_skipped_without_losing_neighbors() {
        let paths = [
            BStr::new(b"before.txt"),
            BStr::new(b"invalid-\xff.txt"),
            BStr::new(b"after.txt"),
        ]
        .into_iter()
        .filter_map(decode_git_path)
        .collect::<Vec<_>>();

        assert_eq!(
            paths,
            vec![PathBuf::from("before.txt"), PathBuf::from("after.txt")]
        );
    }

    #[test]
    fn review_compares_pinned_head_directly_to_the_worktree() {
        let repository = initialized_repository();
        write(repository.path(), "cancelled.txt", "head\n");
        write(repository.path(), "current.txt", "head\n");
        git(repository.path(), &["add", "."]);
        git(repository.path(), &["commit", "--quiet", "-m", "baseline"]);

        write(repository.path(), "cancelled.txt", "index\n");
        write(repository.path(), "current.txt", "index\n");
        git(repository.path(), &["add", "."]);
        write(repository.path(), "cancelled.txt", "head\n");
        write(repository.path(), "current.txt", "worktree\n");

        let reviews = diff_directory(repository.path()).expect("changed scan");

        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].path(), "current.txt");
        let FileReview::Diff(diff) = &reviews[0] else {
            panic!("small text change must remain a diff");
        };
        let rendered = diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.rows)
            .map(|row| format!("{row:?}"))
            .collect::<String>();
        assert!(rendered.contains("head"));
        assert!(rendered.contains("worktree"));
        assert!(!rendered.contains("index"));
    }

    #[test]
    fn unborn_and_nested_scans_use_standard_git_excludes() {
        let repository = initialized_repository();
        write(repository.path(), ".gitignore", "root-ignored/\n");
        write(
            repository.path(),
            "nested/.gitignore",
            "*.tmp\n!keep.tmp\ntracked-ignored.txt\n",
        );
        write(repository.path(), "nested/tracked-ignored.txt", "before\n");
        git(repository.path(), &["add", "."]);
        git(
            repository.path(),
            &["add", "--force", "nested/tracked-ignored.txt"],
        );
        git(repository.path(), &["commit", "--quiet", "-m", "baseline"]);

        let info_exclude = repository.path().join(".git/info/exclude");
        fs::write(&info_exclude, "nested/info-ignored.txt\n").expect("write info exclude");
        let global_exclude = repository.path().join(".git/info/global-exclude");
        fs::write(&global_exclude, "nested/global-ignored.txt\n").expect("write global exclude");
        git(
            repository.path(),
            &[
                "config",
                "core.excludesFile",
                global_exclude.to_str().expect("UTF-8 test path"),
            ],
        );

        write(repository.path(), "nested/tracked-ignored.txt", "after\n");
        write(repository.path(), "nested/.hidden.txt", "visible\n");
        write(repository.path(), "nested/drop.tmp", "ignored\n");
        write(repository.path(), "nested/keep.tmp", "visible\n");
        write(repository.path(), "nested/info-ignored.txt", "ignored\n");
        write(repository.path(), "nested/global-ignored.txt", "ignored\n");
        write(repository.path(), "root-ignored/child.txt", "ignored\n");

        let nested = diff_directory(&repository.path().join("nested")).expect("nested scan");
        let paths = nested.iter().map(FileReview::path).collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                "nested/tracked-ignored.txt",
                "nested/.hidden.txt",
                "nested/keep.tmp",
            ]
        );

        let unborn = initialized_repository();
        write(unborn.path(), ".gitignore", "ignored.txt\n");
        write(unborn.path(), "staged.txt", "staged\n");
        git(unborn.path(), &["add", ".gitignore", "staged.txt"]);
        write(unborn.path(), "untracked.txt", "untracked\n");
        write(unborn.path(), "ignored.txt", "ignored\n");

        let reviews = diff_directory(unborn.path()).expect("unborn scan");
        let paths = reviews.iter().map(FileReview::path).collect::<Vec<_>>();
        assert_eq!(paths, vec![".gitignore", "staged.txt", "untracked.txt"]);
    }

    #[test]
    fn oversized_worktree_revision_stays_in_the_review_without_decoding_head() {
        let repository = initialized_repository();
        write_bytes(repository.path(), "large.txt", &[0xff]);
        git(repository.path(), &["add", "large.txt"]);
        git(repository.path(), &["commit", "--quiet", "-m", "baseline"]);
        write(repository.path(), "large.txt", "12345");

        let reviews = diff_directory_with_limit(repository.path(), 4).expect("changed scan");

        assert_eq!(reviews.len(), 1);
        assert!(matches!(
            &reviews[0],
            FileReview::Notice(FileNotice::TooLarge {
                path,
                before_bytes: Some(1),
                after_bytes: Some(5),
                limit_bytes: 4,
            }) if path == "large.txt"
        ));
    }

    #[test]
    fn oversized_head_revision_stays_in_the_review_without_reading_either_body() {
        let repository = initialized_repository();
        write(repository.path(), "large.txt", "12345");
        git(repository.path(), &["add", "large.txt"]);
        git(repository.path(), &["commit", "--quiet", "-m", "baseline"]);
        write_bytes(repository.path(), "large.txt", &[0xff]);

        let reviews = diff_directory_with_limit(repository.path(), 4).expect("changed scan");

        assert!(matches!(
            reviews.as_slice(),
            [FileReview::Notice(FileNotice::TooLarge {
                before_bytes: Some(5),
                after_bytes: Some(1),
                limit_bytes: 4,
                ..
            })]
        ));
    }

    #[test]
    fn deleted_oversized_head_revision_retains_an_absent_current_side() {
        let repository = initialized_repository();
        write(repository.path(), "deleted.txt", "12345");
        git(repository.path(), &["add", "deleted.txt"]);
        git(repository.path(), &["commit", "--quiet", "-m", "baseline"]);
        fs::remove_file(repository.path().join("deleted.txt")).expect("delete oversized input");

        let reviews = diff_directory_with_limit(repository.path(), 4).expect("changed scan");

        assert!(matches!(
            reviews.as_slice(),
            [FileReview::Notice(FileNotice::TooLarge {
                before_bytes: Some(5),
                after_bytes: None,
                limit_bytes: 4,
                ..
            })]
        ));
    }

    #[test]
    fn added_oversized_revision_retains_an_absent_head_side() {
        let repository = initialized_repository();
        write(repository.path(), "added.txt", "12345");

        let reviews = diff_directory_with_limit(repository.path(), 4).expect("changed scan");

        assert!(matches!(
            reviews.as_slice(),
            [FileReview::Notice(FileNotice::TooLarge {
                before_bytes: None,
                after_bytes: Some(5),
                limit_bytes: 4,
                ..
            })]
        ));
    }

    #[test]
    fn exact_limit_is_still_diffed() {
        let repository = initialized_repository();
        write(repository.path(), "exact.txt", "1234");
        git(repository.path(), &["add", "exact.txt"]);
        git(repository.path(), &["commit", "--quiet", "-m", "baseline"]);
        write(repository.path(), "exact.txt", "5678");

        let reviews = diff_directory_with_limit(repository.path(), 4).expect("changed scan");

        assert!(matches!(
            reviews.as_slice(),
            [FileReview::Diff(diff)] if diff.path == "exact.txt" && !diff.hunks.is_empty()
        ));
    }

    #[test]
    fn rename_is_reviewed_as_one_addition_and_one_deletion() {
        let repository = initialized_repository();
        write(repository.path(), "old.rs", "fn renamed() {}\n");
        git(repository.path(), &["add", "old.rs"]);
        git(repository.path(), &["commit", "--quiet", "-m", "baseline"]);
        git(repository.path(), &["mv", "old.rs", "new.rs"]);

        let reviews = diff_directory(repository.path()).expect("renamed scan");
        let paths = reviews.iter().map(FileReview::path).collect::<Vec<_>>();

        assert_eq!(paths, vec!["new.rs", "old.rs"]);
    }

    #[test]
    fn sha256_repository_uses_the_same_status_and_object_path() {
        let repository =
            initialized_repository_with(&["init", "--quiet", "--object-format=sha256"]);
        write(repository.path(), "tracked.txt", "before\n");
        git(repository.path(), &["add", "tracked.txt"]);
        git(repository.path(), &["commit", "--quiet", "-m", "baseline"]);
        write(repository.path(), "tracked.txt", "after\n");
        write(repository.path(), "untracked.txt", "new\n");

        let reviews = diff_directory(repository.path()).expect("SHA-256 scan");
        let paths = reviews.iter().map(FileReview::path).collect::<Vec<_>>();

        assert_eq!(paths, vec!["tracked.txt", "untracked.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn baseline_symlink_is_not_reinterpreted_as_text() {
        use std::os::unix::fs::symlink;

        let repository = initialized_repository();
        let path = repository.path().join("linked.txt");
        symlink("old-target", &path).expect("create tracked symlink");
        git(repository.path(), &["add", "linked.txt"]);
        git(repository.path(), &["commit", "--quiet", "-m", "baseline"]);
        fs::remove_file(&path).expect("remove tracked symlink");
        fs::write(&path, "regular replacement\n").expect("replace symlink with regular file");

        let reviews = diff_directory(repository.path()).expect("type-change scan");

        assert!(reviews.is_empty());
    }

    fn initialized_repository() -> TempDir {
        initialized_repository_with(&["init", "--quiet"])
    }

    fn initialized_repository_with(init_args: &[&str]) -> TempDir {
        let repository = TempDir::new().expect("temporary repository");
        git(repository.path(), init_args);
        git(repository.path(), &["config", "user.name", "Mig Test"]);
        git(
            repository.path(),
            &["config", "user.email", "mig@example.invalid"],
        );
        repository
    }

    /// Real filesystem input committed through Git's external contract.
    fn write(root: &Path, path: &str, source: &str) {
        write_bytes(root, path, source.as_bytes());
    }

    /// Real binary filesystem input committed through Git's external contract.
    fn write_bytes(root: &Path, path: &str, source: &[u8]) {
        let path = root.join(path);
        let Some(parent) = path.parent() else {
            panic!("test path needs a parent");
        };
        fs::create_dir_all(parent).expect("create test parent");
        fs::write(path, source).expect("write test source");
    }

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git").arg("-C").arg(root).args(args).output();
        let output = output.expect("execute Git");
        assert!(
            output.status.success(),
            "Git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
