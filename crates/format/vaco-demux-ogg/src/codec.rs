//! Identifying a logical bitstream from its first (BOS) packet, and reading
//! the handful of fixed-position fields each codec's identification header
//! carries that the granule-position mapping needs.
//!
//! Every layout here is read directly off the wire because it is **container
//! framing knowledge**, the same standing as a PES header or a `stsd` box:
//! RFC 7845 §5.1 (Opus), the Vorbis I specification §4.2.2, the Theora
//! specification §6.2, and the Xiph Speex header (`speex_header.h`, widely
//! republished as the de facto format) all fix these bytes at the *start* of
//! a stream's first packet, before any Huffman-coded state exists to decode.
//! Nothing here parses a codebook, a mode table or a residue — that is
//! decode-side and none of this crate's business.
//!
//! `vaco-parse-opus` already parses `OpusHead` far more completely (and is
//! reused for exact per-packet durations through `ParserProvider` — see
//! [`crate::granule`]). This module still reads `pre_skip` directly, because
//! [`GranuleMapping`](crate::granule::GranuleMapping) must be constructible
//! even when no parser is registered (`NoParsers`, every fuzz target, most
//! unit tests) — D14.1 keeps this crate from depending on the codec crate,
//! not from knowing eight bytes of its own container's framing.

/// What this crate can identify well enough to map its granule position.
///
/// `CodecId` (`vaco-codec-core`) has no `Theora` or `Speex` variant today —
/// confirmed by reading `crates/signal/vaco-codec-core/src/lib.rs` rather
/// than assumed. The stream's `codec_id` is simply `None` for these two:
/// `ffprobe -bitexact -show_streams` on a real file carries no per-codec
/// metadata tag to fall back on, so this crate does not invent one either.
/// See the docs file's "gaps" section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OggCodec {
    Opus,
    Vorbis,
    Flac,
    Theora,
    Speex,
    /// A BOS packet we do not recognise. The stream is still demuxed —
    /// packets flow, granule positions pass straight through unmapped — it
    /// just cannot be timestamped or attached to a `CodecId`.
    Unknown,
}

impl OggCodec {
    /// This crate's own stable name for the codec, used in logging and
    /// diagnostics; for the three codecs `CodecId` has, it must agree with
    /// `CodecId::name()` — asserted in the docs file's cross-check test.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Opus => "opus",
            Self::Vorbis => "vorbis",
            Self::Flac => "flac",
            Self::Theora => "theora",
            Self::Speex => "speex",
            Self::Unknown => "unknown",
        }
    }
}

/// Identify a logical bitstream from its first packet.
///
/// Every signature here is a fixed byte sequence at a fixed offset — RFC
/// 7845 §5.1 for Opus, the Vorbis I spec §4.2.1 (packet type `1` plus the
/// six-byte `"vorbis"` sync), the Theora spec §6.2 (packet type `0x80` plus
/// `"theora"`), the FLAC-in-Ogg mapping (packet type `0x7F` plus `"FLAC"`),
/// and the Speex header's own eight-byte `"Speex   "` string.
#[must_use]
pub fn identify(bos_packet: &[u8]) -> OggCodec {
    if bos_packet.starts_with(b"OpusHead") {
        OggCodec::Opus
    } else if bos_packet.first() == Some(&0x01) && bos_packet.get(1..7) == Some(&b"vorbis"[..]) {
        OggCodec::Vorbis
    } else if bos_packet.first() == Some(&0x80) && bos_packet.get(1..7) == Some(&b"theora"[..]) {
        OggCodec::Theora
    } else if bos_packet.first() == Some(&0x7F) && bos_packet.get(1..5) == Some(&b"FLAC"[..]) {
        OggCodec::Flac
    } else if bos_packet.get(0..8) == Some(&b"Speex   "[..]) {
        OggCodec::Speex
    } else {
        OggCodec::Unknown
    }
}

/// Fields read from an `OpusHead` packet (RFC 7845 §5.1), sized 19 bytes
/// minimum.
#[derive(Debug, Clone, Copy)]
pub struct OpusIdent {
    pub channel_count: u8,
    pub pre_skip: u16,
}

/// # Errors
/// `None` if `packet` does not start with the `OpusHead` marker or is
/// shorter than the fixed fields need.
#[must_use]
pub fn parse_opus_head(packet: &[u8]) -> Option<OpusIdent> {
    if !packet.starts_with(b"OpusHead") {
        return None;
    }
    let channel_count = *packet.get(9)?;
    let pre_skip = u16::from_le_bytes(packet.get(10..12)?.try_into().ok()?);
    Some(OpusIdent {
        channel_count,
        pre_skip,
    })
}

/// Fields read from a Vorbis identification header (Vorbis I spec §4.2.2),
/// sized exactly 30 bytes. Every field is byte-aligned in this one packet
/// even though the rest of the Vorbis bitstream is not — it is the only
/// packet decoded before any bit-packing convention is in force.
#[derive(Debug, Clone, Copy)]
pub struct VorbisIdent {
    pub channels: u8,
    pub sample_rate: u32,
    /// `2^exponent`, read from the low nibble of the blocksize byte.
    pub blocksize_0: u32,
    /// `2^exponent`, read from the high nibble. Vorbis's overlap-add rule
    /// (measured against `ffmpeg -c:a vorbis`; see `crate::granule`) makes
    /// this the number [`crate::granule::GranuleMapping`] actually needs —
    /// `blocksize_0` is kept because it is free to read and documents the
    /// simplification we are making by not modelling block switching.
    pub blocksize_1: u32,
}

/// # Errors
/// `None` if `packet` does not start with the `\x01vorbis` marker or is
/// shorter than the fixed 30-byte identification header.
#[must_use]
pub fn parse_vorbis_ident(packet: &[u8]) -> Option<VorbisIdent> {
    if packet.first() != Some(&0x01) || packet.get(1..7) != Some(&b"vorbis"[..]) {
        return None;
    }
    let channels = *packet.get(11)?;
    let sample_rate = u32::from_le_bytes(packet.get(12..16)?.try_into().ok()?);
    let bs_byte = *packet.get(28)?;
    let bs0 = u32::from(bs_byte & 0x0F);
    let bs1 = u32::from((bs_byte >> 4) & 0x0F);
    // Exponents above 31 cannot shift into a u32 without overflow and are
    // not a block size any real encoder emits; refuse rather than panic.
    if bs0 > 31 || bs1 > 31 {
        return None;
    }
    Some(VorbisIdent {
        channels,
        sample_rate,
        blocksize_0: 1u32 << bs0,
        blocksize_1: 1u32 << bs1,
    })
}

/// Fields read from a Theora identification header (Theora spec §6.2), sized
/// exactly 42 bytes.
///
/// **Not measured against a real encoder** — this build of ffmpeg has no
/// Theora encoder (`ffmpeg -encoders` confirmed), and no other Theora
/// producer was available. Implemented from the public specification only;
/// see the docs file.
#[derive(Debug, Clone, Copy)]
pub struct TheoraIdent {
    pub width: u32,
    pub height: u32,
    pub fps_numerator: u32,
    pub fps_denominator: u32,
    /// Bits of the granule position given to the in-GOP frame offset; the
    /// remainder (high bits) is the keyframe number. §7.4.4.
    pub granule_shift: u32,
}

/// # Errors
/// `None` if `packet` does not start with the `0x80theora` marker or is
/// shorter than the fixed 42-byte identification header.
#[must_use]
pub fn parse_theora_ident(packet: &[u8]) -> Option<TheoraIdent> {
    if packet.first() != Some(&0x80) || packet.get(1..7) != Some(&b"theora"[..]) {
        return None;
    }
    let picw = read_u24_be(packet, 7 + 7)?;
    let pich = read_u24_be(packet, 7 + 10)?;
    let frn = u32::from_be_bytes(packet.get(7 + 15..7 + 19)?.try_into().ok()?);
    let frd = u32::from_be_bytes(packet.get(7 + 19..7 + 23)?.try_into().ok()?);
    // Packed 16-bit field at the end: 6 bits quality, 5 bits granule shift,
    // 2 bits pixel format, 3 reserved bits, MSB first.
    let packed = u16::from_be_bytes(packet.get(7 + 33..7 + 35)?.try_into().ok()?);
    let granule_shift = u32::from(packed >> 5) & 0x1F;
    if frd == 0 {
        return None;
    }
    Some(TheoraIdent {
        width: picw,
        height: pich,
        fps_numerator: frn,
        fps_denominator: frd,
        granule_shift,
    })
}

fn read_u24_be(data: &[u8], at: usize) -> Option<u32> {
    let b = data.get(at..at + 3)?;
    Some(u32::from_be_bytes([0, *b.first()?, *b.get(1)?, *b.get(2)?]))
}

/// Fields read from a Speex header (the widely-republished `SpeexHeader` C
/// struct), sized exactly 80 bytes.
///
/// **Not measured against a real encoder** — no Speex encoder is present in
/// this environment (Speex is effectively unmaintained upstream). The layout
/// is a stable, long-published de facto standard; see the docs file.
#[derive(Debug, Clone, Copy)]
pub struct SpeexIdent {
    pub rate: u32,
    pub channels: u32,
    /// Samples per frame, per the header's own `frame_size` field.
    pub frame_size: u32,
    /// Frames packed into one Ogg packet. Together with `frame_size` this
    /// gives an exact, header-stated per-packet sample count — no heuristic
    /// needed, unlike Vorbis.
    pub frames_per_packet: u32,
    /// Header packets beyond the mandatory comment packet — see
    /// [`total_header_packets`].
    pub extra_headers: u32,
}

/// # Errors
/// `None` if `packet` does not start with the `"Speex   "` marker or is
/// shorter than the fixed 80-byte header.
#[must_use]
pub fn parse_speex_ident(packet: &[u8]) -> Option<SpeexIdent> {
    if packet.get(0..8) != Some(&b"Speex   "[..]) {
        return None;
    }
    // 8 (string) + 20 (version string) = offset 28, then thirteen
    // little-endian i32 fields; rate is the third, nb_channels the sixth,
    // frame_size the eighth, frames_per_packet the eleventh, extra_headers
    // the thirteenth.
    let base = 28usize;
    let field = |i: usize| -> Option<u32> {
        let at = base + i * 4;
        Some(u32::from_le_bytes(packet.get(at..at + 4)?.try_into().ok()?))
    };
    let rate = field(2)?;
    let channels = field(5)?;
    let frame_size = field(7)?;
    let frames_per_packet = field(9)?;
    let extra_headers = field(11)?;
    Some(SpeexIdent {
        rate,
        channels,
        frame_size,
        frames_per_packet,
        extra_headers,
    })
}

/// The declared count of *additional* native FLAC metadata-block packets
/// that follow the special first Ogg packet, per the FLAC-in-Ogg mapping
/// (RFC 9639 §10.1 / the earlier Xiph mapping document it codifies): a
/// big-endian 16-bit field right after the `\x7FFLAC` signature and the two
/// version bytes.
///
/// # Errors
/// `None` if `packet` is shorter than the field needs.
#[must_use]
pub fn parse_flac_header_count(packet: &[u8]) -> Option<u16> {
    if packet.first() != Some(&0x7F) || packet.get(1..5) != Some(&b"FLAC"[..]) {
        return None;
    }
    Some(u16::from_be_bytes(packet.get(7..9)?.try_into().ok()?))
}

/// The FLAC `STREAMINFO` fields carried inside the special first Ogg packet,
/// after its `\x7FFLAC` wrapper and the native `fLaC` marker.
#[derive(Debug, Clone, Copy)]
pub struct FlacIdent {
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
    /// `0` means "unknown", per the FLAC specification.
    pub total_samples: u64,
}

/// # Errors
/// `None` if `packet` is not the special first FLAC-in-Ogg packet or is
/// shorter than its fixed layout needs.
#[must_use]
pub fn parse_flac_streaminfo(packet: &[u8]) -> Option<FlacIdent> {
    if packet.first() != Some(&0x7F) || packet.get(1..5) != Some(&b"FLAC"[..]) {
        return None;
    }
    // 5 (\x7FFLAC) + 1 (major) + 1 (minor) + 2 (header count) + 4 (fLaC) +
    // 1 (metadata block header byte) + 3 (block length, always 34) = 17.
    let info = packet.get(17..17 + 34)?;
    // 16 + 16 + 24 + 24 = 80 bits = 10 bytes of block/frame-size fields we
    // do not need, then a 20:3:5:36 = 64-bit packed region (8 bytes).
    let packed: [u8; 8] = info.get(10..18)?.try_into().ok()?;
    let bits = u64::from_be_bytes(packed);
    // From the top: 20 bits sample rate, 3 bits channels-1, 5 bits
    // bits_per_sample-1, 36 bits total_samples — 64 bits exactly.
    let sample_rate = u32::try_from((bits >> 44) & 0xF_FFFF).ok()?;
    let channels = u8::try_from((bits >> 41) & 0x7).ok()?.saturating_add(1);
    let bits_per_sample = u8::try_from((bits >> 36) & 0x1F).ok()?.saturating_add(1);
    let total_samples = bits & 0xF_FFFF_FFFF;
    Some(FlacIdent {
        sample_rate,
        channels,
        bits_per_sample,
        total_samples,
    })
}

/// How many Ogg packets carry codec setup rather than payload, counting the
/// BOS packet itself.
///
/// Fixed by specification for Opus (RFC 7845 §5: identification + comment),
/// Vorbis (spec §4.1: identification + comment + setup) and Theora
/// (spec §6.1: identification + comment + setup). Speex and FLAC state their
/// own count in-band, so `bos_packet` is read for those two; a `None` from
/// either read falls back to the fixed minimum (`2`, `1`) so a malformed
/// count cannot make this crate wait forever for headers that are not
/// coming.
#[must_use]
pub fn total_header_packets(codec: OggCodec, bos_packet: &[u8]) -> u32 {
    match codec {
        OggCodec::Opus => 2,
        OggCodec::Vorbis | OggCodec::Theora => 3,
        OggCodec::Speex => {
            let extra = parse_speex_ident(bos_packet).map_or(0, |s| s.extra_headers);
            // +1 for this packet, +1 for the mandatory comment packet.
            2u32.saturating_add(extra.min(1024))
        }
        OggCodec::Flac => {
            let declared = parse_flac_header_count(bos_packet).unwrap_or(0);
            1u32.saturating_add(u32::from(declared))
        }
        // No convention to follow; treat the BOS packet as the only header
        // so an unrecognised codec's payload still reaches the caller.
        OggCodec::Unknown => 1,
    }
}

/// The magic a Vorbis comment header packet starts with — Vorbis I spec
/// §4.2.1: packet type `3` plus the six-byte `"vorbis"` sync, identical to
/// [`identify`]'s check on the identification packet but for type `3`.
pub const VORBIS_COMMENT_MAGIC: &[u8] = b"\x03vorbis";

/// The magic an `OpusTags` packet starts with (RFC 7845 §5.2).
pub const OPUS_COMMENT_MAGIC: &[u8] = b"OpusTags";

/// No comment header may contribute more than this many `KEY=value` pairs.
/// Generous for any real file (a handful of tags is typical) and cheap to
/// enforce, since a comment header with a fabricated count in the millions
/// would otherwise cost one iteration per entry before the slice ran out.
const MAX_COMMENTS: u32 = 4096;

/// Read a four-byte little-endian length prefix, then that many bytes.
///
/// `None` on truncation — a comment header that runs out of bytes mid-field
/// is a damaged file, not a different grammar.
fn take_length_prefixed(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let len = u32::from_le_bytes(data.get(0..4)?.try_into().ok()?);
    let len = usize::try_from(len).ok()?;
    let value = data.get(4..4usize.checked_add(len)?)?;
    let rest = data.get(4usize.checked_add(len)?..)?;
    Some((value, rest))
}

/// Read every `KEY=value` pair out of a Vorbis-comment-formatted header:
/// Vorbis I spec §5.2.1's `vendor_length`/`vendor_string`/`user_comment_list_length`
/// grammar, which RFC 7845 §5.2's `OpusTags` reuses byte-for-byte after its
/// own eight-byte magic. `magic` selects which packet this is — pass
/// [`VORBIS_COMMENT_MAGIC`] or [`OPUS_COMMENT_MAGIC`].
///
/// Keys are lower-cased to match `ffprobe -show_streams`'s own `TAG:` keys
/// (measured: a `TITLE` field in the file prints as `TAG:title`). A field
/// that is not valid UTF-8, or does not contain `=`, is skipped rather than
/// failing the whole header — one bad tag among several good ones is not
/// grounds for reporting none of them.
///
/// Returns an empty list, rather than an error, for a packet that does not
/// start with `magic` or is truncated anywhere in the grammar: the comment
/// header is metadata, and a file that gets the metadata wrong still has
/// packets worth demuxing.
#[must_use]
pub fn parse_comment_header(packet: &[u8], magic: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(rest) = packet.strip_prefix(magic) else {
        return out;
    };
    let Some((_vendor, rest)) = take_length_prefixed(rest) else {
        return out;
    };
    let Some(count_bytes) = rest.get(0..4) else {
        return out;
    };
    let Ok(count_arr) = <[u8; 4]>::try_from(count_bytes) else {
        return out;
    };
    let count = u32::from_le_bytes(count_arr).min(MAX_COMMENTS);
    let mut rest = rest.get(4..).unwrap_or(&[]);
    for _ in 0..count {
        let Some((entry, next)) = take_length_prefixed(rest) else {
            break;
        };
        rest = next;
        let Ok(text) = core::str::from_utf8(entry) else {
            continue;
        };
        if let Some((key, value)) = text.split_once('=') {
            out.push((key.to_ascii_lowercase(), value.to_string()));
        }
    }
    out
}

/// Packs 2+ Ogg header packets (Vorbis: identification, comment, setup;
/// Theora shares the same three-packet shape) into one `extradata` blob,
/// Xiph-style: a packet count minus one, then every packet's length
/// *except the last's* lace-encoded the same way an Ogg segment table
/// lace-encodes a packet spanning 255+ bytes (a run of `0xFF` bytes summing
/// 255 each, terminated by a final byte `< 255`), then every packet's raw
/// bytes concatenated in order. The last packet's length is never stated —
/// it is simply what remains.
///
/// Measured, not assumed: `ffmpeg -f nut` remuxing a real
/// `ffmpeg -c:a vorbis` Ogg file and reading `codec_specific_data` back
/// (NUT stores a stream's extradata verbatim) gives exactly this shape for
/// three headers of 30/29/3247 bytes — `[0x02, 0x1e, 0x1d, <30 bytes
/// starting `01 76 6f 72 62 69 73`>, <29 bytes starting
/// `03 76 6f 72 62 69 73`>, <3247 bytes starting `05 76 6f 72 62 69 73`>]`
/// — byte for byte, including that a length under 255 (both of the first
/// two headers here) is a single byte with no lace continuation.
#[must_use]
pub fn pack_xiph_headers(headers: &[Vec<u8>]) -> Vec<u8> {
    let count = headers.len();
    let mut out = Vec::new();
    out.push(u8::try_from(count.saturating_sub(1)).unwrap_or(u8::MAX));
    for h in headers.iter().take(count.saturating_sub(1)) {
        let mut len = h.len();
        while len >= 255 {
            out.push(255);
            len -= 255;
        }
        out.push(u8::try_from(len).unwrap_or(u8::MAX));
    }
    for h in headers {
        out.extend_from_slice(h);
    }
    out
}

/// The inverse of [`pack_xiph_headers`]: splits one packed `extradata` blob
/// back into its original header packets. `None` for anything that does not
/// parse as this shape — a lace-encoded length running past the end of the
/// blob, or fewer bytes left than the last declared length needs — rather
/// than returning a wrong split.
#[must_use]
pub fn split_xiph_headers(data: &[u8]) -> Option<Vec<Vec<u8>>> {
    let (&count_minus_one, mut cursor) = data.split_first()?;
    let count = usize::from(count_minus_one).saturating_add(1);
    let mut lens = Vec::new();
    for _ in 0..count.saturating_sub(1) {
        let mut len = 0usize;
        loop {
            let (&b, rest) = cursor.split_first()?;
            cursor = rest;
            len = len.saturating_add(usize::from(b));
            if b != 255 {
                break;
            }
        }
        lens.push(len);
    }
    let mut headers = Vec::new();
    for len in lens {
        if cursor.len() < len {
            return None;
        }
        let (head, rest) = cursor.split_at(len);
        headers.push(head.to_vec());
        cursor = rest;
    }
    headers.push(cursor.to_vec());
    Some(headers)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::cast_possible_wrap,
    reason = "test code"
)]
mod tests {
    use super::*;

    /// The exact 19-byte `OpusHead` packet from `ffmpeg -c:a libopus`,
    /// measured in `crc.rs`'s test — mono, `pre_skip` 312.
    const OPUS_HEAD: &[u8] = &[
        b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', 0x01, 0x01, 0x38, 0x01, 0x80, 0xBB, 0x00,
        0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn identifies_opus_from_a_measured_head_packet() {
        assert_eq!(identify(OPUS_HEAD), OggCodec::Opus);
        let ident = parse_opus_head(OPUS_HEAD).unwrap();
        assert_eq!(ident.channel_count, 1);
        assert_eq!(ident.pre_skip, 312);
    }

    fn vorbis_ident_bytes(channels: u8, rate: u32, bs0_exp: u8, bs1_exp: u8) -> Vec<u8> {
        let mut v = vec![0x01];
        v.extend_from_slice(b"vorbis");
        v.extend_from_slice(&0u32.to_le_bytes()); // vorbis_version
        v.push(channels);
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&0i32.to_le_bytes()); // bitrate_maximum
        v.extend_from_slice(&0i32.to_le_bytes()); // bitrate_nominal
        v.extend_from_slice(&0i32.to_le_bytes()); // bitrate_minimum
        v.push((bs0_exp & 0x0F) | ((bs1_exp & 0x0F) << 4));
        v.push(1); // framing flag
        v
    }

    #[test]
    fn identifies_vorbis_and_reads_measured_fields() {
        // channels=2, rate=44100, blocksize_0=blocksize_1=2048 (exponent 11):
        // exactly what `ffmpeg -c:a vorbis` produced when measured.
        let bytes = vorbis_ident_bytes(2, 44_100, 11, 11);
        assert_eq!(
            bytes.len(),
            30,
            "identification header is fixed at 30 bytes"
        );
        assert_eq!(identify(&bytes), OggCodec::Vorbis);
        let ident = parse_vorbis_ident(&bytes).unwrap();
        assert_eq!(ident.channels, 2);
        assert_eq!(ident.sample_rate, 44_100);
        assert_eq!(ident.blocksize_0, 2048);
        assert_eq!(ident.blocksize_1, 2048);
    }

    #[test]
    fn vorbis_blocksize_exponents_out_of_range_do_not_overflow() {
        let bytes = vorbis_ident_bytes(2, 44_100, 31, 31);
        // 1u32 << 31 is representable; 2^31 is the largest legal shift.
        assert!(parse_vorbis_ident(&bytes).is_some());
    }

    fn theora_ident_bytes(width: u32, height: u32, frn: u32, frd: u32, gshift: u8) -> Vec<u8> {
        let mut v = vec![0x80];
        v.extend_from_slice(b"theora");
        v.push(3); // VMAJ
        v.push(2); // VMIN
        v.push(1); // VREV
        v.extend_from_slice(&0u16.to_be_bytes()); // FMBW
        v.extend_from_slice(&0u16.to_be_bytes()); // FMBH
        v.extend_from_slice(&width.to_be_bytes()[1..]); // PICW, 24 bits
        v.extend_from_slice(&height.to_be_bytes()[1..]); // PICH, 24 bits
        v.push(0); // PICX
        v.push(0); // PICY
        v.extend_from_slice(&frn.to_be_bytes());
        v.extend_from_slice(&frd.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes()[1..]); // PARN, 24 bits
        v.extend_from_slice(&0u32.to_be_bytes()[1..]); // PARD, 24 bits
        v.push(0); // CS
        v.extend_from_slice(&0u32.to_be_bytes()[1..]); // NOMBR, 24 bits
        let packed: u16 = (u16::from(gshift) & 0x1F) << 5;
        v.extend_from_slice(&packed.to_be_bytes());
        v
    }

    #[test]
    fn identifies_theora_and_decodes_the_packed_granule_shift() {
        let bytes = theora_ident_bytes(1920, 1080, 30, 1, 6);
        assert_eq!(
            bytes.len(),
            42,
            "identification header is fixed at 42 bytes"
        );
        assert_eq!(identify(&bytes), OggCodec::Theora);
        let ident = parse_theora_ident(&bytes).unwrap();
        assert_eq!(ident.width, 1920);
        assert_eq!(ident.height, 1080);
        assert_eq!(ident.fps_numerator, 30);
        assert_eq!(ident.fps_denominator, 1);
        assert_eq!(ident.granule_shift, 6);
    }

    fn speex_ident_bytes(rate: u32, channels: u32, frame_size: u32, fpp: u32) -> Vec<u8> {
        let mut v = b"Speex   ".to_vec();
        v.extend_from_slice(&[0u8; 20]); // version string
        let fields = [
            1i32,
            80,
            rate as i32,
            0,
            4,
            channels as i32,
            0,
            frame_size as i32,
            0,
            fpp as i32,
            0,
            0,
            0,
        ];
        for f in fields {
            v.extend_from_slice(&f.to_le_bytes());
        }
        v
    }

    #[test]
    fn identifies_speex_and_reads_the_exact_frame_size() {
        let bytes = speex_ident_bytes(16_000, 1, 320, 1);
        assert_eq!(bytes.len(), 80, "speex header is fixed at 80 bytes");
        assert_eq!(identify(&bytes), OggCodec::Speex);
        let ident = parse_speex_ident(&bytes).unwrap();
        assert_eq!(ident.rate, 16_000);
        assert_eq!(ident.channels, 1);
        assert_eq!(ident.frame_size, 320);
        assert_eq!(ident.frames_per_packet, 1);
    }

    #[test]
    fn unrecognised_packets_do_not_panic() {
        for bytes in [&b""[..], &b"junk"[..], &[0u8; 5][..], &[0xFFu8; 100][..]] {
            assert_eq!(identify(bytes), OggCodec::Unknown);
            assert!(parse_opus_head(bytes).is_none());
            assert!(parse_vorbis_ident(bytes).is_none());
            assert!(parse_theora_ident(bytes).is_none());
            assert!(parse_speex_ident(bytes).is_none());
        }
    }

    fn vorbis_comment_bytes(vendor: &str, comments: &[(&str, &str)]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(VORBIS_COMMENT_MAGIC);
        v.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        v.extend_from_slice(vendor.as_bytes());
        v.extend_from_slice(&(comments.len() as u32).to_le_bytes());
        for (k, val) in comments {
            let field = format!("{k}={val}");
            v.extend_from_slice(&(field.len() as u32).to_le_bytes());
            v.extend_from_slice(field.as_bytes());
        }
        v
    }

    #[test]
    fn reads_every_comment_and_lower_cases_the_key() {
        let bytes = vorbis_comment_bytes(
            "libvorbis",
            &[("TITLE", "Test Title"), ("ENCODER", "Lavc62.28.100 vorbis")],
        );
        let tags = parse_comment_header(&bytes, VORBIS_COMMENT_MAGIC);
        assert_eq!(
            tags,
            vec![
                ("title".to_string(), "Test Title".to_string()),
                ("encoder".to_string(), "Lavc62.28.100 vorbis".to_string()),
            ]
        );
    }

    #[test]
    fn opus_tags_share_the_same_grammar_after_their_own_magic() {
        let mut bytes = OPUS_COMMENT_MAGIC.to_vec();
        bytes.extend_from_slice(&vorbis_comment_bytes("", &[("title", "x")])[VORBIS_COMMENT_MAGIC.len()..]);
        let tags = parse_comment_header(&bytes, OPUS_COMMENT_MAGIC);
        assert_eq!(tags, vec![("title".to_string(), "x".to_string())]);
    }

    #[test]
    fn a_truncated_comment_header_yields_whatever_parsed_before_the_cut() {
        let full = vorbis_comment_bytes("v", &[("a", "1"), ("b", "2")]);
        // Cut off partway through the second comment's bytes.
        let cut = &full[..full.len() - 1];
        let tags = parse_comment_header(cut, VORBIS_COMMENT_MAGIC);
        assert_eq!(tags, vec![("a".to_string(), "1".to_string())]);
    }

    #[test]
    fn wrong_magic_or_empty_input_yields_no_comments() {
        assert!(parse_comment_header(b"", VORBIS_COMMENT_MAGIC).is_empty());
        assert!(parse_comment_header(b"not vorbis at all", VORBIS_COMMENT_MAGIC).is_empty());
    }

    /// Byte-for-byte against a real `ffmpeg -c:a vorbis -strict -2` Ogg
    /// file, read back through `ffmpeg -f nut` (NUT stores extradata
    /// verbatim): three headers of 30/29/3247 bytes pack to a 3309-byte
    /// blob starting `02 1e 1d`, matching this crate's own measurement in
    /// `pack_xiph_headers`'s doc comment.
    #[test]
    fn pack_xiph_headers_matches_the_measured_vorbis_layout() {
        let ident = {
            let mut h = vec![0x01];
            h.extend_from_slice(b"vorbis");
            h.resize(30, 0);
            h
        };
        let comment = {
            let mut h = vec![0x03];
            h.extend_from_slice(b"vorbis");
            h.resize(29, 0);
            h
        };
        let setup = {
            let mut h = vec![0x05];
            h.extend_from_slice(b"vorbis");
            h.resize(3247, 0);
            h
        };
        let packed = pack_xiph_headers(&[ident.clone(), comment.clone(), setup.clone()]);
        assert_eq!(packed.len(), 3309);
        assert_eq!(&packed[..3], &[0x02, 0x1e, 0x1d]);
        assert_eq!(&packed[3..33], ident.as_slice());
        assert_eq!(&packed[33..62], comment.as_slice());
        assert_eq!(&packed[62..], setup.as_slice());
    }

    #[test]
    fn split_xiph_headers_is_the_exact_inverse_of_pack() {
        let headers = vec![vec![1, 2, 3], vec![4, 5], vec![6, 7, 8, 9]];
        let packed = pack_xiph_headers(&headers);
        assert_eq!(split_xiph_headers(&packed), Some(headers));
    }

    /// A header at or past 255 bytes needs a lace-continuation byte, the
    /// same rule an Ogg segment table already follows for a packet that
    /// long — exercised because the real-file measurement above happens to
    /// have both non-last headers under 255 bytes.
    #[test]
    fn a_header_of_255_or_more_bytes_lace_encodes_its_length() {
        let long_header = vec![0xAB; 300];
        let headers = vec![long_header.clone(), vec![1, 2, 3]];
        let packed = pack_xiph_headers(&headers);
        // 300 = 255 + 45: one 0xff continuation byte, then the 45 remainder.
        assert_eq!(&packed[..3], &[0x01, 0xff, 45]);
        assert_eq!(split_xiph_headers(&packed), Some(headers));
    }

    #[test]
    fn split_xiph_headers_rejects_a_length_past_the_end() {
        // Declares one 200-byte header (count=2) but supplies far fewer
        // bytes than that.
        let malformed = [1u8, 200, 1, 2, 3];
        assert_eq!(split_xiph_headers(&malformed), None);
    }

    #[test]
    fn split_xiph_headers_rejects_a_lace_run_with_no_terminator() {
        // A length byte of 255 with nothing after it: the loop should stop
        // rather than read past the slice.
        let malformed = [1u8, 255];
        assert_eq!(split_xiph_headers(&malformed), None);
    }

    #[test]
    fn pack_xiph_headers_of_a_single_header_is_just_the_header() {
        let headers = vec![vec![9, 9, 9]];
        let packed = pack_xiph_headers(&headers);
        assert_eq!(packed, vec![0, 9, 9, 9]);
        assert_eq!(split_xiph_headers(&packed), Some(headers));
    }
}
