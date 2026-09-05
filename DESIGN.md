# Design

Mig presents source changes as focused structural hunks with the context needed
to review them. Its staged pipeline carries each change from source text to
typed terminal rows.

```text
worktree, commit, or explicit file pair
                    │
                    ▼
             bounded source text
                    │
                    ▼
       parse concrete syntax or exact lines
                    │
                    ▼
       lower to internal neutral syntax trees
                    │
                    ▼
    correspond trees → form atomic raw hunks
                    │
                    ▼
       refine priority, order, and context
                    │
                    ▼
        present typed rows → terminal UI
```

Every review starts with a before/after text pair: worktree mode reads pinned
`HEAD` and disk, commit mode reads the first parent—or an empty tree for a root
commit—and the selected commit, and file-pair mode reads the two paths directly.
Binary or unsupported Git entries are skipped; oversized files remain visible
as size or line-count notices.

`syntax::parse` binds concrete parser trees to exact sources; `syntax::lower`
discards parser handles and produces typed, language-neutral arenas with
provenance, parentage, identity, and delimiter ownership. If either parse is
unsafe, both revisions become exact `Line` leaves in the same pipeline; syntax
coloring remains presentation metadata.

Frontends distinguish formatting from whitespace carried by literals. Python
retains suite nesting as sealed syntax boundaries, so equivalent indentation
can reflow while a statement changing scope remains a structural edit.

Correspondence pairs flat file-level units and recursively matches descendants
only beneath paired parents; lowering rejects nested unit promotion. Unique
payloads may cross transparent wrappers at any depth, sealed owners prevent
tunneling, and owner-local anchors partition line fallback before changes snap
to source-complete syntax owners.

`tree_diff::RawSourceDiff` couples atomic `SourceHunk`s with the source layout
needed to place them. Refinement ranks and coalesces those hunks, adds
breadcrumbs, halos, and elisions, then emits `RefinedHunk`s containing only
final coverage and semantic changes.

`presentation` alone slices source text into styled, typed rows. The terminal UI
only lays out, clips, styles, and navigates those rows; tests stop at presentation
facts and terminal rendering is checked visually.

The executable enters through `run`; acquisition, differencing, presentation,
and rendering remain private implementation modules.
