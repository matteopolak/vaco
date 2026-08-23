//! The facts a stream specifier is matched against.
//!
//! This crate owns the *grammar*, not the container. So it defines the smallest
//! description of a stream that the grammar needs — index, id, media type,
//! disposition, tags, and enough of the codec parameters to answer `u` (usable)
//! — and demuxers fill it in. Nothing here knows what a demuxer is.

use vaco_core::{Dict, DictFlags, MediaType};

/// Re-exported from `vaco-core`, where it now lives.
///
/// This crate and `vaco-format-core` each defined one, with the same nineteen
/// flags at the same bits — and they disagreed about **case**, which is how one
/// duplication turned into one divergence. See [`vaco_core::disposition`].
pub use vaco_core::Disposition;

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
