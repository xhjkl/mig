use super::parse::{ParsedFile, tree_sitter_language};
use super::{
    ChildSlot, ContentChannel, Declares, Delimiter, Grammar, GrammarField, GrammarSymbol, Leaf,
    LeafRole, NodeContents, NodeId, ReviewRole, SiblingOrder, Symbol, SyntaxNode, SyntaxTree,
    WrapperBoundary, c, cpp, css, go, html, json, nix, python, rust, toml, typescript,
};
use crate::Highlight;
use crate::source::Source;
use ::tree_sitter::{Language as TreeSitterLanguage, Node, Query, QueryCursor, StreamingIterator};
use anyhow::{Context, anyhow};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::OnceLock;

/// Decoration relationship resolved after lowering, when all possible owners are known.
#[derive(Clone, Copy, Default)]
pub enum DecorationHint {
    #[default]
    None,
    /// Next sibling declared to accept decorations, allowing intervening layout and comments.
    FollowingSibling,
    /// Nearest enclosing node declared to accept decorations.
    EnclosingOwner,
}

/// Source text retained as parsed syntax, an opaque token, or an extended token.
#[derive(Default)]
pub enum TextTreatment {
    #[default]
    Parsed,
    Verbatim(ContentChannel),
    Extended {
        channel: ContentChannel,
        extent: Range<usize>,
    },
}

/// Language-specific decisions retained when parser nodes become neutral syntax.
#[derive(Default)]
pub struct NodeAnnotation {
    pub review: Option<ReviewRole>,
    pub text: TextTreatment,
    pub identity: Option<Range<usize>>,
    pub decoration: DecorationHint,
    pub owns_decorations: bool,
    pub sibling_order: SiblingOrder,
    pub wrapper_boundary: WrapperBoundary,
    /// Named construct other code can refer to; see `SyntaxNode::declares`.
    pub declares: Declares,
}

/// Upstream highlight queries with a shared cache of either compiled queries or their error.
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
                    query.with_context(|| format!("invalid highlight query for {grammar:?}"))
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

/// Lower parser nodes into neutral syntax while retaining source omitted by the grammar.
/// Return `None` when parser recovery cannot be reconciled with the source.
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
        Grammar::Cpp => &cpp::HIGHLIGHT_QUERIES,
        Grammar::Rust => &rust::HIGHLIGHT_QUERIES,
        Grammar::Python => &python::HIGHLIGHT_QUERIES,
        Grammar::Go => &go::HIGHLIGHT_QUERIES,
        Grammar::Json => &json::HIGHLIGHT_QUERIES,
        Grammar::Toml => &toml::HIGHLIGHT_QUERIES,
        Grammar::Nix => &nix::HIGHLIGHT_QUERIES,
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
        Grammar::Cpp => cpp::annotate(node, parent_kind),
        Grammar::Rust => rust::annotate(node, parent_kind, source),
        Grammar::Python => python::annotate(node, parent_kind),
        Grammar::Go => go::annotate(node, parent_kind),
        Grammar::Json => json::annotate(node),
        Grammar::Toml => toml::annotate(node, parent_kind),
        Grammar::Nix => nix::annotate(node),
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
        node.symbol == error
            && node
                .parent
                .is_none_or(|parent| nodes[parent.index()].symbol != error)
    })
}

pub fn grammar_symbol(grammar: Grammar, kind: &str, named: bool) -> Symbol {
    let language = tree_sitter_language(grammar);
    Symbol::Grammar(GrammarSymbol::new(
        language.id_for_node_kind(kind, named),
        named,
    ))
}

fn gap_channel(grammar: Grammar, parent_kind: &str, spelling: &str) -> ContentChannel {
    if !spelling.chars().all(char::is_whitespace) {
        return ContentChannel::Syntax;
    }
    let syntactic = match grammar {
        Grammar::C => c::whitespace_is_syntax(parent_kind),
        Grammar::Cpp => cpp::whitespace_is_syntax(parent_kind),
        Grammar::Rust => rust::whitespace_is_syntax(parent_kind),
        Grammar::Python => python::whitespace_is_syntax(parent_kind),
        Grammar::Go => go::whitespace_is_syntax(parent_kind),
        Grammar::Json => matches!(parent_kind, "string" | "string_content"),
        Grammar::Toml => matches!(parent_kind, "string" | "quoted_key"),
        Grammar::Nix => matches!(
            parent_kind,
            "string_expression" | "indented_string_expression" | "string_fragment"
        ),
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
        /// Parser kind of the enclosing node, carried down to avoid root-to-node parent walks.
        parent_kind: Option<&'static str>,
        slot: ChildSlot,
        inherited_classification: Option<LeafClassification>,
    },
    Fragment {
        bytes: Range<usize>,
        parent: NodeId,
        leaf: Leaf,
    },
}

#[derive(Clone, Copy)]
struct LeafClassification {
    role: LeafRole,
    highlight: Highlight,
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
        parent_kind: None,
        slot: ChildSlot::Positional,
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

        let (node, parent, parent_kind, slot, inherited_classification) = match pending_item {
            Pending::Fragment {
                bytes,
                parent,
                leaf,
            } => {
                let id = NodeId::new(nodes.len());
                nodes[parent.index()].children_mut().push(id);
                let lines = source
                    .line_coverage(bytes.clone())
                    .expect("source fragments remain inside source geometry");
                nodes.push(SyntaxNode {
                    symbol: Symbol::SourceFragment,
                    slot: ChildSlot::Positional,
                    source_envelope: bytes.clone(),
                    bytes,
                    lines,
                    parent: Some(parent),
                    contents: NodeContents::Token(leaf),
                    identity: None,
                    decoration_owner: None,
                    sibling_order: SiblingOrder::SourceOrder,
                    wrapper_boundary: WrapperBoundary::Traversable,
                    declares: Declares::Nothing,
                    review: None,
                });
                decoration_hints.push(DecorationHint::None);
                decoration_owners.push(false);
                continue;
            }
            Pending::Parser {
                node,
                parent,
                parent_kind,
                slot,
                inherited_classification,
            } => (node, parent, parent_kind, slot, inherited_classification),
        };

        if node.is_missing() {
            return None;
        }
        let annotation = annotate_node(grammar, node, parent_kind, source.as_str());
        let parser_bytes = node.byte_range();
        let (bytes, token_channel) = match &annotation.text {
            TextTreatment::Parsed => (parser_bytes.clone(), None),
            TextTreatment::Verbatim(channel) => (parser_bytes.clone(), Some(*channel)),
            TextTreatment::Extended { channel, extent } => (extent.clone(), Some(*channel)),
        };
        let leaf_channel = token_channel.unwrap_or(ContentChannel::Syntax);
        let inherited_classification = highlights
            .get(&node.id())
            .copied()
            .or(inherited_classification);

        let id = NodeId::new(nodes.len());
        if let Some(parent) = parent {
            nodes[parent.index()].children_mut().push(id);
        }
        if bytes.end > parser_bytes.end {
            // Absorbing later parser siblings; plaintext treats their apparent tags as text.
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
        let leaf = (token_channel.is_some() || node.child_count() == 0).then(|| {
            let spelling = source
                .slice(bytes.clone())
                .expect("tree-sitter leaves remain inside source geometry");
            let classification =
                leaf_classification(node.kind(), leaf_channel, inherited_classification);
            Leaf {
                role: classification.role,
                highlight: classification.highlight,
                channel: leaf_channel,
                // Preserving named punctuation as syntax; its grammar role takes precedence over spelling.
                delimiter: if node.is_named() {
                    None
                } else {
                    delimiter(spelling)
                },
            }
        });
        let source_envelope = bytes.clone();
        nodes.push(SyntaxNode {
            symbol: Symbol::Grammar(GrammarSymbol::new(node.kind_id(), node.is_named())),
            slot,
            bytes,
            source_envelope,
            lines,
            parent,
            contents: match leaf {
                Some(leaf) => NodeContents::Token(leaf),
                None => NodeContents::Branch(Vec::with_capacity(node.child_count())),
            },
            identity: annotation.identity,
            decoration_owner: None,
            sibling_order: annotation.sibling_order,
            wrapper_boundary: annotation.wrapper_boundary,
            declares: annotation.declares,
            review: annotation.review,
        });
        decoration_hints.push(annotation.decoration);
        decoration_owners.push(annotation.owns_decorations);

        if token_channel.is_some() || node.child_count() == 0 {
            continue;
        }
        let mut lowered_children = Vec::with_capacity(node.child_count() * 2 + 1);
        let mut cursor = parser_bytes.start;
        let mut walker = node.walk();
        let kind = node.kind();
        if walker.goto_first_child() {
            loop {
                let child = walker.node();
                if child.start_byte() > cursor {
                    lowered_children.push(fragment_pending(
                        source,
                        grammar,
                        node,
                        cursor..child.start_byte(),
                        id,
                        inherited_classification,
                    ));
                }
                lowered_children.push(Pending::Parser {
                    node: child,
                    parent: Some(id),
                    parent_kind: Some(kind),
                    slot: child_slot(tree_sitter, walker.field_name()),
                    inherited_classification,
                });
                cursor = cursor.max(child.end_byte());
                if !walker.goto_next_sibling() {
                    break;
                }
            }
        }
        if cursor < parser_bytes.end {
            lowered_children.push(fragment_pending(
                source,
                grammar,
                node,
                cursor..parser_bytes.end,
                id,
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

/// Link decorations to their owners while keeping each source range independently reviewable.
fn resolve_decoration_owners(nodes: &mut [SyntaxNode], hints: &[DecorationHint], owners: &[bool]) {
    debug_assert_eq!(nodes.len(), hints.len());
    debug_assert_eq!(nodes.len(), owners.len());

    for parent_index in 0..nodes.len() {
        let parent = NodeId::new(parent_index);
        let mut following_owner = None;
        for position in (0..nodes[parent_index].children().len()).rev() {
            let child = nodes[parent_index].children()[position];
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

/// Allow a decoration to reach its owner across punctuation, layout, and independent comments.
fn decoration_transparent(node: &SyntaxNode) -> bool {
    !node.is_named()
        || node.leaf().is_some_and(|leaf| {
            matches!(
                leaf.channel,
                ContentChannel::Comment | ContentChannel::Layout
            )
        })
}

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
    inherited_classification: Option<LeafClassification>,
) -> Pending<'tree> {
    let spelling = source
        .slice(bytes.clone())
        .expect("source fragments remain inside source geometry");
    let channel = gap_channel(grammar, parent.kind(), spelling);
    let classification = leaf_classification(parent.kind(), channel, inherited_classification);
    Pending::Fragment {
        bytes,
        parent: parent_id,
        leaf: Leaf {
            role: classification.role,
            highlight: classification.highlight,
            channel,
            delimiter: delimiter(spelling),
        },
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
    let (role, highlight) = match channel {
        ContentChannel::Comment => (LeafRole::Scaffolding, Highlight::Comment),
        ContentChannel::Opaque => (LeafRole::Payload, Highlight::Plain),
        ContentChannel::Layout => (LeafRole::Scaffolding, Highlight::Plain),
        ContentChannel::Syntax => {
            return inherited.unwrap_or_else(|| classification_from_kind(kind));
        }
    };
    LeafClassification { role, highlight }
}

fn capture_classification(capture: &str) -> Option<LeafClassification> {
    let stem = capture.split('.').next().unwrap_or(capture);
    let (role, highlight) = match stem {
        "comment" => (LeafRole::Scaffolding, Highlight::Comment),
        "string" | "character" => (LeafRole::Payload, Highlight::String),
        "constant" | "number" | "boolean" => (LeafRole::Payload, Highlight::Literal),
        "keyword" | "conditional" | "repeat" | "exception" | "include" => {
            (LeafRole::Scaffolding, Highlight::Keyword)
        }
        "type" | "constructor" => (LeafRole::Scaffolding, Highlight::Type),
        "variable" | "function" | "method" | "property" | "field" | "parameter" | "module"
        | "namespace" | "label" | "tag" | "attribute" => {
            (LeafRole::Identifier, Highlight::Identifier)
        }
        "operator" | "punctuation" => (LeafRole::Scaffolding, Highlight::Punctuation),
        _ => return None,
    };
    Some(LeafClassification { role, highlight })
}

fn classification_from_kind(kind: &str) -> LeafClassification {
    let (role, highlight) = if kind.contains("comment") {
        (LeafRole::Scaffolding, Highlight::Comment)
    } else if kind.contains("string") || kind.contains("char") {
        (LeafRole::Payload, Highlight::String)
    } else if kind.contains("number")
        || kind.contains("integer")
        || kind.contains("float")
        || matches!(kind, "true" | "false" | "null" | "undefined")
    {
        (LeafRole::Payload, Highlight::Literal)
    } else if kind.contains("type") {
        (LeafRole::Scaffolding, Highlight::Type)
    } else if kind.contains("identifier") || kind.ends_with("_name") {
        (LeafRole::Identifier, Highlight::Identifier)
    } else if kind
        .chars()
        .any(|character| character.is_ascii_punctuation())
    {
        (LeafRole::Scaffolding, Highlight::Punctuation)
    } else {
        (LeafRole::Payload, Highlight::Plain)
    };
    LeafClassification { role, highlight }
}
