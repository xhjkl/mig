use super::{
    ChildSlot, ContentChannel, Declares, Leaf, LeafRole, NodeContents, NodeId, ReviewRole,
    SiblingOrder, Symbol, SyntaxNode, SyntaxTree, WrapperBoundary,
};
use crate::Highlight;
use crate::source::Source;

/// Represent each physical line as an independent review unit, including its terminator.
pub fn lower(source: Source<'_>) -> SyntaxTree<'_> {
    let root = NodeId::new(0);
    let root_lines = source
        .line_coverage(0..source.as_str().len())
        .expect("the full source is valid geometry");
    let mut nodes = vec![SyntaxNode {
        symbol: Symbol::File,
        slot: ChildSlot::Positional,
        bytes: 0..source.as_str().len(),
        source_envelope: 0..source.as_str().len(),
        lines: root_lines,
        parent: None,
        contents: NodeContents::Branch(Vec::with_capacity(source.lines().len())),
        identity: None,
        decoration_owner: None,
        sibling_order: SiblingOrder::ByIdentity,
        wrapper_boundary: WrapperBoundary::Sealed,
        declares: Declares::Nothing,
        review: None,
    }];

    for line in source.lines() {
        let id = NodeId::new(nodes.len());
        nodes[root.index()].children_mut().push(id);
        nodes.push(SyntaxNode {
            symbol: Symbol::Line,
            slot: ChildSlot::Positional,
            bytes: line.full_bytes.clone(),
            source_envelope: line.full_bytes.clone(),
            lines: super::super::LineSpan::single(line.number),
            parent: Some(root),
            contents: NodeContents::Token(Leaf {
                role: LeafRole::Payload,
                highlight: Highlight::Plain,
                channel: ContentChannel::Opaque,
                delimiter: None,
            }),
            identity: Some(line.full_bytes.clone()),
            decoration_owner: None,
            sibling_order: SiblingOrder::SourceOrder,
            wrapper_boundary: WrapperBoundary::Traversable,
            declares: Declares::Nothing,
            review: Some(ReviewRole::Content),
        });
    }

    SyntaxTree::from_nodes(source, None, root, nodes)
}
