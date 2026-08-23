//! Granule position → timestamp, per codec.
//!
//! This is the part plan 18 §3.4.5 calls out as "the one place a container
//! needs codec knowledge" — resolved, as the plan directs, with a per-mapping
//! interpreter that lives inside this crate rather than a dependency on any
//! codec crate. **Every mapping below is measured against `ffmpeg -f ogg`
//! (or its sibling `oga`) for the codecs a working encoder exists for in
//! this environment**, not assumed from a spec reading — see the doc
//! comments and `docs/format/vaco-demux-ogg.md` for the exact commands and
//! numbers.
//!
//! # The two facts that make this non-trivial
//!
//! 1. A page's granule position states where decode stands **after the last
//!    packet that finishes on this page** (RFC 3533 §6). It says nothing
//!    about the packets that finished earlier in the same page.
//! 2. Packets carry no timestamp of their own. So a page with `n` packets
//!    completing on it has one authoritative data point (the page's own
//!    granule) and `n` unknowns.
//!
//! # The algorithm
//!
//! [`GranuleTimeline`] keeps a running cursor, in the stream's own tick unit
//! (samples for every audio mapping, frames for Theora). For each packet
//! completed on a page, in order:
//!
//! 1. Ask [`GranuleMapping::nominal_duration`] (Opus: exact, via
//!    [`vaco_codec_core::Parser::packet_duration`] over the packet's own
//!    bytes, reached through `ParserProvider` per D14.1; Speex: exact, from
//!    the header's own `frame_size × frames_per_packet`; Theora: exact, one
//!    tick per frame; Vorbis: **approximate**, a constant derived from the
//!    identification header's `blocksize_1`, described below) for a
//!    provisional duration and advance the cursor by it.
//! 2. When the page's own granule position is known (i.e. not the reserved
//!    `-1`), **snap the last packet completed on that page** so the cursor
//!    lands exactly on `GranuleMapping::timestamp(granule)`. This is what
//!    keeps every page boundary exact even when the per-packet estimate
//!    inside the page is only a guess — which for Vorbis it is.
//!
//! Consequence, stated plainly: **cross-page drift cannot accumulate.** The
//! only place an estimate can be visibly wrong is the intra-page
//! distribution for a Vorbis page that switches block sizes, and even there
//! the *last* packet on the page is still exactly right.
//!
//! # Vorbis: measured, and why it is an approximation
//!
//! The Vorbis I specification defines a packet's sample contribution as
//! `(current_blocksize + previous_blocksize) / 4`, where blocksize is either
//! of the two sizes named in the identification header and *which one* a
//! given packet uses is a single bit inside the **setup header** — reached
//! only by walking through the codebook, floor, residue and mapping counts
//! that precede the mode list. That is a real bitstream parser, and this
//! crate does not carry one (D14.1's carve-out is for granule interpretation,
//! not for building a second Vorbis decoder inside a container crate).
//!
//! **Measured** (`ffmpeg -f lavfi -i sine -ac 2 -c:a vorbis -q:a 4
//! -strict -2 vorbis.ogg`, `blocksize_0` = `blocksize_1` = 2048 in the
//! identification header): every packet's `ffprobe -show_packets` duration
//! is exactly 1024 — `blocksize_1 / 2` — including the first, and the page
//! granule positions (`44032`, `88256`) are exact multiples of it up to a
//! final truncated packet. `blocksize_1 / 2` is `(blocksize_1 +
//! blocksize_1) / 4`: this crate assumes every packet uses the long block,
//! which is exact for content that never switches (this test tone, and a
//! large share of real audio) and a documented approximation otherwise. A
//! stream that switches blocks will show the error **only on the packets
//! either side of the switch**, bounded by the next page's exact snap.
//!
//! # Opus: measured
//!
//! `pre_skip` (from `OpusHead`, RFC 7845 §4) is subtracted from the granule
//! position to get the playable-sample position, and the very first
//! decoded sample is therefore at a **negative** timestamp. Measured
//! (`ffmpeg -c:a libopus`, `pre_skip = 312`): `ffprobe -show_packets`
//! reports the first packet's `pts = -312`, and every page's granule minus
//! `pre_skip` matches the running sum of `Parser::packet_duration` exactly.
//! The final packet's nominal duration (a full 960-sample frame) overruns
//! the last page's granule by design — encoders pad the last frame — and is
//! trimmed by the snap in step 2 above; measured trim on the same file is
//! from 960 down to 312.
//!
//! # FLAC: measured, Theora and Speex: implemented from specification only
//!
//! FLAC-in-Ogg's granule is a plain sample count (measured: page granules
//! `46080` and `88200` on a 44.1 kHz mono file are exact multiples of the
//! encoder's constant 4608-sample block, `ffprobe`'s packet durations agree).
//! This crate does not parse the FLAC frame header's own block-size field
//! (which would give an exact per-packet duration the way Speex's fixed
//! `frame_size` does); it falls back to [`crate::demux`]'s byte-length-weighted
//! distribution across the page, exact for a constant block size and a
//! measured improvement over an even split when the final frame is short —
//! see that module's doc comment for the number. Theora's keyframe/offset
//! split is implemented directly from the published specification (§7.4.4)
//! — **no Theora encoder exists in this environment** (`ffmpeg -encoders`
//! confirmed no `theora` row) so it is unmeasured. Speex's per-packet sample
//! count is read straight from its own header field, needing no measurement
//! to be exact — but no Speex encoder was available to confirm the header
//! layout against a real file either.

use vaco_codec_core::Parser;
use vaco_core::Rational;

use crate::codec::OggCodec;

/// Ogg's own reserved value: no packet finishes on this page.
pub use crate::page::GRANULE_UNSET;

/// Per-stream state for interpreting its granule position and estimating
/// per-packet durations.
#[derive(Debug, Clone)]
pub enum GranuleMapping {
    /// A plain, unscaled sample (or, for an unrecognised codec, "granule
    /// unit") count: FLAC, Speex's granule interpretation, and the fallback
    /// for anything this crate cannot identify.
    SampleCount,
    /// RFC 7845 §4: 48 kHz samples including `pre_skip`.
    Opus { pre_skip: u16 },
    /// Vorbis I spec §4.3: sample count, with the approximate constant
    /// per-packet duration described in the module docs.
    Vorbis { nominal: i64 },
    /// A fixed, header-stated per-packet sample count. Exact, unlike Vorbis.
    Speex { samples_per_packet: i64 },
    /// Theora spec §7.4.4: `frame_number = (granule >> shift) + (granule &
    /// ((1 << shift) - 1))`.
    Theora { granule_shift: u32 },
}

impl GranuleMapping {
    /// Build the mapping for `codec` from its already-parsed BOS packet,
    /// falling back to [`Self::SampleCount`] when the fixed fields cannot be
    /// read (a short or malformed identification header) or the codec is
    /// unrecognised — packets still flow, just without an interpreted
    /// timestamp beyond the raw granule passing through unscaled.
    #[must_use]
    pub fn from_bos(codec: OggCodec, bos_packet: &[u8]) -> Self {
        match codec {
            OggCodec::Opus => {
                crate::codec::parse_opus_head(bos_packet).map_or(Self::SampleCount, |h| {
                    Self::Opus {
                        pre_skip: h.pre_skip,
                    }
                })
            }
            OggCodec::Vorbis => {
                crate::codec::parse_vorbis_ident(bos_packet).map_or(Self::SampleCount, |v| {
                    Self::Vorbis {
                        // `blocksize_1` is a power of two from a 4-bit exponent, so
                        // halving by a right shift is exact — and, unlike `/ 2`, is
                        // not `clippy::integer_division` (which exists to flag
                        // truncating division, not a bit shift that cannot lose
                        // anything here).
                        nominal: i64::from(v.blocksize_1) >> 1,
                    }
                })
            }
            OggCodec::Speex => {
                crate::codec::parse_speex_ident(bos_packet).map_or(Self::SampleCount, |s| {
                    Self::Speex {
                        samples_per_packet: i64::from(s.frame_size)
                            .saturating_mul(i64::from(s.frames_per_packet.max(1))),
                    }
                })
            }
            OggCodec::Theora => {
                crate::codec::parse_theora_ident(bos_packet).map_or(Self::SampleCount, |t| {
                    Self::Theora {
                        granule_shift: t.granule_shift,
                    }
                })
            }
            OggCodec::Flac | OggCodec::Unknown => Self::SampleCount,
        }
    }

    /// The stream-tick position `granule` denotes, or `None` for the
    /// reserved "no packet finishes here" value.
    #[must_use]
    pub fn timestamp(&self, granule: i64) -> Option<i64> {
        if granule == GRANULE_UNSET {
            return None;
        }
        Some(match self {
            Self::SampleCount | Self::Vorbis { .. } | Self::Speex { .. } => granule,
            Self::Opus { pre_skip } => granule.saturating_sub(i64::from(*pre_skip)),
            Self::Theora { granule_shift } => {
                let shift = (*granule_shift).min(62);
                let g = granule.max(0);
                let mask = (1i64 << shift) - 1;
                let keyframe = g >> shift;
                let offset = g & mask;
                keyframe.saturating_add(offset)
            }
        })
    }

    /// The cursor position before the very first packet of the stream is
    /// decoded. Negative for Opus (pre-skip) and Vorbis (the first packet's
    /// own priming, per the module docs); zero otherwise.
    #[must_use]
    pub fn initial_cursor(&self) -> i64 {
        match self {
            Self::Opus { pre_skip } => -i64::from(*pre_skip),
            Self::Vorbis { nominal } => -*nominal,
            Self::SampleCount | Self::Speex { .. } | Self::Theora { .. } => 0,
        }
    }

    /// A codec-fixed estimate of one packet's duration, when this mapping
    /// carries one. `None` means "ask the registered parser, or divide the
    /// page evenly" — see [`GranuleTimeline::assign`].
    #[must_use]
    pub fn fixed_duration(&self) -> Option<i64> {
        match self {
            Self::Vorbis { nominal } => Some(*nominal),
            Self::Speex {
                samples_per_packet, ..
            } => Some(*samples_per_packet),
            Self::Theora { .. } => Some(1),
            Self::SampleCount | Self::Opus { .. } => None,
        }
    }
}

/// The stream time base a mapping implies. Fixed by the specifications, not
/// read off any header field: Opus is always 48 kHz on the wire (RFC 7845
/// §2; the *header's* `input_sample_rate` is purely informational — the same
/// fact `vaco-parse-opus` documents and that this crate must not
/// contradict), and Theora counts frames.
#[must_use]
pub fn opus_time_base() -> Rational {
    Rational::new(1, 48_000)
}

/// Assigns timestamps to the packets completed on one page.
///
/// Kept free of any per-stream mutable state beyond the cursor it is handed
/// and hands back, so a fuzz target or a unit test can drive it directly
/// without constructing a whole [`crate::demux::OggDemuxer`].
#[derive(Debug, Clone, Copy)]
pub struct GranuleTimeline {
    cursor: i64,
    started: bool,
}

impl GranuleTimeline {
    /// A timeline that has not seen a packet yet. The real starting cursor
    /// (which may be negative) is adopted on the first call to
    /// [`Self::assign`], because it depends on `mapping` and the first
    /// packet's own nominal duration, and delaying it is simpler than asking
    /// every caller to compute it up front.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cursor: 0,
            started: false,
        }
    }

    /// Current cursor: the tick position the *next* packet will start at.
    #[must_use]
    pub const fn cursor(&self) -> i64 {
        self.cursor
    }

    /// The cursor a caller should plan against *before* calling
    /// [`Self::assign`] — the real cursor once started, or `mapping`'s
    /// initial cursor beforehand.
    ///
    /// Exists so a caller estimating a fallback per-packet duration (equal
    /// division across a page, for a codec with no fixed or parser-derived
    /// duration) can compute it from the correct starting point — `-pre_skip`
    /// for a parser-less Opus stream, not zero — without duplicating
    /// [`Self::assign`]'s own lazy-start logic.
    #[must_use]
    pub fn planned_cursor(&self, mapping: &GranuleMapping) -> i64 {
        if self.started {
            self.cursor
        } else {
            mapping.initial_cursor()
        }
    }

    /// Assign `(pts, duration)` to every packet in `nominal_durations`, which
    /// are completed **in order** on one page whose granule position is
    /// `page_granule` (already the raw field — `-1` and every other value are
    /// both handled).
    ///
    /// When the page's granule is known, the *last* entry's duration is
    /// adjusted so the cursor lands exactly on
    /// `mapping.timestamp(page_granule)` — see the module docs for why this
    /// bounds drift to within one page. A nominal duration is clamped to
    /// non-negative before use; a hostile or buggy estimate cannot walk the
    /// cursor backwards.
    #[must_use]
    pub fn assign(
        &mut self,
        mapping: &GranuleMapping,
        page_granule: i64,
        nominal_durations: &[i64],
    ) -> Vec<(i64, i64)> {
        if !self.started {
            self.cursor = mapping.initial_cursor();
            self.started = true;
        }
        let mut out = Vec::new();
        for &d in nominal_durations {
            let d = d.max(0);
            out.push((self.cursor, d));
            self.cursor = self.cursor.saturating_add(d);
        }
        if let (Some(target), Some(last)) = (mapping.timestamp(page_granule), out.last_mut()) {
            // Snap: the last packet's duration is whatever makes the cursor
            // land exactly on the page's own stated position. `target` can
            // legitimately be *before* the naive sum (the common case: the
            // final page's nominal duration overruns real content and is
            // trimmed) or after it (an underestimate); either way, `target`
            // is the number the container actually states and wins.
            let start_of_last = self.cursor.saturating_sub(last.1);
            let corrected = target.saturating_sub(start_of_last).max(0);
            last.1 = corrected;
            self.cursor = target.max(start_of_last);
        }
        out
    }
}

impl Default for GranuleTimeline {
    fn default() -> Self {
        Self::new()
    }
}

/// Best-effort per-packet duration for a completed packet, in the stream's
/// own tick unit.
///
/// Tries, in order: a codec-fixed value from `mapping` (exact for Opus is
/// deliberately *not* here — Opus has no fixed duration, every packet's TOC
/// can differ); the registered parser's
/// [`vaco_codec_core::Parser::packet_duration`], rescaled from its
/// seconds-fraction `Rational` into `time_base` ticks; `None` when neither
/// is available, which tells the caller to fall back to equal division
/// across the page.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "rounding r (a rational) to the nearest tick count needs exactly \
              one division, guarded non-zero on the line above; see \
              vaco_core::time::muldiv_rnd for the same shape"
)]
pub fn nominal_duration(
    mapping: &GranuleMapping,
    parser: Option<&dyn Parser>,
    time_base: Rational,
    payload: &[u8],
) -> Option<i64> {
    if let Some(fixed) = mapping.fixed_duration() {
        return Some(fixed);
    }
    let parser = parser?;
    let r = parser.packet_duration(payload)?;
    if !r.is_defined() || time_base.num == 0 {
        return None;
    }
    // ticks = r (seconds) / time_base (seconds per tick) = r.num * time_base.den
    //         / (r.den * time_base.num), rounded to nearest.
    let num = i128::from(r.num) * i128::from(time_base.den);
    let den = i128::from(r.den) * i128::from(time_base.num);
    if den == 0 {
        return None;
    }
    // Halving a non-negative value by a right shift is exact and is not
    // `clippy::integer_division`, which exists to flag truncation, not this.
    let half = den.abs() >> 1;
    let ticks = if (num >= 0) == (den >= 0) {
        (num.abs() + half) / den.abs()
    } else {
        -((num.abs() + half) / den.abs())
    };
    i64::try_from(ticks).ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn opus_subtracts_pre_skip_and_starts_negative() {
        let m = GranuleMapping::Opus { pre_skip: 312 };
        assert_eq!(m.timestamp(96_312), Some(96_000));
        assert_eq!(m.initial_cursor(), -312);
        assert_eq!(m.timestamp(GRANULE_UNSET), None);
    }

    #[test]
    fn vorbis_replays_the_measured_two_page_file() {
        // ffmpeg -f lavfi -i sine=r=44100 -ac 2 -c:a vorbis -q:a 4 -strict -2:
        // blocksize_1 = 2048, page granules 44032 then 88256, 44 packets/page.
        let m = GranuleMapping::Vorbis { nominal: 1024 };
        assert_eq!(m.initial_cursor(), -1024);
        let mut tl = GranuleTimeline::new();
        let page1: Vec<i64> = vec![1024; 44];
        let out1 = tl.assign(&m, 44_032, &page1);
        assert_eq!(out1.first(), Some(&(-1024, 1024)));
        assert_eq!(out1.last(), Some(&(43_008, 1024)));
        assert_eq!(tl.cursor(), 44_032);

        let page2: Vec<i64> = vec![1024; 44];
        let out2 = tl.assign(&m, 88_256, &page2);
        // First 43 packets keep the nominal 1024; the last is trimmed to 192,
        // exactly matching the measured `ffprobe -show_packets` trace.
        assert_eq!(out2[42], (44_032 + 42 * 1024, 1024));
        assert_eq!(out2[43], (44_032 + 43 * 1024, 192));
        assert_eq!(tl.cursor(), 88_256);
    }

    #[test]
    fn opus_replays_the_measured_hundred_frame_file() {
        // ffmpeg -c:a libopus over 48 kHz mono, pre_skip=312, 20 ms frames
        // (960 samples), final page granule 96312.
        let m = GranuleMapping::Opus { pre_skip: 312 };
        let mut tl = GranuleTimeline::new();
        let page: Vec<i64> = vec![960; 101];
        let out = tl.assign(&m, 96_312, &page);
        assert_eq!(out[0], (-312, 960));
        assert_eq!(out[1], (648, 960));
        assert_eq!(out[99], (94_728, 960));
        assert_eq!(out[100], (95_688, 312));
        assert_eq!(tl.cursor(), 96_000);
    }

    #[test]
    fn flac_treats_granule_as_a_plain_sample_count() {
        // ffmpeg -c:a flac, constant 4608-sample blocks, page granules
        // 46080 (10 packets) then 88200 (9 full + 1 short).
        let m = GranuleMapping::SampleCount;
        assert_eq!(m.initial_cursor(), 0);
        let mut tl = GranuleTimeline::new();
        let page1: Vec<i64> = vec![4608; 10];
        let _ = tl.assign(&m, 46_080, &page1);
        assert_eq!(tl.cursor(), 46_080);
        let page2: Vec<i64> = vec![4608; 10];
        let out2 = tl.assign(&m, 88_200, &page2);
        assert_eq!(out2[8], (46_080 + 8 * 4608, 4608));
        assert_eq!(out2[9].1, 88_200 - (46_080 + 9 * 4608));
        assert_eq!(tl.cursor(), 88_200);
    }

    #[test]
    fn theora_splits_keyframe_and_offset() {
        let m = GranuleMapping::Theora { granule_shift: 6 };
        // Theora spec §7.4.4: frame_number = keyframe_number + offset, not
        // the raw granule value — keyframe 3, offset 5 decode as frame 8.
        assert_eq!(m.timestamp((3i64 << 6) | 5), Some(8));
        assert_eq!(m.timestamp(0), Some(0));
    }

    #[test]
    fn a_page_with_no_completed_packets_leaves_the_cursor_untouched() {
        let m = GranuleMapping::SampleCount;
        let mut tl = GranuleTimeline::new();
        let out = tl.assign(&m, GRANULE_UNSET, &[]);
        assert!(out.is_empty());
        assert_eq!(tl.cursor(), 0);
    }

    #[test]
    fn a_negative_nominal_duration_cannot_walk_the_cursor_backwards() {
        let m = GranuleMapping::SampleCount;
        let mut tl = GranuleTimeline::new();
        let out = tl.assign(&m, GRANULE_UNSET, &[-100, 50]);
        assert_eq!(out[0].1, 0);
        assert_eq!(out[1].1, 50);
        assert!(tl.cursor() >= 0);
    }

    #[test]
    fn theora_granule_shift_at_the_saturating_edge_does_not_panic() {
        let m = GranuleMapping::Theora { granule_shift: 63 };
        assert!(m.timestamp(i64::MAX).is_some());
        assert!(m.timestamp(0).is_some());
    }
}
