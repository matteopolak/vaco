//! The shared text-subtitle model: one cue representation, the per-format
//! timestamp grammars, and the byte-level encoding sniff every demuxer in
//! `vaco-subtitle-text` needs before it can find a single cue.
//!
//! # Why this crate exists separately from the demuxers/muxers
//!
//! Sixteen-plus text subtitle formats share exactly three hard problems —
//! representing a cue, parsing/printing its timestamp, and deciding what bytes
//! a line of "text" actually is — and share almost nothing else (the grammars
//! around those three problems are all different, on purpose, which is why
//! they are sixteen formats and not one). This crate is the shared 20%;
//! `vaco-subtitle-text` is the sixteen separate 80%s.
//!
//! # What is in here
//!
//! | Module | Contents |
//! |---|---|
//! | [`cue`] | [`Cue`], the one in-memory shape every format demuxes into and every muxer serialises from |
//! | [`time`] | parse/format pairs for every timestamp grammar in the format family, plus the frame<->time conversion `MicroDVD` needs |
//! | [`encoding`] | BOM sniffing and the UTF-16-to-UTF-8 conversion the reference performs at demux time |
//! | [`text`] | byte-level line splitting that does not assume the input is valid UTF-8 |
//!
//! # The one idea worth reading first
//!
//! **A cue's text is bytes, not a `String`.** Measured against the reference
//! (`ffprobe -show_packets -show_data`): a `.srt` with a raw, unlabelled
//! `0xE9` byte in its text demuxes to a packet containing that exact byte,
//! unchanged, no substitution and no rejection. `String::from_utf8` would
//! reject it and `from_utf8_lossy` would corrupt it, so [`Cue::text`] is a
//! `Vec<u8>` and every parser in `vaco-subtitle-text` finds cue boundaries by
//! scanning bytes, never by requiring the payload itself to be valid UTF-8.
//! The *structural* parts of every format (timestamps, counters, brace tags)
//! are pure ASCII, so a lossy conversion is safe to use for finding them —
//! just never for the text passed through as a packet payload.
//!
//! See `docs/format/vaco-format-subtitle.md` for the measured behaviour this
//! crate encodes and the table of every timestamp grammar it implements.

#![forbid(unsafe_code)]

pub mod cue;
pub mod encoding;
pub mod text;
pub mod time;

pub use cue::Cue;
pub use encoding::{DetectedEncoding, decode_to_utf8_bytes};
