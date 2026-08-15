# Design

Mig turns two source revisions into a bounded, presentation-ready review.
Tree-sitter supplies syntax-aware highlighting and structural correspondence
when a Rust parse is trustworthy; a line diff is the conservative fallback.

## Data flow

```text
before/after text + display path
              │
              ▼
   syntax or line projection
              │
              ▼
  correspondence + hunk planning
              │
              ▼
 FileReview (rows, coverage, boundaries)
              │
              ▼
 terminal layout, color, clipping, navigation
```

Revision acquisition ends at the text-pair boundary. The planner owns every
claim about the change: correspondence, row treatment, grouping, elision,
source coverage, and whether displayed content reaches EOF. The UI consumes
those facts and never reconstructs edits from rendered text.

## Correspondence

Line, token, and comment streams share one ordered matcher. Values unique on
both sides become candidate anchors; their longest increasing subsequence keeps
the stable order. LCS aligns each intervening gap, with a deterministic greedy
matcher once a gap exceeds the quadratic budget. This keeps local edits precise
without letting a large repetitive region dominate time or memory.

Structural fingerprints certify unchanged shape instead of guessing from text
similarity. Identity plus an exact fingerprint resolves duplicate occurrences;
the stable-order subsequence distinguishes retained order from moves. A
structural plan is accepted only for recovery-free syntax whose projection
covers every literal edit. Otherwise the whole file uses the line plan: a
noisier complete review is preferable to a confident-looking omission.

The line plan anchors exact lines first. Inside an unmatched HTML gap it makes a
second pass using equality without leading indentation; those matches render as
reflow. This keeps a reindented subtree attached to itself when a wrapper is
inserted, while the wrapper remains the actual addition.

## Bounded review

Line terminators participate in identity, and paired replacements align
whitespace, word, and punctuation runs. Line hunks keep three context lines and
merge when separation would save no space. Structural hunks retain their frame
and every signal row; only two or more distant context rows become an elision.

Coverage survives elision so navigation and file-boundary state do not depend
on visible rows. A hunk reaching the displayed source side's final line receives
a lone gutter stroke. It is scroll-completion feedback, not source content.
