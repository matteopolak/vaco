//! Which hardware backend, and what an opened device can do.

use vaco_codec_core::CodecId;
use vaco_pixfmt::PixFmt;

/// Which hardware-acceleration API a device context was opened through.
///
/// One variant per backend named in the D13 backend-strategy table. This
/// enum exists independently of any `vaco-hw-*` backend crate compiling —
/// it is how [`HwAccelDesc`](crate::HwAccelDesc) values from *different*
/// backend crates sit in the same slice for [`crate::select`] to choose
/// between.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HwDeviceType {
    /// Apple's `VideoToolbox` — the only path to Apple silicon's media engine,
    /// since `MoltenVK` does not implement Vulkan Video.
    VideoToolbox,
    /// Khronos Vulkan Video — the widest-reaching single API, covering Linux,
    /// Windows and Android through one vendor-independent surface.
    Vulkan,
    /// VA-API, Linux's older vendor-neutral video acceleration API.
    Vaapi,
    /// Microsoft's D3D12 Video.
    D3d12,
    /// NVIDIA's NVDEC/NVENC.
    Nvdec,
}

impl HwDeviceType {
    /// The opaque [`PixFmt`] a frame decoded on this backend carries before
    /// [`crate::HwFrame::download`] resolves it to a real pixel format —
    /// `AV_PIX_FMT_VIDEOTOOLBOX`'s counterpart, one per backend, already
    /// tabulated in `vaco-pixfmt` (`ffmpeg -pix_fmts`' own hardware rows).
    #[must_use]
    pub const fn hw_pix_fmt(self) -> PixFmt {
        match self {
            Self::VideoToolbox => PixFmt::VideotoolboxVld,
            Self::Vulkan => PixFmt::Vulkan,
            Self::Vaapi => PixFmt::Vaapi,
            Self::D3d12 => PixFmt::D3d12,
            Self::Nvdec => PixFmt::Cuda,
        }
    }
}

/// What a successfully opened device can do — queried once, at open time,
/// so [`crate::select`] can filter candidates without touching a driver on
/// every call.
#[derive(Debug, Clone)]
pub struct HwDeviceCaps {
    pub device_type: HwDeviceType,
    /// Codecs this device can decode, empty if this device is encode-only.
    pub decode_codecs: Vec<CodecId>,
    /// Codecs this device can encode, empty if this device is decode-only.
    pub encode_codecs: Vec<CodecId>,
    /// Largest picture this device will accept along either axis.
    pub max_dimension: u32,
    /// The concrete pixel format [`crate::HwFrame::download`] produces for
    /// this device — its readback ("software") format, e.g. `Nv12`. Not
    /// necessarily what a caller wants, but always a format `vaco-pixfmt`
    /// can allocate and a filter graph can already read.
    pub readback_pix_fmt: PixFmt,
}

impl HwDeviceCaps {
    #[must_use]
    pub fn supports_decode(&self, codec: CodecId) -> bool {
        self.decode_codecs.contains(&codec)
    }

    #[must_use]
    pub fn supports_encode(&self, codec: CodecId) -> bool {
        self.encode_codecs.contains(&codec)
    }
}

/// A successfully opened hardware device.
///
/// Deliberately minimal: everything a decode or encode *session* needs
/// (surface pools, command queues, the rest) is backend-specific and lives
/// behind [`crate::HwAccel`], which a backend crate builds from one of
/// these. This trait is only the handle that outlives one session and the
/// capabilities it was opened with.
pub trait HwDeviceContext: Send + Sync + std::fmt::Debug {
    fn device_type(&self) -> HwDeviceType;
    fn caps(&self) -> &HwDeviceCaps;
}
