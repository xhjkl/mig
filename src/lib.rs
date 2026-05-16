pub mod diff_engine;
pub mod diff_model;

use anyhow::{Context, Result, bail};
use ignore::WalkBuilder;
use notify::{Config, Event, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use similar::{ChangeTag, TextDiff};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_MAX_TEXT_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
    max_text_bytes: u64,
}

impl Workspace {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        let root = fs::canonicalize(root)
            .with_context(|| format!("failed to resolve workspace root {}", root.display()))?;
        let metadata = fs::metadata(&root)
            .with_context(|| format!("failed to read workspace root {}", root.display()))?;
        if !metadata.is_dir() {
            bail!("workspace root must be a directory: {}", root.display());
        }

        Ok(Self {
            root,
            max_text_bytes: DEFAULT_MAX_TEXT_BYTES,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn snapshot(&self) -> Result<WorkspaceSnapshot> {
        WorkspaceSnapshot::capture(&self.root, self.max_text_bytes)
    }

    pub fn git_state(&self) -> Result<Option<GitState>> {
        GitState::load(&self.root)
    }
}

#[derive(Clone, Debug, Default)]
pub struct WorkspaceSnapshot {
    files: BTreeMap<PathBuf, FileSnapshot>,
}

impl WorkspaceSnapshot {
    fn capture(root: &Path, max_text_bytes: u64) -> Result<Self> {
        let mut files = BTreeMap::new();
        let root = root.to_path_buf();
        let walker = WalkBuilder::new(&root)
            .hidden(false)
            .filter_entry(|entry| {
                let name = entry.file_name();
                name != OsStr::new(".git")
                    && name != OsStr::new(".jj")
                    && name != OsStr::new("target")
            })
            .build();

        for entry in walker {
            let entry = entry?;
            let Some(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() {
                continue;
            }

            let path = entry.path();
            let relative = path
                .strip_prefix(&root)
                .with_context(|| format!("failed to relativize {}", path.display()))?
                .to_path_buf();
            let state = FileSnapshot::read(path, max_text_bytes)
                .with_context(|| format!("failed to read {}", path.display()))?;
            files.insert(relative, state);
        }

        Ok(Self { files })
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.files.keys().map(PathBuf::as_path)
    }

    pub fn diff(&self, next: &Self) -> WorkspaceDiff {
        let mut files = Vec::new();
        let mut old = self.files.iter().peekable();
        let mut new = next.files.iter().peekable();

        loop {
            match (old.peek(), new.peek()) {
                (Some((old_path, old_state)), Some((new_path, new_state))) => {
                    if old_path == new_path {
                        if old_state.fingerprint != new_state.fingerprint {
                            files.push(FileDiff {
                                path: (*old_path).clone(),
                                kind: FileDiffKind::Modified,
                                before: Some((*old_state).clone()),
                                after: Some((*new_state).clone()),
                            });
                        }
                        old.next();
                        new.next();
                    } else if old_path < new_path {
                        files.push(FileDiff {
                            path: (*old_path).clone(),
                            kind: FileDiffKind::Deleted,
                            before: Some((*old_state).clone()),
                            after: None,
                        });
                        old.next();
                    } else {
                        files.push(FileDiff {
                            path: (*new_path).clone(),
                            kind: FileDiffKind::Added,
                            before: None,
                            after: Some((*new_state).clone()),
                        });
                        new.next();
                    }
                }
                (Some((old_path, old_state)), None) => {
                    files.push(FileDiff {
                        path: (*old_path).clone(),
                        kind: FileDiffKind::Deleted,
                        before: Some((*old_state).clone()),
                        after: None,
                    });
                    old.next();
                }
                (None, Some((new_path, new_state))) => {
                    files.push(FileDiff {
                        path: (*new_path).clone(),
                        kind: FileDiffKind::Added,
                        before: None,
                        after: Some((*new_state).clone()),
                    });
                    new.next();
                }
                (None, None) => break,
            }
        }

        WorkspaceDiff { files }
    }
}

#[derive(Clone, Debug)]
pub struct FileSnapshot {
    bytes: u64,
    fingerprint: u64,
    text: Option<String>,
}

impl FileSnapshot {
    fn read(path: &Path, max_text_bytes: u64) -> Result<Self> {
        let bytes = fs::read(path)?;
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        let fingerprint = hasher.finish();
        let len = bytes.len() as u64;
        let text = if len <= max_text_bytes {
            String::from_utf8(bytes).ok()
        } else {
            None
        };

        Ok(Self {
            bytes: len,
            fingerprint,
            text,
        })
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }
}

#[derive(Clone, Debug, Default)]
pub struct WorkspaceDiff {
    files: Vec<FileDiff>,
}

impl WorkspaceDiff {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn files(&self) -> &[FileDiff] {
        &self.files
    }

    pub fn stats(&self) -> WorkspaceDiffStats {
        let mut stats = WorkspaceDiffStats::default();
        for file in &self.files {
            match file.kind {
                FileDiffKind::Added => stats.added += 1,
                FileDiffKind::Deleted => stats.deleted += 1,
                FileDiffKind::Modified => stats.modified += 1,
            }
        }
        stats
    }
}

#[derive(Clone, Debug, Default)]
pub struct WorkspaceDiffStats {
    pub added: usize,
    pub modified: usize,
    pub deleted: usize,
}

#[derive(Clone, Debug)]
pub struct FileDiff {
    path: PathBuf,
    kind: FileDiffKind,
    before: Option<FileSnapshot>,
    after: Option<FileSnapshot>,
}

impl FileDiff {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn kind(&self) -> FileDiffKind {
        self.kind
    }

    pub fn before(&self) -> Option<&FileSnapshot> {
        self.before.as_ref()
    }

    pub fn after(&self) -> Option<&FileSnapshot> {
        self.after.as_ref()
    }

    pub fn render_unified(&self) -> String {
        match (&self.before, &self.after) {
            (Some(before), Some(after)) => render_modified_unified(&self.path, before, after),
            (None, Some(after)) => render_added_unified(&self.path, after),
            (Some(before), None) => render_deleted_unified(&self.path, before),
            (None, None) => String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileDiffKind {
    Added,
    Modified,
    Deleted,
}

impl FileDiffKind {
    pub fn marker(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
        }
    }
}

#[derive(Clone, Debug)]
pub struct GitState {
    pub root: PathBuf,
    pub status: Vec<String>,
    pub diff_stat: Vec<String>,
}

impl GitState {
    fn load(path: &Path) -> Result<Option<Self>> {
        let root = run_git(path, ["rev-parse", "--show-toplevel"])?;
        let Some(root) = root else {
            return Ok(None);
        };
        let root = PathBuf::from(root.trim());
        let status = run_git_lines(&root, ["status", "--short"])?;
        let diff_stat = run_git_lines(&root, ["diff", "--stat", "--no-color", "HEAD", "--"])?;

        Ok(Some(Self {
            root,
            status,
            diff_stat,
        }))
    }

    pub fn is_clean(&self) -> bool {
        self.status.is_empty()
    }
}

#[derive(Clone, Copy, Debug)]
pub enum WatchBackend {
    Native,
    Poll { interval: Duration },
}

pub fn watch(
    workspace: Workspace,
    backend: WatchBackend,
    debounce: Duration,
    mut on_turn: impl FnMut(FilesystemTurn) -> Result<()>,
) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let mut watcher = ActiveWatcher::new(backend, tx)?;
    watcher.watch(workspace.root(), RecursiveMode::Recursive)?;

    let mut previous = workspace.snapshot()?;
    let mut index = 0;

    loop {
        let events = recv_event_batch(&rx, debounce)?;
        let next = workspace.snapshot()?;
        let diff = previous.diff(&next);
        previous = next;

        if diff.is_empty() {
            continue;
        }

        index += 1;
        let git = workspace.git_state()?;
        on_turn(FilesystemTurn {
            index,
            occurred_at: now_seconds(),
            event_count: events.len(),
            diff,
            git,
        })?;
    }
}

enum ActiveWatcher {
    Native(RecommendedWatcher),
    Poll(PollWatcher),
}

impl ActiveWatcher {
    fn new(backend: WatchBackend, tx: mpsc::Sender<notify::Result<Event>>) -> Result<Self> {
        match backend {
            WatchBackend::Native => Ok(Self::Native(notify::recommended_watcher(move |event| {
                let _ = tx.send(event);
            })?)),
            WatchBackend::Poll { interval } => Ok(Self::Poll(PollWatcher::new(
                move |event| {
                    let _ = tx.send(event);
                },
                Config::default()
                    .with_poll_interval(interval)
                    .with_compare_contents(true),
            )?)),
        }
    }

    fn watch(&mut self, path: &Path, recursive_mode: RecursiveMode) -> notify::Result<()> {
        match self {
            Self::Native(watcher) => watcher.watch(path, recursive_mode),
            Self::Poll(watcher) => watcher.watch(path, recursive_mode),
        }
    }
}

#[derive(Clone, Debug)]
pub struct FilesystemTurn {
    pub index: u64,
    pub occurred_at: u64,
    pub event_count: usize,
    pub diff: WorkspaceDiff,
    pub git: Option<GitState>,
}

fn recv_event_batch(
    rx: &mpsc::Receiver<notify::Result<Event>>,
    debounce: Duration,
) -> Result<Vec<Event>> {
    let event = rx.recv().context("filesystem watcher stopped")?;
    let mut events = vec![event?];

    loop {
        match rx.recv_timeout(debounce) {
            Ok(event) => events.push(event?),
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => bail!("filesystem watcher stopped"),
        }
    }

    Ok(events)
}

fn render_modified_unified(path: &Path, before: &FileSnapshot, after: &FileSnapshot) -> String {
    let (Some(before), Some(after)) = (&before.text, &after.text) else {
        return format!(
            "--- {}\n+++ {}\n(binary or large file changed: {} -> {} bytes)\n",
            path.display(),
            path.display(),
            before.bytes,
            after.bytes
        );
    };

    let mut output = format!("--- {}\n+++ {}\n", path.display(), path.display());
    let diff = TextDiff::from_lines(before, after);
    for group in diff.grouped_ops(3) {
        output.push_str("@@\n");
        for op in group {
            for change in diff.iter_changes(&op) {
                let marker = match change.tag() {
                    ChangeTag::Delete => "-",
                    ChangeTag::Insert => "+",
                    ChangeTag::Equal => " ",
                };
                output.push_str(marker);
                output.push_str(change.value());
                if !change.value().ends_with('\n') {
                    output.push('\n');
                }
            }
        }
    }
    output
}

fn render_added_unified(path: &Path, after: &FileSnapshot) -> String {
    let Some(after) = &after.text else {
        return format!(
            "--- /dev/null\n+++ {}\n(binary or large file added: {} bytes)\n",
            path.display(),
            after.bytes
        );
    };

    let mut output = format!("--- /dev/null\n+++ {}\n", path.display());
    for line in after.lines() {
        output.push('+');
        output.push_str(line);
        output.push('\n');
    }
    output
}

fn render_deleted_unified(path: &Path, before: &FileSnapshot) -> String {
    let Some(before) = &before.text else {
        return format!(
            "--- {}\n+++ /dev/null\n(binary or large file deleted: {} bytes)\n",
            path.display(),
            before.bytes
        );
    };

    let mut output = format!("--- {}\n+++ /dev/null\n", path.display());
    for line in before.lines() {
        output.push('-');
        output.push_str(line);
        output.push('\n');
    }
    output
}

fn run_git<const N: usize>(path: &Path, args: [&str; N]) -> Result<Option<String>> {
    let output = Command::new("git").arg("-C").arg(path).args(args).output();
    let output = match output {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to execute git"),
    };

    if !output.status.success() {
        return Ok(None);
    }

    Ok(Some(String::from_utf8_lossy(&output.stdout).to_string()))
}

fn run_git_lines<const N: usize>(path: &Path, args: [&str; N]) -> Result<Vec<String>> {
    let output = run_git(path, args)?;
    Ok(output
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect())
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn snapshot_diff_detects_added_modified_and_deleted_files() -> Result<()> {
        let root = temp_root("diff")?;
        fs::write(root.join("a.txt"), "one\n")?;

        let workspace = Workspace::open(&root)?;
        let first = workspace.snapshot()?;

        fs::write(root.join("a.txt"), "one\ntwo\n")?;
        fs::write(root.join("b.txt"), "new\n")?;
        let second = workspace.snapshot()?;

        let diff = first.diff(&second);
        let stats = diff.stats();
        assert_eq!(stats.added, 1);
        assert_eq!(stats.modified, 1);
        assert_eq!(stats.deleted, 0);

        fs::remove_file(root.join("a.txt"))?;
        let third = workspace.snapshot()?;
        let diff = second.diff(&third);
        let stats = diff.stats();
        assert_eq!(stats.added, 0);
        assert_eq!(stats.modified, 0);
        assert_eq!(stats.deleted, 1);

        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn snapshot_skips_repository_and_build_internals() -> Result<()> {
        let root = temp_root("ignored")?;
        fs::create_dir(root.join(".git"))?;
        fs::create_dir(root.join("target"))?;
        fs::write(root.join(".git").join("index"), "git data")?;
        fs::write(root.join("target").join("artifact"), "build data")?;
        fs::write(root.join("visible.txt"), "visible\n")?;

        let workspace = Workspace::open(&root)?;
        let snapshot = workspace.snapshot()?;
        let paths = snapshot.paths().collect::<Vec<_>>();
        assert_eq!(paths, [Path::new("visible.txt")]);

        fs::remove_dir_all(root)?;
        Ok(())
    }

    fn temp_root(label: &str) -> Result<PathBuf> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let root = env::temp_dir().join(format!("mig-{label}-{nonce}"));
        fs::create_dir(&root)?;
        Ok(root)
    }
}
