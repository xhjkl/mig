//! Render-neutral review signals used before hunk melding and row presentation.

/// Why one changed source region deserves review attention.
///
/// The variants also define buoyancy: payload edits rise above moves, source
/// wiring, and finally layout-only reflow.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Signal {
    /// Source payload was edited in place.
    Edit,
    /// Existing source payload changed order.
    Move,
    /// Low-signal source wiring such as imports and bodyless module declarations.
    Wiring,
    /// Existing payload kept its meaning but changed physical layout.
    Reflow,
}

impl Signal {
    pub(crate) const fn buoyancy(self) -> Buoyancy {
        let buoyancy = match self {
            Self::Edit => 3,
            Self::Move => 2,
            Self::Wiring => 1,
            Self::Reflow => 0,
        };
        Buoyancy(buoyancy)
    }

    pub(crate) const fn receives_context(self) -> bool {
        !matches!(self, Self::Move)
    }
}

/// How strongly a source signal rises toward the front of a review.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct Buoyancy(u8);

/// Zero-based current-world gap containing an event.
///
/// The value is the number of current lines preceding the event, so zero is before
/// the first line and the current line count is the gap at EOF.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct AfterGap(usize);

impl AfterGap {
    pub(crate) const BEFORE_FIRST: Self = Self(0);

    /// Gap after `preceding_lines` current-world lines.
    pub(crate) const fn new(preceding_lines: usize) -> Self {
        Self(preceding_lines)
    }

    /// Number of current-world lines preceding this gap.
    pub(crate) const fn preceding_lines(self) -> usize {
        self.0
    }
}
