# `vaco-codec-gif`

Layer 4. GIF decode and encode through the safe Rust `gif` crate, including
animated-frame compositing and delay/disposal handling.

## What it is

A packet contains one complete GIF file. Decode emits one composited BGRA
frame for every image descriptor; encode buffers input frames and emits one
GIF packet when drained.

## How it works

The dependency resolves palettes and transparency to RGBA subframes.
`src/codec.rs` composites each subframe onto a persistent canvas, applies its
disposal method, and swaps red/blue while writing Vaco's declared BGRA layout.

Frame header parsing and pixel decompression are separate. Once an image
descriptor and graphic-control header parse, Vaco counts the frame when pixel
decompression reports the exact `gif::DecodingError::UnexpectedEof` seen in
the truncated reference fixture, matching `ffprobe -count_frames`; any
successfully decoded prefix is composited and the undecoded remainder stays
initialized. Invalid LZW codes and every other decompression failure instead
return malformed-input errors, so corruption cannot become a zero-filled
frame. Per-frame declared dimensions are checked against the allocation budget
before the decode buffer is allocated.

## How to change it

Compositing, disposal, and corruption-recovery behavior belongs in
`src/codec.rs`. Palette quantization and output encoding use the wrapped `gif`
crate. Keep the real fixtures in `tests/fixtures` when changing frame-count or
pixel-layout behavior, and compare both byte counts and BGRA bytes.

## Configuration

There are no environment variables. `vaco_limits::Limits` bounds logical-screen
and per-frame allocation. `Caps::SUBFRAMES` advertises multiple decoded frames
from one packet; `Caps::DELAY` advertises buffered encoding.

## Dependencies

`gif` supplies format parsing, LZW coding, palette expansion, and encode-time
quantization. Vaco integration uses `vaco-codec-core`, `vaco-frame`,
`vaco-pixfmt`, `vaco-pool`, `vaco-packet`, and `vaco-limits`.

## Verification

`tests/regression.rs` compares a real ffmpeg-produced frame byte-for-byte with
ffmpeg's BGRA decode. A second real fixture is truncated during the third
frame's LZW payload; it proves the two intact frames remain byte-exact and that
the header-complete third frame is counted. A header-complete one-pixel input
with an invalid LZW code proves this exception does not swallow malformed data.
The EOF exception is a pragmatic reference-compatibility rule: GIF89a does not
specify recovery from a truncated data-sub-block sequence.
