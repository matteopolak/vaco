//! Shared option-parsing boilerplate.
//!
//! Every filter's `Opts::parse` is the same three lines — default, then
//! `set_from_string` if the graph text supplied any — so it lives here once
//! rather than several times.

use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

/// `ffmpeg -h filter=colorchannelmixer`/`colorlevels`'s own named
/// constants for their preserve-color-mode option -- spelled `pc` on one
/// filter, `preserve` on the other, same seven values either way. Neither
/// filter implements the behaviour (see each `Opts` field's own doc), but
/// `option_consts_gate.rs` only checks that the name parses, not that it
/// does anything -- a filter legitimately not implementing an option is a
/// different, already-tracked gap.
pub(crate) const PRESERVE_COLOR_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "none",
        help: "disabled",
        unit: "preserve_color",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "lum",
        help: "luminance",
        unit: "preserve_color",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "max",
        help: "max",
        unit: "preserve_color",
        value: vaco_opts::ConstValue::Int(2),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "avg",
        help: "average",
        unit: "preserve_color",
        value: vaco_opts::ConstValue::Int(3),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "sum",
        help: "sum",
        unit: "preserve_color",
        value: vaco_opts::ConstValue::Int(4),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "nrm",
        help: "norm",
        unit: "preserve_color",
        value: vaco_opts::ConstValue::Int(5),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "pwr",
        help: "power",
        unit: "preserve_color",
        value: vaco_opts::ConstValue::Int(6),
        flags: vaco_opts::OptFlags::NONE,
    },
];

/// Parse `O` from the graph-syntax argument text, defaulting every field
/// `args` does not mention.
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
