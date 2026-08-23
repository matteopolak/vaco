//! Shared negotiation helper.

use vaco_pixfmt::PixFmt;

/// Every pixel format for which `pred` holds, in [`PixFmt::all`] order.
#[must_use]
pub(crate) fn formats_where(pred: impl Fn(PixFmt) -> bool) -> Vec<PixFmt> {
    PixFmt::all().iter().copied().filter(|&f| pred(f)).collect()
}
