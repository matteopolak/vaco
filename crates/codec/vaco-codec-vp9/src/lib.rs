//! VP9 video decode (VP9 Bitstream & Decoding Process Specification v0.6).
//!
//! The decoder handles headers, superframes, probability models, coefficient
//! tokens, inverse transforms, intra prediction, and inter prediction for key
//! and inter frames. Backward probability adaptation (§8.3/8.4) remains
//! unsupported; [`header`] documents the resulting conformance boundary.
//!
//! [`loopfilter`] implements §8.8 deblocking for bit-exact lossy output.
//! Profiles 1–3 retain their signalled bit depth and independent horizontal
//! and vertical chroma subsampling across frames. Multi-column streams restrict
//! each tile decode to its own mode-info columns.
//!
//! `-threads N` overlaps pictures, not columns within one picture. Tile parsing
//! stays in decode order on the caller thread because it owns entropy state,
//! §7.2 reference-store updates, and previous-frame motion vectors. Prediction,
//! inverse transforms, sample addition, and loop filtering run through
//! `vaco_codec_core::threading::FrameRunner`. `show_existing_frame` packets use
//! the same dispatch path so they cannot overtake an unfinished decode task.
//!
//! In-flight references are `PictureRef` handles and materialize only when a
//! later reconstruction needs their pixels. The stores use `Arc<Picture>` and
//! convert the byte-oriented worker bands to `u16` planes for samples up to
//! 12-bit. This avoids transferring ownership of incomplete reference frames.
//!
//! Decodes were byte-identical to `ffmpeg -c:v libvpx-vp9` at 1, 2, 4, and 8
//! threads for multi-column multi-frame content and invisible alt-ref
//! superframes. Invisible frames update references but never reach
//! [`decode::Vp9Decoder::receive_frame`]. Tile-column parallelism remains out
//! of scope.
//!
//! [`encode`] writes spec-conformant all-intra key frames with heuristic
//! partition and intra-mode decisions plus lossless residual coding; it does
//! not claim rate-distortion-optimal decisions.
#![forbid(unsafe_code)]

pub mod decode;
pub mod encode;
pub mod framebuf;
pub mod header;
pub mod interpredict;
pub mod loopfilter;
pub mod mvpred;
pub mod predict;
pub mod refframe;
pub mod superframe;
pub mod tables;
pub mod tokens;
pub mod transform;

pub use decode::{VP9_DECODER, Vp9Decoder};
pub use encode::{VP9_ENCODER, Vp9Encoder};
