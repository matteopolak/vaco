//! `framecrc`, `framemd5` and `framehash`: one header block, then one line per
//! packet.
//!
//! # Header
//!
//! `#tb` uses [`Muxer::add_stream_with`]'s supplied stream time base and falls
//! back to [`crate::header::resolve_time_base`] only when none is supplied.
//! `#software` carries a version and is suppressed under `-bitexact`.
//! `#extradata` appears once per stream that has it, hashed with the active
//! frame checksum scheme; [`FrameHashMuxer::extradata_lines`] records the two
//! measured layouts.
//!
//! Measured on ffmpeg 8.1, `framecrc` prints the common header only.
//! `framemd5` and `framehash` prepend `#format`, `#version`, and `#hash`, then
//! append this exact column header:
//!
//! ```text
//! #stream#, dts,        pts, duration,     size, hash
//! ```
//!
//! `framecrc`: `<stream>,<dts>,<pts>,<duration>,<size>, 0x<crc>[, F=0x<flags>]`
//! right-justifies its numeric fields to widths 11, 11, 9, and 9.
//! `framemd5`/`framehash` use plain hex and never print `F=`. Missing PTS is
//! the decimal `i64::MIN`, not `N/A`. For `framecrc`, `F=` is omitted exactly
//! when `flags == KEY`; otherwise it appears even when the bits are zero.

use core::fmt::Write as _;

use vaco_codec_core::CodecParameters;
use vaco_core::{Error, Result};
use vaco_format_core::{FormatFlags, Muxer, StreamSpec};
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_packet::{Packet, PacketFlags};

use crate::algo::{ADLER32_FRAME_SEED, HashAlgo, adler32_seeded};
use crate::header::{StreamHeader, write_common_header};

/// The one column-header line `framemd5`/`framehash` print and `framecrc`
/// does not. Verbatim, `od -c`-checked.
const COLUMN_HEADER: &str = "#stream#, dts,        pts, duration,     size, hash";

/// Which of the two checksum schemes a [`FrameHashMuxer`] uses.
#[derive(Debug, Clone, Copy)]
pub enum FrameMode {
    /// `framecrc`: bespoke per-packet Adler-32, seed [`ADLER32_FRAME_SEED`]
    /// (measured **not** to be the standard RFC 1950 seed — see
    /// `crate::algo`), printed `0x%08x`, with the conditional `F=` field.
    Crc,
    /// `framemd5` / `framehash`: the ordinary [`HashAlgo`] digest, printed as
    /// plain hex, no `F=` field ever.
    Hash(HashAlgo),
}

/// `framecrc`, `framemd5` and `framehash`.
#[derive(Debug)]
pub struct FrameHashMuxer {
    out: IoWriter,
    mode: FrameMode,
    streams: Vec<StreamHeader>,
    /// [`Muxer::set_bitexact`] gates the version-bearing `#software` line.
    bitexact: bool,
}

impl FrameHashMuxer {
    /// # Errors
    /// Propagates buffer allocation failure from [`IoWriter`].
    pub fn new(sink: Box<dyn MediaSink>, mode: FrameMode) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            mode,
            streams: Vec::new(),
            bitexact: false,
        })
    }

    /// `framecrc`.
    ///
    /// # Errors
    /// As [`FrameHashMuxer::new`].
    pub fn framecrc(sink: Box<dyn MediaSink>) -> Result<Self> {
        Self::new(sink, FrameMode::Crc)
    }

    /// `framemd5`.
    ///
    /// # Errors
    /// As [`FrameHashMuxer::new`].
    pub fn framemd5(sink: Box<dyn MediaSink>) -> Result<Self> {
        Self::new(sink, FrameMode::Hash(HashAlgo::Md5))
    }

    /// `framehash`, algorithm chosen by the caller (the reference's own
    /// default, absent a `-hash` option, is SHA-256).
    ///
    /// # Errors
    /// As [`FrameHashMuxer::new`].
    pub fn framehash(sink: Box<dyn MediaSink>, algo: HashAlgo) -> Result<Self> {
        Self::new(sink, FrameMode::Hash(algo))
    }

    /// Shared body of [`Muxer::add_stream`]/[`Muxer::add_stream_with`]:
    /// declare a stream, with a supplied usable `time_base` overriding
    /// [`StreamHeader`]'s `CodecParameters`-only fallback.
    fn push_stream(
        &mut self,
        params: &CodecParameters,
        time_base: Option<vaco_core::Rational>,
    ) -> Result<u32> {
        let idx = u32::try_from(self.streams.len()).map_err(|_| Error::LimitExceeded {
            limit: "stream count",
            requested: u64::try_from(self.streams.len().saturating_add(1)).unwrap_or(u64::MAX),
            cap: u64::from(u32::MAX),
        })?;
        self.streams.push(StreamHeader::new(params, time_base));
        Ok(idx)
    }

    /// `#extradata <n>` lines, one per stream that has any, hashed with
    /// *this muxer's own* checksum scheme rather than a fixed algorithm —
    /// measured (`ffmpeg 8.1`) against the same input's avcC: `framecrc`
    /// hashes it with the special seeded Adler-32 its packet lines use
    /// (`0x27ba0f4a`, not `zlib.crc32`'s `0x6b488af1`), and `framemd5`/
    /// `framehash -hash <algo>` hash it with that run's own `<algo>`
    /// (verified for MD5, SHA-256, CRC-32 and adler32) — so this crate
    /// deliberately does not reach for [`vaco_hash::crc32`] here despite the
    /// `0x`-prefixed hex looking exactly like one.
    ///
    /// Column widths and separators are two different, both measured,
    /// shapes: `framecrc` is `#extradata {n}: {len:>9}, 0x{hash:08x}`;
    /// `framemd5`/`framehash` is `#extradata {n}, {len:>32}, {hash_hex}`.
    ///
    /// # Errors
    /// [`Error::InvalidData`] if this build cannot compute the selected
    /// [`FrameMode::Hash`] algorithm — the same failure [`Muxer::write_packet`]
    /// reports for the same reason, just found one write earlier.
    fn extradata_lines(&self) -> Result<Vec<String>> {
        let mut lines = Vec::new();
        for (i, st) in self.streams.iter().enumerate() {
            let Some(data) = st.params.extradata.as_ref().filter(|d| !d.is_empty()) else {
                continue;
            };
            let line = match self.mode {
                FrameMode::Crc => {
                    let (a0, b0) = ADLER32_FRAME_SEED;
                    let crc = adler32_seeded(data, a0, b0);
                    format!("#extradata {i}:{:>9}, 0x{crc:08x}", data.len())
                }
                FrameMode::Hash(algo) => {
                    let hex = algo.digest_hex(data).ok_or(Error::InvalidData(
                        "framehash: this build cannot compute the requested hash",
                    ))?;
                    format!("#extradata {i},{:>32}, {hex}", data.len())
                }
            };
            lines.push(line);
        }
        Ok(lines)
    }
}

impl Muxer for FrameHashMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        self.push_stream(params, None)
    }

    fn add_stream_with(&mut self, params: &CodecParameters, spec: &StreamSpec) -> Result<u32> {
        self.push_stream(params, spec.time_base)
    }

    fn set_bitexact(&mut self, bitexact: bool) {
        self.bitexact = bitexact;
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<vaco_core::Rational> {
        usize::try_from(stream_index)
            .ok()
            .and_then(|i| self.streams.get(i))
            .map(|s| s.time_base)
    }

    fn write_header(&mut self) -> Result<()> {
        let extra: Vec<String> = match self.mode {
            FrameMode::Crc => Vec::new(),
            FrameMode::Hash(algo) => vec![
                "#format: frame checksums".to_owned(),
                "#version: 2".to_owned(),
                format!("#hash: {}", algo.label()),
            ],
        };
        let extradata = self.extradata_lines()?;
        write_common_header(
            &mut self.out,
            &self.streams,
            &extra,
            &extradata,
            self.bitexact,
        )?;
        if matches!(self.mode, FrameMode::Hash(_)) {
            let mut line = COLUMN_HEADER.to_owned();
            line.push('\n');
            self.out.write(line.as_bytes())?;
        }
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        let idx = usize::try_from(packet.stream_index).unwrap_or(usize::MAX);
        let time_base = self
            .streams
            .get(idx)
            .map_or(vaco_core::Rational::MICROSECONDS, |s| s.time_base);

        // Measured: a missing pts prints as the raw `AV_NOPTS_VALUE` integer,
        // not `N/A` — see the module docs. `Timestamp`'s own `Display` prints
        // `N/A`, which is deliberately not used here.
        let dts = packet.dts.ticks().unwrap_or(i64::MIN);
        let pts = packet.pts.ticks().unwrap_or(i64::MIN);
        let duration = packet
            .duration_ts()
            .or_else(|| packet.duration.to_ticks(time_base))
            .unwrap_or(0);
        let size = u64::try_from(packet.len).unwrap_or(u64::MAX);

        let mut line = String::new();
        let _ = write!(
            line,
            "{},{dts:>11},{pts:>11},{duration:>9},{size:>9}, ",
            packet.stream_index
        );
        match self.mode {
            FrameMode::Crc => {
                let (a0, b0) = ADLER32_FRAME_SEED;
                let crc = adler32_seeded(packet.payload(), a0, b0);
                let _ = write!(line, "0x{crc:08x}");
                // The conditional `F=` field — `framecrc` only, see the
                // module docs. `PacketFlags::KEY` alone is the "ordinary
                // keyframe" case the reference suppresses.
                if packet.flags != PacketFlags::KEY {
                    let _ = write!(line, ", F=0x{:x}", packet.flags.bits());
                }
            }
            FrameMode::Hash(algo) => {
                line.push_str(&algo.digest_hex(packet.payload()).ok_or(
                    vaco_core::Error::InvalidData(
                        "framehash: this build cannot compute the requested hash",
                    ),
                )?);
            }
        }
        line.push('\n');
        self.out.write(line.as_bytes())
    }

    fn write_trailer(&mut self) -> Result<()> {
        self.out.flush()
    }

    /// This crate never rewrites a stream's own timescale (M12 already
    /// declares [`Muxer::stream_time_base`] above), and never needs a
    /// container-specific interleave policy or bitstream conversion, so every
    /// other default is left as the trait's own (per-DTS interleave, `Keep`
    /// bitstream action, [`vaco_format_core::mux::CodecSupport::Supported`]
    /// for any codec).
    fn flags(&self) -> FormatFlags {
        FormatFlags::empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_core::{Rational, Timestamp};
    use vaco_format_core::vacoraw::MemorySink;
    use vaco_limits::{Budget, Limits};

    /// Two consecutive frames of `ffmpeg -f lavfi -i
    /// testsrc=size=64x64:rate=5:duration=1 -pix_fmt yuv420p -c:v rawvideo
    /// -frames:v N -f rawvideo -`, captured as raw bytes (ffmpeg 8.1,
    /// `LC_ALL=C`; MD5-verified byte-for-byte against `-f md5` on the same
    /// command, so these are exactly what the reference's own muxers hashed).
    /// See `docs/format/vaco-mux-hash.md` for how to regenerate them.
    const FRAME0: &[u8] = include_bytes!("../tests/fixtures/testsrc_64x64_frame0.yuv");
    const FRAME1: &[u8] = include_bytes!("../tests/fixtures/testsrc_64x64_frame1.yuv");

    fn video_params() -> CodecParameters {
        let mut p = CodecParameters::video();
        p.video.as_mut().unwrap().frame_rate = Rational::new(5, 1);
        p.video.as_mut().unwrap().width = 64;
        p.video.as_mut().unwrap().height = 64;
        p
    }

    fn pkt(payload: &[u8], dts: i64, pts: Option<i64>) -> Packet {
        let mut budget = Budget::new(Limits::strict());
        let mut p = Packet::from_slice(&mut budget, payload).unwrap();
        p.dts = Timestamp::new(dts);
        p.pts = pts.map_or(Timestamp::NONE, Timestamp::new);
        p.flags = PacketFlags::KEY;
        p
    }

    #[test]
    fn framecrc_matches_the_two_lines_measured_from_the_reference() {
        // `ffmpeg -f lavfi -i testsrc=size=64x64:rate=5:duration=1 -pix_fmt
        // yuv420p -c:v rawvideo -frames:v 2 -f framecrc -`:
        //   0,          0,          0,        1,     6144, 0xb907b704
        //   0,          1,          1,        1,     6144, 0x3e18b700
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = FrameHashMuxer::framecrc(Box::new(sink)).unwrap();
        m.add_stream(&video_params()).unwrap();
        m.write_header().unwrap();
        for (i, frame) in [FRAME0, FRAME1].into_iter().enumerate() {
            let dts = i64::try_from(i).unwrap();
            let mut p = pkt(frame, dts, Some(dts));
            p.stream_index = 0;
            p.duration = Timestamp::new(1).to_duration(Rational::new(1, 5)).unwrap();
            m.write_packet(&p).unwrap();
        }
        m.write_trailer().unwrap();

        let dumped = String::from_utf8(shared.snapshot()).unwrap();
        assert!(dumped.contains("#tb 0: 1/5\n"));
        assert!(dumped.contains("#media_type 0: video\n"));
        assert!(dumped.contains("#dimensions 0: 64x64\n"));
        assert!(dumped.contains("0,          0,          0,        1,     6144, 0xb907b704\n"));
        assert!(dumped.ends_with("0,          1,          1,        1,     6144, 0x3e18b700\n"));
    }

    #[test]
    fn a_non_key_packet_gets_the_f_field_and_a_keyframe_does_not() {
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = FrameHashMuxer::framecrc(Box::new(sink)).unwrap();
        m.add_stream(&video_params()).unwrap();
        m.write_header().unwrap();

        let mut key = pkt(b"abc", 0, Some(0));
        key.stream_index = 0;
        m.write_packet(&key).unwrap();

        let mut nonkey = pkt(b"def", 1, Some(1));
        nonkey.stream_index = 0;
        nonkey.flags = PacketFlags::empty();
        m.write_packet(&nonkey).unwrap();
        m.write_trailer().unwrap();

        let dumped = String::from_utf8(shared.snapshot()).unwrap();
        let lines: Vec<&str> = dumped.lines().filter(|l| !l.starts_with('#')).collect();
        assert!(!lines[0].contains("F="), "keyframe line: {}", lines[0]);
        assert!(lines[1].contains(", F=0x0"), "non-key line: {}", lines[1]);
    }

    #[test]
    fn framemd5_matches_the_measured_reference_lines_and_has_no_f_field() {
        // `ffmpeg ... -frames:v 2 -f framemd5 -`:
        //   0,          0,          0,        1,     6144, a111606e32508d2d9bb294bed727979e
        //   0,          1,          1,        1,     6144, 8f0af0ff395c5eefbe531b808a579b8f
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = FrameHashMuxer::framemd5(Box::new(sink)).unwrap();
        m.add_stream(&video_params()).unwrap();
        m.write_header().unwrap();
        let tb = Rational::new(1, 5);
        let mut p0 = pkt(FRAME0, 0, Some(0));
        p0.flags = PacketFlags::empty();
        p0.stream_index = 0;
        p0.duration = Timestamp::new(1).to_duration(tb).unwrap();
        m.write_packet(&p0).unwrap();
        let mut p1 = pkt(FRAME1, 1, Some(1));
        p1.stream_index = 0;
        p1.duration = Timestamp::new(1).to_duration(tb).unwrap();
        m.write_packet(&p1).unwrap();
        m.write_trailer().unwrap();

        let dumped = String::from_utf8(shared.snapshot()).unwrap();
        assert!(dumped.starts_with("#format: frame checksums\n"));
        assert!(dumped.contains("#version: 2\n"));
        assert!(dumped.contains("#hash: MD5\n"));
        assert!(dumped.contains(COLUMN_HEADER));
        assert!(!dumped.contains("F="));
        assert!(dumped.contains(
            "0,          0,          0,        1,     6144, a111606e32508d2d9bb294bed727979e\n"
        ));
        assert!(dumped.ends_with(
            "0,          1,          1,        1,     6144, 8f0af0ff395c5eefbe531b808a579b8f\n"
        ));
    }

    #[test]
    fn a_missing_pts_prints_as_the_raw_i64_min_not_n_a() {
        // Measured on a raw H.264 elementary stream (see the module docs):
        // the reference prints the literal `AV_NOPTS_VALUE` integer, not
        // `N/A`. This crate's `Timestamp::Display` prints `N/A`; the muxer
        // deliberately does not use it.
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = FrameHashMuxer::framecrc(Box::new(sink)).unwrap();
        m.add_stream(&video_params()).unwrap();
        m.write_header().unwrap();
        let mut p = pkt(b"x", 0, None);
        p.stream_index = 0;
        m.write_packet(&p).unwrap();
        m.write_trailer().unwrap();
        let dumped = String::from_utf8(shared.snapshot()).unwrap();
        assert!(dumped.contains(&i64::MIN.to_string()));
        assert!(!dumped.contains("N/A"));
    }

    #[test]
    fn framehash_matches_the_measured_sha256_line() {
        // `ffmpeg ... -frames:v 1 -f framehash -hash sha256 -`:
        //   0,          0,          0,        1,     6144, c7eb1a16dc0cf68770cc974c5ce1ca0c384560d7f17e517b00c1d3d0c86fb923
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = FrameHashMuxer::framehash(Box::new(sink), HashAlgo::Sha256).unwrap();
        m.add_stream(&video_params()).unwrap();
        m.write_header().unwrap();
        let mut p = pkt(FRAME0, 0, Some(0));
        p.stream_index = 0;
        p.duration = Timestamp::new(1).to_duration(Rational::new(1, 5)).unwrap();
        m.write_packet(&p).unwrap();
        m.write_trailer().unwrap();
        let dumped = String::from_utf8(shared.snapshot()).unwrap();
        assert!(dumped.contains("#hash: SHA256\n"));
        assert!(dumped.ends_with(
            "0,          0,          0,        1,     6144, \
             c7eb1a16dc0cf68770cc974c5ce1ca0c384560d7f17e517b00c1d3d0c86fb923\n"
        ));
    }

    /// A real H.264 `avcC` payload (45 bytes, extracted from an
    /// `ffmpeg`-muxed MP4's `stsd`), used to cross-check `#extradata`'s hash
    /// against values measured directly from the reference rather than only
    /// against this crate's own algorithms.
    fn avcc_extradata() -> Vec<u8> {
        const HEX: &str = "0164000affe100186764000aacd94426c044000003000400000300c83c\
                            48965801000668ebe3cb22c0fdf8f800";
        (0..HEX.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&HEX[i..i + 2], 16).unwrap())
            .collect()
    }

    /// [`Muxer::add_stream_with`] supplies the real stream time base; the
    /// `CodecParameters`-only `1/frame_rate` guess from
    /// [`crate::header::resolve_time_base`] is only the `add_stream` fallback.
    #[test]
    fn add_stream_with_overrides_the_frame_rate_guess() {
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = FrameHashMuxer::framecrc(Box::new(sink)).unwrap();
        // `video_params()` carries `frame_rate = 5/1`, which would resolve to
        // `1/5`; the measured reference keeps the input's `1/12800` base.
        let spec = StreamSpec {
            time_base: Some(Rational::new(1, 12_800)),
            ..StreamSpec::default()
        };
        let idx = m.add_stream_with(&video_params(), &spec).unwrap();
        assert_eq!(m.stream_time_base(idx), Some(Rational::new(1, 12_800)));
        m.write_header().unwrap();
        m.write_trailer().unwrap();
        let dumped = String::from_utf8(shared.snapshot()).unwrap();
        assert!(dumped.contains("#tb 0: 1/12800\n"), "{dumped}");
        assert!(!dumped.contains("#tb 0: 1/5\n"), "{dumped}");
    }

    /// [`Muxer::add_stream_with`] with an undefined/zero base falls back to
    /// the same `CodecParameters` guess `add_stream` alone would give — the
    /// case a freshly encoded raw/PCM stream hits (no better base to hand
    /// down), which is what the fallback exists for.
    #[test]
    fn add_stream_with_falls_back_when_the_spec_has_no_usable_base() {
        let sink = MemorySink::new();
        let mut m = FrameHashMuxer::framecrc(Box::new(sink)).unwrap();
        let idx = m
            .add_stream_with(&video_params(), &StreamSpec::default())
            .unwrap();
        assert_eq!(m.stream_time_base(idx), Some(Rational::new(1, 5)));
    }

    /// `#extradata <n>: <len>, 0x<hash>` — `framecrc` hashes it with the same
    /// seeded Adler-32 as its packet lines, *not* `zlib`/real CRC-32 (measured
    /// against `ffmpeg 8.1`: `0x27ba0f4a`, where real CRC-32 of the same
    /// bytes is `0x6b488af1` — see [`FrameHashMuxer::extradata_lines`]).
    #[test]
    fn framecrc_prints_extradata_with_its_own_seeded_adler32() {
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = FrameHashMuxer::framecrc(Box::new(sink)).unwrap();
        let mut params = video_params();
        params.extradata = Some(avcc_extradata());
        m.add_stream(&params).unwrap();
        m.write_header().unwrap();
        m.write_trailer().unwrap();
        let dumped = String::from_utf8(shared.snapshot()).unwrap();
        assert!(
            dumped.contains("#extradata 0:       45, 0x27ba0f4a\n"),
            "{dumped}"
        );
    }

    /// `#extradata <n>, <len>, <hash>` — `framemd5` hashes it with that run's
    /// own algorithm (MD5 here), plain hex, no `0x` — measured against
    /// `ffmpeg 8.1`: `8a107aac933ae9470ec1efe74fc780fe`.
    #[test]
    fn framemd5_prints_extradata_with_the_selected_algorithm() {
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = FrameHashMuxer::framemd5(Box::new(sink)).unwrap();
        let mut params = video_params();
        params.extradata = Some(avcc_extradata());
        m.add_stream(&params).unwrap();
        m.write_header().unwrap();
        m.write_trailer().unwrap();
        let dumped = String::from_utf8(shared.snapshot()).unwrap();
        assert!(
            dumped.contains(
                "#extradata 0,                              45, \
                 8a107aac933ae9470ec1efe74fc780fe\n"
            ),
            "{dumped}"
        );
    }

    /// No extradata, no line at all — measured (`ffmpeg 8.1`): the reference
    /// omits `#extradata` entirely rather than printing a zero length.
    #[test]
    fn no_extradata_means_no_extradata_line() {
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = FrameHashMuxer::framecrc(Box::new(sink)).unwrap();
        m.add_stream(&video_params()).unwrap();
        m.write_header().unwrap();
        m.write_trailer().unwrap();
        let dumped = String::from_utf8(shared.snapshot()).unwrap();
        assert!(!dumped.contains("#extradata"), "{dumped}");
    }

    /// `#software` is suppressed under `-bitexact` and otherwise prints a
    /// version-bearing name rather than the bare `vaco` label.
    #[test]
    fn software_line_is_suppressed_only_under_bitexact() {
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = FrameHashMuxer::framecrc(Box::new(sink)).unwrap();
        m.add_stream(&video_params()).unwrap();
        m.write_header().unwrap();
        m.write_trailer().unwrap();
        let dumped = String::from_utf8(shared.snapshot()).unwrap();
        assert!(dumped.contains("#software: vaco"), "{dumped}");
        assert!(!dumped.contains("#software: vaco\n"), "{dumped}");

        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = FrameHashMuxer::framecrc(Box::new(sink)).unwrap();
        m.set_bitexact(true);
        m.add_stream(&video_params()).unwrap();
        m.write_header().unwrap();
        m.write_trailer().unwrap();
        let dumped = String::from_utf8(shared.snapshot()).unwrap();
        assert!(!dumped.contains("#software"), "{dumped}");
    }
}
