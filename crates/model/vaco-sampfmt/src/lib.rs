#![forbid(unsafe_code)]
//! Audio sample formats.
//!
//! # What it is
//!
//! Twelve formats — six sample types (`u8`, `s16`, `s32`, `s64`, `flt`, `dbl`)
//! crossed with packed and planar storage — plus the metadata every audio
//! component asks about them: how wide is a sample, is the buffer one
//! interleaved block or one block per channel, and what is this format called
//! on a command line.
//!
//! This is the audio counterpart of `vaco-pixfmt` and it is deliberately just
//! as dumb: pure metadata, no conversion code, no "best format for this codec"
//! scoring. Conversion belongs in `vaco-resample`, and keeping it out of here is
//! what makes this crate exhaustively testable.
//!
//! # How it works
//!
//! There is no generated table. Twelve variants is small enough that a `match`
//! per property is both the clearest form and the fastest — every accessor is a
//! `const fn` over a `match`, so a call on a compile-time-known format folds to
//! an immediate inside a monomorphised conversion kernel, and a call on a
//! dynamic format is a jump table.
//!
//! The one non-obvious piece is [`SampleFmt::ALL`]: the enum's own declaration
//! order is *not* the order the reference tool lists formats in, and the listing
//! order is observable output. See that constant.
//!
//! # Where the names come from
//!
//! The twelve names and their bit depths are an interface: a command line
//! written against the reference tool has to mean the same thing here, and
//! `ffprobe`'s `sample_fmt` field has to spell them identically. They were
//! recorded from `ffmpeg -hide_banner -sample_fmts` (`FFmpeg` 8.1) rather than
//! chosen — see `docs/model/vaco-sampfmt.md` for the exact probe transcript.

use core::fmt;
use core::str::FromStr;

use vaco_core::Error;

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

/// How the samples of one format are represented numerically.
///
/// Split out from [`SampleFmt`] because a resampler branches on the arithmetic
/// (integer versus float, signed versus offset-binary) far more often than it
/// branches on the specific width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum SampleKind {
    /// Offset binary: silence is `0x80`, not `0`. Only `u8` uses this.
    Unsigned,
    /// Two's-complement signed integer; full scale is the type's range.
    Signed,
    /// IEEE-754 binary32 or binary64; nominal full scale is `-1.0..=1.0`,
    /// but nothing clamps it and codecs do produce values outside it.
    Float,
}

impl SampleFmt {
    /// Every format, in the order the reference tool lists them.
    ///
    /// # D17: this is *not* the enum's declaration order
    ///
    /// `ffmpeg -sample_fmts` prints
    ///
    /// ```text
    /// u8 s16 s32 flt dbl u8p s16p s32p fltp dblp s64 s64p
    /// ```
    ///
    /// — the two 64-bit integer formats last, after the planar float ones,
    /// because they were added to the enumeration after the rest and appending
    /// was the only way to keep the numeric values stable. A specification
    /// would have grouped them with their siblings.
    ///
    /// The listing order is observable output (`-sample_fmts` prints one row
    /// per format, in this order), so we reproduce it. The enum below is
    /// declared in the tidy order instead, because its discriminants are ours
    /// and are not observable anywhere. Iterate `ALL` whenever the order
    /// reaches a user; iterate the enum's own order never.
    pub const ALL: [Self; 12] = [
        Self::U8,
        Self::S16,
        Self::S32,
        Self::F32,
        Self::F64,
        Self::U8P,
        Self::S16P,
        Self::S32P,
        Self::F32P,
        Self::F64P,
        Self::S64,
        Self::S64P,
    ];

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

    /// The `depth` column of `ffmpeg -sample_fmts`: significant bits per sample.
    ///
    /// Always `8 * bytes_per_sample()` — there is no packed-into-a-wider-word
    /// audio format in the set, unlike the pixel-format table where the two
    /// differ constantly.
    #[must_use]
    pub const fn bits_per_sample(self) -> u32 {
        self.bytes_per_sample() as u32 * 8
    }

    /// How samples of this format are represented numerically.
    #[must_use]
    pub const fn kind(self) -> SampleKind {
        match self {
            Self::U8 | Self::U8P => SampleKind::Unsigned,
            Self::S16 | Self::S16P | Self::S32 | Self::S32P | Self::S64 | Self::S64P => {
                SampleKind::Signed
            }
            Self::F32 | Self::F32P | Self::F64 | Self::F64P => SampleKind::Float,
        }
    }

    /// Samples are IEEE-754 floats.
    #[must_use]
    pub const fn is_float(self) -> bool {
        matches!(self.kind(), SampleKind::Float)
    }

    /// The planar member of this format's pair. Idempotent on planar formats.
    #[must_use]
    pub const fn to_planar(self) -> Self {
        match self {
            Self::U8 | Self::U8P => Self::U8P,
            Self::S16 | Self::S16P => Self::S16P,
            Self::S32 | Self::S32P => Self::S32P,
            Self::S64 | Self::S64P => Self::S64P,
            Self::F32 | Self::F32P => Self::F32P,
            Self::F64 | Self::F64P => Self::F64P,
        }
    }

    /// The packed (interleaved) member of this format's pair. Idempotent on
    /// packed formats.
    #[must_use]
    pub const fn to_packed(self) -> Self {
        match self {
            Self::U8 | Self::U8P => Self::U8,
            Self::S16 | Self::S16P => Self::S16,
            Self::S32 | Self::S32P => Self::S32,
            Self::S64 | Self::S64P => Self::S64,
            Self::F32 | Self::F32P => Self::F32,
            Self::F64 | Self::F64P => Self::F64,
        }
    }

    /// Number of separate buffers a frame of `channels` channels needs.
    ///
    /// One per channel when planar, one in total when packed. `channels` is
    /// returned unchanged for a planar format, so a caller does not have to
    /// special-case zero.
    #[must_use]
    pub const fn plane_count(self, channels: u32) -> u32 {
        if self.is_planar() { channels } else { 1 }
    }

    /// Bytes in one buffer holding `samples` samples of `channels` channels.
    ///
    /// This is the size of **one** plane: for a planar format that is one
    /// channel's worth, for a packed format it is the whole interleaved block.
    /// Multiply by [`plane_count`](Self::plane_count) for the frame total.
    ///
    /// Returns `None` rather than wrapping. Both operands are attacker-chosen in
    /// practice — a container header supplies the channel count and a packet
    /// header the sample count — so overflow here is a real input, not a
    /// theoretical one, and `usize` is 32 bits on some targets we intend to
    /// build for.
    #[must_use]
    pub const fn plane_size(self, channels: u32, samples: u32) -> Option<usize> {
        let per_frame = if self.is_planar() { 1 } else { channels as u64 };
        // Checked, not saturating: on a 64-bit target `usize::MAX == u64::MAX`,
        // so a saturated product would compare equal to the cap and pass the
        // bound check it was supposed to fail.
        let Some(bytes) = (self.bytes_per_sample() as u64).checked_mul(per_frame) else {
            return None;
        };
        let Some(bytes) = bytes.checked_mul(samples as u64) else {
            return None;
        };
        if bytes > usize::MAX as u64 {
            None
        } else {
            Some(bytes as usize)
        }
    }

    /// Total bytes for every plane of a frame.
    ///
    /// # Errors
    /// [`Error::LimitExceeded`] if the buffer would not fit in a `usize`.
    pub fn buffer_size(self, channels: u32, samples: u32) -> Result<usize, Error> {
        let overflow = || Error::LimitExceeded {
            limit: "audio buffer",
            requested: u64::from(channels)
                .saturating_mul(u64::from(samples))
                .saturating_mul(self.bytes_per_sample() as u64),
            cap: usize::MAX as u64,
        };
        let plane = self.plane_size(channels, samples).ok_or_else(overflow)?;
        plane
            .checked_mul(self.plane_count(channels).max(1) as usize)
            .ok_or_else(overflow)
    }

    /// Parse a CLI-facing format name such as `s16` or `fltp`.
    ///
    /// # D17: exact match only
    ///
    /// The reference's `av_get_sample_fmt` is a linear `strcmp` over the name
    /// table and nothing else. It is case-sensitive (`S16` is rejected), does
    /// not trim (`" s16"` and `"s16 "` are both rejected), accepts no numeric
    /// form (`"1"` is rejected even though the enumerant exists), and has no
    /// aliases. Verified against `FFmpeg` 8.1 via `-sample_fmt`; the more
    /// forgiving behaviour seen through `aformat=sample_fmts=` comes from the
    /// filter's list splitter trimming its elements, not from this function.
    ///
    /// `none` is deliberately **not** accepted here. The reference does accept
    /// it, but one level up, in the option layer, where the target is a nullable
    /// field. Our equivalent is `Option<SampleFmt>` and the `None` spelling
    /// belongs with it — see `vaco-opts`.
    ///
    /// # Errors
    /// Returns [`Error::Option`] when the name is not a known format.
    pub fn from_name(name: &str) -> Result<Self, Error> {
        Self::ALL
            .into_iter()
            .find(|f| f.name() == name)
            .ok_or_else(|| Error::Option {
                name: "sample_fmt".to_owned(),
                detail: format!("unknown sample format `{name}`"),
            })
    }
}

/// The CLI-facing name — the same text [`SampleFmt::name`] returns.
impl fmt::Display for SampleFmt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for SampleFmt {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_name(s)
    }
}

#[cfg(test)]
mod tests;
