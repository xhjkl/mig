use super::parse::{ParsedFile, ParserLanguage, SyntaxFailure, tree_sitter_language};
use super::{
    ChildSlot, ContentChannel, Delimiter, DelimiterKind, GrammarField, GrammarSymbol, Language,
    Leaf, LeafRole, NodeId, ReviewUnit, SiblingMatching, SyntaxKind, SyntaxNode, SyntaxTree,
    WrapperBoundary, c, css, html, rust, typescript,
};
use crate::diff::SyntaxClass;
use crate::diff::source::Source;
use ::tree_sitter::{Language as TreeSitterLanguage, Node, Query, QueryCursor, StreamingIterator};
use anyhow::{Context, anyhow};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::OnceLock;

/// Parser-node view available only while a language lowerer annotates the neutral arena.
pub(super) struct NodeContext<'tree, 'source> {
    pub(super) node: Node<'tree>,
    pub(super) parent_kind: Option<&'static str>,
    pub(super) source: &'source str,
}

/// Parser-omitted bytes owned directly by one CST node.
pub(super) struct GapContext<'tree, 'source> {
    pub(super) parent: Node<'tree>,
    pub(super) bytes: Range<usize>,
    pub(super) source: &'source str,
}

impl GapContext<'_, '_> {
    pub(super) fn is_whitespace(&self) -> bool {
        self.source[self.bytes.clone()]
            .chars()
            .all(char::is_whitespace)
    }

    pub(super) fn default_channel(&self) -> ContentChannel {
        if self.is_whitespace() {
            return ContentChannel::Layout;
        }
        ContentChannel::Syntax
    }
}

/// Frontend instruction resolved to one concrete decoration-owner edge after lowering.
#[derive(Clone, Copy, Default)]
pub(super) enum DecorationHint {
    #[default]
    None,
    /// Decorates the next frontend-declared owner in this sibling scope.
    FollowingSibling,
    /// Decorates the nearest frontend-declared enclosing owner.
    EnclosingOwner,
}

/// Language decision for one parser node before its parser handle is discarded.
#[derive(Default)]
pub(super) struct NodeAnnotation {
    pub(super) review: Option<ReviewUnit>,
    pub(super) channel: Option<ContentChannel>,
    pub(super) descendant_channel: Option<ContentChannel>,
    /// Language-specific spelling used as a graph identity, independent of review units.
    pub(super) identity: Option<Range<usize>>,
    /// Source-language decoration relationship; resolved only after all siblings exist.
    pub(super) decoration: DecorationHint,
    /// Whether this semantic boundary may own decorations.
    pub(super) owns_decorations: bool,
    pub(super) sibling_matching: SiblingMatching,
    pub(super) wrapper_boundary: WrapperBoundary,
    /// Language-owned extent when the grammar stops before the semantic content does.
    pub(super) extent: Option<Range<usize>>,
    /// Treat the node's complete byte range as one payload and discard parser children.
    pub(super) prune_children: bool,
}

/// Official query sources and their one process-wide compilation for one grammar.
pub(super) struct HighlightQueries {
    sources: &'static [&'static str],
    compiled: OnceLock<Result<Box<[Query]>, String>>,
}

impl HighlightQueries {
    pub(super) const fn new(sources: &'static [&'static str]) -> Self {
        Self {
            sources,
            compiled: OnceLock::new(),
        }
    }

    fn compiled(
        &'static self,
        language: &TreeSitterLanguage,
        syntax_language: Language,
    ) -> anyhow::Result<&'static [Query]> {
        let queries = self.compiled.get_or_init(|| {
            self.sources
                .iter()
                .map(|source| {
                    let query = Query::new(language, source);
                    query.with_context(|| {
                        format!("invalid official highlight query for {syntax_language:?}")
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()
                .map(Vec::into_boxed_slice)
                .map_err(|error| format!("{error:#}"))
        });
        let queries = match queries {
            Err(error) => return Err(anyhow!(error.to_owned())),
            Ok(queries) => queries,
        };
        Ok(queries)
    }
}

/// Lower parser-owned syntax into Mig's language-neutral source arena.
pub(super) fn lower<'source>(
    parsed: ParsedFile<'source>,
) -> Result<SyntaxTree<'source>, SyntaxFailure> {
    let source = parsed.source;
    let parsed_language = parsed.language;
    let language = tree_sitter_language(parsed_language);
    let root = parsed.tree.root_node();
    let language_id = syntax_language(parsed_language);
    let highlight_queries = highlight_queries(parsed_language)
        .compiled(&language, language_id)
        .map_err(SyntaxFailure::Setup)?;
    let highlights = collect_highlights(highlight_queries, root, source.as_str().as_bytes());
    let Some(nodes) = lower_nodes(&source, root, parsed_language, &language, &highlights) else {
        return Err(SyntaxFailure::Fallback);
    };
    Ok(SyntaxTree::from_nodes(
        source,
        language_id,
        NodeId::new(0),
        nodes,
    ))
}

fn syntax_language(language: ParserLanguage) -> Language {
    match language {
        ParserLanguage::C => Language::C,
        ParserLanguage::Rust => Language::Rust,
        ParserLanguage::Html => Language::Html,
        ParserLanguage::Css => Language::Css,
        ParserLanguage::TypeScript => Language::TypeScript,
        ParserLanguage::Tsx => Language::Tsx,
        ParserLanguage::JavaScript => Language::JavaScript,
        ParserLanguage::Jsx => Language::Jsx,
    }
}

fn highlight_queries(language: ParserLanguage) -> &'static HighlightQueries {
    match language {
        ParserLanguage::C => &c::HIGHLIGHT_QUERIES,
        ParserLanguage::Rust => &rust::HIGHLIGHT_QUERIES,
        ParserLanguage::Html => &html::HIGHLIGHT_QUERIES,
        ParserLanguage::Css => &css::HIGHLIGHT_QUERIES,
        ParserLanguage::TypeScript => &typescript::TYPESCRIPT_HIGHLIGHTS,
        ParserLanguage::Tsx => &typescript::TSX_HIGHLIGHTS,
        ParserLanguage::JavaScript => &typescript::JAVASCRIPT_HIGHLIGHTS,
        ParserLanguage::Jsx => &typescript::JSX_HIGHLIGHTS,
    }
}

fn annotate_node(language: ParserLanguage, context: NodeContext<'_, '_>) -> NodeAnnotation {
    match language {
        ParserLanguage::C => c::annotate(context),
        ParserLanguage::Rust => rust::annotate(context),
        ParserLanguage::Html => html::annotate(context),
        ParserLanguage::Css => css::annotate(context),
        ParserLanguage::TypeScript
        | ParserLanguage::Tsx
        | ParserLanguage::JavaScript
        | ParserLanguage::Jsx => typescript::annotate(context),
    }
}

fn normalize_recovery(
    language: ParserLanguage,
    source: &Source<'_>,
    nodes: &mut [SyntaxNode],
) -> bool {
    if matches!(language, ParserLanguage::Html) {
        return html::normalize_recovery(source, nodes);
    }
    let error = grammar_symbol(language, "ERROR", true);
    !nodes.iter().any(|node| {
        node.missing
            || (node.kind == error
                && node
                    .parent
                    .is_none_or(|parent| nodes[parent.index()].kind != error))
    })
}

fn grammar_symbol(language: ParserLanguage, kind: &str, named: bool) -> SyntaxKind {
    let language = tree_sitter_language(language);
    SyntaxKind::Grammar(GrammarSymbol::new(language.id_for_node_kind(kind, named)))
}

fn gap_channel(language: ParserLanguage, context: GapContext<'_, '_>) -> ContentChannel {
    match language {
        ParserLanguage::C => c::gap_channel(context),
        ParserLanguage::Rust => rust::gap_channel(context),
        ParserLanguage::Html => html::gap_channel(context),
        ParserLanguage::Css => css::gap_channel(context),
        ParserLanguage::TypeScript
        | ParserLanguage::Tsx
        | ParserLanguage::JavaScript
        | ParserLanguage::Jsx => typescript::gap_channel(context),
    }
}

enum Pending<'tree> {
    Parser(PendingNode<'tree>),
    Fragment(PendingFragment),
}

impl Pending<'_> {
    fn start_byte(&self) -> usize {
        match self {
            Self::Parser(pending) => pending.node.start_byte(),
            Self::Fragment(pending) => pending.bytes.start,
        }
    }
}

#[derive(Clone, Copy)]
struct PendingNode<'tree> {
    node: Node<'tree>,
    parent: Option<NodeId>,
    slot: ChildSlot,
    inherited_channel: Option<ContentChannel>,
    inherited_classification: Option<LeafClassification>,
}

struct PendingFragment {
    bytes: Range<usize>,
    parent: NodeId,
    channel: ContentChannel,
    classification: LeafClassification,
}

/// Parser-level leaf meaning kept separate from its terminal palette category.
#[derive(Clone, Copy)]
struct LeafClassification {
    role: LeafRole,
    syntax: SyntaxClass,
}

fn lower_nodes(
    source: &Source<'_>,
    root: Node<'_>,
    language: ParserLanguage,
    grammar: &TreeSitterLanguage,
    highlights: &HashMap<usize, LeafClassification>,
) -> Option<Vec<SyntaxNode>> {
    let mut nodes = Vec::<SyntaxNode>::new();
    let mut decoration_hints = Vec::<DecorationHint>::new();
    let mut decoration_owners = Vec::<bool>::new();
    let mut pending = vec![Pending::Parser(PendingNode {
        node: root,
        parent: None,
        slot: ChildSlot::Positional,
        inherited_channel: None,
        inherited_classification: None,
    })];
    let mut consumed_until = 0;

    while let Some(pending_item) = pending.pop() {
        if pending_item.start_byte() < consumed_until {
            continue;
        }

        let pending_node = match pending_item {
            Pending::Fragment(fragment) => {
                let id = NodeId::new(nodes.len());
                nodes[fragment.parent.index()].children.push(id);
                let lines = source
                    .line_coverage(fragment.bytes.clone())
                    .expect("source fragments remain inside source geometry");
                let spelling = source
                    .slice(fragment.bytes.clone())
                    .expect("source fragments remain inside source geometry");
                let delimiter = delimiter(spelling);
                let bytes = fragment.bytes;
                nodes.push(SyntaxNode {
                    kind: SyntaxKind::SourceFragment,
                    slot: ChildSlot::Positional,
                    source_envelope: bytes.clone(),
                    bytes,
                    lines,
                    parent: Some(fragment.parent),
                    children: Vec::new(),
                    leaf: Some(Leaf {
                        role: fragment.classification.role,
                        syntax: fragment.classification.syntax,
                        channel: fragment.channel,
                        delimiter,
                    }),
                    identity: None,
                    decoration_owner: None,
                    sibling_matching: SiblingMatching::OrderedSyntax,
                    wrapper_boundary: WrapperBoundary::Traversable,
                    review: None,
                    named: false,
                    extra: false,
                    missing: false,
                });
                decoration_hints.push(DecorationHint::None);
                decoration_owners.push(false);
                continue;
            }
            Pending::Parser(pending_node) => pending_node,
        };

        let parent_kind = pending_node.node.parent().map(|parent| parent.kind());
        let context = NodeContext {
            node: pending_node.node,
            parent_kind,
            source: source.as_str(),
        };
        let annotation = annotate_node(language, context);
        let inherited_channel = annotation
            .descendant_channel
            .or(pending_node.inherited_channel);
        let leaf_channel = annotation
            .channel
            .or(pending_node.inherited_channel)
            .unwrap_or(ContentChannel::Syntax);
        let inherited_classification = highlights
            .get(&pending_node.node.id())
            .copied()
            .or(pending_node.inherited_classification);

        let node = pending_node.node;
        let id = NodeId::new(nodes.len());
        if let Some(parent) = pending_node.parent {
            nodes[parent.index()].children.push(id);
        }
        let parser_bytes = node.byte_range();
        let bytes = annotation.extent.clone().unwrap_or(parser_bytes.clone());
        if bytes.end > parser_bytes.end {
            // Plaintext-style constructs absorb every parser sibling through their extent.
            consumed_until = consumed_until.max(bytes.end);
            let mut ancestor = pending_node.parent;
            while let Some(id) = ancestor {
                let ancestor_node = &mut nodes[id.index()];
                ancestor_node.bytes.end = ancestor_node.bytes.end.max(bytes.end);
                ancestor_node.source_envelope.end =
                    ancestor_node.source_envelope.end.max(bytes.end);
                ancestor_node.lines = source
                    .line_coverage(ancestor_node.bytes.clone())
                    .expect("expanded ancestors remain inside source geometry");
                ancestor = ancestor_node.parent;
            }
        }
        let lines = source
            .line_coverage(bytes.clone())
            .expect("tree-sitter nodes remain within their parsed source");
        let leaf = (annotation.prune_children || node.child_count() == 0).then(|| {
            let spelling = source
                .slice(bytes.clone())
                .expect("tree-sitter leaves remain inside source geometry");
            let classification =
                leaf_classification(node.kind(), leaf_channel, inherited_classification);
            Leaf {
                role: classification.role,
                syntax: classification.syntax,
                channel: leaf_channel,
                // Named leaves carry grammar meaning even when their spelling is punctuation.
                delimiter: if node.is_named() {
                    None
                } else {
                    delimiter(spelling)
                },
            }
        });
        let source_envelope = bytes.clone();
        nodes.push(SyntaxNode {
            kind: SyntaxKind::Grammar(GrammarSymbol::new(node.kind_id())),
            slot: pending_node.slot,
            bytes,
            source_envelope,
            lines,
            parent: pending_node.parent,
            children: Vec::with_capacity(if annotation.prune_children {
                0
            } else {
                node.child_count()
            }),
            leaf,
            identity: annotation.identity,
            decoration_owner: None,
            sibling_matching: annotation.sibling_matching,
            wrapper_boundary: annotation.wrapper_boundary,
            review: annotation.review,
            named: node.is_named(),
            extra: node.is_extra(),
            missing: node.is_missing(),
        });
        decoration_hints.push(annotation.decoration);
        decoration_owners.push(annotation.owns_decorations);

        if annotation.prune_children || node.child_count() == 0 {
            continue;
        }
        let mut lowered_children = Vec::with_capacity(node.child_count() * 2 + 1);
        let mut cursor = parser_bytes.start;
        for index in 0..node.child_count() {
            let Some(child) = node.child(index as u32) else {
                continue;
            };
            if child.start_byte() > cursor {
                lowered_children.push(fragment_pending(
                    source,
                    language,
                    node,
                    cursor..child.start_byte(),
                    id,
                    inherited_channel,
                    inherited_classification,
                ));
            }
            lowered_children.push(Pending::Parser(PendingNode {
                node: child,
                parent: Some(id),
                slot: child_slot(grammar, node.field_name_for_child(index as u32)),
                inherited_channel,
                inherited_classification,
            }));
            cursor = cursor.max(child.end_byte());
        }
        if cursor < parser_bytes.end {
            lowered_children.push(fragment_pending(
                source,
                language,
                node,
                cursor..parser_bytes.end,
                id,
                inherited_channel,
                inherited_classification,
            ));
        }
        pending.extend(lowered_children.into_iter().rev());
    }
    if !normalize_recovery(language, source, &mut nodes) {
        return None;
    }
    resolve_decoration_owners(&mut nodes, &decoration_hints, &decoration_owners);
    Some(nodes)
}

/// Resolve frontend-declared decoration relationships without widening either source extent.
fn resolve_decoration_owners(nodes: &mut [SyntaxNode], hints: &[DecorationHint], owners: &[bool]) {
    debug_assert_eq!(nodes.len(), hints.len());
    debug_assert_eq!(nodes.len(), owners.len());

    for parent_index in 0..nodes.len() {
        let parent = NodeId::new(parent_index);
        let children = nodes[parent_index].children.clone();
        let mut following_owner = None;
        for child in children.into_iter().rev() {
            match hints[child.index()] {
                DecorationHint::FollowingSibling => {
                    nodes[child.index()].decoration_owner = following_owner;
                }
                DecorationHint::EnclosingOwner => {
                    nodes[child.index()].decoration_owner =
                        nearest_decoration_owner(nodes, owners, parent);
                }
                DecorationHint::None if owners[child.index()] => {
                    following_owner = Some(child);
                }
                DecorationHint::None if !decoration_transparent(&nodes[child.index()]) => {
                    following_owner = None;
                }
                DecorationHint::None => {}
            }
        }
    }
}

fn nearest_decoration_owner(
    nodes: &[SyntaxNode],
    owners: &[bool],
    mut candidate: NodeId,
) -> Option<NodeId> {
    loop {
        if owners[candidate.index()] {
            return Some(candidate);
        }
        candidate = nodes[candidate.index()].parent?;
    }
}

/// Layout and independent commentary may separate a decoration from its semantic owner.
fn decoration_transparent(node: &SyntaxNode) -> bool {
    !node.named
        || node.leaf.is_some_and(|leaf| {
            matches!(
                leaf.channel,
                ContentChannel::Comment | ContentChannel::Layout
            )
        })
}

/// Anonymous punctuation and spacing delimit syntax without carrying anchor payload.
fn delimiter(spelling: &str) -> Option<Delimiter> {
    let kind = match spelling {
        "," => DelimiterKind::Comma,
        ";" => DelimiterKind::Semicolon,
        _ if spelling
            .chars()
            .all(|character| character.is_whitespace() || character.is_ascii_punctuation()) =>
        {
            DelimiterKind::Structural
        }
        _ => return None,
    };
    Some(Delimiter { kind, owner: None })
}

fn child_slot(grammar: &TreeSitterLanguage, field: Option<&str>) -> ChildSlot {
    let Some(field) = field else {
        return ChildSlot::Positional;
    };
    let field = grammar
        .field_id_for_name(field)
        .expect("tree-sitter child fields belong to their grammar");
    ChildSlot::Field(GrammarField::new(field))
}

fn fragment_pending<'tree>(
    source: &Source<'_>,
    language: ParserLanguage,
    parent: Node<'tree>,
    bytes: Range<usize>,
    parent_id: NodeId,
    inherited_channel: Option<ContentChannel>,
    inherited_classification: Option<LeafClassification>,
) -> Pending<'tree> {
    let context = GapContext {
        parent,
        bytes: bytes.clone(),
        source: source.as_str(),
    };
    let channel = inherited_channel.unwrap_or_else(|| gap_channel(language, context));
    let classification = leaf_classification(parent.kind(), channel, inherited_classification);
    Pending::Fragment(PendingFragment {
        bytes,
        parent: parent_id,
        channel,
        classification,
    })
}

fn collect_highlights(
    queries: &[Query],
    root: Node<'_>,
    source: &[u8],
) -> HashMap<usize, LeafClassification> {
    let mut highlights = HashMap::new();
    for query in queries {
        let mut cursor = QueryCursor::new();
        let mut captures = cursor.captures(query, root, source);
        while let Some((matched, capture_index)) = captures.next() {
            let capture = matched.captures[*capture_index];
            let name = query.capture_names()[capture.index as usize];
            let classification = capture_classification(name);
            let Some(classification) = classification else {
                continue;
            };
            highlights.insert(capture.node.id(), classification);
        }
    }
    highlights
}

fn leaf_classification(
    kind: &str,
    channel: ContentChannel,
    inherited: Option<LeafClassification>,
) -> LeafClassification {
    if channel == ContentChannel::Comment {
        return LeafClassification {
            role: LeafRole::Scaffolding,
            syntax: SyntaxClass::Comment,
        };
    }
    if channel == ContentChannel::Opaque {
        return LeafClassification {
            role: LeafRole::Payload,
            syntax: SyntaxClass::Plain,
        };
    }
    if channel == ContentChannel::Layout {
        return LeafClassification {
            role: LeafRole::Scaffolding,
            syntax: SyntaxClass::Plain,
        };
    }

    inherited.unwrap_or_else(|| classification_from_kind(kind))
}

fn capture_classification(capture: &str) -> Option<LeafClassification> {
    let stem = capture.split('.').next().unwrap_or(capture);
    let (role, syntax) = match stem {
        "comment" => (LeafRole::Scaffolding, SyntaxClass::Comment),
        "string" | "character" => (LeafRole::Payload, SyntaxClass::String),
        "constant" | "number" | "boolean" => (LeafRole::Payload, SyntaxClass::Literal),
        "keyword" | "conditional" | "repeat" | "exception" | "include" => {
            (LeafRole::Scaffolding, SyntaxClass::Keyword)
        }
        "type" | "constructor" => (LeafRole::Scaffolding, SyntaxClass::Type),
        "variable" | "function" | "method" | "property" | "field" | "parameter" | "module"
        | "namespace" | "label" | "tag" | "attribute" => {
            (LeafRole::Identifier, SyntaxClass::Identifier)
        }
        "operator" | "punctuation" => (LeafRole::Scaffolding, SyntaxClass::Punctuation),
        _ => return None,
    };
    Some(LeafClassification { role, syntax })
}

fn classification_from_kind(kind: &str) -> LeafClassification {
    if kind.contains("comment") {
        return LeafClassification {
            role: LeafRole::Scaffolding,
            syntax: SyntaxClass::Comment,
        };
    }
    if kind.contains("string") || kind.contains("char") {
        return LeafClassification {
            role: LeafRole::Payload,
            syntax: SyntaxClass::String,
        };
    }
    if kind.contains("number")
        || kind.contains("integer")
        || kind.contains("float")
        || matches!(kind, "true" | "false" | "null" | "undefined")
    {
        return LeafClassification {
            role: LeafRole::Payload,
            syntax: SyntaxClass::Literal,
        };
    }
    if kind.contains("type") {
        return LeafClassification {
            role: LeafRole::Scaffolding,
            syntax: SyntaxClass::Type,
        };
    }
    if kind.contains("identifier") || kind.ends_with("_name") {
        return LeafClassification {
            role: LeafRole::Identifier,
            syntax: SyntaxClass::Identifier,
        };
    }
    if kind
        .chars()
        .any(|character| character.is_ascii_punctuation())
    {
        return LeafClassification {
            role: LeafRole::Scaffolding,
            syntax: SyntaxClass::Punctuation,
        };
    }
    LeafClassification {
        role: LeafRole::Payload,
        syntax: SyntaxClass::Plain,
    }
}
