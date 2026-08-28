//! The `STREAMINFO` metadata block: the 34-byte payload every FLAC stream
//! carries, and the only part of a container's FLAC extradata this crate
//! actually needs.
//!
//! Different containers wrap the same 34 bytes differently — Ogg-FLAC's
//! first packet, MP4's `dfLa` box and Matroska's native-header
//! `CodecPrivate` each put a different envelope around it (see
//! `set_extradata`'s doc on [`crate::decoder::FlacDecoder`]) — so rather
//! than parse each envelope, this module finds the block by its own
//! structure: a metadata block header whose type is 0 (`STREAMINFO`) and
//! whose 24-bit length is exactly 34.

use vaco_bitstream::{BitReader, BitWriter};

/// `STREAMINFO`'s own 34-byte payload, found inside a container's extradata.
///
/// Vaco-Spec-Ref: rfc-9639-flac Section 8.1, "Metadata Block Header"
///
/// Scans byte-by-byte rather than assuming the block starts at a fixed
/// offset, because the wrapper bytes preceding it differ per container
/// (an Ogg-FLAC marker packet, an MP4 `dfLa` box, or none at all). Returns
/// `None` rather than erroring when nothing matches — callers fail soft and
/// fall back to guessing from the first frame header instead, per
/// `guess_from_frame_header`.
#[must_use]
pub fn find_streaminfo_block(extradata: &[u8]) -> Option<[u8; 34]> {
    let len = extradata.len();
    let mut i = 0usize;
    while i.checked_add(4)? <= len {
        let header = *extradata.get(i)?;
        if header.trailing_zeros() >= 7 {
            let b1 = u32::from(*extradata.get(i + 1)?);
            let b2 = u32::from(*extradata.get(i + 2)?);
            let b3 = *extradata.get(i + 3)?;
            let block_len = (b1 << 16) | (b2 << 8) | u32::from(b3);
            if block_len == 34
                && let Some(slice) = extradata.get(i + 4..i + 4 + 34)
            {
                let mut block = [0u8; 34];
                block.copy_from_slice(slice);
                return Some(block);
            }
        }
        i += 1;
    }
    None
}

/// The three `STREAMINFO` fields this crate's own decode path needs: enough
/// to build a [`SampleFmt`](vaco_sampfmt::SampleFmt) and a
/// [`ChannelLayout`](vaco_chlayout::ChannelLayout) for the decoded [`Frame`](vaco_frame::Frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamInfoFields {
    pub sample_rate: u32,
    pub channels: u32,
    pub bits_per_sample: u32,
}

impl StreamInfoFields {
    /// Read the three fields out of a raw 34-byte `STREAMINFO` payload.
    ///
    /// Vaco-Spec-Ref: rfc-9639-flac Section 8.2, "Streaminfo"
    #[must_use]
    pub fn parse(block: &[u8; 34]) -> Self {
        let mut r = BitReader::new(block);
        let _min_block_size = r.get(16);
        let _max_block_size = r.get(16);
        let _min_frame_size = r.get(24);
        let _max_frame_size = r.get(24);
        let sample_rate = r.get(20);
        let channels = r.get(3) + 1;
        let bits_per_sample = r.get(5) + 1;
        Self {
            sample_rate,
            channels,
            bits_per_sample,
        }
    }
}

/// Build a `STREAMINFO` block payload (34 bytes, header excluded) from the
/// fields this crate actually tracks.
///
/// `min_block_size`/`max_block_size` bound every frame but the last one
/// (RFC 9639 §8.2 explicitly allows the last frame to fall outside them,
/// because "the encoder has to write these fields before receiving any
/// input audio data"), so a fixed, conservative `(16, max_block_size)` is
/// always valid regardless of what the last, possibly short, frame turns
/// out to need — this crate never claims a tighter bound than that.
/// Frame-size bounds, total sample count and the MD5 signature are left at
/// their "unknown" sentinel of zero; nothing here reads them back.
#[must_use]
pub fn to_block_bytes(
    sample_rate: u32,
    channels: u32,
    bits_per_sample: u32,
    max_block_size: u16,
) -> [u8; 34] {
    let mut w = BitWriter::new();
    w.put(16, 16); // min_block_size: conservative floor, see doc above.
    w.put(16, u32::from(max_block_size.max(16)));
    w.put(24, 0); // min_frame_size: unknown.
    w.put(24, 0); // max_frame_size: unknown.
    w.put(20, sample_rate);
    w.put(3, channels.saturating_sub(1));
    w.put(5, bits_per_sample.saturating_sub(1));
    w.put_long(36, 0); // total_samples: unknown.
    w.put_long(64, 0); // md5sum, high 64 bits: unknown.
    w.put_long(64, 0); // md5sum, low 64 bits: unknown.
    let bytes = w.finish();
    let mut out = [0u8; 34];
    if let Some(src) = bytes.get(..34) {
        out.copy_from_slice(src);
    }
    out
}

/// Wrap a `STREAMINFO` block payload in its own 4-byte metadata block
/// header, marked as the last metadata block — what a synthetic
/// single-block `"fLaC"` file needs before its first frame.
#[must_use]
pub fn wrap_as_last_metadata_block(payload: &[u8; 34]) -> [u8; 38] {
    let mut out = [0u8; 38];
    out[0] = 0x80; // last-block flag set, type 0 (STREAMINFO).
    out[1] = 0;
    out[2] = 0;
    out[3] = 34;
    if let Some(dst) = out.get_mut(4..38) {
        dst.copy_from_slice(payload);
    }
    out
}

/// Best-effort recovery of sample rate, channel count and bit depth
/// straight from one frame's own header, for the (rare) case where no
/// container extradata ever arrived. Every FLAC frame header restates
/// these — see [`crate::decoder::FlacDecoder::set_extradata`]'s doc — so
/// when they are coded explicitly (not "get this from `STREAMINFO`")
/// this is exact, not a guess.
///
/// What is genuinely a guess: an *uncommon* sample rate, coded in bytes
/// that follow a variable-length frame/sample number this function does
/// not decode. That case, and the case where the header itself says "get
/// this from `STREAMINFO`" (meaning there is nothing to recover it from
/// at all), fall back to a generic 44.1 kHz/16-bit assumption rather than
/// failing outright — a wrong guess here only matters for a stream with
/// neither extradata nor an explicit rate, which is not a shape any real
/// container produces.
///
/// Vaco-Spec-Ref: rfc-9639-flac Section 9.1, "Frame Header"
#[must_use]
pub fn guess_from_frame_header(payload: &[u8]) -> Option<(u32, u32, u32)> {
    let mut r = BitReader::new(payload);
    let sync_reserved_blocking = r.get(16);
    if sync_reserved_blocking >> 2 != 0b11_1111_1111_1111 {
        return None;
    }
    let bs_sr = r.get(8);
    let sample_rate_code = bs_sr & 0xF;
    let chan_bps = r.get(8);
    let channel_code = (chan_bps >> 4) & 0xF;
    let bps_code = (chan_bps >> 1) & 0x7;

    let channels = match channel_code {
        0..=7 => channel_code + 1,
        8..=10 => 2,
        _ => return None,
    };
    let bits_per_sample = match bps_code {
        1 => 8,
        2 => 12,
        5 => 20,
        6 => 24,
        7 => 32,
        // 0 ("from streaminfo"), 4 (16, which is also this function's
        // guess), or the reserved value 3.
        _ => 16,
    };
    let sample_rate = match sample_rate_code {
        1 => 88_200,
        2 => 176_400,
        3 => 192_000,
        4 => 8_000,
        5 => 16_000,
        6 => 22_050,
        7 => 24_000,
        8 => 32_000,
        10 => 48_000,
        11 => 96_000,
        // 0 ("from streaminfo"), 9 (44.1 kHz, also this function's guess),
        // or one of the uncommon-rate escapes (12/13/14) whose value sits
        // past the coded number this function does not parse.
        _ => 44_100,
    };
    Some((sample_rate, channels, bits_per_sample))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test code")]
mod tests {
    use super::{
        StreamInfoFields, find_streaminfo_block, to_block_bytes, wrap_as_last_metadata_block,
    };

    #[test]
    fn round_trips_through_bytes() {
        let block = to_block_bytes(48_000, 2, 24, 4096);
        let fields = StreamInfoFields::parse(&block);
        assert_eq!(
            fields,
            StreamInfoFields {
                sample_rate: 48_000,
                channels: 2,
                bits_per_sample: 24,
            }
        );
    }

    #[test]
    fn finds_block_behind_an_arbitrary_prefix() {
        let block = to_block_bytes(44_100, 1, 16, 1024);
        let wrapped = wrap_as_last_metadata_block(&block);
        let mut extradata = vec![0x7F, b'F', b'L', b'A', b'C', 1, 0, 0, 2];
        extradata.extend_from_slice(b"fLaC");
        extradata.extend_from_slice(&wrapped);
        let found = find_streaminfo_block(&extradata).expect("streaminfo present");
        assert_eq!(found, block);
    }

    #[test]
    fn absent_block_reports_none() {
        assert_eq!(find_streaminfo_block(b"not flac at all"), None);
    }
}
