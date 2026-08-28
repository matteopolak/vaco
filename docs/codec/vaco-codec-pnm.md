# `vaco-codec-pnm`

Layer 4. PNM family (pbm/pgm/ppm/pam/pfm/phm) decode and encode.

## What it is

Six sibling NetPBM formats: a text or keyword header naming dimensions and a
sample layout, then one raster. [`netpbm`] covers pbm/pgm/ppm (`P1`-`P6`),
[`pam`] covers PAM (`P7`), [`floatmap`] covers PFM/PHM. `ImageDecoder`/
`ImageEncoder` wrap any of the six in `vaco_codec_core::SendReceive` via a
function pointer.

Written from the NetPBM format descriptions (`netpbm.sourceforge.net`),
cross-checked against the reference codec's observable byte behaviour.

## How it works

- **pbm/pgm/ppm**: `P1`/`P2`/`P3` are ASCII, `P4`/`P5`/`P6` are binary; both
  are decoded, only binary is encoded (the reference never emits ASCII). PBM
  is 1 bit/pixel into `monowhite`. PGM/PPM pick `gray8`/`rgb24` for
  `maxval <= 255` or `gray16be`/`rgb48be` otherwise — **always big-endian**
  above 255, regardless of the source frame's own endianness (measured).
  A `maxval` other than 255/65535 is rescaled to fill the output range;
  see the module doc in `src/netpbm.rs` for the one unresolved edge (an
  exact-tie rounding direction that two probes disagreed on).
- **pam**: a `WIDTH`/`HEIGHT`/`DEPTH`/`MAXVAL`/`TUPLTYPE` keyword header.
  Five tuple types are mapped: `GRAYSCALE`, `GRAYSCALE_ALPHA`, `RGB`,
  `RGB_ALPHA`, and `BLACKANDWHITE` — the last decodes to the *bit-packed*
  `monoblack`, not a literal copy of PAM's own byte-per-sample raster
  (measured by comparing a `BLACKANDWHITE` PAM and a `P4` PBM built from the
  same image).
- **pfm/phm**: `Pf`/`PF`/`Ph`/`PH` magic selects grayscale/colour and
  32-bit/16-bit float samples. Rows are stored **bottom row first**; the
  scale field's sign picks little/big-endian, and its magnitude is not
  otherwise interpreted. Half-float samples are copied as opaque 16-bit
  lanes — no float16 arithmetic is needed anywhere in this crate.

## How to change it

Each module owns its header parsing and pixel-format mapping independently;
`src/reader.rs` is the shared whitespace/`#`-comment tokenizer, and
`src/bits.rs` is the shared 1-bit-per-pixel packer both PBM and PAM's
`BLACKANDWHITE` use.

## Configuration

`vaco_limits::Limits` bounds every decode: dimensions come from the header,
validated by `vaco_frame::Frame::alloc_video` before a pixel is touched.

## Dependencies

`vaco-codec-core`, `vaco-frame`/`vaco-pixfmt`/`vaco-pool`, `vaco-packet`,
`vaco-limits`.

## Registration

`vaco-component.toml` registers all twelve descriptors (`PBM_DECODER`/
`PBM_ENCODER` … `PHM_DECODER`/`PHM_ENCODER`, via the `pnm_codec!` macro in
`src/lib.rs`) under the six `CodecId` variants C-13 added
(`Pbm`/`Pgm`/`Ppm`/`Pam`/`Pfm`/`Phm`), feature `codec-pnm` (on by default).
Reachable as `-c:v pbm`/`pgm`/`ppm`/`pam`/`pfm`/`phm` through
`vaco_registry::encoder_by_name`; each `ImageDecoder`/`ImageEncoder::send`
stamps `pts` from its input, since the per-format `decode`/`encode`
functions are pure over bytes/pixels alone and have none of their own.

`vaco -i in.ppm -c:v qoi -f null -` (a cross-crate leg: this crate decodes,
`vaco-codec-qoi` encodes) was verified end to end, including a byte-identical
QOI output against `ffmpeg`'s own encoder — see `vaco-codec-qoi`'s doc file.
`vaco-demux-image2` does not yet map most of these formats' extensions to
their new `CodecId`, so `-i in.pgm` alone currently reports "no known input
codec" until that demuxer is updated — a gap in that crate, not this one; see
`planning/TECH-DEBT.md`'s C-13 entry.

## Testing

20 unit tests across the three modules, including cross-checks (ASCII vs
binary agreement, PAM `BLACKANDWHITE` vs PBM bit convention, PFM row-order).
Differential verification against `ffmpeg` covered all 13 realistic
combinations (pbm, pgm×2, ppm×2, pam×4, pfm×2, phm×2) and found every one
byte-identical on encode and pixel-identical on decode — finding 51 in
`planning/CONFORMANCE-FINDINGS.md`.

A `parse_pnm` fuzz target runs all six decoders over arbitrary bytes.
