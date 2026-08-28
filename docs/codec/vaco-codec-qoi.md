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

`QoiDecoder`/`QoiEncoder`'s `send` stamps the decoded frame's/encoded
packet's `pts` from its input, since `codec::decode`/`codec::encode` are pure
over bytes/pixels alone and have no timestamp to read on their own — found
wiring a real decode-then-encode leg through `vaco-cli`, where a packet with
no `pts` fails at the muxer.

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

## Registration

`vaco-component.toml` registers `QOI_DECODER`/`QOI_ENCODER` (both wrapping
`QoiDecoder`/`QoiEncoder` in `AsDecoder`/`AsEncoder(Validated::new(...))`)
under `vaco_codec_core::CodecId::Qoi`, feature `codec-qoi` (on by default).
`-c:v qoi`/`-c:a qoi`... resolves through `vaco_registry::encoder_by_name`
(C-13); `vaco -decoders`/`-encoders` list the `qoi` row.

Verified end to end: `vaco -i in.ppm -c:v qoi -f null -` runs a real
decode-then-encode leg (not a stub); the produced bytes are byte-identical to
`ffmpeg`'s own QOI encoder on the same input, checked directly via
`vaco_codec_pnm::decode_ppm`/`vaco_codec_qoi::{encode,decode}`. A real *muxed*
`.qoi` file could not be produced through `-f image2` in this pass — see
`planning/TECH-DEBT.md`'s C-13 entry for why (a `vaco-format-core`/
`vaco-mux-image2` gap, not this crate's).

## Testing

Unit tests cover round-tripping RGB/RGBA at several sizes, a solid-colour
run-length check, and the send/receive protocol shape. Differential
verification against `ffmpeg`'s own QOI encoder/decoder (5 fixtures spanning
gradients, noise, a solid colour and RGBA) found byte-identical encode output
and pixel-identical decode output in every case — see finding 51 in
`planning/CONFORMANCE-FINDINGS.md`.

A `parse_qoi` fuzz target decodes arbitrary bytes and re-encodes anything
that decodes successfully.
