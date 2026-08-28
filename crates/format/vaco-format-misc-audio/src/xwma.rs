//! Microsoft xWMA (`.xwma`): a RIFF container around raw WMA frames, used by
//! `XAudio2`. A thin wrapper — the container's whole job is naming a codec
//! that already has a `CodecId` and framing its data, not describing one of
//! its own.
//!
//! # Layout, measured against hand-built fixtures and `ffprobe`/`ffmpeg` 8.1
//!
//! ```text
//! "RIFF"  file_size:le32  "XWMA"
//! chunk*                          -- id:[u8;4] size:le32 data[size] pad?
//!   "fmt "  WAVEFORMATEX (18 bytes: wFormatTag, nChannels, nSamplesPerSec,
//!           nAvgBytesPerSec, nBlockAlign, wBitsPerSample, cbSize) plus
//!           cbSize bytes of codec-specific extra data
//!   "dpds"  le32[]                -- present in every real file, see below
//!   "data"  raw WMA frame bytes
//! ```
//!
//! `WAVEFORMATEX`'s field layout is the public Win32 API struct (not an
//! `FFmpeg` artifact); the xWMA-specific chunk set (`fmt `/`dpds`/`data`) is
//! documented on `MultimediaWiki` (`Vaco-Spec-Ref multimedia-wiki-xwma`) and
//! corroborated by Microsoft's own RIFF/XAudio2 reference
//! (`Vaco-Spec-Ref microsoft-riff-xaudio2`).
//!
//! # Two surprises the reference's framing had, neither guessable from the
//! chunk names alone
//!
//! **`dpds` is not what splits the data into packets.** It looks like a
//! per-packet byte-offset table (a cumulative `le32` per encoded WMA
//! frame), and `MultimediaWiki` describes it that way, but measuring against
//! `ffprobe` shows the reference ignores it for packetisation: a `dpds`
//! table declaring four packets of `100/150/120/90` bytes still produces
//! **one** `-show_packets` entry covering the whole 460-byte `data` chunk,
//! because that data chunk is shorter than the `fmt` chunk's own
//! `nBlockAlign` (2230 in that fixture) — i.e. packets are `nBlockAlign`-
//! aligned reads of `data`, exactly like a generic PCM/ADPCM WAV, with the
//! final short block still emitted. A second fixture with `nBlockAlign =
//! 100` over 350 bytes of `data` confirmed it: four packets of
//! `100/100/100/50` bytes, matching block alignment exactly and
//! contradicting the `dpds`-declared `100/150/120/90` split. This module
//! therefore parses `dpds` only far enough to skip over it and does not use
//! it for anything.
//!
//! **A `WMAv2` stream with no extra data in its `fmt` chunk gets a fixed
//! 6-byte extradata synthesised by the reference anyway.** Measured via
//! `ffprobe -show_data_hash md5 -show_entries stream=extradata_size` and
//! confirmed byte-exact by hashing candidates: `extradata_size=6`,
//! `extradata_hash=MD5:1833e47c…`, which matches `00 00 00 00 1F 00`
//! exactly and no other candidate. The same test with `wFormatTag =
//! 0x0160` (`WMAv1`) produces no extradata at all — this synthesis is `WMAv2`-
//! specific. This module reproduces it: when the `fmt` chunk's `cbSize` is
//! zero and `wFormatTag` selects `Wmav2`, `codec_parameters.extradata` is
//! set to `[0x00, 0x00, 0x00, 0x00, 0x1F, 0x00]`. A `fmt` chunk that *does*
//! carry `cbSize` bytes of its own is passed through unmodified — not
//! independently measured, since building a fixture with genuine WMA
//! codec-private data was out of scope for framing work.
//!
//! # Packet timing
//!
//! Per-packet duration in samples is `floor(packet_bytes * nSamplesPerSec /
//! nAvgBytesPerSec)` — confirmed exactly on both fixtures above (a full
//! 100-byte block at 8000 Hz / 1000 B/s reports `duration=800`; the whole
//! 460-byte read at 44100 Hz / 8000 B/s reports `duration=2535`, i.e.
//! `floor(460*44100/8000)`). `pts`/`dts` accumulate that per-packet sample
//! count.
//!
//! # A real, unresolved divergence: stream-level duration when `dpds` exists
//!
//! The per-packet duration formula above holds whether or not a `dpds`
//! chunk is present. The **stream-level** `duration_ts`/`duration` do not:
//! on a 350-byte `data` chunk at 8000 Hz / 1000 B/s with no `dpds` chunk,
//! the reference reports `duration_ts=2800` (`350*8000/1000`, the same
//! formula as the packet level, applied to the whole chunk). Add *any*
//! `dpds` chunk — one entry, two, four, seven, with any content — and the
//! reported `duration_ts` becomes a fixed `175` regardless of what is in
//! it, `2800 / 16` for no principled reason this crate could find (`16`
//! being the byte size of one four-entry `dpds` chunk tried, but the same
//! `175` also came back for one-, two- and seven-entry `dpds` chunks of
//! different byte sizes, ruling out "divided by the `dpds` chunk's byte
//! length" as the mechanism). This looks like a symptom of `ffprobe`'s
//! generic duration-estimation fallback reacting to `dpds`-signalled "real
//! WMA container" by attempting something codec-probe-dependent against
//! this crate's non-decodable synthetic payload, rather than a fact about
//! xWMA's container framing — but that is a hypothesis, not a measurement,
//! and confirming it would mean building genuinely valid WMA bitstream
//! data, which is out of scope for framing work. This crate always uses
//! the plain byte-rate formula for `duration_ts` and does not attempt to
//! reproduce whatever this is; the fixture this crate's own tests
//! differentially check against was deliberately built without a `dpds`
//! chunk so the comparison is against the unambiguous number.
//!
//! # `codec_id` mapping
//!
//! `wFormatTag` `0x0160`/`0x0161`/`0x0162` map to the existing
//! `CodecId::Wmav1`/`Wmav2`/`Wmapro` — no `vaco-codec-core` gap here, unlike
//! most of this crate's other new formats this session. Any other
//! `wFormatTag` leaves `codec_id: None`.

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, DemuxerDesc, FormatFlags, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags};

/// Synthesised `WMAv2` extradata the reference always supplies when the `fmt`
/// chunk itself carries none. See the module doc for how this was measured.
const WMAV2_DEFAULT_EXTRADATA: [u8; 6] = [0x00, 0x00, 0x00, 0x00, 0x1F, 0x00];
/// Bounds a chunk's declared size before it is used to seek/allocate.
const MAX_CHUNK: u64 = 1 << 31;
const MAX_FMT_EXTRA: usize = 1 << 16;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(b"RIFF") && data.tag(8) == Some(*b"XWMA") {
        ProbeScore::MAGIC_CHECKED
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "xwma",
    long_name: "Microsoft xWMA",
    extensions: &["xwma"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(XwmaDemuxer::open(src)?))
}

fn codec_for_format_tag(tag: u16) -> Option<CodecId> {
    match tag {
        0x0160 => Some(CodecId::Wmav1),
        0x0161 => Some(CodecId::Wmav2),
        0x0162 => Some(CodecId::Wmapro),
        _ => None,
    }
}

#[derive(Debug)]
pub struct XwmaDemuxer {
    io: IoContext,
    stream: Stream,
    data_start: u64,
    data_len: u64,
    block_align: u32,
    sample_rate: u32,
    avg_bytes_per_sec: u32,
    bytes_read: u64,
    eof: bool,
    budget: Budget,
}

impl XwmaDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the RIFF/XWMA framing or `fmt` chunk is
    /// malformed, or no `data` chunk is found.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut riff = [0u8; 4];
        io.read_exact(&mut riff)?;
        if &riff != b"RIFF" {
            return Err(Error::InvalidData("xwma: missing RIFF signature"));
        }
        let _riff_size = io.rl32()?;
        let mut form = [0u8; 4];
        io.read_exact(&mut form)?;
        if &form != b"XWMA" {
            return Err(Error::InvalidData("xwma: not an XWMA-typed RIFF file"));
        }

        let mut format_tag: Option<u16> = None;
        let mut channels: u16 = 0;
        let mut sample_rate: u32 = 0;
        let mut avg_bytes_per_sec: u32 = 0;
        let mut block_align: u32 = 0;
        let mut extra: Vec<u8> = Vec::new();
        let mut data_start = None;
        let mut data_len = 0u64;

        loop {
            let mut id = [0u8; 4];
            if io.read_exact(&mut id).is_err() {
                break;
            }
            let size = u64::from(io.rl32()?);
            if size > MAX_CHUNK {
                return Err(Error::InvalidData("xwma: implausible chunk size"));
            }
            let chunk_start = io.pos();
            match &id {
                b"fmt " => {
                    format_tag = Some(io.rl16()?);
                    channels = io.rl16()?;
                    sample_rate = io.rl32()?;
                    avg_bytes_per_sec = io.rl32()?;
                    block_align = u32::from(io.rl16()?);
                    let _bits_per_sample = io.rl16()?;
                    if io.pos() < chunk_start.saturating_add(size) {
                        let cb_size = usize::from(io.rl16()?);
                        let cb_size = cb_size.min(MAX_FMT_EXTRA);
                        extra = vec_from_budget(&mut io, cb_size)?;
                    }
                }
                b"data" => {
                    data_start = Some(chunk_start);
                    data_len = size;
                }
                _ => {}
            }
            let padded = size.saturating_add(size & 1);
            io.seek(chunk_start.saturating_add(padded))?;
            if id == *b"data" {
                // The data chunk can be large; nothing after it matters for
                // framing, and `dpds` (whichever side of `data` it falls on)
                // is not read for anything — see the module doc.
                break;
            }
        }

        let Some(format_tag) = format_tag else {
            return Err(Error::InvalidData("xwma: no fmt chunk"));
        };
        let Some(data_start) = data_start else {
            return Err(Error::InvalidData("xwma: no data chunk"));
        };
        if sample_rate == 0 || channels == 0 {
            return Err(Error::InvalidData("xwma: implausible fmt chunk"));
        }
        let clamped_len = match io.size() {
            Some(total) => data_len.min(total.saturating_sub(data_start)),
            None => data_len,
        };

        let mut stream = Stream::new(0, MediaType::Audio, Rational::new(1, sample_rate.cast_signed()));
        let mut params = CodecParameters::audio();
        params.codec_id = codec_for_format_tag(format_tag);
        if extra.is_empty() && format_tag == 0x0161 {
            extra = WMAV2_DEFAULT_EXTRADATA.to_vec();
        }
        params.extradata = if extra.is_empty() { None } else { Some(extra) };
        if let Some(audio) = params.audio.as_mut() {
            audio.sample_rate = sample_rate;
            audio.layout = ChannelLayout::default_for(u32::from(channels));
        }
        stream.params = params;
        if avg_bytes_per_sec > 0 {
            #[allow(
                clippy::integer_division,
                reason = "sample-count estimate from a byte count; matches the measured reference formula"
            )]
            let frames = clamped_len.saturating_mul(u64::from(sample_rate)) / u64::from(avg_bytes_per_sec);
            stream.duration_ts = i64::try_from(frames).ok();
        }

        io.seek(data_start)?;
        Ok(Self {
            io,
            stream,
            data_start,
            data_len: clamped_len,
            block_align: block_align.max(1),
            sample_rate,
            avg_bytes_per_sec: avg_bytes_per_sec.max(1),
            bytes_read: 0,
            eof: false,
            budget: Budget::new(vaco_limits::Limits::permissive()),
        })
    }

    #[allow(clippy::integer_division, reason = "packet-duration estimate from a byte count; matches the measured reference formula")]
    fn samples_for(&self, bytes: usize) -> i64 {
        (bytes as u64 * u64::from(self.sample_rate) / u64::from(self.avg_bytes_per_sec))
            .try_into()
            .unwrap_or(i64::MAX)
    }
}

fn vec_from_budget(io: &mut IoContext, len: usize) -> Result<Vec<u8>> {
    let mut budget = Budget::new(vaco_limits::Limits::permissive());
    let mut buf = budget.alloc::<u8>(len)?;
    io.read_exact(&mut buf)?;
    Ok(buf)
}

impl Demuxer for XwmaDemuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        if self.bytes_read >= self.data_len {
            self.eof = true;
            return Err(Error::Eof);
        }
        let remaining = self.data_len.saturating_sub(self.bytes_read);
        let want = remaining.min(u64::from(self.block_align)) as usize;
        let pos = self.data_start.saturating_add(self.bytes_read);

        let mut pkt = Packet::alloc(&mut self.budget, want)?;
        let n = self.io.read_partial(pkt.payload_mut())?;
        if n == 0 {
            self.eof = true;
            return Err(Error::Eof);
        }
        pkt.len = n;
        pkt.stream_index = 0;
        let sample_pos = self.samples_for(usize::try_from(self.bytes_read).unwrap_or(usize::MAX));
        pkt.pts = Timestamp::new(sample_pos);
        pkt.dts = pkt.pts;
        let dur = self.samples_for(n);
        pkt.duration = Duration::from_micros(
            dur.saturating_mul(1_000_000)
                .checked_div(i64::from(self.sample_rate))
                .unwrap_or(0),
        );
        pkt.flags = PacketFlags::KEY;
        pkt.pos = Some(pos);
        self.bytes_read = self.bytes_read.saturating_add(n as u64);
        if n < want {
            self.eof = true;
        }
        Ok(pkt)
    }

    fn seek(&mut self, target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        if self.io.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }
        let frame_to_bytes = |frame: u64| {
            frame
                .saturating_mul(u64::from(self.avg_bytes_per_sec))
                .checked_div(u64::from(self.sample_rate))
                .unwrap_or(0)
        };
        let byte_offset = match target {
            SeekTarget::Byte(b) => b.saturating_sub(self.data_start),
            SeekTarget::Frame { frame, .. } => frame_to_bytes(frame),
            SeekTarget::Timestamp { ts, .. } => {
                frame_to_bytes(u64::try_from(ts.ticks().unwrap_or(0).max(0)).unwrap_or(0))
            }
        };
        #[allow(clippy::integer_division, reason = "block-align a byte offset down to a packet boundary")]
        let block = byte_offset / u64::from(self.block_align);
        let aligned = block.saturating_mul(u64::from(self.block_align)).min(self.data_len);
        self.io.seek(self.data_start.saturating_add(aligned))?;
        self.bytes_read = aligned;
        self.eof = false;
        Ok(())
    }

    #[allow(
        clippy::integer_division,
        reason = "sample-count estimate from a byte count; matches the measured reference formula"
    )]
    fn duration(&self) -> Option<Duration> {
        let frames = self.data_len.saturating_mul(u64::from(self.sample_rate)) / u64::from(self.avg_bytes_per_sec);
        let micros = frames.checked_mul(1_000_000)?.checked_div(u64::from(self.sample_rate))?;
        Some(Duration::from_micros(i64::try_from(micros).unwrap_or(i64::MAX)))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    fn chunk(id: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        v.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            v.push(0);
        }
        v
    }

    fn build_file(
        format_tag: u16,
        channels: u16,
        sample_rate: u32,
        avg_bytes_per_sec: u32,
        block_align: u32,
        data: &[u8],
    ) -> Vec<u8> {
        let fmt_payload = {
            let mut v = Vec::new();
            v.extend_from_slice(&format_tag.to_le_bytes());
            v.extend_from_slice(&channels.to_le_bytes());
            v.extend_from_slice(&sample_rate.to_le_bytes());
            v.extend_from_slice(&avg_bytes_per_sec.to_le_bytes());
            v.extend_from_slice(&(block_align as u16).to_le_bytes());
            v.extend_from_slice(&16u16.to_le_bytes());
            v.extend_from_slice(&0u16.to_le_bytes());
            v
        };
        let dpds_payload = {
            let mut v = Vec::new();
            v.extend_from_slice(&100u32.to_le_bytes());
            v.extend_from_slice(&250u32.to_le_bytes());
            v
        };
        let mut body = b"XWMA".to_vec();
        body.extend(chunk(*b"fmt ", &fmt_payload));
        body.extend(chunk(*b"dpds", &dpds_payload));
        body.extend(chunk(*b"data", data));
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&(body.len() as u32).to_le_bytes());
        v.extend(body);
        v
    }

    #[test]
    fn block_aligned_packets_ignore_the_dpds_split() {
        let data = vec![0xAB; 350];
        let file = build_file(0x0161, 1, 8000, 1000, 100, &data);
        let mut d = XwmaDemuxer::open(Box::new(MemorySource::new(file))).unwrap();
        let mut sizes = Vec::new();
        loop {
            match d.read_packet() {
                Ok(pkt) => sizes.push(pkt.len),
                Err(Error::Eof) => break,
                Err(e) => panic!("unexpected error {e:?}"),
            }
        }
        // Block-aligned (100 bytes), NOT the dpds-declared 100/150/120/90/... split.
        assert_eq!(sizes, vec![100, 100, 100, 50]);
    }

    #[test]
    fn wmav2_with_no_extra_fmt_data_gets_the_synthesised_extradata() {
        let file = build_file(0x0161, 2, 44_100, 8000, 2230, &[0u8; 40]);
        let d = XwmaDemuxer::open(Box::new(MemorySource::new(file))).unwrap();
        let s = d.streams().first().unwrap();
        assert_eq!(s.params.extradata, Some(WMAV2_DEFAULT_EXTRADATA.to_vec()));
        assert_eq!(s.params.codec_id, Some(CodecId::Wmav2));
    }

    #[test]
    fn wmav1_gets_no_synthesised_extradata() {
        let file = build_file(0x0160, 1, 8000, 1000, 100, &[0u8; 40]);
        let d = XwmaDemuxer::open(Box::new(MemorySource::new(file))).unwrap();
        let s = d.streams().first().unwrap();
        assert!(s.params.extradata.is_none());
        assert_eq!(s.params.codec_id, Some(CodecId::Wmav1));
    }

    #[test]
    fn probe_checks_riff_and_xwma_form_type() {
        let file = build_file(0x0161, 1, 8000, 1000, 100, &[0u8; 8]);
        assert_eq!(probe(&ProbeData::new(&file)), ProbeScore::MAGIC_CHECKED);
        let mut wav = file.clone();
        if let Some(slot) = wav.get_mut(8..12) {
            slot.copy_from_slice(b"WAVE");
        }
        assert_eq!(probe(&ProbeData::new(&wav)), ProbeScore::NONE);
    }

    #[test]
    fn missing_data_chunk_is_rejected() {
        let fmt_payload = {
            let mut v = Vec::new();
            v.extend_from_slice(&0x0161u16.to_le_bytes());
            v.extend_from_slice(&1u16.to_le_bytes());
            v.extend_from_slice(&8000u32.to_le_bytes());
            v.extend_from_slice(&1000u32.to_le_bytes());
            v.extend_from_slice(&100u16.to_le_bytes());
            v.extend_from_slice(&16u16.to_le_bytes());
            v.extend_from_slice(&0u16.to_le_bytes());
            v
        };
        let mut body = b"XWMA".to_vec();
        body.extend(chunk(*b"fmt ", &fmt_payload));
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&(body.len() as u32).to_le_bytes());
        v.extend(body);
        assert!(XwmaDemuxer::open(Box::new(MemorySource::new(v))).is_err());
    }
}
