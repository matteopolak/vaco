# `vaco-hw-core`

Layer 9. The hardware-acceleration framework: device/frame contexts, the
`HwAccel` trait a codec crate implements against, hardware-resident frame
storage, and selection with software fallback.

This crate contains **no backend**. It never talks to a GPU, a driver, or an
OS media API, and it stays `#![forbid(unsafe_code)]` — it is pure
orchestration. A concrete backend (VideoToolbox, Vulkan Video, VA-API, D3D12,
NVDEC) is a separate `vaco-hw-<backend>` crate that implements the traits
here against real `unsafe` bindings; D13 is what permits `unsafe` in a
`vaco-hw-*` crate and nowhere else. None of those backend crates exist yet —
each needs a dependency decision (below) the crate owner has not made.

## What it is

| Module | Contents |
|---|---|
| `device` | `HwDeviceType` (which backend), `HwDeviceCaps` (what an opened device can do), `HwDeviceContext` (the opened handle) |
| `frame` | `HwSurface` (the backend-specific payload trait) and `HwFrame` (device-resident frame: geometry + pixel-format tag + a surface, downloadable to a real `vaco_frame::Frame`) |
| `accel` | `HwAccel` (the per-picture decode/encode session trait) and `HwAccelDesc` (a `const`-friendly descriptor, mirroring `vaco_codec_core::ParserDesc`) |
| `select` | `HwPreference` and `select()`, the one function that turns a codec + direction + preference + a candidate list into `Selected::Hardware` or `Selected::Software` |

## How it works

A hardware-resident picture is `HwFrame`, never a variant of
`vaco_frame::Frame::FrameData` — that enum is deliberately closed (see its own
doc), and `vaco-frame` is not this crate's to edit. The only way from an
`HwFrame` to a real `Frame` is `HwFrame::download`, an explicit, budgeted
readback into the device's `readback_pix_fmt` (e.g. `Nv12`) — never a
hardware-tagged `PixFmt`, since `Frame::alloc_video` already refuses to
allocate one.

Selection is one function:

```rust,ignore
let selected = vaco_hw_core::select(
    CodecId::H264,
    HwDirection::Decode,
    HwPreference::PreferHardware,   // or Require(HwDeviceType::Vaapi), or SoftwareOnly
    &candidates,                    // assembled by the caller from whichever vaco-hw-<backend> crates are linked in
)?;
```

`PreferHardware` never fails just because nothing is available — an empty
`candidates` slice (true of every build today, since no backend crate ships
real code) or every candidate failing to probe/support/open all resolve to
`Selected::Software`, silently. Only `Require(device_type)` can fail, and
only when the caller explicitly named a backend that turned out not to be
there — the one case where silence would hide something the caller asked to
be told.

## How to change it

- Add a backend by creating `crates/hw/vaco-hw-<name>/`, implementing
  `HwDeviceContext` and `HwAccel`, and exposing an `HwAccelDesc` for callers
  to add to their candidate list. That crate is where the `unsafe` goes.
- Wiring `select()`'s output into an actual `Decoder`/`Encoder` (a
  `-hwaccel` CLI flag, a registry-visible `HwAccelDesc` list) is deliberately
  not done here — this crate ships the seam, not the integration, since no
  backend exists yet to integrate.
- If `vaco-frame`'s `FrameData` enum ever grows a hardware variant, this
  crate's `HwFrame`/`HwSurface` split is still valid underneath it — the enum
  variant would just wrap `HwFrame` rather than being invented fresh.

## Configuration

None. No feature flags, no env vars — the crate has no OS coupling to gate.
Backend crates will each need their own `cfg(target_os = "...")` gating and,
per D18, an entry in `xtask/src/wasm.rs`'s `NATIVE_ONLY` list once they
actually depend on a platform binding crate.

## Dependencies

`vaco-core`, `vaco-limits`, `vaco-frame`, `vaco-pixfmt`, `vaco-codec-core` —
all already-workspace crates. No new external dependency. The backend crates
this one is designed to be extended by each need one, and none has been
approved yet:

| Backend | Candidate crate | Licence | Notes |
|---|---|---|---|
| VideoToolbox | `objc2-video-toolbox` | MIT | Actively maintained (objc2 project); only path to Apple's media engine, since MoltenVK does not implement Vulkan Video. |
| Vulkan Video | `ash` | MIT OR Apache-2.0 | Pure-Rust Vulkan bindings; widest single-API reach (Linux/Windows/Android). D13's "best single investment". |
| D3D12 Video | `windows` (Microsoft) | MIT OR Apache-2.0 | Optional per D13 — only worth adding if Vulkan Video proves insufficient on Windows. |
| VA-API | none identified | — | D13: "only if Vulkan Video proves insufficient in practice. Prefer not to." |
| NVDEC/NVENC | none identified | — | Needs a CUDA/NVIDIA Video Codec SDK binding; no vetted pure-Rust crate found in the dependency register. |

None of these are in `planning/research/09-dependency-licence-register.md`
today. Adding one is a D10/D11 decision for the repository owner, not
something a crate owner adds unilaterally.
