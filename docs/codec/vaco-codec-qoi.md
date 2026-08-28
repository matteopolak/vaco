# `vaco-codec-qoi`

Layer 4. QOI ("Quite OK Image format") decode and encode.

## What it is

A whole-image, single-frame lossless codec: one 14-byte header, then a byte
stream of seven chunk types over a 64-entry cache of recently seen pixels, no
entropy coder. [`codec::decode`]/[`codec::encode`] are the pure functions;
[`QoiDecoder`]/[`QoiEncoder`] wrap them in `vaco_codec_core::SendReceive`.

Written from the QOI specification (`qoiformat.org`), cross-checked against
the reference codec's observable byte behaviour per D17/D7.

## How it works

Decode reads the header (magic, width, height, channel count, colourspace),
picks `rgb24` (3 channels) or `rgba` (4 channels) — from the header, never
from anything observed in the pixel data — then walks the chunk stream:
`QOI_OP_RGB`/`RGBA` (full pixel), `QOI_OP_INDEX` (replay from the 64-entry
cache), `QOI_OP_DIFF`/`LUMA` (small deltas from the previous pixel), and
`QOI_OP_RUN` (repeat the previous pixel). The cache is updated with every
newly produced pixel except during a run — including on an `INDEX` chunk
itself, which looks redundant but is what the reference does.

Encode is the mirror: track a run, flush it on a mismatch or at 62, else try
index/diff/luma in that order before falling back to a full pixel.

## How to change it

`src/codec.rs` is the only module that knows the byte format; a future QOI
extension lives there. `src/reader.rs` is a small bounds-checked cursor
shared by nothing else. `src/lib.rs`'s `SendReceive` wrappers should not need
to change for a codec-level change.

## Configuration

`vaco_limits::Limits` bounds the decoded frame: `width`/`height` come from
the header, so `vaco_frame::Frame::alloc_video` (via `Budget::check_frame`)
validates them before a pixel is touched.

## Dependencies

`vaco-codec-core` (the send/receive protocol and `Machine`), `vaco-frame`/
`vaco-pixfmt`/`vaco-pool` (the decoded picture), `vaco-packet` (the encoded
bytes), `vaco-limits` (allocation bounds).

## Registration gap

No `vaco-component.toml` fragment exists for this crate. `vaco_codec_core::CodecId`
has no `Qoi` variant, and `EncoderDesc` does not exist as a type at all — see
`planning/TECH-DEBT.md`'s C-13 entry. `QoiDecoder`/`QoiEncoder` are usable
directly by any caller that constructs them, just not reachable through
`vaco-registry` or `vaco-cli -c:v qoi` yet.

## Testing

Unit tests cover round-tripping RGB/RGBA at several sizes, a solid-colour
run-length check, and the send/receive protocol shape. Differential
verification against `ffmpeg`'s own QOI encoder/decoder (5 fixtures spanning
gradients, noise, a solid colour and RGBA) found byte-identical encode output
and pixel-identical decode output in every case — see finding 51 in
`planning/CONFORMANCE-FINDINGS.md`.

A `parse_qoi` fuzz target decodes arbitrary bytes and re-encodes anything
that decodes successfully.
