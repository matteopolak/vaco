//! `streamhash`: one line per stream, no header at all.
//!
//! Measured (ffmpeg 8.1, `LC_ALL=C`), `-f streamhash -hash crc32` on a
//! `testsrc` plus a `sine` audio input:
//!
//! ```text
//! 0,v,CRC32=e03bd439
//! 1,a,CRC32=f2a6c4ff
//! ```
//!
//! No `#`-comment lines of any kind — unlike every other muxer in this crate,
//! `streamhash` prints nothing before or between these lines. Each line is
//! `<stream index>,<media type letter>,<ALGO>=<hex>`, in stream order (not the
//! order the streams' packets happened to interleave in), where the digest is
//! a running hash of *that stream's own* packet payloads only, standard-seeded
//! — the same "ordinary algorithm, no per-packet reset" behaviour
//! [`crate::whole`] documents for the whole-file muxers, just scoped to one
//! stream instead of the whole file. The media-type letter is
//! [`vaco_core::MediaType::specifier_char`], the same letter the reference
//! uses in `-map`/`-c:v` stream specifiers.

use core::fmt::Write as _;

use vaco_codec_core::CodecParameters;
use vaco_core::{MediaType, Result};
use vaco_format_core::Muxer;
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_packet::Packet;

use crate::algo::HashAlgo;
use crate::algo::RunningHash;

struct StreamState {
    media: Option<MediaType>,
    hasher: Option<RunningHash>,
}

/// `streamhash`.
#[derive(Debug)]
pub struct StreamHashMuxer {
    out: IoWriter,
    algo: HashAlgo,
    streams: Vec<StreamState>,
}

impl core::fmt::Debug for StreamState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StreamState")
            .field("media", &self.media)
            .field("hasher", &self.hasher)
            .finish()
    }
}

impl StreamHashMuxer {
    /// `streamhash`, algorithm chosen by the caller (the reference's own
    /// default, absent `-hash`, is SHA-256).
    ///
    /// # Errors
    /// Propagates buffer allocation failure from [`IoWriter`].
    pub fn new(sink: Box<dyn MediaSink>, algo: HashAlgo) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            algo,
            streams: Vec::new(),
        })
    }
}

impl Muxer for StreamHashMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        let idx = u32::try_from(self.streams.len()).unwrap_or(u32::MAX);
        self.streams.push(StreamState {
            media: params.effective_media_type(),
            hasher: self.algo.running(),
        });
        Ok(idx)
    }

    fn write_header(&mut self) -> Result<()> {
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if let Some(st) = usize::try_from(packet.stream_index)
            .ok()
            .and_then(|i| self.streams.get_mut(i))
            && let Some(h) = st.hasher.as_mut()
        {
            h.update(packet.payload());
        }
        Ok(())
    }

    fn write_trailer(&mut self) -> Result<()> {
        let mut buf = String::new();
        for (i, st) in self.streams.iter_mut().enumerate() {
            let Some(hasher) = st.hasher.take() else {
                continue;
            };
            let letter = st.media.map_or('?', MediaType::specifier_char);
            let _ = writeln!(
                buf,
                "{i},{letter},{}={}",
                self.algo.label(),
                hasher.finish_hex()
            );
        }
        self.out.write(buf.as_bytes())?;
        self.out.flush()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_format_core::vacoraw::MemorySink;
    use vaco_limits::{Budget, Limits};

    const FRAME0: &[u8] = include_bytes!("../tests/fixtures/testsrc_64x64_frame0.yuv");
    const FRAME1: &[u8] = include_bytes!("../tests/fixtures/testsrc_64x64_frame1.yuv");

    #[test]
    fn streamhash_matches_the_measured_reference_line() {
        // `ffmpeg -f lavfi -i testsrc=size=64x64:rate=5:duration=1 -pix_fmt
        // yuv420p -c:v rawvideo -frames:v 2 -f streamhash -hash crc32 -` on
        // just these two frames gives `0,v,CRC32=2dee74fe` — confirmed
        // independently against `zlib.crc32` on the concatenated fixture
        // bytes. (The multi-stream, full-duration probe in the module docs,
        // `0,v,CRC32=e03bd439` / `1,a,CRC32=f2a6c4ff`, is a different,
        // longer input and is not the value reproduced here.)
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = StreamHashMuxer::new(Box::new(sink), HashAlgo::Crc32).unwrap();
        m.add_stream(&CodecParameters::video()).unwrap();
        m.write_header().unwrap();
        let mut budget = Budget::new(Limits::strict());
        for frame in [FRAME0, FRAME1] {
            let mut p = Packet::from_slice(&mut budget, frame).unwrap();
            p.stream_index = 0;
            m.write_packet(&p).unwrap();
        }
        m.write_trailer().unwrap();
        assert_eq!(shared.snapshot(), b"0,v,CRC32=2dee74fe\n");
    }

    #[test]
    fn each_stream_hashes_only_its_own_packets() {
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut m = StreamHashMuxer::new(Box::new(sink), HashAlgo::Md5).unwrap();
        m.add_stream(&CodecParameters::video()).unwrap();
        m.add_stream(&CodecParameters::audio()).unwrap();
        m.write_header().unwrap();
        let mut budget = Budget::new(Limits::strict());
        let mut v = Packet::from_slice(&mut budget, b"video-bytes").unwrap();
        v.stream_index = 0;
        let mut a = Packet::from_slice(&mut budget, b"audio-bytes").unwrap();
        a.stream_index = 1;
        m.write_packet(&v).unwrap();
        m.write_packet(&a).unwrap();
        m.write_trailer().unwrap();

        let dumped = String::from_utf8(shared.snapshot()).unwrap();
        let lines: Vec<&str> = dumped.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("0,v,MD5="));
        assert!(lines[1].starts_with("1,a,MD5="));
        assert_ne!(lines[0], lines[1]);
    }
}
