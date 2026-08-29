//! Device-resident frame storage, kept out of `vaco_frame::Frame` on purpose
//! — see the crate doc for why.

use std::fmt;
use std::sync::Arc;

use vaco_core::Result;
use vaco_frame::Frame;
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

use crate::device::HwDeviceType;

/// The backend-specific payload behind one [`HwFrame`] — a texture, a
/// `CVPixelBuffer`, a `VkImage`, whatever the owning backend crate's
/// `unsafe` code actually holds.
///
/// A trait object rather than a generic parameter on [`HwFrame`] because
/// [`crate::select`] hands back frames from whichever backend it chose at
/// runtime, and nothing above this crate is generic over backend.
pub trait HwSurface: Send + Sync + fmt::Debug {
    fn device_type(&self) -> HwDeviceType;

    /// Copy this surface's pixels into a newly allocated, ordinary
    /// [`Frame`] in the device's [`HwDeviceCaps::readback_pix_fmt`]
    /// (`vaco_pixfmt::PixFmt::is_hw` is false on the result — `Frame::alloc_video`
    /// refuses to allocate a hardware format, so a surface that produced one
    /// would be a backend bug, not a caller error).
    ///
    /// [`HwDeviceCaps::readback_pix_fmt`]: crate::HwDeviceCaps::readback_pix_fmt
    ///
    /// # Errors
    /// Whatever the backend's own readback path returns — a real API error,
    /// or [`vaco_core::Error::LimitExceeded`] if `budget` refuses the
    /// allocation.
    fn download(&self, budget: &mut Budget) -> Result<Frame>;
}

/// A frame that lives in device memory rather than in ordinary heap-backed
/// [`vaco_frame::Plane`](vaco_frame::Plane) storage.
///
/// Carries only what every backend agrees on — which device produced it, its
/// geometry, and the opaque `PixFmt` tag a caller would print for `-pix_fmts`
/// or pass to `-hwaccel_output_format` — plus the one operation every backend
/// must support: [`download`](HwFrame::download) back to software.
#[derive(Clone, Debug)]
pub struct HwFrame {
    pub hw_pix_fmt: PixFmt,
    pub width: u32,
    pub height: u32,
    surface: Arc<dyn HwSurface>,
}

impl HwFrame {
    /// # Panics
    /// Never; `hw_pix_fmt` is not validated here (that is
    /// [`vaco_frame::Frame::alloc_video`]'s job on the *readback* side, and
    /// this side never allocates ordinary plane storage).
    #[must_use]
    pub fn new(hw_pix_fmt: PixFmt, width: u32, height: u32, surface: Arc<dyn HwSurface>) -> Self {
        Self {
            hw_pix_fmt,
            width,
            height,
            surface,
        }
    }

    #[must_use]
    pub fn device_type(&self) -> HwDeviceType {
        self.surface.device_type()
    }

    /// Read this frame back into ordinary software storage.
    ///
    /// # Errors
    /// See [`HwSurface::download`].
    pub fn download(&self, budget: &mut Budget) -> Result<Frame> {
        self.surface.download(budget)
    }
}
