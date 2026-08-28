//! The MPJPEG demuxer: MIME parts in, JPEG packets out.
//!
//! One part is: a boundary line (`--<tag>\r\n`), a small header block ending
//! in a blank line, `Content-length` bytes of JPEG, then a bare `\r\n`. This
//! reads exactly that shape and nothing more permissive — see the module
//! docs in `lib.rs` for what is and is not tolerated.

use vaco_codec_core::{CodecId, CodecParameters, VideoParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

/// This is a live-stream wire format: no index, no byte-seek target survives
/// a re-read (a `Content-length` boundary is not derivable without reading
/// from the start), and packets carry no real timestamps of their own.
pub const FLAGS: FormatFlags = FormatFlags::NOBINSEARCH
    .union(FormatFlags::NOGENSEARCH)
    .union(FormatFlags::NO_BYTE_SEEK);

/// Measured: `ffprobe` reports `time_base=1/25`, `r_frame_rate=25/1` for an
/// MPJPEG stream regardless of how it was produced — there is no per-file
/// signal for the real rate (a `Content-length`-delimited JPEG stream states
/// none), so the reference falls back to a fixed assumption. This is that
/// same fixed assumption, not a computed value.
const ASSUMED_FRAME_RATE: Rational = Rational { num: 25, den: 1 };

/// Longest boundary/header line this will scan for before giving up.
/// `IoContext::peek` already enforces `max_probe_bytes`; this is a second,
/// much tighter bound so a header that never terminates fails fast rather
/// than growing the peek window all the way to that cap.
const MAX_HEADER_LINE: usize = 4096;

/// The MPJPEG demuxer.
pub struct MpjpegDemuxer {
    io: IoContext,
    streams: Vec<Stream>,
    budget: Budget,
    strict_mime_boundary: bool,
    /// The boundary tag text (without the leading `--` or trailing CRLF)
    /// read from the very first part. Only compared against later parts when
    /// `strict_mime_boundary` is set.
    boundary_tag: Vec<u8>,
    frame_index: u64,
    eof: bool,
    pending: Option<Packet>,
}

impl std::fmt::Debug for MpjpegDemuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MpjpegDemuxer")
            .field("strict_mime_boundary", &self.strict_mime_boundary)
            .field("frame_index", &self.frame_index)
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

impl MpjpegDemuxer {
    /// Open an MPJPEG stream with the reference's default options
    /// (`strict_mime_boundary=false`).
    ///
    /// # Errors
    /// [`Error::InvalidData`] when the input does not open with a boundary
    /// line, or [`Error::Eof`] when it is empty.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        Self::open_with_limits(src, Limits::permissive())
    }

    /// Open with an explicit allocation ceiling.
    ///
    /// # Errors
    /// As [`MpjpegDemuxer::open`].
    pub fn open_with_limits(src: Box<dyn MediaSource>, limits: Limits) -> Result<Self> {
        let io = IoContext::new(src, &IoOptions::default().with_limits(limits.clone()))?;
        let mut demux = Self {
            io,
            streams: Vec::new(),
            budget: Budget::new(limits),
            strict_mime_boundary: false,
            boundary_tag: Vec::new(),
            frame_index: 0,
            eof: false,
            pending: None,
        };
        // Read the first part up front so `streams()` never answers with an
        // empty list before the caller has read anything.
        demux.read_part()?;
        Ok(demux)
    }

    /// Opt into requiring every boundary line's tag to match the first one
    /// seen. Mirrors `ffmpeg -strict_mime_boundary`; the reference default
    /// is `false`.
    #[must_use]
    pub const fn strict_mime_boundary(mut self, v: bool) -> Self {
        self.strict_mime_boundary = v;
        self
    }

    /// Read one line, including its terminator, without exceeding
    /// `MAX_HEADER_LINE`. Tolerates a stream that ends mid-line by returning
    /// whatever is left, so a truncated final part reads as EOF rather than
    /// corruption.
    fn read_line(&mut self) -> Result<Vec<u8>> {
        let mut window = 128usize;
        loop {
            let capped = window.min(MAX_HEADER_LINE);
            let buf = self.io.peek(capped)?;
            if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line = buf.get(..=pos).ok_or(Error::UnexpectedEof)?.to_vec();
                self.io.skip(line.len() as u64)?;
                return Ok(line);
            }
            if buf.len() < capped {
                if buf.is_empty() {
                    return Err(Error::Eof);
                }
                let line = buf.to_vec();
                self.io.skip(line.len() as u64)?;
                return Ok(line);
            }
            if capped >= MAX_HEADER_LINE {
                return Err(Error::InvalidData("mpjpeg: header line too long"));
            }
            window = window.saturating_mul(4);
        }
    }

    /// Read one MIME part: boundary line, headers, blank line, then the
    /// `Content-length` payload and its trailing `\r\n`.
    fn read_part(&mut self) -> Result<()> {
        let boundary = self.read_line()?;
        let trimmed = trim_line(&boundary);
        let Some(tag) = trimmed.strip_prefix(b"--") else {
            return Err(Error::InvalidData("mpjpeg: expected a MIME boundary line"));
        };
        if self.boundary_tag.is_empty() {
            self.boundary_tag = tag.to_vec();
        } else if self.strict_mime_boundary && tag != self.boundary_tag.as_slice() {
            return Err(Error::InvalidData(
                "mpjpeg: boundary tag changed mid-stream",
            ));
        }

        let mut content_length: Option<u64> = None;
        loop {
            let line = self.read_line()?;
            let trimmed = trim_line(&line);
            if trimmed.is_empty() {
                break;
            }
            if let Some(value) = split_header(trimmed, b"content-length") {
                let text = std::str::from_utf8(value)
                    .map_err(|_| Error::InvalidData("mpjpeg: Content-length is not UTF-8"))?;
                let n: u64 = text
                    .trim()
                    .parse()
                    .map_err(|_| Error::InvalidData("mpjpeg: Content-length is not a number"))?;
                content_length = Some(n);
            }
        }

        let Some(len) = content_length else {
            return Err(Error::InvalidData(
                "mpjpeg: part has no Content-length header (unsupported: no EOI-scan fallback)",
            ));
        };
        // `len` is attacker-controlled (it is a header value read straight
        // from the stream) — bounded through `Budget` before a byte is
        // allocated for it, exactly like every other declared length in
        // this workspace's demuxers.
        let len_usize = usize::try_from(len)
            .map_err(|_| Error::InvalidData("mpjpeg: Content-length overflows usize"))?;
        let mut pkt = Packet::alloc(&mut self.budget, len_usize)?;
        self.io.read_exact(pkt.payload_mut())?;
        // The bare CRLF (or LF) after the payload, before the next boundary.
        let _ = self.read_line()?;

        let (width, height) = sof_dimensions(pkt.payload());
        if self.streams.is_empty() {
            self.streams.push(new_stream(width, height));
        }

        pkt.stream_index = 0;
        pkt.pts = Timestamp::new(self.frame_index.cast_signed());
        pkt.dts = pkt.pts;
        // 1/25 s in microseconds; a literal, not `1_000_000 / 25`, to avoid
        // the integer-division lint over what is a fixed constant, not a
        // computed ratio.
        pkt.duration = Duration::from_micros(40_000);
        pkt.flags |= PacketFlags::KEY; // JPEG is all-intra
        self.frame_index += 1;
        self.pending = Some(pkt);
        Ok(())
    }
}

fn trim_line(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 {
        match line.get(end - 1) {
            Some(b'\r' | b'\n') => end -= 1,
            _ => break,
        }
    }
    line.get(..end).unwrap_or(&[])
}

/// Split `line` as `name: value` and return `value` when `name` matches
/// `field` case-insensitively, matching HTTP header-name comparison.
fn split_header<'a>(line: &'a [u8], field: &[u8]) -> Option<&'a [u8]> {
    let colon = line.iter().position(|&b| b == b':')?;
    let name = line.get(..colon)?;
    if name.len() != field.len() {
        return None;
    }
    if !name
        .iter()
        .zip(field)
        .all(|(a, b)| a.to_ascii_lowercase() == *b)
    {
        return None;
    }
    let value = line.get(colon.saturating_add(1)..)?;
    Some(value.strip_prefix(b" ").unwrap_or(value))
}

/// Read `width`/`height` out of the first JPEG SOF marker found. Bounded to
/// the packet's own bytes (already budget-checked at allocation), so this
/// never reads or allocates anything new — it is a scan, not a parse.
///
/// Returns `(0, 0)` when no SOF marker is found; `vaco-format-core`'s
/// `VideoParameters::default()` already treats `0` as "unknown" everywhere
/// else in this workspace.
fn sof_dimensions(data: &[u8]) -> (u32, u32) {
    let mut i = 0usize;
    while i.saturating_add(4) <= data.len() {
        let Some(&marker_hi) = data.get(i) else {
            break;
        };
        if marker_hi != 0xFF {
            i += 1;
            continue;
        }
        let Some(&marker) = data.get(i + 1) else {
            break;
        };
        // Skip fill bytes and markers with no length (SOI/EOI/RSTn/TEM).
        if marker == 0xFF {
            i += 1;
            continue;
        }
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        let Some(seg_len) = data
            .get(i + 2)
            .zip(data.get(i + 3))
            .map(|(&hi, &lo)| u16::from_be_bytes([hi, lo]) as usize)
        else {
            break;
        };
        let is_sof =
            (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC;
        if is_sof {
            // precision(1) height(2) width(2), starting after the 2-byte length.
            let base = i + 4;
            if let (Some(h), Some(w)) = (
                data.get(base + 1).zip(data.get(base + 2)),
                data.get(base + 3).zip(data.get(base + 4)),
            ) {
                let height = u32::from(u16::from_be_bytes([*h.0, *h.1]));
                let width = u32::from(u16::from_be_bytes([*w.0, *w.1]));
                return (width, height);
            }
            break;
        }
        if marker == 0xDA {
            // Start of scan: entropy-coded data follows and no more markers
            // are reliably at fixed offsets. Stop scanning.
            break;
        }
        i = i.saturating_add(2).saturating_add(seg_len);
    }
    (0, 0)
}

fn new_stream(width: u32, height: u32) -> Stream {
    let video = VideoParameters {
        width,
        height,
        coded_width: width,
        coded_height: height,
        frame_rate: ASSUMED_FRAME_RATE,
        ..VideoParameters::default()
    };
    let mut params = CodecParameters::new(MediaType::Video).with_codec(CodecId::Jpeg);
    params.video = Some(video);
    let time_base = Rational {
        num: ASSUMED_FRAME_RATE.den,
        den: ASSUMED_FRAME_RATE.num,
    };
    let mut stream = Stream::new(0, MediaType::Video, time_base);
    stream.params = params;
    stream.r_frame_rate = ASSUMED_FRAME_RATE;
    stream
}

impl Demuxer for MpjpegDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if let Some(pkt) = self.pending.take() {
            return Ok(pkt);
        }
        if self.eof {
            return Err(Error::Eof);
        }
        match self.read_part() {
            Ok(()) => self.pending.take().ok_or(Error::Eof),
            Err(Error::Eof) => {
                self.eof = true;
                Err(Error::Eof)
            }
            Err(e) => Err(e),
        }
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        Err(Error::NotSeekable)
    }

    fn duration(&self) -> Option<Duration> {
        None
    }
}
