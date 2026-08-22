//! The option type model: 20 bases plus an orthogonal array modifier.

/// The base type of an option value.
///
/// The inventory's 21 `AVOptionType` values map onto 20 bases plus
/// [`OptKind::array`], because the array-ness is a modifier in the C model too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum OptBase {
    /// `u64` bitmask; named constants via `unit`; `+a-b` syntax.
    Flags,
    /// `i32`
    Int,
    /// `i64`
    Int64,
    /// `u32`
    UInt,
    /// `u64`
    UInt64,
    /// `f64`
    Double,
    /// `f32`
    Float,
    /// `bool`; `Option<bool>` additionally accepts and emits `auto`.
    Bool,
    /// `String`
    String,
    /// [`vaco_core::Rational`], `"num/den"`.
    Rational,
    /// [`crate::Binary`], hex-encoded.
    Binary,
    /// [`crate::Dict`], nested `k=v:k=v`.
    Dict,
    /// Never a struct field: a named constant belonging to a `unit`.
    Const,
    /// `(u32, u32)`, `"1920x1080"` or an abbreviation such as `"hd1080"`.
    ImageSize,
    /// Implemented in `vaco-pixfmt`; `vaco-opts` only names the tag.
    PixelFmt,
    /// Implemented in `vaco-sampfmt`.
    SampleFmt,
    /// Implemented in `vaco-chlayout`.
    ChLayout,
    /// [`crate::VideoRate`], `"25"` or `"ntsc"`.
    VideoRate,
    /// [`vaco_core::Duration`], microseconds.
    Duration,
    /// [`crate::Rgba`].
    Color,
}

impl OptBase {
    /// The type name printed in the `-h full` type column.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Flags => "flags",
            Self::Int => "int",
            Self::Int64 => "int64",
            Self::UInt => "uint",
            Self::UInt64 => "uint64",
            Self::Double => "double",
            Self::Float => "float",
            Self::Bool => "boolean",
            Self::String => "string",
            Self::Rational => "rational",
            Self::Binary => "binary",
            Self::Dict => "dictionary",
            Self::Const => "const",
            Self::ImageSize => "image_size",
            Self::PixelFmt => "pix_fmt",
            Self::SampleFmt => "sample_fmt",
            Self::ChLayout => "channel_layout",
            Self::VideoRate => "video_rate",
            Self::Duration => "duration",
            Self::Color => "color",
        }
    }

    /// Whether the base carries a number that range checks and
    /// [`crate::OptValue::as_f64`] apply to.
    #[must_use]
    pub const fn is_numeric(self) -> bool {
        matches!(
            self,
            Self::Flags
                | Self::Int
                | Self::Int64
                | Self::UInt
                | Self::UInt64
                | Self::Double
                | Self::Float
                | Self::Duration
        )
    }

    /// Every base, in declaration order. Used by the exhaustiveness tests.
    pub const ALL: [Self; 20] = [
        Self::Flags,
        Self::Int,
        Self::Int64,
        Self::UInt,
        Self::UInt64,
        Self::Double,
        Self::Float,
        Self::Bool,
        Self::String,
        Self::Rational,
        Self::Binary,
        Self::Dict,
        Self::Const,
        Self::ImageSize,
        Self::PixelFmt,
        Self::SampleFmt,
        Self::ChLayout,
        Self::VideoRate,
        Self::Duration,
        Self::Color,
    ];
}

/// The array modifier: separator plus a length bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArrayDesc {
    pub sep: char,
    pub min_len: u32,
    pub max_len: u32,
}

impl ArrayDesc {
    /// The separator `FFmpeg` uses when an option table does not name one.
    pub const DEFAULT_SEP: char = '|';

    #[must_use]
    pub const fn new(sep: char, min_len: u32, max_len: u32) -> Self {
        Self {
            sep,
            min_len,
            max_len,
        }
    }
}

impl Default for ArrayDesc {
    fn default() -> Self {
        Self::new(Self::DEFAULT_SEP, 0, u32::MAX)
    }
}

/// A base plus its optional array modifier. `FLAG_ARRAY` is
/// `OptKind { base: Flags, array: Some(_) }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptKind {
    pub base: OptBase,
    pub array: Option<ArrayDesc>,
}

impl OptKind {
    #[must_use]
    pub const fn scalar(base: OptBase) -> Self {
        Self { base, array: None }
    }

    #[must_use]
    pub const fn array(base: OptBase, desc: ArrayDesc) -> Self {
        Self {
            base,
            array: Some(desc),
        }
    }

    /// The type string printed by `-h full`, e.g. `int` or `[int]`.
    #[must_use]
    pub fn type_name(self) -> String {
        if self.array.is_some() {
            format!("[{}]", self.base.name())
        } else {
            self.base.name().to_owned()
        }
    }
}
