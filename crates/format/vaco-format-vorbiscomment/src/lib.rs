//! Vorbis comment (vendor + tag list) and FLAC picture block parsing.
//!
//! Vorbis, FLAC and Opus all carry the same vendor-plus-tag-list metadata
//! shape (Xiph Vorbis I §5.2; FLAC's `VORBIS_COMMENT` block states it is
//! exactly that shape "without the framing bit"). This crate is the one
//! place `#274`'s Vorbis/FLAC header work and `#540`'s own scope overlap —
//! deliberately: parsing the same bit layout twice under two names is
//! exactly what D19 forbids, so `vaco-parse-audio-misc`'s Vorbis and FLAC
//! `Parser`s depend on this crate for tag parsing rather than re-deriving it.
//!
//! # What is in here
//!
//! | Module | Contents |
//! |---|---|
//! | [`comment`] | [`comment::VorbisComment`] — the vendor+tag-list reader, native-header and raw-block forms |
//! | [`picture`] | [`picture::Picture`] — FLAC's `METADATA_BLOCK_PICTURE` |
//! | [`conv`] | [`conv::TABLE`] — the measured Vorbis-comment field-name renames |
//!
//! # What is deliberately not shared
//!
//! `vaco-parse-opus::comment::CommentHeader` parses the identical wire shape
//! for `OpusTags` and predates this crate. It is not refactored to depend on
//! this one: that crate is not this work's to edit, and a parse-only crate
//! quietly growing a new dependency on a metadata crate is exactly the kind
//! of change that belongs in that crate's own commit, made by whoever owns
//! it. Recorded as a known, accepted duplication rather than worked around.

#![forbid(unsafe_code)]

pub mod comment;
pub mod conv;
pub mod picture;

pub use comment::{CommentIter, VorbisComment, VORBIS_MAGIC};
pub use picture::{Picture, PictureType};
