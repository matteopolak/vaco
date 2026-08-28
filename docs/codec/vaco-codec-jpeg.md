# `vaco-codec-jpeg`

Layer 4. JPEG (ITU-T T.81 / ISO/IEC 10918-1) native baseline and progressive
decode, plus a baseline encoder.

## What it is

The first native D11 replacement scheduled in plan 15 §4A.4: JPEG still
images and, via the same decoder, Motion JPEG frames. [`decode::decode`] and
[`encode::encode`] are the pure functions; [`JpegDecoder`]/[`JpegEncoder`]
wrap them in `vaco_codec_core::Decoder`/`Encoder`.

Covers `SOF0`/`SOF1` (baseline, extended sequential) and `SOF2` (progressive,
Annex G: spectral selection and successive approximation), 8-bit and 12-bit
precision, 4:4:4/4:2:2/4:2:0/4:4:0 subsampling plus grayscale, restart
markers, JFIF `APP0` and Adobe `APP14` recognition. Written from ITU-T T.81
directly (D7/D15 clean room); the Annex K default quantization and Huffman
tables and the Annex A zig-zag order are format-dictated constants, declared
in `provenance/vaco-codec-jpeg.toml`.

## How it works

### Decode: one engine for baseline and progressive

A baseline scan (`Ss=0, Se=63, Ah=Al=0`) is a degenerate case of progressive
spectral selection with successive approximation: its plain `EOB` symbol is
exactly progressive's `EOBn` with a run of zero. So `decode.rs` has one
coefficient-accumulation engine, fed by every scan a stream contains
(`decode_scan`, `ac_first`, `ac_refine`), and one finishing pass
(`finish_frame`) that dequantizes and inverse-transforms every block —
baseline reaches it after one scan, progressive after its last one.

Marker parsing is a single pass over the byte stream (`decode::decode`):
`SOF` allocates the coefficient store (sized from the attacker-controlled
width/height/sampling factors via `vaco_limits::Budget::alloc`, never
before), `DQT`/`DHT` update the active tables, `SOS` decodes one scan
in-line before returning to marker scanning. Non-interleaved
(single-component) scans and interleaved (MCU-based) scans use different
block-iteration order but the same per-block decode path.

The entropy-coded bitstream needs JPEG's own byte-stuffing (`0xFF 0x00` is
a literal `0xFF`; `0xFF` followed by anything else is a marker) and restart
markers interrupting it mid-scan — a different contract from
`vaco_bitstream::BitReader`'s zero-padding model, so `bits.rs`'s
`EntropyReader`/`EntropyWriter` are purpose-built rather than a wrapper.

### The "spec-exact" IDCT

ITU-T T.81 Annex A.3.3 gives the inverse DCT an accuracy bound, not a
mandated bit pattern. `idct.rs`'s `SpecExactIdct` reuses
`vaco_codec_dsp_idct::mpeg2::Idct8x8<f64>` (which already serves MPEG-2
under the identical contract) for the literal `f64` evaluation; `Fdct8x8` is
new, since that crate is inverse-only — built the same separable way on
`vaco_tx`'s DCT-II, verified against a direct classical evaluation.

### Pixel format mapping

A component's own subsampled plane in this crate's output frame *is* JPEG's
own subsampled component: `yuvj420p`'s half-resolution chroma planes are
exactly what a 4:2:0 JPEG already stores, with no separate upsampling step.
Four-component (CMYK/YCCK) JPEGs and any sampling ratio outside
4:4:4/4:2:2/4:2:0/4:4:0 have no matching `PixFmt` and are rejected with
`Error::Unsupported` — see "Known gaps" below.

### Motion JPEG container quirks

Informally "MJPEG-A/B" streams (QuickTime/AVI-era) often omit `DQT`/`DHT`
per frame and rely on the Annex K default tables. `DecodeState` falls back
to those defaults per table index when no `DHT` ever defined one, so such a
stream still decodes without any container-level table injection. Parsing
the QuickTime `mjqt`/`mjht` sample-description atoms that some encoders use
*instead* of even the implicit default (a genuinely different table set,
container-supplied) is a demuxer-side concern, not this codec's.

### Encode

Baseline only (`encode.rs`): reads sampling factors and precision from the
input `Frame`'s `PixFmt` (accepts grayscale and planar, non-RGB YCbCr —
colour-space conversion is `vaco-scale`'s job, not this crate's), scales the
Annex K quantization tables by an IJG-style quality formula, and always
emits the Annex K.3–K.6 default Huffman tables rather than building
optimized ones per image. Forward DCT via `Fdct8x8`; DC/AC Huffman coding
mirrors the decoder's symbol conventions exactly.

## How to change it

`tables.rs` holds every standard constant. `header.rs` parses each marker's
payload into a plain struct and does no entropy decoding. `decode.rs` is the
only module that interprets those structs against entropy-coded data — a
new scan variant or marker starts there. `marker.rs` just names byte values.
`idct.rs` is the only place a different (faster, less accurate) IDCT mode
would be added, alongside `SpecExactIdct`.

## Configuration

`vaco_limits::Limits` bounds every decode: component and block-grid sizes
come straight from `SOF`, validated by `Budget::alloc` before a coefficient
is stored. `EncodeOptions { quality, restart_interval }` controls the
encoder.

## Dependencies

`vaco-codec-dsp-idct` (inverse transform), `vaco-tx` (the forward transform
this crate builds directly), `vaco-bitstream` (header-segment byte reading
only — the entropy bitstream has its own reader), `vaco-frame`/`vaco-pixfmt`
(the decoded picture), `vaco-packet` (encoded bytes), `vaco-limits`
(allocation bounds), `vaco-codec-core` (the decoder/encoder protocol).

## Known gaps

- **Arithmetic entropy coding (Annex D)** is not implemented; such a stream
  is rejected with `Error::Unsupported` rather than decoded.
- **Lossless JPEG (Annex H)** is out of scope; likewise rejected.
- **Four-component CMYK/YCCK JPEGs** decode to nothing, because
  `vaco-pixfmt` has no CMYK/YCCK format — this crate cannot add one.
- **Arbitrary (non-power-of-two) sampling factors** are rejected rather than
  resampled, for the same `PixFmt` reason.
- **Progressive encode** is not implemented; only baseline output.
- **Optimized (per-image) Huffman tables** are not built; the encoder always
  uses the Annex K defaults, which costs compression ratio, not correctness.
- **Progressive successive-approximation AC refinement has a measured,
  unresolved numeric discrepancy** against `ffmpeg`'s decode on some
  multi-block images: a scan pattern that splits the AC band across two
  first-scans (e.g. `Ss=1-5` then `Ss=6-63`) followed by a later
  full-band (`Ss=1-63`) refinement scan decodes correctly for a single
  isolated block, but diverges from the reference by up to max-abs-deviation
  ~45 / RMS 3-8 starting around the 4th-8th block of some (not all) test
  images — the trigger is content-dependent, not a clean size threshold
  (32/40/56px-wide test images decoded correctly; 48/64px-wide did not). The
  simpler `ac_first` engine was independently verified bit-exact against a
  from-scratch reference re-implementation; the bug is isolated to
  `ac_refine`'s multi-block, multi-scan interaction and was not root-caused
  in the time available. See `planning/TECH-DEBT.md` for the full
  investigation notes.
- **QuickTime `mjqt`/`mjht` sample-description atoms** (container-supplied
  quantization/Huffman tables some MJPEG-A/B encoders use *instead of* the
  Annex K defaults) are not read by this crate — only the implicit
  default-table fallback is implemented. Reading those atoms is a
  demuxer-side concern.
- **`vaco-cli` has no path from `-c:v mjpeg`/`-c:v jpeg` to any leaf decoder
  or encoder** (`check_codecs` accepts only `copy`) — a workspace-wide gap
  affecting every codec crate, not specific to this one (tracked as #652).

## Testing

Unit tests cover the Huffman table builder against the standard tables, the
entropy reader/writer's byte-stuffing round trip, header parsing for every
segment type (including truncation), the forward/inverse DCT pair against a
direct classical evaluation, the progressive `EOBn`/refinement primitives in
isolation, and a hand-built two-scan progressive stream decoded end to end
through `decode()`. `tests/roundtrip.rs` encodes with this crate's own
encoder and decodes with its own decoder: a perfectly flat image round-trips
exactly at quality 100 (quantization step 1, so the DCT/IDCT pair is the
only source of error and stays within floating-point rounding), a gradient
stays within 2 LSB / RMS 1.0 at quality 100, and every subsampling variant,
restart intervals, and non-MCU-aligned dimensions all round-trip to the
right pixel format and size.

`ffmpeg`/`cjpeg` binaries were available in this environment and were used
for black-box differential testing (D17: probing a reference binary's
observed output is not a clean-room violation; reading its source would be).
Measured results, decoding this crate's output and feeding independently
generated JPEGs into this crate's decoder:

- **Baseline, all subsampling variants (4:4:4/4:2:2/4:2:0/4:4:0/gray), both
  restart intervals on and off, both default and optimized Huffman tables**:
  effectively bit-exact against `ffmpeg` — measured max-abs-deviation of 1,
  consistent with floating-point IDCT rounding noise rather than a decode
  error.
- **Progressive**: no longer crashes (see the `apply_correction` fix below),
  and matches `ffmpeg` exactly on single-block images and on the first few
  blocks of larger ones, but has the residual multi-block `ac_refine`
  discrepancy described in "Known gaps" above.
- A real crash bug was found and fixed during this testing: `apply_correction`
  was reading a successive-approximation correction bit from the entropy
  stream unconditionally, before checking whether the target coefficient was
  actually nonzero — the spec requires a still-zero coefficient to cost no
  bit at all, so this desynchronized the bitstream and crashed on nearly
  every real-world progressive JPEG with a non-trivial correction sweep.

A `jpeg_decode` fuzz target decodes arbitrary bytes; a 60-second local
`cargo fuzz run` pass found no crashes.
