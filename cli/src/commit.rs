use crate::{
    input::{GitBlob, decode_text},
    review::{FileNotice, ReviewEntry, rank_reviews, review_source_pair},
};
use anyhow::{Context, Result, bail};
use gix::ObjectId;
use gix::bstr::{BStr, ByteSlice};
use gix::object::tree::diff::ChangeDetached;
use std::path::Path;

/// Produce bounded reviews introduced by one commit, using its first parent as the baseline.
pub fn diff_commit(directory: &Path, commitish: &Path, limit: u64) -> Result<Vec<ReviewEntry>> {
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

    // Keeping renames as additions and deletions, consistent with worktree review.
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
        let review = review_change(&repo, change, limit)?;
        let Some(review) = review else {
            continue;
        };
        reviews.push(review);
    }

    rank_reviews(&mut reviews, |_| 0);
    Ok(reviews)
}

/// Review a regular-file change, checking both blob sizes before loading either body.
fn review_change(
    repo: &gix::Repository,
    change: ChangeDetached,
    limit: u64,
) -> Result<Option<ReviewEntry>> {
    let (location, before_id, after_id) = match change {
        ChangeDetached::Rewrite { .. } => {
            bail!("Git reported a rewrite while rename detection was disabled")
        }
        ChangeDetached::Addition { entry_mode, .. } if !entry_mode.is_blob() => return Ok(None),
        ChangeDetached::Deletion { entry_mode, .. } if !entry_mode.is_blob() => return Ok(None),
        ChangeDetached::Modification {
            previous_entry_mode,
            entry_mode,
            ..
        } if !previous_entry_mode.is_blob() || !entry_mode.is_blob() => return Ok(None),
        ChangeDetached::Addition {
            location,
            entry_mode: _,
            id,
            ..
        } => (location, None, Some(id)),
        ChangeDetached::Deletion {
            location,
            entry_mode: _,
            id,
            ..
        } => (location, Some(id), None),
        ChangeDetached::Modification {
            location,
            previous_entry_mode: _,
            previous_id,
            entry_mode: _,
            id,
        } => (location, Some(previous_id), Some(id)),
    };
    let path = decode_git_path(location.as_bstr());
    let Some(path) = path else {
        return Ok(None);
    };
    let before = match before_id {
        None => None,
        Some(object) => {
            let blob = inspect_blob(repo, object, &path, "previous")?;
            Some(blob)
        }
    };
    let after = match after_id {
        None => None,
        Some(object) => {
            let blob = inspect_blob(repo, object, &path, "current")?;
            Some(blob)
        }
    };
    let before_bytes = before.as_ref().map(|blob| blob.bytes);
    let after_bytes = after.as_ref().map(|blob| blob.bytes);
    if before_bytes.is_some_and(|bytes| bytes > limit)
        || after_bytes.is_some_and(|bytes| bytes > limit)
    {
        return Ok(Some(ReviewEntry::Notice(FileNotice::TooLarge {
            path,
            before_bytes,
            after_bytes,
            limit_bytes: limit,
        })));
    }

    let before = read_revision(repo, before, &path, "previous")?;
    let before = match before {
        Revision::Binary => return Ok(None),
        Revision::Absent => None,
        Revision::Text(text) => Some(text),
    };
    let after = read_revision(repo, after, &path, "current")?;
    let after = match after {
        Revision::Binary => return Ok(None),
        Revision::Absent => None,
        Revision::Text(text) => Some(text),
    };
    if before.as_deref().unwrap_or_default() == after.as_deref().unwrap_or_default() {
        return Ok(None);
    }

    review_source_pair(&path, before.as_deref(), after.as_deref())
}

enum Revision {
    Absent,
    Binary,
    Text(String),
}

/// Decode an existing revision, keeping binary content distinct from an absent file.
fn read_revision(
    repo: &gix::Repository,
    blob: Option<GitBlob>,
    path: &str,
    side: &str,
) -> Result<Revision> {
    let Some(blob) = blob else {
        return Ok(Revision::Absent);
    };
    let source = blob.read(repo);
    let source = source.with_context(|| format!("failed to read the {side} blob for {path}"))?;
    Ok(match decode_text(source) {
        None => Revision::Binary,
        Some(text) => Revision::Text(text),
    })
}

/// Read the blob's declared size without allocating its contents.
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

/// Omit paths that cannot be displayed losslessly in the UTF-8 review ribbon.
fn decode_git_path(path: &BStr) -> Option<String> {
    path.to_str().ok().map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests;
