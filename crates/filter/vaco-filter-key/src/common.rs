//! Shared option-parsing and negotiation helpers.

use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

/// Parse `O` from the graph-syntax argument text, defaulting every field
/// `args` does not mention. Same shape as `vaco-filter-color::common`'s
/// helper of the same name.
pub(crate) fn parse<O: vaco_opts::Options + Default>(
    args: Option<&str>,
) -> std::result::Result<O, String> {
    let mut o = O::default();
    if let Some(text) = args {
        o.set_from_string(text, "=", ":")
            .map_err(|e| e.to_string())?;
    }
    Ok(o)
}

/// Every pixel format for which `pred` holds, in [`PixFmt::all`] order.
#[must_use]
pub(crate) fn formats_where(pred: impl Fn(PixFmt) -> bool) -> Vec<PixFmt> {
    PixFmt::all().iter().copied().filter(|&f| pred(f)).collect()
}
