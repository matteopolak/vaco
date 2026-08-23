//! `ftyp` brand/compatible-brand lists per container profile, and the
//! registry descriptors for each name this crate answers to.
//!
//! ISO/IEC 14496-12 defines the box; it does not define which brands a given
//! tool writes for `-f ipod`/`-f psp`/etc. Those are measured — `ffmpeg 8.1`,
//! `-fflags +bitexact` **before** the input so it lands on the muxer and not
//! the demuxer (the position trap `planning/AGENT-CONSTRAINTS.md` warns
//! about) — and reproduced exactly. `docs/format/vaco-mux-mp4.md` lists the
//! commands.
//!
//! | Profile | Command | `major_brand` | `minor_version` | `compatible_brands` |
//! |---|---|---|---|---|
//! | `mp4` | `-f mp4` | `isom` | `0x200` | `isom iso2 mp41` |
//! | `mov` | `-f mov` | `qt  ` | `0x200` | `qt  ` |
//! | `ipod` | `-f ipod` | `M4V ` | `0x200` | `M4V  isom iso2` |
//! | `ismv` | `-f ismv` | `isml` | `0x200` | `isml piff` |
//! | `f4v` | `-f f4v` | `f4v ` | `0x200` | `f4v  isom iso2 avc1` |
//! | `psp` | `-f psp` | `MSNV` | `0x200` | `MSNV isom iso2` |
//! | `3gp` | `-f 3gp` | `3gp4` | `0x200` | `3gp4 isom iso2` |
//! | `3g2` | `-f 3g2` | `3g2a` | `0x10000` | `3g2a isom iso2` |
//! | `avif` | `-f avif` (still image) | `avif` | `0` | `avif mif1 miaf MA1B` |
//!
//! `avif`'s brand bytes are recorded for completeness, but this crate does
//! **not** write AVIF's actual structure: an AVIF file is a HEIF item
//! (`meta ▸ iinf/iloc/iprp/ipco/pitm`), not a `moov`/`trak` sample-table
//! track, and building that is a different box model than the rest of this
//! crate — see the crate-level *What is deferred* note. [`MUXER_AVIF`] is not
//! registered as a working muxer for this reason.

use vaco_core::Result;
use vaco_format_isom::fourcc::FourCc;
use vaco_format_isom::writer;
use vaco_io::MediaSink;

use crate::mux::MovMuxer;
use crate::options::{Brand, MuxOptions};
use vaco_codec_core::CodecId;
use vaco_format_core::{Muxer, MuxerDesc};

/// The `ftyp` fields for one brand profile.
#[derive(Debug, Clone, Copy)]
pub struct BrandSpec {
    pub major: FourCc,
    pub minor_version: u32,
    pub compatible: &'static [FourCc],
}

const fn fcc(b: [u8; 4]) -> FourCc {
    FourCc(b)
}

/// `-f mp4`.
pub const MP4: BrandSpec = BrandSpec {
    major: fcc(*b"isom"),
    minor_version: 0x0200,
    compatible: &[fcc(*b"isom"), fcc(*b"iso2"), fcc(*b"mp41")],
};

/// `-f mov`.
pub const MOV: BrandSpec = BrandSpec {
    major: fcc(*b"qt  "),
    minor_version: 0x0200,
    compatible: &[fcc(*b"qt  ")],
};

/// `-f ipod`.
pub const IPOD: BrandSpec = BrandSpec {
    major: fcc(*b"M4V "),
    minor_version: 0x0200,
    compatible: &[fcc(*b"M4V "), fcc(*b"isom"), fcc(*b"iso2")],
};

/// `-f ismv`.
pub const ISMV: BrandSpec = BrandSpec {
    major: fcc(*b"isml"),
    minor_version: 0x0200,
    compatible: &[fcc(*b"isml"), fcc(*b"piff")],
};

/// `-f f4v`.
pub const F4V: BrandSpec = BrandSpec {
    major: fcc(*b"f4v "),
    minor_version: 0x0200,
    compatible: &[fcc(*b"f4v "), fcc(*b"isom"), fcc(*b"iso2"), fcc(*b"avc1")],
};

/// `-f psp`.
pub const PSP: BrandSpec = BrandSpec {
    major: fcc(*b"MSNV"),
    minor_version: 0x0200,
    compatible: &[fcc(*b"MSNV"), fcc(*b"isom"), fcc(*b"iso2")],
};

/// `-f 3gp`.
pub const THREE_GP: BrandSpec = BrandSpec {
    major: fcc(*b"3gp4"),
    minor_version: 0x0200,
    compatible: &[fcc(*b"3gp4"), fcc(*b"isom"), fcc(*b"iso2")],
};

/// `-f 3g2`.
pub const THREE_G2: BrandSpec = BrandSpec {
    major: fcc(*b"3g2a"),
    minor_version: 0x0001_0000,
    compatible: &[fcc(*b"3g2a"), fcc(*b"isom"), fcc(*b"iso2")],
};

/// `-f avif`. Recorded for documentation; see the module docs — this crate
/// does not write AVIF's item-based structure.
pub const AVIF: BrandSpec = BrandSpec {
    major: fcc(*b"avif"),
    minor_version: 0,
    compatible: &[fcc(*b"avif"), fcc(*b"mif1"), fcc(*b"miaf"), fcc(*b"MA1B")],
};

impl Brand {
    /// The `ftyp` fields this brand writes.
    #[must_use]
    pub const fn spec(self) -> BrandSpec {
        match self {
            Self::Mp4 => MP4,
            Self::Mov => MOV,
            Self::Ipod => IPOD,
            Self::Ismv => ISMV,
            Self::F4v => F4V,
            Self::Psp => PSP,
            Self::ThreeGp => THREE_GP,
            Self::ThreeG2 => THREE_G2,
            Self::Avif => AVIF,
        }
    }
}

/// `ftyp`, for whichever brand [`MuxOptions::brand`] names.
#[must_use]
pub fn file_type_box(brand: Brand) -> Vec<u8> {
    let s = brand.spec();
    writer::file_type(b"ftyp", s.major, s.minor_version, s.compatible)
}

fn open_with(brand: Brand) -> impl Fn(Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    move |sink| {
        let opts = MuxOptions {
            brand,
            ..MuxOptions::default()
        };
        Ok(Box::new(MovMuxer::with_options(sink, opts)?) as Box<dyn Muxer>)
    }
}

fn open_mp4(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    open_with(Brand::Mp4)(sink)
}
fn open_mov(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    open_with(Brand::Mov)(sink)
}
fn open_ipod(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    open_with(Brand::Ipod)(sink)
}
fn open_ismv(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    open_with(Brand::Ismv)(sink)
}
fn open_f4v(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    open_with(Brand::F4v)(sink)
}
fn open_psp(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    open_with(Brand::Psp)(sink)
}
fn open_3gp(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    open_with(Brand::ThreeGp)(sink)
}
fn open_3g2(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    open_with(Brand::ThreeG2)(sink)
}

/// The registry descriptor for `-f mp4`.
pub const MUXER_MP4: MuxerDesc = MuxerDesc {
    name: "mp4",
    long_name: "MP4 (MPEG-4 Part 14)",
    extensions: &["mp4", "m4a", "m4v"],
    default_video: Some(CodecId::H264),
    default_audio: Some(CodecId::Aac),
    open: open_mp4,
};

/// `-f mov`.
pub const MUXER_MOV: MuxerDesc = MuxerDesc {
    name: "mov",
    long_name: "QuickTime / MOV",
    extensions: &["mov"],
    default_video: Some(CodecId::H264),
    default_audio: Some(CodecId::Aac),
    open: open_mov,
};

/// `-f ipod`.
pub const MUXER_IPOD: MuxerDesc = MuxerDesc {
    name: "ipod",
    long_name: "iPod H.264 MP4 (MPEG-4 Part 14)",
    extensions: &["m4v", "m4a"],
    default_video: Some(CodecId::H264),
    default_audio: Some(CodecId::Aac),
    open: open_ipod,
};

/// `-f ismv`.
pub const MUXER_ISMV: MuxerDesc = MuxerDesc {
    name: "ismv",
    long_name: "ISMV/ISMA (Smooth Streaming)",
    extensions: &["ismv", "isma"],
    default_video: Some(CodecId::H264),
    default_audio: Some(CodecId::Aac),
    open: open_ismv,
};

/// `-f f4v`.
pub const MUXER_F4V: MuxerDesc = MuxerDesc {
    name: "f4v",
    long_name: "F4V Adobe Flash Video",
    extensions: &["f4v"],
    default_video: Some(CodecId::H264),
    default_audio: Some(CodecId::Aac),
    open: open_f4v,
};

/// `-f psp`.
pub const MUXER_PSP: MuxerDesc = MuxerDesc {
    name: "psp",
    long_name: "PSP MP4 (MPEG-4 Part 14)",
    extensions: &["mp4", "psp"],
    default_video: Some(CodecId::H264),
    default_audio: Some(CodecId::Aac),
    open: open_psp,
};

/// `-f 3gp`.
pub const MUXER_3GP: MuxerDesc = MuxerDesc {
    name: "3gp",
    long_name: "3GP (3GPP file format)",
    extensions: &["3gp"],
    default_video: Some(CodecId::H264),
    default_audio: Some(CodecId::Aac),
    open: open_3gp,
};

/// `-f 3g2`.
pub const MUXER_3G2: MuxerDesc = MuxerDesc {
    name: "3g2",
    long_name: "3GP2 (3GPP2 file format)",
    extensions: &["3g2"],
    default_video: Some(CodecId::H264),
    default_audio: Some(CodecId::Aac),
    open: open_3g2,
};

/// Not a working muxer — see the module docs. Kept so the brand bytes have a
/// name other agents/tools can find, and so a future AVIF item writer has an
/// obvious place to register from.
pub const MUXER_AVIF: MuxerDesc = MuxerDesc {
    name: "avif",
    long_name: "AVIF (unsupported: HEIF item structure, not a track-based mux)",
    extensions: &[],
    default_video: None,
    default_audio: None,
    open: unsupported_avif,
};

fn unsupported_avif(_sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Err(vaco_core::Error::Unsupported(
        "mp4: avif is a HEIF item structure, not a moov/trak track mux; not implemented",
    ))
}
