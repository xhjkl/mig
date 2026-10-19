//! Which declarations a change touches, and what a file refers to.
//!
//! Review ranking across files uses these facts; the diff itself does not.

use super::SyntaxClass;
use super::refine::RefinedHunk;
use super::syntax::{Declares, LeafRole, SyntaxPair, SyntaxTree};
use super::tree_diff::SourceChange;
use std::collections::HashSet;
use std::ops::Range;

/// A named construct and the lines of its heading, before any body.
struct Declaration {
    symbol: String,
    declares: Declares,
    lines: Range<usize>,
    head: Range<usize>,
    parent: Option<usize>,
}

impl Declaration {
    /// Definitions carry a name other files can be said to use.
    /// Members are too generic to attribute on their own; they credit their owner.
    fn attributable(&self) -> bool {
        self.declares == Declares::Definition
    }
}

/// A definition a hunk touches, with the definition it is nested in.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TouchedSymbol {
    pub name: String,
    /// A nested definition is only used where its owner is also spelled.
    pub owner: Option<String>,
}

/// Declarations touched by one hunk, in review order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HunkImpact {
    /// Definitions touched, innermost first, with their enclosing definitions.
    pub symbols: Vec<TouchedSymbol>,
    /// Whether a heading changed rather than only a body: a signature, field, or constant.
    pub contract: bool,
}

/// Facts about one file used to rank it among the other reviewed files.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FileImpact {
    /// Attributable names this file defines for itself in either revision.
    pub defined: HashSet<String>,
    /// Every identifier or type name spelled in either revision.
    pub references: HashSet<String>,
}

struct Declarations {
    items: Vec<Declaration>,
    /// Innermost declaration index per one-based line.
    innermost: Vec<Option<usize>>,
}

impl Declarations {
    fn collect(tree: &SyntaxTree<'_>) -> Self {
        let mut items = Vec::<Declaration>::new();
        // Preorder arena: an open declaration encloses every later node until its subtree ends.
        let mut open = Vec::<(usize, usize)>::new();
        for (index, node) in tree.nodes.iter().enumerate() {
            while open.last().is_some_and(|(end, _)| *end <= index) {
                open.pop();
            }
            if node.declares == Declares::Nothing {
                continue;
            }
            let Some(symbol) = tree
                .identity_text(NodeIdOf(index).id())
                .and_then(symbol_from_identity)
            else {
                continue;
            };
            let head = heading_lines(tree, index);
            items.push(Declaration {
                symbol,
                declares: node.declares,
                lines: node.lines.clone(),
                head,
                parent: open.last().map(|(_, declaration)| *declaration),
            });
            let end = tree
                .descendants(NodeIdOf(index).id())
                .last()
                .map_or(index + 1, |last| last.index() + 1);
            open.push((end, items.len() - 1));
        }
        let mut innermost = vec![None; tree.source.lines().len() + 1];
        for (index, declaration) in items.iter().enumerate() {
            for line in declaration.lines.clone() {
                if let Some(slot) = innermost.get_mut(line) {
                    *slot = Some(index);
                }
            }
        }
        Self { items, innermost }
    }

    fn touch(&self, line: usize, symbols: &mut Vec<TouchedSymbol>, contract: &mut bool) {
        let Some(mut index) = self.innermost.get(line).copied().flatten() else {
            return;
        };
        if self.items[index].head.contains(&line) {
            *contract = true;
        }
        loop {
            let declaration = &self.items[index];
            if declaration.attributable()
                && !symbols
                    .iter()
                    .any(|symbol| symbol.name == declaration.symbol)
            {
                let owner = self
                    .owner(index)
                    .map(|owner| self.items[owner].symbol.clone());
                symbols.push(TouchedSymbol {
                    name: declaration.symbol.clone(),
                    owner,
                });
            }
            match declaration.parent {
                Some(parent) => index = parent,
                None => break,
            }
        }
    }

    fn owner(&self, mut index: usize) -> Option<usize> {
        loop {
            index = self.items[index].parent?;
            if self.items[index].attributable() {
                return Some(index);
            }
        }
    }
}

/// Zero-cost wrapper so arena indices read as node identities at the call site.
struct NodeIdOf(usize);

impl NodeIdOf {
    fn id(&self) -> super::syntax::NodeId {
        super::syntax::NodeId::new(self.0)
    }
}

/// Lines from the declaration's first line through the line its body opens on.
/// A declaration without a multi-line body is heading throughout.
fn heading_lines(tree: &SyntaxTree<'_>, index: usize) -> Range<usize> {
    let node = &tree.nodes[index];
    let body = node
        .children
        .iter()
        .map(|child| tree.node(*child))
        .filter(|child| child.leaf.is_none() && child.lines.end == node.lines.end)
        .filter(|child| child.lines.len() > 1 || child.lines.start > node.lines.start)
        .map(|child| child.lines.start)
        .min();
    let Some(body) = body else {
        return node.lines.clone();
    };
    if body == node.lines.start {
        node.lines.start..node.lines.start + 1
    } else {
        node.lines.start..body
    }
}

/// Reduce a spelled identity to one referable name.
/// Receivers, parameter lists, qualifiers, and generics do not survive.
fn symbol_from_identity(identity: &str) -> Option<String> {
    let mut text = identity.trim();
    if text.contains(')') && !text.ends_with(')') {
        text = text.rsplit(')').next().unwrap_or(text);
    }
    text = text.split('(').next().unwrap_or(text);
    text = text.split('<').next().unwrap_or(text);
    text = text.rsplit("::").next().unwrap_or(text);
    text = text.rsplit('.').next().unwrap_or(text);
    let text = text.trim().trim_start_matches(['*', '&', '!']).trim();
    (!text.is_empty()
        && text
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '_' | '$')))
    .then(|| text.to_owned())
}

fn references(tree: &SyntaxTree<'_>, into: &mut HashSet<String>) {
    for (index, node) in tree.nodes.iter().enumerate() {
        let Some(leaf) = node.leaf else {
            continue;
        };
        if leaf.role != LeafRole::Identifier && leaf.syntax != SyntaxClass::Type {
            continue;
        }
        if let Some(text) = tree.leaf_text(NodeIdOf(index).id())
            && !text.is_empty()
            && !into.contains(text)
        {
            into.insert(text.to_owned());
        }
    }
}

/// Attribute each hunk to the declarations it touches and summarize the file.
pub fn file_impact(
    pair: &SyntaxPair<'_, '_>,
    hunks: &[RefinedHunk],
) -> (Vec<HunkImpact>, FileImpact) {
    let before = Declarations::collect(&pair.before);
    let after = Declarations::collect(&pair.after);
    let impacts = hunks
        .iter()
        .map(|hunk| {
            let mut impact = HunkImpact::default();
            for change in &hunk.changes {
                match change {
                    SourceChange::Edited(line) => {
                        after.touch(line.number, &mut impact.symbols, &mut impact.contract);
                    }
                    SourceChange::Replace {
                        before: removed,
                        after: added,
                        ..
                    } => {
                        for line in removed {
                            before.touch(line.number, &mut impact.symbols, &mut impact.contract);
                        }
                        for line in added {
                            after.touch(line.number, &mut impact.symbols, &mut impact.contract);
                        }
                    }
                    SourceChange::Compact(change) => {
                        if let Some(line) = change.before_line {
                            before.touch(line, &mut impact.symbols, &mut impact.contract);
                        }
                        if let Some(line) = change.after_line {
                            after.touch(line, &mut impact.symbols, &mut impact.contract);
                        }
                    }
                    SourceChange::Move(block) => {
                        let mut contract = false;
                        for line in block.after.clone() {
                            after.touch(line, &mut impact.symbols, &mut contract);
                        }
                    }
                    SourceChange::Context(_)
                    | SourceChange::Reflow(_)
                    | SourceChange::LineEnding(_)
                    | SourceChange::Elision(_) => {}
                }
            }
            impact
        })
        .collect();
    let mut file = FileImpact::default();
    for declaration in before.items.iter().chain(&after.items) {
        if declaration.attributable() {
            file.defined.insert(declaration.symbol.clone());
        }
    }
    references(&pair.before, &mut file.references);
    references(&pair.after, &mut file.references);
    (impacts, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_reduce_to_referable_names() {
        assert_eq!(symbol_from_identity("foo").as_deref(), Some("foo"));
        assert_eq!(
            symbol_from_identity("ns::foo(int a)").as_deref(),
            Some("foo")
        );
        assert_eq!(symbol_from_identity("(s *Foo) Bar").as_deref(), Some("Bar"));
        assert_eq!(symbol_from_identity("Foo<T>").as_deref(), Some("Foo"));
        assert_eq!(symbol_from_identity("*Foo").as_deref(), Some("Foo"));
        assert_eq!(symbol_from_identity("a, b"), None);
        assert_eq!(symbol_from_identity(""), None);
    }
}
