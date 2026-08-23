//! `crc`, `md5` and `hash`: one line for the whole file, no header at all.
//!
//! Measured (ffmpeg 8.1, `LC_ALL=C`): none of the three prints a `#`-comment
//! block — `ffmpeg -f crc`/`-f md5`/`-f hash` on a `testsrc` produce exactly
//! one line each (`CRC=0x88956e14`, `MD5=0c006add1a6bfa412f0f804469a09083`,
//! `SHA256=...` — the reference's own default absent `-hash`), nothing else,
//! regardless of stream count. The checksum runs across every packet from
//! every stream, in the order [`vaco_format_core::mux::MuxWriter`] hands them
//! to [`Muxer::write_packet`] — i.e. interleaved order, not stream order.
//!
//! # `crc` is `-hash adler32` under a different label, not a different
//! algorithm
//!
//! `ffmpeg -f crc` and `ffmpeg -f hash -hash adler32` on the same two-frame
//! input both print `88956e14` (`CRC=0x88956e14` / `adler32=88956e14`) — the
//! *whole-file* Adler-32 uses the ordinary RFC 1950 seed. Only `framecrc`'s
//! **per-packet** Adler-32 uses the nonstandard zero seed; see `crate::algo`
//! and `crate::frame`.
//!
//! `md5` is the same generic machinery with a fixed algorithm rather than a
//! second bespoke one: measured, `ffmpeg -h muxer=md5` lists its own `-hash`
//! option (default `"md5"`), so `md5` is `hash` with a different default, not
//! a distinct code path the way `crc`/`framecrc` are. This crate models that
//! as one type, [`WholeHashMuxer`], built through three constructors.

use core::fmt::Write as _;

use vaco_codec_core::CodecParameters;
use vaco_core::Result;
use vaco_format_core::Muxer;
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_packet::Packet;

use crate::algo::{ADLER32_STANDARD_SEED, HashAlgo, RunningHash};

/// `crc`, `md5` and `hash`.
#[derive(Debug)]
pub struct WholeHashMuxer {
    out: IoWriter,
    /// `None` once [`Muxer::write_trailer`] has consumed it — see the doc on
    /// [`RunningHash::finish_hex`], which takes `self` by value.
    hasher: Option<RunningHash>,
    label: &'static str,
    /// Only the dedicated `crc` muxer spells its line `CRC=0x%08x`; every
    /// other registration in this module is `<ALGO>=<hex>` with no `0x`.
    hex_prefix: bool,
    stream_count: usize,
}

impl WholeHashMuxer {
    fn build(
        sink: Box<dyn MediaSink>,
        hasher: RunningHash,
        label: &'static str,
        hex_prefix: bool,
    ) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            hasher: Some(hasher),
            label,
            hex_prefix,
            stream_count: 0,
        })
    }

    /// `crc`: whole-file Adler-32, standard seed, `CRC=0x%08x`.
    ///
    /// # Errors
    /// As [`IoWriter::new`].
    pub fn crc(sink: Box<dyn MediaSink>) -> Result<Self> {
        let (a0, b0) = ADLER32_STANDARD_SEED;
        Self::build(sink, RunningHash::adler32_seeded(a0, b0), "CRC", true)
    }

    /// `md5`: whole-file MD5, `MD5=<hex>`. The reference's own default for
    /// this muxer name, not a bespoke algorithm — see the module docs.
    ///
    /// # Errors
    /// As [`IoWriter::new`].
    pub fn md5(sink: Box<dyn MediaSink>) -> Result<Self> {
        Self::build(
            sink,
            HashAlgo::Md5.running().ok_or(unavailable())?,
            "MD5",
            false,
        )
    }

    /// `hash`: whole-file digest, any algorithm, `<ALGO>=<hex>`. The
    /// reference's own default absent `-hash` is SHA-256.
    ///
    /// # Errors
    /// As [`IoWriter::new`].
    pub fn hash(sink: Box<dyn MediaSink>, algo: HashAlgo) -> Result<Self> {
        Self::build(
            sink,
            algo.running().ok_or(unavailable())?,
            algo.label(),
            false,
        )
    }
}

impl Muxer for WholeHashMuxer {
    fn add_stream(&mut self, _params: &CodecParameters) -> Result<u32> {
        let idx = u32::try_from(self.stream_count).unwrap_or(u32::MAX);
        self.stream_count = self.stream_count.saturating_add(1);
        Ok(idx)
    }

    fn write_header(&mut self) -> Result<()> {
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if let Some(h) = self.hasher.as_mut() {
            h.update(packet.payload());
        }
        Ok(())
    }

    fn write_trailer(&mut self) -> Result<()> {
        // `take()`, not a borrow: `finish_hex`/`finish_u32` consume the
        // hasher. `write_trailer` runs at most once per
        // `vaco_format_core::mux::MuxWriter` (M11), so `None` here would mean
        // a caller drove this `Muxer` directly and called it twice — silently
        // writing nothing a second time is the right answer for that, not a
        // panic.
        let Some(hasher) = self.hasher.take() else {
            return Ok(());
        };
        let mut line = String::new();
        if self.hex_prefix {
            let value = hasher.finish_u32()?;
            let _ = writeln!(line, "{}=0x{value:08x}", self.label);
        } else {
            let _ = writeln!(line, "{}={}", self.label, hasher.finish_hex());
        }
        self.out.write(line.as_bytes())?;
        self.out.flush()
    }
}

/// The error for an algorithm this build names but cannot compute.
///
/// Refusing beats omitting: a muxer that quietly wrote no digest would look
/// exactly like success to a differential harness.
fn unavailable() -> vaco_core::Error {
    vaco_core::Error::InvalidData("this build cannot compute the requested hash algorithm")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_format_core::vacoraw::MemorySink;
    use vaco_limits::{Budget, Limits};

    const FRAME0: &[u8] = include_bytes!("../tests/fixtures/testsrc_64x64_frame0.yuv");
    const FRAME1: &[u8] = include_bytes!("../tests/fixtures/testsrc_64x64_frame1.yuv");

    fn write_both_frames(m: &mut WholeHashMuxer) {
        m.add_stream(&CodecParameters::video()).unwrap();
        m.write_header().unwrap();
        let mut budget = Budget::new(Limits::strict());
        for frame in [FRAME0, FRAME1] {
            let p = Packet::from_slice(&mut budget, frame).unwrap();
            m.write_packet(&p).unwrap();
        }
    }

    #[test]
    fn crc_matches_the_measured_reference_line() {
        // `ffmpeg -f lavfi -i testsrc=size=64x64:rate=5:duration=1 -pix_fmt
        // yuv420p -c:v rawvideo -frames:v 2 -f crc -` → `CRC=0x88956e14`.
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = WholeHashMuxer::crc(Box::new(sink)).unwrap();
        write_both_frames(&mut m);
        m.write_trailer().unwrap();
        assert_eq!(shared.snapshot(), b"CRC=0x88956e14\n");
    }

    #[test]
    fn md5_matches_the_measured_reference_line() {
        // Same two frames, `-f md5 -` → `MD5=0c006add1a6bfa412f0f804469a09083`.
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = WholeHashMuxer::md5(Box::new(sink)).unwrap();
        write_both_frames(&mut m);
        m.write_trailer().unwrap();
        assert_eq!(shared.snapshot(), b"MD5=0c006add1a6bfa412f0f804469a09083\n");
    }

    #[test]
    fn crc_equals_hash_adler32_but_not_the_frame_seed() {
        // The finding worth pinning down with a test, not just a comment:
        // whole-file `crc` is standard-seeded Adler-32.
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = WholeHashMuxer::hash(Box::new(sink), HashAlgo::Adler32).unwrap();
        write_both_frames(&mut m);
        m.write_trailer().unwrap();
        assert_eq!(shared.snapshot(), b"adler32=88956e14\n");
    }

    #[test]
    fn hash_defaults_are_wired_correctly() {
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = WholeHashMuxer::hash(Box::new(sink), HashAlgo::Sha256).unwrap();
        write_both_frames(&mut m);
        m.write_trailer().unwrap();
        let out = shared.snapshot();
        assert!(out.starts_with(b"SHA256="));
        assert_eq!(out.len(), "SHA256=".len() + 64 + 1);
    }

    #[test]
    fn a_second_trailer_writes_nothing_rather_than_panicking() {
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = WholeHashMuxer::crc(Box::new(sink)).unwrap();
        write_both_frames(&mut m);
        m.write_trailer().unwrap();
        let first = shared.snapshot();
        m.write_trailer().unwrap();
        assert_eq!(shared.snapshot(), first);
    }
}
