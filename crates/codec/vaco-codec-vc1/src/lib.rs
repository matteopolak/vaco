//! VC-1 / WMV3 decode (SMPTE ST 421:2013, freely published by SMPTE from
//! <https://pub.smpte.org/pub/st421/st0421-2013.pdf> — Tier A per D7, a
//! published Standard) plus SMPTE RP 227 ("VC-1 Bitstream Transport
//! Encodings", <https://multimedia.cx/mirror/rp227.pdf>) for the Annex J/L
//! decoder-initialization metadata layout.
//!
//! `Vaco-Spec-Ref: smpte-st421-2013`
//!
//! # Legal status: encumbered
//!
//! VC-1 was absent from `planning/research/07-legal-patents-licensing.md`
//! entirely before this crate. It is patent-encumbered (the format was
//! developed by Microsoft and later placed under an MPEG-LA-administered
//! pool of contributor patents covering VC-1 encode/decode). No ruling on
//! this project's own exposure has been made. Per D4, this crate is
//! registered `encumbered = true` / `default = false` — the same gate
//! `vaco-codec-h264` and `vaco-codec-aac` use — pending an owner decision,
//! not shipped green by default. See `planning/research/07-legal-patents-licensing.md`
//! SS5.1's new VC-1 row for what was recorded and what was not decided.
//!
//! # Scope: Simple/Main profile, progressive I-frame only
//!
//! VC-1's full syntax (Advanced profile, interlace coding, P/B inter
//! prediction with quarter-pel motion compensation, in-loop deblocking,
//! overlap smoothing, range reduction, multi-resolution up-sampling) is
//! comparable in scope to H.264 baseline plus its own transform family, and
//! does not fit in one breadth pass. What is implemented, and genuinely
//! verified against a real bitstream (see "Verification" below):
//!
//! - Decoder-initialization metadata (`STRUCT_C`, Annex J.2/Table 263) via
//!   [`Decoder::set_extradata`], plus this crate's own documented extension
//!   for width/height (see [`header`]'s module doc — real ASF/AVI containers
//!   that hand a decoder only the bare 4-byte `STRUCT_C` cannot supply
//!   dimensions through today's `Decoder` interface at all; that is a
//!   genuine, separate gap in the container-to-decoder plumbing, not
//!   something this crate can fix from inside a codec crate).
//! - Progressive I-picture header (SS7.1.1, Table 16) for Simple/Main
//!   profile.
//! - Macroblock/block layer decode for intra macroblocks only (Table 27):
//!   `CBPCY` with its neighbour-predicted decode (SS8.1.2.1), `ACPRED`,
//!   DC-differential decode with both DC table families (SS8.1.3.1, SS11.7),
//!   AC run/level/escape decode (SS8.1.3.4/8.1.3.5, all three escape modes)
//!   for the **High Rate** intra/inter coding sets only (SS11.8.6/11.8.7—
//!   the pair a `PQINDEX <= 8` picture selects at coding-set index 0, and
//!   the pair this crate's own real fixture uses; see "What is cut").
//! - The exact Annex A 8x8 integer inverse transform.
//! - Uniform and non-uniform AC dequantization (SS8.1.3.8), constant-`MQUANT`
//!   DC dequantization (SS8.1.3.3).
//!
//! # What is cut
//!
//! - **Only two of the eight AC coding sets are transcribed**: High Rate
//!   Intra/Inter (SS11.8.6/11.8.7). The other six (High/Mid/Low Motion,
//!   Mid/Low Rate) are large VLC table sets (SS11.8.1-11.8.5) this pass did
//!   not have the budget to transcribe and verify to the tier-3 standard
//!   this project holds hand-transcribed tables to. A picture whose
//!   `TRANSACFRM`/`TRANSACFRM2` select an untranscribed set returns
//!   [`vaco_core::Error::Unsupported`] by name rather than guessing — no
//!   fabricated table, matching this project's own G.722/G.726/DFPWM
//!   precedent for "measured but not transcribed this pass".
//! - **`OVERLAP == 1` is refused** (`Error::Unsupported`): SS8.1.3.10 states
//!   plainly that for Simple/Main I frames the final `+128` DC-offset step
//!   is skipped *unless* overlap filtering is used — i.e. the two are one
//!   coupled reconstruction rule, not two independent filters. Implementing
//!   the `+128` path without also implementing overlap smoothing (SS8.5.1)
//!   would silently offset every pixel by a constant this crate cannot yet
//!   justify. The `OVERLAP == 0` path (this crate's own real fixture) is
//!   fully implemented and needs no smoothing at all.
//! - **In-loop deblocking (SS8.6) is not implemented.** `LOOPFILTER == 1` is
//!   refused for the same reason as `OVERLAP` — a real, measured effect on
//!   output pixels this crate cannot silently omit.
//! - **`MULTIRES`/`RESPIC != 0` (down-sampled I-frame decode + up-sampling,
//!   Annex B) is refused.** Only full-resolution (`RESPIC == 0`) frames
//!   decode.
//! - **`RANGERED`, non-implicit `QUANTIZER` values, `HALFQP` combined with
//!   `MQUANT` from `VOPDQUANT`**: `VOPDQUANT` (per-macroblock quantizer
//!   variation) never appears in Table 27's own I/BI Simple/Main macroblock
//!   syntax at all, so `MQUANT` is constant (`== PQUANT`) for the whole
//!   picture in this crate's scope by construction, not by omission.
//! - **P, B, BI pictures, interlace, and Advanced profile are all refused**
//!   outright.
//!
//! Every refusal above is a real `Error::Unsupported` return, not a wrong
//! decode — this crate never fabricates pixels for a picture shape it has
//! not implemented.
//!
//! # Verification
//!
//! Against a real Main-profile RCV (SMPTE Annex L serialization) fixture
//! fetched from `FFmpeg`'s own public FATE sample corpus
//! (`fate-suite.ffmpeg.org/vc1/SMM0015.rcv`, 720x576, `PROFILE=4` (Main),
//! `OVERLAP=0`, `LOOPFILTER=0`, `RESPIC=0`, `PQINDEX=2` (Uniform quantizer),
//! `TRANSACFRM=TRANSACFRM2=0` (High Rate Intra/Inter — exactly the coding
//! sets this crate transcribes), `TRANSDCTAB=0` (low-motion DC tables)) —
//! every sequence/picture-header field this crate parses was cross-checked
//! bit-by-bit by hand against the file's own byte layout before any
//! decoder code was written (see `tests/oracle.rs`). See that test module's
//! own doc for the per-plane result.
//!
//! # Dependencies
//!
//! [`vaco_codec_vlc`] for VLC table decode (`vaco_codec_vlc::VlcTable`).
//! [`vaco_bitstream::BitReader`] for the MSB-first picture/macroblock/block
//! bitstream. No dependency on `vaco-codec-mpegvideo`: that crate's shared
//! machinery (B-picture reorder, sequential/spatial motion-vector
//! prediction, half-pel motion compensation, MPEG-style zigzag/dequant) is
//! for the *inter*-prediction half of the MPEG-heritage family this crate's
//! I-frame-only scope never reaches, and VC-1's own zigzag tables, CBPCY
//! neighbour-prediction rule, and Annex A transform are not shaped like
//! that crate's MPEG-derived equivalents at all — reusing it here would
//! have meant importing a dependency for zero shared code.

#![forbid(unsafe_code)]

mod decoder;
mod header;
mod tables;
mod transform;

pub use decoder::{DECODER_VC1, Vc1Decoder};
