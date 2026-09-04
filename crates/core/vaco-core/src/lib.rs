//! Foundation types shared by every Vaco crate.
//!
//! This crate depends on nothing but `std`, `thiserror` and `tracing`, and sits
//! at layer 0. Everything here is either a vocabulary type that crosses crate
//! boundaries or an error.
//!
//! # What lives here
//!
//! | Module | Contents |
//! |---|---|
//! | [`error`] | the closed [`Error`] taxonomy every crate returns |
//! | [`rational`] | exact `i32/i32` [`Rational`] arithmetic — time bases, frame rates, aspect ratios |
//! | [`time`] | [`Timestamp`], [`TimeBase`], [`Duration`] and exact rescaling |
//! | [`dict`] | the insertion-ordered [`Dict`] used for metadata and option maps |
//! | [`escape`] | the shared quoting/escaping grammar of the option and filtergraph layers |
//! | [`parse`] | the CLI value grammars: image size, video rate, duration, colour |
//!
//! # The two rules everything else follows from
//!
//! **Exactness.** No timestamp, time base or rate ever passes through `f64`.
//! Rational arithmetic runs in `i128` and reduces; rescaling multiplies in
//! `i128` and divides once, with the rounding mode named by the caller. The
//! `to_f64` methods exist for display and heuristics and say so.
//!
//! **No panics.** This crate is on the path of every byte of untrusted input in
//! the project, so a malformed value is an `Option`/`Err`, an unrepresentable
//! result is `None` or a saturation, and `0/0` and `1/0` are ordinary inputs
//! that every operation handles.
#![forbid(unsafe_code)]

pub mod cancel;
pub mod dict;
pub mod disposition;
pub mod error;
pub mod escape;
pub mod parse;
pub mod rational;
pub mod time;

pub use cancel::CancelToken;
pub use dict::{Dict, DictFlags};
pub use disposition::Disposition;
pub use error::{Error, Result};
pub use escape::EscapeError;
pub use parse::Rgba;
pub use rational::Rational;
pub use time::{Duration, ExactDuration, Rounding, TimeBase, Timestamp, rescale_rnd};

/// The kind of data a stream, codec or filter pad carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MediaType {
    /// Coded or decoded pictures.
    Video,
    /// Coded or decoded audio.
    Audio,
    /// Text, bitmap or teletext subtitles.
    Subtitle,
    /// Timed data with no decoder — timecode tracks, private streams.
    Data,
    /// A file carried inside the container: a font, a cover image.
    Attachment,
}

impl MediaType {
    /// Every variant, in specifier order.
    pub const ALL: [Self; 5] = [
        Self::Video,
        Self::Audio,
        Self::Subtitle,
        Self::Data,
        Self::Attachment,
    ];

    /// The single-letter code used by CLI stream specifiers (`-c:v`, `-map 0:a`).
    #[must_use]
    pub const fn specifier_char(self) -> char {
        match self {
            Self::Video => 'v',
            Self::Audio => 'a',
            Self::Subtitle => 's',
            Self::Data => 'd',
            Self::Attachment => 't',
        }
    }

    /// The long name, as printed by `-show_streams` and friends.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Subtitle => "subtitle",
            Self::Data => "data",
            Self::Attachment => "attachment",
        }
    }

    /// Accepts either spelling: the long name or the specifier letter.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|t| s == t.name() || (s.len() == 1 && s.starts_with(t.specifier_char())))
    }
}

impl core::fmt::Display for MediaType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}
