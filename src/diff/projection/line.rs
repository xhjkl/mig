use super::{
    ContentChannel, FallbackReason, Frame, Language, Leaf, NodeId, Projection, ProjectionHealth,
    ReviewTreatment, ReviewUnit, SyntaxNode,
};
use crate::diff::SyntaxClass;
use crate::diff::source::Source;

/// Degenerate CST whose file root owns one exact leaf per physical source line.
pub(super) fn project<'source>(
    source: Source<'source>,
    reason: FallbackReason,
) -> Projection<'source> {
    let root = NodeId::new(0);
    let root_lines = source
        .line_coverage(0..source.as_str().len())
        .expect("the full source is valid geometry");
    let mut nodes = vec![SyntaxNode {
        kind: "file",
        field: None,
        bytes: 0..source.as_str().len(),
        lines: root_lines,
        parent: None,
        children: Vec::with_capacity(source.lines().len()),
        leaf: None,
        identity: None,
        review: Some(ReviewUnit::ignored(ReviewTreatment::Linewise)),
        named: true,
        extra: false,
        missing: false,
    }];

    for line in source.lines() {
        let id = NodeId::new(nodes.len());
        nodes[root.index()].children.push(id);
        nodes.push(SyntaxNode {
            kind: "line",
            field: None,
            bytes: line.full_bytes.clone(),
            lines: line.number..line.number + 1,
            parent: Some(root),
            children: Vec::new(),
            leaf: Some(Leaf {
                syntax: SyntaxClass::Plain,
                channel: ContentChannel::Opaque,
            }),
            identity: Some(line.full_bytes.clone()),
            review: Some(ReviewUnit::stationary(
                ReviewTreatment::Linewise,
                Frame::None,
            )),
            named: true,
            extra: false,
            missing: false,
        });
    }

    Projection::from_nodes(
        source,
        Language::Lines,
        ProjectionHealth::Fallback(reason),
        root,
        nodes,
    )
}
