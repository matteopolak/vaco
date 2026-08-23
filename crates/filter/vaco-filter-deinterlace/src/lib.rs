//! T2/T3 field-order and deinterlace filters (plan 16 SS4.3, FT-4.12 long tail).
//!
//! # Membership, checked against the reference rather than assumed
//!
//! `planning/16-filters.md` SS4.3's `vaco-filter-deinterlace` row lists `yadif,
//! bwdif, w3fdif, estdif, separatefields, weave, doubleweave, fieldorder,
//! fieldmatch, fieldhint, detelecine, telecine, idet, vfrdet, interlace,
//! tinterlace, kerndeint, pullup, repeatfields, phase`. Every one of the
//! twenty names was checked against `ffmpeg -hide_banner -filters` and
//! `ffmpeg -h filter=<name>` (ffmpeg 8.1, 2026-08-23): all twenty exist with
//! exactly that name, and — this matters for how the crate is shaped — all
//! twenty are `V->V` (one input pad, one output pad) **except** `fieldmatch`,
//! which is `N->V` with a dynamic input count decided by its own `ppsrc`
//! option (1 input when `ppsrc=false`, the default; 2 when `true`). The row
//! is exact: nothing to add, nothing to drop.
//!
//! # A finding worth stating plainly: this row does not need `Paired` or `Fanout`
//!
//! `separatefields` emits two frames per input frame, `weave`/`doubleweave`
//! consume two and emit one, and `tinterlace`/`telecine`/`detelecine` change
//! the frame rate — the brief that dispatched this crate flagged all of that
//! as the reason `vaco-filter-core::adapt`'s new `Paired`/`Fanout` adapters
//! (interface gap 10) might be needed here. Measured against the reference,
//! none of them are: every one of these filters keeps its frame-count or
//! rate change **inside a single pad**, via internal buffering, exactly the
//! way `vaco-filter-temporal`'s `tmix`/`decimate` already do. `[FrameFilter]`
//! plus [`vaco_filter_core::adapt::Simple`] — returning [`FrameOut::Many`],
//! [`FrameOut::None`], or holding one frame of state between calls — is
//! sufficient for every filter in this row. `fieldmatch`'s `ppsrc=true` path
//! is the one genuine two-input shape, and it is handled with
//! [`vaco_filter_core::adapt::Paired`], per this row's own dependency column.
//!
//! # Shape
//!
//! One module per filter, `pub const DESC: FilterDesc` and a crate-private
//! `fn create`, aggregated by [`registry::DeinterlaceRegistry`] — the same
//! shape `vaco-filter-temporal` and `vaco-filter-denoise` use. [`video`] is
//! the shared byte-level plane/row helper this crate's filters are built on:
//! unlike `vaco-filter-temporal`'s `PlaneBuf` (decode to `f32`, run
//! arithmetic, encode back), almost everything in this row is *row
//! selection and rearrangement* with no per-sample math, so operating
//! directly on the raw row bytes is both simpler and exact for every sample
//! depth without a `PlaneBuf` round trip.
//!
//! # `vdsp`
//!
//! The row's extra-deps column calls for `vdsp`. `idet` and `fieldmatch`
//! both need a per-row/per-block "how combed is this" metric that is not
//! `plane_sad`/`block_sad`/`normalised_sad` (those compare two *whole*
//! planes; combing is a property of *one* frame's own field alternation).
//! [`vaco_filter_vdsp::comb_score`] and [`vaco_filter_vdsp::field_sad`] are
//! added there for this crate to use, per that crate's own invitation to
//! extend rather than duplicate (see `docs/filter/vaco-filter-vdsp.md`).
//!
//! # What is verified versus structural
//!
//! See `docs/filter/vaco-filter-deinterlace.md` for the full per-filter
//! accounting — which filters are checked byte-for-byte against measured
//! `ffmpeg -f rawvideo`/`-f framecrc` output (the whole round-trip family:
//! `separatefields`, `weave`, `doubleweave`, `interlace`, `fieldorder`,
//! `telecine`, `detelecine`, `phase`, `repeatfields`, five of
//! `tinterlace`'s eight modes), which are documented structural
//! implementations that satisfy the required invariants without claiming
//! byte-exactness (`yadif`, `bwdif`, `w3fdif`, `estdif`, `kerndeint`,
//! `pullup`, `fieldmatch`, `fieldhint`, and `tinterlace`'s `pad`/
//! `interlacex2`/`mergex2` modes), and the two detectors, `idet` and
//! `vfrdet`. `INTERFACE-GAPS.md` gap 11 (`vaco_frame::Frame` had no
//! per-frame metadata dictionary) was still open when this crate started
//! and closed additively before it finished; `idet` writes real
//! `lavfi.idet.*` keys via `Frame::set_metadata` as a result (narrower
//! vocabulary than the reference's four-way `tff`/`bff`/`progressive`/
//! `undetermined`, under the correct key names — see the `idet` module
//! doc). `vfrdet` still has nowhere to put its answer: measured directly,
//! the reference's own `vfrdet` publishes *no* per-frame metadata at all,
//! only a final summary log line, so gap 11 closing does not open a new
//! channel for it specifically.
#![forbid(unsafe_code)]

pub mod bwdif;
pub mod detelecine;
pub mod estdif;
pub mod fieldhint;
pub mod fieldmatch;
pub mod fieldorder;
pub mod idet;
pub mod interlace;
pub mod kerndeint;
mod mad;
pub mod phase;
pub mod pullup;
pub mod registry;
pub mod repeatfields;
pub mod separatefields;
pub mod telecine;
pub mod tinterlace;
pub mod vfrdet;
mod video;
pub mod w3fdif;
pub mod weave;
pub mod yadif;

pub use registry::DeinterlaceRegistry;
