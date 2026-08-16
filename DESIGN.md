# Design

Mig answers one question: which structural unit or source region changed, and
what context makes that change understandable? It is a staged review engine,
not a terminal renderer interpreting raw diff text.

```text
worktree, commit, or explicit file pair
                    │
                    ▼
        bounded revisions or notices
                    │
                    ▼
         neutral source projections
                    │
                    ▼
          correspondence graph
                    │
                    ▼
          review planner → FileDiff
                    │
                    ▼
         FileReview → terminal UI
```

Acquisition yields bounded text pairs or retained size and line notices;
Git-backed modes omit unsupported and non-text entries. Worktree review compares
a pinned `HEAD` tree directly with regular files on disk, so index state affects
review order rather than content. Commit review uses the selected commit's first
parent, or the empty tree for a root commit, keeping every later stage strictly
two-revision. Explicit pairs are independent of Git. Rename detection stays off
so path-only renames remain visible as a deletion and addition.

Frontends turn both revisions into parser-independent projections. The
correspondence engine links those projections without creating display rows.
The planner chooses rows, marks, ordering, context, elision, and coverage.
`FileDiff` is that semantic boundary; `FileReview` adds retained notices. The UI
only lays out, styles, clips, and navigates the already ordered review.
