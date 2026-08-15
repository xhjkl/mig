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
FileDiff → DiffWindow → DiffRow
        │
        ▼
terminal UI
```

The core produces presentation-ready rows. The UI owns layout, clipping, color,
and navigation; it does not infer correspondence or edits.

## Input and dispatch

With no arguments, `m` reviews the net `HEAD`→working-tree state beneath the
current directory, including deletions and nonignored untracked paths.
NUL-containing and non-UTF-8 files are skipped.

A lowercase `.rs` extension selects structural Rust diffing. Other paths use
the plain planner.

A case-sensitive `@generated` substring within the first 20 lines of either
revision forces the plain planner. Generated files remain visible, are tagged
in the UI, and follow authored files in directory reviews.

## Review model

```text
FileDiff
└── DiffWindow
    ├── LineMapping
    └── DiffRow
        ├── Code
        ├── Linewise
        ├── Moved
        ├── Wordwise
        └── Elision
```

`LineMapping` stores one-based, half-open source ranges. Rows carry render-ready
source fragments and syntax/change spans. Windows group one reviewable area;
unchanged interiors may become explicit elisions, except that a single omitted
line is always shown.

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

Reflow is certified only when both parses are free of recovery. Recovery may
produce a conservative structural diff, but never a reflow claim.

## Plain frontend

Lines correspond by exact text and line ending using the same anchored matcher.
Changed gaps are paired in order; paired lines receive intra-line spans over
whitespace, word, and punctuation runs. Remaining lines are additions or
deletions.

Each hunk retains three surrounding context lines. Nearby hunks merge. Line
endings participate in identity, so an end-of-file newline change remains
visible.

## Limits

Structural correspondence currently stops at top-level Rust definitions.
Wrapper removal, reparenting, and edited moves require occurrence matching below
that boundary.

There is no generic language abstraction yet. A second structural frontend
should determine the shared interface rather than encoding it speculatively.
