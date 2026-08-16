use anyhow::{Context, Result, bail};
use gix::ObjectId;
use gix::bstr::{BStr, ByteSlice};
use gix::object::tree::diff::ChangeDetached;
use mig::diff::diff_file;
use mig::review::{
    FileNotice, FileReview, MAX_REVISION_BYTES, MAX_REVISION_LINES, revision_line_count,
};
use std::cmp::Ordering;
use std::path::Path;

/// Reviews introduced by one commit, using its first parent as the baseline.
pub(crate) fn diff_commit(directory: &Path, commitish: &Path) -> Result<Vec<FileReview>> {
    diff_commit_with_limit(directory, commitish, MAX_REVISION_BYTES)
}

fn diff_commit_with_limit(
    directory: &Path,
    commitish: &Path,
    limit: u64,
) -> Result<Vec<FileReview>> {
    let repo = gix::discover(directory);
    let repo = repo.with_context(|| {
        format!(
            "failed to discover a Git repository from {}",
            directory.display()
        )
    })?;
    let commitish_bytes = gix::path::try_into_bstr(commitish).with_context(|| {
        format!(
            "commitish is not representable in this repository: {}",
            commitish.display()
        )
    })?;
    let commit_id = repo.rev_parse_single(commitish_bytes.as_ref());
    let commit_id = commit_id
        .with_context(|| format!("failed to resolve commitish {}", commitish.display()))?;
    let commit_object = commit_id.object();
    let commit_object = commit_object
        .with_context(|| format!("failed to read commitish {}", commitish.display()))?;
    let commit = commit_object.peel_to_commit();
    let commit = commit.with_context(|| {
        format!(
            "commitish does not resolve to a commit: {}",
            commitish.display()
        )
    })?;

    let parent_id = commit.parent_ids().next().map(|id| id.detach());
    let after_tree = commit.tree();
    let after_tree = after_tree
        .with_context(|| format!("failed to read the tree for {}", commitish.display()))?;
    let before_tree = match parent_id {
        None => repo.empty_tree(),
        Some(parent_id) => {
            // Mig has a two-revision model, so merge commits use Git's first-parent view.
            let parent = repo.find_commit(parent_id);
            let parent = parent.with_context(|| {
                format!("failed to read the first parent of {}", commitish.display())
            })?;
            let tree = parent.tree();
            tree.with_context(|| {
                format!(
                    "failed to read the first-parent tree for {}",
                    commitish.display()
                )
            })?
        }
    };

    // Keep renames as an addition and deletion, matching worktree review semantics.
    let options = gix::diff::Options::default().with_rewrites(None);
    let changes = repo.diff_tree_to_tree(&before_tree, &after_tree, Some(options));
    let changes = changes.with_context(|| {
        format!(
            "failed to compare {} with its first parent",
            commitish.display()
        )
    })?;
    let mut reviews = Vec::new();

    for change in changes {
        let change = changed_blobs(&repo, change)?;
        let Some(change) = change else {
            continue;
        };
        let review = plan_change(&repo, change, limit)?;
        let Some(review) = review else {
            continue;
        };
        reviews.push(review);
    }

    reviews.sort_by(review_order);
    Ok(reviews)
}

/// Immutable object identity and size for one side of a committed change.
#[derive(Clone, Copy)]
struct BlobRevision {
    object: ObjectId,
    bytes: u64,
}

/// One UTF-8 path and the committed blobs that differ there.
struct ChangedBlobs {
    path: String,
    before: Option<BlobRevision>,
    after: Option<BlobRevision>,
}

fn changed_blobs(repo: &gix::Repository, change: ChangeDetached) -> Result<Option<ChangedBlobs>> {
    match change {
        ChangeDetached::Rewrite { .. } => {
            bail!("Git reported a rewrite while rename detection was disabled")
        }
        ChangeDetached::Addition { entry_mode, .. } if !entry_mode.is_blob() => Ok(None),
        ChangeDetached::Deletion { entry_mode, .. } if !entry_mode.is_blob() => Ok(None),
        ChangeDetached::Modification {
            previous_entry_mode,
            entry_mode,
            ..
        } if !previous_entry_mode.is_blob() || !entry_mode.is_blob() => Ok(None),
        ChangeDetached::Addition {
            location,
            entry_mode: _,
            id,
            ..
        } => {
            let path = decode_git_path(location.as_bstr());
            let Some(path) = path else {
                return Ok(None);
            };
            let after = inspect_blob(repo, id, &path, "current")?;
            Ok(Some(ChangedBlobs {
                path,
                before: None,
                after: Some(after),
            }))
        }
        ChangeDetached::Deletion {
            location,
            entry_mode: _,
            id,
            ..
        } => {
            let path = decode_git_path(location.as_bstr());
            let Some(path) = path else {
                return Ok(None);
            };
            let before = inspect_blob(repo, id, &path, "previous")?;
            Ok(Some(ChangedBlobs {
                path,
                before: Some(before),
                after: None,
            }))
        }
        ChangeDetached::Modification {
            location,
            previous_entry_mode: _,
            previous_id,
            entry_mode: _,
            id,
        } => {
            let path = decode_git_path(location.as_bstr());
            let Some(path) = path else {
                return Ok(None);
            };
            let before = inspect_blob(repo, previous_id, &path, "previous")?;
            let after = inspect_blob(repo, id, &path, "current")?;
            Ok(Some(ChangedBlobs {
                path,
                before: Some(before),
                after: Some(after),
            }))
        }
    }
}

/// Header-only size check before either side of a pair is allocated.
fn inspect_blob(
    repo: &gix::Repository,
    object: ObjectId,
    path: &str,
    side: &str,
) -> Result<BlobRevision> {
    let header = repo.find_header(object);
    let header = header.with_context(|| format!("failed to inspect the {side} blob for {path}"))?;
    if header.kind() != gix::objs::Kind::Blob {
        bail!("the {side} object for {path} is not a blob");
    }

    Ok(BlobRevision {
        object,
        bytes: header.size(),
    })
}

/// Bounded committed pair projected into the same review boundary as worktree input.
fn plan_change(
    repo: &gix::Repository,
    change: ChangedBlobs,
    limit: u64,
) -> Result<Option<FileReview>> {
    let before_bytes = change.before.as_ref().map(|blob| blob.bytes);
    let after_bytes = change.after.as_ref().map(|blob| blob.bytes);
    if before_bytes.is_some_and(|bytes| bytes > limit)
        || after_bytes.is_some_and(|bytes| bytes > limit)
    {
        let notice = FileNotice::too_large(change.path, before_bytes, after_bytes, limit);
        return Ok(Some(FileReview::Notice(notice)));
    }

    let before = match change.before {
        None => Vec::new(),
        Some(before) => {
            let before = read_blob(repo, before, &change.path, "previous");
            before?
        }
    };
    let after = match change.after {
        None => Vec::new(),
        Some(after) => {
            let after = read_blob(repo, after, &change.path, "current");
            after?
        }
    };
    let Some(before) = decode_text(before) else {
        return Ok(None);
    };
    let Some(after) = decode_text(after) else {
        return Ok(None);
    };
    if before == after {
        return Ok(None);
    }

    let before_lines = before_bytes.map(|_| revision_line_count(&before));
    let after_lines = after_bytes.map(|_| revision_line_count(&after));
    if before_lines.is_some_and(|lines| lines > MAX_REVISION_LINES)
        || after_lines.is_some_and(|lines| lines > MAX_REVISION_LINES)
    {
        let notice =
            FileNotice::too_many_lines(change.path, before_lines, after_lines, MAX_REVISION_LINES);
        return Ok(Some(FileReview::Notice(notice)));
    }

    let diff = diff_file(&change.path, &before, &after)?;
    if diff.hunks.is_empty() {
        return Ok(None);
    }

    Ok(Some(FileReview::from(diff)))
}

/// Blob body whose length must still agree with its previously inspected header.
fn read_blob(
    repo: &gix::Repository,
    revision: BlobRevision,
    path: &str,
    side: &str,
) -> Result<Vec<u8>> {
    let blob = repo.find_blob(revision.object);
    let mut blob = blob.with_context(|| format!("failed to read the {side} blob for {path}"))?;
    if u64::try_from(blob.data.len()).unwrap_or(u64::MAX) != revision.bytes {
        bail!(
            "Git returned {} bytes for the {side} blob of {path}, reported as {} bytes",
            blob.data.len(),
            revision.bytes
        );
    }

    Ok(std::mem::take(&mut blob.data))
}

/// UTF-8 source without NUL bytes; other blobs are terminal-review binary input.
fn decode_text(source: Vec<u8>) -> Option<String> {
    if source.contains(&0) {
        return None;
    }

    String::from_utf8(source).ok()
}

/// UI labels are UTF-8, so an undecodable Git path is outside this review.
fn decode_git_path(path: &BStr) -> Option<String> {
    path.to_str().ok().map(ToOwned::to_owned)
}

/// Source paths first, inspected generated files last.
fn review_order(left: &FileReview, right: &FileReview) -> Ordering {
    left.is_generated()
        .cmp(&right.is_generated())
        .then_with(|| left.path().cmp(right.path()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn commit_review_uses_pinned_trees_and_keeps_generated_files_last() {
        let repository = initialized_repository();
        write(
            repository.path(),
            "changed.rs",
            "fn answer() -> &'static str { \"baseline\" }\n",
        );
        write(repository.path(), "old-name.rs", "fn renamed() {}\n");
        write(repository.path(), "removed.txt", "removed\n");
        write(
            repository.path(),
            "a-generated.rs",
            "// @generated\nfn generated() -> u8 { 1 }\n",
        );
        write_bytes(repository.path(), "binary.dat", b"old\0binary\n");
        git(repository.path(), &["add", "."]);
        git(repository.path(), &["commit", "--quiet", "-m", "baseline"]);

        write(repository.path(), "added.txt", "added\n");
        write(
            repository.path(),
            "changed.rs",
            "fn answer() -> &'static str { \"selected\" }\n",
        );
        git(repository.path(), &["mv", "old-name.rs", "new-name.rs"]);
        fs::remove_file(repository.path().join("removed.txt")).expect("remove tracked input");
        write(
            repository.path(),
            "a-generated.rs",
            "// @generated\nfn generated() -> u8 { 2 }\n",
        );
        write_bytes(repository.path(), "binary.dat", b"new\0binary\n");
        git(repository.path(), &["add", "."]);
        git(repository.path(), &["commit", "--quiet", "-m", "change"]);

        write(
            repository.path(),
            "changed.rs",
            "fn answer() -> &'static str { \"later\" }\n",
        );
        git(repository.path(), &["add", "."]);
        git(repository.path(), &["commit", "--quiet", "-m", "later"]);
        write(
            repository.path(),
            "changed.rs",
            "fn answer() -> &'static str { \"worktree\" }\n",
        );
        fs::create_dir(repository.path().join("nested")).expect("create nested invocation path");

        let reviews = diff_commit(&repository.path().join("nested"), Path::new("HEAD~1"))
            .expect("review commit");
        let paths = reviews.iter().map(FileReview::path).collect::<Vec<_>>();

        assert_eq!(
            paths,
            vec![
                "added.txt",
                "changed.rs",
                "new-name.rs",
                "old-name.rs",
                "removed.txt",
                "a-generated.rs"
            ]
        );
        let changed = reviews
            .iter()
            .find(|review| review.path() == "changed.rs")
            .expect("changed file review");
        let rendered = format!("{changed:?}");
        assert!(rendered.contains("answer"));
        assert!(rendered.contains("selected"));
        assert!(!rendered.contains("later"));
        assert!(!rendered.contains("worktree"));

        let regex_reviews = diff_commit(&repository.path().join("nested"), Path::new(":/ch.nge"))
            .expect("review commit selected by message regex");
        let regex_paths = regex_reviews
            .iter()
            .map(FileReview::path)
            .collect::<Vec<_>>();
        assert_eq!(regex_paths, paths);
    }

    #[test]
    fn root_commit_and_annotated_tag_are_reviewed_against_the_empty_tree() {
        let repository = initialized_repository();
        write(repository.path(), "root.rs", "fn root() {}\n");
        git(repository.path(), &["add", "."]);
        git(repository.path(), &["commit", "--quiet", "-m", "root"]);
        git(
            repository.path(),
            &["tag", "-a", "root-release", "-m", "root release"],
        );

        let reviews =
            diff_commit(repository.path(), Path::new("root-release")).expect("review root tag");

        assert!(matches!(
            reviews.as_slice(),
            [FileReview::Diff(diff)] if diff.path == "root.rs" && !diff.hunks.is_empty()
        ));
    }

    #[test]
    fn merge_commit_is_compared_with_its_first_parent() {
        let repository = initialized_repository();
        write(repository.path(), "main.txt", "base\n");
        write(repository.path(), "side.txt", "base\n");
        git(repository.path(), &["add", "."]);
        git(repository.path(), &["commit", "--quiet", "-m", "base"]);
        git(repository.path(), &["branch", "side"]);

        write(repository.path(), "main.txt", "main\n");
        git(repository.path(), &["add", "."]);
        git(repository.path(), &["commit", "--quiet", "-m", "main"]);
        git(repository.path(), &["switch", "--quiet", "side"]);
        write(repository.path(), "side.txt", "side\n");
        git(repository.path(), &["add", "."]);
        git(repository.path(), &["commit", "--quiet", "-m", "side"]);
        git(repository.path(), &["switch", "--quiet", "main"]);
        git(
            repository.path(),
            &["merge", "--quiet", "--no-ff", "side", "-m", "merge"],
        );

        let reviews = diff_commit(repository.path(), Path::new("HEAD")).expect("review merge");
        let paths = reviews.iter().map(FileReview::path).collect::<Vec<_>>();

        assert_eq!(paths, vec!["side.txt"]);
    }

    #[test]
    fn oversized_commit_blob_stays_visible_without_loading_its_pair() {
        let repository = initialized_repository();
        write_bytes(repository.path(), "large.txt", &[0xff]);
        git(repository.path(), &["add", "."]);
        git(repository.path(), &["commit", "--quiet", "-m", "baseline"]);
        write(repository.path(), "large.txt", "12345");
        git(repository.path(), &["add", "."]);
        git(repository.path(), &["commit", "--quiet", "-m", "large"]);

        let reviews = diff_commit_with_limit(repository.path(), Path::new("HEAD"), 4)
            .expect("review oversized commit");

        assert!(matches!(
            reviews.as_slice(),
            [FileReview::Notice(FileNotice::TooLarge {
                path,
                before_bytes: Some(1),
                after_bytes: Some(5),
                limit_bytes: 4,
            })] if path == "large.txt"
        ));
    }

    #[test]
    fn non_commit_revision_is_rejected_as_a_commitish() {
        let repository = initialized_repository();
        git(
            repository.path(),
            &["commit", "--quiet", "--allow-empty", "-m", "empty"],
        );

        let error = diff_commit(repository.path(), Path::new("HEAD^{tree}"))
            .expect_err("tree must not be accepted as a commit");

        assert!(error.to_string().contains("does not resolve to a commit"));
    }

    #[test]
    fn sha256_root_commit_uses_the_repository_empty_tree() {
        let repository = TempDir::new().expect("temporary SHA-256 repository");
        git(
            repository.path(),
            &[
                "init",
                "--quiet",
                "--initial-branch=main",
                "--object-format=sha256",
            ],
        );
        git(repository.path(), &["config", "user.name", "Mig Test"]);
        git(
            repository.path(),
            &["config", "user.email", "mig@example.invalid"],
        );
        write(repository.path(), "root.txt", "root\n");
        git(repository.path(), &["add", "."]);
        git(repository.path(), &["commit", "--quiet", "-m", "root"]);

        let reviews =
            diff_commit(repository.path(), Path::new("HEAD")).expect("review SHA-256 root commit");

        assert!(matches!(
            reviews.as_slice(),
            [FileReview::Diff(diff)] if diff.path == "root.txt" && !diff.hunks.is_empty()
        ));
    }

    fn initialized_repository() -> TempDir {
        let repository = TempDir::new().expect("temporary repository");
        git(
            repository.path(),
            &["init", "--quiet", "--initial-branch=main"],
        );
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
