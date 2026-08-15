# Design

Mig turns two source revisions into a bounded unified review. Rust has a
structural frontend; unsupported syntax and generated files use a literal line
diff.

## Pipeline

```text
Git scan or explicit file pair
        │
        ▼
diff_file(path, before, after)
        ├── Rust structural planner
        └── plain line planner
        │
        ▼
FileReview
        ├── FileDiff → Hunk → DiffRow
        └── FileNotice
        │
        ▼
terminal UI
```

The core produces presentation-ready rows. The UI owns layout, clipping, color,
and navigation; it does not infer correspondence or edits.

## Repository backend

Mig uses `gix` directly and does not invoke the Git executable at runtime. The
status pass unions HEAD→index, index→worktree, and nonignored untracked
candidates, with rename detection and submodule worktree inspection disabled.
Candidate order is undefined by the parallel backend, so Mig restores lexical
repository-relative order before planning reviews.

Status is deliberately only a candidate generator. Mig pins the HEAD tree once,
uses that same tree for status and baseline object lookup, then compares its
blob bytes directly with the current regular file. This final comparison removes
staged changes that were subsequently restored to HEAD and never substitutes
index content for the before-world. An unborn HEAD uses Git's empty tree.

`gix` was selected over `git2` after equivalent status prototypes. `git2` had a
smaller API and dependency graph, but bundled a native C backend and rejected
SHA-256 repositories. `gix` was faster in the representative repositories,
keeps `cargo install` pure Rust, supports SHA-1 and SHA-256, and exposes object
headers so the size guard runs before blob materialization. Its accepted costs
are a larger compile graph and a pre-1.0 API.

## Input and dispatch

With no arguments, `m` reviews the net `HEAD`→working-tree state beneath the
current directory, including deletions and nonignored untracked paths.
NUL-containing and non-UTF-8 files are skipped. Current worktree inputs must be
regular files; links and special entries are left to Git's ordinary diff tools.
Standard ignores include per-directory `.gitignore`, `.git/info/exclude`, and
the configured global excludes file; tracked paths remain reviewable even when
an ignore rule later matches them. Renames appear as one deletion and one
addition.

Each source revision is capped at 16 MiB before it is loaded or parsed. The
worktree reader checks the opened file and remains bounded if that file grows;
committed blobs are sized against a pinned HEAD revision. A path over the limit
stays in the review as a navigable notice naming both observed sizes instead of
being silently skipped. Explicit file pairs use the same limit.

A lowercase `.rs` extension selects structural Rust diffing. Other paths use
the plain planner.

A standalone, case-sensitive `@generated` token on a comment/marker line within
the first 20 lines of either revision forces the plain planner. Generated files
remain visible, are tagged in the UI, and follow authored files in directory
reviews.

## Review model

```text
FileReview
├── FileDiff
│   └── Hunk
│       ├── LineCoverage
│       └── DiffRow
│           ├── Code
│           ├── Linewise
│           ├── LineEnding
│           ├── Moved
│           ├── Wordwise
│           └── Elision
└── FileNotice
    └── TooLarge
```

`LineCoverage` stores one-based, half-open source ranges. Rows carry render-ready
source fragments and syntax/change spans. Hunks group one reviewable area;
unchanged interiors may become explicit elisions, except that a single omitted
line is always shown.

The UI presents review items in one path-ordered ribbon. The active path is
bold; left/right (or `h`/`l`, `[`/`]`) moves between paths and resets the body
scroll. When the complete ribbon does not fit, it keeps the active filename
visible and marks hidden neighbors with ellipses.

## Rust frontend

Tree-sitter supplies the occurrence CST. The frontend retains:

- original source and line ranges;
- top-level definitions and imports;
- token and comment occurrences;
- recursive structural fingerprints.

A fingerprint records node kind, grammar field, ordered containment, leaf
payload, and named, extra, and missing-node state. The code fingerprint excludes
comments; the full fingerprint includes them.

Definitions correspond one-to-one by kind and name. Exact fingerprints
disambiguate duplicate names before source order breaks remaining ties.

Tokens inside matched definitions use a shared ordered matcher:

1. Find values unique on both sides.
2. Keep their longest monotone subset as anchors.
3. Align each intervening gap with LCS.
4. Use a deterministic greedy matcher when a gap exceeds the quadratic budget.

The planner then emits:

- inline spans for changed definitions;
- linewise rows for comment edits;
- compact wordwise rows for imports;
- move rows for structurally identical definitions outside the stable order;
- reflow rows when source bytes changed but the full structure did not.

Reflow is certified only when both parses are free of recovery. Any recovered
parse uses the conservative plain planner. A valid Rust plan also falls back to
plain rows when a line ending changes or an exact non-whitespace edit falls
outside its structural projection.

## Plain frontend

Lines correspond by exact text and line ending using the same anchored matcher.
Changed gaps are paired in order; paired lines receive intra-line spans over
whitespace, word, and punctuation runs. Remaining lines are additions or
deletions.

Each hunk retains three surrounding context lines. Nearby hunks merge. Line
endings participate in identity, and a dedicated row names LF, CRLF, or a
missing end-of-file newline.

## Limits

Structural correspondence currently stops at top-level Rust definitions.
Wrapper removal, reparenting, and edited moves require occurrence matching below
that boundary.

There is no generic language abstraction yet. A second structural frontend
should determine the shared interface rather than encoding it speculatively.

## Source layout

Rust module roots use `X.rs`, with child modules under `X/**`; `X/mod.rs` is
forbidden. Clippy enforces the convention for compiled modules, and the
repository layout test also catches unlinked files.
