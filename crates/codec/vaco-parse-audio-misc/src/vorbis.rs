//! The Vorbis identification header, and the Xiph header-packing convention
//! containers use to carry it (plus the comment and setup headers) as one
//! `extradata` blob.
//!
//! Xiph Vorbis I specification §4.2.2 defines the identification header;
//! field layout verified against the real bytes `ffmpeg -c:a vorbis` writes,
//! not transcribed from the specification text alone.
//!
//! ```text
//!   packet_type='\x01'  'vorbis'
//!   vorbis_version   (u32, LE, must be 0)
//!   audio_channels   (u8)
//!   audio_sample_rate (u32, LE)
//!   bitrate_maximum  (i32, LE)
//!   bitrate_nominal  (i32, LE)
//!   bitrate_minimum  (i32, LE)
//!   blocksize        (u8: high nibble = blocksize_1 exponent, low = blocksize_0)
//!   framing_bit      (u8, low bit set)
//! ```

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{AudioParameters, CodecId, CodecParameters, Parser};
use vaco_core::{Error, Result};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

/// The eight bytes an identification header packet opens with.
pub const MAGIC: &[u8; 7] = b"\x01vorbis";

/// Bytes in a well-formed identification header, magic included.
pub const HEADER_LEN: usize = 30;

/// A parsed Vorbis identification header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentificationHeader {
    pub channels: u8,
    pub sample_rate: u32,
    pub bitrate_maximum: i32,
    pub bitrate_nominal: i32,
    pub bitrate_minimum: i32,
    /// `2^exponent` samples. Long window.
    pub blocksize_1_exponent: u8,
    /// `2^exponent` samples. Short window. `blocksize_0 <= blocksize_1`.
    pub blocksize_0_exponent: u8,
}

impl IdentificationHeader {
    /// Parse the 30-byte identification header, magic included.
    ///
    /// `Vaco-Spec-Ref: xiph-vorbis-i` §4.2.2; measured against a real
    /// `ffmpeg -c:a vorbis` encode's first Ogg packet byte for byte.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the magic is absent, the packet is
    /// shorter than [`HEADER_LEN`], the version field is nonzero, or the
    /// framing bit is clear.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let Some(rest) = data.strip_prefix(MAGIC.as_slice()) else {
            return Err(Error::InvalidData("missing Vorbis identification header magic"));
        };
        let Some(body) = rest.get(..HEADER_LEN - MAGIC.len()) else {
            return Err(Error::InvalidData(
                "Vorbis identification header is shorter than its own length",
            ));
        };
        let Some(&version) = body.first_chunk::<4>() else {
            return Err(Error::InvalidData("truncated Vorbis identification header"));
        };
        if u32::from_le_bytes(version) != 0 {
            return Err(Error::InvalidData("Vorbis identification header version is not 0"));
        }
        let Some(&channels) = body.get(4) else {
            return Err(Error::InvalidData("truncated Vorbis identification header"));
        };
        let Some(rate) = body.get(5..9).and_then(|s| <[u8; 4]>::try_from(s).ok()) else {
            return Err(Error::InvalidData("truncated Vorbis identification header"));
        };
        let sample_rate = u32::from_le_bytes(rate);
        let Some(bmax) = body.get(9..13).and_then(|s| <[u8; 4]>::try_from(s).ok()) else {
            return Err(Error::InvalidData("truncated Vorbis identification header"));
        };
        let Some(bnom) = body.get(13..17).and_then(|s| <[u8; 4]>::try_from(s).ok()) else {
            return Err(Error::InvalidData("truncated Vorbis identification header"));
        };
        let Some(bmin) = body.get(17..21).and_then(|s| <[u8; 4]>::try_from(s).ok()) else {
            return Err(Error::InvalidData("truncated Vorbis identification header"));
        };
        let Some(&blocksize) = body.get(21) else {
            return Err(Error::InvalidData("truncated Vorbis identification header"));
        };
        let Some(&framing) = body.get(22) else {
            return Err(Error::InvalidData("truncated Vorbis identification header"));
        };
        if framing & 1 == 0 {
            return Err(Error::InvalidData(
                "Vorbis identification header framing bit is not set",
            ));
        }
        if channels == 0 || sample_rate == 0 {
            return Err(Error::InvalidData(
                "Vorbis identification header states zero channels or sample rate",
            ));
        }
        Ok(Self {
            channels,
            sample_rate,
            bitrate_maximum: i32::from_le_bytes(bmax),
            bitrate_nominal: i32::from_le_bytes(bnom),
            bitrate_minimum: i32::from_le_bytes(bmin),
            blocksize_1_exponent: blocksize >> 4,
            blocksize_0_exponent: blocksize & 0x0f,
        })
    }

    /// Fold the header into the parameters a container reports.
    ///
    /// `sample_fmt` is `fltp`, measured against a real `-c:a vorbis` decode
    /// with `ffprobe 8.1` — the decoder's output format, the same convention
    /// `vaco-parse-aac`/`vaco-parse-opus` document for their own codecs.
    #[must_use]
    pub fn to_codec_parameters(&self) -> CodecParameters {
        let mut params = CodecParameters::audio().with_codec(CodecId::Vorbis);
        if self.bitrate_nominal > 0 {
            params.bit_rate = Some(self.bitrate_nominal.unsigned_abs().into());
        }
        params.audio = Some(AudioParameters {
            sample_rate: self.sample_rate,
            format: Some(vaco_sampfmt::SampleFmt::F32P),
            layout: Some(
                ChannelLayout::default_for(u32::from(self.channels))
                    .unwrap_or_else(|| ChannelLayout::unspecified(u32::from(self.channels))),
            ),
            bits_per_coded_sample: None,
            bits_per_raw_sample: None,
            initial_padding: 0,
        });
        params
    }
}

/// Read one Xiph-laced length: a run of `0xFF` bytes summing 255 each,
/// terminated by a byte `< 255`, exactly as an Ogg segment table lace-encodes
/// a packet spanning 255+ bytes.
fn take_laced_len(data: &[u8]) -> Option<(usize, &[u8])> {
    let mut len = 0usize;
    let mut rest = data;
    loop {
        let (&b, tail) = rest.split_first()?;
        rest = tail;
        len = len.checked_add(usize::from(b))?;
        if b != 0xff {
            return Some((len, rest));
        }
    }
}

/// Split a Xiph-packed `extradata` blob back into its header packets:
/// `[count - 1]`, then `count - 1` lace-encoded lengths, then every packet's
/// raw bytes concatenated — the last packet's length is never stated, it is
/// simply what remains.
///
/// Containers that are not Ogg itself (Matroska, MP4, NUT) carry a Vorbis
/// track's identification/comment/setup headers this way, since none of them
/// has Ogg's own page/segment framing to deliver three separate packets.
///
/// `Vaco-Spec-Ref: xiph-vorbis-i`; measured by remuxing a real
/// `ffmpeg -c:a vorbis` Ogg file to Matroska and reading `CodecPrivate` back
/// byte for byte: `[0x02, 0x1e, 0x1d, <30-byte id header>, <29-byte comment
/// header>, <setup header>]`.
///
/// # Errors
///
/// [`Error::InvalidData`] when a lace-encoded length runs past the end of
/// the blob, or fewer bytes remain than the declared lengths (plus the
/// unstated last packet) need.
pub fn unpack_headers(data: &[u8]) -> Result<Vec<&[u8]>> {
    let Some((&count_minus_one, mut rest)) = data.split_first() else {
        return Err(Error::InvalidData("empty Vorbis header extradata"));
    };
    let count = usize::from(count_minus_one).saturating_add(1);
    let mut lens = Vec::new();
    for _ in 0..count.saturating_sub(1) {
        let Some((len, tail)) = take_laced_len(rest) else {
            return Err(Error::InvalidData(
                "Vorbis header extradata's lace-encoded length runs past the blob",
            ));
        };
        lens.push(len);
        rest = tail;
    }
    let mut out = Vec::new();
    for len in lens {
        let Some((packet, tail)) = rest.split_at_checked(len) else {
            return Err(Error::InvalidData(
                "Vorbis header extradata is shorter than its declared lengths",
            ));
        };
        out.push(packet);
        rest = tail;
    }
    // The last packet's length is never stated: whatever remains, empty or
    // not — pushed unconditionally, so `out.len() == count` always holds
    // rather than depending on whether the remainder happens to be empty.
    out.push(rest);
    Ok(out)
}

/// Validates already-framed Vorbis packets and reports stream parameters.
///
/// Like `vaco-parse-opus::OpusParser`, **each `parse` call's input must be
/// exactly one already-framed packet** — an Ogg packet, a Matroska block, an
/// MP4 sample. Vorbis has no in-band way to find a packet boundary by
/// looking at the bytes.
///
/// The three setup packets (identification, comment, setup) are not
/// expected to reach [`VorbisParser::parse`] at all: a demuxer that carries
/// Vorbis natively (Ogg) recognises and consumes them itself, the way
/// `vaco-demux-ogg` already does for its own probing; a demuxer that carries
/// Vorbis as `extradata` (Matroska, MP4) hands the whole Xiph-packed blob to
/// [`VorbisParser::set_extradata`] once. Either way this parser only ever
/// sees audio packets.
#[derive(Debug)]
pub struct VorbisParser {
    ident: Option<IdentificationHeader>,
    params: Option<CodecParameters>,
    /// The comment header packet, verbatim, when [`Self::set_extradata`] saw
    /// one alongside the identification header. Kept whole rather than
    /// parsed eagerly: not every caller wants the tags, and
    /// [`Self::comment`] parses it on demand through
    /// `vaco-format-vorbiscomment`, which is the one place this exact
    /// vendor-plus-tag-list shape is read — see the crate docs on why that
    /// reader is not duplicated here.
    comment: Option<Vec<u8>>,
    budget: Budget,
    packets: u64,
}

impl VorbisParser {
    /// A parser with no identification header yet.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            ident: None,
            params: None,
            comment: None,
            budget: Budget::new(limits),
            packets: 0,
        }
    }

    /// The identification header, once one has been supplied.
    #[must_use]
    pub const fn identification_header(&self) -> Option<&IdentificationHeader> {
        self.ident.as_ref()
    }

    /// The comment header's tags, parsed on demand, when
    /// [`Self::set_extradata`] was given the full Xiph-packed blob rather
    /// than a bare identification header.
    #[must_use]
    pub fn comment(&self) -> Option<vaco_format_vorbiscomment::VorbisComment<'_>> {
        vaco_format_vorbiscomment::VorbisComment::parse_native(self.comment.as_deref()?).ok()
    }

    /// Packets validated so far.
    #[must_use]
    pub const fn packets(&self) -> u64 {
        self.packets
    }

    fn set_identification_header(&mut self, header: IdentificationHeader) {
        self.params = Some(header.to_codec_parameters());
        self.ident = Some(header);
    }
}

impl Parser for VorbisParser {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        if input.is_empty() {
            return Ok((None, 0));
        }
        let mut packet = Packet::from_slice(&mut self.budget, input)?;
        packet.flags = PacketFlags::KEY;
        self.packets = self.packets.saturating_add(1);
        Ok((Some(packet), input.len()))
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        self.params.as_ref()
    }

    /// Read a Xiph-packed header blob, or a bare identification header.
    ///
    /// Vorbis has **no in-band configuration at all** beyond what the
    /// identification header itself states, so like Opus this is the only
    /// path that describes the stream at all when the container is not Ogg.
    ///
    /// # Errors
    ///
    /// Whatever [`unpack_headers`]/[`IdentificationHeader::parse`] returns,
    /// unless the bare-header fallback below succeeds instead.
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if extradata.is_empty() {
            return Ok(());
        }
        if let Ok(header) = IdentificationHeader::parse(extradata) {
            self.set_identification_header(header);
            return Ok(());
        }
        let headers = unpack_headers(extradata)?;
        let Some(first) = headers.first() else {
            return Err(Error::InvalidData("Vorbis header extradata has no packets"));
        };
        let header = IdentificationHeader::parse(first)?;
        self.set_identification_header(header);
        if let Some(&comment) = headers.get(1) {
            self.comment = Some(comment.to_vec());
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;

    fn ident_bytes(channels: u8, rate: u32, bs0: u8, bs1: u8) -> Vec<u8> {
        let mut v = MAGIC.to_vec();
        v.extend_from_slice(&0u32.to_le_bytes());
        v.push(channels);
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&0i32.to_le_bytes());
        v.extend_from_slice(&128_000i32.to_le_bytes());
        v.extend_from_slice(&0i32.to_le_bytes());
        v.push((bs1 << 4) | (bs0 & 0x0f));
        v.push(1);
        v
    }

    #[test]
    fn parses_the_measured_shape() {
        let bytes = ident_bytes(2, 44_100, 8, 11);
        let header = IdentificationHeader::parse(&bytes).expect("valid header");
        assert_eq!(header.channels, 2);
        assert_eq!(header.sample_rate, 44_100);
        assert_eq!(header.bitrate_nominal, 128_000);
        assert_eq!(header.blocksize_0_exponent, 8);
        assert_eq!(header.blocksize_1_exponent, 11);
    }

    #[test]
    fn rejects_a_missing_magic_or_framing_bit() {
        assert!(IdentificationHeader::parse(&[0u8; 30]).is_err());
        let mut bad = ident_bytes(2, 44_100, 8, 11);
        let last = bad.len() - 1;
        if let Some(b) = bad.get_mut(last) {
            *b = 0;
        }
        assert!(IdentificationHeader::parse(&bad).is_err());
    }

    #[test]
    fn unpack_headers_round_trips_the_measured_shape() {
        let id = ident_bytes(2, 44_100, 8, 11);
        let comment = vec![3u8; 29];
        let setup = vec![5u8; 100];
        let mut blob = vec![2u8, id.len() as u8, comment.len() as u8];
        blob.extend_from_slice(&id);
        blob.extend_from_slice(&comment);
        blob.extend_from_slice(&setup);
        let headers = unpack_headers(&blob).expect("valid blob");
        assert_eq!(headers.len(), 3);
        assert_eq!(headers[0], id.as_slice());
        assert_eq!(headers[1], comment.as_slice());
        assert_eq!(headers[2], setup.as_slice());
    }

    #[test]
    fn unpack_headers_handles_a_lace_continuation() {
        let mut header_a = vec![0u8; 300];
        header_a[0] = 0xaa;
        let header_b = vec![0xbbu8; 10];
        let mut blob = vec![1u8]; // 2 packets, one length stated
        blob.push(255);
        blob.push(45); // 255 + 45 = 300
        blob.extend_from_slice(&header_a);
        blob.extend_from_slice(&header_b);
        let headers = unpack_headers(&blob).expect("valid blob");
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].len(), 300);
        assert_eq!(headers[1], header_b.as_slice());
    }

    #[test]
    fn never_panics_on_arbitrary_bytes() {
        for len in 0..64usize {
            let data = vec![0xffu8; len];
            let _ = IdentificationHeader::parse(&data);
            let _ = unpack_headers(&data);
        }
    }

    #[test]
    fn a_boxed_parser_via_extradata_describes_the_stream() {
        let mut parser = VorbisParser::new(Limits::strict());
        assert!(parser.parameters().is_none());
        let bytes = ident_bytes(2, 44_100, 8, 11);
        parser.set_extradata(&bytes).expect("valid header");
        let params = parser.parameters().expect("described");
        assert_eq!(params.codec_id, Some(CodecId::Vorbis));
        assert_eq!(
            params.audio.as_ref().map(|a| a.sample_rate),
            Some(44_100)
        );
    }

    #[test]
    fn set_extradata_exposes_the_comment_header_via_vaco_format_vorbiscomment() {
        let id = ident_bytes(2, 44_100, 8, 11);
        let mut comment = vaco_format_vorbiscomment::VORBIS_MAGIC.to_vec();
        comment.extend_from_slice(&4u32.to_le_bytes());
        comment.extend_from_slice(b"Vaco");
        comment.extend_from_slice(&1u32.to_le_bytes());
        let tag = b"title=T";
        comment.extend_from_slice(&(tag.len() as u32).to_le_bytes());
        comment.extend_from_slice(tag);
        comment.push(1); // framing bit
        let setup = vec![5u8; 10];
        let mut blob = vec![2u8, id.len() as u8, comment.len() as u8];
        blob.extend_from_slice(&id);
        blob.extend_from_slice(&comment);
        blob.extend_from_slice(&setup);

        let mut parser = VorbisParser::new(Limits::strict());
        parser.set_extradata(&blob).expect("valid blob");
        let comment = parser.comment().expect("comment header was captured");
        assert_eq!(comment.get("title"), Some("T"));
    }

    #[test]
    fn parse_passes_a_packet_through_unexamined() {
        let mut parser = VorbisParser::new(Limits::strict());
        let (packet, used) = parser.parse(&[1, 2, 3, 4]).expect("any bytes are one packet");
        assert!(packet.is_some());
        assert_eq!(used, 4);
        assert_eq!(parser.packets(), 1);
    }
}
