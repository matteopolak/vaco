//! The `s337m` demuxer: SMPTE 337M-2008 non-PCM-in-PCM bursts.
//!
//! # Why this is a thin wrapper around [`SpdifDemuxer`]
//!
//! IEC 61937 (what `spdif.rs` implements) is a specific 16-bit-word profile
//! of SMPTE 337M's burst encapsulation — same sync words, same `Pc`/`Pd`
//! fields, same AC-3 framing (measured: the exact same bytes `ffmpeg -f
//! spdif` reads correctly are the bytes this crate's `SpdifDemuxer` also
//! reads correctly). Duplicating that logic under a second name would
//! violate D19 (one definition per concept) for zero behavioural gain, so
//! [`S337mDemuxer`] delegates to it entirely today.
//!
//! # What is measured, and what is an honest gap
//!
//! `ffmpeg -h demuxer=s337m` (8.1) lists no options, no extensions and no
//! MIME type — there is nothing there to measure a *behavioural* difference
//! from `spdif` against. What **is** measurable is that this reference
//! build's own `-f s337m` demuxer refuses every data type this crate could
//! generate a real sample for: AC-3 (data type 1), MPEG-1 layer 2/3 (5),
//! DTS (11) and E-AC-3 (21) all fail with `Data type 0x.. in SMPTE 337M is
//! not implemented`, even though the identical bytes open cleanly under
//! `-f spdif`. That is a decode-completeness gate in the reference itself,
//! not a framing difference this crate could reproduce differently — there
//! is no successful `-f s337m` run in this environment to compare output
//! against.
//!
//! Rather than mirror "refuses everything" (which would ship a demuxer that
//! never does anything) or invent unverified support for 20/24-bit
//! "professional" SMPTE 337M word-packing (a real feature of the standard,
//! but one no muxer in this workspace or the reference can produce a sample
//! of), this crate supports exactly the one case it can verify — 16-bit
//! AC-3 bursts, byte-identical to `spdif`'s own — and is explicit here and
//! in `docs/format/vaco-format-spdif.md` about not supporting anything
//! wider than that.
//!
//! [`SpdifDemuxer`]: crate::demux::SpdifDemuxer

use crate::demux::SpdifDemuxer;
use vaco_core::{Duration, Result};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, Stream};
use vaco_io::MediaSource;
use vaco_limits::Limits;
use vaco_packet::Packet;

pub const FLAGS: FormatFlags = crate::demux::FLAGS;

/// The `s337m` demuxer.
#[derive(Debug)]
pub struct S337mDemuxer(SpdifDemuxer);

impl S337mDemuxer {
    /// # Errors
    /// As [`SpdifDemuxer::open`] — today, the exact same AC-3-only burst
    /// reader.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        Ok(Self(SpdifDemuxer::open(src)?))
    }

    /// # Errors
    /// As [`SpdifDemuxer::open_with_limits`].
    pub fn open_with_limits(src: Box<dyn MediaSource>, limits: Limits) -> Result<Self> {
        Ok(Self(SpdifDemuxer::open_with_limits(src, limits)?))
    }
}

impl Demuxer for S337mDemuxer {
    fn streams(&self) -> &[Stream] {
        self.0.streams()
    }

    fn read_packet(&mut self) -> Result<Packet> {
        self.0.read_packet()
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        self.0.seek(target, flags)
    }

    fn duration(&self) -> Option<Duration> {
        self.0.duration()
    }
}

/// Never claims a file by content. This crate has no positive measurement
/// of what should auto-select `s337m` over `spdif` (or anything else) —
/// `ffprobe` on every sample this crate can generate picks `spdif`,
/// unprompted, at `probe_score=100` — so inventing a competing score here
/// would be a guess this crate has no evidence for. `-f s337m` still opens
/// it by explicit name.
#[must_use]
pub fn probe(_data: &vaco_format_core::ProbeData<'_>) -> vaco_format_core::ProbeScore {
    vaco_format_core::ProbeScore::NONE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_probe_never_claims_content() {
        let data = vaco_format_core::ProbeData::new(&[0x72, 0xF8, 0x1F, 0x4E, 0x01, 0x00]);
        assert_eq!(probe(&data), vaco_format_core::ProbeScore::NONE);
    }
}
