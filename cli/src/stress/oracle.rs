//! Independent lexical audit using concrete parser leaves, without Mig's lowering or matching.

use anyhow::Result;
use mig_core::LineNumber;
use mig_core::syntax::{Grammar, grammar_for_path};
use std::ops::Range;
use std::path::Path;

struct Lexeme {
    bytes: Range<usize>,
    symbol: &'static str,
}

struct Revision<'text> {
    text: &'text str,
    lines: Vec<Range<usize>>,
    lexemes: Vec<Lexeme>,
}

pub(super) struct Oracle<'text> {
    before: Revision<'text>,
    after: Revision<'text>,
}

impl<'text> Oracle<'text> {
    pub fn new(path: &str, before: &'text str, after: &'text str) -> Result<Self> {
        let grammar = grammar_for_path(Path::new(path));
        let before = Revision::new(before, grammar)?;
        let after = Revision::new(after, grammar)?;
        Ok(Self { before, after })
    }

    pub fn preserved(&self, before: &[LineNumber], after: &[LineNumber]) -> bool {
        self.before.tokens(before) == self.after.tokens(after)
    }
}

impl<'text> Revision<'text> {
    fn new(text: &'text str, grammar: Option<Grammar>) -> Result<Self> {
        let mut offset = 0;
        let lines = text
            .split_inclusive('\n')
            .map(|line| {
                let bytes = offset..offset + line.len();
                offset = bytes.end;
                bytes
            })
            .collect::<Vec<_>>();
        let Some(grammar) = grammar else {
            let lexemes = lines
                .iter()
                .cloned()
                .map(|bytes| Lexeme {
                    bytes,
                    symbol: "line",
                })
                .collect();
            return Ok(Self {
                text,
                lines,
                lexemes,
            });
        };
        let mut parser = tree_sitter::Parser::new();
        let language = grammar.language();
        parser.set_language(&language)?;
        let tree = parser
            .parse(text, None)
            .expect("an uncancelled parser produces a tree");
        let mut pending = vec![tree.root_node()];
        let mut lexemes = Vec::new();
        while let Some(node) = pending.pop() {
            if grammar == Grammar::Python && node.kind() == "line_continuation" {
                continue;
            }
            // Keeping debug f-string expressions whole; their source spacing is visible output.
            let debug_expression = grammar == Grammar::Python
                && node.kind() == "interpolation"
                && node
                    .children(&mut node.walk())
                    .any(|child| child.kind() == "=");
            if node.child_count() == 0 || debug_expression {
                if !node.byte_range().is_empty() {
                    lexemes.push(Lexeme {
                        bytes: node.byte_range(),
                        symbol: node.kind(),
                    });
                }
                continue;
            }
            let mut cursor = node.walk();
            let children = node.children(&mut cursor).collect::<Vec<_>>();
            pending.extend(children.into_iter().rev());
        }
        Ok(Self {
            text,
            lines,
            lexemes,
        })
    }

    fn tokens(&self, numbers: &[LineNumber]) -> Vec<(&'static str, &'text str)> {
        let mut ranges = Vec::<Range<usize>>::new();
        for number in numbers {
            let index = number.index();
            let Some(bytes) = self.lines.get(index) else {
                return Vec::new();
            };
            if let Some(last) = ranges.last_mut()
                && last.end == bytes.start
            {
                last.end = bytes.end;
            } else {
                ranges.push(bytes.clone());
            }
        }
        let mut tokens = Vec::new();
        for bytes in ranges {
            let first = self
                .lexemes
                .partition_point(|lexeme| lexeme.bytes.end <= bytes.start);
            for lexeme in &self.lexemes[first..] {
                if lexeme.bytes.start >= bytes.end {
                    break;
                }
                let overlap = lexeme.bytes.start.max(bytes.start)..lexeme.bytes.end.min(bytes.end);
                if !overlap.is_empty() {
                    tokens.push((lexeme.symbol, &self.text[overlap]));
                }
            }
        }
        tokens
    }
}
