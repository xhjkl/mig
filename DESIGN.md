# Design

Mig presents code changes as structural hunks with enough context to read them.
The `core/` crate owns comparison and source accounting. The `cli/` crate owns
input, review order, terminal interaction, and text rendering.

## Inputs

```sh
m                 # HEAD → disk, under the current directory
m HEAD~2          # HEAD~3 → HEAD~2
m old.rs new.rs   # old → new; no Git required
```

With no arguments, the CLI compares HEAD with disk, including staged, unstaged,
and untracked paths below the current directory. It resolves HEAD once. A commit
argument compares that commit with its first parent, or an empty tree for a root
commit. Two file arguments supply the texts directly.

Git input skips non-text files, symlinks, and submodules. A revision over 16 MiB
or 100,000 physical lines produces a navigable notice instead of a diff.

## Source contract

- Context reproduces the same source text on both sides. Terminator changes are
  recorded separately.
- A replacement has different sides. A move preserves tokens and their boundaries.
  A reflow preserves token spelling, literal newlines, and syntax ownership within
  its corresponding scopes.
- Every physical line belongs to one kept pair or one piece of an island.
- Changes remain whole enough to read: touched statements, members, and definitions
  retain their structure, and heavily fragmented regions become before/after blocks.

`islands::verify` checks coverage, order, duplicate claims, preservation evidence,
and terminator facts against the sources. A failed structural account falls back
to a verified physical-line comparison in both debug and release builds.
`Comparison::Lines` records the reason; verification failure is distinct from an
unsupported language, generated input, parser recovery, or a resource limit.

## Stages

```text
text → source → syntax → matching → islands → document
                                   islands + document → attribution
```

The stages share immutable source geometry. Matching produces correspondence;
islands produce source changes and preservation evidence; document layout
produces excerpts. Attribution consumes the source islands associated with an
excerpt, so layout choices cannot turn formatting into a contract edit.

### Source

`Source` indexes physical lines with exact content and terminator byte ranges.
`LineNumber` is one-based and cannot represent zero. `LineSpan` is a half-open
range of those coordinates. Byte offsets and internal line indices remain
zero-based; conversion to a displayed line number is explicit.

LF, CRLF, and an unterminated final line are distinct. `EndingChange` represents
an added, removed, or replaced terminator. Islands account for these facts before
layout chooses rows or elisions.

### Syntax

Tree-sitter parses each revision with the same grammar. Language annotations
identify review units, identities, declarations, wrapper boundaries, decorations,
and layout whitespace. `TextTreatment` chooses parsed children, verbatim text,
or an extended token, such as HTML plaintext consuming the rest of a file.

Each syntax occurrence contains either branch children or token metadata.
Grammar symbols distinguish named and anonymous occurrences. Missing parser
nodes are rejected before the immutable arena is constructed. Only lowering can
change arena storage; consumers receive stable node IDs and read-only views.

Trailing commas and semicolons attach to the preceding item. Its source envelope
includes that delimiter even though its grammar subtree does not. Matching checks
both structural fingerprints and envelope tokens before declaring equivalence.
Parser-owned terminators count as part of a complete physical line.

Unknown languages, generated headers, and unreconciled parser errors use physical
lines. HTML has a narrow recovery for explicit paragraph nesting. A 500,000-node
budget and a depth budget of 128 apply before highlighting, lowering, and recursive
matching, so deeply nested input remains reviewable through line comparison.

### Matching

Review units pair before their contents. Definitions pair by identity, with exact
content deciding among repeated names before position. Comments and plain lines
pair by content. Unnamed or renamed units may pair by reciprocal similarity;
remaining compatible units pair between established anchors.

Within matched parents, children pair by identity, exact content, wrapper recovery,
syntax role, and then reciprocal similarity. Siblings outside the heaviest
in-order chain relocate. Exact subtrees may relocate between surviving owners
within one review unit; member declarations remain attached to their owners.
A statement appearing in another definition is a removal and addition.

Each correspondence is one record containing its partner, relation, placement,
and ancestry. Unmatched nodes have no correspondence record. A reverse index
supports after-side lookup. Exact subtree claims can supersede earlier wrapper
recoveries; superseded records cannot later mark an unrelated correspondence as
relocated.

Similarity requires at least two shared meaningful tokens, half of the smaller
candidate, and no more than an eightfold ratio between the larger candidate and
the shared part. Groups over 96 candidates skip pairwise similarity.

### Islands

Complete-line syntax occurrences anchor alignment. Lines between anchors align
by exact text; blank and punctuation-only lines do not anchor themselves.
Explicit wrapper evidence allows a preserved construct to align within its new
or former wrapper.

An island contains `Piece` variants:

- `Block`: removed lines followed by added lines, with either side possibly empty.
- `Inline`: line-for-line edits with equal side lengths.
- `Reflow`: layout changes within corresponding syntax scopes.
- `Move`: intact syntax shown at its destination.

Reflow evidence retains token boundaries, literal newlines, ancestor structure,
and ownership. It never compares whitespace-stripped strings. A reflow under an
explicit wrapper uses its corresponding inner scopes, rather than claiming that
the wrapper itself is formatting.

Adjacent islands merge when no kept line separates them. Alternating blocks and
heavily fragmented interiors become one before/after block. Equal edge lines
become kept pairs again. An island's change description is derived from its
pieces; it has no separate mutable kind tag. Terminator notices are independent
of piece presentation.

### Document and attribution

Nearby islands share an `Excerpt`. Kept lines provide context; unchanged opening
lines provide orientation crumbs; skipped stretches have explicit elisions.
Every displayed source coordinate uses `LineNumber` or `LineSpan`.

Moved lines align by boundary-preserving tokens. Long consecutive interior runs
may be elided as `Elision::Move`, with equal line counts on both sides. A skipped
source gap is `Elision::Gap` and makes no claim of content identity. Rewrapped
parts remain explicit reflows. Document sections retain their contributing island
indices for attribution; finished hunks contain their excerpt without those
transient references.

`attribution` attributes changes to declarations, innermost first. Decorators and
their inner definition share one declaration identity. Heading lines are estimated
from source geometry; material edits to those lines set the contract flag. Moves
can name their declarations without setting that flag. `Attribution` keeps
formatting labels separate from declaration changes, so formatting cannot create
review dependencies. `Hunk` combines an excerpt with its attribution; `FileDiff`
contains those hunks and `FileSymbols` across both revisions.

## Review and rendering

The CLI converts `FileDiff` into `ReviewFile`, adding consumer counts and review
ordering. A name is attributable only when exactly one reviewed non-test source
file defines it. A consumer spells the name and any required owner without
defining the name itself.

Sources precede tests, documentation, and generated files. Within a tier, more
consumers lead, then contract changes, the caller's tie-break, and path. Within a
file, edits precede moves, wiring, and formatting. Consumer counts and contract
changes break ties within a priority.

The terminal UI and text dump render prepared rows. They do not match syntax,
classify edits, or reconstruct terminator changes.

## Verification

Behavior tests exercise the public comparison entry point and inspect its rows.
Language matrices cover significant whitespace, formatting, configuration owners,
list ordering, and web text. CLI tests cover semantic edits, final newlines,
deep nesting, replay, failure status, and operation without a Git executable.
History reproductions retain source provenance; snapshot expectations must come
from a public contract's captured output.

The stress harness walks first-parent history or replays captured pairs. Each
comparison runs in a child process on the normal main-thread stack. At a deadline,
the parent terminates and reaps that process. Worker failures cannot leave diff
threads running in the background or change the resource limits being tested.

The row checker checks text reproduction, bounds, ordering, duplicate claims,
identical replacements, and the alignment of unshown source lines. An independent
concrete-parser walk audits lexical preservation without using Mig's lowered
syntax, fingerprints, or preservation predicates. Readability findings and timing
are recorded separately. Hard defects produce a failing exit status after the
summary and reproduction cases have been written.
