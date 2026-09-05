use crate::{
    input::{BoundedBytes, GitBlob, OpenFile, decode_text},
    review::{FileNotice, ReviewEntry, review_source_pair},
};
use anyhow::{Context, Result};
use gix::ObjectId;
use gix::bstr::{BStr, BString, ByteSlice};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// A revision eligible for text review, absent, or excluded by its file type.
enum Revision<T> {
    Absent,
    Present(T),
    Unsupported,
}

/// Review priority in ascending order; a file with several statuses keeps the highest.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum GitChange {
    Untracked,
    Staged,
    Dirty,
}

/// Review worktree files against HEAD, with dirty files first and generated files last.
pub fn diff_directory(directory: &Path, limit: u64) -> Result<Vec<ReviewEntry>> {
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

    for (change, path) in changes {
        let before = head_revision(&repo, &head_tree, &path)?;
        let before = match before {
            Revision::Absent => None,
            Revision::Present(before) => Some(before),
            Revision::Unsupported => continue,
        };
        let after = worktree_revision(&root, &path)?;
        let after = match after {
            Revision::Absent => None,
            Revision::Present(after) => Some(after),
            Revision::Unsupported => continue,
        };

        let before_bytes = before.as_ref().map(|blob| blob.bytes);
        let after_bytes = after.as_ref().map(OpenFile::bytes);
        if before_bytes.is_some_and(|bytes| bytes > limit)
            || after_bytes.is_some_and(|bytes| bytes > limit)
        {
            reviews.push((
                change,
                ReviewEntry::Notice(FileNotice::TooLarge {
                    path: path.to_string_lossy().into_owned(),
                    before_bytes,
                    after_bytes,
                    limit_bytes: limit,
                }),
            ));
            continue;
        }

        // Reading the mutable side first; growth past the limit also spares the HEAD allocation.
        let after = match after {
            None => None,
            Some(after) => {
                let after = after.read(limit)?;
                match after {
                    BoundedBytes::TooLarge(bytes) => {
                        reviews.push((
                            change,
                            ReviewEntry::Notice(FileNotice::TooLarge {
                                path: path.to_string_lossy().into_owned(),
                                before_bytes,
                                after_bytes: Some(bytes),
                                limit_bytes: limit,
                            }),
                        ));
                        continue;
                    }
                    BoundedBytes::Contents(after) => {
                        let Some(after) = decode_text(after) else {
                            continue;
                        };
                        Some(after)
                    }
                }
            }
        };
        let before = match before {
            None => None,
            Some(before) => {
                let before = before.read(&repo);
                let before = before.context("failed to read a pinned HEAD blob")?;
                let Some(before) = decode_text(before) else {
                    continue;
                };
                Some(before)
            }
        };
        if before.as_deref().unwrap_or_default() == after.as_deref().unwrap_or_default() {
            continue;
        }
        let label = path.to_string_lossy();
        let review = review_source_pair(&label, before.as_deref(), after.as_deref())?;
        let Some(review) = review else {
            continue;
        };
        reviews.push((change, review));
    }

    reviews.sort_by(|(left_change, left), (right_change, right)| {
        left.is_generated()
            .cmp(&right.is_generated())
            .then_with(|| {
                if left.is_generated() {
                    Ordering::Equal
                } else {
                    right_change.cmp(left_change)
                }
            })
            .then_with(|| left.path().cmp(right.path()))
    });
    Ok(reviews.into_iter().map(|(_, review)| review).collect())
}

/// Collect changed paths within the directory, keeping each path's highest review priority.
fn changed_paths(
    repo: &gix::Repository,
    directory: &Path,
    head_tree_id: ObjectId,
) -> Result<Vec<(GitChange, PathBuf)>> {
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
    let mut paths: BTreeMap<PathBuf, GitChange> = BTreeMap::new();

    for item in items {
        let item = item.context("failed while reading Git status")?;
        let Some(change) = git_change(&item) else {
            continue;
        };
        let Some(path) = decode_git_path(item.location()) else {
            continue;
        };
        paths
            .entry(path)
            .and_modify(|existing| *existing = (*existing).max(change))
            .or_insert(change);
    }

    let mut paths = paths
        .into_iter()
        .map(|(path, change)| (change, path))
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

fn git_change(item: &gix::status::Item) -> Option<GitChange> {
    match item {
        gix::status::Item::TreeIndex(_) => Some(GitChange::Staged),
        gix::status::Item::IndexWorktree(change) => match change {
            gix::status::index_worktree::Item::DirectoryContents { entry, .. }
                if matches!(entry.status, gix::dir::entry::Status::Untracked) =>
            {
                Some(GitChange::Untracked)
            }
            // `NeedsUpdate` is bookkeeping, not an unstaged content change.
            _ if change.summary().is_some() => Some(GitChange::Dirty),
            _ => None,
        },
    }
}

/// Scope status to a literal directory path, resolved from the repository root.
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

/// Omit paths that cannot be displayed losslessly in the UTF-8 review ribbon.
fn decode_git_path(path: &BStr) -> Option<PathBuf> {
    let path = path.to_str().ok()?;
    Some(PathBuf::from(path))
}

fn head_revision(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
    path: &Path,
) -> Result<Revision<GitBlob>> {
    let entry = tree
        .lookup_entry_by_path(path)
        .with_context(|| format!("failed to look up {} in HEAD", path.display()))?;
    let Some(entry) = entry else {
        return Ok(Revision::Absent);
    };
    if !entry.mode().is_blob() {
        return Ok(Revision::Unsupported);
    }

    let object = entry.object_id();
    let header = repo
        .find_header(object)
        .with_context(|| format!("failed to inspect the HEAD blob for {}", path.display()))?;
    if header.kind() != gix::objs::Kind::Blob {
        return Ok(Revision::Unsupported);
    }

    Ok(Revision::Present(GitBlob {
        object,
        bytes: header.size(),
    }))
}

fn worktree_revision(root: &Path, path: &Path) -> Result<Revision<OpenFile>> {
    let full_path = root.join(path);
    let metadata = fs::symlink_metadata(&full_path);
    if metadata
        .as_ref()
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(Revision::Absent);
    }
    let metadata =
        metadata.with_context(|| format!("failed to inspect file {}", full_path.display()))?;
    if !metadata.file_type().is_file() {
        return Ok(Revision::Unsupported);
    }

    let source = OpenFile::open(&full_path)?;
    Ok(Revision::Present(source))
}

#[cfg(test)]
mod tests;
