//! Text subtitle containers: demuxers and muxers over the shared model in
//! `vaco-format-subtitle`.
//!
//! # Scope (D17 — measured against `ffmpeg 8.1`, not the plan's count)
//!
//! `ffmpeg -demuxers` / `-muxers` name **16 demuxers and 8 muxers** in this
//! family, not the 15/6 a plan once counted. `sbg` looked like a subtitle
//! format by name and was not one — `ffmpeg -h demuxer=sbg` reports
//! `"SBaGen binaural beats script"`, an audio-synthesis format, and is out of
//! scope here. `ttml` is a **muxer only**: there is no `ttml` demuxer in the
//! reference at all, so [`ttml`]'s demuxer is implemented from the W3C TTML1
//! spec with no reference to differential-test against — flagged as such
//! everywhere it matters.
//!
//! | | Demux | Mux | Codec ID |
//! |---|---|---|---|
//! | [`srt`] | yes | yes | `CodecId::SubRip` (the reference's `subrip`; a second, distinct `srt` codec — "`SubRip` subtitle with embedded timing" — exists in `ffmpeg -codecs` and is not what this demuxer produces) |
//! | [`webvtt`] | yes | yes | `CodecId::Webvtt` |
//! | [`ass`] | yes | yes | `CodecId::Ass` (one demuxer for SSA v4 and ASS v4+ scripts alike — measured, see the module) |
//! | [`scc`] | yes | yes | `CodecId::Eia608` |
//! | [`microdvd`] | yes | yes | `CodecId::Microdvd` |
//! | [`jacosub`] | yes | yes | `CodecId::Jacosub` |
//! | [`lrc`] | yes | yes | `CodecId::Text` (generic — measured, see below) |
//! | [`ttml`] | yes (spec-only) | yes | `CodecId::Ttml` |
//! | [`subviewer`] | yes | no (reference has no encoder) | `CodecId::Subviewer` |
//! | [`subviewer1`] | yes | no | `CodecId::Subviewer1` |
//! | [`mpsub`] | yes | no | `CodecId::Text` (generic — measured, see below) |
//! | [`pjs`] | yes | no | `CodecId::Pjs` |
//! | [`realtext`] | yes | no | `CodecId::Realtext` |
//! | [`sami`] | yes | no | `CodecId::Sami` |
//! | [`vplayer`] | yes | no | `CodecId::Vplayer` |
//! | [`mpl2`] | yes | no | `CodecId::Mpl2` |
//! | [`stl`] | yes | no | `CodecId::Stl` |
//!
//! # The `CodecId` gap — closed
//!
//! `vaco_codec_core::CodecId` is a closed, non-exhaustive enum: only the
//! crate that defines it can add a variant, and `vaco-codec-core` was not in
//! this crate's scope. Eleven of the formats above had no variant of their
//! own at first — reported rather than worked around, per
//! `planning/AGENT-CONSTRAINTS.md`'s "Scope" — and the owning agent then
//! added `Jacosub`, `Microdvd`, `Mpl2`, `Pjs`, `Realtext`, `Sami`, `Stl`,
//! `Subviewer`, `Subviewer1`, `Ttml`, `Vplayer`, and a generic `Text` (probed
//! from `ffmpeg -codecs` 8.1, independently of the names this crate had
//! guessed at — `Lrc` and `Mpsub` were **not** added, because the reference
//! genuinely has no codec for either and both measure as the generic `Text`,
//! matching what this crate had already found). Every format above now
//! carries the codec its demuxer/muxer actually produces, so `vaco-probe`
//! prints the reference's own `codec_name` rather than `unknown`.
//!
//! # The demuxer/decoder boundary
//!
//! Every demuxer here does exactly one job: recover `(start, end, text)`
//! triples and hand them out as packets. None of them interpret ASS override
//! tags, SAMI's HTML fragments, or anything else inside the text — that is
//! rendering, which is a decoder's job in a different wave (see
//! `planning/AGENT-CONSTRAINTS.md`, "Detection and demuxing ask different
//! questions"). [`engine::CueDemuxer`] is the one generic type every format's
//! `open` function builds; the sixteen-plus formats differ only in how bytes
//! become [`vaco_format_subtitle::Cue`]s.

#![forbid(unsafe_code)]

pub mod engine;

pub mod ass;
pub mod jacosub;
pub mod lrc;
pub mod microdvd;
pub mod mpl2;
pub mod mpsub;
pub mod pjs;
pub mod realtext;
pub mod sami;
pub mod scc;
pub mod srt;
pub mod stl;
pub mod subviewer;
pub mod subviewer1;
pub mod ttml;
pub mod vplayer;
pub mod webvtt;
