//! The four-character code, and the box-type constants built from it.
//!
//! ISO/IEC 14496-12 §4.2 gives every box a 32-bit type, conventionally written
//! as four printable characters. `QuickTime` adds types whose first byte is
//! `0xA9` (`©nam` and friends), so the display implementation cannot assume
//! ASCII — it escapes anything outside the printable range rather than
//! producing a lossy character.

use core::fmt;

/// A box type or brand: four bytes, big-endian in the file, never reinterpreted
/// as an integer by this crate.
///
/// Comparison is byte comparison. `FourCc` is `Copy` and pointer-free so tables
/// of them are `const`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FourCc(pub [u8; 4]);

impl FourCc {
    /// From a literal, e.g. `FourCc::new(b"moov")`.
    #[must_use]
    pub const fn new(v: &[u8; 4]) -> Self {
        Self(*v)
    }

    /// The raw bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 4] {
        self.0
    }

    /// The big-endian integer form, as `ffprobe` prints `codec_tag`.
    ///
    /// Note the reference prints `codec_tag` **little-endian** (`avc1` shows as
    /// `0x31637661`) while `codec_tag_string` shows the characters in file
    /// order. Both spellings are available; pick deliberately.
    #[must_use]
    pub const fn as_u32_be(self) -> u32 {
        u32::from_be_bytes(self.0)
    }

    /// The little-endian integer form — the value `ffprobe`'s `codec_tag` field
    /// carries.
    #[must_use]
    pub const fn as_u32_le(self) -> u32 {
        u32::from_le_bytes(self.0)
    }

    /// Whether every byte is printable ASCII, i.e. whether [`fmt::Display`]
    /// round-trips.
    #[must_use]
    pub const fn is_printable(self) -> bool {
        let [a, b, c, d] = self.0;
        a.is_ascii_graphic() && b.is_ascii_graphic() && c.is_ascii_graphic() && d.is_ascii_graphic()
    }
}

impl fmt::Display for FourCc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            if b.is_ascii_graphic() || b == b' ' {
                write!(f, "{}", char::from(b))?;
            } else {
                write!(f, "\\x{b:02x}")?;
            }
        }
        Ok(())
    }
}

impl fmt::Debug for FourCc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FourCc({self})")
    }
}

impl From<[u8; 4]> for FourCc {
    fn from(v: [u8; 4]) -> Self {
        Self(v)
    }
}

/// The box types this crate names.
///
/// Grouped as ISO/IEC 14496-12 groups them. Anything not listed is still
/// parsed structurally — an unknown box is skipped by its declared size, which
/// is the whole point of the box grammar — these are only the ones with
/// meaning attached.
pub mod boxes {
    use super::FourCc;

    macro_rules! types {
        ($($name:ident = $lit:literal),* $(,)?) => {
            $(
                #[doc = concat!("The box type ", stringify!($lit), ".")]
                pub const $name: FourCc = FourCc(*$lit);
            )*
        };
    }

    types! {
        // Top level.
        FTYP = b"ftyp", STYP = b"styp", MOOV = b"moov", MOOF = b"moof", MDAT = b"mdat",
        FREE = b"free", SKIP = b"skip", WIDE = b"wide", JUNK = b"junk", PNOT = b"pnot",
        SIDX = b"sidx", SSIX = b"ssix", PRFT = b"prft", EMSG = b"emsg", MFRA = b"mfra",
        PSSH = b"pssh", UUID = b"uuid", META = b"meta",

        // Movie.
        MVHD = b"mvhd", TRAK = b"trak", MVEX = b"mvex", MEHD = b"mehd", TREX = b"trex",
        TREP = b"trep", UDTA = b"udta", IODS = b"iods",

        // Track.
        TKHD = b"tkhd", TREF = b"tref", EDTS = b"edts", ELST = b"elst", MDIA = b"mdia",
        TAPT = b"tapt",

        // Media.
        MDHD = b"mdhd", HDLR = b"hdlr", ELNG = b"elng", MINF = b"minf",

        // Media information.
        VMHD = b"vmhd", SMHD = b"smhd", HMHD = b"hmhd", NMHD = b"nmhd", GMHD = b"gmhd",
        STHD = b"sthd", DINF = b"dinf", DREF = b"dref", URL_ = b"url ", URN_ = b"urn ",
        STBL = b"stbl",

        // Sample table.
        STSD = b"stsd", STTS = b"stts", CTTS = b"ctts", CSLG = b"cslg", STSS = b"stss",
        STSC = b"stsc", STSZ = b"stsz", STZ2 = b"stz2", STCO = b"stco", CO64 = b"co64",
        SDTP = b"sdtp", SBGP = b"sbgp", SGPD = b"sgpd", SAIZ = b"saiz", SAIO = b"saio",
        SENC = b"senc", SUBS = b"subs", PADB = b"padb", STSH = b"stsh",

        // Fragments.
        MFHD = b"mfhd", TRAF = b"traf", TFHD = b"tfhd", TFDT = b"tfdt", TRUN = b"trun",
        TFRA = b"tfra", MFRO = b"mfro",

        // Handler types.
        VIDE = b"vide", SOUN = b"soun", SUBT = b"subt", SBTL = b"sbtl", TEXT = b"text",
        CLCP = b"clcp", TMCD = b"tmcd", HINT = b"hint", META_HDLR = b"meta",

        // Sample-entry extension boxes.
        AVCC = b"avcC", HVCC = b"hvcC", VVCC = b"vvcC", AV1C = b"av1C", VPCC = b"vpcC",
        ESDS = b"esds", DOPS = b"dOps", DFLA = b"dfLa", DAC3 = b"dac3", DEC3 = b"dec3",
        DMLP = b"dmlp", ALAC = b"alac", WAVE = b"wave", FRMA = b"frma", BTRT = b"btrt",
        PASP = b"pasp", COLR = b"colr", CLAP = b"clap", FIEL = b"fiel", GAMA = b"gama",
        CHNL = b"chnl", SRAT = b"srat", CHAN = b"chan", GLBL = b"glbl", SINF = b"sinf",
        SCHM = b"schm", SCHI = b"schi", TENC = b"tenc", DVCC = b"dvcC", DVVC = b"dvvC",
        DVWC = b"dvwC", CLLI = b"clli", MDCV = b"mdcv", SMDM = b"SmDm", COLL = b"CoLL",
        CCST = b"ccst",
        // `QuickTime`'s endian atom — inside `wave`, alongside `frma`, for the
        // sample entries whose fourcc does not fix a byte order on its own
        // (`in24`, `in32`, `fl32`, `fl64`). See `stsd.rs`'s module docs.
        ENDA = b"enda",

        // Metadata.
        ILST = b"ilst", KEYS = b"keys", CHPL = b"chpl", DATA = b"data", MEAN = b"mean",
        NAME = b"name",

        // Track reference types.
        CHAP = b"chap", CDSC = b"cdsc", DPND = b"dpnd", FALL = b"fall", VDEP = b"vdep",
        VPLX = b"vplx",

        // HEIF/AVIF item model (ISO/IEC 23008-12 §9), inside a `meta` box.
        PITM = b"pitm", ILOC = b"iloc", IINF = b"iinf", INFE = b"infe",
        IPRP = b"iprp", IPCO = b"ipco", IPMA = b"ipma", IREF = b"iref",
        IDAT = b"idat", ISPE = b"ispe", PIXI = b"pixi", PICT = b"pict",
        // `iref` reference types. `cdsc` ("content describes") is the same
        // four bytes as the `tref` reference type above (`boxes::CDSC`) —
        // reused rather than redeclared.
        DIMG = b"dimg", THMB = b"thmb", AUXL = b"auxl",
    }
}
