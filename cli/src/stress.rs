//! Corpus stress test: run the diff pipeline across a repository's history and
//! check that nothing it presents could mislead a reader.
//!
//! Every finding is a claim about the presented rows, not about taste: a row
//! whose text does not match the source, a change shown where both sides are
//! identical, a changed line never shown, a hunk with nothing in it. Readability
//! smells (meatgrinders, churn) are reported separately so they can be ranked.

mod guard;
mod oracle;
#[cfg(test)]
mod tests;
use crate::dump;
use crate::input::decode_text;
use anyhow::{Context, Result};
use clap::Parser;
use gix::bstr::ByteSlice;
use gix::object::tree::diff::ChangeDetached;
use mig_core::syntax::Side;
use mig_core::{FileDiff, Hunk, LineNumber, LineSpan, Row, SourceRow, diff_file};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "stress",
    about = "Run Mig across a repository's history and report presentation defects"
)]
struct Cli {
    /// Repository to walk, or captured-case directory with `--cases`.
    input: PathBuf,
    /// Revision to start from.
    #[arg(long, default_value = "HEAD")]
    rev: String,
    /// Number of first-parent commits to inspect.
    #[arg(long, default_value_t = 200)]
    commits: usize,
    /// First-parent commits to skip before inspecting.
    #[arg(long, default_value_t = 0)]
    skip: usize,
    /// Comma-separated extensions to include; all files by default.
    #[arg(long)]
    ext: Option<String>,
    /// Output directory for findings, reproduction cases, and the summary.
    #[arg(long, default_value = "stress-out")]
    out: PathBuf,
    #[arg(long, default_value_t = 8)]
    threads: usize,
    /// Abandon a single diff after this many seconds.
    #[arg(long, default_value_t = 30)]
    timeout_secs: u64,
    /// Per-revision byte limit.
    #[arg(long, default_value_t = 2 * 1024 * 1024)]
    max_bytes: u64,
    /// Reproduction cases to keep per finding kind.
    #[arg(long, default_value_t = 40)]
    keep: usize,
    /// Report diffs slower than this many milliseconds.
    #[arg(long, default_value_t = 2000)]
    slow_ms: u64,
    /// Label for this corpus in findings.
    #[arg(long)]
    label: Option<String>,
    /// Exercise only the core (mapping and islands) and its verifier.
    #[arg(long)]
    islands: bool,
    /// Replay captured cases from the input directory.
    #[arg(long)]
    cases: bool,
}

/// One changed file in one commit, with both revisions decoded.
struct FilePair {
    commit: String,
    path: String,
    before_oid: String,
    after_oid: String,
    before: String,
    after: String,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum Kind {
    /// The pipeline panicked.
    Panic,
    /// The pipeline returned an error.
    Error,
    /// The pipeline did not finish within the time limit.
    Timeout,
    /// The diff took longer than the slow threshold.
    Slow,
    /// Structural source accounting failed verification.
    Degraded,
    /// Revisions differ but no hunk was produced.
    NoHunks,
    /// A hunk shows no change at all.
    EmptyHunk,
    /// A replacement whose removed and added text are identical.
    SameReplacement,
    /// A line that exists in only one revision is never shown as changed.
    MissedChange,
    /// A row shown as unchanged context whose two sides differ.
    PhantomContext,
    /// Lines outside every hunk do not line up between revisions.
    ResidueMismatch,
    /// A row's spans do not reproduce the source line.
    RowTextMismatch,
    /// A line is shown as changed by more than one row or hunk.
    DuplicateClaim,
    /// A row's line number lies outside its hunk's coverage or the file.
    OutOfBounds,
    /// Rows within a hunk are not in source order.
    NonMonotonic,
    /// A moved or reflowed line's tokens differ between its two sides.
    MoveMismatch,
    /// A replacement where a removed line reappears verbatim among the added lines.
    Churn,
    /// Several replacements chained through weak context lines.
    Meatgrinder,
    /// The islands account failed verification against the text.
    IslandsInvalid,
}

impl Kind {
    /// Hard defects mislead; the rest are smells to rank, not failures.
    pub fn is_defect(self) -> bool {
        !matches!(self, Self::Slow | Self::Churn | Self::Meatgrinder)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Finding {
    pub kind: Kind,
    pub hunk: Option<usize>,
    pub detail: String,
}

/// What running one pair produced, however it ended.
enum Guarded<T> {
    Done(T),
    Panic(String),
    Error(String),
    Timeout,
}

struct Report {
    pair: FilePair,
    elapsed: Duration,
    result: Result<Inspection, Finding>,
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct BlockStats {
    additions: usize,
    deletions: usize,
    replacements: usize,
    max_chain: usize,
}

#[derive(Serialize, Deserialize)]
struct Inspection {
    findings: Vec<Finding>,
    evidence: Evidence,
}

#[derive(Serialize, Deserialize)]
enum Evidence {
    Document {
        dump: String,
        hunks: usize,
        blocks: BlockStats,
    },
    Islands {
        count: usize,
    },
}

pub fn run_stress() -> Result<()> {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    if arguments
        .get(1)
        .is_some_and(|argument| argument == "--worker")
    {
        return guard::worker();
    }
    let cli = Cli::parse();
    let label = cli.label.clone().unwrap_or_else(|| {
        cli.input
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".to_owned())
    });
    fs::create_dir_all(&cli.out)?;

    let extensions = cli.ext.as_ref().map(|list| {
        list.split(',')
            .map(|extension| extension.trim().to_ascii_lowercase())
            .filter(|extension| !extension.is_empty())
            .collect::<HashSet<_>>()
    });
    let (pair_sender, pair_receiver) = mpsc::sync_channel::<FilePair>(256);
    let pair_receiver = Arc::new(Mutex::new(pair_receiver));
    let (report_sender, report_receiver) = mpsc::channel::<Report>();
    let timeout = Duration::from_secs(cli.timeout_secs);
    let slow = Duration::from_millis(cli.slow_ms);
    let mode = if cli.islands {
        guard::Mode::Islands
    } else {
        guard::Mode::Document
    };
    let started = Instant::now();

    let mut aggregate = Aggregate::new(cli.keep, cli.out.clone(), label);
    std::thread::scope(|scope| -> Result<()> {
        let input_path = cli.input.clone();
        let rev = cli.rev.clone();
        let (skip, commits, max_bytes) = (cli.skip, cli.commits, cli.max_bytes);
        let extensions = &extensions;
        let replay = cli.cases;
        let producer = scope.spawn(move || -> Result<usize> {
            if replay {
                return produce_case_pairs(&input_path, &pair_sender);
            }
            produce_pairs(
                &input_path,
                &rev,
                skip,
                commits,
                max_bytes,
                extensions.as_ref(),
                &pair_sender,
            )
        });
        for _ in 0..cli.threads.max(1) {
            let pair_receiver = Arc::clone(&pair_receiver);
            let report_sender = report_sender.clone();
            scope.spawn(move || {
                loop {
                    let pair = {
                        let receiver = pair_receiver.lock().expect("pair queue lock");
                        receiver.recv()
                    };
                    let Ok(pair) = pair else {
                        break;
                    };
                    let report = inspect_pair(pair, timeout, slow, mode);
                    let sent = report_sender.send(report);
                    if sent.is_err() {
                        break;
                    }
                }
            });
        }
        drop(pair_receiver);
        drop(report_sender);

        for report in report_receiver {
            aggregate.absorb(report)?;
        }
        let commits = producer.join().expect("producer thread")?;
        aggregate.commits = commits;
        Ok(())
    })?;

    aggregate.finish(started.elapsed())
}

fn produce_pairs(
    repo_path: &std::path::Path,
    rev: &str,
    skip: usize,
    commits: usize,
    max_bytes: u64,
    extensions: Option<&HashSet<String>>,
    sender: &mpsc::SyncSender<FilePair>,
) -> Result<usize> {
    let repo = gix::discover(repo_path)
        .with_context(|| format!("failed to open repository {}", repo_path.display()))?;
    let start = repo
        .rev_parse_single(rev)
        .with_context(|| format!("failed to resolve {rev}"))?;
    let walk = repo
        .rev_walk([start.detach()])
        .first_parent_only()
        .all()
        .context("failed to start the revision walk")?;
    let mut inspected = 0;
    for info in walk.skip(skip).take(commits) {
        let info = info.context("revision walk failed")?;
        let commit = repo.find_commit(info.id)?;
        let after_tree = commit.tree()?;
        let before_tree = match info.parent_ids.first() {
            None => repo.empty_tree(),
            Some(parent) => repo.find_commit(*parent)?.tree()?,
        };
        let options = gix::diff::Options::default().with_rewrites(None);
        let changes = repo.diff_tree_to_tree(&before_tree, &after_tree, Some(options))?;
        inspected += 1;
        if inspected % 25 == 0 {
            eprintln!("[stress] {inspected} commits inspected");
        }
        for change in changes {
            let (location, before_id, after_id) = match change {
                ChangeDetached::Rewrite { .. } => continue,
                ChangeDetached::Addition { entry_mode, .. } if !entry_mode.is_blob() => continue,
                ChangeDetached::Deletion { entry_mode, .. } if !entry_mode.is_blob() => continue,
                ChangeDetached::Modification {
                    previous_entry_mode,
                    entry_mode,
                    ..
                } if !previous_entry_mode.is_blob() || !entry_mode.is_blob() => continue,
                ChangeDetached::Addition { location, id, .. } => (location, None, Some(id)),
                ChangeDetached::Deletion { location, id, .. } => (location, Some(id), None),
                ChangeDetached::Modification {
                    location,
                    previous_id,
                    id,
                    ..
                } => (location, Some(previous_id), Some(id)),
            };
            let Ok(path) = location.to_str() else {
                continue;
            };
            if let Some(extensions) = extensions {
                let extension = std::path::Path::new(path)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map(str::to_ascii_lowercase)
                    .unwrap_or_default();
                if !extensions.contains(&extension) {
                    continue;
                }
            }
            let Some(before) = load_text(&repo, before_id, max_bytes)? else {
                continue;
            };
            let Some(after) = load_text(&repo, after_id, max_bytes)? else {
                continue;
            };
            if before == after {
                continue;
            }
            let pair = FilePair {
                commit: info.id.to_string(),
                path: path.to_owned(),
                before_oid: before_id.map(|id| id.to_string()).unwrap_or_default(),
                after_oid: after_id.map(|id| id.to_string()).unwrap_or_default(),
                before,
                after,
            };
            let sent = sender.send(pair);
            if sent.is_err() {
                return Ok(inspected);
            }
        }
    }
    Ok(inspected)
}

/// Replay dumped cases: every directory holding a `meta.txt` next to `before.*` and `after.*`.
fn produce_case_pairs(
    root: &std::path::Path,
    sender: &mpsc::SyncSender<FilePair>,
) -> Result<usize> {
    let mut stack = vec![root.to_path_buf()];
    let mut cases = 0;
    while let Some(directory) = stack.pop() {
        let meta_path = directory.join("meta.txt");
        if meta_path.is_file() {
            let meta = fs::read_to_string(&meta_path)?;
            let field = |name: &str| {
                meta.lines()
                    .find_map(|line| line.strip_prefix(name).map(|value| value.trim().to_owned()))
                    .unwrap_or_default()
            };
            let mut before = None;
            let mut after = None;
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name == "before" || name.starts_with("before.") {
                    before = Some(fs::read_to_string(entry.path())?);
                } else if name == "after" || name.starts_with("after.") {
                    after = Some(fs::read_to_string(entry.path())?);
                }
            }
            let (Some(before), Some(after)) = (before, after) else {
                continue;
            };
            cases += 1;
            let pair = FilePair {
                commit: field("commit:"),
                path: field("path:"),
                before_oid: field("before:"),
                after_oid: field("after:"),
                before,
                after,
            };
            let sent = sender.send(pair);
            if sent.is_err() {
                return Ok(cases);
            }
            continue;
        }
        let mut children = fs::read_dir(&directory)?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        children.sort();
        stack.extend(children.into_iter().rev());
    }
    Ok(cases)
}

/// Load a blob as text; an absent revision is the empty string, binary or oversized is `None`.
fn load_text(
    repo: &gix::Repository,
    id: Option<gix::ObjectId>,
    max_bytes: u64,
) -> Result<Option<String>> {
    let Some(id) = id else {
        return Ok(Some(String::new()));
    };
    let header = repo.find_header(id)?;
    if header.kind() != gix::objs::Kind::Blob || header.size() > max_bytes {
        return Ok(None);
    }
    let mut blob = repo.find_blob(id)?;
    Ok(decode_text(std::mem::take(&mut blob.data)))
}

fn inspect_pair(pair: FilePair, timeout: Duration, slow: Duration, mode: guard::Mode) -> Report {
    let started = Instant::now();
    let outcome = guard::inspect(&pair, timeout, mode);
    let elapsed = started.elapsed();
    let mut result = match outcome {
        Guarded::Done(inspection) => Ok(inspection),
        Guarded::Timeout => Err(finding(
            Kind::Timeout,
            format!("terminated after {}s", timeout.as_secs()),
        )),
        Guarded::Panic(message) => Err(finding(Kind::Panic, message)),
        Guarded::Error(message) => Err(finding(Kind::Error, message)),
    };
    if elapsed > slow
        && let Ok(inspection) = &mut result
    {
        inspection.findings.push(slow_finding(&pair, elapsed));
    }
    Report {
        pair,
        elapsed,
        result,
    }
}

/// Inspect one pair on the worker process's ordinary main stack.
fn inspect_document(path: &str, before: &str, after: &str) -> Result<Inspection> {
    let presented = diff_file(path, before, after)?;
    let hunks = presented.hunks.len();
    let checked = check_presentation(path, before, after, &presented)?;
    let mut text = Vec::new();
    dump::print_diff(&mut text, &presented.into())?;
    Ok(Inspection {
        findings: checked.findings,
        evidence: Evidence::Document {
            dump: String::from_utf8(text)?,
            hunks,
            blocks: checked.blocks,
        },
    })
}

fn inspect_islands(path: &str, before: &str, after: &str) -> Result<Inspection> {
    let generated = mig_core::has_generated_marker(before) || mig_core::has_generated_marker(after);
    let syntax =
        mig_core::syntax::syntax_pair(std::path::Path::new(path), before, after, generated)?;
    let mapping = mig_core::matching::correspond(&syntax);
    let islands = mig_core::islands::islands(&syntax, &mapping);
    let findings = match mig_core::islands::verify(&syntax, &islands) {
        Err(invalid) => vec![finding(Kind::IslandsInvalid, invalid)],
        Ok(()) => Vec::new(),
    };
    Ok(Inspection {
        findings,
        evidence: Evidence::Islands {
            count: islands.islands().len(),
        },
    })
}

fn finding(kind: Kind, detail: String) -> Finding {
    Finding {
        kind,
        hunk: None,
        detail,
    }
}

fn slow_finding(pair: &FilePair, elapsed: Duration) -> Finding {
    finding(
        Kind::Slow,
        format!(
            "{} ms for {} + {} lines",
            elapsed.as_millis(),
            pair.before.lines().count(),
            pair.after.lines().count()
        ),
    )
}

/// Physical lines in the same geometry as `Source`: terminators belong to their line.
struct Lines<'text> {
    full: Vec<&'text str>,
    content: Vec<&'text str>,
}

impl<'text> Lines<'text> {
    fn new(text: &'text str) -> Self {
        let full = text.split_inclusive('\n').collect::<Vec<_>>();
        let content = full
            .iter()
            .map(|line| {
                line.strip_suffix("\r\n")
                    .or_else(|| line.strip_suffix('\n'))
                    .unwrap_or(line)
            })
            .collect();
        Self { full, content }
    }

    fn len(&self) -> usize {
        self.full.len()
    }

    fn content(&self, number: LineNumber) -> Option<&'text str> {
        self.content.get(number.index()).copied()
    }

    fn full(&self, number: LineNumber) -> Option<&'text str> {
        self.full.get(number.index()).copied()
    }
}

/// One value per revision.
#[derive(Default)]
struct Sides<T> {
    before: T,
    after: T,
}

impl<T> Sides<T> {
    fn get(&self, side: Side) -> &T {
        match side {
            Side::Before => &self.before,
            Side::After => &self.after,
        }
    }

    fn get_mut(&mut self, side: Side) -> &mut T {
        match side {
            Side::Before => &mut self.before,
            Side::After => &mut self.after,
        }
    }
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Before => "before",
        Side::After => "after",
    }
}

struct Checked {
    findings: Vec<Finding>,
    blocks: BlockStats,
}

/// A maximal run of removed and added rows, read as one replacement.
#[derive(Default)]
struct Block {
    removed: Vec<(LineNumber, String)>,
    added: Vec<(LineNumber, String)>,
    endings: usize,
}

impl Block {
    fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.added.is_empty() && self.endings == 0
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Run {
    Change,
    Weak,
    Strong,
    Break,
}

/// One file's presentation checked against its two texts.
struct Checker<'text> {
    lines: Sides<Lines<'text>>,
    oracle: oracle::Oracle<'text>,
    findings: Vec<Finding>,
    blocks: BlockStats,
    /// Lines shown as changed, with the hunk that showed them.
    changed: Sides<HashMap<LineNumber, usize>>,
    /// Lines whose terminator an ending row reports.
    ending: Sides<HashSet<LineNumber>>,
    /// Lines a row shows as unchanged context, by both coordinates.
    context: Vec<(LineNumber, LineNumber)>,
}

/// Progress through one hunk's rows.
struct Walk {
    hunk: usize,
    coverage: Sides<Option<LineSpan>>,
    /// Whether any row shows a change.
    signal: bool,
    last_after: Option<LineNumber>,
    block: Block,
    finished: Vec<Block>,
    runs: Vec<Run>,
}

impl Walk {
    /// Close the open replacement block, recording how it reads in the row sequence.
    fn flush(&mut self) {
        if self.block.is_empty() {
            return;
        }
        let block = std::mem::take(&mut self.block);
        let run = if !block.removed.is_empty() && !block.added.is_empty() {
            Run::Change
        } else if block.removed.len() + block.added.len() <= 2 {
            Run::Weak
        } else {
            Run::Break
        };
        self.runs.push(run);
        self.finished.push(block);
    }
}

fn check_presentation(
    path: &str,
    before: &str,
    after: &str,
    presented: &FileDiff,
) -> Result<Checked> {
    let oracle = oracle::Oracle::new(path, before, after)?;
    let mut checker = Checker {
        oracle,
        lines: Sides {
            before: Lines::new(before),
            after: Lines::new(after),
        },
        findings: Vec::new(),
        blocks: BlockStats::default(),
        changed: Sides::default(),
        ending: Sides::default(),
        context: Vec::new(),
    };
    if matches!(
        presented.comparison,
        mig_core::Comparison::Lines(mig_core::LineFallback::VerificationFailure(_))
    ) {
        checker.push(
            None,
            Kind::Degraded,
            "syntax account failed verification; plain lines shown".to_owned(),
        );
    }
    if presented.hunks.is_empty() {
        checker.push(
            None,
            Kind::NoHunks,
            "revisions differ but nothing is shown".to_owned(),
        );
    } else {
        for (index, hunk) in presented.hunks.iter().enumerate() {
            checker.hunk(index, hunk);
        }
        checker.terminators();
        checker.missed_changes();
        checker.residue();
    }
    Ok(Checked {
        findings: checker.findings,
        blocks: checker.blocks,
    })
}

impl Checker<'_> {
    fn push(&mut self, hunk: Option<usize>, kind: Kind, detail: String) {
        self.findings.push(Finding { kind, hunk, detail });
    }

    /// Check that a row reproduces its source line and lies inside the hunk.
    fn row(&mut self, walk: &Walk, side: Side, row: &SourceRow, what: &str) {
        let text = row.text();
        if let Some(actual) = self.lines.get(side).content(row.number)
            && actual != text
        {
            let detail = format!(
                "{what} row {} {} shows {text:?} but source has {actual:?}",
                side_name(side),
                row.number
            );
            self.push(Some(walk.hunk), Kind::RowTextMismatch, detail);
        }
        self.number(walk, side, row.number, what);
    }

    /// Check that a line number exists and lies inside the hunk's coverage.
    fn number(&mut self, walk: &Walk, side: Side, number: LineNumber, what: &str) {
        let lines = self.lines.get(side);
        if lines.content(number).is_none() {
            let detail = format!(
                "{what} row names {} line {number} of {}",
                side_name(side),
                lines.len()
            );
            self.push(Some(walk.hunk), Kind::OutOfBounds, detail);
        }
        if let Some(coverage) = walk.coverage.get(side)
            && !coverage.contains(&number)
        {
            let detail = format!(
                "{what} row {} {number} outside hunk coverage {coverage:?}",
                side_name(side)
            );
            self.push(Some(walk.hunk), Kind::OutOfBounds, detail);
        }
    }

    fn claim(&mut self, walk: &Walk, side: Side, number: LineNumber) {
        let previous = self.changed.get_mut(side).insert(number, walk.hunk);
        if let Some(previous) = previous {
            let detail = format!(
                "{} line {number} already shown as changed by hunk {}",
                side_name(side),
                previous + 1
            );
            self.push(Some(walk.hunk), Kind::DuplicateClaim, detail);
        }
    }

    fn monotonic(&mut self, walk: &mut Walk, number: LineNumber) {
        if let Some(previous) = walk.last_after
            && number <= previous
        {
            self.push(
                Some(walk.hunk),
                Kind::NonMonotonic,
                format!("after line {number} follows after line {previous}"),
            );
        }
        walk.last_after = Some(number);
    }

    /// A row that shows one line as unchanged on both sides.
    fn unchanged(&mut self, walk: &mut Walk, before: LineNumber, after: &SourceRow, what: &str) {
        self.row(walk, Side::After, after, what);
        self.number(walk, Side::Before, before, what);
        self.monotonic(walk, after.number);
        let before_text = self.lines.before.content(before);
        let after_text = self.lines.after.content(after.number);
        if before_text != after_text {
            let detail = format!(
                "{what} pairs before {before} with after {} but they differ: {:?} vs {:?}",
                after.number,
                before_text.unwrap_or(""),
                after_text.unwrap_or("")
            );
            self.push(Some(walk.hunk), Kind::PhantomContext, detail);
        }
        self.context.push((before, after.number));
    }

    fn same_tokens(&self, before: LineNumber, after: LineNumber) -> bool {
        self.oracle.preserved(&[before], &[after])
    }

    fn hunk(&mut self, index: usize, hunk: &Hunk) {
        let mut walk = Walk {
            hunk: index,
            coverage: Sides {
                before: hunk.excerpt.coverage.before.clone(),
                after: hunk.excerpt.coverage.after.clone(),
            },
            signal: false,
            last_after: None,
            block: Block::default(),
            finished: Vec::new(),
            runs: Vec::new(),
        };
        for row in &hunk.excerpt.rows {
            match row {
                Row::Context { before, after } => {
                    walk.flush();
                    self.unchanged(&mut walk, *before, after, "context row");
                    let weak = is_weak(self.lines.after.content(after.number).unwrap_or(""));
                    walk.runs.push(if weak { Run::Weak } else { Run::Strong });
                }
                Row::Crumb { before, after } => {
                    walk.flush();
                    self.unchanged(&mut walk, *before, after, "crumb");
                    walk.runs.push(Run::Break);
                }
                Row::Reflow { before, after } => {
                    walk.flush();
                    walk.signal = true;
                    for line in after {
                        self.row(&walk, Side::After, line, "reflow");
                        self.monotonic(&mut walk, line.number);
                        self.claim(&walk, Side::After, line.number);
                    }
                    for number in before {
                        self.number(&walk, Side::Before, *number, "reflow");
                        self.claim(&walk, Side::Before, *number);
                    }
                    let numbers = after.iter().map(|line| line.number).collect::<Vec<_>>();
                    if !self.oracle.preserved(before, &numbers) {
                        let numbers = after.iter().map(|line| line.number).collect::<Vec<_>>();
                        self.push(
                            Some(index),
                            Kind::MoveMismatch,
                            format!("reflow rows {before:?} -> {numbers:?} change tokens"),
                        );
                    }
                    walk.runs.push(Run::Strong);
                }
                Row::Removed(line) => {
                    walk.signal = true;
                    self.row(&walk, Side::Before, line, "removed");
                    if walk
                        .block
                        .removed
                        .last()
                        .is_some_and(|(previous, _)| *previous >= line.number)
                    {
                        self.push(
                            Some(index),
                            Kind::NonMonotonic,
                            format!("removed rows out of order at before line {}", line.number),
                        );
                    }
                    self.claim(&walk, Side::Before, line.number);
                    walk.block.removed.push((line.number, line.text()));
                }
                Row::Added(line) => {
                    walk.signal = true;
                    self.row(&walk, Side::After, line, "added");
                    self.monotonic(&mut walk, line.number);
                    self.claim(&walk, Side::After, line.number);
                    walk.block.added.push((line.number, line.text()));
                }
                Row::Changed { before, after } => {
                    walk.signal = true;
                    self.row(&walk, Side::Before, before, "changed");
                    self.row(&walk, Side::After, after, "changed");
                    self.monotonic(&mut walk, after.number);
                    self.claim(&walk, Side::Before, before.number);
                    self.claim(&walk, Side::After, after.number);
                    if before.text() == after.text() {
                        let detail = format!(
                            "changed row {}:{} has identical sides {:?}",
                            before.number,
                            after.number,
                            before.text()
                        );
                        self.push(Some(index), Kind::SameReplacement, detail);
                    }
                    walk.block.removed.push((before.number, before.text()));
                    walk.block.added.push((after.number, after.text()));
                }
                Row::Moved {
                    before,
                    after,
                    edge,
                } => {
                    walk.flush();
                    walk.signal = true;
                    self.row(&walk, Side::After, after, "moved");
                    self.number(&walk, Side::Before, *before, "moved");
                    self.monotonic(&mut walk, after.number);
                    self.claim(&walk, Side::Before, *before);
                    self.claim(&walk, Side::After, after.number);
                    if !self.same_tokens(*before, after.number) {
                        self.push(
                            Some(index),
                            Kind::MoveMismatch,
                            format!(
                                "moved row {before} -> {} ({edge:?}) differs in tokens",
                                after.number
                            ),
                        );
                    }
                    walk.runs.push(Run::Break);
                }
                Row::Ending(change) => {
                    let before = change.before();
                    let after = change.after();
                    walk.signal = true;
                    for (side, ending) in [(Side::Before, before), (Side::After, after)] {
                        if let Some(terminator) = ending {
                            self.number(&walk, side, terminator.line, "terminator");
                            let actual = self.lines.get(side).full(terminator.line).map(|text| {
                                if text.ends_with("\r\n") {
                                    mig_core::LineEnding::CrLf
                                } else if text.ends_with('\n') {
                                    mig_core::LineEnding::Lf
                                } else {
                                    mig_core::LineEnding::Missing
                                }
                            });
                            if actual != Some(terminator.ending) {
                                self.push(
                                    Some(index),
                                    Kind::RowTextMismatch,
                                    format!(
                                        "terminator for {side:?} line {} is {:?}, not {:?}",
                                        terminator.line, actual, terminator.ending
                                    ),
                                );
                            }
                            self.ending.get_mut(side).insert(terminator.line);
                        }
                    }
                    walk.block.endings += 1;
                }
                Row::Elision(elision) => {
                    walk.flush();
                    if let mig_core::Elision::Move { before, after } = elision {
                        walk.signal = true;
                        let before_lines = before.clone().into_iter().collect::<Vec<_>>();
                        let after_lines = after.clone().into_iter().collect::<Vec<_>>();
                        if before.len() != after.len()
                            || !self.oracle.preserved(&before_lines, &after_lines)
                        {
                            self.push(
                                Some(index),
                                Kind::MoveMismatch,
                                format!("elided move {before:?} -> {after:?} changes tokens"),
                            );
                        }
                        for line in before_lines {
                            self.number(&walk, Side::Before, line, "elided move");
                            self.claim(&walk, Side::Before, line);
                        }
                        for line in after_lines {
                            self.number(&walk, Side::After, line, "elided move");
                            self.claim(&walk, Side::After, line);
                        }
                    }
                    let range = elision.after();
                    if !range.is_empty() {
                        if let Some(previous) = walk.last_after
                            && range.start() <= previous
                        {
                            self.push(
                                Some(index),
                                Kind::NonMonotonic,
                                format!("elision of after {range:?} follows after line {previous}"),
                            );
                        }
                        walk.last_after = range.last();
                    }
                    let before_beyond = elision
                        .before()
                        .is_some_and(|range| range.end().index() > self.lines.before.len());
                    let after_beyond = range.end().index() > self.lines.after.len();
                    if before_beyond || after_beyond {
                        self.push(
                            Some(index),
                            Kind::OutOfBounds,
                            format!("elision {elision:?} exceeds the file"),
                        );
                    }
                    walk.runs.push(Run::Break);
                }
                Row::FileEnd => {
                    walk.flush();
                    walk.runs.push(Run::Break);
                }
            }
        }
        walk.flush();
        if !walk.signal {
            self.push(
                Some(index),
                Kind::EmptyHunk,
                format!("{} rows, none of them a change", hunk.excerpt.rows.len()),
            );
        }
        self.block_findings(index, &walk.finished);
        let chain = longest_change_chain(&walk.runs);
        self.blocks.max_chain = self.blocks.max_chain.max(chain);
        if chain >= 3 {
            let detail = format!(
                "{chain} replacement blocks chained through weak context ({} blocks in hunk)",
                walk.finished.len()
            );
            self.push(Some(index), Kind::Meatgrinder, detail);
        }
    }

    /// Count the hunk's blocks and report replacements that are identical or repeat lines verbatim.
    fn block_findings(&mut self, hunk: usize, blocks: &[Block]) {
        for block in blocks {
            match (block.removed.is_empty(), block.added.is_empty()) {
                (true, true) => {}
                (true, false) => self.blocks.additions += 1,
                (false, true) => self.blocks.deletions += 1,
                (false, false) => {
                    self.blocks.replacements += 1;
                    let text = |lines: &[(LineNumber, String)]| {
                        lines
                            .iter()
                            .map(|(_, text)| text.as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    };
                    let (removed, added) = (text(&block.removed), text(&block.added));
                    if removed == added {
                        let detail = format!(
                            "before {}..{} replaced by identical after {}..{}: {:?}",
                            block.removed[0].0,
                            block
                                .removed
                                .last()
                                .map_or(0, |(number, _)| number.next().get()),
                            block.added[0].0,
                            block
                                .added
                                .last()
                                .map_or(0, |(number, _)| number.next().get()),
                            removed.chars().take(80).collect::<String>()
                        );
                        self.push(Some(hunk), Kind::SameReplacement, detail);
                        continue;
                    }
                    let added_set = block
                        .added
                        .iter()
                        .map(|(_, text)| text.as_str())
                        .collect::<HashSet<_>>();
                    let churned = block
                        .removed
                        .iter()
                        .filter(|(_, text)| !is_weak(text) && added_set.contains(text.as_str()))
                        .map(|(number, text)| format!("{number}:{:?}", text.trim()))
                        .collect::<Vec<_>>();
                    if !churned.is_empty() {
                        let detail = format!(
                            "{} of {} removed lines reappear verbatim among {} added: {}",
                            churned.len(),
                            block.removed.len(),
                            block.added.len(),
                            churned.into_iter().take(3).collect::<Vec<_>>().join(", ")
                        );
                        self.push(Some(hunk), Kind::Churn, detail);
                    }
                }
            }
        }
    }

    /// Require explicit notices for changed context terminators and a changed final newline.
    fn terminators(&mut self) {
        let mut missing = Vec::new();
        for (before, after) in &self.context {
            if self.lines.before.full(*before) != self.lines.after.full(*after)
                && (!self.ending.before.contains(before) || !self.ending.after.contains(after))
            {
                missing.push(format!(
                    "context terminator change {before}:{after} has no notice"
                ));
            }
        }
        let unterminated =
            |lines: &Lines<'_>| lines.full.last().is_some_and(|line| !line.ends_with('\n'));
        if unterminated(&self.lines.before) != unterminated(&self.lines.after) {
            for side in [Side::Before, Side::After] {
                let lines = self.lines.get(side);
                if unterminated(lines) {
                    let last = LineNumber::from_index(lines.len() - 1);
                    if !self.ending.get(side).contains(&last) {
                        missing.push(format!(
                            "missing final newline on {side:?} line {last} has no notice"
                        ));
                    }
                }
            }
        }
        for detail in missing {
            self.push(None, Kind::MissedChange, detail);
        }
    }

    /// Lines whose text exists in only one revision must be shown as changed.
    fn missed_changes(&mut self) {
        for side in [Side::Before, Side::After] {
            let lines = self.lines.get(side);
            let other = self
                .lines
                .get(side.other())
                .full
                .iter()
                .copied()
                .collect::<HashSet<_>>();
            let revision = match side {
                Side::Before => "old",
                Side::After => "new",
            };
            let details = lines
                .full
                .iter()
                .enumerate()
                .map(|(index, line)| (LineNumber::from_index(index), *line))
                .filter(|(number, line)| {
                    !other.contains(line)
                        && !self.changed.get(side).contains_key(number)
                        && !self.ending.get(side).contains(number)
                })
                .take(5)
                .map(|(number, _)| {
                    let content = lines.content[number.index()].chars().take(80).collect::<String>();
                    format!("{} line {number} exists only in the {revision} revision but is never shown as changed: {content:?}", side_name(side))
                })
                .collect::<Vec<_>>();
            for detail in details {
                self.push(None, Kind::MissedChange, detail);
            }
        }
    }

    /// Every line is shown as changed, shown as context, vouched for by an elision, or outside
    /// every hunk. The lines outside must line up exactly between the revisions, and the shown
    /// context must be consistent with that alignment.
    fn residue(&mut self) {
        let covered = |side: Side, checker: &Self| {
            let mut covered = vec![false; checker.lines.get(side).len()];
            let changed = checker.changed.get(side).keys();
            let shown = checker.context.iter().map(|(before, after)| match side {
                Side::Before => before,
                Side::After => after,
            });
            for line in changed.chain(shown) {
                if let Some(slot) = covered.get_mut(line.index()) {
                    *slot = true;
                }
            }
            (0..checker.lines.get(side).len())
                .map(LineNumber::from_index)
                .filter(|line| !covered[line.index()])
                .collect::<Vec<_>>()
        };
        let before_residue = covered(Side::Before, self);
        let after_residue = covered(Side::After, self);
        let mut context = self.context.clone();
        context.sort_unstable();
        context.dedup();
        // Interleave uncovered lines and context pairs: the combined sequence must pair up in order.
        let mut implied = Vec::<(LineNumber, LineNumber)>::new();
        let (mut b, mut a, mut c) = (0, 0, 0);
        while b < before_residue.len() || a < after_residue.len() || c < context.len() {
            let next_before = before_residue.get(b).copied();
            let next_after = after_residue.get(a).copied();
            let take_context = context.get(c).is_some_and(|(before, after)| {
                next_before.is_none_or(|line| *before < line)
                    && next_after.is_none_or(|line| *after < line)
            });
            if take_context {
                c += 1;
                continue;
            }
            let (Some(before), Some(after)) = (next_before, next_after) else {
                let detail = format!(
                    "{} unshown before lines and {} unshown after lines cannot be paired with the shown context",
                    before_residue.len(),
                    after_residue.len()
                );
                self.push(None, Kind::ResidueMismatch, detail);
                return;
            };
            implied.push((before, after));
            b += 1;
            a += 1;
        }
        // Shown context was checked by content above; unshown lines must match terminator and all.
        for (before, after) in implied {
            if self.lines.before.full(before) != self.lines.after.full(after) {
                let detail = format!(
                    "unshown lines imply before {before} is after {after}, but they differ: {:?} vs {:?}",
                    self.lines.before.content(before).unwrap_or(""),
                    self.lines.after.content(after).unwrap_or("")
                );
                self.push(None, Kind::ResidueMismatch, detail);
                return;
            }
        }
    }
}

fn longest_change_chain(runs: &[Run]) -> usize {
    let mut best = 0;
    let mut chain = 0;
    let mut weak_gap = 0;
    let mut in_change = false;
    for run in runs {
        match run {
            Run::Change => {
                if !in_change {
                    chain = if chain == 0 || weak_gap <= 2 {
                        chain + 1
                    } else {
                        1
                    };
                    in_change = true;
                }
                weak_gap = 0;
            }
            Run::Weak => {
                in_change = false;
                weak_gap += 1;
            }
            Run::Strong | Run::Break => {
                best = best.max(chain);
                chain = 0;
                weak_gap = 0;
                in_change = false;
            }
        }
    }
    best.max(chain)
}

fn is_weak(text: &str) -> bool {
    let text = text.trim();
    text.len() <= 3
        || text
            .chars()
            .all(|character| character.is_ascii_punctuation() || character.is_whitespace())
}

struct Aggregate {
    keep: usize,
    out: PathBuf,
    label: String,
    findings_file: fs::File,
    kept: BTreeMap<Kind, usize>,
    counts: BTreeMap<Kind, usize>,
    files_with_defects: usize,
    pairs: usize,
    commits: usize,
    hunks: usize,
    blocks: BlockStats,
    chains: BTreeMap<usize, usize>,
    timings: Vec<(Duration, String)>,
}

impl Aggregate {
    fn new(keep: usize, out: PathBuf, label: String) -> Self {
        let findings_file =
            fs::File::create(out.join("findings.jsonl")).expect("create findings.jsonl");
        Self {
            keep,
            out,
            label,
            findings_file,
            kept: BTreeMap::new(),
            counts: BTreeMap::new(),
            files_with_defects: 0,
            pairs: 0,
            commits: 0,
            hunks: 0,
            blocks: BlockStats::default(),
            chains: BTreeMap::new(),
            timings: Vec::new(),
        }
    }

    fn absorb(&mut self, report: Report) -> Result<()> {
        self.pairs += 1;
        if let Ok(inspection) = &report.result {
            match &inspection.evidence {
                Evidence::Document { hunks, blocks, .. } => {
                    self.hunks += hunks;
                    self.blocks.additions += blocks.additions;
                    self.blocks.deletions += blocks.deletions;
                    self.blocks.replacements += blocks.replacements;
                    *self.chains.entry(blocks.max_chain).or_default() += 1;
                }
                Evidence::Islands { count } => self.hunks += count,
            }
        }
        let findings = match &report.result {
            Err(finding) => std::slice::from_ref(finding),
            Ok(inspection) => &inspection.findings,
        };
        self.timings.push((
            report.elapsed,
            format!(
                "{}@{}",
                report.pair.path,
                &report.pair.commit[..7.min(report.pair.commit.len())]
            ),
        ));
        if findings.iter().any(|finding| finding.kind.is_defect()) {
            self.files_with_defects += 1;
        }
        let mut kinds_to_dump = Vec::new();
        for finding in findings {
            *self.counts.entry(finding.kind).or_default() += 1;
            let record = serde_json::json!({
                "repo": self.label,
                "commit": report.pair.commit,
                "path": report.pair.path,
                "before": report.pair.before_oid,
                "after": report.pair.after_oid,
                "kind": finding.kind,
                "hunk": finding.hunk.map(|hunk| hunk + 1),
                "detail": finding.detail,
            });
            serde_json::to_writer(&mut self.findings_file, &record)?;
            self.findings_file.write_all(b"\n")?;
            let kept = self.kept.entry(finding.kind).or_default();
            if *kept < self.keep && !kinds_to_dump.contains(&finding.kind) {
                *kept += 1;
                kinds_to_dump.push(finding.kind);
            }
        }
        for kind in kinds_to_dump {
            self.dump_case(kind, &report)?;
        }
        Ok(())
    }

    fn dump_case(&self, kind: Kind, report: &Report) -> Result<()> {
        let file_name = std::path::Path::new(&report.pair.path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_owned());
        let extension = std::path::Path::new(&file_name)
            .extension()
            .map(|extension| format!(".{}", extension.to_string_lossy()))
            .unwrap_or_default();
        let directory = self
            .out
            .join("cases")
            .join(format!("{kind:?}"))
            .join(format!(
                "{}-{}-{}",
                self.kept.get(&kind).copied().unwrap_or(0),
                &report.pair.commit[..7.min(report.pair.commit.len())],
                file_name
            ));
        fs::create_dir_all(&directory)?;
        fs::write(
            directory.join(format!("before{extension}")),
            &report.pair.before,
        )?;
        fs::write(
            directory.join(format!("after{extension}")),
            &report.pair.after,
        )?;
        if let Ok(Inspection {
            evidence: Evidence::Document { dump, .. },
            ..
        }) = &report.result
        {
            fs::write(directory.join("mig.txt"), dump)?;
        }
        let findings = match &report.result {
            Err(finding) => std::slice::from_ref(finding),
            Ok(inspection) => &inspection.findings,
        };
        let mut meta = format!(
            "repo: {}\ncommit: {}\npath: {}\nbefore: {}\nafter: {}\nelapsed_ms: {}\n\nfindings:\n",
            self.label,
            report.pair.commit,
            report.pair.path,
            report.pair.before_oid,
            report.pair.after_oid,
            report.elapsed.as_millis()
        );
        for finding in findings {
            meta.push_str(&format!(
                "- {:?}{}: {}\n",
                finding.kind,
                finding
                    .hunk
                    .map(|hunk| format!(" (hunk {})", hunk + 1))
                    .unwrap_or_default(),
                finding.detail
            ));
        }
        fs::write(directory.join("meta.txt"), meta)?;
        Ok(())
    }

    fn finish(mut self, elapsed: Duration) -> Result<()> {
        self.timings
            .sort_by_key(|(time, _)| std::cmp::Reverse(*time));
        let percentile = |fraction: f64| -> u128 {
            if self.timings.is_empty() {
                return 0;
            }
            let index = ((1.0 - fraction) * (self.timings.len() - 1) as f64).round() as usize;
            self.timings[index.min(self.timings.len() - 1)]
                .0
                .as_millis()
        };
        let mut summary = String::new();
        summary.push_str(&format!(
            "corpus {}: {} commits, {} file pairs, {} hunks in {:.1}s\n",
            self.label,
            self.commits,
            self.pairs,
            self.hunks,
            elapsed.as_secs_f64()
        ));
        summary.push_str(&format!(
            "blocks: {} additions, {} deletions, {} replacements\n",
            self.blocks.additions, self.blocks.deletions, self.blocks.replacements
        ));
        summary.push_str(&format!(
            "timing ms: p50 {} p90 {} p99 {} max {}\n",
            percentile(0.5),
            percentile(0.9),
            percentile(0.99),
            self.timings.first().map_or(0, |(time, _)| time.as_millis())
        ));
        summary.push_str(&format!(
            "files with defects: {} of {}\n",
            self.files_with_defects, self.pairs
        ));
        summary.push_str("findings by kind:\n");
        for (kind, count) in &self.counts {
            summary.push_str(&format!(
                "  {:<18} {:>7}{}\n",
                format!("{kind:?}"),
                count,
                if kind.is_defect() { "" } else { "  (smell)" }
            ));
        }
        summary.push_str("longest replacement chain per file (chain -> files):\n");
        for (chain, files) in &self.chains {
            summary.push_str(&format!("  {chain:>3} -> {files}\n"));
        }
        summary.push_str("slowest:\n");
        for (time, name) in self.timings.iter().take(10) {
            summary.push_str(&format!("  {:>7} ms  {name}\n", time.as_millis()));
        }
        print!("{summary}");
        fs::write(self.out.join("summary.txt"), summary)?;
        anyhow::ensure!(
            self.files_with_defects == 0,
            "{} file pairs contain defects",
            self.files_with_defects
        );
        Ok(())
    }
}
