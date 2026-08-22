//! Frame synchronisation: aligning several inputs on one timeline.
//!
//! Sixty-eight filters need this — `overlay`, `blend`, `psnr`, `ssim`, `lut2`,
//! the `masked*` family, the stack family, `remap`, `displace`, `mix`,
//! `paletteuse` and the rest — and its option set (`eof_action`, `shortest`,
//! `repeatlast`, `ts_sync_mode`) is documented user-facing surface that has to
//! behave identically on all of them. That is why it is a crate rather than a
//! helper.
//!
//! # The shape
//!
//! | Module | Contents |
//! |---|---|
//! | [`opts`] | the four options, and the per-input modes they compile to |
//! | [`sync`] | [`FrameSync`], the event loop, as a pure state machine |
//! | [`adapt`] | [`Synced`], which turns a [`FrameSyncFilter`] into a `Filter` |
//! | [`mock`] | a worked two-input filter, and the proof that the traits are usable |
//!
//! # What a filter writes
//!
//! ```no_run
//! use vaco_core::Result;
//! use vaco_filter_core::adapt::FrameOut;
//! use vaco_filter_core::FilterContext;
//! use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter};
//!
//! struct Blend;
//!
//! impl FrameSyncFilter for Blend {
//!     fn on_event(
//!         &mut self,
//!         _ctx: &mut FilterContext<'_>,
//!         event: &mut FrameSyncEvent<'_>,
//!     ) -> Result<FrameOut> {
//!         let Some(main) = event.take(0) else {
//!             return Ok(FrameOut::None);
//!         };
//!         // `event.get(1)` is `None` before the second input starts and,
//!         // under `repeatlast=0`, after it ends.
//!         Ok(FrameOut::One(main))
//!     }
//! }
//! ```
//!
//! Everything else — which timestamps become events, which frame each input
//! contributes at each one, what happens when an input ends early — is
//! [`FrameSync`]'s, and every rule of it was measured against ffmpeg 8.1
//! rather than inferred. The measurements, with their commands, are in
//! `docs/filter/vaco-filter-framesync.md`; four of them contradict
//! plan 16 §3.
//!
//! # Threading
//!
//! Nothing here spawns anything and nothing here reads a clock, so the crate
//! builds for `wasm32-unknown-unknown` unchanged. [`FrameSync`] is `Send` but
//! not `Sync`, which is the same single-driver shape `vaco-filter-core`'s
//! `Graph` has and for the same reason: a deterministic schedule is what makes
//! differential testing of a filtergraph meaningful.

#![forbid(unsafe_code)]

pub mod adapt;
pub mod mock;
pub mod opts;
pub mod sync;

pub use adapt::{FrameSyncFilter, Synced};
pub use opts::{EofAction, ExtendMode, FrameSyncOpts, FsInput, TsSyncMode, apply_opts};
pub use sync::{FALLBACK_TIME_BASE, FrameSync, FrameSyncEvent, MAX_DENOMINATOR, Step, gcd_q};
