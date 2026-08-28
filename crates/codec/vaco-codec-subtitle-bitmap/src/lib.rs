//! DVB (ETSI EN 300 743), DVD/`VobSub` SPU and PGS/HDMV bitmap subtitle
//! decode: run-length pixel decompression, CLUT/palette resolution, and
//! region/window/object composition into a
//! [`vaco_format_subtitle_bitmap::IndexedBitmap`].
//!
//! # Why this is a standalone library, not a `vaco_codec_core::Decoder`
//!
//! `vaco_codec_core::Decoder::receive_frame` returns a `vaco_frame::Frame`,
//! and `vaco_frame::FrameData` is a closed `Video`/`Audio` enum with no
//! subtitle variant to hold this crate's output — recorded as interface gaps
//! 17/18 (commit `e54161c`, ahead of this crate) precisely so the next reader
//! would not go looking for a `Decoder` impl that has nowhere to route its
//! result. This crate exposes its own decode entry point per format instead:
//! [`dvb::decode_display_set`]/[`dvb::DvbSubDecoder`],
//! [`pgs::PgsDecoder`], [`vobsub::decode_spu`].
//!
//! # Shared output shape
//!
//! Every format converges on [`SubtitleEvent`]: a start/end time, a forced
//! flag (PGS only — DVB and `VobSub` have no such bit) and zero or more
//! [`vaco_format_subtitle_bitmap::IndexedBitmap`]s, each already carrying its
//! absolute canvas position in its own `rect()`. [`rgba::to_rgba`] expands
//! one to packed RGBA8 for pixel-level comparison against a reference
//! decoder.
//!
//! # Dependencies
//!
//! `vaco-format-subtitle-bitmap` for the shared [`Rect`]/[`Palette`]/
//! [`IndexedBitmap`] shapes, and `vaco-subtitle-bitmap` for the
//! already-written, already-fuzzed segment/header parsing each format's own
//! demuxer uses (`dvbsub::segments`, `sup`'s segment header, `vobsub::idx`) —
//! this crate builds the run-length decompression and composition on top of
//! that rather than re-deriving segment framing from scratch.

#![forbid(unsafe_code)]

pub mod decoder;
pub mod dvb;
pub mod pgs;
pub mod rgba;
pub mod vobsub;

pub use vaco_format_subtitle_bitmap::{IndexedBitmap, Palette, Rect, Rgba};

use vaco_core::Duration;

/// One decoded, positioned display event.
///
/// `end` is `None` when the format itself never states a duration (DVB's
/// `page_time_out` and `VobSub`'s stop time both resolve to `Some`; a caller
/// combining this with a container's own next-cue timing decides what "no
/// end yet" means for its own pipeline).
#[derive(Debug, Clone, Default)]
pub struct SubtitleEvent {
    pub start: Duration,
    pub end: Option<Duration>,
    /// PGS's own composition-object flag: a forced (non-skippable) subtitle,
    /// e.g. burned-in signage translation, distinct from an ordinary
    /// closed-caption-style subtitle a viewer can turn off. DVB and `VobSub`
    /// have no equivalent bit and always report `false`.
    pub forced: bool,
    pub rects: Vec<IndexedBitmap>,
}
