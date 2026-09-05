//! Video encoders that spawn a user-installed CLI tool as a subprocess.
//!
//! # Why this crate exists
//!
//! `vaco` will never carry a software H.264 or HEVC encoder: x264/x265 are
//! GPL- or patent-encumbered without a cleared counterparty. The `exec`
//! backend instead spawns the user's installed binary, pipes raw frames over
//! Y4M stdin, and reads Annex-B output from stdout. The process boundary keeps
//! that implementation outside this crate, binary, and process address space.
//!
//! # What is here
//!
//! | Module | Job |
//! |---|---|
//! | [`y4m`] | Serialise a [`vaco_frame::Frame`]'s video planes as a YUV4MPEG2 stream (header line + `FRAME` markers), matching what real `ffmpeg -f yuv4mpegpipe` emits for `yuv420p` (measured, not guessed — see that module's doc) |
//! | [`annexb`] | Split a raw Annex-B byte stream (H.264/H.265 NAL units, start-code delimited) into one packet per access unit, using an Access Unit Delimiter NAL as the boundary |
//! | [`process`] | Spawn the child, own its stdin/stdout/stderr, and never block on one pipe while the other is unread (the classic subprocess deadlock: a child that fills the pipe we are not currently reading stalls when it also stops reading the pipe we ARE writing) |
//! | [`encoder`] | [`vaco_codec_core::Encoder`] impl driving the above three, one per tool |
//!
//! # Scope cuts, stated plainly
//!
//! - **No B-frames**: both backends are invoked with `--bframes 0`. This
//!   guarantees the external tool's output access units are in the same
//!   order as the input frames (no encoder-side reordering), which is what
//!   lets [`encoder::ExecEncoder`] hand back the exact input `pts`/`duration`
//!   for each output packet without needing to parse a real presentation
//!   timestamp back out of the elementary stream. A real cost in compression
//!   efficiency; not a correctness cut, since it trades a genuine feature for
//!   the plumbing to be provably correct rather than a PTS/DTS reordering
//!   heuristic that could be wrong in a way nothing here would catch.
//! - **8-bit 4:2:0 input only** (`Yuv420p`). x264/x265 both support far more
//!   (10/12-bit, 4:2:2/4:4:4), but this crate only builds the one Y4M
//!   colour-space tag it has measured against a real `ffmpeg` Y4M mux
//!   (`C420jpeg` — see [`y4m`]'s doc for the measurement).
//! - **Two tools**: `libx264` (H.264) and `libx265` (HEVC), named to match
//!   `ffmpeg -encoders`'s own names for the same idea. The mechanism in
//!   [`process`] and [`annexb`] is generic; a third tool is a new
//!   [`encoder::ExecTool`] impl, not a new pipe layer.
#![forbid(unsafe_code)]

pub mod annexb;
pub mod encoder;
pub mod process;
pub mod y4m;

pub use encoder::{LIBX264_ENCODER, LIBX265_ENCODER};
