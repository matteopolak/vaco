//! `TextRenderer`: the one text-shaping-and-rasterisation path every filter
//! that draws glyphs sits on (plan 16 SS6.1).
//!
//! # The stack
//!
//! | Upstream role | Ours |
//! |---|---|
//! | font discovery (libfontconfig) | `fontdb`, via `cosmic_text::FontSystem`, plus [`alias`]'s own generic-family table |
//! | Unicode bidi (libfribidi) | `unicode-bidi`, via `cosmic_text` |
//! | shaping (libharfbuzz) | `rustybuzz`, via `cosmic_text` |
//! | outline + rasterisation (libfreetype) | `swash`, via `cosmic_text::SwashCache` |
//!
//! All four are pulled in through the single `cosmic-text` dependency
//! (already reviewed and declared in the workspace manifest's "text shaping
//! and fonts" section) rather than named directly. No `FreeType` dependency
//! exists anywhere in this crate's tree, so the FTL attribution obligation
//! never arises.
//!
//! # What is here versus what `cosmic-text` already does
//!
//! `cosmic_text::Buffer` already shapes and lays out; `SwashCache` already
//! rasterises and caches glyph images by `(font, glyph, size, subpixel
//! position)` — that cache-by-`CacheKey` behaviour *is* the glyph cache this
//! crate's `TextRenderer` needs, not something reimplemented here. What
//! this crate adds:
//!
//! - [`alias`]: the generic-family fallback table fontconfig would otherwise
//!   provide, plus embedded-font loading (Matroska attachments) and
//!   `-font_dirs`.
//! - [`layout::TextRenderer`]: a shaped-run LRU on top of `Buffer` (a
//!   `drawtext` with `%{pts}` reshapes an unchanged-looking string every
//!   frame otherwise — SS6.1's own stated reason a cache is not optional),
//!   plus a bound on `SwashCache`'s own unbounded growth.
//! - [`mask::AlphaMask`]: a coverage buffer independent of any one colour, so
//!   a border or shadow can be produced by *operating on the mask*
//!   (dilate/blur/offset) rather than re-rasterising — needed by `drawtext`'s
//!   `borderw`/`shadowx`/`shadowy` and ASS's `\bord`/`\shad`/`\blur`/`\be`.
//! - [`mask::composite`]: tinting a mask and alpha-compositing it into a real
//!   [`vaco_frame::Frame`], subsampled-chroma and high-bit-depth aware, built
//!   on `vaco-filter-draw`'s already-measured `sample`/`solid`/`rect`
//!   primitives (see that module's doc for why the blend formula itself is
//!   reproduced here rather than reached through that crate).
//! - [`drawtext`]: the filter itself.
#![forbid(unsafe_code)]

pub mod alias;
pub mod drawtext;
pub mod expand;
pub mod layout;
pub mod mask;
pub mod registry;
pub mod style;

pub use layout::{Layout, TextRenderer};
pub use mask::AlphaMask;
pub use registry::TextRegistry;
pub use style::{Anchor, TextStyle, Wrap};
