//! Audio sample formats.

/// A sample format. Planar variants store each channel in its own buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SampleFmt {
    U8,
    S16,
    S32,
    S64,
    F32,
    F64,
    U8P,
    S16P,
    S32P,
    S64P,
    F32P,
    F64P,
}

impl SampleFmt {
    #[must_use]
    pub const fn is_planar(self) -> bool {
        matches!(
            self,
            Self::U8P | Self::S16P | Self::S32P | Self::S64P | Self::F32P | Self::F64P
        )
    }

    #[must_use]
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Self::U8 | Self::U8P => 1,
            Self::S16 | Self::S16P => 2,
            Self::S32 | Self::S32P | Self::F32 | Self::F32P => 4,
            Self::S64 | Self::S64P | Self::F64 | Self::F64P => 8,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::S16 => "s16",
            Self::S32 => "s32",
            Self::S64 => "s64",
            Self::F32 => "flt",
            Self::F64 => "dbl",
            Self::U8P => "u8p",
            Self::S16P => "s16p",
            Self::S32P => "s32p",
            Self::S64P => "s64p",
            Self::F32P => "fltp",
            Self::F64P => "dblp",
        }
    }
}
