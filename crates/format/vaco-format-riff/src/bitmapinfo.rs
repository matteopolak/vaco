//! `BITMAPINFOHEADER` — the structure carried in an AVI video stream's
//! format data, and, verbatim, in a Matroska `V_MS/VFW/FOURCC` track's
//! `CodecPrivate`.
//!
//! Windows SDK `wingdi.h`:
//!
//! ```text
//! biSize:u32           biWidth:i32          biHeight:i32
//! biPlanes:u16         biBitCount:u16       biCompression:u32
//! biSizeImage:u32      biXPelsPerMeter:i32  biYPelsPerMeter:i32
//! biClrUsed:u32        biClrImportant:u32
//! ```
//!
//! Exactly 40 bytes. `biSize` can claim more — `BITMAPV4HEADER` (108 bytes)
//! and `BITMAPV5HEADER` (124 bytes) extend it with colour-mask and ICC
//! profile fields for `BI_BITFIELDS`/embedded-profile bitmaps — but this
//! crate parses only the classic 40-byte prefix every one of those forms
//! shares and leaves the extension unparsed; see [`BitmapInfoHeader::parse`].

use vaco_bitstream::ByteReader;
use vaco_core::{Error, Result};

use crate::chunk::ChunkId;

/// A parsed `BITMAPINFOHEADER` (its classic 40-byte prefix; see the module
/// docs for the `BITMAPV4HEADER`/`BITMAPV5HEADER` case).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BitmapInfoHeader {
    /// `biSize`. Larger than [`BitmapInfoHeader::LEN`] for `BITMAPV4HEADER`/
    /// `BITMAPV5HEADER`; this crate does not parse the extension, but the
    /// field is preserved so a caller can tell the two apart.
    pub size: u32,
    pub width: i32,
    /// Negative for a top-down bitmap (rows stored top row first); positive
    /// for the ordinary bottom-up DIB row order. See
    /// [`BitmapInfoHeader::is_top_down`].
    pub height: i32,
    pub planes: u16,
    pub bit_count: u16,
    /// `biCompression`, raw. See [`Compression`] for the interpreted form.
    pub compression_raw: u32,
    pub size_image: u32,
    pub x_pels_per_meter: i32,
    pub y_pels_per_meter: i32,
    pub clr_used: u32,
    pub clr_important: u32,
}

impl BitmapInfoHeader {
    /// Bytes in the classic header.
    pub const LEN: usize = 40;

    /// Parse the classic 40-byte prefix from the start of `data`.
    ///
    /// Anything past the 40th byte (a `BITMAPV4HEADER`/`BITMAPV5HEADER`
    /// extension, or simply trailing palette/bitmap data in the same buffer)
    /// is ignored, not an error.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when fewer than [`BitmapInfoHeader::LEN`] bytes
    /// are present.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < Self::LEN {
            return Err(Error::InvalidData(
                "riff: BITMAPINFOHEADER shorter than 40 bytes",
            ));
        }
        let mut r = ByteReader::new(data);
        let size = r.le32();
        let width = r.le32().cast_signed();
        let height = r.le32().cast_signed();
        let planes = r.le16();
        let bit_count = r.le16();
        let compression_raw = r.le32();
        let size_image = r.le32();
        let x_pels_per_meter = r.le32().cast_signed();
        let y_pels_per_meter = r.le32().cast_signed();
        let clr_used = r.le32();
        let clr_important = r.le32();
        r.check()?;
        Ok(Self {
            size,
            width,
            height,
            planes,
            bit_count,
            compression_raw,
            size_image,
            x_pels_per_meter,
            y_pels_per_meter,
            clr_used,
            clr_important,
        })
    }

    /// Whether rows are stored top-first. The ordinary DIB convention
    /// (`height > 0`) is bottom-first.
    #[must_use]
    pub const fn is_top_down(&self) -> bool {
        self.height < 0
    }

    /// `|biHeight|`, the row count regardless of storage direction.
    #[must_use]
    pub const fn abs_height(&self) -> u32 {
        self.height.unsigned_abs()
    }

    /// [`Compression`], interpreted from [`BitmapInfoHeader::compression_raw`].
    #[must_use]
    pub fn compression(&self) -> Compression {
        Compression::from_u32(self.compression_raw)
    }
}

/// `biCompression`, interpreted.
///
/// Most values in the wild are a four-character code (`XVID`, `H264`, …:
/// probed spellings live in [`crate::video_tags`]); a handful of small
/// integers below any real `FourCC` are reserved by `wingdi.h` for the
/// original uncompressed and RLE forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// `BI_RGB` (0): uncompressed.
    Rgb,
    /// `BI_RLE8` (1): 8-bit run-length encoding.
    Rle8,
    /// `BI_RLE4` (2): 4-bit run-length encoding.
    Rle4,
    /// `BI_BITFIELDS` (3): uncompressed, with an explicit component bitmask
    /// immediately following the header (or, for `BITMAPV4HEADER` and later,
    /// carried in the header extension this crate does not parse).
    BitFields,
    /// A four-character codec code, e.g. `XVID`, `H264`, `MJPG`.
    FourCc(ChunkId),
    /// Some other numeric value below `0x20202020` (the smallest four
    /// printable-ASCII-byte value in little-endian order) that is neither a
    /// known reserved constant nor a plausible `FourCC`.
    Other(u32),
}

impl Compression {
    /// Interpret a raw `biCompression` value.
    #[must_use]
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::Rgb,
            1 => Self::Rle8,
            2 => Self::Rle4,
            3 => Self::BitFields,
            _ => {
                // A FourCC is stored as four ASCII bytes read in file order;
                // reassembling them from the little-endian u32 the chunk
                // reader already produced is the inverse of
                // `ChunkId::as_bytes` feeding `u32::from_le_bytes`. Probed:
                // ffprobe's `codec_tag` for `-c:v ffv1` is `0x31564646`,
                // whose LE bytes are `F F V 1`.
                let bytes = v.to_le_bytes();
                if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
                    Self::FourCc(ChunkId(bytes))
                } else {
                    Self::Other(v)
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn header(compression: u32, height: i32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&40u32.to_le_bytes()); // biSize
        out.extend_from_slice(&64i32.to_le_bytes()); // biWidth
        out.extend_from_slice(&height.to_le_bytes()); // biHeight
        out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
        out.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
        out.extend_from_slice(&compression.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // biSizeImage
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out
    }

    #[test]
    fn parses_the_classic_fields() {
        let data = header(0, 48);
        let h = BitmapInfoHeader::parse(&data).unwrap();
        assert_eq!(h.width, 64);
        assert_eq!(h.height, 48);
        assert_eq!(h.bit_count, 24);
        assert!(!h.is_top_down());
        assert_eq!(h.abs_height(), 48);
    }

    #[test]
    fn negative_height_is_top_down() {
        let data = header(0, -48);
        let h = BitmapInfoHeader::parse(&data).unwrap();
        assert!(h.is_top_down());
        assert_eq!(h.abs_height(), 48);
    }

    #[test]
    fn small_integers_are_the_reserved_constants() {
        assert_eq!(Compression::from_u32(0), Compression::Rgb);
        assert_eq!(Compression::from_u32(1), Compression::Rle8);
        assert_eq!(Compression::from_u32(2), Compression::Rle4);
        assert_eq!(Compression::from_u32(3), Compression::BitFields);
    }

    #[test]
    fn ffv1_fourcc_round_trips_from_the_probed_tag() {
        // ffprobe: `codec_tag=0x31564646` for `-c:v ffv1` in an AVI container.
        assert_eq!(
            Compression::from_u32(0x3156_4646),
            Compression::FourCc(ChunkId::new(b"FFV1"))
        );
    }

    #[test]
    fn extra_trailing_bytes_are_ignored_not_an_error() {
        let mut data = header(0, 1);
        data.extend_from_slice(&[0xAA; 68]); // BITMAPV4HEADER-sized tail
        let h = BitmapInfoHeader::parse(&data).unwrap();
        assert_eq!(h.height, 1);
    }

    #[test]
    fn shorter_than_forty_bytes_is_rejected() {
        assert!(BitmapInfoHeader::parse(&[0; 39]).is_err());
    }
}
