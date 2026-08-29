//! What a codec crate implements against to drive a hardware decode or
//! encode session.

use vaco_codec_core::CodecId;
use vaco_core::Result;

use crate::device::{HwDeviceCaps, HwDeviceType};
use crate::frame::HwFrame;

/// Which direction a session runs. Most backends support both, but a
/// candidate list still needs to say which one a given call is for —
/// `HwDeviceCaps::decode_codecs`/`encode_codecs` can disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HwDirection {
    Decode,
    Encode,
}

/// One picture's worth of hardware decode, driven by a codec crate that has
/// already parsed the bitstream down to per-slice compressed data.
///
/// Mirrors [`vaco_codec_core::Decoder`]'s send/receive shape at the
/// granularity a hardware decode session actually needs: hardware does not
/// consume raw packets, it consumes parsed parameters plus the still-coded
/// slice bytes, one picture at a time. A `vaco-hw-<backend>` crate's own
/// decoder wraps *both* this trait and the codec's software bitstream
/// parser; this trait alone is not a `Decoder`.
pub trait HwAccel: Send {
    fn device_type(&self) -> HwDeviceType;

    /// Begin one picture. A backend allocates or borrows a destination
    /// surface here, before any slice data arrives.
    ///
    /// # Errors
    /// Any per-picture setup failure the backend's own API reports, or a
    /// budget refusal if allocating the destination surface would exceed the
    /// caller's limits.
    fn start_frame(&mut self) -> Result<()>;

    /// Hand the accelerator one slice, tile or OBU's worth of still-coded
    /// data, already extracted from its container framing.
    ///
    /// # Errors
    /// Whatever the underlying decode-slice call reports. Malformed slice
    /// data is not necessarily fatal to the frame — a backend may recover
    /// the same way a software decoder resynchronises — so this returning
    /// `Err` does not obligate a caller to abandon the picture; that call is
    /// the caller's, mirroring `vaco_codec_core::Decoder`'s own error
    /// recoverability convention.
    fn decode_slice(&mut self, data: &[u8]) -> Result<()>;

    /// Finish the picture and hand back the hardware-resident frame.
    ///
    /// # Errors
    /// Any completion failure the backend's own API reports.
    fn end_frame(&mut self) -> Result<HwFrame>;
}

/// Static description of one hardware-acceleration backend, mirroring
/// `vaco_codec_core::ParserDesc`'s const-descriptor shape so this can be
/// collected into a registry-visible list the same way once a
/// `vaco-registry` fragment kind exists for it — there is nothing to
/// register yet, because no `vaco-hw-<backend>` crate ships real code today.
///
/// A caller assembles a `&[HwAccelDesc]` from whichever backend crates it
/// was built with and hands it to [`crate::select`]; nothing in this crate
/// discovers backends on its own.
#[derive(Debug, Clone, Copy)]
pub struct HwAccelDesc {
    pub name: &'static str,
    pub device_type: HwDeviceType,
    /// Probe whether a device of this type is present and usable right now.
    /// Cheap enough to call once per [`crate::select`] invocation — real
    /// backends cache the expensive part (driver enumeration) behind their
    /// own `OnceLock`, not behind this call being rare.
    ///
    /// # Errors
    /// Any reason the device is not usable: absent, driver too old, this
    /// build was compiled without the platform's binding crate. Every
    /// reason collapses to the same outcome in [`crate::select`] — try the
    /// next candidate, or fall back to software.
    pub probe: fn() -> Result<HwDeviceCaps>,
    /// Open a device context from a successful probe's capabilities.
    ///
    /// # Errors
    /// The device can be probed but opening it can still fail (another
    /// process holds it exclusively, a permission is denied); this is kept
    /// separate from `probe` so [`crate::select`] can log which of the two
    /// stages a candidate failed at.
    pub open: fn(&HwDeviceCaps) -> Result<Box<dyn crate::device::HwDeviceContext>>,
}

impl HwAccelDesc {
    #[must_use]
    pub fn supports(&self, codec: CodecId, direction: HwDirection, caps: &HwDeviceCaps) -> bool {
        match direction {
            HwDirection::Decode => caps.supports_decode(codec),
            HwDirection::Encode => caps.supports_encode(codec),
        }
    }
}
