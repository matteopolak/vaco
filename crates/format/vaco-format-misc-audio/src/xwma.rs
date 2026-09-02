//! Microsoft xWMA (`.xwma`): a RIFF container around raw WMA frames, used by
//! `XAudio2`. It names a codec that already has a `CodecId` and frames its
//! data; it describes none of its own. Everything below was measured against
//! hand-built fixtures and `ffprobe`/`ffmpeg` 8.1.
//!
//! ```text
//! "RIFF"  file_size:le32  "XWMA"
//! chunk*                        -- id:[u8;4] size:le32 data[size] pad?
//!   "fmt "  WAVEFORMATEX (wFormatTag, nChannels, nSamplesPerSec,
//!           nAvgBytesPerSec, nBlockAlign, wBitsPerSample, cbSize) + cbSize
//!   "dpds"  le32[]              -- in every real file; unused here
//!   "data"  raw WMA frame bytes
//! ```
//!
//! Chunk set documented on `MultimediaWiki`
//! (`Vaco-Spec-Ref multimedia-wiki-xwma`), corroborated by Microsoft's
//! RIFF/XAudio2 reference (`Vaco-Spec-Ref microsoft-riff-xaudio2`).
//!
//! **`dpds` does not drive packetisation**, though `MultimediaWiki` calls
//! it a cumulative per-frame offset table. Packets are
//! `nBlockAlign`-aligned reads of `data`, final short block included: a
//! `dpds` declaring `100/150/120/90` still gave one packet over all 460
//! bytes (`nBlockAlign` 2230); `nBlockAlign = 100` over 350 bytes gave
//! `100/100/100/50`.
//!
//! **`WMAv2` with `cbSize == 0` still gets 6 bytes of extradata**:
//! `extradata_size=6`, `extradata_hash=MD5:1833e47c…`, matching
//! `00 00 00 00 1F 00` and no other candidate. `wFormatTag = 0x0160`
//! (`WMAv1`) produces none. A `fmt` with its own `cbSize` bytes passes
//! through unmodified (not independently measured).
//!
//! **A `dpds` chunk switches `duration_ts` to PCM arithmetic.** Per-packet
//! duration is always `floor(packet_bytes * nSamplesPerSec /
//! nAvgBytesPerSec)`, and stream `duration_ts` uses that over the whole
//! chunk when `dpds` is absent (350 bytes at 8000 Hz / 1000 B/s -> `2800`);
//! with any `dpds` present it becomes `data_len / (channels *
//! wBitsPerSample / 8)`, confirmed across mono/stereo, 8/16-bit, `wmav1`
//! and `wmav2`. `wFormatTag` `0x0160`/`0x0161`/`0x0162` map to
//! `CodecId::Wmav1`/`Wmav2`/`Wmapro`; any other tag leaves `codec_id: None`.

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
    /// `channels * bytes_per_sample` from the `fmt` chunk — used only for
    /// the `dpds`-present `duration_ts` quirk below, never for packet
    /// framing (packets are `block_align`-sized regardless).
    pcm_frame_bytes: u32,
    /// Whether a `dpds` chunk was seen while scanning. See the module doc:
    /// its mere presence, not its content, changes how the reference
    /// computes `duration_ts`.
    has_dpds: bool,
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
        let mut bits_per_sample: u16 = 0;
        let mut has_dpds = false;

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
                    bits_per_sample = io.rl16()?;
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
                b"dpds" => {
                    has_dpds = true;
                }
                _ => {}
            }
            let padded = size.saturating_add(size & 1);
            io.seek(chunk_start.saturating_add(padded))?;
            if id == *b"data" {
                // The data chunk can be large, so the scan stops here rather
                // than reading past it looking for a trailing `dpds` — every
                // real xWMA file orders chunks `fmt `/`dpds`/`data`, so a
                // `dpds` chunk after `data` (legal RIFF, not a shape any
                // known encoder produces) would not be detected. `dpds`'s
                // own content is never read either way — see the module doc.
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

        let mut stream = Stream::new(
            0,
            MediaType::Audio,
            Rational::new(1, sample_rate.cast_signed()),
        );
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
        #[allow(
            clippy::integer_division,
            reason = "bytes_per_sample from a bit depth measured to always be byte-aligned"
        )]
        let bytes_per_sample = u32::from(bits_per_sample) / 8;
        let pcm_frame_bytes = bytes_per_sample.saturating_mul(u32::from(channels));
        if has_dpds && pcm_frame_bytes > 0 {
            // Measured, not guessed: a `dpds` chunk's mere presence makes
            // the reference compute `duration_ts` as if `data` were already
            // decoded PCM at the `fmt` chunk's own channels/bits-per-sample
            // — confirmed exactly across mono/stereo and 8/16-bit fixtures
            // (`data_len / (channels * bytes_per_sample)`, verified against
            // both `wmav1`/`wmav2`). See the module doc.
            #[allow(
                clippy::integer_division,
                reason = "matches the measured reference formula exactly"
            )]
            let frames = clamped_len / u64::from(pcm_frame_bytes);
            stream.duration_ts = i64::try_from(frames).ok();
        } else if avg_bytes_per_sec > 0 {
            #[allow(
                clippy::integer_division,
                reason = "sample-count estimate from a byte count; matches the measured reference formula"
            )]
            let frames =
                clamped_len.saturating_mul(u64::from(sample_rate)) / u64::from(avg_bytes_per_sec);
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
            pcm_frame_bytes,
            has_dpds,
            bytes_read: 0,
            eof: false,
            budget: Budget::new(vaco_limits::Limits::permissive()),
        })
    }

    #[allow(
        clippy::integer_division,
        reason = "packet-duration estimate from a byte count; matches the measured reference formula"
    )]
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
        #[allow(
            clippy::integer_division,
            reason = "block-align a byte offset down to a packet boundary"
        )]
        let block = byte_offset / u64::from(self.block_align);
        let aligned = block
            .saturating_mul(u64::from(self.block_align))
            .min(self.data_len);
        self.io.seek(self.data_start.saturating_add(aligned))?;
        self.bytes_read = aligned;
        self.eof = false;
        Ok(())
    }

    #[allow(
        clippy::integer_division,
        reason = "sample-count estimate from a byte count; matches the measured reference formula"
    )]
    #[allow(
        clippy::integer_division,
        reason = "matches the measured reference formula exactly, both branches"
    )]
    fn duration(&self) -> Option<Duration> {
        let frames = if self.has_dpds && self.pcm_frame_bytes > 0 {
            self.data_len / u64::from(self.pcm_frame_bytes)
        } else {
            self.data_len.saturating_mul(u64::from(self.sample_rate))
                / u64::from(self.avg_bytes_per_sec)
        };
        let micros = frames
            .checked_mul(1_000_000)?
            .checked_div(u64::from(self.sample_rate))?;
        Some(Duration::from_micros(
            i64::try_from(micros).unwrap_or(i64::MAX),
        ))
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

    fn build_file_without_dpds(
        format_tag: u16,
        channels: u16,
        sample_rate: u32,
        avg_bytes_per_sec: u32,
        block_align: u32,
        bits_per_sample: u16,
        data: &[u8],
    ) -> Vec<u8> {
        let fmt_payload = {
            let mut v = Vec::new();
            v.extend_from_slice(&format_tag.to_le_bytes());
            v.extend_from_slice(&channels.to_le_bytes());
            v.extend_from_slice(&sample_rate.to_le_bytes());
            v.extend_from_slice(&avg_bytes_per_sec.to_le_bytes());
            v.extend_from_slice(&(block_align as u16).to_le_bytes());
            v.extend_from_slice(&bits_per_sample.to_le_bytes());
            v.extend_from_slice(&0u16.to_le_bytes());
            v
        };
        let mut body = b"XWMA".to_vec();
        body.extend(chunk(*b"fmt ", &fmt_payload));
        body.extend(chunk(*b"data", data));
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&(body.len() as u32).to_le_bytes());
        v.extend(body);
        v
    }

    #[test]
    fn duration_ts_uses_the_byte_rate_formula_with_no_dpds_chunk() {
        let data = vec![0xABu8; 350];
        let file = build_file_without_dpds(0x0161, 1, 8000, 1000, 100, 16, &data);
        let d = XwmaDemuxer::open(Box::new(MemorySource::new(file))).unwrap();
        // 350 * 8000 / 1000 = 2800, the byte-rate formula, not the
        // dpds-present PCM-frame-size formula.
        assert_eq!(d.streams().first().unwrap().duration_ts, Some(2800));
    }

    #[test]
    fn duration_ts_uses_the_pcm_frame_size_formula_when_dpds_is_present() {
        // build_file always includes a dpds chunk and fixes bits_per_sample
        // at 16, so the expected divisor is channels * 2.
        let data = vec![0xABu8; 2000];
        let mono = XwmaDemuxer::open(Box::new(MemorySource::new(build_file(
            0x0161, 1, 8000, 1000, 100, &data,
        ))))
        .unwrap();
        assert_eq!(mono.streams().first().unwrap().duration_ts, Some(1000)); // 2000 / (1*2)

        let stereo = XwmaDemuxer::open(Box::new(MemorySource::new(build_file(
            0x0161, 2, 44_100, 8000, 2230, &data,
        ))))
        .unwrap();
        assert_eq!(stereo.streams().first().unwrap().duration_ts, Some(500)); // 2000 / (2*2)
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
