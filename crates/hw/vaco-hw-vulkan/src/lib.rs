//! Vulkan Video device and capability probing, built against `ash`.
//!
//! # Scope — H-06a only
//!
//! This crate implements the device/capability layer: loading the Vulkan
//! loader, creating an instance, enumerating physical devices, and checking
//! each one's device extensions for the Vulkan Video decode chain
//! (`VK_KHR_video_queue`, `VK_KHR_video_decode_queue`,
//! `VK_KHR_video_decode_h264`). **It does not implement a decode session** —
//! that is real, substantially larger work (video session objects, DPB
//! slots, the bitstream-to-`VkVideoDecodeInfoKHR` mapping) that this pass
//! did not attempt, in the same spirit as `vaco-hw-videotoolbox` not
//! attempting HEVC: better to ship a correct, real, smaller thing than an
//! unverifiable larger one.
//!
//! # Untested beyond "does the probe run without crashing"
//!
//! This crate is developed and built on macOS, which has no real Vulkan
//! Video support (`MoltenVK` does not implement the extension — D13's own
//! backend table says so). `probe()` is real code that really calls into
//! `ash`, but on this development machine there is no properly configured
//! system Vulkan loader either (only application-bundled copies of
//! `libMoltenVK.dylib`/`libvulkan.dylib` inside unrelated apps, none on the
//! default `dlopen` search path) — measured directly, not assumed:
//! `probe()` returns [`ProbeOutcome::NoLoader`] here, meaning even ordinary
//! Vulkan *instance creation* is unexercised on this machine, let alone the
//! video-decode extension check. Treat this crate as *built against the
//! specification*, not *verified against real Vulkan or real Vulkan Video
//! hardware*.
//!
//! # Dependency
//!
//! `ash` (MIT OR Apache-2.0), pure-Rust Vulkan bindings that load the system
//! Vulkan loader via `dlopen`/`LoadLibrary` at runtime (the `loaded`
//! feature, on by default) rather than linking anything at compile time —
//! `planning/00-decisions.md` D14.3 names this crate by name as a permitted
//! OS-API binding for `vaco-hw-*`. See `docs/dependencies.md`.

use vaco_codec_core::CodecId;
use vaco_core::Result;
use vaco_hw_core::{HwAccelDesc, HwDeviceCaps, HwDeviceContext, HwDeviceType};

mod probe;

pub use probe::{ProbeOutcome, probe};

/// The opened "device" — nothing beyond its capabilities, mirroring
/// `vaco-hw-videotoolbox`'s equivalent: this crate implements no decode
/// session, so there is nothing else for an opened handle to hold yet.
#[derive(Debug)]
pub struct VulkanDevice {
    caps: HwDeviceCaps,
}

impl HwDeviceContext for VulkanDevice {
    fn device_type(&self) -> HwDeviceType {
        HwDeviceType::Vulkan
    }

    fn caps(&self) -> &HwDeviceCaps {
        &self.caps
    }
}

/// # Errors
/// Whatever [`probe`] returns.
fn probe_desc() -> Result<HwDeviceCaps> {
    probe().into_caps()
}

/// # Errors
/// Never — probing already established the device works; this only builds
/// the handle.
#[allow(
    clippy::unnecessary_wraps,
    reason = "must match HwAccelDesc::open's fn-pointer signature, shared with backends where opening can fail"
)]
fn open_desc(caps: &HwDeviceCaps) -> Result<Box<dyn HwDeviceContext>> {
    Ok(Box::new(VulkanDevice { caps: caps.clone() }))
}

/// The `HwAccelDesc` this backend offers, for a caller assembling
/// `vaco_hw_core::select`'s candidate list. Always `Some` — unlike
/// `vaco-hw-videotoolbox`'s platform-specific `Cargo.toml` gating, `ash`
/// itself is cross-platform, so whether a real device is present is exactly
/// what [`probe`] (called through this descriptor) determines at runtime,
/// not something this function can know in advance.
#[must_use]
pub const fn accel_desc() -> HwAccelDesc {
    HwAccelDesc {
        name: "vulkan-video",
        device_type: HwDeviceType::Vulkan,
        probe: probe_desc,
        open: open_desc,
    }
}

pub(crate) const DECODE_CODECS: &[CodecId] = &[CodecId::H264];
