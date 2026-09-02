//! The 8-byte SWF signature/version/length header, the bit-packed `RECT`
//! stage-size record that follows it, and the two fixed fields after that
//! (frame rate, frame count).
//!
//! Measured against a real `ffmpeg -f lavfi ... -c:v flv1 -c:a mp3 out.swf`
//! capture (8.1): `46 57 53 06 17 79 00 00 60 00 28 00 00 28 00 00 0c 0c 00`.
//! `FWS`, version 6, file length `0x00007917` (little-endian, and it is
//! exactly the real file size — checked directly, not assumed), a 7-byte
//! `RECT` (`Nbits=12`, `xmin=ymin=0`, `xmax=ymax=1280` twips = 64x64 px),
//! then `frame_rate_raw=0x0c00` (12.0 fps, 8.8 fixed) and `frame_count=12`.

use vaco_bitstream::BitReader;
use vaco_core::{Error, Rational, Result};

/// `F`,`W`,`S` — uncompressed. The only signature this crate writes, and the
/// only one it reads: `ffmpeg -f swf`'s own muxer never produces anything
/// else (checked directly), so `CWS` (zlib) and `ZWS` (LZMA) compressed SWF
/// are a real, documented gap rather than a silent one — see the crate
/// docs.
pub const SIGNATURE_UNCOMPRESSED: [u8; 3] = *b"FWS";
pub const SIGNATURE_ZLIB: [u8; 3] = *b"CWS";
pub const SIGNATURE_LZMA: [u8; 3] = *b"ZWS";

/// Bytes before the bit-packed `RECT`: signature(3) + version(1) + file
/// length(4).
pub const FIXED_HEADER_LEN: usize = 8;

/// The parsed fixed header plus stage size and frame rate/count, everything
/// before the tag stream begins.
#[derive(Debug, Clone, Copy)]
pub struct SwfHeader {
    pub version: u8,
    /// The `FileLength` field. Not trusted for parsing (this crate reads
    /// tags until an `End` tag or EOF, same as any tag-length-driven walk
    /// has to on a source that might be truncated) — kept only to write
    /// back out, since the muxer must state it.
    pub file_length: u32,
    /// Stage width/height, in twips (1/20 px) — `(xmax - xmin, ymax -
    /// ymin)`. `ffprobe` reports pixel dimensions, i.e. this divided by 20;
    /// `vaco_pixfmt`-free here because SWF's own video dimensions come from
    /// `DefineVideoStream`, not the stage `RECT` — see `demux.rs`.
    pub stage_width_twips: i32,
    pub stage_height_twips: i32,
    /// 8.8 fixed-point frames/second, exactly as the file stores it.
    pub frame_rate_raw: u16,
    pub frame_count: u16,
}

impl SwfHeader {
    /// Frame rate as a rational (`frame_rate_raw / 256`).
    #[must_use]
    pub const fn frame_rate(self) -> Rational {
        Rational {
            num: self.frame_rate_raw as i32,
            den: 256,
        }
    }

    /// Parse the whole header (fixed part + `RECT` + rate/count) from the
    /// start of a file. Returns the header and the byte offset the tag
    /// stream starts at.
    ///
    /// # Errors
    /// [`Error::InvalidData`] for a bad signature or a `RECT`/rate/count
    /// that runs past `buf`; [`Error::Unsupported`] for a compressed
    /// signature (`CWS`/`ZWS`) — see the crate docs.
    pub fn parse(buf: &[u8]) -> Result<(Self, usize)> {
        let sig: [u8; 3] = buf
            .get(0..3)
            .and_then(|s| s.try_into().ok())
            .ok_or(Error::UnexpectedEof)?;
        if sig == SIGNATURE_ZLIB || sig == SIGNATURE_LZMA {
            return Err(Error::Unsupported(
                "swf: compressed (CWS/ZWS) SWF is not supported, only uncompressed FWS",
            ));
        }
        if sig != SIGNATURE_UNCOMPRESSED {
            return Err(Error::InvalidData("swf: expected an FWS/CWS/ZWS signature"));
        }
        let version = *buf.get(3).ok_or(Error::UnexpectedEof)?;
        let file_length = u32::from_le_bytes(
            buf.get(4..8)
                .and_then(|s| s.try_into().ok())
                .ok_or(Error::UnexpectedEof)?,
        );

        let rect_start = buf.get(FIXED_HEADER_LEN..).ok_or(Error::UnexpectedEof)?;
        let mut r = BitReader::new(rect_start);
        let nbits = r.get(5);
        let xmin = r.get_signed(nbits);
        let xmax = r.get_signed(nbits);
        let ymin = r.get_signed(nbits);
        let ymax = r.get_signed(nbits);
        r.align();
        // `finish()` would reject trailing bits, which is the wrong check
        // here (there is a whole tag stream after this) — `align()` already
        // did the only thing this parse needs, byte-align the cursor.
        let rect_bytes = r.remaining_bytes();
        let rect_len = rect_start
            .len()
            .checked_sub(rect_bytes.len())
            .ok_or(Error::InvalidData(
                "swf: RECT parse consumed more than it read",
            ))?;

        let after_rect = FIXED_HEADER_LEN.saturating_add(rect_len);
        let tail = buf
            .get(after_rect..after_rect.saturating_add(4))
            .ok_or(Error::UnexpectedEof)?;
        let frame_rate_raw = u16::from_le_bytes(
            tail.get(0..2)
                .ok_or(Error::UnexpectedEof)?
                .try_into()
                .unwrap_or([0, 0]),
        );
        let frame_count = u16::from_le_bytes(
            tail.get(2..4)
                .ok_or(Error::UnexpectedEof)?
                .try_into()
                .unwrap_or([0, 0]),
        );

        Ok((
            Self {
                version,
                file_length,
                stage_width_twips: xmax.saturating_sub(xmin),
                stage_height_twips: ymax.saturating_sub(ymin),
                frame_rate_raw,
                frame_count,
            },
            after_rect.saturating_add(4),
        ))
    }

    /// Serialise the fixed header, a minimal `RECT` (`xmin=ymin=0`,
    /// `xmax`/`ymax` from `stage_width_twips`/`stage_height_twips`) and the
    /// rate/count fields. `file_length` should be patched in afterwards
    /// once the whole file's length is known (see `mux.rs`).
    #[must_use]
    pub fn write(self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&SIGNATURE_UNCOMPRESSED);
        out.push(self.version);
        out.extend_from_slice(&self.file_length.to_le_bytes());

        // A minimal RECT: nbits sized to fit `xmax`/`ymax` (xmin/ymin are
        // always 0), matching how a real encoder would never spend more
        // bits than the values need.
        let max_val = self.stage_width_twips.max(self.stage_height_twips).max(0);
        let nbits = bits_needed_signed(max_val);
        let mut w = vaco_bitstream::BitWriter::new();
        w.put(5, u32::from(nbits));
        w.put_signed(u32::from(nbits), 0);
        w.put_signed(u32::from(nbits), self.stage_width_twips);
        w.put_signed(u32::from(nbits), 0);
        w.put_signed(u32::from(nbits), self.stage_height_twips);
        w.align_zero();
        out.extend_from_slice(&w.finish());

        out.extend_from_slice(&self.frame_rate_raw.to_le_bytes());
        out.extend_from_slice(&self.frame_count.to_le_bytes());
        out
    }
}

/// The smallest `n` such that a signed value `v` fits in `n` bits
/// (two's-complement, so `n` must also cover the sign bit). Used to size
/// `RECT`'s `Nbits` field the way a real encoder would: no wider than the
/// values need.
fn bits_needed_signed(v: i32) -> u8 {
    if v == 0 {
        return 1;
    }
    // +1 for the sign bit on top of the magnitude's own bit width.
    let magnitude_bits = 32 - v.unsigned_abs().leading_zeros();
    u8::try_from(magnitude_bits.saturating_add(1)).unwrap_or(u8::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    /// The exact bytes measured in the module docs.
    const MEASURED: &[u8] = &[
        0x46, 0x57, 0x53, 0x06, 0x17, 0x79, 0x00, 0x00, 0x60, 0x00, 0x28, 0x00, 0x00, 0x28, 0x00,
        0x00, 0x0c, 0x0c, 0x00,
    ];

    #[test]
    fn the_measured_header_parses_to_the_known_values() {
        let (h, next) = SwfHeader::parse(MEASURED).unwrap();
        assert_eq!(h.version, 6);
        assert_eq!(h.file_length, 0x0000_7917);
        assert_eq!(h.stage_width_twips, 1280);
        assert_eq!(h.stage_height_twips, 1280);
        assert_eq!(h.frame_rate_raw, 0x0c00); // 12.0 fps, 8.8 fixed
        assert_eq!(h.frame_count, 12);
        assert_eq!(next, MEASURED.len());
    }

    #[test]
    fn a_zlib_signature_is_a_named_unsupported_error() {
        let mut buf = MEASURED.to_vec();
        buf[0..3].copy_from_slice(&SIGNATURE_ZLIB);
        assert!(matches!(SwfHeader::parse(&buf), Err(Error::Unsupported(_))));
    }

    #[test]
    fn a_bad_signature_is_rejected() {
        let mut buf = MEASURED.to_vec();
        buf[0] = b'X';
        assert!(SwfHeader::parse(&buf).is_err());
    }

    #[test]
    fn writing_and_reparsing_a_header_round_trips_the_stage_size_and_rate() {
        let h = SwfHeader {
            version: 6,
            file_length: 12345,
            stage_width_twips: 1280,
            stage_height_twips: 1280,
            frame_rate_raw: 0x0c00,
            frame_count: 12,
        };
        let bytes = h.write();
        let (parsed, _) = SwfHeader::parse(&bytes).unwrap();
        assert_eq!(parsed.stage_width_twips, 1280);
        assert_eq!(parsed.stage_height_twips, 1280);
        assert_eq!(parsed.frame_rate_raw, 0x0c00);
        assert_eq!(parsed.frame_count, 12);
    }
}
