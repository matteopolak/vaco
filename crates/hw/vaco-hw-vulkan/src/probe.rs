//! Real `ash` calls: load the Vulkan loader, create an instance, and check
//! whether any physical device exposes the Vulkan Video H.264 decode chain.

use ash::vk;

use vaco_core::{Error, Result};
use vaco_hw_core::{HwDeviceCaps, HwDeviceType};

use crate::DECODE_CODECS;

/// The three device extensions Vulkan Video H.264 decode needs, checked
/// together — a device advertising only some of them cannot actually decode.
const REQUIRED_EXTENSIONS: [&str; 3] = [
    "VK_KHR_video_queue",
    "VK_KHR_video_decode_queue",
    "VK_KHR_video_decode_h264",
];

/// Exactly what [`probe`] observed, kept distinct from a plain `Result` so a
/// caller (or a test) can tell "no Vulkan here at all" apart from "Vulkan is
/// here, but no device does video decode" — two different facts a `-hwaccel
/// vulkan` diagnostic would want to report differently, even though both
/// collapse to the same [`Error::Unsupported`] for [`ProbeOutcome::into_caps`].
#[derive(Debug)]
pub enum ProbeOutcome {
    /// No Vulkan loader could be found (`ash::Entry::load` failed) — the
    /// ordinary "this machine has no Vulkan at all" case, expected on most
    /// machines this code runs on.
    NoLoader,
    /// A loader was found but `vkCreateInstance` itself failed.
    InstanceCreationFailed(ash::vk::Result),
    /// An instance was created, but no physical device advertises all of
    /// [`REQUIRED_EXTENSIONS`].
    NoVideoDecodeCapableDevice,
    /// At least one physical device supports H.264 decode.
    Capable,
}

impl ProbeOutcome {
    /// # Errors
    /// [`Error::Unsupported`] for every variant except [`Self::Capable`].
    pub fn into_caps(self) -> Result<HwDeviceCaps> {
        match self {
            Self::Capable => Ok(HwDeviceCaps {
                device_type: HwDeviceType::Vulkan,
                decode_codecs: DECODE_CODECS.to_vec(),
                encode_codecs: Vec::new(),
                max_dimension: 8192,
                readback_pix_fmt: vaco_pixfmt::PixFmt::Nv12,
            }),
            Self::NoLoader => Err(Error::Unsupported("no Vulkan loader found on this system")),
            Self::InstanceCreationFailed(_) => Err(Error::Unsupported(
                "Vulkan loader found, but instance creation failed",
            )),
            Self::NoVideoDecodeCapableDevice => Err(Error::Unsupported(
                "a Vulkan instance was created, but no physical device supports \
                 VK_KHR_video_decode_h264 (this is `MoltenVK`'s documented status on macOS)",
            )),
        }
    }
}

/// Probe this machine for a Vulkan Video H.264-decode-capable device.
///
/// Every step is real: this actually calls `ash::Entry::load`, actually
/// creates a `VkInstance`, and actually enumerates physical devices and
/// their extension lists. Nothing here is simulated. What it finds depends
/// entirely on the machine it runs on — see the crate doc for what this
/// development machine specifically was observed to do.
#[must_use]
pub fn probe() -> ProbeOutcome {
    // SAFETY: `Entry::load`'s own safety contract is that `dlopen`ing a
    // native library is inherently unsafe, and that no Vulkan function
    // reached through `entry` may be called after `entry` is dropped —
    // upheld here because every call below happens before `entry` goes out
    // of scope at the end of this function, and `entry` is not returned.
    let Ok(entry) = (unsafe { ash::Entry::load() }) else {
        return ProbeOutcome::NoLoader;
    };

    let app_info = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_1);
    let create_info = vk::InstanceCreateInfo::default().application_info(&app_info);

    // SAFETY: `entry` is live; `create_info` borrows `app_info`, which
    // outlives this call; `None` for allocation callbacks is the ordinary
    // "use the default allocator" case every `ash` example uses.
    let instance = match unsafe { entry.create_instance(&create_info, None) } {
        Ok(instance) => instance,
        Err(err) => return ProbeOutcome::InstanceCreationFailed(err),
    };
    // Ensures `destroy_instance` runs on every exit path below, including
    // the early `return`s inside the loop.
    let instance = OwnedInstance(instance);

    // SAFETY: `instance` is live for the duration of this call.
    let Ok(physical_devices) = (unsafe { instance.0.enumerate_physical_devices() }) else {
        return ProbeOutcome::NoVideoDecodeCapableDevice;
    };

    for device in physical_devices {
        // SAFETY: `instance` is live; `device` came from
        // `enumerate_physical_devices` on this same instance, immediately
        // above.
        let Ok(extensions) = (unsafe { instance.0.enumerate_device_extension_properties(device) })
        else {
            continue;
        };
        let names: Vec<&str> = extensions
            .iter()
            .filter_map(|ext| ext.extension_name_as_c_str().ok())
            .filter_map(|s| s.to_str().ok())
            .collect();
        if REQUIRED_EXTENSIONS
            .iter()
            .all(|req| names.iter().any(|n| n == req))
        {
            return ProbeOutcome::Capable;
        }
    }
    ProbeOutcome::NoVideoDecodeCapableDevice
}

/// Guarantees `vkDestroyInstance` runs exactly once, on every exit path
/// (including the early returns inside [`probe`]'s device loop), without
/// requiring each of them to remember to call it by hand.
struct OwnedInstance(ash::Instance);

impl Drop for OwnedInstance {
    fn drop(&mut self) {
        // SAFETY: `self.0` was created by `entry.create_instance` in
        // `probe` and has not been destroyed yet — this is the only place
        // that destroys it, and it runs exactly once (`Drop` semantics).
        unsafe { self.0.destroy_instance(None) };
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    /// The one thing this test can assert on every machine it might run
    /// on: probing never panics, and always resolves to one of the four
    /// documented outcomes. It cannot assert *which* outcome, because that
    /// depends on whether the machine running it has Vulkan at all — see
    /// the crate doc.
    #[test]
    fn probe_never_panics_and_produces_a_documented_outcome() {
        let outcome = probe();
        match outcome {
            ProbeOutcome::NoLoader
            | ProbeOutcome::InstanceCreationFailed(_)
            | ProbeOutcome::NoVideoDecodeCapableDevice
            | ProbeOutcome::Capable => {}
        }
    }

    /// `select`'s own contract (`vaco-hw-core`) is that `PreferHardware`
    /// falls back to software when a candidate is unusable — proven here
    /// against this crate's *real* probe function, not a mock, which is
    /// the fallback guarantee that actually matters: this development
    /// machine has no Vulkan Video-capable device, so this is a genuine
    /// "hardware absent" run, not a simulated one.
    #[test]
    fn falls_back_to_software_when_this_machine_has_no_video_decode_device() {
        let candidates = [crate::accel_desc()];
        let selected = vaco_hw_core::select(
            vaco_codec_core::CodecId::H264,
            vaco_hw_core::HwDirection::Decode,
            vaco_hw_core::HwPreference::PreferHardware,
            &candidates,
        )
        .expect("PreferHardware never errors");
        assert!(
            matches!(selected, vaco_hw_core::Selected::Software),
            "this development machine has no Vulkan Video H.264 decode device"
        );
    }
}
