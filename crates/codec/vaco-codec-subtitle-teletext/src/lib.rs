//! EBU/ETSI Teletext decode (EN 300 706): Hamming 8/4 and 24/18 forward
//! error correction, odd-parity Latin text, page header parsing and
//! Level 1 page assembly into a 40-column by 25-row character grid.
//!
//! # What it is
//!
//! [`decoder::TeletextDecoder`] consumes the EN 300 472 46-byte data units
//! a DVB teletext elementary stream is carried in and assembles
//! [`page::Page`]s: an eight-magazine state machine over
//! [`hamming::decode8`]/[`hamming::decode24`] (§8.2/§8.3), odd-parity text
//! ([`parity::decode`], §8.1) and the Latin G0 table
//! ([`charset::latin_g0`], §15.6). See each module's docs for the specific
//! clause it implements.
//!
//! # No registry-to-decoder path — by design, not oversight
//!
//! This crate is **not** a `vaco_codec_core::Decoder`. `CodecId::DvbTeletext`
//! already exists (`vaco-codec-core`) and `MediaType::Subtitle` already
//! exists, but `vaco_frame::FrameData` has exactly two variants, `Video` and
//! `Audio` — there is no way to hand a decoded page grid to the rest of the
//! pipeline through the shape every other decoder returns through. Adding a
//! `Subtitle` variant is a change to `vaco-frame`, a crate this crate does
//! not own; a `vaco-component.toml` fragment naming a `kind = "decoder"`
//! `ctor` here would either lie about what it produces or fail the
//! registry's own descriptor-resolution check. So [`decoder::TeletextDecoder`]
//! is a plain library type with its own output ([`page::Page`]), callable
//! directly, and this crate registers nothing.
//!
//! # Level 1.5 coverage
//!
//! Implemented: Hamming 8/4 and 24/18 decode, odd-parity Latin text, page
//! header parsing (page number, subcode, all eleven `C4`-`C14` control
//! bits), and Level 1 page-grid assembly including spacing attributes
//! (colours, flash, conceal, box, double height/width/size, hold mosaics)
//! and the English national-option G0 substitution (Table 36).
//!
//! Not implemented — this crate's Level 1.5 gap, stated plainly rather than
//! implied: the G0/G2 character-set re-designation packets (X/28, M/29),
//! the `ESC` second-G0-set toggle, G2 supplementary-character access, and
//! X/26 composite-character overwriting. Packets X/26, X/27 and X/28 are
//! still Hamming 24/18-decoded (so a malformed one is detected rather than
//! misread as page text) but their addressing semantics are not applied —
//! see [`decoder`]'s module docs. National-option sub-sets other than
//! English (German, French, Italian, ...) also render with English's
//! glyphs at the thirteen reserved code points — see [`charset`]'s module
//! docs.

#![forbid(unsafe_code)]

pub mod charset;
pub mod decoder;
pub mod hamming;
pub mod packet;
pub mod page;
pub mod parity;

pub use decoder::{PageEvent, TeletextDecoder};
pub use page::{Cell, Color, ControlBits, Glyph, Page, Row};
