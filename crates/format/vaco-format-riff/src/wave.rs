//! `WAVEFORMATEX` and `WAVEFORMATEXTENSIBLE` — the structure carried in a
//! WAVE `fmt ` chunk, and, verbatim, in a Matroska `A_MS/ACM` track's
//! `CodecPrivate`.
//!
//! Windows SDK `mmreg.h` / `ksmedia.h`. `WAVEFORMATEX` is
//!
//! ```text
//! wFormatTag:u16       nChannels:u16        nSamplesPerSec:u32
//! nAvgBytesPerSec:u32  nBlockAlign:u16      wBitsPerSample:u16
//! [ cbSize:u16  extra[cbSize] ]   // present iff the chunk is >= 18 bytes
//! ```
//!
//! A 16-byte chunk is the plain structure with no `cbSize` at all (some
//! writers omit it rather than writing zero); an 18-byte chunk has `cbSize`
//! present and equal to zero; anything longer carries `cbSize` bytes of
//! codec-specific data after it — the MS-ADPCM coefficient table, the IMA
//! ADPCM samples-per-block count, or, when `wFormatTag` is
//! [`WAVE_FORMAT_EXTENSIBLE`], the 22-byte `WAVEFORMATEXTENSIBLE` tail.
//!
//! Probed directly against `ffmpeg` 8.1 (`ffmpeg -f lavfi -i sine=... -c:a
//! pcm_s24le out.wav`, then `xxd`): the `pcm_s24le` and `pcm_s32le` encoders
//! write `WAVEFORMATEXTENSIBLE` even for a single mono channel, not plain
//! `WAVEFORMATEX` with `wFormatTag = 1` — only `pcm_u8`, `pcm_s16le` and the
//! `WAVE_FORMAT_IEEE_FLOAT` encoders use the plain 16/18-byte form. A parser
//! that assumes "tag 1 always means plain `WAVEFORMATEX`" misreads exactly
//! the two most common lossless-beyond-16-bit encodings.

use vaco_bitstream::ByteReader;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

/// `WAVE_FORMAT_PCM`.
pub const WAVE_FORMAT_PCM: u16 = 0x0001;
/// `WAVE_FORMAT_ADPCM` (Microsoft ADPCM).
pub const WAVE_FORMAT_ADPCM: u16 = 0x0002;
/// `WAVE_FORMAT_IEEE_FLOAT`.
pub const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
/// `WAVE_FORMAT_ALAW`.
pub const WAVE_FORMAT_ALAW: u16 = 0x0006;
/// `WAVE_FORMAT_MULAW`.
pub const WAVE_FORMAT_MULAW: u16 = 0x0007;
/// `WAVE_FORMAT_DVI_ADPCM`, a.k.a. `WAVE_FORMAT_IMA_ADPCM` — RFC 2361 lists
/// both names for the same value.
pub const WAVE_FORMAT_DVI_ADPCM: u16 = 0x0011;
/// `WAVE_FORMAT_MPEG` (MPEG-1 Layer I/II).
pub const WAVE_FORMAT_MPEG: u16 = 0x0050;
/// `WAVE_FORMAT_MPEGLAYER3`.
pub const WAVE_FORMAT_MPEGLAYER3: u16 = 0x0055;
/// `WAVE_FORMAT_WMAUDIO1` (WMA version 1).
pub const WAVE_FORMAT_WMAUDIO1: u16 = 0x0160;
/// `WAVE_FORMAT_WMAUDIO2` (WMA version 2, a.k.a. WMA "9").
pub const WAVE_FORMAT_WMAUDIO2: u16 = 0x0161;
/// `WAVE_FORMAT_DOLBY_AC3_SPDIF`.
pub const WAVE_FORMAT_DOLBY_AC3_SPDIF: u16 = 0x2000;
/// `WAVE_FORMAT_AAC` — not RFC 2361, but the value every current encoder
/// (`ffmpeg` included) uses for AAC-in-WAV.
pub const WAVE_FORMAT_AAC: u16 = 0x00FF;
/// `WAVE_FORMAT_EXTENSIBLE` — the format tag is not the codec; look at
/// [`WaveFormatExtensible::sub_format_tag`] instead.
pub const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// A parsed `WAVEFORMATEX`.
#[derive(Debug, Clone, Default)]
pub struct WaveFormatEx {
    pub format_tag: u16,
    pub channels: u16,
    pub samples_per_sec: u32,
    pub avg_bytes_per_sec: u32,
    pub block_align: u16,
    pub bits_per_sample: u16,
    /// The `cbSize` bytes verbatim: an MS-ADPCM coefficient table, an IMA
    /// ADPCM samples-per-block count, a `WAVEFORMATEXTENSIBLE` tail, or
    /// nothing. Never longer than the chunk actually carried — see
    /// [`WaveFormatEx::parse`].
    pub extra: Vec<u8>,
}

/// Bytes in the fixed, pre-`cbSize` part of `WAVEFORMATEX`.
pub const FIXED_LEN: usize = 14;
/// Bytes once `wBitsPerSample` is present (the smallest form a `fmt ` chunk
/// legitimately uses).
pub const MIN_LEN: usize = 16;
/// Bytes once `cbSize` is present (possibly zero).
pub const WITH_CB_SIZE_LEN: usize = 18;
/// Bytes of the `WAVEFORMATEXTENSIBLE` tail carried in `extra` when the
/// format tag is [`WAVE_FORMAT_EXTENSIBLE`]:
/// `samples:u16 + channelMask:u32 + subFormat:u8[16]`.
pub const EXTENSIBLE_TAIL_LEN: usize = 22;

impl WaveFormatEx {
    /// Parse a `fmt `-chunk payload (or an `A_MS/ACM` `CodecPrivate` blob,
    /// which is the same structure verbatim).
    ///
    /// `data` may be exactly [`MIN_LEN`] bytes (no `cbSize` field at all —
    /// some writers omit it); anything shorter is rejected. The `cbSize`
    /// field, if present, is **not trusted**: it is clamped to the bytes
    /// `data` actually has left, so a `cbSize` claiming more than the chunk
    /// holds yields the bytes that exist rather than reading past the end or
    /// erroring. `budget` charges for the (small, bounded) `extra` copy.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if `data` is shorter than [`MIN_LEN`].
    /// [`vaco_core::Error::LimitExceeded`] if `extra` would exceed `budget`.
    pub fn parse(data: &[u8], budget: &mut Budget) -> Result<Self> {
        if data.len() < MIN_LEN {
            return Err(Error::InvalidData(
                "riff: fmt chunk shorter than WAVEFORMATEX",
            ));
        }
        let mut r = ByteReader::new(data);
        let format_tag = r.le16();
        let channels = r.le16();
        let samples_per_sec = r.le32();
        let avg_bytes_per_sec = r.le32();
        let block_align = r.le16();
        let bits_per_sample = r.le16();
        r.check()?;

        let extra = if data.len() >= WITH_CB_SIZE_LEN {
            let cb_size = usize::from(r.le16());
            r.check()?;
            let avail = r.remaining();
            let take = cb_size.min(avail);
            let src = r.bytes(take);
            let mut buf = budget.alloc::<u8>(src.len())?;
            buf.copy_from_slice(src);
            buf
        } else {
            Vec::new()
        };

        Ok(Self {
            format_tag,
            channels,
            samples_per_sec,
            avg_bytes_per_sec,
            block_align,
            bits_per_sample,
            extra,
        })
    }

    /// The `WAVEFORMATEXTENSIBLE` tail, if [`WaveFormatEx::format_tag`] is
    /// [`WAVE_FORMAT_EXTENSIBLE`] and `extra` is long enough to hold it.
    ///
    /// A tag of `WAVE_FORMAT_EXTENSIBLE` with a short `extra` is malformed
    /// rather than merely non-extensible, but this returns `None` for it
    /// rather than an error: every caller either wants the real codec (see
    /// [`crate::wave_tags::codec_name`], which already falls back to the raw
    /// tag) or is happy to treat an unrecognisable extensible header as
    /// "unknown", and neither needs a distinct error type for it.
    #[must_use]
    pub fn extensible(&self) -> Option<WaveFormatExtensible> {
        if self.format_tag != WAVE_FORMAT_EXTENSIBLE || self.extra.len() < EXTENSIBLE_TAIL_LEN {
            return None;
        }
        let mut r = ByteReader::new(&self.extra);
        let valid_bits_per_sample = r.le16();
        let channel_mask = r.le32();
        let sub_format = <[u8; 16]>::try_from(r.bytes(16)).unwrap_or([0; 16]);
        Some(WaveFormatExtensible {
            valid_bits_per_sample,
            channel_mask,
            sub_format,
        })
    }
}

/// The `WAVEFORMATEXTENSIBLE` tail: which of `wBitsPerSample`'s container
/// bits actually carry signal, which speaker each channel maps to, and the
/// GUID that is the *real* format tag when `wFormatTag == 0xFFFE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaveFormatExtensible {
    /// Also `wSamplesPerBlock` / `wReserved` in the same union slot for some
    /// non-PCM sub-formats; named for the common (PCM) case per `ksmedia.h`.
    pub valid_bits_per_sample: u16,
    /// `SPEAKER_*` bitmask (`ksmedia.h`), e.g. `0x4` for front-centre-only
    /// mono, `0x3` for front-left+front-right stereo.
    pub channel_mask: u32,
    /// The 16-byte `SubFormat` GUID, in the file's own byte order (the first
    /// four bytes are `Data1`, little-endian, as `ffmpeg` writes it).
    pub sub_format: [u8; 16],
}

/// The fixed suffix every Microsoft media subtype GUID shares:
/// `-0000-0010-8000-00AA00389B71`. Only `Data1` (the GUID's first four
/// bytes) varies, and it carries the pre-`WAVEFORMATEXTENSIBLE` format tag —
/// confirmed byte-for-byte against `ffmpeg`'s `pcm_s24le` encoder, whose
/// `SubFormat` is `01 00 00 00 00 00 10 00 80 00 00 aa 00 38 9b 71`: `Data1
/// = 0x00000001 = WAVE_FORMAT_PCM`.
const KSDATAFORMAT_SUBTYPE_SUFFIX: [u8; 12] = [
    0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];

impl WaveFormatExtensible {
    /// Recover the pre-extensible format tag from `sub_format`, if it is one
    /// of the standard Microsoft media subtypes (`Data1` varies, the rest of
    /// the GUID is the fixed suffix above). `None` for a vendor GUID that
    /// does not follow this convention.
    #[must_use]
    pub fn sub_format_tag(&self) -> Option<u16> {
        let data1 = self.sub_format.get(0..4)?;
        let suffix = self.sub_format.get(4..16)?;
        if suffix != KSDATAFORMAT_SUBTYPE_SUFFIX {
            return None;
        }
        let data1 = <[u8; 4]>::try_from(data1).ok()?;
        let value = u32::from_le_bytes(data1);
        u16::try_from(value).ok()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn plain(tag: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * u32::from(channels) * u32::from(bits) / 8).to_le_bytes());
        out.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out
    }

    #[test]
    fn sixteen_byte_form_has_no_extra() {
        let data = plain(WAVE_FORMAT_PCM, 2, 44_100, 16);
        assert_eq!(data.len(), MIN_LEN);
        let mut budget = Budget::new(Limits::permissive());
        let fmt = WaveFormatEx::parse(&data, &mut budget).unwrap();
        assert_eq!(fmt.format_tag, WAVE_FORMAT_PCM);
        assert_eq!(fmt.channels, 2);
        assert_eq!(fmt.samples_per_sec, 44_100);
        assert_eq!(fmt.bits_per_sample, 16);
        assert!(fmt.extra.is_empty());
    }

    #[test]
    fn eighteen_byte_form_with_zero_cbsize_has_no_extra() {
        let mut data = plain(WAVE_FORMAT_IEEE_FLOAT, 1, 44_100, 32);
        data.extend_from_slice(&0u16.to_le_bytes());
        let mut budget = Budget::new(Limits::permissive());
        let fmt = WaveFormatEx::parse(&data, &mut budget).unwrap();
        assert!(fmt.extra.is_empty());
    }

    #[test]
    fn extensible_tail_round_trips_pcm_s24le() {
        // Byte-for-byte the fmt chunk ffmpeg 8.1 writes for
        // `-c:a pcm_s24le` at 44100 Hz mono: WAVEFORMATEXTENSIBLE,
        // cbSize=22, validBitsPerSample=24, channelMask=SPEAKER_FRONT_CENTER,
        // SubFormat = KSDATAFORMAT_SUBTYPE_PCM.
        let mut data = plain(WAVE_FORMAT_EXTENSIBLE, 1, 44_100, 24);
        data.extend_from_slice(&22u16.to_le_bytes()); // cbSize
        data.extend_from_slice(&24u16.to_le_bytes()); // wValidBitsPerSample
        data.extend_from_slice(&4u32.to_le_bytes()); // SPEAKER_FRONT_CENTER
        data.extend_from_slice(&[
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38,
            0x9b, 0x71,
        ]);
        let mut budget = Budget::new(Limits::permissive());
        let fmt = WaveFormatEx::parse(&data, &mut budget).unwrap();
        let ext = fmt.extensible().unwrap();
        assert_eq!(ext.valid_bits_per_sample, 24);
        assert_eq!(ext.channel_mask, 4);
        assert_eq!(ext.sub_format_tag(), Some(WAVE_FORMAT_PCM));
    }

    #[test]
    fn a_vendor_subformat_guid_has_no_recoverable_tag() {
        let mut data = plain(WAVE_FORMAT_EXTENSIBLE, 2, 44_100, 16);
        data.extend_from_slice(&22u16.to_le_bytes());
        data.extend_from_slice(&16u16.to_le_bytes());
        data.extend_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(&[0xAB; 16]); // not the standard suffix
        let mut budget = Budget::new(Limits::permissive());
        let fmt = WaveFormatEx::parse(&data, &mut budget).unwrap();
        assert_eq!(fmt.extensible().unwrap().sub_format_tag(), None);
    }

    #[test]
    fn a_lying_cbsize_is_clamped_to_what_is_actually_there() {
        let mut data = plain(WAVE_FORMAT_ADPCM, 1, 8_000, 4);
        data.extend_from_slice(&60_000u16.to_le_bytes()); // wildly over
        data.extend_from_slice(&[1, 2, 3, 4]); // only four real bytes follow
        let mut budget = Budget::new(Limits::permissive());
        let fmt = WaveFormatEx::parse(&data, &mut budget).unwrap();
        assert_eq!(fmt.extra, vec![1, 2, 3, 4]);
    }

    #[test]
    fn shorter_than_sixteen_bytes_is_rejected() {
        let mut budget = Budget::new(Limits::permissive());
        assert!(WaveFormatEx::parse(&[0; 10], &mut budget).is_err());
    }

    #[test]
    fn a_tiny_budget_rejects_rather_than_panics() {
        let mut data = plain(WAVE_FORMAT_ADPCM, 1, 8_000, 4);
        data.extend_from_slice(&64u16.to_le_bytes());
        data.extend_from_slice(&[0xAA; 64]);
        let mut budget = Budget::new(Limits::tiny().with_alloc_total(2));
        assert!(WaveFormatEx::parse(&data, &mut budget).is_err());
    }
}
