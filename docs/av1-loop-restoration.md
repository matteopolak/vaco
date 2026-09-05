# AV1 loop restoration

`vaco-codec-av1::restoration` implements the scalar Wiener and self-guided
restoration processes from AV1 sections 7.17.1–7.17.6. The frame header retains
the plane modes and restoration unit sizes from section 5.9.20.

## How it works

`restore_plane` takes two immutable, already-upscaled planes: the deblocked
image before CDEF and the image after CDEF. Pixels inside the current stripe
come from the latter; the two rows on either side come from the former. A
request for a third outside row repeats the second. Frame edges clamp to the
visible dimensions, excluding allocation padding.

Stripes are 64 luma samples high and start eight rows above a superblock
boundary. Unit rows use the same offset; unit columns do not. A trailing
partial unit shorter than half the nominal size merges with the previous
unit. Chroma uses its own dimensions, unit size and vertical subsampling.

Wiener uses horizontal-then-vertical symmetric convolution, with the
bit-depth-dependent intermediate clipping and rounding required by AV1.
Self-guided restoration implements all 16 radius/epsilon sets, odd-row
sampling for the first pass, and the transmitted projection coefficients.
Intermediate arithmetic uses `i64` to cover 12-bit sample sums and products.

The output allocation is charged to `Budget`; neither source is modified.
The entry point validates dimensions, supported bit depths, coefficient
ranges, source samples and the exact number of restoration units.

## Integration boundary

The decoder currently calls `FrameRestoration::check_scope` before decoding
tiles. Frames selecting restoration return a named `Unsupported` error:
their tile-unit entropy syntax is not implemented, and continuing would
misinterpret restoration coefficients as block data. Standalone filter
tests do not establish end-to-end AV1 or Argon conformance.

To integrate, decode section 5.11.57–5.11.58 unit symbols before each
superblock, maintain tile-local coefficient reference state, and populate
the row-major `RestorationUnit` map. Preserve deblocked pixels before CDEF,
upscale both source images, then call restoration after CDEF/superresolution.
Remove the scope refusal only with independent full-frame measurements.
The crate's remaining inter/deblocking/superresolution gaps still apply.

## How to change it

The implementation is in `src/restoration.rs`; `frame_header.rs` owns
`lr_params` parsing. `tests/restoration.rs` covers geometry, parameter
validation and reference pixels. Keep the pre-CDEF source separate when
changing buffering: overwriting it with CDEF output produces errors near
every stripe boundary. Filter across horizontal unit boundaries using
source pixels, even when adjacent units choose different filters.

No performance claims are made for this scalar reference implementation.
Profile the real integrated pipeline before changing its buffering or
arithmetic.

## Configuration and dependencies

There are no environment flags. `PlaneConfig` supplies visible geometry,
8/10/12-bit precision, vertical subsampling and a 32/64/128/256-sample unit
size. `RestorationUnit` supplies each unit's selected filter and parameters.
Production code depends only on the crate's `Plane`, `vaco-core` errors and
`vaco-limits::Budget`.

The independent test oracle is BSD-licensed dav1d revision
`aa09a630ef57ee7d9482ffb7ef355a903dbb5302`, declared in
`provenance/vaco-codec-av1-restoration.toml`. Its scalar filter functions are
called directly by `provenance/vaco-codec-av1-restoration-oracle.c`; the
adapter supplies geometry, pixels and parameters but no filter equations.

## Independent verification

Measured on 2026-09-04: the scalar dav1d comparison passes all 300 nonconstant
plane cases, comparing every one of 2,089,260 output samples exactly. The
matrix covers all 16 SGR parameter sets, four Wiener tap combinations,
8/10/12-bit output, five geometries, different neighboring-unit parameters,
subsampled stripes and distinct pre/post-CDEF source pixels. An additional
48 constant-plane SGR cases compare 3,024 samples against dav1d output.

The constant cases exposed an incorrect test assumption: self-guided
restoration does not always preserve constant input exactly. The specified
rounded reciprocal in the box filter can change it. For example, 10-bit
input 1023 with SGR set 0 and `xqd = [-32, 31]` produces 1022 in both dav1d
and this implementation. The test compares independent reference pixels;
it does not enforce an invented constant-preservation rule.

The five restoration integration tests and two restoration-header tests
pass. These are isolated filter/header results. The Argon entry in
`crates/tool/vaco-corpus/vaco-media.lock` remains a documented gap with no
fetchable stream, and decoder integration remains refused.

## Reproducing the oracle

Download and extract the pinned
[dav1d source archive](https://github.com/videolan/dav1d/archive/aa09a630ef57ee7d9482ffb7ef355a903dbb5302.tar.gz)
into a scratch directory. In its root, create `config.h` with this scalar
configuration:

```c
#define HAVE_ASM 0
#define ARCH_AARCH64 1
#define CONFIG_8BPC 1
#define CONFIG_16BPC 1
```

The configuration records the ARM64 machine used for these measurements;
set the architecture macro for the host when reproducing elsewhere. Set
`dav1d_src` to that extracted directory, then run from the repository root:

```sh
for bits in 8 16; do
  cc -std=c11 -O2 -DBITDEPTH="$bits" \
    -I"$dav1d_src" -I"$dav1d_src/include" \
    provenance/vaco-codec-av1-restoration-oracle.c \
    "$dav1d_src/src/tables.c" -o "/tmp/vaco-restoration-oracle-$bits"
done
/tmp/vaco-restoration-oracle-8 > crates/codec/vaco-codec-av1/tests/fixtures/restoration-dav1d-8.u16le
/tmp/vaco-restoration-oracle-16 > crates/codec/vaco-codec-av1/tests/fixtures/restoration-dav1d-high.u16le
/tmp/vaco-restoration-oracle-8 constant > crates/codec/vaco-codec-av1/tests/fixtures/restoration-constant-8.u16le
/tmp/vaco-restoration-oracle-16 constant > crates/codec/vaco-codec-av1/tests/fixtures/restoration-constant-high.u16le
```

Each output sample is a little-endian `u16`, including 8-bit cases. The
nonconstant outputs contain 1,392,840 and 2,785,680 bytes respectively; the
constant outputs contain 2,016 and 4,032 bytes. SHA-256 checksums:

| Fixture | SHA-256 |
| --- | --- |
| `restoration-dav1d-8.u16le` | `e954d64ad6726702d9347508d7e7982a6eddfe84e11aad361edf1c512a5f761f` |
| `restoration-dav1d-high.u16le` | `1f46e3cc1038ee590638381de786f51a63949d5d9d5c762be34f9980c76c6780` |
| `restoration-constant-8.u16le` | `222e3f56d30ec897fa902cc63b2d21e63b2d7555443b97c85e1af82241f0f56e` |
| `restoration-constant-high.u16le` | `2861f658c08a5231da898e1a0944524816a66aa95e7011d3037bca0234af8bd2` |

Run the regression tests with `CARGO_INCREMENTAL=0 cargo test -p
vaco-codec-av1 --test restoration --target-dir <private-target> -j 1` and
`CARGO_INCREMENTAL=0 cargo test -p vaco-codec-av1 --lib restoration
--target-dir <private-target> -j 1`.
