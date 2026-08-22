//! `ID3v1`/`ID3v1.1` and `ID3v2` (2.2/2.3/2.4) tag parsing.
//!
//! This is **not** a demuxer and registers no component — `vaco-demux-mp3`
//! (and any other raw-stream demuxer that carries ID3 tags) is what calls
//! into this crate, the way a container demuxer calls into
//! `vaco-format-riff` or `vaco-format-isom`.
//!
//! # What is in here
//!
//! | Module | Contents |
//! |---|---|
//! | [`header`] | the ten-byte `ID3v2` header and footer, and their flags |
//! | [`synchsafe`] | the 7-bit-per-byte size encoding, and where it does *not* apply |
//! | [`unsync`] | undoing `ID3v2` unsynchronisation |
//! | [`frame_header`] | per-version frame headers (v2.2's 6-byte form; v2.3/v2.4's 10-byte form) |
//! | [`encoding`] | the four text encodings and null-terminated string reading |
//! | [`frames`] | frame content decoding and the frame-ID → metadata-key table |
//! | [`tag`] | assembling a whole tag: header, extended header, unsynchronisation, the frame walk |
//! | [`id3v1`] | the 128-byte `ID3v1`/`ID3v1.1` tag, and its genre table |
//! | [`skip`] | skipping a tag at the start of a stream, for probing past it |
//!
//! # Example
//!
//! ```
//! use vaco_format_id3::tag::Id3v2Tag;
//! use vaco_limits::{Budget, Limits};
//!
//! # let mut data = b"ID3".to_vec();
//! # data.extend_from_slice(&[3, 0, 0]);
//! # let mut frame = b"TIT2".to_vec();
//! # frame.extend_from_slice(&13u32.to_be_bytes());
//! # frame.extend_from_slice(&[0, 0]);
//! # frame.push(0x00);
//! # frame.extend_from_slice(b"Hello World");
//! # let size = frame.len() as u32;
//! # data.extend_from_slice(&[(size>>21) as u8 &0x7f, (size>>14) as u8&0x7f, (size>>7) as u8&0x7f, size as u8&0x7f]);
//! # data.extend_from_slice(&frame);
//! let mut budget = Budget::new(Limits::permissive());
//! let tag = Id3v2Tag::parse(&data, &mut budget)?;
//! assert_eq!(tag.entries, vec![("title".to_string(), "Hello World".to_string())]);
//! # Ok::<(), vaco_core::Error>(())
//! ```
//!
//! # Reference behaviour and its limits
//!
//! Every frame-ID → metadata-key mapping in [`frames`], and the entire
//! 192-entry `ID3v1` genre table in [`id3v1`], was obtained by running
//! `ffmpeg`/`ffprobe` 8.1 and reading back its own `TAG:<key>` output — see
//! each module's docs for the exact commands, repeated in
//! `docs/format/vaco-format-id3.md` so they can be re-derived when the
//! pinned reference version moves (plan 13 §1b). The one exception is the
//! `ID3v2.3`/2.4 extended header's byte layout ([`tag`]'s
//! `extended_header_len`), which is read from the published specification
//! rather than probed, because `ffmpeg` does not write one under any option
//! found — flagged there, not silently assumed correct.

#![forbid(unsafe_code)]

pub mod encoding;
pub mod frame_header;
pub mod frames;
pub mod header;
pub mod id3v1;
mod id3v1_genres;
pub mod skip;
pub mod synchsafe;
pub mod tag;
pub mod unsync;

pub use encoding::Encoding;
pub use frame_header::{FrameHeaderV2, FrameHeaderV34, Id3FrameFlags};
pub use frames::{Frame, Picture};
pub use header::{Flags, Id3v2Footer, Id3v2Header};
pub use id3v1::Id3v1Tag;
pub use tag::Id3v2Tag;
