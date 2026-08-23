//! `webm_chunk`: `WebM` output split at `Cluster` boundaries for DASH-style
//! segmented delivery.
//!
//! # What this crate can and cannot do here
//!
//! Measured against `ffmpeg 8.1` (`ffmpeg -h muxer=webm_chunk`): the real
//! muxer writes one file per chunk on disk, named from the output pattern,
//! plus a separate `-header` file holding the initialization segment, driven
//! by `-chunk_start_index` and `-audio_chunk_duration`.
//!
//! [`vaco_format_core::Muxer`] cannot do that. [`MuxerDesc::open`] receives
//! exactly one already-opened [`vaco_io::MediaSink`] — there is no channel
//! for opening a second file, let alone a numbered sequence of them, and no
//! channel for muxer-private options like `-chunk_start_index` to reach a
//! `Muxer` built through the registry at all (`FormatOptions` is generic
//! across every container; these two are `webm_chunk`-only `AVOption`s).
//!
//! So this is what is actually implemented: [`WebmChunkMuxer`] wraps
//! [`MatroskaMuxer`] configured for `webm`, forces every `Cluster` boundary
//! to fall on a chunk boundary (RFC 9559 places no restriction the other way
//! — a `Cluster` may already start anywhere), and writes the whole thing as
//! one continuous stream — exactly the bytes the numbered chunk files would
//! contain if concatenated in order. [`WebmChunkMuxer::chunk_boundaries`]
//! reports where each chunk starts, in the underlying byte stream, numbered
//! from `chunk_start_index`, so a caller that *does* have multi-file
//! capability (a CLI segmenter, say) can still cut the stream into the real
//! files without this crate needing to grow one.
//!
//! `-header`'s separate initialization file is the header chunk (`EBML` +
//! `Segment` + `Info` + `Tracks`, no `Cluster`) — chunk index
//! `chunk_start_index - 1` by convention in [`WebmChunkMuxer::chunk_boundaries`]
//! is not emitted as a distinct entry, since it and chunk `chunk_start_index`
//! share nothing to split on: it is simply everything before the first
//! reported boundary.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::Result;
use vaco_format_core::mux::{BitstreamAction, CodecSupport};
use vaco_format_core::options::FormatOptions;
use vaco_format_core::{FormatFlags, Muxer, MuxerDesc};
use vaco_io::MediaSink;
use vaco_packet::Packet;

use crate::mux::{MatroskaMuxer, WEBM};

/// `ffmpeg 8.1`'s own default (`-h muxer=webm_chunk`): five seconds.
const DEFAULT_AUDIO_CHUNK_DURATION_MS: i64 = 5000;

/// The registry descriptor for `webm_chunk`.
pub const MUXER_WEBM_CHUNK: MuxerDesc = MuxerDesc {
    name: "webm_chunk",
    long_name: "WebM Chunk Muxer",
    extensions: &["chk"],
    default_video: Some(CodecId::Vp9),
    default_audio: Some(CodecId::Opus),
    open: open_webm_chunk,
};

fn open_webm_chunk(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(WebmChunkMuxer::new(
        sink,
        &FormatOptions::default(),
        0,
        DEFAULT_AUDIO_CHUNK_DURATION_MS,
    )?))
}

/// The `webm_chunk` muxer. See the module docs for what "chunk" means here.
#[derive(Debug)]
pub struct WebmChunkMuxer {
    inner: MatroskaMuxer,
    chunk_start_index: u32,
}

impl WebmChunkMuxer {
    /// A muxer over `sink`, numbering chunks from `chunk_start_index` and
    /// capping each one at `audio_chunk_duration_ms`.
    ///
    /// # Errors
    ///
    /// As [`MatroskaMuxer::new`].
    pub fn new(
        sink: Box<dyn MediaSink>,
        opts: &FormatOptions,
        chunk_start_index: u32,
        audio_chunk_duration_ms: i64,
    ) -> Result<Self> {
        let mut inner = MatroskaMuxer::new(WEBM, sink, opts)?;
        inner.set_max_cluster_ms(audio_chunk_duration_ms.max(1));
        Ok(Self {
            inner,
            chunk_start_index,
        })
    }

    /// Where each chunk begins, as `(chunk_index, byte_offset)` pairs in
    /// write order. `byte_offset` is absolute in the single stream this
    /// muxer writes; everything before the first pair is the header chunk.
    pub fn chunk_boundaries(&self) -> impl Iterator<Item = (u32, u64)> + '_ {
        self.inner
            .cluster_starts()
            .iter()
            .enumerate()
            .map(move |(i, &pos)| (self.chunk_start_index.saturating_add(i as u32), pos))
    }
}

impl Muxer for WebmChunkMuxer {
    fn flags(&self) -> FormatFlags {
        self.inner.flags()
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        self.inner.add_stream(params)
    }

    fn init(&mut self) -> Result<()> {
        self.inner.init()
    }

    fn write_header(&mut self) -> Result<()> {
        self.inner.write_header()
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        self.inner.write_packet(packet)
    }

    fn write_trailer(&mut self) -> Result<()> {
        self.inner.write_trailer()
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<vaco_core::Rational> {
        self.inner.stream_time_base(stream_index)
    }

    fn query_codec(&self, codec: CodecId, strict: i32) -> CodecSupport {
        self.inner.query_codec(codec, strict)
    }

    fn check_bitstream(
        &mut self,
        params: &CodecParameters,
        pkt: &Packet,
    ) -> Result<BitstreamAction> {
        self.inner.check_bitstream(params, pkt)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_codec_core::VideoParameters;
    use vaco_core::{Rational, Timestamp};
    use vaco_format_core::vacoraw::MemorySink;
    use vaco_packet::PacketFlags;

    fn video_params() -> CodecParameters {
        let mut p = CodecParameters::video().with_codec(CodecId::Vp9);
        p.video = Some(VideoParameters {
            width: 32,
            height: 32,
            frame_rate: Rational::new(10, 1),
            ..VideoParameters::default()
        });
        p
    }

    fn pkt(stream: u32, pts_ms: i64, key: bool) -> Packet {
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::strict());
        let mut p = Packet::from_slice(&mut budget, b"x").unwrap();
        p.stream_index = stream;
        p.pts = Timestamp::new(pts_ms);
        p.dts = p.pts;
        if key {
            p.flags = PacketFlags::KEY;
        }
        p
    }

    #[test]
    fn a_time_cap_below_the_stream_forces_more_than_one_chunk_boundary() {
        let sink = MemorySink::new();
        let mut mux =
            WebmChunkMuxer::new(Box::new(sink), &FormatOptions::default(), 3, 100).unwrap();
        let idx = mux.add_stream(&video_params()).unwrap();
        mux.write_header().unwrap();
        // Ten frames at 100ms apart span a full second; a 100ms cap must
        // therefore produce several boundaries, not one giant cluster.
        for i in 0..10i64 {
            mux.write_packet(&pkt(idx, i * 100, true)).unwrap();
        }
        mux.write_trailer().unwrap();
        let boundaries: Vec<_> = mux.chunk_boundaries().collect();
        assert!(boundaries.len() > 1, "{boundaries:?}");
        // Numbering starts at the configured index.
        assert_eq!(boundaries.first().map(|&(i, _)| i), Some(3));
        // Strictly increasing indices and offsets.
        for w in boundaries.windows(2) {
            assert!(w[1].0 > w[0].0);
            assert!(w[1].1 > w[0].1);
        }
    }
}
