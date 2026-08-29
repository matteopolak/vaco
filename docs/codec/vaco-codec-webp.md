# `vaco-codec-webp`

Layer 4. WebP: a native lossless (`VP8L`) codec (decode and encode), lossy
encode routed through `vaco-codec-vp8` (C-19), and `image-webp` kept only as
the fallback for `VP8X`-wrapped files (alpha via a separate chunk,
animation, ICCP/EXIF metadata) this crate does not yet handle natively.

## What it is

`WEBP_DECODER`/`WEBP_ENCODER` register a `Decoder`/`Encoder` for codec id
`webp`. Decode always tries the fast, native path first: a bare `VP8L` RIFF
chunk (no `VP8X` wrapper before it) is decoded end to end by [`vp8l`],
covering both still-lossless and lossless-with-alpha files, since VP8L
carries alpha directly in its ARGB pixels rather than needing `VP8X`'s
separate `ALPH` chunk. Anything else falls back to `image_webp`. Encode is
native `VP8L` by default; setting the `"lossless"` option to `"0"` switches
to a lossy `VP8` image via `vaco_codec_vp8::encode::Vp8Encoder`, wrapped in
this crate's own (~20-line) RIFF writer.

| Module | Contents |
|---|---|
| `codec` | Byte-level glue: RIFF chunk sniffing, `Frame`↔ARGB packing, the `image-webp` fallback, the lossy-via-`vaco-codec-vp8` path, RIFF container writing |
| `vp8l::bitio` | LSB-first bit reader/writer (VP8L's own convention — see its doc for why `vaco-bitstream`'s shared reader is the wrong fit) |
| `vp8l::huffman` | Canonical Huffman: decode-table construction from lengths, length choice from frequencies (real Huffman when it fits 15 bits, a balanced fallback when it provably cannot), the one-symbol zero-bits special case |
| `vp8l::prefix` | The "simple"/"normal" code-length transmission formats, including the 16/17/18 run-length codes on decode |
| `vp8l::transform` | The four VP8L transforms — predictor, color, subtract-green, color-indexing — applied (decode: all four) or emitted (encode: subtract-green only) |
| `vp8l::codes`/`vp8l::distance_map` | LZ77 length/distance prefix-code arithmetic and the 120-entry 2D neighbourhood distance table |
| `vp8l::lz` | This crate's own single-candidate hash matcher (encode only) |
| `vp8l` (`mod.rs`) | Orchestrates a whole VP8L stream: header, transform list, the (recursive, role-agnostic) image-stream decoder, and this crate's own encoder |

### What is deliberately not here

**Predictor/color/palette transforms and the color cache, on encode.**
Every one of VP8L's transforms is independently optional (spec §4), so an
encoder that never emits three of them still produces fully valid,
standard-conformant files — verified against `cwebp`/`dwebp`/`ffmpeg`, not
assumed. This crate's own encoder always writes exactly one transform
(subtract-green, which needs no side data and never hurts), a single
prefix-code group, and a simple single-candidate LZ77 match (min length 3,
no chaining, `O(1)` memory in image size). What that trades away is
density, not correctness or interoperability: a real `cwebp -m 6 -q 100`
file on the same content is meaningfully smaller, but every file either
side produces decodes correctly on the other.

**An optimal LZ77 parse.** The matcher records one candidate per hash
bucket and does not hash the interior of an accepted match, so it can miss
matches a real encoder's hash-chain search would find. This only costs
compression ratio.

**Animation encode**, native or via `image-webp` (`image-webp` has no
`ANMF` writer at all). **`VP8X` features going native** — alpha-via-`ALPH`,
animation, ICCP/EXIF — stay on the `image_webp` fallback; only a bare
`VP8L` chunk gets the native path on decode.

**Multi-threading.** Both directions are single-threaded; nothing in this
crate declares `vaco_codec_core::Threading::Slice`/`Frame`.

## How it works

### VP8L in one paragraph

A VP8L stream is a 14-bit width, 14-bit height, an `alpha_is_used` hint, a
3-bit version (always 0), an ordered list of 0-4 transforms (each read as a
type plus its own side data, itself encoded as a "sub-image" using the same
machinery as the main image), and then the main ARGB image data: an
optional color cache, an optional multi-group "meta-prefix" mechanism
(different image regions using different Huffman tables), five canonical
Huffman tables per prefix-code group (green+length+cache, red, blue, alpha,
distance), and a stream of per-pixel tokens — a literal, an LZ77 backward
reference, or a color-cache hit. `vp8l::decode_image_stream` is one
function implementing this for every role an "image" plays in the format
(the main picture, or any transform's own sub-image), since the format
itself defines it that way (spec §5.1, §7.3): the only difference is
whether the meta-prefix mechanism is even consulted (`is_top_level`).

### The one-symbol Huffman table, and why it needs a real branch

A canonical Huffman table with exactly one used symbol is a valid "full
binary tree" by the spec's own definition, and reading it consumes **zero
bits** — the decoder already knows what it will find before looking.
Treating it as an ordinary one-leaf tree and walking it for a bit would
desynchronise the whole bitstream the first time a solid-color image or an
all-color-cache prefix-code group produced one. `HuffmanTable::Single` and
`EncodeTable`'s own `single` field exist specifically for this case, on
both the read and write sides.

### `ClampAddSubtractHalf` needs truncating, not flooring, division

Predictor mode 13 computes `Clamp(a + (a - b) / 2)` where `a - b` can be
negative. C's `/` truncates toward zero; Rust's `>>1` floors — they disagree
by one on a negative odd difference. `Average2` (spec's other `/2`) is safe
to right-shift because both its inputs are unsigned channel values, so the
sum is never negative; `ClampAddSubtractHalf`'s difference is a genuine
signed value and gets its own `trunc_div2` (`if x >= 0 { x >> 1 } else {
-((-x) >> 1) }`, since `clippy::integer_division` denies a literal `/`
here). Found by re-reading the spec text carefully rather than by a failing
test — this project's own record of "checked against a primary spec
edition" tables surviving tier-1/tier-2 checks while still being wrong is
exactly why the fix came before, not after, running anything.

### The rightmost-column predictor border case

Spec §4.1 says the rightmost column's "top-right" predictor input is *not*
the (nonexistent) pixel above-and-right, but **the leftmost pixel of the
current row** — already decoded, since decode scans left to right. An
earlier draft used the row *above*'s leftmost pixel instead (an easy
misreading, since TR conventionally means "one row up"), which would have
been a real, silent pixel-level defect on every rightmost-column pixel of
every predictor block. Caught by re-checking the spec text against the
draft before running anything, then confirmed by the `cwebp`/`dwebp`
differential test below, which does exercise predictor blocks with a real
rightmost column.

## Verification

**Unit tests** (32, `src/`): bit-writer/reader round trips, canonical
Huffman round trips including the one-symbol case, `lengths_from_freqs`'s
Kraft-sum-equals-one property (both the real-Huffman and balanced-fallback
branches), every transform's inverse (including a hand-checked
bundled-palette expansion), the LZ matcher, a synthetic-image round trip
through this crate's own encoder and decoder, and truncation robustness
(feeding every prefix length of a valid stream to `decode` must not panic).

**Differential tests** (`tests/ffmpeg_differential.rs`, 4, real binaries
per D6 — skip cleanly if a tool is missing):

1. This crate's own lossless encode, decoded by `ffmpeg`'s `webp_pipe`
   demuxer: byte-exact (WebP lossless is D11 "Exact").
2. This crate's own lossless decode of a real `cwebp -lossless -m 6 -q 100`
   file (a `mandelbrot` test pattern, chosen because it has enough distinct
   colors that `cwebp` picks the predictor transform rather than
   color-indexing), cross-checked byte-exact against `dwebp`'s own decode
   of the same bytes. This is the "decoder you did not write" check for
   the transform/palette/color-cache surface this crate's own encoder never
   emits and so cannot self-verify.
3. The same, for a real `cwebp`-produced file with non-opaque alpha
   (confirms the bare-`VP8L`-with-alpha case takes the native path, not the
   `image-webp` fallback, and that alpha round-trips byte-exact).
4. This crate's lossy (`VP8`) encode path, decoded by `ffmpeg` and compared
   by PSNR: ~26 dB on an 80×60 synthetic image at the registered encoder's
   default constant-quality setting (`vaco-codec-vp8`'s own
   `DEFAULT_CONSTANT_QSCALE`) — a real number, not "it did not crash",
   though with no exact target since VP8 is lossy.

A stale version of test 1/4 used `-f webp` as the `ffmpeg` demuxer name,
which is not a registered demuxer (`webp_pipe` is) — `ffmpeg` failed to
open the input every time, and the test's own "skip if the tool can't
decode it" branch swallowed that as a clean skip rather than a failure.
Caught by rerunning with output capture disabled and noticing the "skip"
lines that should not have been there for an available `ffmpeg`; this is
the exact "test that cannot fail" trap `705779d`'s planning doc warns
about, corrected in the same commit as everything else here.

**Fuzzing**: `fuzz/fuzz_targets/webp_decode.rs` (pre-existing, still fits
this crate's new decode/encode paths unchanged) ran 1,356,554 executions at
30s with zero crashes and `fuzz/artifacts/` carrying nothing for this
target (two pre-existing artifacts under `cbs_h264`/`cbs_vp9` are unrelated
crates' work, not this one's).

## How to change it

Adding a pixel-format-coverage gap on encode belongs in `codec::frame_to_argb`'s
match. Adding VP8X-native decode (alpha-via-`ALPH`, animation, metadata)
would extend `codec::decode`'s RIFF sniffing and needs its own `VP8X`/`ANMF`
parser — nothing in `vp8l` assumes there is only ever one image. Improving
this encoder's density is entirely inside `vp8l::mod::encode_image_stream`
and `vp8l::lz`: a hash-chain matcher (see that module's own doc for why a
single-candidate table was chosen instead), a color cache, or a multi-group
meta-prefix split would all slot in without changing the bitstream-level
contract `vp8l::decode` already implements in full.

## Configuration

The `"lossless"` encoder option (`"0"`/`"false"` for lossy, anything else —
including never setting it — for the default native lossless path), read
via `vaco_codec_core::Encoder::set_option`. `WebpEncoder` implements
`Encoder` directly rather than going through the `SendReceive` + `AsEncoder`
adapter every other codec in this crate previously used, because
`AsEncoder<T>`'s `Encoder` impl does not forward `set_option` at all — there
is no `SendReceive::set_option` to forward *from* — so anything wrapped
that way is unreachable from the CLI's option surface regardless of what
the inner type wants to do with it. `vaco-codec-vp8`'s `Vp8Encoder` hit the
same wall for its own `"b"`/`"qscale"` options and took the same way out;
fixing `AsEncoder` itself belongs to `vaco-codec-core`'s owner.

## Dependencies

`vaco-codec-vp8` (lossy encode), `vaco-scale` (pixel-format conversion to
`Yuv420p` for lossy encode), `image-webp` (the `VP8X` fallback),
`vaco-codec-core`, `vaco-frame`/`vaco-pixfmt`/`vaco-pool`, `vaco-packet`,
`vaco-limits`. `vp8l` itself has none beyond `vaco-core`/`vaco-limits`
(pure bitstream logic, no format-crate dependency at all).

## Provenance

Format: `webp-lossless-bitstream-spec` (Google, fetched 2026-08-28,
already registered in `provenance/sources.toml`), transcribed line-by-line
for the 120-entry distance-neighbourhood table (tier 3 per
`AGENT-CONSTRAINTS.md`'s three-tier table-verification guidance) and for
every transform formula. The canonical-Huffman construction and RLE
length-transmission shape follow DEFLATE's published algorithm (RFC 1951
§3.2.2), original code, not derived from any single implementation. The
LZ77 matcher and the balanced-fallback Huffman lengths are original.
`cwebp`/`dwebp`/`ffmpeg` were run and their output measured (D6); their
source was never read.
