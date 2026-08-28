//! `METADATA_BLOCK_PICTURE`: an embedded cover image or similar artwork.
//!
//! `Vaco-Spec-Ref: rfc-9639`. Field layout verified against the actual
//! bytes `ffmpeg`'s FLAC muxer writes for an attached picture (a real 4x4
//! JPEG, `-disposition:v attached_pic`), not transcribed from the
//! specification text alone:
//!
//! ```text
//! picture_type       (u32, BE)
//! mime_type_length   (u32, BE)    mime_type (ASCII)
//! description_length (u32, BE)    description (UTF-8)
//! width               (u32, BE)
//! height              (u32, BE)
//! color_depth         (u32, BE)   bits per pixel
//! indexed_colors      (u32, BE)   0 for a non-indexed format
//! data_length         (u32, BE)   picture_data (data_length bytes)
//! ```

use vaco_core::{Error, Result};

/// `picture_type`'s standard values. The same numbering `ID3v2`'s `APIC` frame
/// uses — FLAC's own format document says so directly — reproduced here as
/// a plain enumeration of the values, not as spec prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PictureType {
    Other,
    FileIcon32x32,
    OtherFileIcon,
    CoverFront,
    CoverBack,
    LeafletPage,
    Media,
    LeadArtist,
    Artist,
    Conductor,
    Band,
    Composer,
    Lyricist,
    RecordingLocation,
    DuringRecording,
    DuringPerformance,
    VideoScreenCapture,
    BrightColouredFish,
    Illustration,
    ArtistLogo,
    PublisherLogo,
    /// A value outside the standard `0..=20` range.
    Unknown(u32),
}

impl PictureType {
    #[must_use]
    pub const fn from_u32(value: u32) -> Self {
        match value {
            0 => Self::Other,
            1 => Self::FileIcon32x32,
            2 => Self::OtherFileIcon,
            3 => Self::CoverFront,
            4 => Self::CoverBack,
            5 => Self::LeafletPage,
            6 => Self::Media,
            7 => Self::LeadArtist,
            8 => Self::Artist,
            9 => Self::Conductor,
            10 => Self::Band,
            11 => Self::Composer,
            12 => Self::Lyricist,
            13 => Self::RecordingLocation,
            14 => Self::DuringRecording,
            15 => Self::DuringPerformance,
            16 => Self::VideoScreenCapture,
            17 => Self::BrightColouredFish,
            18 => Self::Illustration,
            19 => Self::ArtistLogo,
            20 => Self::PublisherLogo,
            other => Self::Unknown(other),
        }
    }

    #[must_use]
    pub const fn to_u32(self) -> u32 {
        match self {
            Self::Other => 0,
            Self::FileIcon32x32 => 1,
            Self::OtherFileIcon => 2,
            Self::CoverFront => 3,
            Self::CoverBack => 4,
            Self::LeafletPage => 5,
            Self::Media => 6,
            Self::LeadArtist => 7,
            Self::Artist => 8,
            Self::Conductor => 9,
            Self::Band => 10,
            Self::Composer => 11,
            Self::Lyricist => 12,
            Self::RecordingLocation => 13,
            Self::DuringRecording => 14,
            Self::DuringPerformance => 15,
            Self::VideoScreenCapture => 16,
            Self::BrightColouredFish => 17,
            Self::Illustration => 18,
            Self::ArtistLogo => 19,
            Self::PublisherLogo => 20,
            Self::Unknown(v) => v,
        }
    }
}

/// A parsed `METADATA_BLOCK_PICTURE`, borrowing its input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture<'a> {
    pub picture_type: PictureType,
    pub mime_type: &'a str,
    pub description: &'a str,
    pub width: u32,
    pub height: u32,
    /// Bits per pixel.
    pub color_depth: u32,
    /// Number of colours for an indexed (e.g. GIF) image; `0` otherwise.
    pub indexed_colors: u32,
    pub data: &'a [u8],
}

impl<'a> Picture<'a> {
    /// Parse a `METADATA_BLOCK_PICTURE` payload: the FLAC metadata block's
    /// content, with its own 4-byte block header already stripped.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when a declared length overruns the input, or
    /// `mime_type`/`description` are not valid text — the reference states
    /// `mime_type` is restricted to printable ASCII and `description` is
    /// UTF-8; both are checked here as UTF-8, which accepts every valid
    /// ASCII `mime_type` and only ever rejects an already-malformed one.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let (picture_type, rest) = take_u32(data)?;
        let (mime_type, rest) = take_str(rest)?;
        let (description, rest) = take_str(rest)?;
        let (width, rest) = take_u32(rest)?;
        let (height, rest) = take_u32(rest)?;
        let (color_depth, rest) = take_u32(rest)?;
        let (indexed_colors, rest) = take_u32(rest)?;
        let (data_len, rest) = take_u32(rest)?;
        let data_len = usize::try_from(data_len)
            .map_err(|_| Error::InvalidData("FLAC picture data length too large"))?;
        let Some(picture_data) = rest.get(..data_len) else {
            return Err(Error::InvalidData("FLAC picture data overruns the block"));
        };
        Ok(Self {
            picture_type: PictureType::from_u32(picture_type),
            mime_type,
            description,
            width,
            height,
            color_depth,
            indexed_colors,
            data: picture_data,
        })
    }
}

fn take_u32(data: &[u8]) -> Result<(u32, &[u8])> {
    let Some((head, rest)) = data.split_at_checked(4) else {
        return Err(Error::InvalidData("truncated FLAC picture field"));
    };
    let Some(bytes) = head.first_chunk::<4>() else {
        return Err(Error::InvalidData("truncated FLAC picture field"));
    };
    Ok((u32::from_be_bytes(*bytes), rest))
}

fn take_str(data: &[u8]) -> Result<(&str, &[u8])> {
    let (len, rest) = take_u32(data)?;
    let len = usize::try_from(len).map_err(|_| Error::InvalidData("FLAC picture string too long"))?;
    let Some((head, tail)) = rest.split_at_checked(len) else {
        return Err(Error::InvalidData("FLAC picture string overruns the block"));
    };
    let text =
        str::from_utf8(head).map_err(|_| Error::InvalidData("FLAC picture string is not UTF-8"))?;
    Ok((text, tail))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test code over fixed fixtures")]
mod tests {
    use super::*;

    fn be32(n: u32) -> [u8; 4] {
        n.to_be_bytes()
    }

    /// Byte-for-byte the shape measured from a real `ffmpeg -c:a flac
    /// -disposition:v attached_pic` file: type 0 ("Other"), `image/jpeg`, no
    /// description, 4x4, and a handful of placeholder picture bytes.
    fn fixture() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&be32(0)); // Other
        out.extend_from_slice(&be32(10));
        out.extend_from_slice(b"image/jpeg");
        out.extend_from_slice(&be32(0)); // no description
        out.extend_from_slice(&be32(4)); // width
        out.extend_from_slice(&be32(4)); // height
        out.extend_from_slice(&be32(12)); // depth
        out.extend_from_slice(&be32(0)); // not indexed
        out.extend_from_slice(&be32(3));
        out.extend_from_slice(&[0xff, 0xd8, 0xff]); // JPEG SOI + marker start
        out
    }

    #[test]
    fn parses_the_measured_shape() {
        let data = fixture();
        let picture = Picture::parse(&data).expect("valid picture block");
        assert_eq!(picture.picture_type, PictureType::Other);
        assert_eq!(picture.mime_type, "image/jpeg");
        assert_eq!(picture.description, "");
        assert_eq!((picture.width, picture.height), (4, 4));
        assert_eq!(picture.color_depth, 12);
        assert_eq!(picture.indexed_colors, 0);
        assert_eq!(picture.data, &[0xff, 0xd8, 0xff]);
    }

    #[test]
    fn picture_type_round_trips() {
        for v in 0u32..=20 {
            assert_eq!(PictureType::from_u32(v).to_u32(), v);
        }
        assert_eq!(PictureType::from_u32(99), PictureType::Unknown(99));
        assert_eq!(PictureType::Unknown(99).to_u32(), 99);
    }

    #[test]
    fn a_declared_length_past_the_end_is_an_error_not_a_panic() {
        assert!(Picture::parse(&[]).is_err());
        let mut data = fixture();
        let len = data.len();
        data.truncate(len - 1);
        assert!(Picture::parse(&data).is_err());
    }

    #[test]
    fn never_panics_on_arbitrary_bytes() {
        for len in 0..64usize {
            let data = vec![0xffu8; len];
            let _ = Picture::parse(&data);
        }
    }
}
