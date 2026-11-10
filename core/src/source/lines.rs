use std::fmt;
use std::num::NonZeroUsize;
use std::ops::Range;

/// One-based source coordinate, including an insertion position after the last line.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LineNumber(NonZeroUsize);

impl LineNumber {
    pub const fn new(number: usize) -> Option<Self> {
        match NonZeroUsize::new(number) {
            Some(number) => Some(Self(number)),
            None => None,
        }
    }

    pub const fn from_index(index: usize) -> Self {
        assert!(index < usize::MAX, "line index exceeds source coordinates");
        Self(NonZeroUsize::new(index + 1).unwrap())
    }

    pub const fn get(self) -> usize {
        self.0.get()
    }
    pub const fn index(self) -> usize {
        self.get() - 1
    }
    pub const fn next(self) -> Self {
        Self::from_index(self.get())
    }
}

impl fmt::Display for LineNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(formatter)
    }
}

/// Half-open source span whose coordinates remain one-based at public boundaries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineSpan {
    pub(crate) start: LineNumber,
    pub(crate) end: LineNumber,
}

impl LineSpan {
    pub fn new(start: LineNumber, end: LineNumber) -> Self {
        assert!(start <= end, "source span ends before it starts");
        Self { start, end }
    }

    pub fn from_indices(indices: Range<usize>) -> Self {
        Self::new(
            LineNumber::from_index(indices.start),
            LineNumber::from_index(indices.end),
        )
    }

    pub fn single(number: LineNumber) -> Self {
        Self::new(number, number.next())
    }
    pub fn start(&self) -> LineNumber {
        self.start
    }
    pub fn end(&self) -> LineNumber {
        self.end
    }
    pub fn indices(&self) -> Range<usize> {
        self.start.index()..self.end.index()
    }
    pub fn len(&self) -> usize {
        self.end.index() - self.start.index()
    }
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
    pub fn contains(&self, number: &LineNumber) -> bool {
        self.start <= *number && *number < self.end
    }
    pub fn last(&self) -> Option<LineNumber> {
        self.indices().last().map(LineNumber::from_index)
    }
}

impl IntoIterator for LineSpan {
    type Item = LineNumber;
    type IntoIter = std::iter::Map<Range<usize>, fn(usize) -> LineNumber>;

    fn into_iter(self) -> Self::IntoIter {
        self.indices().map(LineNumber::from_index)
    }
}
