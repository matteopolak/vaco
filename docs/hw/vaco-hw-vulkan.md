# `vaco-hw-vulkan`

Layer 9. Vulkan Video device/capability probing, built against `ash`.
Implements H-06a's scope only (instance/device/queue bring-up and a
video-capability query) — no decode session, which is real, substantially
larger work (H-06b) not attempted this pass.

## What it is

`probe()` performs real `ash` calls: load the Vulkan loader (`ash::Entry::load`,
which `dlopen`s the system loader rather than requiring one at compile time),
create a `VkInstance`, enumerate physical devices, and check each one's
device extensions for `VK_KHR_video_queue` + `VK_KHR_video_decode_queue` +
`VK_KHR_video_decode_h264` — the chain a device needs to actually decode
H.264 through Vulkan Video. `ProbeOutcome` distinguishes four real, distinct
outcomes (no loader / instance creation failed / no capable device / capable)
rather than collapsing everything to one boolean, so a future diagnostic can
say which one happened.

## Honesty about what is and is not verified

This crate is written and built on macOS. `probe()` is measured, not
assumed, to return `ProbeOutcome::NoLoader` on this machine — there is no
properly configured system-wide Vulkan loader here (only application-bundled
`libMoltenVK.dylib`/`libvulkan.dylib` copies inside unrelated apps, none on
the default `dlopen` search path). That means even ordinary Vulkan instance
creation is unexercised end to end on this development machine, let alone
the video-decode extension check, and even on a machine where it *is*
exercised, D13's own backend table already records that MoltenVK (macOS's
only Vulkan implementation) does not implement Vulkan Video at all. This
crate is built against the Vulkan specification and `ash`'s real API, and
its fallback behaviour is tested against its own real `probe()` — but it has
never observed a real Vulkan Video-capable device, on this host or any
other.

## How it works

| Module | Contents |
|---|---|
| `probe` | `probe()`/`ProbeOutcome` — the real device/capability query, plus `OwnedInstance` (a tiny `Drop` guard so `vkDestroyInstance` runs on every exit path of the enumeration loop) |
| `lib` | `VulkanDevice` (`HwDeviceContext`), `accel_desc()` (the `HwAccelDesc` a caller adds to `vaco_hw_core::select`'s candidate list) |

## How to change it

- A decode session (H-06b) is a new module built on top of `probe()`'s
  result: a `VkVideoSessionKHR`, DPB slot management, and the
  bitstream-to-`VkVideoDecodeInfoKHR` mapping for each slice. None of that
  exists here.
- If Vulkan Video ever needs testing for real, it needs a Linux or Windows
  machine with a real Vulkan Video driver — this crate's own doc says so
  plainly rather than implying macOS coverage it does not have.

## Configuration

None. `ash`'s `loaded` feature (on by default) means no build-time linking
requirement — the crate always compiles; what `probe()` finds at runtime
depends entirely on the machine.

## Dependencies

`ash` 0.38 (MIT OR Apache-2.0), pure-Rust Vulkan bindings, permitted by name
in `planning/00-decisions.md` D14.3. See `docs/dependencies.md` for the full
adoption record, including the wasm wall this crate hits (`xtask/src/wasm.rs`
`NATIVE_ONLY`: `ash`'s `loaded` feature pulls in `libloading`, which does not
support `wasm32-unknown-unknown` — measured directly, not assumed).

## Testing

`probe::tests::probe_never_panics_and_produces_a_documented_outcome` asserts
only that `probe()` resolves to one of its four documented outcomes without
panicking — the one thing true on every machine regardless of what Vulkan
support it has. `probe::tests::falls_back_to_software_when_this_machine_has_no_video_decode_device`
runs this crate's real `HwAccelDesc` through `vaco_hw_core::select` under
`HwPreference::PreferHardware` and asserts `Selected::Software` — a genuine,
non-mocked proof that the fallback path works on a machine with no capable
device, which is the guarantee that matters most (see `vaco-hw-core`'s own
doc and H-01's closing comment).
