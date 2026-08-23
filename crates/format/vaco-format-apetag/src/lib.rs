//! APEv1/APEv2 tag read and write, and `ReplayGain` across all four
//! conventions it and its neighbours use.
//!
//! This is **not** a demuxer and registers no component — `vaco-demux-ape`,
//! `vaco-demux-wv` (`WavPack`), `vaco-demux-mpc` (Musepack), `vaco-demux-tta`
//! and `vaco-demux-mp3` are the eventual callers, the way a container
//! demuxer calls into `vaco-format-riff` or `vaco-format-id3`. None of those
//! demuxers exist yet in this workspace (SH-08/SH-09's brief is explicit
//! that landing the shared helper does not require its callers to exist
//! first), so this crate is exercised here only by its own unit and
//! property tests.
//!
//! # What is in here
//!
//! | Module | Contents |
//! |---|---|
//! | [`tag`] | the APEv1/APEv2 header/footer, item list, parse and serialise |
//! | [`locate`] | finding a tag at the start or end of a file, honouring the `ID3v1` coexistence rule |
//! | [`replaygain`] | `ReplayGain` from Vorbis-comment-shaped text, and from a LAME binary header |
//!
//! # Example
//!
//! ```
//! use vaco_format_apetag::replaygain;
//! use vaco_format_apetag::tag::{ApeItem, ApeTag};
//! use vaco_limits::{Budget, Limits};
//!
//! let tag = ApeTag {
//!     version: 2000,
//!     items: vec![
//!         ApeItem::text("Artist", "Test Artist"),
//!         ApeItem::text("REPLAYGAIN_TRACK_GAIN", "-3.50 dB"),
//!     ],
//! };
//! let bytes = tag.to_bytes()?;
//!
//! let mut budget = Budget::new(Limits::permissive());
//! let parsed = ApeTag::parse(&bytes, &mut budget)?;
//! assert_eq!(parsed.get("artist").unwrap().text_lossy(), "Test Artist");
//!
//! let text: Vec<(&str, &str)> = parsed
//!     .items
//!     .iter()
//!     .map(|i| (i.key.as_str(), i.value.as_slice()))
//!     .map(|(k, v)| (k, std::str::from_utf8(v).unwrap_or_default()))
//!     .collect();
//! let rg = replaygain::from_text_entries(text);
//! assert_eq!(rg.track_gain, Some(-3.50));
//! # Ok::<(), vaco_core::Error>(())
//! ```
//!
//! # Reference behaviour and its limits
//!
//! The tag structure ([`tag`], [`locate`]) is implemented from the published
//! `APEv2` specification (cited in [`tag`]'s docs), not probed against
//! `ffmpeg`: no muxer in the `ffmpeg` 8.1 build available while writing this
//! crate exposes an option to write one (`mp3`, `wv` and `caf` were all
//! checked — see `docs/format/vaco-format-apetag.md` for the exact commands
//! tried), which is itself the finding plan 13 §1b asks to be recorded
//! rather than papered over. The LAME-header `ReplayGain` fields in
//! [`replaygain`] are transcribed from the equivalent published LAME Tag
//! specification and are unverified against a live encoder for the same
//! reason — flagged again at [`replaygain::decode_gain_field`], not silently
//! assumed correct.

#![forbid(unsafe_code)]

pub mod locate;
pub mod replaygain;
pub mod tag;

pub use locate::{Found, find_trailing, parse_trailing, read_trailing};
pub use replaygain::ReplayGain;
pub use tag::{ApeItem, ApeTag, ItemKind, TagFlags};
