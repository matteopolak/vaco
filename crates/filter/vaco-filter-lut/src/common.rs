//! Shared negotiation helper.

use vaco_pixfmt::PixFmt;

/// Every pixel format for which `pred` holds, in [`PixFmt::all`] order.
#[must_use]
pub(crate) fn formats_where(pred: impl Fn(PixFmt) -> bool) -> Vec<PixFmt> {
    PixFmt::all().iter().copied().filter(|&f| pred(f)).collect()
}

/// `ffmpeg -h filter=lut3d`/`filter=haldclut`'s own named constants for
/// `clut` (`first`/`all`), confirmed directly. Shared because both
/// filters register the identical option. A plain ranged `i32` field
/// with no `unit`/`consts` never accepts these names, only the bare
/// integer -- `clut=all` used to fail to parse against either filter
/// outright.
pub(crate) const CLUT_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "first",
        help: "",
        unit: "clut_mode",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "all",
        help: "",
        unit: "clut_mode",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
];

/// `ffmpeg -h filter=lut3d`/`filter=haldclut`'s own named constants for
/// `interp`, confirmed directly. Shared for the same reason as
/// [`CLUT_CONSTS`]. Only `nearest`/anything-else is actually
/// distinguished by each filter's own `Interp::from_opt` (a pre-existing,
/// documented fallback, not touched here); this fixes parsing the other
/// four names, which used to fail outright rather than silently falling
/// back.
pub(crate) const LUT3D_INTERP_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "nearest",
        help: "",
        unit: "lut3d_interp",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "trilinear",
        help: "",
        unit: "lut3d_interp",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "tetrahedral",
        help: "",
        unit: "lut3d_interp",
        value: vaco_opts::ConstValue::Int(2),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "pyramid",
        help: "",
        unit: "lut3d_interp",
        value: vaco_opts::ConstValue::Int(3),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "prism",
        help: "",
        unit: "lut3d_interp",
        value: vaco_opts::ConstValue::Int(4),
        flags: vaco_opts::OptFlags::NONE,
    },
];
