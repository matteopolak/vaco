# `vaco-codec-dsp-fmtconvert`

Layer 3. Fused scale/widen/narrow sample conversions for codec-internal hot
loops (D-05, issue #122).

## What it is

A fixed set of per-sample conversions a decoder needs between its internal
working representation (usually `f32` or a fixed-point `i32`) and the
sample types it hands onward: `int16_to_float`, `int32_to_float`,
`int32_to_float_fmul_scalar` (widen-and-scale in one pass),
`float_to_int16`, `float_to_int32`, plus `interleave_f32`/`deinterleave_f32`
and their `i16` counterparts for going between one packed buffer and N
per-channel planar slices.

## How it works

Every function is a plain loop over `.zip()`, processing
`min(dst.len(), src.len())` elements rather than asserting equal lengths —
mismatched-length inputs truncate instead of panicking. Rounding
(`clip_i16`/`clip_i32`/`clip_u8`) is round-half-away-from-zero with
saturation, a design choice documented in the crate's own module docs
rather than a contract measured against the reference binary: nothing this
crate computes is independently observable through `ffmpeg`'s command-line
surface, since it runs entirely inside a decoder before a container or
filter ever sees the samples.

## How to change it

Two files: `convert.rs` (per-sample numeric conversions) and
`interleave.rs` (packed/planar layout). Add a new conversion pair as its
own `pub fn` following the existing naming (`<src>_to_<dst>`); if it needs
a different rounding rule than the crate default, say so in its own doc
comment rather than changing the shared `clip_*` helpers, since those are
relied on by every other function here.

**Gotcha**: this crate is deliberately *not* the same thing as
`vaco-resample`'s `SampleFmt`-driven conversion matrix (D19) — see the
crate root doc for the full reasoning. Do not add a `SampleFmt` parameter
here; that is a sign the function belongs in `vaco-resample` instead.

## Configuration

None — pure functions, no state, no allocation.

## Dependencies

`vaco-simd` provides the portable-tier candidates for `int16_to_float` and
`int32_to_float`. They are exact against the scalar loops and remain wired to
the differential harness, but the public `i16` path stays scalar because the
candidate has measured slower on this machine. No current codec caller uses
this crate yet: audio decoders output `f32` planar directly and let
`vaco-resample` handle final format conversion.
