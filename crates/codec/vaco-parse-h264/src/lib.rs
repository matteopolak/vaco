//! H.264 parameter-set, SEI and slice-header parsing — **no decode**.
//!
//! # Parsing is not decoding, and that is load-bearing
//!
//! This crate reads the syntax H.264 puts in front of the picture data: the
//! sequence and picture parameter sets, the SEI messages, and the slice
//! *headers*. It stops there. There is no `slice_data()`, no macroblock, no
//! residual, no motion compensation, no sample of output anywhere in it.
//!
//! That line is where the legal argument lives (D7, D15, plan 15 §1.6 and
//! §6.2). H.264 is patent-encumbered and its decoders are not in our default
//! build — but a parameter-set parser implements no decoding process and is not
//! "a decoder" under any pool's definition of a unit, so it ships. Anything
//! that drifts across the line takes the crate out of the default build with
//! it. The one clause-8 procedure here is picture order count (§8.2.1), which
//! is integer arithmetic over slice-header fields, needs no reference picture,
//! and produces an output *order* rather than an output *picture*. See
//! [`poc`] for why that is on the near side.
//!
//! # What is here
//!
//! | Module | Syntax |
//! |---|---|
//! | [`nal`] | NAL unit types, Table 7-1 |
//! | [`sps`] | `seq_parameter_set_data()` §7.3.2.1.1, `vui_parameters()` §E.1.1, `hrd_parameters()` §E.1.2 |
//! | [`pps`] | `pic_parameter_set_rbsp()` §7.3.2.2 |
//! | [`slice`] | `slice_header()` §7.3.3 and its three sub-structures |
//! | [`sei`] | `sei_message()` §7.3.2.3 and the Annex D payloads worth decoding |
//! | [`poc`] | picture order count, §8.2.1 |
//! | [`profile`] | profiles and levels, Annex A |
//! | [`avcc`] | `AVCDecoderConfigurationRecord`, ISO/IEC 14496-15 §5.3.3.1 |
//! | [`params`] | the parameter-set store and the [`CodecParameters`] an SPS implies |
//! | [`parser`] | [`H264Parser`], the streaming access-unit splitter |
//!
//! # The three numbers that are easy to get wrong
//!
//! Each has its own `// D17:` note where it is computed, because each is
//! printed by `ffprobe -show_streams` and each has a version that looks right:
//!
//! 1. **Resolution.** `pic_width_in_mbs_minus1` and
//!    `pic_height_in_map_units_minus1` give a *macroblock-aligned* size; the
//!    displayed size subtracts the four `frame_crop_*_offset` values scaled by
//!    a crop unit that depends on the chroma format *and* on frame/field
//!    coding. A 1080-line stream is coded as **1088** and cropped by eight
//!    rows. See [`Sps::dimensions`].
//! 2. **Frame rate.** The VUI's `num_units_in_tick` counts ticks per *field*,
//!    so the picture rate is `time_scale / (2 * num_units_in_tick)`. The
//!    reference reports the unhalved rate as `r_frame_rate`. Both are exposed;
//!    see [`Sps::frame_rate`].
//! 3. **Pixel format.** Monochrome is reported as 4:2:0, and the `yuvj` family
//!    exists only at 8 bits. See [`params::pixel_format`].
//!
//! # Safety on untrusted input
//!
//! This crate parses fully untrusted data and is the classic
//! decoder-vulnerability surface, so:
//!
//! * **Every `ue(v)` has a bound.** Reads go through
//!   [`BoundedGolomb`](vaco_codec_golomb::BoundedGolomb), which takes the
//!   ceiling at the read site and charges fuel per syntax element. The bound is
//!   the specification's own range constraint wherever it states one, and the
//!   comment says which clause.
//! * **Every count that sizes an allocation goes through
//!   [`Budget`](vaco_limits::Budget)**, and the loop is charged against fuel
//!   *before* it runs, so a declared count of four billion fails immediately
//!   rather than after four billion reads.
//! * **Every `do … while` has an explicit ceiling.** The two in the slice
//!   header (§7.3.3.1, §7.3.3.3) and the two `ff_byte` runs in an SEI header
//!   (§7.3.2.3.1) are unbounded in the syntax and bounded here.
//! * **No panics.** `unwrap`/`expect`/`panic`/`indexing_slicing` are denied
//!   workspace-wide and nothing here escapes them. The `parse_h264` fuzz target
//!   exists to keep it that way.
//!
//! # Example
//!
//! ```
//! use vaco_limits::{Budget, Limits};
//! use vaco_parse_h264::Sps;
//!
//! // An SPS from `libx264`, emulation prevention already removed.
//! let rbsp = [
//!     0x67, 0x64, 0x00, 0x1E, 0xAC, 0xD9, 0x40, 0xA0, 0x2F, 0xF9, 0x70, 0x11,
//!     0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x30, 0x0F, 0x16, 0x2D, 0x96,
//! ];
//! let mut budget = Budget::new(Limits::strict());
//! let sps = Sps::parse(&rbsp, &mut budget)?;
//!
//! assert_eq!(sps.dimensions(), Some((640, 360)));   // cropped from 640x368
//! assert_eq!(sps.coded_height(), 368);              // macroblock-aligned
//! assert_eq!(sps.profile_name(), Some("High"));
//! assert_eq!(sps.frame_rate(), vaco_core::Rational::new(24, 1));
//! # Ok::<(), vaco_core::Error>(())
//! ```
//!
//! # Specification
//!
//! ITU-T H.264 (ISO/IEC 14496-10), version 14 (2020): §7.3 and §7.4 for the
//! syntax and semantics, §8.2.1 for picture order count, Annex A for profiles
//! and levels, Annex B for the byte stream, Annex D for SEI, Annex E for the
//! VUI and HRD. ISO/IEC 14496-15 §5.3.3.1 for `avcC`. Nothing here was taken
//! from any implementation (D7).
//!
//! # Dependencies
//!
//! `vaco-bitstream` for the reader, `vaco-codec-golomb` for the bounded
//! Exp-Golomb reads, `vaco-format-nalu` for framing and RBSP extraction,
//! `vaco-codec-core` for the [`Parser`](vaco_codec_core::Parser) trait and
//! [`CodecParameters`], `vaco-color` and `vaco-pixfmt` for the signalling
//! enums, `vaco-limits` for the budget, `vaco-packet` for the emitted packets.
//! No external runtime dependencies.

#![forbid(unsafe_code)]

pub mod a53;
pub mod avcc;
pub mod cbs;
pub mod nal;
pub mod params;
pub mod parser;
pub mod poc;
pub mod pps;
pub mod profile;
pub mod sei;
pub mod slice;
pub mod sps;
mod util;

pub use a53::cc_data_from_sei;
pub use avcc::AvcDecoderConfigurationRecord;
pub use cbs::{H264Cbs, H264Content};
pub use nal::{H264NalHeader, NalUnitType};
pub use params::{MAX_PPS, MAX_SPS, ParameterSets, codec_parameters, pixel_format};
pub use parser::{H264Parser, PicStructHint, PictureInfo};
pub use poc::{PictureOrderCount, PocState};
pub use pps::{Pps, SliceGroupMap};
pub use profile::{ConstraintFlags, LEVELS, is_level_1b, max_dpb_frames, profile_name};
pub use sei::{PicStruct, SeiMessage, SeiPayload};
pub use slice::{
    MmcoCommand, PredWeightTable, RefPicListModification, RefPicMarking, SliceHeader, SliceKind,
};
pub use sps::{
    AuxFormat, BitstreamRestriction, ChromaFormat, Crop, HrdParameters, PocType1, ScalingLists,
    Sps, SpsExtension, Timing, VuiParameters,
};

// Re-exported so a caller can describe a stream without also depending on
// `vaco-codec-core` directly.
pub use vaco_codec_core::CodecParameters;

/// The registry descriptor for this parser.
///
/// `vaco-component.toml` names this const, `cargo xtask gen-registry` puts it
/// in `vaco_registry::PARSERS`, and a demuxer reaches it through
/// `ParserProvider` without ever naming this crate (D14.1).
pub const PARSER: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "h264",
    long_name: "H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10",
    codecs: &[::vaco_codec_core::CodecId::H264],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(parser::H264Parser::new(limits)),
};
