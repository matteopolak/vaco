# `vaco-codec-exr`

Layer 4. OpenEXR still-image decode and encode through the safe Rust `exr`
crate, behind Vaco's codec boundary.

## What it is

The crate translates single-part RGB/RGBA floating-point EXR images between
encoded packets and `vaco_frame::Frame`. It also exposes a header-only
`parameters` reader used by `vaco-parse-image` during stream negotiation.

## How it works

`src/codec.rs` is the only module coupled to `exr`. Decode produces
`rgbaf32le`; encode accepts the crate's supported floating-point frame layout
and applies the selected EXR compression. `ExrDecoder` and `ExrEncoder` adapt
those operations to the common send/receive protocol.

The end-to-end route requires all three registrations: `exr_pipe` must attach
`CodecId::Exr`, `vaco-parse-image` must register `PARSER_EXR`, and the codec
registry must expose the decoder. Missing either of the first two leaves a
registered decoder unreachable or makes format negotiation use zero/default
parameters.

## How to change it

Add channel-layout, tiled, multipart, or deep-data support in `src/codec.rs`,
then update `parameters` from the same metadata interpretation so probe and
decode cannot disagree. Registry changes belong in the owning
`vaco-component.toml`; regenerate the registry rather than editing its output.

## Configuration

Encoder compression is selected through `CompressionAlgo`/`EncodeOptions`.
Normal `vaco_limits::Limits` bound frame and packet allocation. The registry
feature is `codec-exr`.

## Dependencies

`exr` performs EXR parsing and writing. The adapter also uses
`vaco-codec-core`, `vaco-frame`, `vaco-pixfmt`, `vaco-pool`, `vaco-packet`, and
`vaco-limits`; the reachable CLI path additionally depends on
`vaco-demux-image2`, `vaco-parse-image`, and the generated registry.

## Verification and known gap

The CLI now identifies and decodes a real ffmpeg-produced EXR after the
missing codec-id and parser links were added. Crate tests cover safe failure,
compression choices, protocol behavior, and RGBA-f32 round trips. A direct
numeric comparison of decoded linear channel values against a reference EXR
decode has not yet been recorded; do not interpret reachability alone as that
pixel-level claim. The reported layout also intentionally follows Vaco's
`rgbaf32le` output while the reference reports `gbrpf32le`.
