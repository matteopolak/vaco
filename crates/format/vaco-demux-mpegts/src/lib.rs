//! The MPEG-TS demuxer: ISO/IEC 13818-1 transport streams.
//!
//! # What makes this container different
//!
//! MP4 ships an index and Matroska ships cues. MPEG-TS ships **nothing**. It
//! was designed to be broadcast, so a decoder joining halfway through must
//! bootstrap itself from the stream, and every consequence of that follows:
//!
//! * **There is no duration field**, so the length of a file is *estimated* by
//!   reading its tail — see [`demux::DURATION_READ_BACK`], whose value is
//!   measured against the reference rather than guessed.
//! * **There is no table of contents**, so seeking bisects byte positions and
//!   reads the timestamp of whatever PES packet it lands near.
//! * **There is no stream list**, only a Program Association Table that repeats
//!   and a Program Map Table per program that repeats, either of which can
//!   change mid-file. Discovery is progressive by construction.
//! * **Timestamps are 33 bits at 90 kHz**, so a recording longer than about
//!   26.5 hours wraps, and the adaptation field can declare a jump that is
//!   *legitimate* rather than corrupt.
//!
//! # Layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`probe`] | content detection, with its scores measured against ffprobe 8.1 |
//! | [`pes`] | PES packet headers and the 33-bit timestamp field |
//! | [`demux`] | framing, PES assembly, the clock, duration, seeking |
//!
//! The PSI/SI layer — sections, CRC-32, PAT/PMT/CAT/SDT, descriptors,
//! `stream_type` — lives in `vaco-format-mpegts-tables` and has no I/O at all.
//!
//! ```no_run
//! use vaco_demux_mpegts::MpegTsDemuxer;
//! use vaco_format_core::discovery::NoParsers;
//! use vaco_format_core::{Demuxer, FormatOptions};
//! use vaco_io::MemorySource;
//!
//! # fn main() -> vaco_core::Result<()> {
//! let bytes: Vec<u8> = std::fs::read("recording.ts").unwrap_or_default();
//! let src = Box::new(MemorySource::new(bytes));
//! let mut demux = MpegTsDemuxer::open(src, &NoParsers, &FormatOptions::default())?;
//! for s in demux.streams() {
//!     println!("pid {:?} {:?}", s.id, s.params.codec_id);
//! }
//! let pkt = demux.read_packet()?;
//! println!("{:?} {} bytes", pkt.pts, pkt.len);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod demux;
pub mod pes;
pub mod probe;
pub mod raw;

use vaco_core::Result;
use vaco_format_core::{Demuxer, DemuxerDesc, FormatOptions, ParserProvider};
use vaco_io::MediaSource;

pub use demux::{DemuxStats, FLAGS, MpegTsDemuxer};
pub use probe::{TS_SCORE_STRONG, TS_SCORE_WEAK, probe};
pub use raw::{MpegTsRawDemuxer, RAW_DEMUXER};

/// The registry descriptor for reassembled-PES `mpegts`.
///
/// `mpegtsraw`, the PID-level view that skips PES reassembly, is
/// [`RAW_DEMUXER`] — a second, real registration confirmed present in
/// `ffmpeg -demuxers` (`mpegts` and `mpegtsraw` both list, distinctly from
/// each other); see `raw` for what was measured about it. `m2ts`, by
/// contrast, is **not** a third demuxer: `ffmpeg -demuxers` lists no such
/// entry, and Blu-ray's four-byte-timestamp-prefixed stride is already one of
/// [`vaco_format_mpegts_tables::packet::PacketStride`]'s three variants,
/// autodetected the same way 188 and 204 are.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "mpegts",
    long_name: "MPEG-TS (MPEG-2 Transport Stream)",
    extensions: &["ts", "m2t", "m2ts", "mts", "mpegts"],
    mime_types: &["video/mp2t"],
    flags: crate::demux::FLAGS,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(MpegTsDemuxer::open(
        src,
        parsers,
        &FormatOptions::default(),
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptor_answers_to_the_names_the_cli_uses() {
        assert!(DEMUXER.matches_name("mpegts"));
        assert!(DEMUXER.matches_extension("/tmp/x.ts"));
        assert!(DEMUXER.matches_extension("/tmp/x.M2TS"));
        assert!(!DEMUXER.matches_extension("/tmp/x.mp4"));
    }

    #[test]
    fn the_declared_flags_are_the_ones_the_models_depend_on() {
        use vaco_format_core::FormatFlags;
        // TS_DISCONT suppresses the monotonic-DTS repair, which is what stops
        // a spliced recording being silently smoothed over.
        assert!(FLAGS.contains(FormatFlags::TS_DISCONT));
        assert!(FLAGS.contains(FormatFlags::GENERIC_INDEX));
        assert!(FLAGS.contains(FormatFlags::SHOW_IDS));
        // And the flag that is *not* set: MPEG-TS is byte-seekable.
        assert!(FLAGS.allows_byte_seek());
    }
}
