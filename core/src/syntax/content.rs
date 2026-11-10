//! Evidence that text survived an edit, preserving token boundaries and literal newlines.

use super::{ChildSlot, ContentChannel, NodeId, Symbol, SyntaxTree};
use std::ops::Range;

#[derive(Eq, PartialEq, Hash)]
pub(crate) struct Token<'source> {
    symbol: Symbol,
    channel: ContentChannel,
    spelling: &'source str,
}

#[derive(Eq, PartialEq)]
enum Evidence<'source> {
    Open(Symbol, ChildSlot, Option<&'source str>),
    Token(Token<'source>),
    Close,
}

/// Compare tokens without their surrounding owners, allowing intact constructs to relocate.
pub fn same_tokens(
    before: &SyntaxTree<'_>,
    before_lines: &[usize],
    after: &SyntaxTree<'_>,
    after_lines: &[usize],
) -> bool {
    tokens(before, before_lines) == tokens(after, after_lines)
}

pub(crate) fn tokens<'source>(tree: &SyntaxTree<'source>, lines: &[usize]) -> Vec<Token<'source>> {
    fragments(tree, lines)
        .into_iter()
        .map(|(id, bytes)| token(tree, id, bytes))
        .collect()
}

pub(crate) fn tokens_in_bytes<'source>(
    tree: &SyntaxTree<'source>,
    bytes: Range<usize>,
) -> Vec<Token<'source>> {
    tree.leaf_ids_in(bytes.clone())
        .filter_map(|id| {
            let node = tree.node(id);
            if node.leaf()?.channel == ContentChannel::Layout {
                return None;
            }
            let clipped = node.bytes.start.max(bytes.start)..node.bytes.end.min(bytes.end);
            (!clipped.is_empty()).then(|| token(tree, id, clipped))
        })
        .collect()
}

/// Prove that only layout changed, retaining syntax ownership as well as token spelling.
pub fn same_layout(
    before: &SyntaxTree<'_>,
    before_lines: &[usize],
    after: &SyntaxTree<'_>,
    after_lines: &[usize],
) -> bool {
    same_layout_under(
        before,
        before_lines,
        before.root,
        after,
        after_lines,
        after.root,
    )
}

/// Compare layout within corresponding owners, allowing an explicit wrap or unwrap.
pub fn same_layout_under(
    before: &SyntaxTree<'_>,
    before_lines: &[usize],
    before_scope: NodeId,
    after: &SyntaxTree<'_>,
    after_lines: &[usize],
    after_scope: NodeId,
) -> bool {
    projection(before, before_lines, before_scope) == projection(after, after_lines, after_scope)
}

fn token<'source>(tree: &SyntaxTree<'source>, id: NodeId, bytes: Range<usize>) -> Token<'source> {
    let node = tree.node(id);
    Token {
        symbol: node.symbol,
        channel: node.leaf().expect("content fragments are leaves").channel,
        spelling: tree
            .source
            .slice(bytes)
            .expect("content stays within the source"),
    }
}

/// Retain full line bytes while clipping leaves; terminators inside literals are content.
fn fragments(tree: &SyntaxTree<'_>, lines: &[usize]) -> Vec<(NodeId, Range<usize>)> {
    let mut ranges = Vec::<Range<usize>>::new();
    for line in lines {
        let bytes = tree.source.lines()[*line].full_bytes.clone();
        if let Some(previous) = ranges.last_mut()
            && previous.end == bytes.start
        {
            previous.end = bytes.end;
        } else {
            ranges.push(bytes);
        }
    }
    let mut fragments = Vec::new();
    for bytes in ranges {
        for id in tree.leaf_ids_in(bytes.clone()) {
            let node = tree.node(id);
            if node
                .leaf()
                .is_some_and(|leaf| leaf.channel == ContentChannel::Layout)
            {
                continue;
            }
            let clipped = node.bytes.start.max(bytes.start)..node.bytes.end.min(bytes.end);
            if !clipped.is_empty() {
                fragments.push((id, clipped));
            }
        }
    }
    fragments
}

fn projection<'source>(
    tree: &SyntaxTree<'source>,
    lines: &[usize],
    scope: NodeId,
) -> Vec<Evidence<'source>> {
    let mut evidence = Vec::new();
    let mut open = Vec::<NodeId>::new();
    for (id, bytes) in fragments(tree, lines) {
        let mut ancestors = Vec::new();
        let mut parent = tree.node(id).parent;
        while let Some(id) = parent {
            ancestors.push(id);
            if id == scope {
                break;
            }
            parent = tree.node(id).parent;
        }
        ancestors.reverse();
        let shared = open
            .iter()
            .zip(&ancestors)
            .take_while(|(left, right)| left == right)
            .count();
        evidence.extend((shared..open.len()).map(|_| Evidence::Close));
        for id in &ancestors[shared..] {
            let node = tree.node(*id);
            let slot = if *id == scope {
                ChildSlot::Positional
            } else {
                node.slot
            };
            evidence.push(Evidence::Open(node.symbol, slot, tree.identity_text(*id)));
        }
        open = ancestors;
        evidence.push(Evidence::Token(token(tree, id, bytes)));
    }
    evidence.extend(open.iter().map(|_| Evidence::Close));
    evidence
}
