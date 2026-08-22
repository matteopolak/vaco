//! CABAC — context-adaptive binary arithmetic coding, ITU-T H.264 clause 9.3.
//!
//! # What this crate is
//!
//! The arithmetic coder that H.264, HEVC and VVC are built on, and nothing else.
//! It knows about bins, contexts and an arithmetic interval; it does not know
//! what a macroblock, a coding unit or a motion vector is, and that separation
//! is what lets one engine serve every codec in the family.
//!
//! | Module | Contents |
//! |---|---|
//! | `tables` | Tables 9-44 and 9-45, and the two tables derived from them at compile time |
//! | `context` | [`ContextModel`], the two initialisation formulas, the context-set helpers |
//! | `decode` | [`CabacDecoder`] — `DecodeDecision`, `DecodeBypass`, `DecodeTerminate`, the binarizations |
//! | `encode` | [`CabacEncoder`] — the clause 9.3.4 counterparts |
//!
//! # What is deliberately *not* here
//!
//! **Per-syntax-element context initialisation values.** H.264's Tables 9-12 to
//! 9-33 and H.265's equivalents are indexed by `ctxIdx` assignments that only a
//! specific codec's slice syntax defines, and holding them here would make this
//! crate know what a macroblock is — which `10-architecture.md` §1.5 forbids of
//! a shared layer. Both *derivation formulas* are here
//! ([`ContextModel::init_h264`], [`ContextModel::init_hevc`]) together with the
//! loops that apply them; the values belong to `vaco-codec-h264` and
//! `vaco-codec-hevc`.
//!
//! **`ctxIdxInc` derivation.** Which context a given bin uses depends on
//! neighbouring blocks, and neighbour availability is a codec concept.
//!
//! # Three design decisions worth reading before the code
//!
//! ### 1. The inner loop is the specification's shape, because it measured fastest
//!
//! Two plausible optimisations — decoding a bin without the MPS/LPS branch, and
//! renormalising by a computed width instead of one bit at a time — were
//! implemented first and are both **slower**, by 35% and 45% respectively, on a
//! skewed corpus *and* on a high-entropy one. `benches/cabac.rs` keeps all four
//! combinations and [`decode`] explains why. The engine below is written the way
//! clause 9.3.3.2 writes it, and that is a measurement rather than a
//! preference.
//!
//! The one thing kept is the **one-byte context**: packing `pStateIdx` and
//! `valMPS` as `(pStateIdx << 1) | valMPS` — which is what clause 9.3.1.1's
//! `preCtxState` derivation already produces — lets
//! `if (pStateIdx == 0) valMPS = 1 - valMPS` be folded into the transition table
//! at compile time. That removes a *conditional* without removing a *branch the
//! processor was predicting well*, which is exactly the distinction the
//! measurements turned on.
//!
//! ### 2. `ivlOffset < ivlCurrRange` is enforced, not assumed
//!
//! The specification states it as a constraint on conforming bitstreams. This
//! implementation enforces it at initialisation, because it is also the bound
//! that stops `offset` from doubling away on malformed input — which, with the
//! overflow checks the fuzzing profile turns on, would be a panic. See
//! [`decode`] for the derivation. It is asserted after every operation by both
//! the property tests and the fuzz target.
//!
//! ### 3. Every loop over input has a ceiling
//!
//! `DecodeBypass` runs of ones, truncated-unary prefixes and `EGk` prefixes are
//! all terminated by the *bitstream*, which means an adversarial one terminates
//! none of them. Each has an explicit cap that sets
//! [`CabacDecoder::malformed`] rather than looping. Nothing in this crate can
//! hang on any input.
//!
//! # Why there is an encoder
//!
//! Not because Vaco ships an H.264 or HEVC encoder — D9 puts both outside the
//! default build. It is here because **a CABAC decoder cannot be tested against
//! hand-written bit patterns**: the standard publishes no worked bitstream, and
//! a bin sequence reaches bytes only through the whole adaptive state machine.
//! An encoder written from clause 9.3.4 turns that into a property — encode
//! arbitrary bins with arbitrary contexts, decode, get the same bins — which
//! exercises every state transition, every renormalisation width and the carry
//! propagation at once.
//!
//! # Example
//!
//! ```
//! use vaco_codec_cabac::{CabacDecoder, CabacEncoder, ContextModel};
//!
//! let bins = [1u32, 0, 0, 1, 1, 1, 0, 1, 0, 0, 1, 1];
//!
//! let mut enc = CabacEncoder::new();
//! let mut ctx = ContextModel::init_h264(23, 33, 30);
//! for &b in &bins {
//!     enc.encode_decision(&mut ctx, b);
//! }
//! enc.encode_terminate(1);
//! let bytes = enc.finish();
//!
//! let mut dec = CabacDecoder::new(&bytes);
//! let mut ctx = ContextModel::init_h264(23, 33, 30);
//! for &b in &bins {
//!     assert_eq!(dec.decode_decision(&mut ctx), b);
//! }
//! assert_eq!(dec.decode_terminate(), 1);
//! ```
//!
//! # Specification
//!
//! ITU-T H.264 (ISO/IEC 14496-10) clause 9.3: 9.3.1.1 context initialisation,
//! 9.3.1.2 engine initialisation, 9.3.3.1 binarizations, 9.3.3.2 the decoding
//! process (Tables 9-44 and 9-45), 9.3.4 the encoding process. ITU-T H.265
//! (ISO/IEC 23008-2) clause 9.3.2.2 for the HEVC context initialisation
//! formula; its engine and tables are identical to H.264's. Nothing here was
//! taken from any implementation.
//!
//! # Dependencies
//!
//! `vaco-bitstream` for the bit reader the renormalisation loop pulls from.
//! `vaco-core` for the shared error taxonomy. No external runtime dependencies.

#![forbid(unsafe_code)]

mod context;
mod decode;
mod encode;
pub mod tables;

pub use context::{
    ContextInit, ContextModel, init_contexts, init_contexts_hevc, is_terminal_state,
};
pub use decode::CabacDecoder;
pub use encode::{CabacEncoder, DEFAULT_MAX_BYTES};

// Re-exported so a caller can build a padded buffer without also naming
// `vaco-bitstream` directly.
pub use vaco_bitstream::Padded;
