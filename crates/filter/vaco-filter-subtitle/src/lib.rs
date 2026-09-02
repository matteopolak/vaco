//! Subtitle rendering filters (plan 16 SS6.3, GitHub #486/FT-5.1,
//! #487/#488/FT-5.2/5.3): `ass` and `subtitles`, both self-contained
//! (`filename=`, no second input pad) like the reference's own filters —
//! there is no `vaco-filter-movie`/`MediaOpener` in this tree yet, and a
//! subtitle file is small enough to read and parse whole at construction,
//! so building on that missing piece was never actually necessary here.
//!
//! - [`bitmap::composite_bitmap`]: DVB/`VobSub`/PGS-shaped palette bitmaps
//!   (#486's bitmap half) — a positioned alpha-composite, no typesetting.
//! - [`text::composite_simple_text`]: plain multi-line text, bottom-
//!   centred with a legibility outline (#486's text half) — what
//!   `subtitles` falls back to for a `.srt` file.
//! - [`ass_filter`]: the `ass` filter, and `subtitles`' own path for a
//!   `.ass`/`.ssa` file — drives `vaco-ass`'s parser and tag interpreter
//!   through `vaco-filter-text`'s `TextRenderer`. See that module's own
//!   doc for the one real, stated rendering simplification (one style per
//!   event line, not per run).
//! - [`subtitles`]: `WebVTT`/`MicroDVD`/SAMI are not implemented — a named
//!   gap, not a silent one.
#![forbid(unsafe_code)]

pub mod ass_filter;
pub mod bitmap;
pub mod registry;
pub mod subtitles;
pub mod text;

pub use bitmap::composite_bitmap;
pub use registry::SubtitleRegistry;
pub use text::{SimpleTextStyle, composite_simple_text};
