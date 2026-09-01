//! Synthesising the header packets a caller's [`CodecParameters`] does not
//! carry by itself.
//!
//! [`vaco_codec_core::CodecParameters::extradata`] is one blob; Ogg's own
//! codecs need between one (Opus, FLAC) and three (Vorbis, Theora) header
//! packets. This crate follows the same convention its sibling demuxer
//! reads: `extradata` is exactly the *identification* packet's bytes
//! (`OpusHead`, or the FLAC `STREAMINFO` payload), and this module
//! synthesises the mandatory comment packet each format still needs to be
//! well-formed. **This makes Opus and FLAC round-trip through this crate's
//! own demuxer**, which the `roundtrip` integration test checks directly.
//!
//! Vorbis and Theora both additionally need a *setup* header carrying
//! encoder-chosen codebooks or quantisation tables that cannot be
//! synthesised generically — there is no crate in this workspace that
//! produces one. **Vorbis is closed**: `vaco-demux-ogg::codec` now defines
//! the convention for packing all three packets into one `extradata` blob
//! (measured against a real `ffmpeg -c:a vorbis` file), and
//! [`writer::OggMuxer::add_stream`]'s `CodecId::Vorbis` arm unpacks it with
//! that same module's inverse — one definition shared by both crates, not
//! two. **Theora is not**: a caller muxing Theora through this crate today
//! still gets only its identification packet written; see
//! `docs/format/vaco-mux-ogg.md` for that half of the gap.

use vaco_core::{Error, Result};

/// The comment vendor string every header this module writes carries.
pub const VENDOR: &[u8] = b"vaco";

/// Build a minimal, valid `OpusTags` packet (RFC 7845 §5.2): the vendor
/// string and zero user comments.
#[must_use]
pub fn opus_tags() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"OpusTags");
    out.extend_from_slice(&u32::try_from(VENDOR.len()).unwrap_or(0).to_le_bytes());
    out.extend_from_slice(VENDOR);
    out.extend_from_slice(&0u32.to_le_bytes()); // user_comment_list_length
    out
}

/// Recover the bare 34-byte `STREAMINFO` payload from a `CodecId::Flac`
/// stream's [`vaco_codec_core::CodecParameters::extradata`], accepting
/// either shape that field arrives in.
///
/// # Why two shapes
///
/// This module's own doc says `extradata` "is exactly the identification
/// packet's bytes... the FLAC `STREAMINFO` payload" — true of a stream this
/// crate's own demuxer produced (copy-through round trip), but not the only
/// shape a `CodecId::Flac` encoder hands a muxer. `vaco-codec-flac`'s own
/// [`Encoder::extradata`](vaco_codec_core::Encoder::extradata) — and every
/// container whose own convention for "FLAC's extradata" is a synthetic
/// single-block `.flac` file header, which is most of them (Matroska's
/// `CodecPrivate`, the native `.flac` muxer, MP4's `dfLa` box) — answers
/// with `"fLaC" + the STREAMINFO metadata block, header included` (42
/// bytes: 4-byte magic, 1-byte last-block-flag/type, 3-byte length, 34-byte
/// payload). Requiring a caller to pre-strip that down to the bare 34 was
/// this crate's own inconsistency, not a difference in what the *codec*
/// needs: measured directly, `vaco -i in.wav -c:a flac out.ogg` refused
/// every such stream with "FLAC STREAMINFO extradata must be exactly 34
/// bytes" even though the exact same encoder's output muxed into Matroska
/// (or `.flac`) correctly. `CodecParameters::extradata` is one field (D19);
/// this is the seam that already has to read both of its established
/// shapes, and no other consumer in this workspace needs to change to keep
/// producing the one most of them share.
///
/// # Errors
/// [`Error::InvalidData`] if `extradata` is neither exactly the bare 34
/// bytes nor a `"fLaC"`-prefixed metadata block whose first block is a
/// 34-byte `STREAMINFO` (type 0).
pub fn streaminfo_payload_from_extradata(extradata: &[u8]) -> Result<[u8; 34]> {
    let bad = || Error::InvalidData("FLAC STREAMINFO extradata must be exactly 34 bytes");
    let payload = if extradata.len() == 34 {
        extradata
    } else {
        let body = extradata.strip_prefix(b"fLaC").ok_or_else(bad)?;
        let header = body.get(..4).ok_or_else(bad)?;
        let [b0, b1, b2, b3] = header else {
            return Err(bad());
        };
        // Metadata block header: bit 7 is the last-block flag (either value
        // is fine here — the comment block this module writes next may or
        // may not already have been the last one in the source file), bits
        // 0-6 are the block type; STREAMINFO is type 0.
        if b0 & 0x7F != 0 {
            return Err(Error::InvalidData(
                "FLAC extradata's first metadata block is not STREAMINFO",
            ));
        }
        let len = (u32::from(*b1) << 16) | (u32::from(*b2) << 8) | u32::from(*b3);
        if len != 34 {
            return Err(bad());
        }
        body.get(4..38).ok_or_else(bad)?
    };
    payload.try_into().map_err(|_| bad())
}

/// Build the special first FLAC-in-Ogg packet from a raw 34-byte
/// `STREAMINFO` payload: the `\x7FFLAC` wrapper, one more header packet
/// declared (the mandatory comment block this module also writes), the
/// native `fLaC` marker, and the metadata-block-header-wrapped `STREAMINFO`
/// itself (not the last block, since the comment block follows).
///
/// # Errors
/// [`Error::InvalidData`] if `streaminfo` is not exactly 34 bytes.
pub fn flac_first_packet(streaminfo: &[u8]) -> Result<Vec<u8>> {
    if streaminfo.len() != 34 {
        return Err(Error::InvalidData(
            "FLAC STREAMINFO extradata must be exactly 34 bytes",
        ));
    }
    let mut out = Vec::new();
    out.push(0x7F);
    out.extend_from_slice(b"FLAC");
    out.push(1); // major version
    out.push(0); // minor version
    out.extend_from_slice(&1u16.to_be_bytes()); // one more header packet follows
    out.extend_from_slice(b"fLaC");
    out.push(0x00); // metadata block header: last=0, type=0 (STREAMINFO)
    out.extend_from_slice(&[0x00, 0x00, 0x22]); // 24-bit big-endian length, 34
    out.extend_from_slice(streaminfo);
    Ok(out)
}

/// Build the mandatory `METADATA_BLOCK_VORBIS_COMMENT` FLAC-in-Ogg requires
/// as its second header packet: the native metadata-block wrapper (this
/// *is* the last block), around the vendor string and zero user comments.
#[must_use]
pub fn flac_comment_block() -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&u32::try_from(VENDOR.len()).unwrap_or(0).to_le_bytes());
    payload.extend_from_slice(VENDOR);
    payload.extend_from_slice(&0u32.to_le_bytes());
    let mut out = Vec::new();
    out.push(0x84); // last=1, type=4 (VORBIS_COMMENT)
    let len = u32::try_from(payload.len()).unwrap_or(0).to_be_bytes();
    out.extend_from_slice(&len[1..]); // low 24 bits, big-endian
    out.extend_from_slice(&payload);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn opus_tags_is_well_formed() {
        let tags = opus_tags();
        assert!(tags.starts_with(b"OpusTags"));
        let vendor_len = u32::from_le_bytes(tags[8..12].try_into().unwrap()) as usize;
        assert_eq!(vendor_len, VENDOR.len());
        assert_eq!(&tags[12..12 + vendor_len], VENDOR);
        let comments = u32::from_le_bytes(
            tags[12 + vendor_len..12 + vendor_len + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(comments, 0);
    }

    #[test]
    fn flac_first_packet_round_trips_through_the_demuxer_side_parser() {
        let streaminfo = [0u8; 34];
        let packet = flac_first_packet(&streaminfo).unwrap();
        let info = vaco_demux_ogg::codec::parse_flac_streaminfo(&packet).unwrap();
        // All-zero STREAMINFO decodes to zero fields; the point is that the
        // wrapper this module writes is exactly what the sibling demuxer's
        // parser expects, not that zero is meaningful.
        assert_eq!(info.sample_rate, 0);
        let count = vaco_demux_ogg::codec::parse_flac_header_count(&packet).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn flac_first_packet_rejects_the_wrong_streaminfo_length() {
        assert!(flac_first_packet(&[0u8; 33]).is_err());
        assert!(flac_first_packet(&[0u8; 35]).is_err());
    }

    #[test]
    fn flac_comment_block_declares_the_vorbis_comment_type_and_last_flag() {
        let block = flac_comment_block();
        assert_eq!(block[0], 0x84);
        let len = u32::from_be_bytes([0, block[1], block[2], block[3]]) as usize;
        assert_eq!(block.len(), 4 + len);
    }

    /// The bare shape (this crate's own demuxer's convention) passes through
    /// unchanged.
    #[test]
    fn streaminfo_payload_accepts_the_bare_34_bytes() {
        let mut bare = [0u8; 34];
        bare[4] = 0xAB;
        let got = streaminfo_payload_from_extradata(&bare).unwrap();
        assert_eq!(got, bare);
    }

    /// The shape `vaco-codec-flac::FlacEncoder::extradata` (and most
    /// containers' own "FLAC extradata" convention) actually produces:
    /// `"fLaC"` + a last-block-flagged STREAMINFO metadata block. This is
    /// the exact regression this function exists to close — measured via a
    /// real `vaco -c:a flac out.ogg` refusing this shape outright before
    /// the fix.
    #[test]
    fn streaminfo_payload_unwraps_the_flac_encoders_own_shape() {
        let mut payload = [0u8; 34];
        payload[10] = 0xCD;
        let mut extradata = Vec::new();
        extradata.extend_from_slice(b"fLaC");
        extradata.push(0x80); // last-block flag set, type 0 (STREAMINFO)
        extradata.extend_from_slice(&[0x00, 0x00, 0x22]); // 24-bit length, 34
        extradata.extend_from_slice(&payload);
        let got = streaminfo_payload_from_extradata(&extradata).unwrap();
        assert_eq!(got, payload);
    }

    #[test]
    fn streaminfo_payload_rejects_garbage() {
        assert!(streaminfo_payload_from_extradata(&[0u8; 10]).is_err());
        assert!(streaminfo_payload_from_extradata(b"not fLaC at all, and not 34 bytes").is_err());
    }
}
