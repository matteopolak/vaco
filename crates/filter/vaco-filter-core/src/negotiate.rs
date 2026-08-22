//! Format negotiation across filter links.
//!
//! Each pad declares what it accepts; the graph then finds one assignment
//! satisfying every link, inserting conversion filters where no common format
//! exists. Expressed as constraint sets plus a "must be equal" relation over
//! pads, which handles the common case — a filter that does not care what the
//! format is, only that input and output agree — without special-casing it.

use vaco_chlayout::ChannelLayout;
use vaco_pixfmt::PixFmt;
use vaco_sampfmt::SampleFmt;

/// What one pad will accept for a single property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Constraint<T> {
    /// Any value the peer proposes.
    Any,
    /// One of these, in preference order.
    OneOf(Vec<T>),
    /// Exactly this.
    Exact(T),
}

/// The full constraint set for one pad.
#[derive(Debug, Clone, Default)]
pub struct FormatSet {
    pub pixel_formats: Option<Constraint<PixFmt>>,
    pub sample_formats: Option<Constraint<SampleFmt>>,
    pub sample_rates: Option<Constraint<u32>>,
    pub channel_layouts: Option<Constraint<ChannelLayout>>,
}

impl FormatSet {
    /// Intersect two pads' constraints.
    ///
    /// `None` means no common format exists and a conversion filter must be
    /// inserted between them.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Option<Self> {
        let _ = other;
        todo!("P0-03 freeze: per-property intersection preserving preference order")
    }
}
