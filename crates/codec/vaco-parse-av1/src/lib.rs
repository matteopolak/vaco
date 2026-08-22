//! AV1 OBU framing, sequence-header and frame-header parsing, and `av1C` —
//! **no decode**.
//!
//! # Parsing is not decoding, and that is load-bearing
//!
//! This crate reads the syntax AV1 puts in front of the coded picture: OBU
//! framing, the sequence header, enough of the frame header to know a
//! picture's type and size, metadata OBUs, and `AV1CodecConfigurationRecord`.
//! It implements no tile decoding, no prediction, no film-grain synthesis, no
//! reconstruction of any kind — there is no sample of output anywhere in it.
//!
//! That line is where the legal argument lives (D7/D15, plan 15 §1.6): a
//! sequence-header parser implements no decoding process, so it ships in the
//! default build even though AV1 decoders do not (yet). AV1 itself needs no
//! patent-posture gate the way H.264/HEVC do — the AOM Patent License covers
//! it — but the parsing/decoding line is drawn the same way regardless, and
//! this crate stays on the parsing side of it.
//!
//! # What is here
//!
//! | Module | Syntax |
//! |---|---|
//! | [`leb`] | `leb128()`, `uvlc()`, `su(n)`, `ns(n)` — §4.10.3–§4.10.7 |
//! | [`obu`] | `obu_header()`, `open_bitstream_unit()`, both OBU framings — §5.2–§5.3, Annex B |
//! | [`seq`] | `sequence_header_obu()`, `color_config()`, `timing_info()`, `decoder_model_info()` — §5.5 |
//! | [`frame_header`] | `uncompressed_header()`'s common prefix and the intra `frame_size()`/`render_size()` path — §5.9 |
//! | [`metadata`] | `metadata_obu()` and its five payload shapes — §5.8 |
//! | [`profile`] | Profiles, tiers and levels — Annex A |
//! | [`av1c`] | `AV1CodecConfigurationRecord` — AV1 Codec ISO Media File Format Binding §2.3.3 |
//! | [`params`] | The [`CodecParameters`] a sequence header implies, and the pixel-format mapping |
//! | [`parser`] | [`Av1Parser`](parser::Av1Parser), the streaming temporal-unit splitter |
//! | [`cbs`] | The [`CbsCodec`](vaco_codec_cbs::CbsCodec) implementation, and this crate's verdict on it |
//!
//! # AV1 is not H.264 or HEVC wearing a different header
//!
//! This crate's brief specifically warned against assuming otherwise, and
//! measurement bore that out. Two examples, both cited where they are
//! implemented:
//!
//! 1. **No `yuvj` family.** H.264 maps full-range 4:2:0/4:2:2/4:4:4 at 8 bits
//!    to `yuvj420p`/`yuvj422p`/`yuvj444p`; HEVC narrows that to 4:2:0 only.
//!    AV1 has neither — measured against `ffmpeg 8.1`, a full-range 4:2:0
//!    8-bit stream stays `yuv420p` with `color_range=pc` reported alongside
//!    it. See [`params::pixel_format`].
//! 2. **Resolution has no separate "coded" and "display" size the way
//!    HEVC's conformance window does.** AV1's `max_frame_width`/
//!    `max_frame_height` *is* the coded and (absent a `render_size()`
//!    override) the displayed size; there is no cropping-by-offsets syntax
//!    at all. See [`params::codec_parameters`].
//!
//! Every other AV1-specific number this crate reports — bit depth, chroma
//! subsampling, colour signalling, profile and level — was checked against
//! `ffprobe -show_streams` on files this crate's own test suite builds from,
//! not assumed from another codec's answer. `docs/codec/vaco-parse-av1.md`
//! has the full measurement log.
//!
//! # Safety on untrusted input
//!
//! This crate parses fully untrusted data:
//!
//! * **No Exp-Golomb, but the same discipline.** `leb128()` and `uvlc()` are
//!   built on [`vaco_bitstream::BitReader`]'s sticky-overrun model: neither
//!   can read past its logical bound or panic, and both cap their own
//!   iteration count against the specification's own limits (`leb128`'s
//!   eight-byte cap, `uvlc`'s 32-bit `leadingZeros` cap) rather than looping
//!   on attacker-chosen input.
//! * **Every loop whose trip count comes from the bitstream is bounded before
//!   it runs.** `operating_points_cnt_minus_1` is a 5-bit field (at most 32
//!   iterations) charged as fuel before the loop starts; every other loop in
//!   this crate (`NUM_REF_FRAMES`, the colour primaries triple, the metadata
//!   fixed shapes) has a compile-time bound.
//! * **OBU sizing is arithmetic on declared lengths, not a scan.** AV1 has no
//!   start codes, so [`obu::units`] cannot resynchronise past a corrupt
//!   length the way a NAL scanner resynchronises past a bad start code —
//!   every size computation in [`obu`] uses checked arithmetic and rejects a
//!   unit whose declared length would run past the buffer, rather than
//!   trusting it.
//! * **No panics.** `unwrap`/`expect`/`panic`/`indexing_slicing` are denied
//!   workspace-wide and nothing here escapes them. The `parse_av1` and
//!   `cbs_av1` fuzz targets exist to keep it that way, tested at 1 byte, 4
//!   KiB and whole-file chunk sizes (`docs/codec/vaco-parse-av1.md` has the
//!   run counts).
//!
//! # Example
//!
//! ```
//! use vaco_limits::{Budget, Limits};
//! use vaco_parse_av1::SequenceHeader;
//!
//! // The OBU_SEQUENCE_HEADER payload from a real `libsvtav1` capture
//! // (642x358, 8-bit 4:2:0, level 2.1), header and size field stripped.
//! let payload = [
//!     0x00, 0x00, 0x00, 0x0c, 0xc5, 0x03, 0x65, 0x00, 0xbe, 0x00, 0x10,
//! ];
//! let mut budget = Budget::new(Limits::strict());
//! let sh = SequenceHeader::parse(&payload, &mut budget)?;
//!
//! assert_eq!((sh.max_frame_width, sh.max_frame_height), (642, 358));
//! assert_eq!(sh.color_config.bit_depth, 8);
//! # Ok::<(), vaco_core::Error>(())
//! ```
//!
//! # Specification
//!
//! AV1 Bitstream & Decoding Process Specification v1.0.0 with Errata 1
//! (AOMedia): §5 for OBU/sequence-header/frame-header syntax, §6 for
//! semantics, Annex A for profiles and levels. AV1 Codec ISO Media File
//! Format Binding §2.3.3 for `av1C`. Nothing here was taken from any
//! implementation (D7) — see `docs/codec/vaco-parse-av1.md` for how the
//! Annex A level table and the pixel-format mapping were cross-checked.
//!
//! # Dependencies
//!
//! `vaco-bitstream` for the reader (this crate supplies its own `leb128`/
//! `uvlc`, since AV1 needs neither Exp-Golomb nor NAL framing), `vaco-limits`
//! for the budget, `vaco-codec-cbs` for the read/modify/write layer,
//! `vaco-codec-core` for the [`Parser`](vaco_codec_core::Parser) trait and
//! [`CodecParameters`], `vaco-color` and `vaco-pixfmt` for the signalling
//! enums, `vaco-packet` for the emitted packets. No external runtime
//! dependencies.

#![forbid(unsafe_code)]

pub mod av1c;
pub mod cbs;
pub mod frame_header;
pub mod leb;
pub mod metadata;
pub mod obu;
pub mod params;
pub mod parser;
pub mod profile;
pub mod seq;

pub use av1c::Av1CodecConfigurationRecord;
pub use cbs::{Av1Cbs, Av1Content, FRAME_UNIT_GRANULARITY_DIVERGENCE};
pub use frame_header::{FrameHeader, FrameSize, FrameType};
pub use metadata::Metadata;
pub use obu::{Av1Framing, ObuHeader, ObuType, ObuUnit};
pub use params::{codec_parameters, color_info, pixel_format};
pub use parser::Av1Parser;
pub use profile::{LEVELS, PROFILES, Tier, level_constraints, level_name, profile, profile_name};
pub use seq::{ColorConfig, OperatingPoint, SequenceHeader, TimingInfo};

// Re-exported so a caller can describe a stream without also depending on
// `vaco-codec-core` directly.
pub use vaco_codec_core::CodecParameters;
