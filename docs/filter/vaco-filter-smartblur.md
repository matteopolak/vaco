# `smartblur`

`smartblur` is the edge-aware blur/sharpen filter in `vaco-filter-blur`. It
operates independently on luma, chroma, and alpha planes and protects strong
local edges with a threshold.

## What it is

The implementation provides the reference filter's nine options: radius,
strength, and threshold for each of luma, chroma, and alpha, including the
`lr`/`ls`/`lt`, `cr`/`cs`/`ct`, and `ar`/`as`/`at` aliases. It supports the
workspace's 8-bit addressable video formats, like the other blur filters.

The implementation is an algorithmic core, not a claim of framecrc identity
with the reference. A black-box impulse probe showed that the reference's
weighting is not a plain box average: with a 100-valued centre impulse,
`radius=1:strength=1:threshold=0` produced `[0, 27, 45, 27, 0]`, while the
three-tap box average produces `[0, 33, 33, 33, 0]`. The implementation keeps
that boundary explicit rather than silently presenting the approximation as
reference-compatible.

## How it works

For an enabled plane, the kernel computes a replicate-border square box
average. Positive strength blends the original sample toward that average;
negative strength pushes it away, giving a sharpening mode. When the positive
threshold is exceeded by the absolute original-versus-average difference, the
original sample is retained. A negative radius disables the plane, matching
the default disabled chroma and alpha planes.

The filter is registered by `BlurRegistry` and runs through the normal
`FrameFilter`/`Simple` adapter. Plane selection follows the format's plane
layout: plane zero uses luma parameters, the final alpha plane uses alpha
parameters when present, and remaining planes use chroma parameters.

## How to change it

Touch `crates/filter/vaco-filter-blur/src/smartblur.rs` for option parsing or
the kernel. Keep the flat-field and identity tests: they are independent
properties of the blend, not a second transcription of the implementation.
Update the crate-level blur documentation when the reference weighting is
solved or the supported format scope changes. Do not register `sab` until it
has a real constructor and tests.

## Configuration

The defaults are luma `(radius=1, strength=1, threshold=0)` and disabled
chroma/alpha `(radius=-0.9, strength=-2, threshold=-31)`. Radius ranges from
`0.1` to `5` for luma and `-0.9` to `5` for chroma/alpha; strength ranges from
`-1` to `1` for luma and `-2` to `1` for chroma/alpha; thresholds use the
reference ranges `-30..=30` for luma and `-31..=30` for chroma/alpha.

No environment variables or feature flags affect the filter.

## Dependencies

- `vaco-filter-core` supplies `FrameFilter`, `FilterDesc`, and the `Simple`
  adapter.
- `vaco-filter-graph` constructs the registered filter instance.
- `vaco-frame` and `vaco-pixfmt` provide frame and plane access.
- `vaco-filter-blur::common` supplies 8-bit validation, metadata copying, and
  the measured replicate-border box pass.

## Verification

The blur crate's registry test constructs `smartblur` with default options.
Dedicated unit tests cover disabled default planes, zero-strength identity,
movement toward the box average, threshold edge protection, and the flat-field
fixed point. The full scoped run passed 37 tests and the crate doctests.

The shared box-pass benchmark was rerun in a private `CARGO_INCREMENTAL=0`
target with one build job: for a 640x480 plane at radius 16, the fast
separable path measured 719.8 microseconds median versus 230.1 milliseconds
for the retained brute-force reference. The crate has no hand-written SIMD
path; macOS process-total cycle counters are unavailable, so no cycle ratio is
claimed here.
