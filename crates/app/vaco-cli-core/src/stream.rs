//! The facts a stream specifier is matched against.
//!
//! This crate owns the *grammar*, not the container. So it defines the smallest
//! description of a stream that the grammar needs — index, id, media type,
//! disposition, tags, and enough of the codec parameters to answer `u` (usable)
//! — and demuxers fill it in. Nothing here knows what a demuxer is.

use vaco_core::{Dict, DictFlags, MediaType};

/// The 19 stream disposition flags, in the reference's bit order.
///
/// The order is an interface fact: `ffmpeg -dispositions` prints exactly this
/// list, `-disposition:v default+forced` parses against it, and the
/// `stream_disposition` ffprobe section prints one field per name in this
/// order. Bit *n* is the *n*-th name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Disposition(u32);

macro_rules! dispositions {
    ($($bit:literal => $konst:ident, $name:literal;)*) => {
        impl Disposition {
            $(
                #[doc = concat!("`", $name, "`")]
                pub const $konst: Self = Self(1 << $bit);
            )*

            /// Every flag, paired with its CLI name, in bit order.
            pub const ALL: &'static [(Self, &'static str)] = &[
                $((Self(1 << $bit), $name),)*
            ];
        }
    };
}

dispositions! {
     0 => DEFAULT,           "default";
     1 => DUB,               "dub";
     2 => ORIGINAL,          "original";
     3 => COMMENT,           "comment";
     4 => LYRICS,            "lyrics";
     5 => KARAOKE,           "karaoke";
     6 => FORCED,            "forced";
     7 => HEARING_IMPAIRED,  "hearing_impaired";
     8 => VISUAL_IMPAIRED,   "visual_impaired";
     9 => CLEAN_EFFECTS,     "clean_effects";
    10 => ATTACHED_PIC,      "attached_pic";
    11 => TIMED_THUMBNAILS,  "timed_thumbnails";
    12 => NON_DIEGETIC,      "non_diegetic";
    13 => CAPTIONS,          "captions";
    14 => DESCRIPTIONS,      "descriptions";
    15 => METADATA,          "metadata";
    16 => DEPENDENT,         "dependent";
    17 => STILL_IMAGE,       "still_image";
    18 => MULTILAYER,        "multilayer";
}

impl Disposition {
    /// No flags set.
    pub const NONE: Self = Self(0);

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// Whether every bit of `other` is set here. An empty `other` always
    /// matches, which is what makes `disp:0` select every stream.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Look a single flag up by its CLI name. Case-sensitive, as the reference's
    /// named-constant lookup is.
    #[must_use]
    pub fn by_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(_, n)| *n == name)
            .map(|&(flag, _)| flag)
    }

    /// The names of the set bits, in bit order.
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        Self::ALL
            .iter()
            .filter(move |(f, _)| self.contains(*f))
            .map(|&(_, n)| n)
    }
}

impl core::ops::BitOr for Disposition {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl core::ops::BitOrAssign for Disposition {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// One stream, as far as the specifier grammar is concerned.
///
/// `Default` gives an untyped, unusable, untagged stream — deliberately, so a
/// test or a fuzz target can build a stream set without naming every field.
#[derive(Debug, Clone, Default)]
pub struct StreamInfo {
    /// Position in container order. Also the value a bare-integer specifier
    /// compares against when it is the whole specifier.
    pub index: u32,
    /// The container's own stream id, matched by `#N` and `i:N`.
    pub id: i64,
    /// `None` for a stream whose type the demuxer could not determine — such a
    /// stream matches no type letter, and is never `u`sable.
    pub media_type: Option<MediaType>,
    pub disposition: Disposition,
    /// Stream metadata. Key matching is ASCII-case-insensitive; value matching
    /// is case-sensitive. Both verified against the reference.
    pub tags: Dict,
    /// Whether a codec was identified at all.
    pub codec_known: bool,
    pub width: u32,
    pub height: u32,
    pub sample_rate: u32,
}

impl StreamInfo {
    /// The `u` predicate: "usable", meaning the stream carries enough
    /// information for the tool to do something with it.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        if !self.codec_known {
            return false;
        }
        match self.media_type {
            Some(MediaType::Video) => self.width > 0 && self.height > 0,
            Some(MediaType::Audio) => self.sample_rate > 0,
            Some(_) => true,
            None => false,
        }
    }

    /// Case-insensitive tag lookup, matching the reference's metadata specifier.
    #[must_use]
    pub fn tag(&self, key: &str) -> Option<&str> {
        self.tags
            .get_with(
                key,
                None,
                DictFlags {
                    match_case: false,
                    ..DictFlags::exact()
                },
            )
            .map(|(_, _, v)| v)
    }
}

/// A program: the MPEG-TS grouping the `p:` specifier selects over.
#[derive(Debug, Clone, Default)]
pub struct ProgramInfo {
    pub id: i64,
    /// Stream indices, in the order the program lists them.
    pub streams: Vec<u32>,
}

/// A stream group: the `g:` specifier's target.
#[derive(Debug, Clone, Default)]
pub struct GroupInfo {
    pub id: i64,
    pub streams: Vec<u32>,
}

/// Everything one file offers a specifier to match against.
#[derive(Debug, Clone, Copy, Default)]
pub struct MatchCtx<'a> {
    /// In container order. A specifier's index counts within this order, after
    /// filtering.
    pub streams: &'a [StreamInfo],
    pub programs: &'a [ProgramInfo],
    /// Indexed by position; `g:0` is `groups[0]`, `g:#0` matches by `id`.
    pub groups: &'a [GroupInfo],
}

impl<'a> MatchCtx<'a> {
    /// A context with no programs and no groups — the common case for a plain
    /// media file.
    #[must_use]
    pub const fn streams(streams: &'a [StreamInfo]) -> Self {
        Self {
            streams,
            programs: &[],
            groups: &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_names_are_the_reference_list() {
        let names: Vec<_> = Disposition::ALL.iter().map(|&(_, n)| n).collect();
        assert_eq!(names.len(), 19);
        assert_eq!(names.first(), Some(&"default"));
        assert_eq!(names.last(), Some(&"multilayer"));
        assert_eq!(
            Disposition::by_name("attached_pic"),
            Some(Disposition::ATTACHED_PIC)
        );
        assert_eq!(Disposition::ATTACHED_PIC.bits(), 1 << 10);
        assert_eq!(Disposition::by_name("Default"), None);
    }

    #[test]
    fn empty_disposition_is_contained_by_everything() {
        assert!(Disposition::NONE.contains(Disposition::NONE));
        assert!(Disposition::DEFAULT.contains(Disposition::NONE));
        assert!(!Disposition::NONE.contains(Disposition::DEFAULT));
    }

    #[test]
    fn usable_needs_dimensions_for_video() {
        let mut s = StreamInfo {
            media_type: Some(MediaType::Video),
            codec_known: true,
            ..StreamInfo::default()
        };
        assert!(!s.is_usable());
        s.width = 4;
        s.height = 4;
        assert!(s.is_usable());
    }

    #[test]
    fn tag_lookup_is_case_insensitive_on_the_key() {
        let mut s = StreamInfo::default();
        s.tags.set("PLAIN", "p");
        assert_eq!(s.tag("plain"), Some("p"));
        assert_eq!(s.tag("Plain"), Some("p"));
        assert_eq!(s.tag("other"), None);
    }
}
