//! H.265/HEVC parameter-set, SEI and slice-header parsing — **no decode**.
//!
//! # Parsing is not decoding, and that is load-bearing
//!
//! This crate reads the syntax HEVC puts in front of the picture data: the
//! video, sequence and picture parameter sets, the SEI messages, and the slice
//! segment *headers*. It stops there. There is no coding unit, no residual, no
//! motion compensation, no sample of output anywhere in it.
//!
//! That line is where the legal argument lives (D7, D15, plan 15 §1.6 and
//! §6.2). HEVC is the most patent-encumbered codec in the inventory and its
//! decoders are not in our default build — but a parameter-set parser
//! implements no decoding process and is not "a decoder" under any pool's
//! definition of a unit, so it ships. Anything that drifts across the line takes
//! the crate out of the default build with it.
//!
//! Two procedures sit close to the line and are on the near side, each for a
//! reason written where it lives: picture order count ([`poc`], §8.3.1) is
//! integer arithmetic over slice-header fields that produces an output *order*
//! rather than an output *picture*; and the short-term reference picture set
//! derivation ([`rps`], §7.4.8) is required in order to *parse* the next set at
//! all, and produces a list of numbers.
//!
//! # What is here
//!
//! | Module | Syntax |
//! |---|---|
//! | [`nal`] | NAL unit types, Table 7-1, and the two-byte header §7.3.1.2 |
//! | [`ptl`] | `profile_tier_level()` §7.3.3 — the structure HEVC differs from H.264 in most |
//! | [`vps`] | `video_parameter_set_rbsp()` §7.3.2.1 |
//! | [`sps`] | `seq_parameter_set_rbsp()` §7.3.2.2, `vui_parameters()` §E.2.1, `hrd_parameters()` §E.2.2, `scaling_list_data()` §7.3.4 |
//! | [`pps`] | `pic_parameter_set_rbsp()` §7.3.2.3 |
//! | [`rps`] | `st_ref_pic_set()` §7.3.7 and the derivation of §7.4.8 |
//! | [`slice`] | `slice_segment_header()` §7.3.6.1 and its three sub-structures |
//! | [`sei`] | `sei_message()` §7.3.5 and the Annex D payloads worth decoding |
//! | [`poc`] | picture order count, §8.3.1 |
//! | [`profile`] | profiles, tiers and levels, Annex A |
//! | [`hvcc`] | `HEVCDecoderConfigurationRecord`, ISO/IEC 14496-15 §8.3.3.1 |
//! | [`params`] | the parameter-set store and the [`CodecParameters`] an SPS implies |
//! | [`parser`] | [`HevcParser`], the streaming access-unit splitter |
//! | [`cbs`] | the [`CbsCodec`](vaco_codec_cbs::CbsCodec) implementation — split, edit, re-assemble |
//!
//! # The four numbers that are easy to get wrong
//!
//! Each has its own `// D17:` note where it is computed, because each is printed
//! by `ffprobe -show_streams`, each has a version that looks right, and **all
//! four differ from the answer `vaco-parse-h264` gives for H.264**:
//!
//! 1. **Resolution.** `pic_width_in_luma_samples` is the *coded* size, already a
//!    multiple of `MinCbSizeY`; the displayed size subtracts the conformance
//!    window, whose four offsets are in **chroma units** — for 4:2:0 a
//!    `conf_win_right_offset` of 1 removes two luma columns. A 1918x1078 stream
//!    is coded 1920x1080. And unlike H.264, `ffprobe` reports **both**:
//!    `coded_width=1920 width=1918`. See [`Sps::dimensions`].
//! 2. **Frame rate.** `vui_time_scale / vui_num_units_in_tick` — **not** halved.
//!    H.264's is, because `num_units_in_tick` counts field durations there.
//!    See [`sps::VuiParameters::frame_rate`].
//! 3. **Pixel format.** Monochrome is `gray`, not 4:2:0; and the `yuvj` family
//!    exists at 4:2:0, 8 bits and nowhere else. Both are the opposite of the
//!    H.264 answer. See [`params::pixel_format`].
//! 4. **Chroma location.** The specification's "infer 0, which is left" applies
//!    at every chroma format; the reference applies it for 4:2:0 only. See
//!    [`Sps::color_info`].
//!
//! # Where HEVC is *easier* than H.264
//!
//! Worth saying, because it shapes the parser: **`first_slice_segment_in_pic_flag`
//! is the first bit of every slice segment header**, so access-unit boundary
//! detection is a bit test rather than H.264 §7.4.1.2.4's seven-field
//! comparison — and it needs no parameter sets at all. Nothing in an HEVC PPS is
//! sized by an SPS field either, so parameter sets parse independently and may
//! arrive in any order.
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
//! * **Every count that sizes a loop is charged before the loop runs**, so a
//!   declared count of four billion fails immediately rather than after four
//!   billion reads.
//! * **Every structure whose length the syntax leaves open has an explicit
//!   ceiling**: the SEI header's `ff_byte` run, the entry-point offset list, the
//!   slice header extension, the VPS's layer-set flag matrix.
//! * **No panics.** `unwrap`/`expect`/`panic`/`indexing_slicing` are denied
//!   workspace-wide and nothing here escapes them. The `parse_hevc` and
//!   `cbs_hevc` fuzz targets exist to keep it that way.
//!
//! # Example
//!
//! ```
//! use vaco_limits::{Budget, Limits};
//! use vaco_parse_hevc::Sps;
//!
//! // The SPS from an `x265` 1918x1078 stream, emulation prevention removed.
//! let ebsp = [
//!     0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03,
//!     0x00, 0x00, 0x03, 0x00, 0x78, 0xa0, 0x03, 0xc0, 0x80, 0x10, 0xe7, 0x55, 0x96,
//!     0x56, 0x69, 0x24, 0xca, 0xf0, 0x16, 0x80, 0x80, 0x00, 0x00, 0x03, 0x00, 0x80,
//!     0x00, 0x00, 0x0c, 0x84,
//! ];
//! let mut scratch = Vec::new();
//! let rbsp = vaco_bitstream::annexb::to_rbsp(&ebsp, &mut scratch);
//! let mut budget = Budget::new(Limits::strict());
//! let sps = Sps::parse(rbsp, &mut budget)?;
//!
//! assert_eq!(sps.dimensions(), Some((1918, 1078)));  // after the window
//! assert_eq!(sps.coded_width(), 1920);               // and before it
//! assert_eq!(sps.profile_name(), Some("Main"));
//! assert_eq!(sps.frame_rate(), vaco_core::Rational::new(25, 1));
//! # Ok::<(), vaco_core::Error>(())
//! ```
//!
//! # Specification
//!
//! ITU-T H.265 (ISO/IEC 23008-2), version 8 (2023): §7.3 and §7.4 for the syntax
//! and semantics, §8.3.1 for picture order count, Annex A for profiles, tiers
//! and levels, Annex B for the byte stream, Annex D for SEI, Annex E for the VUI
//! and HRD. ISO/IEC 14496-15 §8.3.3.1 for `hvcC`. Nothing here was taken from any
//! implementation (D7).
//!
//! # Dependencies
//!
//! `vaco-bitstream` for the reader, `vaco-codec-golomb` for the bounded
//! Exp-Golomb reads, `vaco-format-nalu` for framing and RBSP extraction,
//! `vaco-codec-cbs` for the read/modify/write layer, `vaco-codec-core` for the
//! [`Parser`](vaco_codec_core::Parser) trait and [`CodecParameters`],
//! `vaco-color` and `vaco-pixfmt` for the signalling enums, `vaco-limits` for the
//! budget, `vaco-packet` for the emitted packets. No external runtime
//! dependencies.

#![forbid(unsafe_code)]

pub mod cbs;
pub mod hvcc;
pub mod nal;
pub mod params;
pub mod parser;
pub mod poc;
pub mod pps;
pub mod profile;
pub mod ptl;
pub mod rps;
pub mod sei;
pub mod slice;
pub mod sps;
mod util;
pub mod vps;

pub use cbs::{HevcCbs, HevcContent};
pub use hvcc::{HevcDecoderConfigurationRecord, NalArray};
pub use nal::{HevcNalHeader, NalUnitType};
pub use params::{MAX_PPS, MAX_SPS, MAX_VPS, ParameterSets, codec_parameters, pixel_format};
pub use parser::{HevcParser, PicStructHint, PictureInfo};
pub use poc::{PictureOrderCount, PocState};
pub use pps::Pps;
pub use profile::{LEVELS, PROFILES, Tier, level_name, profile_name};
pub use ptl::{Constraint, ProfileTier, ProfileTierLevel, SubLayerPtl};
pub use rps::ShortTermRps;
pub use sei::{PicStruct, SeiMessage, SeiPayload};
pub use slice::{SliceHeader, SliceKind};
pub use sps::{ChromaFormat, HrdParameters, ScalingListData, Sps, Timing, VuiParameters, Window};
pub use vps::Vps;

// Re-exported so a caller can describe a stream without also depending on
// `vaco-codec-core` directly.
pub use vaco_codec_core::CodecParameters;

/// The registry descriptor for this parser.
///
/// `vaco-component.toml` names this const, `cargo xtask gen-registry` puts it
/// in `vaco_registry::PARSERS`, and a demuxer reaches it through
/// `ParserProvider` without ever naming this crate (D14.1).
pub const PARSER: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "hevc",
    long_name: "H.265 / HEVC (High Efficiency Video Coding)",
    codecs: &[::vaco_codec_core::CodecId::Hevc],
    media_type: ::vaco_core::MediaType::Video,
    make: |limits| ::std::boxed::Box::new(parser::HevcParser::new(limits)),
};
