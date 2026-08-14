# 🫆 mig

Mig is a fixture-driven structural-diff experiment. The binary deliberately has
no filesystem, Git, network, or workspace layer yet: `main` passes one hardcoded
Rust before/after pair into the core and opens its unified terminal review.

The implementation follows one short pipeline:

```text
fixture strings
→ parse_rust
→ one-to-one syntax correspondence
→ plan_unified
→ FileDiff windows
→ terminal rendering
```

`parse_rust` retains the source, Tree-sitter CST, line index, top-level
definition occurrences, imports, tokens, comments, and exact recursive
fingerprints. A fingerprint includes node kind, grammar field, ordered
containment, leaf payload, and recovery nodes. Names help locate corresponding
definitions but never make two occurrences the same; duplicate `impl` blocks
remain one-to-one occurrences.

Correspondence uses exact occurrences and unique syntax tokens as anchors, then
does ordered alignment only inside bounded unresolved gaps. The planner projects
that correspondence directly into a unified review. There is intentionally no
intermediate “semantic change” object that merely repackages presentation rows.

A `~` row has a strict meaning: raw source changed while the parser-certified
structural content stayed identical. Mig does not globally declare whitespace
to be trivia. A future Python or Haskell frontend must expose indentation/layout
through its CST (or virtual layout nodes); indentation that changes containment
is then a semantic edit, while indentation or wrapping that preserves the tree
may be shown as reflow. Parser recovery disables the reflow claim
conservatively.

The render contract is:

```text
FileDiff
└── DiffWindow
    ├── one-based, half-open before/current LineMapping
    └── DiffRow
        ├── Code       current source: context, inline change, or reflow
        ├── Linewise   optional before and/or current source line
        ├── Moved      current source with an optional old line
        ├── Wordwise   compact shared-affix replacement
        └── Elision    intentionally omitted mapped source
```

Windows are the common-area/anchor boundary: related code and comment edits
stay together, and unrelated source is folded. The terminal does no semantic
inference. It measures the exact labels it will display (`- 26`, `16 → 38`,
`⋮`, and so on), aligns every unified gutter to their maximum Unicode display
width, and reserves a practical 62-column source viewport. There is no
split-screen model or renderer.

The visual grammar is intentionally small. `⋮` means vertically omitted source,
cyan-tinted run-ins belong to the before-world, `~` means certified reflow, and
bold body text is reserved for the exact changed span. Syntax classes use hue,
not weight, and the user's terminal owns every background.

Run the fixture:

```sh
cargo run
```

Quit with `q`, `Esc`, or `Ctrl-C`. Scroll with `j`/`k`, Up/Down, Page Up/Page
Down, Space, Home, and End.

The current public core entry point is:

```rust
diff_file(path, before, after) -> Result<FileDiff>
```

Run all checks with:

```sh
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt -- --check
```

The longer-term matching theory, explicit non-goals, and acceptance cases live
in [DESIGN.md](DESIGN.md).
