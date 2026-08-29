//! `VideoToolbox` hardware decode: the one `vaco-hw-*` backend this tree can
//! build and test end to end on the macOS development machine.
//!
//! # Dependency
//!
//! Built against `objc2-video-toolbox` and its sibling `objc2-core-media`/
//! `objc2-core-video`/`objc2-core-foundation` crates (all Zlib/Apache-2.0/MIT,
//! all maintained by the `objc2` project). `planning/00-decisions.md`'s D14.3
//! names this exact crate family, by name, as permitted inside `vaco-hw-*` —
//! a pure-Rust binding to an OS media API, not vendored/compiled C. See
//! `docs/dependencies.md` for the adoption record Gate 3 asks for.
//!
//! # What is implemented
//!
//! H.264 baseline/main-profile decode of one access unit at a time, driven
//! synchronously through `VTDecompressionSessionDecodeFrameWithOutputHandler`
//! (`decode_flags = 0`, so the callback has already run by the time the call
//! returns — no async plumbing needed). The session is built once from a
//! stream's SPS/PPS via `CMVideoFormatDescriptionCreateFromH264ParameterSets`
//! and reused picture to picture, exactly like a real decoder would.
//!
//! Not implemented: HEVC/AV1/ProRes (H-02b's own scope, not attempted here),
//! asynchronous/out-of-order output, and any wiring into an actual
//! `vaco_codec_core::Decoder` — there is no `-hwaccel` call site yet for this
//! to plug into (see `vaco-hw-core`'s own doc).
//!
//! # Platform
//!
//! Everything above lives behind `#[cfg(target_os = "macos")]`, and the
//! `objc2-*` dependencies themselves are declared under a
//! `[target.'cfg(target_os = "macos")']` table in `Cargo.toml` — so a
//! non-macOS build (including `wasm32-unknown-unknown`) pulls none of them in
//! at all and this crate compiles to an empty shell. [`accel_desc`] is the
//! one function available everywhere; it returns `None` off macOS.

#[cfg(target_os = "macos")]
mod device;
#[cfg(target_os = "macos")]
mod nal;
#[cfg(target_os = "macos")]
mod session;

#[cfg(target_os = "macos")]
pub use device::VideoToolboxDevice;
#[cfg(target_os = "macos")]
pub use nal::{nal_unit_type, split_annex_b};
#[cfg(target_os = "macos")]
pub use session::VideoToolboxDecoder;

/// The `HwAccelDesc` this backend offers, for a caller assembling
/// `vaco_hw_core::select`'s candidate list.
///
/// `None` on anything other than macOS/iOS: `VideoToolbox` does not exist
/// there, and this crate is meant to be linked in unconditionally and
/// contribute nothing on those targets rather than requiring every caller to
/// `cfg` it out individually.
#[must_use]
pub fn accel_desc() -> Option<vaco_hw_core::HwAccelDesc> {
    #[cfg(target_os = "macos")]
    {
        Some(device::DESC)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod portable_tests {
    #[test]
    fn accel_desc_is_none_off_macos() {
        assert!(super::accel_desc().is_none());
    }
}
