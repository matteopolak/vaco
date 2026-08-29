//! The `vaco-hw-core` seams: probing, opening, and the descriptor a caller
//! adds to its candidate list.
//!
//! `VideoToolbox` has no separate "device" to enumerate the way Vulkan or
//! VA-API do — it is always present on a real macOS/iOS build, compiled in
//! or not. So "probing" here means exactly one thing: is this binary running
//! on a platform where the framework exists at all. Real per-format-and-size
//! capability is checked when a session is actually created
//! ([`VideoToolboxDecoder::new`](crate::VideoToolboxDecoder::new)), the same
//! place a Vulkan or VA-API backend would discover a specific codec/profile
//! is unsupported.

use vaco_codec_core::CodecId;
use vaco_core::Result;
use vaco_hw_core::{HwAccelDesc, HwDeviceCaps, HwDeviceContext, HwDeviceType};
use vaco_pixfmt::PixFmt;

/// The opened "device". Holds nothing beyond its capabilities, per the
/// module doc — `VideoToolbox` has no persistent handle below session level.
#[derive(Debug)]
pub struct VideoToolboxDevice {
    caps: HwDeviceCaps,
}

impl HwDeviceContext for VideoToolboxDevice {
    fn device_type(&self) -> HwDeviceType {
        HwDeviceType::VideoToolbox
    }

    fn caps(&self) -> &HwDeviceCaps {
        &self.caps
    }
}

/// Always succeeds on macOS/iOS, unconditionally: `VideoToolbox` is a system
/// framework, not a piece of hardware that can be absent from an otherwise
/// working install.
///
/// # Errors
/// Never, on this platform. The `Result` return type matches
/// [`HwAccelDesc::probe`]'s signature, which has to accommodate backends
/// (VA-API, NVDEC) where absence really is the common case.
#[allow(
    clippy::unnecessary_wraps,
    reason = "must match HwAccelDesc::probe's fn-pointer signature, shared with backends where absence is real"
)]
pub(crate) fn probe() -> Result<HwDeviceCaps> {
    Ok(HwDeviceCaps {
        device_type: HwDeviceType::VideoToolbox,
        decode_codecs: vec![CodecId::H264],
        encode_codecs: vec![],
        max_dimension: 8192,
        readback_pix_fmt: PixFmt::Nv12,
    })
}

/// # Errors
/// Never, on this platform — see [`probe`].
#[allow(
    clippy::unnecessary_wraps,
    reason = "must match HwAccelDesc::open's fn-pointer signature, shared with backends where opening can fail"
)]
pub(crate) fn open(caps: &HwDeviceCaps) -> Result<Box<dyn HwDeviceContext>> {
    Ok(Box::new(VideoToolboxDevice { caps: caps.clone() }))
}

pub(crate) const DESC: HwAccelDesc = HwAccelDesc {
    name: "videotoolbox",
    device_type: HwDeviceType::VideoToolbox,
    probe,
    open,
};
