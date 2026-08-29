# `vaco-hw-videotoolbox`

Layer 9. VideoToolbox hardware decode: the first real `vaco-hw-*` backend,
and the only one this tree can build and test end to end (macOS is the
development machine; Vulkan Video/D3D12/VA-API/NVDEC all need a driver path
that does not exist here).

## What it is

H.264 decode through `VTDecompressionSession`, driven synchronously (no
async output reordering — `decode_flags` never sets the asynchronous or
temporal-processing bits, so `VTDecompressionSessionDecodeFrameWithOutputHandler`'s
own callback has already run by the time the call returns). One session per
stream, built once from its SPS/PPS and reused picture to picture, exactly
the shape a real `Decoder` implementation would hold.

Not implemented: HEVC/AV1/ProRes, asynchronous/reordered output, and any
wiring into an actual `vaco_codec_core::Decoder` or a `-hwaccel` CLI flag —
there is no call site for that yet (see `vaco-hw-core`'s own doc).

## How it works

| Module | Contents |
|---|---|
| `nal` | `split_annex_b` / `nal_unit_type` — Annex-B parsing with start codes stripped and emulation-prevention bytes left intact, which `CMVideoFormatDescriptionCreateFromH264ParameterSets` requires |
| `device` | `probe`/`open`/`DESC` — VideoToolbox has no separate device to enumerate, so "probing" only checks the platform; per-codec capability is discovered at session creation instead |
| `session` | `VideoToolboxDecoder` (implements `HwAccel`) and `VideoToolboxSurface` (implements `HwSurface`, downloads a `CVPixelBuffer` to a real `Nv12` `Frame`) |

The one non-obvious API fact this crate is built around: **a sample buffer's
format description must match the session's**, exactly. Passing `None` (on
the theory that the session already knows the format) fails with
`kVTFormatDescriptionChangeNotSupportedErr` — measured directly against a
real decode call, not assumed from documentation. `VideoToolboxDecoder`
therefore keeps its `CMFormatDescription` alongside the session and passes it
into every sample buffer it builds.

`decode_slice` accepts one NAL unit at a time (no start code, no length
prefix — exactly what a length-prefixed/AVCC container already hands a
caller) and re-frames it as 4-byte-length-prefixed AVCC data internally,
matching the `nal_unit_header_length = 4` the format description was built
with.

## How to change it

- Adding HEVC support means a second `create_hevc_format_description` (the
  HEVC sibling API needs VPS+SPS+PPS rather than SPS+PPS) and a
  `CodecId::Hevc` branch in `VideoToolboxDecoder::new` — the session/decode
  machinery below that point is codec-agnostic.
- Wiring this into an actual decode pipeline needs a `-hwaccel` call site
  that does not exist anywhere in this workspace yet; that is out of this
  crate's scope, not a gap in it.
- The `unsafe` surface is 19 sites (`cargo xtask unsafe-audit`), documented
  individually in `session.rs`'s module doc and at each call site. Adding a
  new CoreMedia/CoreVideo/VideoToolbox call should add exactly one more,
  with its own `SAFETY` comment — the count is meant to stay legible, not
  just small.

## Configuration

None. The crate compiles to an empty shell (only `accel_desc() -> None`) on
anything other than `target_os = "macos"`, because the `objc2-*` dependencies
themselves sit behind a `[target.'cfg(target_os = "macos")']` table in
`Cargo.toml` rather than a Cargo feature — confirmed clean on
`wasm32-unknown-unknown`.

## Dependencies

`objc2`, `block2`, `objc2-video-toolbox`, `objc2-core-media`,
`objc2-core-video`, `objc2-core-foundation` — all Zlib/Apache-2.0/MIT, all
from the same actively-maintained `objc2` project (madsmtm/objc2), named by
family in `planning/00-decisions.md` D14.3 as a permitted pure-Rust OS-API
binding for `vaco-hw-*`. See `docs/dependencies.md` for the full adoption
record. No vendored or compiled C: `objc2`'s own `build.rs` only emits
`cargo:rustc-cfg` target-triple checks, and no crate in this dependency
subtree pulls in `cc`/`bindgen`/`cmake` (checked directly, not assumed).

## Testing

`tests/videotoolbox_decode.rs` decodes a real 64x64 H.264 baseline keyframe
(`tests/fixtures/tiny_baseline_64x64.h264`, generated once via
`ffmpeg -f lavfi -i testsrc -c:v libx264 -profile:v baseline`, D6 black-box
tooling) through a real `VTDecompressionSession` on this machine and checks
structural correctness — dimensions, pixel format, non-degenerate pixel
content — not byte-exactness against any reference, which D17 does not ask
for here and which this test has no reference frame to check against in any
case. A second test feeds deliberately-malformed slice data through the same
path and checks it fails cleanly (`Err`, not a panic or a hang).
