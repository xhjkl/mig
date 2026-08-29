use super::{
    ChildSlot, ContentChannel, LayoutOwnership, Leaf, LeafRole, NodeId, ReviewUnit,
    SiblingMatching, SyntaxKind, SyntaxNode, SyntaxTree, WrapperBoundary,
};
use crate::diff::SyntaxClass;
use crate::diff::source::Source;

/// Lower source to a degenerate tree whose file root owns one exact leaf per physical line.
pub fn lower(source: Source<'_>) -> SyntaxTree<'_> {
    let root = NodeId::new(0);
    let root_lines = source
        .line_coverage(0..source.as_str().len())
        .expect("the full source is valid geometry");
    let mut nodes = vec![SyntaxNode {
        kind: SyntaxKind::File,
        slot: ChildSlot::Positional,
        bytes: 0..source.as_str().len(),
        source_envelope: 0..source.as_str().len(),
        lines: root_lines,
        parent: None,
        children: Vec::with_capacity(source.lines().len()),
        leaf: None,
        identity: None,
        decoration_owner: None,
        sibling_matching: SiblingMatching::LocalIdentity,
        wrapper_boundary: WrapperBoundary::Sealed,
        review: None,
        named: true,
        extra: false,
        missing: false,
    }];

    for line in source.lines() {
        let id = NodeId::new(nodes.len());
        nodes[root.index()].children.push(id);
        nodes.push(SyntaxNode {
            kind: SyntaxKind::Line,
            slot: ChildSlot::Positional,
            bytes: line.full_bytes.clone(),
            source_envelope: line.full_bytes.clone(),
            lines: line.number..line.number + 1,
            parent: Some(root),
            children: Vec::new(),
            leaf: Some(Leaf {
                role: LeafRole::Payload,
                syntax: SyntaxClass::Plain,
                channel: ContentChannel::Opaque,
                delimiter: None,
            }),
            identity: Some(line.full_bytes.clone()),
            decoration_owner: None,
            sibling_matching: SiblingMatching::OrderedSyntax,
            wrapper_boundary: WrapperBoundary::Traversable,
            review: Some(ReviewUnit::linewise(LayoutOwnership::None)),
            named: true,
            extra: false,
            missing: false,
        });
    }

    SyntaxTree::from_nodes(source, None, root, nodes)
}
