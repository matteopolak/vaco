//! The hardware-acceleration framework: device contexts, hardware-resident
//! frames, the [`HwAccel`] trait a codec crate implements against, and
//! selection with software fallback.
//!
//! # What lives here, and what does not
//!
//! This crate defines the *shape* of hardware acceleration — nothing here
//! talks to a GPU, a driver, or an OS media API. A concrete backend
//! (`VideoToolbox`, Vulkan Video, VA-API, D3D12, NVDEC) is a separate
//! `vaco-hw-<backend>` crate that implements [`HwDeviceContext`] and
//! [`HwAccel`] against real `unsafe` bindings — D13 is what permits `unsafe`
//! there and nowhere outside `vaco-hw-*`. This crate itself needs none: it is
//! pure orchestration, so it stays `#![forbid(unsafe_code)]` like every other
//! layer.
//!
//! # Why frame storage is a separate type rather than a new `Frame` variant
//!
//! [`vaco_frame::Frame`]'s `FrameData` enum is deliberately closed (its own
//! doc explains why: adding a variant is a change felt across every filter
//! that matches on it). A hardware-resident picture is therefore represented
//! here as [`HwFrame`] — device memory plus a pixel-format tag, never mixed
//! into `Frame` — and the only way to get a real [`vaco_frame::Frame`] out of
//! one is [`HwFrame::download`], an explicit, budgeted readback. A decode
//! pipeline that wants hardware frames to flow through the existing `Decoder`
//! trait unchanged downloads at the boundary; one written against hardware
//! end to end holds `HwFrame`s directly. Either is possible without touching
//! `vaco-frame`.
//!
//! # Selection and fallback, in one call
//!
//! [`select`] is the whole story: give it a codec, a direction and a
//! preference, and it returns [`Selected::Hardware`] or [`Selected::Software`]
//! — never an error for the common case of "no hardware here", because no
//! `unsafe`-audited backend has anything registered on most machines this
//! code runs on, and a media tool that requires hardware acceleration to
//! function at all is not one anybody can ship. See [`HwPreference`] for the
//! one case that *should* fail loudly: a caller who named a specific backend.

#![forbid(unsafe_code)]

mod accel;
mod device;
mod frame;
mod select;

pub use accel::{HwAccel, HwAccelDesc, HwDirection};
pub use device::{HwDeviceCaps, HwDeviceContext, HwDeviceType};
pub use frame::{HwFrame, HwSurface};
pub use select::{HwPreference, Selected, select};
