//! Raw `.flac`: `"fLaC"` followed by metadata blocks, then a back-to-back
//! sequence of self-delimiting FLAC frames.
//!
//! Specification: RFC 9639 §8 (stream structure).
//!
//! # The demuxer used not to exist here — `cargo xtask reachability-check`
//!
//! This module used to say there was no demuxer, on the theory that reading a
//! bare `.flac` file back out was `vaco-demux-raw`'s job. It never was: no
//! crate registered a `flac` demuxer at all, so a standalone `.flac` file
//! fell through to whichever *other* format's probe scored highest on its
//! bytes — measured as CD+Graphics (`vaco-format-misc::cdg`), whose
//! zero-structure probe scores any input with enough 24-byte-aligned chunks
//! whose low six command bits happen to read `0x09`. [`DEMUXER`] below closes
//! that gap: real frame parsing, not a hand-off to a crate that never had it.
//!
//! # Frame boundaries, since FLAC states none
//!
//! A FLAC frame carries no length field — decoders find the end of one by
//! decoding it. A demuxer that will not decode audio instead looks for the
//! **next** frame: [`parse_frame_header`] validates a candidate header's own
//! CRC-8 (RFC 9639 §9.1.1), which cuts a false positive on ordinary
//! compressed frame data from roughly 1-in-16384 (the 14-bit sync code alone)
//! to roughly 1-in-4-million. [`FlacDemuxer::read_one_frame`] walks forward a
//! byte at a time from the current frame's own header until the next valid
//! header turns up (or end of file), and everything in between is the
//! current frame's packet — exactly the boundary `vaco-codec-flac`'s own
//! decoder is documented to accept ("each `parse` call...may contain more
//! than one concatenated, complete FLAC frame"), so a coarser split here
//! would also have worked; per-frame is what lets duration/pts track the
//! reference's own per-packet numbers (measured below).
//!
//! # Measured against `ffmpeg 8.1`/`ffprobe`
//!
//! `ffmpeg -f lavfi -i sine=frequency=440:duration=1 -ar 44100 -ac 2 -c:a
//! flac out.flac`, then `ffprobe -show_streams`/`-show_packets`:
//!
//! - `Format flac probed with size=2048 and score=100` — [`probe`] returns
//!   the maximum score on the `"fLaC"` magic, not a partial one.
//! - `time_base=1/44100` — the stream's time base is `1/sample_rate`, taken
//!   straight from `STREAMINFO`.
//! - Every packet's `duration` equals that frame's own coded block size
//!   (`1024` throughout this fixture, the encoder's default), `pts`
//!   accumulates it exactly, and every packet carries `flags=K__` — FLAC
//!   frames are never inter-predicted, so every one is a keyframe.
//!
//! # Reaching `STREAMINFO` without depending on the parser crate
//!
//! D14.1 forbids a `vaco-format-*` crate depending on a `vaco-parse-*` crate
//! directly — reach a parser through `vaco-registry`'s `ParserProvider`,
//! which is what [`FlacDemuxer::open`] does: the registered `flac` parser
//! (`vaco-parse-audio-misc::PARSER_FLAC`) turns the raw 34-byte `STREAMINFO`
//! payload into `CodecParameters` without this crate ever naming that type.
//!
//! # Why the header is just `params.extradata`, verbatim
//!
//! `vaco-codec-flac::FlacEncoder::extradata()` already returns exactly
//! `"fLaC"` followed by the `STREAMINFO` metadata block, marked as the last
//! one — the complete raw-FLAC file header, byte for byte, because that is
//! also what a container's own `CodecPrivate`/`extradata` channel wants
//! (E2E-GAPS #2's `Encoder::extradata`/`Encoder::prime_audio` pair is what
//! makes it reach here before `write_header` needs it: `Muxer::add_stream`
//! runs once, before a single frame is encoded, and there is no later hook
//! this trait offers to patch the header in afterwards). So this muxer does
//! not reconstruct `STREAMINFO` from `CodecParameters`' scattered fields —
//! it writes the encoder's own header unmodified, which is also, by
//! construction, exactly what `vaco-mux-matroska`'s `CodecPrivate` for the
//! same encoder carries.
//!
//! Every packet after that is one already-framed FLAC frame
//! (`vaco-codec-flac::encoder`'s own sync code, header, subframes and
//! footer CRC) — this muxer never touches frame payloads, only concatenates
//! them.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, Muxer, MuxerDesc, ParserProvider, SeekFlags, SeekTarget,
    Stream,
};
use vaco_io::{IoContext, IoOptions, IoWriter, MediaSink, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

// ------------------------------------------------------------------ probing

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(MAGIC) {
        ProbeScore(100)
    } else {
        ProbeScore::NONE
    }
}

const MAGIC: &[u8; 4] = b"fLaC";

/// Metadata blocks scanned before giving up on ever finding `STREAMINFO` (or
/// a terminating last-block flag) — a defensive bound against a corrupt or
/// adversarial file whose blocks never set it, not a real limit any
/// well-formed file approaches (real files have one to a handful).
const MAX_METADATA_BLOCKS: u32 = 4096;

/// Longest single frame this demuxer will search for the boundary of before
/// refusing the file. Generous relative to any real encode (max block size
/// 65535 samples, 8 channels, 32 bits — nowhere near this) and small enough
/// that a corrupt stream fails fast rather than scanning to end of file one
/// byte at a time.
const MAX_FRAME_SEARCH: usize = 16 * 1024 * 1024;

/// How far ahead [`FlacDemuxer::read_one_frame`] looks to recognise the next
/// frame header — long enough for the longest possible header (sync,
/// block-size/sample-rate byte, channel/sample-size byte, a 7-byte extended
/// UTF-8 frame number, up to 4 bytes of extra block-size/sample-rate fields,
/// one CRC-8 byte), with margin.
const HEADER_PEEK: usize = 32;

// -------------------------------------------------------------- frame sync

/// One FLAC frame header, decoded just enough to know its own byte length
/// and coded block size — not its sample rate, channel layout or sample
/// size, which the demuxer does not need per frame (they come from
/// `STREAMINFO` once, via the registered parser).
///
/// RFC 9639 §9.1.1/§9.1.2. Returns `None` for anything that is not a
/// **CRC-8-valid** frame header — the sync code alone (14 bits) is not
/// enough to trust as a frame boundary in the middle of compressed audio
/// data; see the module doc for the measured false-positive rates.
fn parse_frame_header(buf: &[u8]) -> Option<(usize, u32)> {
    let &[b0, b1, b2, b3] = buf.first_chunk::<4>()?;
    // 14-bit sync `11111111111110`, reserved bit 0, blocking-strategy bit
    // either way.
    if b0 != 0xFF || (b1 >> 2) != 0x3E || (b1 & 0x02) != 0 {
        return None;
    }
    let block_code = b2 >> 4;
    let rate_code = b2 & 0x0F;
    let channel_assignment = b3 >> 4;
    let sample_size_code = (b3 >> 1) & 0x07;
    // Reject the header's own reserved combinations before spending a CRC-8
    // check on them — cheap, and it keeps those bit patterns from ever being
    // accepted as a frame boundary even in the unlikely event the CRC-8
    // happened to match too.
    if b3 & 0x01 != 0
        || block_code == 0
        || channel_assignment > 10
        || sample_size_code == 0b011
        || rate_code == 0x0F
    {
        return None;
    }

    let mut pos = 4usize;
    let lead = *buf.get(pos)?;
    let utf8_len = utf8_number_len(lead)?;
    let utf8_bytes = buf.get(pos..pos.checked_add(utf8_len)?)?;
    if utf8_bytes.iter().skip(1).any(|&b| b & 0xC0 != 0x80) {
        return None; // continuation bytes must read `10xxxxxx`
    }
    pos = pos.checked_add(utf8_len)?;

    let (block_extra, mut blocksize) = match block_code {
        0x1 => (0usize, 192u32),
        0x2..=0x5 => (0, 576u32 << (block_code - 2)),
        0x6 => (1, 0),
        0x7 => (2, 0),
        0x8..=0xF => (0, 256u32 << (block_code - 8)),
        _ => return None,
    };
    let rate_extra = match rate_code {
        0xC => 1,
        0xD | 0xE => 2,
        _ => 0,
    };
    let extra = buf.get(pos..pos.checked_add(block_extra)?.checked_add(rate_extra)?)?;
    if block_code == 0x6 {
        blocksize = u32::from(*extra.first()?) + 1;
    } else if block_code == 0x7 {
        let hi = u32::from(*extra.first()?);
        let lo = u32::from(*extra.get(1)?);
        blocksize = ((hi << 8) | lo) + 1;
    }
    pos = pos.checked_add(block_extra)?.checked_add(rate_extra)?;

    let crc = *buf.get(pos)?;
    if crc8(buf.get(..pos)?) != crc {
        return None;
    }
    let header_len = pos.checked_add(1)?;
    Some((header_len, blocksize))
}

/// Byte length of a FLAC frame/sample number field, from its lead byte —
/// standard extended UTF-8 (RFC 9639 §9.1.1 calls it "UTF-8 like"), with
/// libFLAC's own one-byte extension (`0xFE` -> 7 bytes, for the 36-bit
/// sample-number case) that plain UTF-8 does not have. Only the byte count
/// is wanted here, never the decoded value.
const fn utf8_number_len(lead: u8) -> Option<usize> {
    if lead & 0x80 == 0 {
        Some(1)
    } else if lead & 0xE0 == 0xC0 {
        Some(2)
    } else if lead & 0xF0 == 0xE0 {
        Some(3)
    } else if lead & 0xF8 == 0xF0 {
        Some(4)
    } else if lead & 0xFC == 0xF8 {
        Some(5)
    } else if lead & 0xFE == 0xFC {
        Some(6)
    } else if lead == 0xFE {
        Some(7)
    } else {
        None
    }
}

/// CRC-8, polynomial `0x07`, init `0`, no reflect, no final XOR — RFC 9639's
/// frame-header checksum.
fn crc8(data: &[u8]) -> u8 {
    let mut crc: u8 = 0;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

// ------------------------------------------------------------------ demuxer

#[derive(Debug)]
pub struct FlacDemuxer {
    io: IoContext,
    budget: Budget,
    stream: Stream,
    sample_rate: u32,
    sample_pos: u64,
    next: Option<Packet>,
}

impl FlacDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the file is not `"fLaC"`, has no
    /// `STREAMINFO` block, or has no valid frame after its metadata;
    /// [`Error::Unsupported`] if this build has no registered `flac` parser
    /// (the `parse-audio-misc` feature); otherwise whatever the transport or
    /// budget reports.
    pub fn open(src: Box<dyn MediaSource>, parsers: &dyn ParserProvider) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut magic = [0u8; 4];
        io.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(Error::InvalidData("flac: missing \"fLaC\" marker"));
        }

        let mut streaminfo: Option<[u8; 34]> = None;
        let mut blocks = 0u32;
        loop {
            blocks += 1;
            if blocks > MAX_METADATA_BLOCKS {
                return Err(Error::InvalidData(
                    "flac: too many metadata blocks without a last-block flag",
                ));
            }
            let mut head = [0u8; 4];
            io.read_exact(&mut head)?;
            let last = head[0] & 0x80 != 0;
            let block_type = head[0] & 0x7F;
            let len = (u32::from(head[1]) << 16) | (u32::from(head[2]) << 8) | u32::from(head[3]);
            if block_type == 0 {
                let mut body = [0u8; 34];
                let take = (len as usize).min(34);
                io.read_exact(
                    body.get_mut(..take)
                        .ok_or(Error::InvalidData("flac: bad STREAMINFO length"))?,
                )?;
                if len as usize > take {
                    io.skip(u64::from(len) - take as u64)?;
                }
                streaminfo = Some(body);
            } else {
                io.skip(u64::from(len))?;
            }
            if last {
                break;
            }
        }
        let streaminfo = streaminfo.ok_or(Error::InvalidData("flac: no STREAMINFO block"))?;

        let mut parser = parsers.parser_for(CodecId::Flac).ok_or(Error::Unsupported(
            "flac: no `flac` parser registered in this build (enable parse-audio-misc)",
        ))?;
        parser.set_extradata(&streaminfo)?;
        let mut params = parser.parameters().cloned().ok_or(Error::InvalidData(
            "flac: STREAMINFO did not describe a stream",
        ))?;
        params.extradata = Some(streaminfo.to_vec());
        let sample_rate = params
            .audio
            .as_ref()
            .map(|a| a.sample_rate)
            .filter(|&r| r > 0)
            .ok_or(Error::InvalidData(
                "flac: STREAMINFO states zero sample rate",
            ))?;

        let mut stream = Stream::new(
            0,
            MediaType::Audio,
            Rational::new(1, sample_rate.cast_signed()),
        );
        stream.params = params;

        let mut demuxer = Self {
            io,
            budget: Budget::new(Limits::permissive()),
            stream,
            sample_rate,
            sample_pos: 0,
            next: None,
        };
        demuxer.next = demuxer.read_one_frame()?;
        Ok(demuxer)
    }

    /// Read one frame's worth of bytes — from the header at the current
    /// position through the byte just before the *next* valid frame header,
    /// or through end of file for the last frame — and turn it into one
    /// packet. `Ok(None)` at a clean end of stream.
    fn read_one_frame(&mut self) -> Result<Option<Packet>> {
        let head = self.io.peek(HEADER_PEEK)?;
        if head.is_empty() {
            return Ok(None);
        }
        let Some((header_len, blocksize)) = parse_frame_header(head) else {
            return Err(Error::InvalidData(
                "flac: no valid frame header at the expected position",
            ));
        };

        let mut acc = self.budget.incremental::<u8>(MAX_FRAME_SEARCH);
        let mut chunk = self.budget.alloc::<u8>(header_len)?;
        self.io.read_exact(&mut chunk)?;
        acc.push_slice(&mut self.budget, &chunk)?;

        loop {
            let ahead = self.io.peek(HEADER_PEEK)?;
            if ahead.is_empty() {
                break; // end of file: this is the last frame
            }
            if parse_frame_header(ahead).is_some() {
                break; // the next frame starts here
            }
            let mut one = [0u8; 1];
            self.io.read_exact(&mut one)?;
            acc.push_slice(&mut self.budget, &one)?;
            if acc.len() >= MAX_FRAME_SEARCH {
                return Err(Error::InvalidData(
                    "flac: no next frame header found within the search window",
                ));
            }
        }

        let mut packet = Packet::from_slice(&mut self.budget, acc.as_slice())?;
        packet.stream_index = 0;
        packet.pts = Timestamp::new(self.sample_pos.min(i64::MAX as u64).cast_signed());
        packet.dts = packet.pts;
        let time_base = Rational::new(1, self.sample_rate.cast_signed());
        packet.duration = Timestamp::new(i64::from(blocksize))
            .to_duration(time_base)
            .unwrap_or(Duration::ZERO);
        packet.set_duration_ts(i64::from(blocksize));
        packet.flags |= PacketFlags::KEY;
        self.sample_pos = self.sample_pos.saturating_add(u64::from(blocksize));
        Ok(Some(packet))
    }
}

impl Demuxer for FlacDemuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    fn read_packet(&mut self) -> Result<Packet> {
        let out = self.next.take().ok_or(Error::Eof)?;
        self.next = self.read_one_frame()?;
        Ok(out)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        Err(Error::Unsupported(
            "flac: byte-accurate seek needs a frame index this demuxer does not keep",
        ))
    }

    fn duration(&self) -> Option<Duration> {
        None
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "flac",
    long_name: "raw FLAC",
    extensions: &["flac"],
    mime_types: &["audio/x-flac"],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(FlacDemuxer::open(src, parsers)?))
}

#[derive(Debug)]
pub struct FlacMuxer {
    out: IoWriter,
    sample_rate: Option<u32>,
    header_written: bool,
    added: bool,
    /// `params.extradata` from `add_stream`, taken by `write_header` — see
    /// the module doc for why this is the whole header, unmodified.
    pending_extradata: Option<Vec<u8>>,
}

impl FlacMuxer {
    /// # Errors
    /// Propagates transport failure from `sink`.
    pub fn new(sink: Box<dyn MediaSink>) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            sample_rate: None,
            header_written: false,
            added: false,
            pending_extradata: None,
        })
    }
}

impl Muxer for FlacMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.added {
            return Err(Error::Unsupported("flac: only one stream is supported"));
        }
        if params.codec_id != Some(CodecId::Flac) {
            return Err(Error::Unsupported(
                "flac: this container only carries the flac codec",
            ));
        }
        let audio = params
            .audio
            .as_ref()
            .ok_or(Error::InvalidData("flac: not an audio stream"))?;
        self.sample_rate = Some(audio.sample_rate.max(1));
        self.pending_extradata.clone_from(&params.extradata);
        self.added = true;
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        if !self.added {
            return Err(Error::InvalidData("flac: no stream added"));
        }
        // `Encoder::extradata`'s doc: `None` by default, `Some` only from an
        // encoder that overrides it (`FlacEncoder` does, once primed or
        // once it has seen a frame) — a copied stream from a source that
        // never carried one (or an encoder this build has not wired the
        // same way) has nothing valid to write here, and guessing a
        // `STREAMINFO` this crate did not itself measure is exactly the
        // kind of synthesis this whole batch's other fixes replaced with a
        // real answer or a refusal.
        let Some(extradata) = self.pending_extradata.take() else {
            return Err(Error::Unsupported(
                "flac: no STREAMINFO available for this stream (fLaC extradata missing)",
            ));
        };
        self.out.write(&extradata)?;
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("flac: packet written before the header"));
        }
        self.out.write(packet.payload())
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        if stream_index != 0 {
            return None;
        }
        self.sample_rate.map(|r| Rational::new(1, r.cast_signed()))
    }

    fn write_trailer(&mut self) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData(
                "flac: trailer written before the header",
            ));
        }
        self.out.flush()
    }
}

pub const MUXER: MuxerDesc = MuxerDesc {
    name: "flac",
    long_name: "raw FLAC",
    extensions: &["flac"],
    default_video: None,
    default_audio: Some(CodecId::Flac),
    open: open_muxer,
};

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(FlacMuxer::new(sink)?))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_codec_core::AudioParameters;
    use vaco_core::MediaType;
    use vaco_format_core::vacoraw::MemorySink;

    fn params_with_extradata(extradata: &[u8]) -> CodecParameters {
        let mut p = CodecParameters::new(MediaType::Audio).with_codec(CodecId::Flac);
        p.audio = Some(AudioParameters {
            sample_rate: 44_100,
            ..AudioParameters::default()
        });
        p.extradata = Some(extradata.to_vec());
        p
    }

    #[test]
    fn the_header_is_the_encoders_own_extradata_verbatim() {
        let sink = MemorySink::new();
        let buf = sink.shared();
        let mut m = FlacMuxer::new(Box::new(sink)).unwrap();
        let extradata = b"fLaC\x80\x00\x00\x22whatever-streaminfo-bytes-go-here".to_vec();
        m.add_stream(&params_with_extradata(&extradata)).unwrap();
        m.write_header().unwrap();
        m.write_trailer().unwrap();
        assert_eq!(buf.snapshot(), extradata);
    }

    #[test]
    fn packets_are_concatenated_verbatim_after_the_header() {
        let sink = MemorySink::new();
        let buf = sink.shared();
        let mut m = FlacMuxer::new(Box::new(sink)).unwrap();
        m.add_stream(&params_with_extradata(b"fLaC-header"))
            .unwrap();
        m.write_header().unwrap();
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
        let p1 = Packet::from_slice(&mut budget, &[0xFF, 0xF8, 1, 2, 3]).unwrap();
        let p2 = Packet::from_slice(&mut budget, &[0xFF, 0xF8, 4, 5]).unwrap();
        m.write_packet(&p1).unwrap();
        m.write_packet(&p2).unwrap();
        m.write_trailer().unwrap();
        let mut want = b"fLaC-header".to_vec();
        want.extend_from_slice(&[0xFF, 0xF8, 1, 2, 3]);
        want.extend_from_slice(&[0xFF, 0xF8, 4, 5]);
        assert_eq!(buf.snapshot(), want);
    }

    #[test]
    fn a_second_stream_is_refused() {
        let mut m = FlacMuxer::new(Box::new(MemorySink::new())).unwrap();
        m.add_stream(&params_with_extradata(b"fLaC-header"))
            .unwrap();
        assert!(
            m.add_stream(&params_with_extradata(b"fLaC-header"))
                .is_err()
        );
    }

    #[test]
    fn a_non_flac_codec_is_refused() {
        let mut m = FlacMuxer::new(Box::new(MemorySink::new())).unwrap();
        let mut p = params_with_extradata(b"irrelevant");
        p.codec_id = Some(CodecId::PcmS16le);
        assert!(m.add_stream(&p).is_err());
    }

    #[test]
    fn writing_the_header_without_extradata_is_refused_not_guessed() {
        let mut m = FlacMuxer::new(Box::new(MemorySink::new())).unwrap();
        let mut p = params_with_extradata(b"unused");
        p.extradata = None;
        m.add_stream(&p).unwrap();
        assert!(m.write_header().is_err());
    }
}
