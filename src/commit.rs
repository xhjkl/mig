use crate::{
    input::{GitBlob, decode_text},
    review_source_pair,
};
use anyhow::{Context, Result, bail};
use gix::ObjectId;
use gix::bstr::{BStr, ByteSlice};
use gix::object::tree::diff::ChangeDetached;
use mig::review::{FileNotice, MAX_REVISION_BYTES, ReviewItem};
use std::cmp::Ordering;
use std::path::Path;

/// Reviews introduced by one commit, using its first parent as the baseline.
pub(crate) fn diff_commit(directory: &Path, commitish: &Path) -> Result<Vec<ReviewItem>> {
    diff_commit_with_limit(directory, commitish, MAX_REVISION_BYTES)
}

fn diff_commit_with_limit(
    directory: &Path,
    commitish: &Path,
    limit: u64,
) -> Result<Vec<ReviewItem>> {
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
        let review = review_change(&repo, change, limit)?;
        let Some(review) = review else {
            continue;
        };
        reviews.push(review);
    }

    reviews.sort_by(review_order);
    Ok(reviews)
}

/// One UTF-8 path and the committed blobs that differ there.
struct ChangedBlobs {
    path: String,
    before: Option<GitBlob>,
    after: Option<GitBlob>,
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
) -> Result<GitBlob> {
    let header = repo.find_header(object);
    let header = header.with_context(|| format!("failed to inspect the {side} blob for {path}"))?;
    if header.kind() != gix::objs::Kind::Blob {
        bail!("the {side} object for {path} is not a blob");
    }

    Ok(GitBlob {
        object,
        bytes: header.size(),
    })
}

/// Route a bounded committed pair through the same review boundary as worktree input.
fn review_change(
    repo: &gix::Repository,
    change: ChangedBlobs,
    limit: u64,
) -> Result<Option<ReviewItem>> {
    let before_bytes = change.before.as_ref().map(|blob| blob.bytes);
    let after_bytes = change.after.as_ref().map(|blob| blob.bytes);
    if before_bytes.is_some_and(|bytes| bytes > limit)
        || after_bytes.is_some_and(|bytes| bytes > limit)
    {
        let notice = FileNotice::too_large(change.path, before_bytes, after_bytes, limit);
        return Ok(Some(ReviewItem::Notice(notice)));
    }

    let mut before = Vec::new();
    if let Some(blob) = change.before {
        before = read_blob(repo, blob, &change.path, "previous")?;
    }
    let mut after = Vec::new();
    if let Some(blob) = change.after {
        after = read_blob(repo, blob, &change.path, "current")?;
    }
    let Some(before) = decode_text(before) else {
        return Ok(None);
    };
    let Some(after) = decode_text(after) else {
        return Ok(None);
    };
    if before == after {
        return Ok(None);
    }

    let before = before_bytes.map(|_| before.as_str());
    let after = after_bytes.map(|_| after.as_str());
    review_source_pair(&change.path, before, after)
}

/// Blob body whose length must still agree with its previously inspected header.
fn read_blob(repo: &gix::Repository, revision: GitBlob, path: &str, side: &str) -> Result<Vec<u8>> {
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

/// UI labels are UTF-8, so an undecodable Git path is outside this review.
fn decode_git_path(path: &BStr) -> Option<String> {
    path.to_str().ok().map(ToOwned::to_owned)
}

/// Source paths first, inspected generated files last.
fn review_order(left: &ReviewItem, right: &ReviewItem) -> Ordering {
    left.is_generated()
        .cmp(&right.is_generated())
        .then_with(|| left.path().cmp(right.path()))
}

#[cfg(test)]
mod tests;
