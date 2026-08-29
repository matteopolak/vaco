//! Choosing a device, and falling back to software when there is none.

use vaco_codec_core::CodecId;
use vaco_core::{Error, Result};

use crate::accel::{HwAccelDesc, HwDirection};
use crate::device::{HwDeviceContext, HwDeviceType};

/// What a caller wants from hardware acceleration.
///
/// The default a CLI should offer is [`PreferHardware`](Self::PreferHardware)
/// — try, and say nothing if there was nothing to try. Only an explicit
/// per-backend request should be able to fail the whole operation over
/// hardware being absent, because that request came with a promise the
/// caller can be held to: they named the backend, so "it is not there" is
/// information they asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwPreference {
    /// Try every registered backend that claims this codec and direction, in
    /// list order; use the first one that probes and opens successfully.
    /// Falls back to [`Selected::Software`] if none do — never an error.
    PreferHardware,
    /// Use exactly this backend, or fail. For a caller that named a device
    /// explicitly (a future `-hwaccel <name>` flag) and wants to know
    /// definitively rather than silently transcode in software.
    Require(HwDeviceType),
    /// Do not probe anything; always [`Selected::Software`].
    SoftwareOnly,
}

/// The outcome of [`select`].
#[derive(Debug)]
pub enum Selected {
    /// An opened device, ready for a `vaco-hw-<backend>` crate to build an
    /// [`crate::HwAccel`] session on top of.
    Hardware {
        device: Box<dyn HwDeviceContext>,
        backend_name: &'static str,
    },
    /// No hardware was requested, available, or willing to open — proceed
    /// with the existing software `Decoder`/`Encoder` path. Not a
    /// degraded state to warn about: on most machines this is simply the
    /// only correct answer, since `candidates` is often empty.
    Software,
}

/// Pick a device for `codec` in `direction`, honouring `preference`, from
/// whichever backends `candidates` lists.
///
/// `candidates` is assembled by the caller (a future codec-path integration,
/// not this crate) from whichever `vaco-hw-<backend>` crates the build
/// includes. An empty slice is a completely ordinary input — it is what
/// every build produces today, since no backend crate ships real code yet —
/// and produces [`Selected::Software`] under every preference except
/// [`HwPreference::Require`], which has nothing to require and says so.
///
/// # Errors
/// Only under [`HwPreference::Require`]: the named backend is not among
/// `candidates`, or every candidate matching it failed to probe or open.
pub fn select(
    codec: CodecId,
    direction: HwDirection,
    preference: HwPreference,
    candidates: &[HwAccelDesc],
) -> Result<Selected> {
    match preference {
        HwPreference::SoftwareOnly => Ok(Selected::Software),
        HwPreference::PreferHardware => {
            for desc in candidates {
                if let Some(hw) = try_open(*desc, codec, direction) {
                    return Ok(Selected::Hardware {
                        device: hw,
                        backend_name: desc.name,
                    });
                }
            }
            Ok(Selected::Software)
        }
        HwPreference::Require(want) => {
            let mut named_but_unusable = false;
            for desc in candidates.iter().filter(|d| d.device_type == want) {
                named_but_unusable = true;
                if let Some(hw) = try_open(*desc, codec, direction) {
                    return Ok(Selected::Hardware {
                        device: hw,
                        backend_name: desc.name,
                    });
                }
            }
            Err(if named_but_unusable {
                Error::Unsupported("requested hardware backend is present but unusable for this codec")
            } else {
                Error::Unsupported("requested hardware backend is not compiled in or not registered")
            })
        }
    }
}

/// Probe, filter by codec support, then open — the one path shared by both
/// `select` branches above, so "try a candidate" means the same thing in
/// both.
fn try_open(
    desc: HwAccelDesc,
    codec: CodecId,
    direction: HwDirection,
) -> Option<Box<dyn HwDeviceContext>> {
    let caps = (desc.probe)().ok()?;
    if !desc.supports(codec, direction, &caps) {
        return None;
    }
    (desc.open)(&caps).ok()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "mock fn pointers must match HwAccelDesc::probe/open's Result-returning signature"
)]
mod tests {
    use std::sync::Arc;

    use vaco_core::Error;
    use vaco_frame::Frame;
    use vaco_limits::Budget;
    use vaco_pixfmt::PixFmt;

    use super::*;
    use crate::device::HwDeviceCaps;
    use crate::frame::HwSurface;

    #[derive(Debug)]
    struct MockSurface;

    impl HwSurface for MockSurface {
        fn device_type(&self) -> HwDeviceType {
            HwDeviceType::VideoToolbox
        }

        fn download(&self, budget: &mut Budget) -> Result<Frame> {
            Frame::alloc_video(budget, PixFmt::Nv12, 64, 64)
        }
    }

    #[derive(Debug)]
    struct MockDevice(HwDeviceCaps);

    impl HwDeviceContext for MockDevice {
        fn device_type(&self) -> HwDeviceType {
            self.0.device_type
        }

        fn caps(&self) -> &HwDeviceCaps {
            &self.0
        }
    }

    fn caps_supporting(device_type: HwDeviceType, codec: CodecId) -> HwDeviceCaps {
        HwDeviceCaps {
            device_type,
            decode_codecs: vec![codec],
            encode_codecs: vec![],
            max_dimension: 8192,
            readback_pix_fmt: PixFmt::Nv12,
        }
    }

    // `HwAccelDesc`'s `probe`/`open` fields are `fn` pointers, not closures
    // (so a real descriptor can be a `const`), which means each mock below
    // is a free `fn` rather than something parameterised by codec/device —
    // one codec (H264) and three devices is enough to exercise every branch
    // `select` has.

    fn present_desc() -> HwAccelDesc {
        fn probe() -> Result<HwDeviceCaps> {
            Ok(caps_supporting(HwDeviceType::VideoToolbox, CodecId::H264))
        }
        fn open(caps: &HwDeviceCaps) -> Result<Box<dyn HwDeviceContext>> {
            Ok(Box::new(MockDevice(caps.clone())))
        }
        HwAccelDesc {
            name: "mock-present",
            device_type: HwDeviceType::VideoToolbox,
            probe,
            open,
        }
    }

    fn absent_desc() -> HwAccelDesc {
        fn probe() -> Result<HwDeviceCaps> {
            Err(Error::Unsupported("no device"))
        }
        fn open(_: &HwDeviceCaps) -> Result<Box<dyn HwDeviceContext>> {
            unreachable!("select must not open a device that failed to probe")
        }
        HwAccelDesc {
            name: "mock-absent",
            device_type: HwDeviceType::Vulkan,
            probe,
            open,
        }
    }

    fn present_but_empty_caps_desc() -> HwAccelDesc {
        fn probe() -> Result<HwDeviceCaps> {
            // Probes fine, but its capabilities name no codec at all — the
            // "present, but does not claim to help here" case, distinct from
            // `present_but_wont_open_desc` below where it claims the codec
            // and fails later, at `open`.
            Ok(caps_supporting(HwDeviceType::Vaapi, CodecId::Hevc))
        }
        fn open(_: &HwDeviceCaps) -> Result<Box<dyn HwDeviceContext>> {
            unreachable!("select must not open a device that does not support the codec")
        }
        HwAccelDesc {
            name: "mock-present-wrong-codec",
            device_type: HwDeviceType::Vaapi,
            probe,
            open,
        }
    }

    fn present_but_wont_open_desc() -> HwAccelDesc {
        fn probe() -> Result<HwDeviceCaps> {
            Ok(caps_supporting(HwDeviceType::Vaapi, CodecId::H264))
        }
        fn open(_: &HwDeviceCaps) -> Result<Box<dyn HwDeviceContext>> {
            Err(Error::Unsupported("exclusively held by another process"))
        }
        HwAccelDesc {
            name: "mock-present-but-busy",
            device_type: HwDeviceType::Vaapi,
            probe,
            open,
        }
    }

    #[test]
    fn software_only_never_probes() {
        let candidates = [present_desc()];
        let selected = select(
            CodecId::H264,
            HwDirection::Decode,
            HwPreference::SoftwareOnly,
            &candidates,
        )
        .expect("software-only never fails");
        assert!(matches!(selected, Selected::Software));
    }

    #[test]
    fn prefer_hardware_with_no_candidates_falls_back_to_software() {
        let selected = select(
            CodecId::H264,
            HwDirection::Decode,
            HwPreference::PreferHardware,
            &[],
        )
        .expect("no candidates is not an error");
        assert!(matches!(selected, Selected::Software));
    }

    #[test]
    fn prefer_hardware_falls_back_when_every_candidate_is_unusable() {
        // Three distinct reasons none of these can serve this request: fails
        // to probe at all, probes but does not claim this codec, and probes
        // and claims the codec but fails to open. `select` must treat all
        // three the same way — try the next, then fall back.
        let candidates = [
            absent_desc(),
            present_but_empty_caps_desc(),
            present_but_wont_open_desc(),
        ];
        let selected = select(
            CodecId::H264,
            HwDirection::Decode,
            HwPreference::PreferHardware,
            &candidates,
        )
        .expect("no candidate here can serve H264 decode, which is not an error");
        assert!(matches!(selected, Selected::Software));
    }

    #[test]
    fn prefer_hardware_picks_the_first_usable_candidate() {
        let candidates = [absent_desc(), present_desc()];
        let selected = select(
            CodecId::H264,
            HwDirection::Decode,
            HwPreference::PreferHardware,
            &candidates,
        )
        .expect("present_desc is usable");
        match selected {
            Selected::Hardware { backend_name, .. } => assert_eq!(backend_name, "mock-present"),
            Selected::Software => panic!("expected hardware, got software fallback"),
        }
    }

    #[test]
    fn require_absent_backend_errors_instead_of_falling_back() {
        let candidates = [present_desc()]; // VideoToolbox, not Vulkan
        let err = select(
            CodecId::H264,
            HwDirection::Decode,
            HwPreference::Require(HwDeviceType::Vulkan),
            &candidates,
        )
        .expect_err("Require must fail loudly when the named backend is absent");
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn require_present_backend_succeeds() {
        let candidates = [present_desc()];
        let selected = select(
            CodecId::H264,
            HwDirection::Decode,
            HwPreference::Require(HwDeviceType::VideoToolbox),
            &candidates,
        )
        .expect("VideoToolbox is present and supports H264 decode");
        assert!(matches!(selected, Selected::Hardware { .. }));
    }

    #[test]
    fn downloaded_frame_is_a_real_software_frame() {
        use vaco_limits::Limits;

        let surface: Arc<dyn HwSurface> = Arc::new(MockSurface);
        let hw = crate::frame::HwFrame::new(PixFmt::VideotoolboxVld, 64, 64, surface);
        assert!(hw.hw_pix_fmt.is_hw());

        let mut budget = Budget::new(Limits::strict());
        let downloaded = hw.download(&mut budget).expect("mock surface always downloads");
        let vaco_frame::FrameData::Video { format, width, height, .. } = downloaded.data else {
            panic!("expected a video frame");
        };
        assert!(!format.is_hw());
        assert_eq!((width, height), (64, 64));
    }
}
