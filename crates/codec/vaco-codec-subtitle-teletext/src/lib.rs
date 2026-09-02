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
//! ([`parity::decode`], §8.1), the Latin G0 table
//! ([`charset::latin_g0`], §15.6) and X/26 composite characters
//! ([`x26::apply`], §12.3). See each module's docs for the specific clause
//! it implements. [`registry::TeletextSubtitleDecoder`] is the
//! `vaco_codec_core::Decoder` face over the same state machine, reachable
//! from `vaco-registry` as the `teletext` decoder for `CodecId::
//! DvbTeletext` — see that module's docs for the `SubtitleContent::Text`
//! rendering it produces and what a page's spacing attributes cannot
//! survive translation into plain text.
//!
//! # Level 1.5 coverage
//!
//! Implemented: Hamming 8/4 and 24/18 decode, odd-parity Latin text, page
//! header parsing (page number, subcode, all eleven `C4`-`C14` control
//! bits), Level 1 page-grid assembly including spacing attributes (colours,
//! flash, conceal, box, double height/width/size, hold mosaics), the
//! national-option G0 substitution for the eight sub-sets a page header's
//! `C12`-`C14` bits can actually select (English, German, Italian, French,
//! Portuguese/Spanish, Czech/Slovak — see [`charset`]'s module docs for
//! which two of the eight fall back to English and why), and X/26 composite
//! characters: diacritical-mark composition over a G0 base letter (the
//! feature EN 300 706 §15.1 names as what Level 1.5 *is* — "a few
//! characters from the G2 supplementary set ... plus a few G0 characters
//! with diacritical marks") plus direct G2 SPACE.
//!
//! Not implemented — this crate's remaining Level 1.5 gap, stated plainly
//! rather than implied: the G0/G2 character-set re-designation packets
//! (X/28, M/29) and the `ESC` second-G0-set toggle. Per §15.2/§15.3 these
//! are explicitly *not* needed by a Level 1 or 1.5 decoder ("this
//! definition will be ignored by existing Level 1 and 1.5 decoders" /
//! "unlikely to be interpreted by Level 1 or 1.5 decoders") — they exist so
//! a Level 2.5/3.5 decoder stays compatible with a Level 1.5 transmission,
//! which this crate is not. The other ~80 non-diacritical entries of the G2
//! supplementary set (Table 37, [`x26::g2_char`]'s stated gap) and X/26's
//! non-text triplet modes (colours, DRCS, font style, PDC, object linking —
//! see [`x26`]'s module docs) are also not implemented, since none of them
//! changes what text a Level 1.5 page shows.

#![forbid(unsafe_code)]

pub mod charset;
pub mod decoder;
pub mod hamming;
pub mod packet;
pub mod page;
pub mod parity;
pub mod registry;
mod x26;

pub use decoder::{PageEvent, TeletextDecoder};
pub use page::{Cell, Color, ControlBits, Glyph, Page, Row};
pub use registry::TELETEXT_DECODER;
