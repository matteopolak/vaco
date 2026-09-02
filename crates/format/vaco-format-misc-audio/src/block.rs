//! Shared framing for every format in this crate that reduces, once its
//! (possibly nonexistent) header is parsed, to "the rest of the source is a
//! run of fixed-size blocks, each covering a fixed number of sample frames".
//!
//! That covers headerless ITU/3GPP speech codecs (a block is a byte, a
//! sample-count ratio is the whole codec), CRI ADX (a block is one 18-byte
//! ADPCM chunk), and the plain-PCM tail of `nistsphere`/`pvf` alike — the
//! same shape [`vaco_format_audio_simple::pcm::RawPcmDemuxer`] gives
//! byte-for-byte PCM, generalised to a block covering more than one frame.
//!
//! # `target_packet_bytes`: measured per format, not assumed
//!
//! `BlockDemuxer::new` takes the packet size to emit as an explicit,
//! required argument rather than picking one itself: no single constant is
//! correct. Measuring every consumer against `ffprobe`/`ffmpeg` 8.1 found
//! the reference emits
//! **one packet per block** for `adx`, `gsm` and `g729` (`18`/`33`/`10`
//! bytes respectively — confirmed directly against `-show_packets` on real
//! and hand-built fixtures), and a *different*, format-specific fixed byte
//! count for every other codec in `rawcodec.rs`: `1024` for `g722`, `1020`
//! for `g726`/`g726le`/`g728`, `512` for `dfpwm`, `1024` for `aptx`, `1536`
//! for `aptx_hd`, `1024` for `sln`. None of these divide evenly into a
//! single formula (`g722` and `g726` share the same 1:2 byte:frame ratio
//! and a fixed 8000/16000 Hz rate, yet batch into different byte counts),
//! so each is a directly measured, hardcoded constant on its own
//! `RawCodecSpec`/call site.
//!
//! `nistsphere` and `pvf` still pass [`DEFAULT_TARGET_PACKET_BYTES`]
//! (`4096`), which is **not** measured against the reference: their raw-PCM
//! payload showed the reference batching by a packet size that scales with
//! the stream's own sample rate in a way that was measured at several
//! points (roughly 64 ms of audio at low rates, rounded to a nearby power
//! of two, with an unexplained early transition somewhere between 16 kHz
//! and 32 kHz that broke every closed-form guess tried) but never reduced
//! to one clean rule. Reproducing it would mean guessing at un-pinned
//! behaviour, which is worse than an honestly approximate constant.

use vaco_core::{Duration, Error, Result, Timestamp};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::Stream;
use vaco_io::{IoContext, Seekability};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags};

/// The old, unmeasured default packet size, kept only for `nistsphere`/
/// `pvf`'s raw-PCM tail — see the module doc for why those two do not have
/// a measured value to use instead.
pub const DEFAULT_TARGET_PACKET_BYTES: u32 = 4096;

/// One elementary stream, framed as fixed-size blocks after `data_start`.
#[derive(Debug)]
pub struct BlockDemuxer {
    io: IoContext,
    stream: Stream,
    data_start: u64,
    /// Data bytes available, clamped to the source's own size when known.
    /// `None` means "read to EOF".
    data_len: Option<u64>,
    bytes_per_block: u32,
    frames_per_block: u32,
    /// Packets are sized to roughly this many bytes, rounded down to a
    /// whole number of blocks (never zero). Measured per format at the call
    /// site — see the module doc.
    target_packet_bytes: u32,
    blocks_emitted: u64,
    eof: bool,
}

impl BlockDemuxer {
    /// `declared_len` is clamped against `io.size()` here so no format module
    /// has to repeat that policy.
    #[must_use]
    #[allow(
        clippy::integer_division,
        reason = "exact block count from a byte count; a partial trailing block is discarded intentionally"
    )]
    pub fn new(
        io: IoContext,
        mut stream: Stream,
        data_start: u64,
        declared_len: Option<u64>,
        bytes_per_block: u32,
        frames_per_block: u32,
        target_packet_bytes: u32,
    ) -> Self {
        let bytes_per_block = bytes_per_block.max(1);
        let frames_per_block = frames_per_block.max(1);
        let target_packet_bytes = target_packet_bytes.max(bytes_per_block);
        let data_len = declared_len.map(|n| match io.size() {
            Some(size) => n.min(size.saturating_sub(data_start)),
            None => n,
        });
        if let Some(len) = data_len {
            let blocks = len / u64::from(bytes_per_block);
            let frames = blocks.saturating_mul(u64::from(frames_per_block));
            stream.duration_ts = i64::try_from(frames).ok();
            stream.frame_count = Some(frames);
        }
        Self {
            io,
            stream,
            data_start,
            data_len,
            bytes_per_block,
            frames_per_block,
            target_packet_bytes,
            blocks_emitted: 0,
            eof: false,
        }
    }

    #[must_use]
    pub fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    /// The block index the next [`Self::read_packet`] will start from.
    /// Exposed for a caller layered on top (e.g. `xa`'s own `dwOutSize`
    /// packet-count gate) that needs to re-derive its own block-based state
    /// after a seek lands here.
    #[must_use]
    pub fn block_index(&self) -> u64 {
        self.blocks_emitted
    }

    #[must_use]
    fn total_data_bytes(&self) -> Option<u64> {
        self.data_len
            .or_else(|| self.io.size().map(|s| s.saturating_sub(self.data_start)))
    }

    #[must_use]
    #[allow(
        clippy::integer_division,
        reason = "exact block count from a byte count; a partial trailing block is discarded intentionally"
    )]
    pub fn duration(&self) -> Option<Duration> {
        let bytes = self.total_data_bytes()?;
        let blocks = bytes / u64::from(self.bytes_per_block);
        let frames = blocks.saturating_mul(u64::from(self.frames_per_block));
        let rate = u64::from(self.stream.params.audio.as_ref()?.sample_rate.max(1));
        let micros = frames.checked_mul(1_000_000)?.checked_div(rate)?;
        Some(Duration::from_micros(
            i64::try_from(micros).unwrap_or(i64::MAX),
        ))
    }

    /// # Errors
    /// [`Error::Eof`] at the end of the data; propagates transport failure
    /// and [`Error::LimitExceeded`] from `budget`.
    #[allow(
        clippy::integer_division,
        reason = "exact block count from a byte count; a partial trailing block is discarded intentionally"
    )]
    pub fn read_packet(&mut self, budget: &mut Budget) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        let bpb = u64::from(self.bytes_per_block);
        let pos_bytes = self.blocks_emitted.saturating_mul(bpb);
        if let Some(len) = self.data_len
            && pos_bytes >= len
        {
            self.eof = true;
            return Err(Error::Eof);
        }

        let target = self.target_packet_bytes as usize;
        let mut want = target - target % self.bytes_per_block as usize;
        if want == 0 {
            want = self.bytes_per_block as usize;
        }
        if let Some(len) = self.data_len {
            let remaining = usize::try_from(len.saturating_sub(pos_bytes)).unwrap_or(usize::MAX);
            // Clamp to a whole number of blocks: a partial trailing block at
            // the declared end is discarded, never read as a short packet.
            let remaining_whole = remaining - remaining % self.bytes_per_block as usize;
            want = want.min(remaining_whole);
        }
        if want == 0 {
            self.eof = true;
            return Err(Error::Eof);
        }

        let mut pkt = Packet::alloc(budget, want)?;
        let mut n = 0usize;
        while let Some(rest) = pkt.payload_mut().get_mut(n..) {
            if rest.is_empty() {
                break;
            }
            let got = self.io.read_partial(rest)?;
            if got == 0 {
                break;
            }
            n = n.saturating_add(got);
        }
        if n == 0 {
            self.eof = true;
            return Err(Error::Eof);
        }
        pkt.len = n;
        pkt.stream_index = 0;
        let whole_blocks = u64::try_from(n).unwrap_or(0) / bpb;
        let frame_index = self
            .blocks_emitted
            .saturating_mul(u64::from(self.frames_per_block));
        pkt.pts = Timestamp::new(i64::try_from(frame_index).unwrap_or(i64::MAX));
        pkt.dts = pkt.pts;
        pkt.flags = PacketFlags::KEY;
        pkt.pos = Some(self.data_start.saturating_add(pos_bytes));

        if whole_blocks == 0 {
            // A short final read that does not land on a block boundary is
            // EOF, not corruption: stop rather than re-read a fractional
            // tail forever.
            self.eof = true;
        }
        if let Some(audio) = self.stream.params.audio.as_ref() {
            let rate = u64::from(audio.sample_rate.max(1));
            let frames = whole_blocks.saturating_mul(u64::from(self.frames_per_block));
            let micros = frames.saturating_mul(1_000_000).checked_div(rate).unwrap_or(0);
            pkt.duration = Duration::from_micros(i64::try_from(micros).unwrap_or(i64::MAX));
        }
        self.blocks_emitted = self.blocks_emitted.saturating_add(whole_blocks.max(1));
        Ok(pkt)
    }

    /// Byte-accurate seek: a timestamp or frame target converts to a block
    /// index and then a byte offset. No block in this family carries a
    /// keyframe distinction.
    ///
    /// # Errors
    /// [`Error::NotSeekable`] if the source cannot seek.
    #[allow(
        clippy::integer_division,
        reason = "exact block index from a byte offset; a partial trailing block is discarded intentionally"
    )]
    pub fn seek(&mut self, target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        if self.io.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }
        let frame = match target {
            SeekTarget::Byte(b) => {
                let block = b.saturating_sub(self.data_start) / u64::from(self.bytes_per_block);
                block.saturating_mul(u64::from(self.frames_per_block))
            }
            SeekTarget::Frame { frame, .. } => frame,
            SeekTarget::Timestamp { ts, .. } => {
                let ticks = ts.ticks().unwrap_or(0);
                u64::try_from(ticks.max(0)).unwrap_or(0)
            }
        };
        let block = frame / u64::from(self.frames_per_block);
        let byte_pos = self
            .data_start
            .saturating_add(block.saturating_mul(u64::from(self.bytes_per_block)));
        self.io.seek(byte_pos)?;
        self.blocks_emitted = block;
        self.eof = false;
        Ok(())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_codec_core::CodecParameters;
    use vaco_core::{MediaType, Rational};
    use vaco_io::{IoOptions, MemorySource};
    use vaco_limits::Limits;

    fn demux_of(data: Vec<u8>, bpb: u32, fpb: u32, declared_len: Option<u64>) -> BlockDemuxer {
        demux_of_with_target(data, bpb, fpb, declared_len, DEFAULT_TARGET_PACKET_BYTES)
    }

    fn demux_of_with_target(
        data: Vec<u8>,
        bpb: u32,
        fpb: u32,
        declared_len: Option<u64>,
        target_packet_bytes: u32,
    ) -> BlockDemuxer {
        let src = Box::new(MemorySource::new(data));
        let io = IoContext::new(src, &IoOptions::default()).unwrap();
        let mut stream = Stream::new(0, MediaType::Audio, Rational::new(1, 8000));
        stream.params = CodecParameters::audio();
        if let Some(a) = stream.params.audio.as_mut() {
            a.sample_rate = 8000;
        }
        BlockDemuxer::new(io, stream, 0, declared_len, bpb, fpb, target_packet_bytes)
    }

    #[test]
    fn packets_cover_the_whole_stream_with_increasing_pts() {
        let data = vec![0xABu8; DEFAULT_TARGET_PACKET_BYTES as usize * 2 + 40];
        let mut d = demux_of(data.clone(), 4, 2, Some(data.len() as u64));
        let mut budget = Budget::new(Limits::permissive());
        let mut total = 0usize;
        let mut last_pts = -1i64;
        loop {
            match d.read_packet(&mut budget) {
                Ok(pkt) => {
                    assert!(pkt.pts.ticks().unwrap() > last_pts);
                    last_pts = pkt.pts.ticks().unwrap();
                    total += pkt.len;
                }
                Err(Error::Eof) => break,
                Err(e) => panic!("unexpected error {e:?}"),
            }
        }
        assert_eq!(total, data.len());
    }

    #[test]
    fn an_unbounded_stream_reads_to_true_eof() {
        let data = vec![0x11u8; 101];
        let mut d = demux_of(data.clone(), 1, 4, None);
        let mut budget = Budget::new(Limits::permissive());
        let mut total = 0usize;
        loop {
            match d.read_packet(&mut budget) {
                Ok(pkt) => total += pkt.len,
                Err(Error::Eof) => break,
                Err(e) => panic!("unexpected error {e:?}"),
            }
        }
        assert_eq!(total, 101);
    }

    #[test]
    fn seeking_lands_on_a_block_boundary() {
        let data = (0u8..=200).collect::<Vec<_>>();
        let mut d = demux_of(data.clone(), 4, 2, Some(data.len() as u64));
        d.seek(
            SeekTarget::Frame {
                stream_index: 0,
                frame: 6,
            },
            SeekFlags::empty(),
        )
        .unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let pkt = d.read_packet(&mut budget).unwrap();
        assert_eq!(pkt.pts.ticks(), Some(6));
        assert_eq!(pkt.payload()[0], data[12]);
    }

    #[test]
    fn zero_sized_block_params_do_not_divide_by_zero() {
        let data = vec![1u8; 8];
        let mut d = demux_of(data, 0, 0, Some(8));
        let mut budget = Budget::new(Limits::permissive());
        assert!(d.read_packet(&mut budget).is_ok());
    }

    #[test]
    fn a_declared_length_longer_than_the_source_is_clamped() {
        let data = vec![0x22u8; 10];
        let mut d = demux_of(data, 2, 1, Some(10_000));
        let mut budget = Budget::new(Limits::permissive());
        let mut total = 0usize;
        loop {
            match d.read_packet(&mut budget) {
                Ok(pkt) => total += pkt.len,
                Err(Error::Eof) => break,
                Err(e) => panic!("unexpected error {e:?}"),
            }
        }
        assert_eq!(total, 10);
    }
}
