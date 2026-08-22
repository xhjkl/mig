use crate::input::{BoundedBytes, GitBlob, OpenFile};
use anyhow::{Context, Result, bail};
use gix::ObjectId;
use gix::bstr::{BStr, BString, ByteSlice};
use mig::diff::diff_file;
use mig::review::{
    FileNotice, FileReview, MAX_REVISION_BYTES, MAX_REVISION_LINES, revision_line_count,
};
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// One side of a status candidate before its contents are decoded as text.
enum Revision<T> {
    Absent,
    Present(T),
    Unsupported,
}

/// Git provenance retained until the review ribbon is ordered.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum GitChange {
    Dirty,
    Staged,
    Untracked,
}

/// How strongly a file rises toward the front of the review ribbon.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RibbonBuoyancy(u8);

/// Planned text reviews with dirty files most buoyant and generated files least.
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
                        change,
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
            reviews.push((change, complexity_review(&path, before_lines, after_lines)));
            continue;
        }

        let label = path.to_string_lossy();
        let diff = diff_file(&label, &before, &after)?;
        reviews.push((change, FileReview::from(diff)));
    }

    reviews.sort_by(|(left_change, left), (right_change, right)| {
        Reverse(ribbon_buoyancy(*left_change, left))
            .cmp(&Reverse(ribbon_buoyancy(*right_change, right)))
            .then_with(|| left.path().cmp(right.path()))
    });
    Ok(reviews.into_iter().map(|(_, review)| review).collect())
}

fn ribbon_buoyancy(change: GitChange, review: &FileReview) -> RibbonBuoyancy {
    if review.is_generated() {
        return RibbonBuoyancy(0);
    }

    let buoyancy = match change {
        GitChange::Dirty => 3,
        GitChange::Staged => 2,
        GitChange::Untracked => 1,
    };
    RibbonBuoyancy(buoyancy)
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
            .and_modify(|existing| *existing = (*existing).min(change))
            .or_insert(change);
    }

    let mut paths = paths
        .into_iter()
        .map(|(path, change)| (change, path))
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

/// Strongest visible status for one emitted tree/index/worktree fact.
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

fn read_head_blob(repo: &gix::Repository, blob: GitBlob) -> Result<Vec<u8>> {
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

/// UTF-8 source without NUL bytes; other content is outside the terminal text review.
fn decode_text(source: Vec<u8>) -> Option<String> {
    if source.contains(&0) {
        return None;
    }

    String::from_utf8(source).ok()
}

#[cfg(test)]
mod tests;
