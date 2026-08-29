# `vaco-hw-core`

Layer 9. The hardware-acceleration framework: device/frame contexts, the
`HwAccel` trait a codec crate implements against, hardware-resident frame
storage, and selection with software fallback.

This crate contains **no backend**. It never talks to a GPU, a driver, or an
OS media API, and it stays `#![forbid(unsafe_code)]` — it is pure
orchestration. A concrete backend (VideoToolbox, Vulkan Video, VA-API, D3D12,
NVDEC) is a separate `vaco-hw-<backend>` crate that implements the traits
here against real `unsafe` bindings; D13 is what permits `unsafe` in a
`vaco-hw-*` crate and nowhere else. `vaco-hw-videotoolbox` is the first one —
see `docs/hw/vaco-hw-videotoolbox.md`. The rest do not exist yet.

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
Backend crates gate their own platform coupling instead: `vaco-hw-videotoolbox`
puts its `objc2-*` dependencies under a `[target.'cfg(target_os = "macos")']`
table rather than behind a Cargo feature, so a non-macOS build (including
`wasm32-unknown-unknown`, confirmed via `cargo check --target
wasm32-unknown-unknown`) never pulls them in and needs no
`xtask/src/wasm.rs` `NATIVE_ONLY` entry at all.

## Dependencies

`vaco-core`, `vaco-limits`, `vaco-frame`, `vaco-pixfmt`, `vaco-codec-core` —
all already-workspace crates. No new external dependency in this crate
itself. Backend crates each bring their own:

| Backend | Candidate crate | Licence | Status |
|---|---|---|---|
| VideoToolbox | `objc2-video-toolbox` + siblings | Zlib/Apache-2.0/MIT | **Adopted**, `vaco-hw-videotoolbox` — see `docs/hw/vaco-hw-videotoolbox.md` and `docs/dependencies.md`. D14.3 names this crate family by name as permitted inside `vaco-hw-*`. |
| Vulkan Video | `ash` | MIT OR Apache-2.0 | Permitted by the same D14.3 naming; not yet built (untestable on this host — no Vulkan Video driver path on macOS). |
| D3D12 Video | `windows` (Microsoft) | MIT OR Apache-2.0 | Permitted by the same D14.3 naming; not yet built (D13 marks it optional pending a Vulkan-Video-on-Windows comparison, and it is untestable on this host regardless). |
| VA-API | none identified | — | D13: "only if Vulkan Video proves insufficient in practice. Prefer not to." Not evaluated. |
| NVDEC/NVENC | none identified | — | Needs a CUDA/NVIDIA Video Codec SDK binding; the one wrapper crate found (`nvidia-video-codec-sdk`) binds NVIDIA's proprietary SDK headers, which is a separate licence question from the wrapper's own MIT — not evaluated. |
