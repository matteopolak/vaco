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
//! - **Deblocking and SAO.** No in-loop filter runs. Verification is against
//!   `ffmpeg -skip_loop_filter all` (and, for the crate's own fixtures,
//!   material encoded with both filters off at the encoder, so there is
//!   nothing for either decoder to disagree about).
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
//! **Not registered.** No `vaco-component.toml` exists for this crate, the
//! same choice `vaco-codec-av1` and `vaco-codec-opus` made while their own
//! decode paths were still gaining verification: a registered decoder is
//! what `vaco` selects for real input, and this crate's cuts (above) are
//! real enough that silent wrongness on an unsupported stream shape would be
//! worse than `vaco -c:v hevc` simply not existing yet. See
//! `docs/codec/vaco-codec-hevc.md` for the measured per-plane agreement this
//! pass reached and what should be re-measured before flipping that switch.

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
