# `vaco-codec-image-simple`

Layer 4. BMP, PCX, TGA, SGI, XWD, XBM decode and encode.

## What it is

Six unrelated-on-the-wire formats sharing one shape: a fixed or near-fixed
binary header naming dimensions and a pixel layout, then one image's worth
of pixels — raw, RLE, or (XBM) a text array. Each has its own module;
`ImageDecoder`/`ImageEncoder` wrap any of them the same way
`vaco-codec-pnm` does.

## How it works, per format

- **BMP** (`src/bmp.rs`): `BITMAPFILEHEADER` + `BITMAPINFOHEADER`, `BI_RGB`
  only. Native decode format for 24bpp is **`bgr24`**, not `rgb24` — the
  file's byte order is kept, not swapped (measured). 32bpp is `bgra`.
  1/4/8bpp are paletted and expand through the palette into `rgb24`, since
  this crate carries no palette side-data type (see `planning/TECH-DEBT.md`);
  that path does not round-trip back to a paletted BMP.
- **PCX** (`src/pcx.rs`): 128-byte header, RLE (top two bits of a byte mark a
  run), planes stored per-scanline (all of R, then all of G, then all of B).
  Only 3-plane 8-bit RGB is handled; single-plane paletted PCX is not.
- **TGA** (`src/tga.rs`): 18-byte header, raw or RLE, truecolor (`bgr24`/
  `bgra`) or grayscale (`gray8`). **The reference tool has a decoder
  (`targa`) but no encoder** in this build, so `encode` has no reference
  output to compare against — it writes a spec-conformant, top-to-bottom,
  uncompressed file rather than guessing at an unverifiable layout.
- **SGI** (`src/sgi.rs`): 512-byte header; RLE's offset/length table is
  indexed **channel-major** (`channel * height + row`, not the other way),
  and scanlines are stored **bottom row first** — both confirmed by decoding
  the reference encoder's own RLE output and diffing against its raw
  pixels. The encoder writes uncompressed (`storage = 0`); it does not
  reproduce the reference's RLE table layout.
- **XWD** (`src/xwd.rs`): the 25-field `XWDFileHeader`, all big-endian
  `u32`. Only 24bpp `ZPixmap`/`MSBFirst`/32-bit-padded is handled, decoding
  to **`rgb24`** (a genuine R,G,B byte order, confirmed via the mask
  fields). The encoder does not reproduce the reference's embedded window
  name (`lavcxwdenc`, an ffmpeg-specific string, not part of the format) —
  it writes an empty name.
- **XBM** (`src/xbm.rs`): a C source fragment, one bit per pixel packed
  **LSB-first** (the opposite of PBM). Its polarity matches `monowhite`
  exactly; the reference converts between the two bit orders by reversing
  each byte *whole*, carrying trailing padding bits through rather than
  zeroing them past the declared width — this decoder does the same. The
  encoder always names the identifier `image`, regardless of output
  filename (measured).

## How to change it

Each format module is self-contained; `src/reader.rs` is the shared
bounds-checked binary cursor (LE and BE integer accessors). None of the six
share a byte layout, so there is little to factor out beyond that cursor and
the `ImageDecoder`/`ImageEncoder` wrappers in `src/lib.rs`.

## Configuration

`vaco_limits::Limits` bounds every decode via `vaco_frame::Frame::alloc_video`.

## Dependencies

`vaco-codec-core`, `vaco-frame`/`vaco-pixfmt`/`vaco-pool`, `vaco-packet`,
`vaco-limits`.

## Registration gap

Same as the other two C-13 crates: no `CodecId` exists for five of the six
formats (only `Bmp` does), `EncoderDesc` does not exist, and there is no
`vaco-cli` dispatch path from a codec name to a live `Decoder`/`Encoder`.
See `planning/TECH-DEBT.md`.

## Testing

18 unit tests, including cross-checks (BMP top-down vs bottom-up, SGI RLE vs
verbatim, TGA RLE vs raw). Differential verification against `ffmpeg` found
BMP, PCX and XBM byte-identical on encode and pixel-identical on decode; SGI
and XWD pixel-identical on decode with documented, deliberate encode
divergences (uncompressed vs RLE; no embedded name); TGA verified decode-only
against hand-built fixtures, since the reference has no encoder — see
finding 51 in `planning/CONFORMANCE-FINDINGS.md`.

A `parse_image_simple` fuzz target runs all six decoders over arbitrary
bytes.
