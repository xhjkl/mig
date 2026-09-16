# Design

Mig (`m`) reviews code changes in a terminal as focused structural hunks with
the context needed to understand them.

## Inputs

```sh
m                 # HEAD → disk, under the current directory
m HEAD~2          # HEAD~3 → HEAD~2
m old.rs new.rs   # old → new; no Git required
```

Every review starts with a before/after text pair. With no arguments, `m` scans
staged, unstaged, and untracked paths under the current directory. It resolves
`HEAD` once and compares it with disk contents, including any edits made after
staging.

`m <commitish>` shows the changes introduced by the selected commit, using its
first parent as the baseline, or an empty tree for a root commit.
`m <before> <after>` reads the two file paths directly, without requiring Git.

Git input skips non-text files, symlinks, and submodules. Either revision
exceeding 16 MiB or 100,000 lines produces a notice instead of a diff.

## From text to screen

`syntax::parse` binds parser trees to the original text. `syntax::lower`
discards parser handles and produces Mig-owned, language-neutral nodes with
source ranges, parentage, identity, and delimiter ownership. Trailing commas
and semicolons stay attached to preceding items. Unknown languages, an
`@generated` header in either revision, or a parse that cannot be safely lowered
send both revisions through the same pipeline as exact `Line` leaves. Syntax
coloring remains presentation metadata.

`correspondence` matches enclosing constructs before their contents. Independent
review units are the file itself or its direct children; nested constructs are
never promoted into independent units. Descendants match beneath paired
parents, and line matching respects the same boundaries.

Wrapper recovery requires a unique surviving containment path through
transparent wrappers and cannot cross sealed boundaries. For `x → Some(x)`,
retain `x` and mark the wrapper added; for `x → (x, x)`, the match is ambiguous.
A new function body is not a wrapper around an old statement. A separate pass
recognizes constrained moves of unique exact subtrees inward or outward between
surviving nested owners within the same matched review unit.

Formatting differs from content: changing the string literal `"a b"` to
`"a  b"` is an edit. Python indentation changes are formatting only when block
nesting stays unchanged. Moving a statement into an `if` body is an edit.

`tree_diff` forms source ranges for changes, expanding them to complete syntax
owners and including their punctuation. Replacements keep their old and new
sides in one change; moves keep their origin and destination together. This
keeps later sorting and context selection from splitting either pair.
`RawSourceDiff` also carries the source layout needed to order changes and
attach context.

`refine` splits distant changes, merges nearby changes, and ranks hunks: edits,
moves, imports/directives, then formatting. It adds enclosing constructs'
opening lines and nearby unchanged lines, marking omitted ranges.
`RefinedHunk` retains only final coverage and changes.

`presentation` converts those ranges to typed `ReviewRow`s with text, line
numbers, and highlighting. `ui` draws and navigates them; it does not decide
what changed. Test diff behavior through these rows; check terminal appearance
visually.

`run` selects inputs; `diff::diff_file` runs the pipeline. Acquisition,
differencing, presentation, and rendering remain private implementation modules.
