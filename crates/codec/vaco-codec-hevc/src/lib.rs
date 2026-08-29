//! H.265/HEVC intra-only video decode — I-slices, 8-bit 4:2:0 only.
//!
//! # Scope, stated once
//!
//! This crate decodes the mainstream shape a real encoder emits for an
//! all-intra sequence: NAL/parameter-set resolution (reusing
//! `vaco-parse-hevc`, which owns SPS/PPS/slice-header parsing already) into a
//! CTU quadtree walk, intra prediction (planar/DC/33 angular modes),
//! transform-tree recursion, dequantisation and the inverse transform,
//! composed into real reconstructed pixels. It is the HEVC analogue of
//! `vaco-codec-h264`'s CABAC macroblock layer, following the same shape:
//! entropy decode and reconstruction are real and tested against a real
//! encoder's output, verified plane-by-plane.
//!
//! Deliberately **not** in scope, each refused by name
//! ([`vaco_core::Error::Unsupported`]) rather than attempted shallowly:
//!
//! - **Inter prediction, B/P slices.** I-slices only.
//! - **Deblocking and SAO.** No in-loop filter runs. Deblocking is purely a
//!   post-reconstruction pixel filter with no bitstream footprint beyond
//!   the slice header (already parsed), so an encode with it on just
//!   reconstructs unfiltered pixels — a real, bounded deviation, not a
//!   crash. SAO is different and refused outright at the SPS
//!   (`sample_adaptive_offset_enabled_flag`): §7.3.8.3's `sao()` syntax is
//!   *in* the bitstream, once per CTU, and this crate parses none of it —
//!   decoding a stream that actually turns SAO on would desync the entropy
//!   decoder from the first merged/offset CTU onward, not just reconstruct
//!   different pixels. Verification is against `ffmpeg -skip_loop_filter
//!   all` for deblocking, and against material encoded with `no-sao=1` for
//!   SAO, so there is nothing for either side to disagree about.
//! - **Tiles and wavefront (`entropy_coding_sync`).** Refused; only a plain
//!   single-tile, independent-slice-segment picture is decoded.
//! - **`cu_qp_delta` / adaptive per-CU QP, chroma QP offset lists, `I_PCM`,
//!   `transform_skip` actually taken, custom scaling lists, and every SPS
//!   range-extension flag.** Each is refused the moment the bitstream
//!   actually uses it, not merely when the syntax element exists — a PPS
//!   that declares `transform_skip_enabled_flag` but never sets the
//!   per-block flag decodes fine.
//! - **Bit depths other than 8, chroma formats other than 4:2:0,
//!   monochrome.** Refused at the SPS.
//!
//! # Reuse, not reimplementation
//!
//! - [`vaco_codec_cabac`] supplies the arithmetic engine and
//!   `ContextModel::init_hevc`/`init_contexts_hevc`; this crate owns only the
//!   HEVC-specific context *tables* and `ctxIdxInc` derivations (D14.1: a
//!   shared layer never encodes one codec's syntax).
//! - [`vaco_codec_dsp_idct::hevc`] supplies the whole 4x4-DST/4x4..32x32-DCT
//!   inverse-transform family — already implemented, tested and documented
//!   as property-verified in that crate.
//! - [`vaco_codec_dsp_intrapred`] supplies the shared DC/planar/angular-project
//!   arithmetic; this crate owns HEVC's mode table, reference-sample
//!   construction/substitution/smoothing and the chroma derived-mode rule,
//!   none of which the shared crate can know about.
//!
//! # Provenance
//!
//! Clean-room per D7/D15: the ITU-T H.265 (08/2021) specification and the
//! **HM reference software** (BSD-3-Clause, Tier A per
//! `planning/research/07-legal-patents-licensing.md` §1.6.1 — a permissively
//! licensed reference implementation, not `FFmpeg`) were both read directly to
//! derive the context tables and syntax-element derivations below; `FFmpeg`'s
//! source was never opened. See `provenance/vaco-codec-hevc.toml` and each
//! module's own `Vaco-Spec-Ref`-cited commit.
//!
//! # Registration
//!
//! **Registered, patent-encumbered-gated.** `vaco-component.toml` declares
//! this decoder with `encumbered = true` / `default = false` behind the
//! `patent-encumbered-hevc-decode` feature — the same D4/D4.1 posture as
//! `vaco-codec-h264` (HEVC decode is covered by multiple patent pools; see
//! `planning/research/07-legal-patents-licensing.md`). Registration was held
//! back while a real `libx265`-encoded fixture showed structured pixel error
//! on multi-coefficient-group (8x8+) residual blocks — see
//! `docs/codec/vaco-codec-hevc.md` for that defect's root cause (a
//! `sig_coeff_flag` DC-context bug in [`residual`]) and the bin-trace method
//! that found it, plus two more real bugs closing this pass found once the
//! real `vaco` binary (not this crate's own tests) was the thing being
//! measured: `hvcC`/AVCC-equivalent extradata was accepted and discarded
//! rather than parsed (so `vaco -i real.mp4` decoded nothing at all — the
//! exact `vaco-codec-h264` history repeating, fixed the same way, by
//! embedding [`vaco_parse_hevc::HevcParser`] instead of a second,
//! ad-hoc parameter-set store), and the luma MPM "above" candidate reused
//! a real, already-decoded neighbour's mode across a CTB row boundary
//! instead of §8.4.2's forced-`INTRA_DC` special case there — invisible to
//! any single-CTU fixture, since it only fires once a *second* row of CTUs
//! exists. A real `libx265` stream now decodes byte-exact per plane
//! against `ffmpeg`'s own decode — measured in-crate
//! (`tests/oracle.rs::dense_content_is_byte_exact`) and via the real
//! `vaco` binary, full-length, multi-row, multi-column, all three planes —
//! within this crate's stated scope above (`no-sao`, `wpp=0`, constant QP,
//! all-intra): see `docs/codec/vaco-codec-hevc.md`'s "Registration"
//! section for the exact verified command and what a stream that also
//! turns SAO/WPP/adaptive-QP on gets instead (a clean, named refusal, not
//! a crash or wrong pixels).

#![forbid(unsafe_code)]

mod cabac_ctx;
mod ctu;
mod decoder;
mod framebuf;
mod intra_mode;
mod intra_pred;
mod residual;
mod scan;
mod transform;

pub use decoder::HevcDecoder;

/// The registry descriptor for this crate's decoder.
///
/// `caps: Caps::PATENT_ENCUMBERED` is the code-level half of D4's gating,
/// mirroring `vaco-codec-h264::DECODER_H264` exactly — see this module's own
/// "Registration" doc, and that crate's, for the other half (the
/// `vaco-component.toml` fragment's `encumbered = true` / `default = false`
/// pair, which is what `cargo xtask patent-gate` actually checks).
pub const DECODER_HEVC: ::vaco_codec_core::DecoderDesc = ::vaco_codec_core::DecoderDesc {
    name: "hevc",
    long_name: "H.265 / HEVC",
    id: ::vaco_codec_core::CodecId::Hevc,
    media_type: ::vaco_core::MediaType::Video,
    caps: ::vaco_codec_core::Caps::PATENT_ENCUMBERED,
    supported_rates: &[],
    make: |limits| ::std::boxed::Box::new(decoder::HevcDecoder::new(limits)),
};
