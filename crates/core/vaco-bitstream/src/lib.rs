//! Bit and byte readers/writers, Exp-Golomb, start-code scanning and RBSP
//! handling.
//!
//! Every codec parser and every container that carries codec-level syntax sits
//! on this crate, so its per-read cost is multiplied by roughly the number of
//! syntax elements in the world. That is the whole design constraint.
//!
//! # The three ideas
//!
//! **Sticky overrun, not `Result` per read.** [`BitReader`] reads return values,
//! not `Result`. Past the end they return zeros — deterministically — and the
//! reader remembers. The parser checks once per syntax structure with
//! [`BitReader::check`]. A 40-line SPS parser stays 40 lines, inlining is not
//! blocked by an error path, and the guarantee that matters survives intact: a
//! truncated or malformed bitstream can never panic and never reads out of
//! bounds.
//!
//! **Overrun is derived, not flagged.** `bit_pos() > logical_bits` is computable
//! from state the reader keeps anyway, so the sticky model costs *nothing* in
//! the read path — not even a predictable branch. A separate flag covers only
//! what position cannot express: a malformed Exp-Golomb prefix, an out-of-range
//! width.
//!
//! **A padded body and a checked tail.** The refill loads eight bytes at a time,
//! so one bounds comparison covers the four to eight syntax elements that refill
//! feeds; reads out of the cache are register operations with no check at all.
//! A [`Padded`] buffer carries 64 zero bytes past its logical end, which pushes
//! the byte-at-a-time tail path 56 bytes beyond where the data stops — a header
//! parser never reaches it. This is `FFmpeg`'s over-read trick with the padding
//! moved *inside* the allocation, where it is memory we own rather than slop we
//! hope is mapped.
//!
//! # What it costs
//!
//! Measured with `cargo bench -p vaco-bitstream`, against a reader that
//! bounds-checks every individual read. See `docs/core/vaco-bitstream.md` for
//! the numbers and the interpretation.
//!
//! # Example
//!
//! ```
//! use vaco_bitstream::{BitReader, GolombRead};
//!
//! // A fragment shaped like an H.264 SPS prologue.
//! let sps = [0x42, 0xC0, 0x1E, 0xD9, 0x00, 0x50, 0x05, 0xBB];
//! let mut r = BitReader::new(&sps);
//! let profile_idc = r.get(8);
//! let constraint_flags = r.get(8);
//! let level_idc = r.get(8);
//! let sps_id = r.ue();
//! assert_eq!((profile_idc, constraint_flags, level_idc), (0x42, 0xC0, 0x1E));
//! assert_eq!(sps_id, 0);
//! r.check()?;                  // one check for the whole structure
//! # Ok::<(), vaco_bitstream::BitstreamError>(())
//! ```
//!
//! # Dependencies
//!
//! `vaco-core` for the error taxonomy, `vaco-limits` for [`BitWriter`]'s budgeted
//! capacity, `thiserror`. No external runtime dependencies: the start-code
//! scanner here is the scalar reference that `vaco-simd`'s vectorised `scan`
//! will have to agree with.

#![forbid(unsafe_code)]

pub mod annexb;
pub mod avcc;
mod bytes;
mod error;
mod golomb;
mod padded;
mod reader;
mod writer;

pub use bytes::ByteReader;
pub use error::{BitstreamError, Result};
pub use golomb::GolombRead;
pub use padded::Padded;
pub use reader::{BitReader, Mark};
pub use writer::{BitWriter, RbspWriter};
