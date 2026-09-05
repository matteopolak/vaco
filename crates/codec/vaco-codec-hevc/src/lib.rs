//! H.265/HEVC video decode — I/P/B-slices, 8-bit 4:2:0 only.
//!
//! # Scope, stated once
//!
//! This crate decodes the mainstream shape a real encoder emits: I-, P- and
//! B-slices all real, not just parsed. NAL/parameter-set resolution (reusing
//! `vaco-parse-hevc`, which owns SPS/PPS/slice-header parsing already) into a
//! CTU quadtree walk, intra prediction (planar/DC/33 angular modes) and
//! motion compensation (merge/AMVP, uni- and bi-predictive, weighted or not),
//! transform-tree recursion, dequantisation and the inverse transform,
//! deblocking and SAO, composed into real reconstructed pixels — verified
//! byte-exact against a fully stock, completely unmodified `libx265`
//! invocation, not just a restricted one. It is the HEVC analogue of
//! `vaco-codec-h264`'s CABAC macroblock layer, following the same shape:
//! entropy decode and reconstruction are real and tested against a real
//! encoder's output, verified plane-by-plane.
//!
//! What each bullet below covers, in the order support for it landed —
//! several were genuinely **not** in scope for a while and refused by name
//! ([`vaco_core::Error::Unsupported`]) before their own pass added them.
//! The last bullet is what remains refused today:
//!
//! - **P-slices are implemented and no longer refused.**
//!   `prediction_unit()` syntax (skip/merge/AMVP), merge/AMVP candidate
//!   derivation (§8.5.3.2), motion compensation (§8.5.3.3), the inter CABAC
//!   context tables (§9.3.2.2), and [`dpb`]'s reference-picture management
//!   (§8.3.2/§8.3.4, Annex C output-reordering "bumping") are all wired into
//!   [`decoder`]. The TMVP-for-AMVP defect that used to block this (a
//!   bottom-right-to-centre temporal fallback gated on "geometrically
//!   available" rather than "yielded a candidate") is fixed — see
//!   `docs/codec/vaco-codec-hevc.md`'s "Stage 2: P-slices" section for the
//!   root cause and the real `libx265` fixture it was found against.
//! - **Weighted prediction (`weighted_pred_flag`, §8.5.3.3.4.3) is
//!   implemented** — see [`weight`]'s own module doc for the per-`ref_idx`
//!   weight/offset resolution and [`mc`]'s
//!   `predict_block_intermediate`/`apply_weight` for the explicit weighted
//!   sample prediction process itself. `pred_weight_table()` was already
//!   parsed by `vaco-parse-hevc`; this closes the gap where the table was
//!   parsed and thrown away. Verified byte-exact against plain `ffmpeg` on
//!   real `libx265` output with genuinely non-neutral weights (confirmed by
//!   inspecting the parsed table, not assumed) — see
//!   `docs/codec/vaco-codec-hevc.md`'s weighted-prediction section for the
//!   fixtures and values.
//! - **B-slices are implemented and no longer refused.**
//!   `inter_pred_idc` parsing (§7.3.8.6) and the rest of a B-slice's
//!   `prediction_unit()` (`ref_idx_l1`/`mvd_coding(x, y, 1)`/`mvp_l1_flag`),
//!   `RefPicList1` construction ([`dpb`]'s `build_ref_pic_lists`, generic
//!   over list index from the P-slice pass onward), combined bi-predictive
//!   merge candidates (§8.5.3.2.4), `collocated_from_l0_flag`-aware temporal
//!   motion vector prediction (§8.5.3.2.9), default and explicit-weighted
//!   bi-predictive motion compensation (§8.5.3.3.4.2/.3 — the
//!   `weighted_bipred_flag` half of weighted prediction the bullet above
//!   leaves implicit is this one, [`weight`]'s `resolve_list` rather than an
//!   L0-only `resolve_l0`), B-slice CABAC context initialisation
//!   (§9.3.2.2), and full Table 8-12 boundary-strength derivation for
//!   deblocking across bi-predicted edges are all wired into [`decoder`].
//!   See `docs/codec/vaco-codec-hevc.md`'s "B-slices... landed" section for
//!   the two Annex C DPB-bumping defects a real hierarchical-B stream
//!   surfaced (fixed there, unrelated to the inter-prediction work itself)
//!   and the fixtures below. Verified byte-exact against a fully stock,
//!   completely unmodified `libx265` invocation (zero `-x265-params`,
//!   `libx265`'s own default GOP structure and B-frame count) at multiple
//!   resolutions, a deep hierarchical-B GOP forced explicitly, and — with
//!   `weightb=1` added specifically to exercise it, since stock `libx265`
//!   does not turn it on by default — weighted bi-prediction itself, with
//!   genuinely non-neutral, distinct weight/offset pairs confirmed on both
//!   lists.
//! - **Deblocking (§8.7.2) is implemented** — see [`deblock`]'s own module
//!   doc for the algorithm, what it reuses from HM 18.0 (Tier A), and why
//!   it does not reuse `vaco-codec-dsp-deblock` (a genuinely different
//!   clause 8.7-shaped algorithm, not the same one at a different call
//!   site). Verified byte-exact against plain `ffmpeg` (no
//!   `-skip_loop_filter`) on real `libx265` output.
//! - **SAO (§7.3.8.3 syntax, §8.7.3 filtering) is implemented** — see
//!   [`sao`]'s own module doc for the per-CTU `sao()` parse (merge/new mode,
//!   band and edge offset) and the filtering process. Verified byte-exact
//!   against plain `ffmpeg` on real `libx265` output with SAO left at its
//!   own (on) default, at multiple resolutions and QPs.
//! - **`transform_skip_flag` (§7.3.8.11 syntax, §8.6.4.2 residual) is
//!   implemented and no longer refused.** The flag was already parsed —
//!   correctly, in all four residual call sites — and then refused by name
//!   whenever it decoded to `1`, which is 26 of the 46 JCT-VC `HEVC_v1`
//!   conformance streams this repo registers in `vaco-media.lock`, the
//!   single largest refusal in that corpus by a wide margin.
//!   [`transform::TransformKind`] now carries the §8.6.4.2 branch choice as
//!   one value rather than a `use_dst` flag beside a separate skip flag.
//!   Verified byte-exact against `ffmpeg` on real `libx265 --tskip` output
//!   (`tests/tskip.rs`, plus an I/P/B encode measured out of tree).
//! - **Tiles.** Refused; only a plain single-tile picture is decoded.
//! - **Wavefront (`entropy_coding_sync_enabled_flag`) is implemented** — see
//!   `decoder::decode_wpp_rows`'s own doc for the per-row CABAC-substream
//!   split, the §9.3.2.3 context save/restore, and why entry-point offsets
//!   must be applied to the *coded* (still-escaped) slice segment data
//!   rather than the de-escaped RBSP a naive reading of `cabac_data`'s own
//!   byte positions would use. Verified byte-exact against plain `ffmpeg`
//!   on real `libx265` output with WPP left at its own (on) default,
//!   alongside deblocking and SAO also at their own defaults, at multiple
//!   resolutions including a partial last CTU row and column.
//! - **`cu_qp_delta` (adaptive per-CU QP, §7.3.8.11/§8.6.1) is
//!   implemented** — see `ctu.rs`'s own module-level derivation
//!   (`qp_y_pred`/`derive_qp_y`/`maybe_parse_cu_qp_delta`) for
//!   `cu_qp_delta_abs`/`sign`'s binarisation and the quantisation-group
//!   `QpY` derivation, and [`deblock`]'s own module doc for why its
//!   `qP_P == qP_Q` constant-QP shortcut had to go the moment two coding
//!   units on either side of an edge could genuinely disagree. Verified
//!   byte-exact against plain `ffmpeg` on a fully stock `libx265` output
//!   (default CRF rate control, which implies this), at multiple
//!   resolutions and CRFs, alongside deblocking/SAO/WPP all at their own
//!   defaults too.
//! - **`cu_transquant_bypass_flag` (§7.3.8.5 syntax, §8.6.4.1
//!   reconstruction) is implemented.** CABAC parses the flag before the rest
//!   of each CU, coefficient levels become residual samples directly, and
//!   sign hiding and `transform_skip_flag` are suppressed as specified.
//!   Deblocking and SAO consult the same per-CU filter-bypass mask already
//!   used for protected I_PCM samples. Verified byte-exact against
//!   `ffmpeg 9.0.1` on JCT-VC `ipcm_D_NEC_3` (`tests/ipcm.rs`).
//! - **Custom scaling lists (§7.4.5/§8.6.3) are implemented.** One resolved
//!   [`transform::ScalingMatrices`] value applies PPS-over-SPS precedence,
//!   default/copy inference and diagonal-scan placement once per active
//!   parameter-set pair; all four luma/chroma, intra/inter dequantisation
//!   paths consume it. JCT-VC `SLIST_C_Sony_4` checks the switch from default
//!   to explicitly signalled matrices byte-for-byte (`tests/scaling_list.rs`).
//! - **Refused by name today**, each with its own
//!   [`vaco_core::Error::Unsupported`] string: bit depths other than 8,
//!   chroma formats other than 4:2:0, `separate_colour_plane_flag`,
//!   tiles, chroma QP offset lists, every SPS/PPS range-extension and
//!   screen-content-coding flag,
//!   long-term reference pictures. Dependent slice segments inherit their
//!   preceding independent header and CABAC context, while independent
//!   segments restart both. Multiple independent segments are decoded when
//!   they share their picture-wide context, enable filtering across their
//!   boundaries, and do not use WPP; WPP multi-segment combinations are
//!   refused by name rather than decoded with the wrong neighbour
//!   availability. The SPS/PPS ones are refused at
//!   `check_scope`; the rest the moment the bitstream actually uses the
//!   feature, so a PPS that declares a flag it never exercises decodes fine.
//!
//! `docs/codec/vaco-codec-hevc.md`'s "The JCT-VC `HEVC_v1` subset,
//! measured" section is the standing record of what this list costs on real
//! conformance bitstreams: 39 of 46 byte-exact against `ffmpeg`'s own decode,
//! one exact against its archive-published checksum, and 6 refused by name,
//! with no wrong-output or CABAC-desync cases.
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
//! all-intra) at the time this paragraph was written: see
//! `docs/codec/vaco-codec-hevc.md`'s "Registration" section for the exact
//! command originally verified. SAO, WPP and `cu_qp_delta` are all
//! implemented now (their own bullets above), which closes the last gap
//! between this crate's own byte-exact fixtures and a fully stock `libx265`
//! invocation (`-c:v libx265 -x265-params log-level=none`, nothing else):
//! default CRF rate control implies both WPP and adaptive per-CU QP, and
//! neither is refused any more.

#![forbid(unsafe_code)]

mod cabac_ctx;
mod ctu;
mod deblock;
mod decoder;
mod dpb;
mod framebuf;
mod intra_mode;
mod intra_pred;
mod mc;
mod motion;
mod residual;
mod sao;
mod scan;
mod transform;
mod wavefront;
mod weight;

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
