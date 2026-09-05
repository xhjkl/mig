use std::ops::Range;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LineEnding {
    /// Unterminated EOF, kept distinct so a missing newline remains a visible change.
    Missing,
    Lf,
    CrLf,
}

/// Byte ranges for a one-based physical line; terminators belong to the preceding line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLine {
    pub number: usize,
    pub content_bytes: Range<usize>,
    pub full_bytes: Range<usize>,
    pub ending: LineEnding,
}

/// Borrowed source with a physical-line index shared by all language frontends.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Source<'source> {
    text: &'source str,
    lines: Vec<SourceLine>,
}

impl<'source> Source<'source> {
    pub fn new(text: &'source str) -> Self {
        let mut lines = Vec::new();
        let mut start = 0;
        for line in text.split_inclusive('\n') {
            let (content, ending) = if let Some(content) = line.strip_suffix("\r\n") {
                (content, LineEnding::CrLf)
            } else if let Some(content) = line.strip_suffix('\n') {
                (content, LineEnding::Lf)
            } else {
                (line, LineEnding::Missing)
            };
            lines.push(SourceLine {
                number: lines.len() + 1,
                content_bytes: start..start + content.len(),
                full_bytes: start..start + line.len(),
                ending,
            });
            start += line.len();
        }

        Self { text, lines }
    }

    pub fn as_str(&self) -> &'source str {
        self.text
    }

    /// Return physical lines, without an extra empty line after a final terminator.
    pub fn lines(&self) -> &[SourceLine] {
        &self.lines
    }

    /// Look up a physical line by its one-based number.
    pub fn line(&self, number: usize) -> Option<&SourceLine> {
        let index = number.checked_sub(1)?;
        self.lines.get(index)
    }

    /// Borrow a line's content, excluding its LF or CRLF terminator.
    pub fn text(&self, line: &SourceLine) -> &'source str {
        &self.text[line.content_bytes.clone()]
    }

    /// Borrow a line together with its original terminator.
    pub fn full_text(&self, line: &SourceLine) -> &'source str {
        &self.text[line.full_bytes.clone()]
    }

    /// Borrow a source range, rejecting invalid bounds or split UTF-8 characters.
    pub fn slice(&self, bytes: Range<usize>) -> Option<&'source str> {
        self.text.get(bytes)
    }

    /// Locate the line owning a byte, assigning both bytes of CRLF to the preceding line.
    fn line_containing_byte(&self, byte: usize) -> Option<&SourceLine> {
        if byte >= self.text.len() {
            return None;
        }

        let index = self
            .lines
            .partition_point(|line| line.full_bytes.end <= byte);
        self.lines.get(index)
    }

    /// Locate the one-based insertion line, including the line after a terminated EOF.
    fn position_line(&self, byte: usize) -> Option<usize> {
        if byte > self.text.len() {
            return None;
        }

        let completed = self.lines.partition_point(|line| {
            line.full_bytes.end < byte
                || (line.full_bytes.end == byte && line.ending != LineEnding::Missing)
        });
        Some(completed + 1)
    }

    /// Find the smallest one-based, half-open line range covering the supplied bytes.
    /// Empty byte ranges produce an empty range at the insertion line. A nonempty range
    /// ending at the start of a line excludes that line.
    pub fn line_coverage(&self, bytes: Range<usize>) -> Option<Range<usize>> {
        if bytes.start > bytes.end || bytes.end > self.text.len() {
            return None;
        }
        if bytes.is_empty() {
            let line = self.position_line(bytes.start)?;
            return Some(line..line);
        }

        let first = self.line_containing_byte(bytes.start)?;
        let last = self.line_containing_byte(bytes.end - 1)?;
        Some(first.number..last.number + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_source_has_no_physical_lines_but_retains_its_first_position() {
        let source = Source::new("");

        assert_eq!(source.as_str(), "");
        assert!(source.lines().is_empty());
        assert_eq!(source.line(1), None);
        assert_eq!(source.line_containing_byte(0), None);
        assert_eq!(source.position_line(0), Some(1));
        assert_eq!(source.line_coverage(0..0), Some(1..1));
    }

    #[test]
    fn line_index_preserves_lf_crlf_and_missing_terminators() {
        let source = Source::new("alpha\r\n\nbeta");

        assert_eq!(source.lines().len(), 3);
        assert_eq!(
            source.lines()[0],
            SourceLine {
                number: 1,
                content_bytes: 0..5,
                full_bytes: 0..7,
                ending: LineEnding::CrLf,
            }
        );
        assert_eq!(
            source.lines()[1],
            SourceLine {
                number: 2,
                content_bytes: 7..7,
                full_bytes: 7..8,
                ending: LineEnding::Lf,
            }
        );
        assert_eq!(
            source.lines()[2],
            SourceLine {
                number: 3,
                content_bytes: 8..12,
                full_bytes: 8..12,
                ending: LineEnding::Missing,
            }
        );
    }

    #[test]
    fn line_text_views_borrow_exact_original_bytes() {
        let source = Source::new("alpha\r\n\nbeta");

        assert_eq!(source.text(source.line(1).unwrap()), "alpha");
        assert_eq!(source.full_text(source.line(1).unwrap()), "alpha\r\n");
        assert_eq!(source.text(source.line(2).unwrap()), "");
        assert_eq!(source.full_text(source.line(2).unwrap()), "\n");
        assert_eq!(source.text(source.line(3).unwrap()), "beta");
        assert_eq!(source.full_text(source.line(3).unwrap()), "beta");
        assert_eq!(source.slice(7..12), Some("\nbeta"));
        assert_eq!(source.slice(1..usize::MAX), None);
    }

    #[test]
    fn lone_carriage_return_is_content() {
        let source = Source::new("a\rb");
        let line = source.line(1).unwrap();

        assert_eq!(source.text(line), "a\rb");
        assert_eq!(line.ending, LineEnding::Missing);
        assert_eq!(line.content_bytes, 0..3);
        assert_eq!(line.full_bytes, 0..3);
    }

    #[test]
    fn terminal_newline_does_not_create_a_physical_line() {
        let source = Source::new("a\n");

        assert_eq!(source.lines().len(), 1);
        assert_eq!(source.full_text(source.line(1).unwrap()), "a\n");
        assert_eq!(source.line(2), None);
        assert_eq!(source.position_line(2), Some(2));
    }

    #[test]
    fn byte_lookup_assigns_terminators_to_the_preceding_line() {
        let source = Source::new("a\r\nb\nç");

        assert_eq!(
            source.line_containing_byte(0).map(|line| line.number),
            Some(1)
        );
        assert_eq!(
            source.line_containing_byte(1).map(|line| line.number),
            Some(1)
        );
        assert_eq!(
            source.line_containing_byte(2).map(|line| line.number),
            Some(1)
        );
        assert_eq!(
            source.line_containing_byte(3).map(|line| line.number),
            Some(2)
        );
        assert_eq!(
            source.line_containing_byte(4).map(|line| line.number),
            Some(2)
        );
        assert_eq!(
            source.line_containing_byte(5).map(|line| line.number),
            Some(3)
        );
        assert_eq!(source.line_containing_byte(source.as_str().len()), None);
    }

    #[test]
    fn position_lookup_distinguishes_terminated_and_unterminated_eof() {
        let terminated = Source::new("a\n");
        let missing = Source::new("a");

        assert_eq!(terminated.position_line(0), Some(1));
        assert_eq!(terminated.position_line(1), Some(1));
        assert_eq!(terminated.position_line(2), Some(2));
        assert_eq!(missing.position_line(0), Some(1));
        assert_eq!(missing.position_line(1), Some(1));
        assert_eq!(missing.position_line(2), None);
    }

    #[test]
    fn byte_coverage_obeys_exclusive_end_geometry() {
        let source = Source::new("one\ntwo\r\nthree");

        assert_eq!(source.line_coverage(0..3), Some(1..2));
        assert_eq!(source.line_coverage(0..4), Some(1..2));
        assert_eq!(source.line_coverage(0..5), Some(1..3));
        assert_eq!(source.line_coverage(4..9), Some(2..3));
        assert_eq!(source.line_coverage(9..14), Some(3..4));
        assert_eq!(source.line_coverage(4..4), Some(2..2));
    }

    #[test]
    fn invalid_byte_coverage_is_rejected() {
        let source = Source::new("abc");
        let reversed = Range { start: 3, end: 2 };

        assert_eq!(source.line_coverage(reversed), None);
        assert_eq!(source.line_coverage(0..4), None);
        assert_eq!(source.position_line(4), None);
    }
}
