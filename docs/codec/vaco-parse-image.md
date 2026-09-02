# `vaco-parse-image`

Layer 4. Header-only stream description for still images: what `ffprobe`
prints as `width`, `height` and `pix_fmt`, without decoding a pixel.

---

## What it is

One `vaco_codec_core::Parser` per still-image format, all of them the same
`ImageParser<H>` wrapper (`src/parser.rs`) over a per-format `ImageHeader`
reader. A demuxer never names this crate: it asks `ParserProvider` for a
parser by `CodecId`, and `vaco-registry`'s generated `PARSERS` table answers
(D14.1).

Nineteen formats: PNG, JPEG, GIF, BMP, TIFF and WebP read their own headers
here; PCX, TGA, SGI, XWD, XBM, QOI, PBM, PGM, PPM, PAM, PFM, PHM and JPEG-LS
forward to their decoder crate's reader through `src/still.rs`.

## Why it matters more than "probe output"

A stream whose `pix_fmt` is absent is not merely under-reported. `vaco-cli`'s
`converter_target` reads `None` as "no opinion" and answers with the
encoder's first accepted format, so the decode path *acts* on the silence: a
colour P6 PPM whose true first pixel is `(128, 0, 255)` came out as
`(67, 67, 67)` — exactly its luma — because nothing described the stream as
RGB. Nine formats were in that state until `still.rs` existed.

## The two shapes, and when to use which

| | Header read | Used by |
|---|---|---|
| `bmp.rs`, `gif.rs`, `jpeg.rs`, `png.rs`, `tiff.rs`, `webp.rs` | in this crate | formats whose decoder lives behind a crate this one does not depend on |
| `still.rs` | in the decoder crate | formats whose decoder is a same-layer dependency |

**Prefer `still.rs`.** Its whole point is that the reported pixel format and
the allocated one are the *same expression*, not two that have to agree. A
second header reader here would be the two-lists-that-must-match shape
`CLAUDE.md` warns about, and the failure it produces is the worst kind: a
probe reporting `rgb24` while the decoder emits something else looks finished.

## How to add a format

1. If the decoder crate is (or can be) a dependency, give it a
   `pub fn parameters(data: &[u8]) -> Option<CodecParameters>` that calls the
   same `read_header` and the same pixel-format helper `decode` calls —
   factor that helper out rather than repeating the match.
2. One `delegate!(Ty => path::parameters)` line in `src/still.rs`.
3. One `PARSER_<NAME>` const in `src/lib.rs`, one `[[component]]` block in
   `vaco-component.toml`, then `cargo run -p xtask -- gen-registry`.
4. A row in `tests/reference_images.rs`, with an `ffmpeg`-written fixture and
   the raw planes `ffmpeg` decodes from it.

## Tests

`tests/reference_images.rs` is the one that can fail usefully. For every
fixture it asserts both halves: the parameters this crate reports equal what
`ffprobe` reports, *and* the decoder's planes equal `ffmpeg`'s own
`-f rawvideo` output byte for byte.

Fixtures are 13x7 and 33x5 — odd in a dimension, because PCX, XWD, XBM and SGI
all pad rows and a 64x48 fixture cannot express a padding bug in any of them —
plus a four-channel SGI carrying a real alpha ramp, because an opaque `gbrap`
and a `gbrp` with the alpha dropped convert to identical RGBA.

Never regenerate a `.raw` from our own encoder. A self round-trip is how a
completely broken FFV1 stayed green in this tree.

## Known gaps

* **JPEG-LS parameters only.** `vaco-codec-jpegls` fails with
  `UnexpectedEof` on some of the reference encoder's own output (12x8 and 20x8
  decode; 16x8 and 32x8 do not), so `still::JpegLs` is covered by a parameter
  assertion and no pixel assertion.
* **EXR** has no parser here and `exr_pipe` carries no `CodecId`, so an EXR
  stream reaches the CLI undescribed. `vaco-codec-exr` also decodes to
  `rgbaf32le` where the reference reports `gbrpf32le`, so a parser would have
  to state one or the other and diverge from something.

## Configuration

None. The crate-wide `parse-image` registry feature gates every component.

## Dependencies

`vaco-bitstream`, `vaco-codec-core`, `vaco-core`, `vaco-limits`,
`vaco-packet`, `vaco-pixfmt`, `vaco-color`; `vaco-parse-vpx` for WebP's lossy
sub-format; and `vaco-codec-image-simple`, `-pnm`, `-qoi`, `-jpegls` for the
header readers `still.rs` forwards to. No external runtime dependencies.
