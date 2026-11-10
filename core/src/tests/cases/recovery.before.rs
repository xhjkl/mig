use super::parse::{ParsedFile, tree_sitter_language};
use super::{
    ChildSlot, ContentChannel, Delimiter, Grammar, GrammarField, GrammarSymbol, Leaf, LeafRole,
    NodeId, ReviewUnit, SiblingMatching, SyntaxKind, SyntaxNode, SyntaxTree, WrapperBoundary, c,
    css, html, rust, typescript,
};
use crate::diff::SyntaxClass;
use crate::diff::source::Source;
use ::tree_sitter::{Language as TreeSitterLanguage, Node, Query, QueryCursor, StreamingIterator};
use anyhow::{Context, anyhow};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::OnceLock;

/// Frontend instruction resolved to one concrete decoration-owner edge after lowering.
#[derive(Clone, Copy, Default)]
pub enum DecorationHint {
    #[default]
    None,
    /// Decorates the next frontend-declared owner in this sibling scope.
    FollowingSibling,
    /// Decorates the nearest frontend-declared enclosing owner.
    EnclosingOwner,
}

/// Language decision for one parser node before its parser handle is discarded.
#[derive(Default)]
pub struct NodeAnnotation {
    pub review: Option<ReviewUnit>,
    pub channel: Option<ContentChannel>,
    pub descendant_channel: Option<ContentChannel>,
    /// Language-specific spelling used as a graph identity, independent of review units.
    pub identity: Option<Range<usize>>,
    /// Source-language decoration relationship; resolved only after all siblings exist.
    pub decoration: DecorationHint,
    /// Whether this semantic boundary may own decorations.
    pub owns_decorations: bool,
    pub sibling_matching: SiblingMatching,
    pub wrapper_boundary: WrapperBoundary,
    /// Language-owned extent when the grammar stops before the semantic content does.
    pub extent: Option<Range<usize>>,
    /// Treat the node's complete byte range as one payload and discard parser children.
    pub prune_children: bool,
}

/// Official query sources and their one process-wide compilation for one grammar.
pub struct HighlightQueries {
    sources: &'static [&'static str],
    compiled: OnceLock<Result<Box<[Query]>, String>>,
}

impl HighlightQueries {
    pub const fn new(sources: &'static [&'static str]) -> Self {
        Self {
            sources,
            compiled: OnceLock::new(),
        }
    }

    fn compiled(
        &'static self,
        language: &TreeSitterLanguage,
        grammar: Grammar,
    ) -> anyhow::Result<&'static [Query]> {
        let queries = self.compiled.get_or_init(|| {
            self.sources
                .iter()
                .map(|source| {
                    let query = Query::new(language, source);
                    query.with_context(|| {
                        format!("invalid official highlight query for {grammar:?}")
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

/// Lower parser-owned syntax into the internal language-neutral source arena.
pub fn lower<'source>(parsed: ParsedFile<'source>) -> anyhow::Result<Option<SyntaxTree<'source>>> {
    let source = parsed.source;
    let grammar = parsed.grammar;
    let language = tree_sitter_language(grammar);
    let root = parsed.tree.root_node();
    let highlight_queries = highlight_queries(grammar).compiled(&language, grammar)?;
    let highlights = collect_highlights(highlight_queries, root, source.as_str().as_bytes());
    let Some(nodes) = lower_nodes(&source, root, grammar, &language, &highlights) else {
        return Ok(None);
    };
    Ok(Some(SyntaxTree::from_nodes(
        source,
        Some(grammar),
        NodeId::new(0),
        nodes,
    )))
}

fn highlight_queries(grammar: Grammar) -> &'static HighlightQueries {
    match grammar {
        Grammar::C => &c::HIGHLIGHT_QUERIES,
        Grammar::Rust => &rust::HIGHLIGHT_QUERIES,
        Grammar::Html => &html::HIGHLIGHT_QUERIES,
        Grammar::Css => &css::HIGHLIGHT_QUERIES,
        Grammar::TypeScript => &typescript::TYPESCRIPT_HIGHLIGHTS,
        Grammar::Tsx => &typescript::TSX_HIGHLIGHTS,
        Grammar::JavaScript => &typescript::JAVASCRIPT_HIGHLIGHTS,
        Grammar::Jsx => &typescript::JSX_HIGHLIGHTS,
    }
}

fn annotate_node(
    grammar: Grammar,
    node: Node<'_>,
    parent_kind: Option<&'static str>,
    source: &str,
) -> NodeAnnotation {
    match grammar {
        Grammar::C => c::annotate(node, parent_kind),
        Grammar::Rust => rust::annotate(node, parent_kind, source),
        Grammar::Html => html::annotate(node, source),
        Grammar::Css => css::annotate(node, parent_kind, source),
        Grammar::TypeScript | Grammar::Tsx | Grammar::JavaScript | Grammar::Jsx => {
            typescript::annotate(node, parent_kind)
        }
    }
}

fn normalize_recovery(grammar: Grammar, source: &Source<'_>, nodes: &mut [SyntaxNode]) -> bool {
    if grammar == Grammar::Html {
        return html::normalize_recovery(source, nodes);
    }
    let error = grammar_symbol(grammar, "ERROR", true);
    !nodes.iter().any(|node| {
        node.missing
            || (node.kind == error
                && node
                    .parent
                    .is_none_or(|parent| nodes[parent.index()].kind != error))
    })
}

pub fn grammar_symbol(grammar: Grammar, kind: &str, named: bool) -> SyntaxKind {
    let language = tree_sitter_language(grammar);
    SyntaxKind::Grammar(GrammarSymbol(language.id_for_node_kind(kind, named)))
}

fn gap_channel(grammar: Grammar, parent_kind: &str, spelling: &str) -> ContentChannel {
    if !spelling.chars().all(char::is_whitespace) {
        return ContentChannel::Syntax;
    }
    let syntactic = match grammar {
        Grammar::C => c::whitespace_is_syntax(parent_kind),
        Grammar::Rust => rust::whitespace_is_syntax(parent_kind),
        Grammar::Html => html::whitespace_is_syntax(parent_kind, spelling),
        Grammar::Css => css::whitespace_is_syntax(parent_kind),
        Grammar::TypeScript | Grammar::Tsx | Grammar::JavaScript | Grammar::Jsx => {
            typescript::whitespace_is_syntax(parent_kind)
        }
    };
    if syntactic {
        ContentChannel::Syntax
    } else {
        ContentChannel::Layout
    }
}

enum Pending<'tree> {
    Parser {
        node: Node<'tree>,
        parent: Option<NodeId>,
        slot: ChildSlot,
        inherited_channel: Option<ContentChannel>,
        inherited_classification: Option<LeafClassification>,
    },
    Fragment {
        bytes: Range<usize>,
        parent: NodeId,
        channel: ContentChannel,
        classification: LeafClassification,
        delimiter: Option<Delimiter>,
    },
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
    grammar: Grammar,
    tree_sitter: &TreeSitterLanguage,
    highlights: &HashMap<usize, LeafClassification>,
) -> Option<Vec<SyntaxNode>> {
    let mut nodes = Vec::<SyntaxNode>::new();
    let mut decoration_hints = Vec::<DecorationHint>::new();
    let mut decoration_owners = Vec::<bool>::new();
    let mut pending = vec![Pending::Parser {
        node: root,
        parent: None,
        slot: ChildSlot::Positional,
        inherited_channel: None,
        inherited_classification: None,
    }];
    let mut consumed_until = 0;

    while let Some(pending_item) = pending.pop() {
        let start_byte = match &pending_item {
            Pending::Parser { node, .. } => node.start_byte(),
            Pending::Fragment { bytes, .. } => bytes.start,
        };
        if start_byte < consumed_until {
            continue;
        }

        let (node, parent, slot, inherited_channel, inherited_classification) = match pending_item {
            Pending::Fragment {
                bytes,
                parent,
                channel,
                classification,
                delimiter,
            } => {
                let id = NodeId::new(nodes.len());
                nodes[parent.index()].children.push(id);
                let lines = source
                    .line_coverage(bytes.clone())
                    .expect("source fragments remain inside source geometry");
                nodes.push(SyntaxNode {
                    kind: SyntaxKind::SourceFragment,
                    slot: ChildSlot::Positional,
                    source_envelope: bytes.clone(),
                    bytes,
                    lines,
                    parent: Some(parent),
                    children: Vec::new(),
                    leaf: Some(Leaf {
                        role: classification.role,
                        syntax: classification.syntax,
                        channel,
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
            Pending::Parser {
                node,
                parent,
                slot,
                inherited_channel,
                inherited_classification,
            } => (
                node,
                parent,
                slot,
                inherited_channel,
                inherited_classification,
            ),
        };

        let parent_kind = node.parent().map(|parent| parent.kind());
        let annotation = annotate_node(grammar, node, parent_kind, source.as_str());
        let child_channel = annotation.descendant_channel.or(inherited_channel);
        let leaf_channel = annotation
            .channel
            .or(inherited_channel)
            .unwrap_or(ContentChannel::Syntax);
        let inherited_classification = highlights
            .get(&node.id())
            .copied()
            .or(inherited_classification);

        let id = NodeId::new(nodes.len());
        if let Some(parent) = parent {
            nodes[parent.index()].children.push(id);
        }
        let parser_bytes = node.byte_range();
        let bytes = annotation.extent.clone().unwrap_or(parser_bytes.clone());
        if bytes.end > parser_bytes.end {
            // Plaintext-style constructs absorb every parser sibling through their extent.
            consumed_until = consumed_until.max(bytes.end);
            let mut ancestor = parent;
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
            kind: SyntaxKind::Grammar(GrammarSymbol(node.kind_id())),
            slot,
            bytes,
            source_envelope,
            lines,
            parent,
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
                    grammar,
                    node,
                    cursor..child.start_byte(),
                    id,
                    child_channel,
                    inherited_classification,
                ));
            }
            lowered_children.push(Pending::Parser {
                node: child,
                parent: Some(id),
                slot: child_slot(tree_sitter, node.field_name_for_child(index as u32)),
                inherited_channel: child_channel,
                inherited_classification,
            });
            cursor = cursor.max(child.end_byte());
        }
        if cursor < parser_bytes.end {
            lowered_children.push(fragment_pending(
                source,
                grammar,
                node,
                cursor..parser_bytes.end,
                id,
                child_channel,
                inherited_classification,
            ));
        }
        pending.extend(lowered_children.into_iter().rev());
    }
    if !normalize_recovery(grammar, source, &mut nodes) {
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
    match spelling {
        "," | ";" => Some(Delimiter::Trailing(None)),
        _ if spelling
            .chars()
            .all(|character| character.is_whitespace() || character.is_ascii_punctuation()) =>
        {
            Some(Delimiter::Structural)
        }
        _ => None,
    }
}

fn child_slot(grammar: &TreeSitterLanguage, field: Option<&str>) -> ChildSlot {
    let Some(field) = field else {
        return ChildSlot::Positional;
    };
    let field = grammar
        .field_id_for_name(field)
        .expect("tree-sitter child fields belong to their grammar");
    ChildSlot::Field(GrammarField(field))
}

fn fragment_pending<'tree>(
    source: &Source<'_>,
    grammar: Grammar,
    parent: Node<'tree>,
    bytes: Range<usize>,
    parent_id: NodeId,
    inherited_channel: Option<ContentChannel>,
    inherited_classification: Option<LeafClassification>,
) -> Pending<'tree> {
    let spelling = source
        .slice(bytes.clone())
        .expect("source fragments remain inside source geometry");
    let channel =
        inherited_channel.unwrap_or_else(|| gap_channel(grammar, parent.kind(), spelling));
    let classification = leaf_classification(parent.kind(), channel, inherited_classification);
    let delimiter = delimiter(spelling);
    Pending::Fragment {
        bytes,
        parent: parent_id,
        channel,
        classification,
        delimiter,
    }
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
