//! Foundation types shared by every Vaco crate.
//!
//! This crate depends on nothing but `std` and sits at layer 0. Everything here
//! is either a vocabulary type that crosses crate boundaries or an error.

pub mod error;
pub mod rational;
pub mod time;

pub use error::{Error, Result};
pub use rational::Rational;
pub use time::{Duration, TimeBase, Timestamp};

/// The kind of data a stream, codec or filter pad carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MediaType {
    Video,
    Audio,
    Subtitle,
    Data,
    Attachment,
}

impl MediaType {
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
}
