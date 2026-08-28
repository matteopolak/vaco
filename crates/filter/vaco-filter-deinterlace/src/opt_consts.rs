//! Shared named-constant lists for options several filters in this crate
//! register identically, confirmed directly against real `ffmpeg 8.1 -h
//! filter=<name>`.
//!
//! Each of these was, until this pass, a plain ranged `i32` field with no
//! `unit`/`consts` attached, so the reference's own documented spelling
//! (`parity=tff`, `deint=interlaced`, `first_field=bottom`) failed to parse
//! against these filters outright, even though the bare integer form
//! (`parity=0`, `deint=1`, `first_field=1`) worked. See
//! `docs/filter/vaco-filter-deinterlace.md` for the survey this closes.

/// `bwdif`/`estdif`/`w3fdif`/`yadif`'s shared `parity` option.
pub(crate) const PARITY_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "tff",
        help: "",
        unit: "parity",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "bff",
        help: "",
        unit: "parity",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "auto",
        help: "",
        unit: "parity",
        value: vaco_opts::ConstValue::Int(-1),
        flags: vaco_opts::OptFlags::NONE,
    },
];

/// `bwdif`/`estdif`/`w3fdif`/`yadif`'s shared `deint` option.
pub(crate) const DEINT_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "all",
        help: "",
        unit: "deint",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "interlaced",
        help: "",
        unit: "deint",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
];

/// `detelecine`/`weave`/`doubleweave`'s shared `first_field` option --
/// two names per non-default-numbering-free value (`top`/`t`, `bottom`/`b`).
pub(crate) const FIRST_FIELD_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "top",
        help: "",
        unit: "first_field",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "t",
        help: "",
        unit: "first_field",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "bottom",
        help: "",
        unit: "first_field",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "b",
        help: "",
        unit: "first_field",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
];
