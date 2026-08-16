# Design

Mig answers one review question: given this current-world structural hierarchy,
what expression and immediate surroundings changed? It obtains a trustworthy
concrete syntax tree when available (or the same model's exact line-leaf
degeneration), finds language-agnostic correspondence, plans bounded pretty
hunks with context halos and hierarchy breadcrumbs, and only then renders them.

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

`FileDiff` is the planner/renderer boundary: it owns row membership, ordering,
marks, elisions, and source coverage. `FileReview` only joins that stream with
retained input notices; the terminal renderer chooses glyphs, style, clipping,
viewport geometry, and navigation without inferring diff structure.
Directory acquisition retains Git provenance through that boundary so the
ribbon presents dirty, staged, then untracked paths; an inspected generated
diff overrides those classes and appears last.

Revision acquisition ends at either the bounded text-pair boundary or a
retained notice. A frontend projects each revision into an arena of exact source
ranges, ordered children, semantic review boundaries, content channels, and
syntax classes; parser-owned nodes never cross that boundary. Bytes owned by a
CST node but omitted from its children become intrinsic-syntax or layout leaves.
Together with the shared source map, this makes each projection byte-total while
keeping indentation out of structural identity.

Language adapters are the only grammar-aware policy layer. They annotate neutral
nodes with review treatment, movement eligibility, and adjacent-layout ownership;
the planner never branches on a language or grammar node name.

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
boundary rematching. Exact physical-line edges remain coverage certificates, but
a line becomes a display checkpoint only when it contains an exact, stable,
non-reparented, substantive leaf. Blank, layout-only, and delimiter edges cannot
puncture a replacement merely because their bytes agree, regardless of whether
those leaves came from a grammar CST or the exact line-leaf degeneration.
Weak rows join only surrounding changes admitted by that same bounded context-
halo rule; weak rows outside that interval remain ordinary context. Selection
therefore absorbs each weak gap only within the same seven-row coalescing
threshold and cannot bridge a larger interval.

The ordered line graph also supplies a shared edit-script coordinate. Facts with
a current side use that source position; before-only facts keep their old-source
order inside the corresponding gap. Before-only and current-only units at the
same gap therefore coalesce and sort as one old-then-current replacement even
when their raw line numbers differ. Replacement row groups remain atomic through
hunk coalescing and source ordering; later context insertion cannot split or
relocate their two revision blocks.

Selection precedes presentation. Each ordinary physical signal row grows a
context halo of up to three unchanged physical lines before and after against
the whole file, not merely its frontend review boundary. Context halos that
overlap, touch, or would leave only one isolated row coalesce. When a matched
syntax region supplies ancestry, the first physical line of every neutral
composite ancestor containing a signal becomes a sparse
current-world hierarchy breadcrumb; even a separately changed ancestor can
appear neutrally in that hierarchy. Each breadcrumb grows its own display-only
context halo of up to three unchanged physical lines on either side, making a
structural step as readable as the signal it explains. Distant context alone is
folded, while a single omitted row stays visible. Breadcrumb context halos do
not widen the signal ranges used to merge hunks, so two distant edits in one
function retain separate local windows and repeat the useful structural
hierarchy. Pure move presentations keep their own compact framing instead of
growing an ordinary context halo.

An unmatched multi-line gap is one replacement fact, not a request to pair rows
by ordinal position. Its complete before block renders first, followed by its
complete current block; a one-line pair can still carry precise token marks.
Aligned edit-script ordering and context-halo coalescing apply across inline
edits, reflow, moves, comments, compact declarations, and ordinary line
replacements without erasing their distinct presentations.

Geometric coalescing runs in aligned source order before review cadence is
applied. A hunk containing any primary semantic signal remains primary; pure
move, compact declaration/import, and reflow hunks follow the logic-change
hunks instead of interrupting them at their physical source positions.

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
