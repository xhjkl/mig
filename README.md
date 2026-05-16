# mig

Mig is becoming a live diff viewer for agent-inhabited worktrees.

The first working layer is CLI-first, with a TUI tryout:

```sh
mig tryout inline-change   # open a hardcoded inline replacement fixture
mig tryout whole-function  # show a whole-function replacement in split view
mig tryout move-without-identity # show a moved line as delete+insert in stacked view
mig status       # one-shot L2 filesystem scan plus L1 git working-tree state
mig files          # list files visible to the L2 scanner
mig watch          # print a new filesystem turn every time watched files change
mig watch --tui    # open the Ratatui viewer for the first text file in each turn
mig watch --poll   # fall back to polling when native events are unavailable
```

The model is intentionally split:

- L1 is git history, when present: durable, shared, and commit-addressable.
- L2 is Mig's live filesystem timeline. At launch it records a baseline
  snapshot, then every batch of filesystem events becomes a turn with a text
  diff between git commits.
- DiffGraph is the immutable-ish truth: snapshots, sides, aligned blocks,
  inline alignments, and anchors.
- DiffView is projection state: viewport, selection, and folds.

The current `mig tryout` TUI is a fixture-first sketchpad: it renders fake
internal state through the same alignment model the real diff path will use.
`mig watch --tui` now feeds real L2 filesystem turns through that same model.
The later Rust + Vello frontend can grow from it without treating hunks as the
source of truth.

`DiffLayout::Auto` is intentionally coarse for now: touching old/new lines imply
a possible transformation and pick split view; pure one-sided delete/insert
stories pick stacked view.
