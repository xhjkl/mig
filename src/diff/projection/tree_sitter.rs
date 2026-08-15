use super::{
    ContentChannel, Language, Leaf, NodeId, Projection, ProjectionHealth, ReviewUnit, SyntaxNode,
};
use crate::diff::SyntaxClass;
use crate::diff::source::Source;
use ::tree_sitter::{
    Language as TreeSitterLanguage, Node, Parser, Query, QueryCursor, StreamingIterator,
};
use anyhow::{Context, anyhow};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::OnceLock;

const MAX_SYNTAX_NODES: usize = 500_000;

/// Untrusted input falls back; frontend setup defects remain explicit errors.
pub(super) enum ProjectFailure<'source> {
    Untrusted(SourceFailure<'source>, SyntaxFailure),
    Setup(anyhow::Error),
}

#[derive(Clone, Copy)]
pub(super) enum SyntaxFailure {
    Parse,
    Complexity,
}

pub(super) struct SourceFailure<'source> {
    pub(super) source: Source<'source>,
}

/// Parser-node view available only while a language adapter annotates the neutral arena.
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

/// Language decision for one parser node before its parser handle is discarded.
#[derive(Default)]
pub(super) struct NodeAnnotation {
    pub(super) review: Option<ReviewUnit>,
    pub(super) channel: Option<ContentChannel>,
    pub(super) descendant_channel: Option<ContentChannel>,
    /// Language-specific spelling used as a graph identity, independent of review units.
    pub(super) identity: Option<Range<usize>>,
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
        projected_language: Language,
    ) -> anyhow::Result<&'static [Query]> {
        let queries = self.compiled.get_or_init(|| {
            self.sources
                .iter()
                .map(|source| {
                    let query = Query::new(language, source);
                    query.with_context(|| {
                        format!("invalid official highlight query for {projected_language:?}")
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

/// Grammar and language policy hidden behind the shared CST walker.
pub(super) trait Adapter {
    fn language(&self) -> TreeSitterLanguage;
    fn projected_language(&self) -> Language;
    fn highlight_queries(&self) -> &'static HighlightQueries;
    fn annotate(&self, context: NodeContext<'_, '_>) -> NodeAnnotation;

    fn gap_channel(&self, context: GapContext<'_, '_>) -> ContentChannel {
        context.default_channel()
    }
}

/// Parse and copy a full CST into neutral source geometry in one preorder walk.
pub(super) fn project<'source>(
    source: Source<'source>,
    adapter: &impl Adapter,
) -> std::result::Result<Projection<'source>, ProjectFailure<'source>> {
    let mut parser = Parser::new();
    let language = adapter.language();
    let language_result = parser.set_language(&language);
    if language_result.is_err() {
        let error = language_result.expect_err("checked language setup failure");
        return Err(ProjectFailure::Setup(anyhow!(error)));
    }

    let tree = parser.parse(source.as_str(), None);
    let Some(tree) = tree else {
        return Err(ProjectFailure::Setup(anyhow!(
            "tree-sitter cancelled a parse without a cancellation callback"
        )));
    };
    let root = tree.root_node();
    if root.has_error() || root.is_missing() {
        return Err(ProjectFailure::Untrusted(
            SourceFailure { source },
            SyntaxFailure::Parse,
        ));
    }
    if tree_exceeds_node_limit(root, MAX_SYNTAX_NODES) {
        return Err(ProjectFailure::Untrusted(
            SourceFailure { source },
            SyntaxFailure::Complexity,
        ));
    }

    let highlight_queries = adapter
        .highlight_queries()
        .compiled(&language, adapter.projected_language())
        .map_err(ProjectFailure::Setup)?;
    let highlights = collect_highlights(highlight_queries, root, source.as_str().as_bytes());
    let nodes = project_nodes(&source, root, adapter, &highlights);

    Ok(Projection::from_nodes(
        source,
        adapter.projected_language(),
        ProjectionHealth::Parsed,
        NodeId::new(0),
        nodes,
    ))
}

fn tree_exceeds_node_limit(root: Node<'_>, limit: usize) -> bool {
    let mut cursor = root.walk();
    let mut nodes = 0;
    loop {
        nodes += 1;
        if nodes > limit {
            return true;
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return false;
            }
        }
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
    field: Option<&'static str>,
    inherited_channel: Option<ContentChannel>,
    inherited_syntax: Option<SyntaxClass>,
}

struct PendingFragment {
    bytes: Range<usize>,
    parent: NodeId,
    channel: ContentChannel,
    syntax: SyntaxClass,
}

fn project_nodes(
    source: &Source<'_>,
    root: Node<'_>,
    adapter: &impl Adapter,
    highlights: &HashMap<usize, Highlight>,
) -> Vec<SyntaxNode> {
    let mut nodes = Vec::<SyntaxNode>::new();
    let mut pending = vec![Pending::Parser(PendingNode {
        node: root,
        parent: None,
        field: None,
        inherited_channel: None,
        inherited_syntax: None,
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
                nodes.push(SyntaxNode {
                    kind: "source_fragment",
                    field: None,
                    bytes: fragment.bytes,
                    lines,
                    parent: Some(fragment.parent),
                    children: Vec::new(),
                    leaf: Some(Leaf {
                        syntax: fragment.syntax,
                        channel: fragment.channel,
                    }),
                    identity: None,
                    review: None,
                    named: false,
                    extra: false,
                    missing: false,
                });
                continue;
            }
            Pending::Parser(pending_node) => pending_node,
        };

        let parent_kind = pending_node.parent.map(|parent| nodes[parent.index()].kind);
        let context = NodeContext {
            node: pending_node.node,
            parent_kind,
            source: source.as_str(),
        };
        let annotation = adapter.annotate(context);
        let inherited_channel = annotation
            .descendant_channel
            .or(pending_node.inherited_channel);
        let leaf_channel = annotation
            .channel
            .or(pending_node.inherited_channel)
            .unwrap_or(ContentChannel::Syntax);
        let inherited_syntax = highlights
            .get(&pending_node.node.id())
            .map(|highlight| highlight.syntax)
            .or(pending_node.inherited_syntax);

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
                ancestor_node.lines = source
                    .line_coverage(ancestor_node.bytes.clone())
                    .expect("expanded ancestors remain inside source geometry");
                ancestor = ancestor_node.parent;
            }
        }
        let lines = source
            .line_coverage(bytes.clone())
            .expect("tree-sitter nodes remain within their parsed source");
        let leaf = (annotation.prune_children || node.child_count() == 0).then(|| Leaf {
            syntax: leaf_syntax(node.kind(), leaf_channel, inherited_syntax),
            channel: leaf_channel,
        });
        nodes.push(SyntaxNode {
            kind: node.kind(),
            field: pending_node.field,
            bytes,
            lines,
            parent: pending_node.parent,
            children: Vec::with_capacity(if annotation.prune_children {
                0
            } else {
                node.child_count()
            }),
            leaf,
            identity: annotation.identity,
            review: annotation.review,
            named: node.is_named(),
            extra: node.is_extra(),
            missing: node.is_missing(),
        });

        if annotation.prune_children || node.child_count() == 0 {
            continue;
        }
        let mut projected_children = Vec::with_capacity(node.child_count() * 2 + 1);
        let mut cursor = parser_bytes.start;
        for index in 0..node.child_count() {
            let Some(child) = node.child(index as u32) else {
                continue;
            };
            if child.start_byte() > cursor {
                projected_children.push(fragment_pending(
                    source,
                    adapter,
                    node,
                    cursor..child.start_byte(),
                    id,
                    inherited_channel,
                    inherited_syntax,
                ));
            }
            projected_children.push(Pending::Parser(PendingNode {
                node: child,
                parent: Some(id),
                field: node.field_name_for_child(index as u32),
                inherited_channel,
                inherited_syntax,
            }));
            cursor = cursor.max(child.end_byte());
        }
        if cursor < parser_bytes.end {
            projected_children.push(fragment_pending(
                source,
                adapter,
                node,
                cursor..parser_bytes.end,
                id,
                inherited_channel,
                inherited_syntax,
            ));
        }
        pending.extend(projected_children.into_iter().rev());
    }
    nodes
}

fn fragment_pending<'tree>(
    source: &Source<'_>,
    adapter: &impl Adapter,
    parent: Node<'tree>,
    bytes: Range<usize>,
    parent_id: NodeId,
    inherited_channel: Option<ContentChannel>,
    inherited_syntax: Option<SyntaxClass>,
) -> Pending<'tree> {
    let context = GapContext {
        parent,
        bytes: bytes.clone(),
        source: source.as_str(),
    };
    let channel = inherited_channel.unwrap_or_else(|| adapter.gap_channel(context));
    let syntax = leaf_syntax(parent.kind(), channel, inherited_syntax);
    Pending::Fragment(PendingFragment {
        bytes,
        parent: parent_id,
        channel,
        syntax,
    })
}

#[derive(Clone, Copy)]
struct Highlight {
    syntax: SyntaxClass,
}

fn collect_highlights(
    queries: &[Query],
    root: Node<'_>,
    source: &[u8],
) -> HashMap<usize, Highlight> {
    let mut highlights = HashMap::new();
    for query in queries {
        let mut cursor = QueryCursor::new();
        let mut captures = cursor.captures(query, root, source);
        while let Some((matched, capture_index)) = captures.next() {
            let capture = matched.captures[*capture_index];
            let name = query.capture_names()[capture.index as usize];
            let syntax = capture_syntax(name);
            let Some(syntax) = syntax else {
                continue;
            };
            highlights.insert(capture.node.id(), Highlight { syntax });
        }
    }
    highlights
}

fn leaf_syntax(
    kind: &str,
    channel: ContentChannel,
    inherited_syntax: Option<SyntaxClass>,
) -> SyntaxClass {
    if channel == ContentChannel::Comment {
        return SyntaxClass::Comment;
    }
    if matches!(channel, ContentChannel::Opaque | ContentChannel::Layout) {
        return SyntaxClass::Plain;
    }

    if let Some(syntax) = inherited_syntax {
        return syntax;
    }

    syntax_from_kind(kind)
}

fn capture_syntax(capture: &str) -> Option<SyntaxClass> {
    let stem = capture.split('.').next().unwrap_or(capture);
    match stem {
        "comment" => Some(SyntaxClass::Comment),
        "string" | "character" => Some(SyntaxClass::String),
        "constant" | "number" | "boolean" => Some(SyntaxClass::Literal),
        "keyword" | "conditional" | "repeat" | "exception" | "include" => {
            Some(SyntaxClass::Keyword)
        }
        "type" | "constructor" => Some(SyntaxClass::Type),
        "variable" | "function" | "method" | "property" | "field" | "parameter" | "module"
        | "namespace" | "label" | "tag" | "attribute" => Some(SyntaxClass::Identifier),
        "operator" | "punctuation" => Some(SyntaxClass::Punctuation),
        _ => None,
    }
}

fn syntax_from_kind(kind: &str) -> SyntaxClass {
    if kind.contains("comment") {
        return SyntaxClass::Comment;
    }
    if kind.contains("string") || kind.contains("char") {
        return SyntaxClass::String;
    }
    if kind.contains("number")
        || kind.contains("integer")
        || kind.contains("float")
        || matches!(kind, "true" | "false" | "null" | "undefined")
    {
        return SyntaxClass::Literal;
    }
    if kind.contains("type") {
        return SyntaxClass::Type;
    }
    if kind.contains("identifier") || kind.ends_with("_name") {
        return SyntaxClass::Identifier;
    }
    if kind
        .chars()
        .any(|character| character.is_ascii_punctuation())
    {
        return SyntaxClass::Punctuation;
    }
    SyntaxClass::Plain
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::projection::ReviewTreatment;

    struct InvalidQueryAdapter;

    static INVALID_HIGHLIGHT_QUERIES: HighlightQueries = HighlightQueries::new(&["("]);

    impl Adapter for InvalidQueryAdapter {
        fn language(&self) -> TreeSitterLanguage {
            tree_sitter_rust::LANGUAGE.into()
        }

        fn projected_language(&self) -> Language {
            Language::Rust
        }

        fn highlight_queries(&self) -> &'static HighlightQueries {
            &INVALID_HIGHLIGHT_QUERIES
        }

        fn annotate(&self, context: NodeContext<'_, '_>) -> NodeAnnotation {
            let review = (context.node.kind() == "source_file")
                .then(|| ReviewUnit::ignored(ReviewTreatment::Linewise));
            NodeAnnotation {
                review,
                ..NodeAnnotation::default()
            }
        }
    }

    #[test]
    fn invalid_highlight_query_is_a_setup_error() {
        let result = project(Source::new("fn main() {}\n"), &InvalidQueryAdapter);

        assert!(matches!(result, Err(ProjectFailure::Setup(_))));
    }

    #[test]
    fn highlight_query_compilation_is_shared_across_projections() {
        static HIGHLIGHT_QUERIES: HighlightQueries =
            HighlightQueries::new(&[tree_sitter_rust::HIGHLIGHTS_QUERY]);
        let language: TreeSitterLanguage = tree_sitter_rust::LANGUAGE.into();

        let first = HIGHLIGHT_QUERIES
            .compiled(&language, Language::Rust)
            .unwrap();
        let second = HIGHLIGHT_QUERIES
            .compiled(&language, Language::Rust)
            .unwrap();

        assert!(std::ptr::eq(first, second));
    }

    #[test]
    fn syntax_node_budget_is_checked_before_projection_allocation() {
        let mut parser = Parser::new();
        let language: TreeSitterLanguage = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&language).unwrap();
        let tree = parser.parse("fn main() { work(); }\n", None).unwrap();

        assert!(tree_exceeds_node_limit(tree.root_node(), 2));
        assert!(!tree_exceeds_node_limit(tree.root_node(), 1_000));
    }
}
