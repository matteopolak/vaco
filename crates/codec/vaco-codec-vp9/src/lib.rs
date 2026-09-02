//! VP9 video decode (VP9 Bitstream & Decoding Process Specification v0.6).
//!
//! Scope of this package: C-29/C-30 (header parsing, superframes, the
//! probability model, the partition/mode-info bitstream walk, coefficient
//! token decode, dequantization, inverse transforms and intra prediction)
//! plus C-31 (inter prediction: reference-frame management, motion-vector
//! prediction, compound prediction, sub-pel interpolation) — a real decoder
//! for both key and inter frames.
//!
//! Backward probability adaptation (§8.3/8.4) is *not* implemented: see
//! `crate::header`'s module doc and `planning/TECH-DEBT.md` for what that
//! means for the frames this crate does and does not fully verify.
//!
//! C-32a lands §8.8's in-loop deblocking filter (`crate::loopfilter`): lossy
//! VP9 content now decodes bit-exactly rather than within a loop-filter
//! tolerance — see `crate::loopfilter`'s module doc and
//! `docs/codec/vaco-codec-vp9.md`'s Verification table.
//!
//! Profiles 1-3 (#327, C-32b) are handled: `predict`/`transform`/
//! `loopfilter`/`interpredict`/`framebuf` were already generic over bit
//! depth and independent x/y chroma subsampling, and the one real gap
//! (`header::parse_uncompressed_header` resetting to a hardcoded 4:2:0
//! 8-bit `ColorConfig` on every frame that does not itself re-signal one)
//! is fixed — see that module's doc.
//!
//! Multi-tile-column decode (#328, C-32c) is now correct: `decode_tile`
//! used to loop every mi-column of the frame regardless of which tile it
//! was called for, so a real multi-tile-column stream decoded wrong from
//! the second tile column onward, not merely "not spec-exact at the tile
//! edge" as this crate's own history previously characterised it — see
//! `decode::tests::two_tile_columns_decode_correctly_on_both_sides_of_the_boundary`
//! and `docs/codec/vaco-codec-vp9.md`'s Verification section.
//!
//! # Threading (issue #328, `C-32c`)
//!
//! `-threads N` overlaps *pictures*, not one picture's own tile columns.
//! `decode::parse_frame_tiles` walks every tile's mode-info/motion-vector/
//! coefficient-token bitstream — cheap, and where the real serial state
//! lives (entropy persistence, §7.2's reference-frame-store bookkeeping,
//! `UsePrevFrameMvs`'s dependency on the *previous* frame's MV grid) — on
//! the caller's own thread, in decode order. Once a frame's tokens are
//! fully parsed, its reconstruction (prediction, inverse transform, sample
//! addition) and §8.8 loop filter (`decode::reconstruct_frame`, dispatched
//! as a `Vp9FrameTask::Decode`) run on a worker thread while the *next*
//! frame's own parse proceeds immediately, over the same
//! `vaco_codec_core::threading::FrameRunner` seam `vaco-codec-h264`/
//! `vaco-codec-hevc`/`vaco-codec-vp8` already use. A `show_existing_frame`
//! packet (`Vp9FrameTask::ShowExisting`) dispatches too, rather than
//! emitting inline, so it cannot jump ahead of a `Decode` task still being
//! reconstructed and violate dispatch-order collection.
//!
//! Reference frames are `PictureRef` handles (`crate::refframe`'s
//! `PendingRefStore`/`PendingRefSlot`) while a frame is in flight, resolved
//! to real pixels (`crate::framebuf::materialize`, waiting on the
//! producing task if needed) only once a *later* frame's own reconstruction
//! task actually runs and needs them — the same handle-not-owned-picture
//! design `vaco-codec-vp8`'s #301 established, adapted for VP9's existing
//! `Arc<Picture>`-based `RefFrameStore`/`RefSlot` (already closer to this
//! shape than VP8's was) and for `u16`-per-sample planes
//! (`framebuf::plane_to_bytes`/`plane_from_bytes`, since VP9 samples run up
//! to 12-bit and `PictureWriter`'s bands are byte-oriented).
//!
//! Verified byte-identical to `ffmpeg -c:v libvpx-vp9`'s own decode at
//! `-threads` 1, 2, 4 and 8, on a multi-tile-column multi-frame stream and
//! on a stream with real invisible alt-ref frames delivered as superframes
//! (`show_frame = 0`, which must be fully reconstructed for the reference
//! store but never reach [`decode::Vp9Decoder::receive_frame`] as output —
//! see `tests/conformance.rs`'s module doc for exactly how that fixture was
//! produced and confirmed). Tile-*column* parallelism (decoding a frame's
//! own tile columns concurrently with each other, as `libvpx`'s
//! `--row-mt`/frame-parallel modes can) is a separate, larger piece of work
//! and remains out of scope — `planning/TECH-DEBT.md` has what that would
//! need.
//!
//! `crate::encode` (issue #329, C-33a) adds this crate's first encoder: a
//! real, spec-conformant all-intra key-frame bitstream writer. #330 (C-33b)
//! replaces its fixed partition/mode/skip choices with a real (heuristic,
//! not RD-optimal) partition-size decision, real intra mode decision, and
//! real lossless residual coding — see `crate::encode`'s own module doc for
//! exactly what it does and does not do.
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
