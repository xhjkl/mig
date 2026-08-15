# Design

Mig turns two source revisions into a bounded, presentation-ready review.
Tree-sitter supplies trustworthy concrete syntax trees and highlighting for
supported languages; an exact line-leaf tree is the universal fallback.

## Data flow

```text
before/after text + display path       retained input notice
              │                                  │
              ▼                                  │
   byte-total source maps                         │
              │                                  │
              ▼                                  │
 neutral revision projections                    │
              │                                  │
              ▼                                  │
 cross-revision correspondence graph             │
              │                                  │
              ▼                                  │
 review planner → FileDiff ───────┬───────────────┘
                                  ▼
                             FileReview
                                  │
                                  ▼
             terminal layout, color, clipping, navigation
```

Revision acquisition ends at either the bounded text-pair boundary or a
retained notice. A frontend projects each revision into an arena of exact source
ranges, ordered children, semantic review boundaries, content channels, and
syntax classes; parser-owned nodes never cross that boundary. Bytes owned by a
CST node but omitted from its children become intrinsic-syntax or layout leaves.
Together with the shared source map, this makes each projection byte-total while
keeping indentation out of structural identity.

The line frontend uses the same projection shape: its synthetic root owns one
terminator-aware leaf per physical line. Linewise review is therefore the
degenerate CST case, not a parallel diff implementation. Generated files,
unsupported extensions, recovered parses, and source facts that a CST cannot
certify select this projection symmetrically for both revisions. Exact physical
line edges are a second graph view used for hunk layout and source-completeness
certification; a line projection derives them directly from its line-node
edges, while a syntax projection computes them over the shared source map.

## Correspondence

The correspondence engine links the two immutable projections; it does not
construct display rows. Structural fingerprints compare node kind, incoming
field, content channel, and ordered child payload while excluding presentation
style. Separate full and code fingerprints let comment-only edits remain
distinct from syntax edits. Identities disambiguate named review boundaries,
and exact fingerprints resolve duplicate occurrences before stable-order
pairing.

Exact ordered streams use one bounded matcher. Values unique on both sides
become candidate anchors, their longest increasing subsequence preserves stable
order, and LCS aligns each intervening gap. A deterministic greedy matcher takes
over when a gap exceeds the quadratic budget. Modified review boundaries add a
weighted evidence pass over exact subtrees and leaves: mutually unique evidence
may cross, then ordered dynamic programming aligns the ambiguous residue within
established anchor gaps. Beyond that pass's quadratic budget, only uniquely
certified evidence is retained; mere grammar-kind similarity is never promoted
to correspondence. Exact composite nodes are likewise linked outside local
order, so moves and reparenting remain explicit graph facts rather than
remove/add noise.

This is why wrapping an existing HTML element needs no wrapper rule. The old and
new element subtrees have the same syntax fingerprint, their different parents
make the edge reparented, and the current source range supplies its exact new
indentation. Whitespace-sensitive HTML bodies are opaque leaves, so their bytes
cannot be mistaken for layout.

## Review planning

The planner consumes projection policy and correspondence edges without knowing
the path, language, or grammar node names. It performs no structural or review-
boundary rematching. Pairing explicit unmatched physical rows and finding common
text affixes are presentation-local layout choices, not new graph claims. The
planner decides row treatments, groups hunks, retains three line-context rows,
merges nearby changes, and abbreviates only two or more distant context rows.
Inline token edits, reflow, moves, comments, compact declarations, and ordinary
line replacements are different presentations of the same graph relations.

Line coverage survives elision and remains planner-owned. Once hunk order and
abbreviation are final, reaching the displayed source side's last line appends
exactly one ordered `FileBoundary` row to the globally final hunk; a before-side
endpoint is consulted only when no current-side candidate exists. The renderer
lays out these facts and never reconstructs correspondence from text. Its lone
gutter stroke is scroll-completion feedback, not source content.

The bounded matcher is only one resource boundary. Acquisition retains a notice
instead of loading a revision above the byte limit; line-dense input is retained
the same way before projection. A trustworthy parse that exceeds the syntax-node
budget falls back symmetrically to the line graph. The byte limit bounds parser
input, while the line and syntax-node limits bound the neutral arenas and graph
that survive analysis.
