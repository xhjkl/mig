//! Which declarations a change touches, and what a file refers to.
//!
//! Review ranking across files uses these facts; the diff itself does not.

use super::document::Section;
use super::islands::{Islands, Piece};
use super::syntax::{Declares, LeafRole, NodeId, SyntaxPair, SyntaxTree};
use super::{Highlight, LineNumber, LineSpan};
use std::collections::HashSet;

/// A named construct with its source span and estimated heading lines.
struct Declaration {
    symbol: String,
    declares: Declares,
    lines: LineSpan,
    heading: LineSpan,
    parent: Option<usize>,
}

impl Declaration {
    /// Test whether the declaration names a possible cross-file dependency.
    /// Members are too generic to attribute on their own; they credit their owner.
    fn attributable(&self) -> bool {
        matches!(
            self.declares,
            Declares::Definition | Declares::DecoratedDefinition
        )
    }
}

/// A definition a hunk touches, with the definition it is nested in.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TouchedSymbol {
    pub name: String,
    /// A nested definition is only used where its owner is also spelled.
    pub owner: Option<String>,
}

/// Declaration dependencies created by edits or relocation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeclarationChanges {
    pub symbols: Vec<TouchedSymbol>,
    /// Heading-line edit used as a contract-change signal.
    pub contract: bool,
}

/// Changed declarations and formatting labels, kept separate for review ranking.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Attribution {
    pub changes: DeclarationChanges,
    pub formatting: Vec<TouchedSymbol>,
}

impl Attribution {
    pub fn symbols(&self) -> impl Iterator<Item = &TouchedSymbol> {
        self.changes.symbols.iter().chain(&self.formatting)
    }
}

/// Definitions and references spelled across both revisions of a file.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FileSymbols {
    pub defined: HashSet<String>,
    pub references: HashSet<String>,
}

struct Declarations {
    items: Vec<Declaration>,
    /// Innermost declaration indexed by zero-based physical line.
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
            let id = NodeId::new(index);
            let Some(symbol) = tree.identity_text(id).and_then(symbol_from_identity) else {
                continue;
            };
            let heading = heading_lines(tree, index);
            if node
                .parent
                .is_some_and(|parent| tree.node(parent).declares == Declares::DecoratedDefinition)
                && let Some((_, declaration)) = open.last()
                && items[*declaration].symbol == symbol
            {
                // Sharing one declaration; decorators extend its heading without adding an owner.
                let declaration = &mut items[*declaration];
                declaration.heading.end = declaration.heading.end.max(heading.end);
                continue;
            }
            items.push(Declaration {
                symbol,
                declares: node.declares,
                lines: node.lines.clone(),
                heading,
                parent: open.last().map(|(_, declaration)| *declaration),
            });
            open.push((tree.subtree_end(id), items.len() - 1));
        }
        let mut innermost = vec![None; tree.source.lines().len()];
        for (index, declaration) in items.iter().enumerate() {
            for line in declaration.lines.clone() {
                if let Some(slot) = innermost.get_mut(line.index()) {
                    *slot = Some(index);
                }
            }
        }
        Self { items, innermost }
    }

    fn touch(&self, line: LineNumber, symbols: &mut Vec<TouchedSymbol>, contract: &mut bool) {
        let Some(mut index) = self.innermost.get(line.index()).copied().flatten() else {
            return;
        };
        if self.items[index].heading.contains(&line) {
            *contract = true;
        }
        loop {
            let declaration = &self.items[index];
            if declaration.attributable() {
                let owner = self
                    .owner(index)
                    .map(|owner| self.items[owner].symbol.as_str());
                if !symbols.iter().any(|symbol| {
                    symbol.name == declaration.symbol && symbol.owner.as_deref() == owner
                }) {
                    symbols.push(TouchedSymbol {
                        name: declaration.symbol.clone(),
                        owner: owner.map(str::to_owned),
                    });
                }
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

/// Estimate heading lines for review ranking from the final composite child.
/// A body sharing the opening line leaves that line in the heading.
fn heading_lines(tree: &SyntaxTree<'_>, index: usize) -> LineSpan {
    let node = &tree.nodes[index];
    let body = node
        .children()
        .iter()
        .map(|child| tree.node(*child))
        .filter(|child| child.leaf().is_none() && child.lines.end == node.lines.end)
        .filter(|child| child.lines.len() > 1 || child.lines.start > node.lines.start)
        .map(|child| child.lines.start)
        .min();
    let Some(body) = body else {
        return node.lines.clone();
    };
    if body == node.lines.start {
        LineSpan::single(node.lines.start)
    } else {
        LineSpan::new(node.lines.start, body)
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
        let Some(leaf) = node.leaf() else {
            continue;
        };
        if leaf.role != LeafRole::Identifier && leaf.highlight != Highlight::Type {
            continue;
        }
        if let Some(text) = tree.leaf_text(NodeId::new(index))
            && !text.is_empty()
            && !into.contains(text)
        {
            into.insert(text.to_owned());
        }
    }
}

/// Attribute each section to its declarations and collect file-wide symbol facts.
/// Return attributions in section order.
pub fn attribute(
    pair: &SyntaxPair<'_, '_>,
    islands: &Islands,
    sections: &[Section],
) -> (Vec<Attribution>, FileSymbols) {
    let before = Declarations::collect(&pair.before);
    let after = Declarations::collect(&pair.after);
    let attributions = sections
        .iter()
        .map(|hunk| {
            let mut attribution = Attribution::default();
            for index in &hunk.islands {
                for piece in &islands.islands[*index].pieces {
                    let symbols = match piece {
                        Piece::Reflow(_) => &mut attribution.formatting,
                        _ => &mut attribution.changes.symbols,
                    };
                    let mut contract = false;
                    for line in &piece.change().after {
                        after.touch(LineNumber::from_index(*line), symbols, &mut contract);
                    }
                    for line in &piece.change().before {
                        before.touch(LineNumber::from_index(*line), symbols, &mut contract);
                    }
                    if matches!(piece, Piece::Block(_) | Piece::Inline(_)) {
                        attribution.changes.contract |= contract;
                    }
                }
            }
            attribution
                .formatting
                .retain(|symbol| !attribution.changes.symbols.contains(symbol));
            attribution
        })
        .collect();
    let mut file = FileSymbols::default();
    for declaration in before.items.iter().chain(&after.items) {
        if declaration.attributable() {
            file.defined.insert(declaration.symbol.clone());
        }
    }
    references(&pair.before, &mut file.references);
    references(&pair.after, &mut file.references);
    (attributions, file)
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
