# AV1 super-resolution

## What it is

`vaco-codec-av1` applies AV1's scalar super-resolution process to intra
frames that set `use_superres`. It converts the CDEF-filtered, downscaled
reconstruction planes into the signalled `UpscaledWidth` before emitting the
output `Frame`.

## How it works

`decode_frame` reconstructs Mi-padded planes, applies CDEF, then calls
`superres::upscale_picture` before copying visible pixels into `vaco-frame`.
For each Y, U, and V row, `superres::upscale_plane` uses §7.16's 14-bit
fixed-point phase, the eight-tap `Upscale_Filter[64][8]`, signed `Round2`,
and bit-depth clipping. The phase uses the visible downscaled plane width;
tap extension instead clamps to the Mi-padded reconstruction width. Keeping
those bounds separate is essential on a right edge that had to be padded for
tile decoding.

Chroma dimensions use positive-integer `Round2` (ceiling division) by the
sequence's horizontal and vertical subsampling factors. The resampler runs
after CDEF, as §7.16 requires. Loop restoration remains a separately gated
later stage.

`tests/superres.rs` has two independent checks:

- `superres-dav1d.u16le` holds 18 scalar outputs from pinned BSD-2-Clause
  dav1d `aa09a630ef57ee7d9482ffb7ef355a903dbb5302`, covering 8/10/12-bit
  samples, nontrivial ratios, and visible widths smaller than Mi padding.
- `superres-96x64.obu` is a flat, one-frame `libsvtav1` AV1 OBU with superres
  enabled. Its complete 96x64 YUV420 output is compared byte-for-byte to a
  pinned-dav1d decode in `superres-96x64_ref.yuv`; this checks full-frame
  pipeline geometry, plane layout, and frame count. The non-flat arithmetic
  coverage is deliberately supplied by the 18-record scalar oracle above.

The oracle generator is `scripts/gen-av1-superres-oracle.py`; it only
extracts the permitted pinned dav1d scalar oracle into a throwaway C build.
Production code and its 64x8 table are transcribed from `aom-av1-spec §7.16`.
The generator's source/revision, table provenance, and clean-room boundary
are recorded in `provenance/vaco-codec-av1-superres.toml`.

The full-frame fixture was generated as a one-frame OBU with `ffmpeg`'s
`libsvtav1` binary and decoded as raw YUV420 by its `libdav1d` binary:

```text
ffmpeg -y -v error -f lavfi -i "nullsrc=size=96x64,geq=lum=100:cb=128:cr=128" \
  -frames:v 1 -pix_fmt yuv420p -c:v libsvtav1 -qp 36 \
  -svtav1-params superres-mode=1:superres-kf-denom=12:enable-dlf=0:enable-cdef=0:enable-restoration=0:enable-tf=0:enable-kf-tf=0:scm=0:enable-intrabc=0:aq-mode=0:lookahead=0 \
  -f obu superres-96x64.obu
ffmpeg -y -v error -c:v libdav1d -i superres-96x64.obu -frames:v 1 \
  -pix_fmt yuv420p -f rawvideo superres-96x64_ref.yuv
```

The checked-in OBU is 29 bytes (`fe85cc2c1fdde08081ebcb34b5e2f80984492fea34c05f3f362dc764d88609b0`);
the reference is 9,216 bytes (`6625904ba25e001eb074eb247253d1e96e68c4f08d2324467747910addd72de4`).

## How to change it

Keep the table, `SCALE_BITS`, `EXTRA_BITS`, and filter rounding together in
`crates/codec/vaco-codec-av1/src/superres.rs`; a phase or orientation change
must be validated against all 18 independent oracle records. When changing
pipeline order, retain the CDEF → superres relationship and preserve the
Mi-padded source bound until resampling finishes. Update the `[[table]]`
entry in `provenance/vaco-codec-av1-superres.toml` whenever the 64x8 table is
renamed or moved.

`frame_size_with_refs()` is intentionally not implemented here. The decoder
rejects all inter frames before reference lookup, and its parser's partial
inter path also stops before that syntax. AV1 §5.9.7/§6.8.6 says a referenced
frame supplies `UpscaledWidth`, then `superres_params()` runs; that remains an
explicit unreachable remainder until inter references and their frame store
are implemented. Do not treat the intra-only superres result as completing
that interaction.

The issue's Argon profiles are not available in the checked-in media lock, so
they are not a conformance claim or a reason to close the broader issue.

## Configuration

No option enables this stage independently: the AV1 frame header's
`use_superres` bit and `superres_denom` determine it. `Limits` bounds output
plane allocation and fuel; every output sample consumes one fuel unit.

## Dependencies

The implementation depends on `vaco-parse-av1` for the parsed frame size and
sequence colour configuration, `vaco-limits` for allocation/fuel accounting,
and `vaco-frame`/`vaco-pixfmt` for final output. Its normative source is AV1
Bitstream & Decoding Process Specification v1.0.0 with Errata 1,
`aom-av1-spec §7.16`; pinned dav1d is test-only.
