# `vaco-codec-exr`

Layer 4. OpenEXR still-image decode and encode through the safe Rust `exr`
crate, behind Vaco's codec boundary.

## What it is

The crate translates single-part RGB/RGBA EXR images between encoded packets
and `vaco_frame::Frame`. It also exposes a header-only `parameters` reader used
by `vaco-parse-image` during stream negotiation.

## How it works

`src/codec.rs` is the only module coupled to `exr`. Decode accepts exactly one
flat layer containing `R`, `G`, `B`, and optional `A`, with 1x1 sampling for
every channel. It accepts scan-line or tiled storage and selects the largest
resolution level. Decode produces native-endian packed RGBA-f32; encode accepts
the crate's supported frame layouts and applies the selected EXR compression.
`ExrDecoder` and `ExrEncoder` adapt those operations to the common send/receive
protocol.

Header parsing and full decode pass the same metadata through the same scope
gate. Deep data, multipart files, non-RGB channel sets, extra channels, and
subsampled channels are therefore refused during negotiation instead of being
advertised and rejected later. The gate also refuses the HTJ2K-32 and HTJ2K-256
compression modes that `exr` can parse but cannot decompress. Decode reads the
header first, checks the frame dimensions, and allocates its RGBA staging buffer
through `vaco_limits::Budget` before asking `exr` to decompress pixels. The
staging buffer and output frame both count toward the cumulative allocation cap
while they are live.

The end-to-end route requires all three registrations: `exr_pipe` must attach
`CodecId::Exr`, `vaco-parse-image` must register `PARSER_EXR`, and the codec
registry must expose the decoder. Missing either of the first two leaves a
registered decoder unreachable or makes format negotiation use zero/default
parameters.

## How to change it

Add channel-layout, multipart, or deep-data support in `src/codec.rs`. Update
`check_scope` and the decode reader together so header negotiation and decode
cannot disagree. Registry changes belong in the owning `vaco-component.toml`;
regenerate the registry rather than editing its output.

## Configuration

Encoder compression is selected through `CompressionAlgo`/`EncodeOptions`.
Normal `vaco_limits::Limits` bound the decoded staging buffer, frame, and packet
allocation. The registry feature is `codec-exr`.

## Dependencies

`exr` performs EXR parsing and writing. The adapter also uses
`vaco-codec-core`, `vaco-frame`, `vaco-pixfmt`, `vaco-pool`, `vaco-packet`, and
`vaco-limits`; the reachable CLI path additionally depends on
`vaco-demux-image2`, `vaco-parse-image`, and the generated registry.

## Verification and known gap

The CLI now identifies and decodes a real ffmpeg-produced EXR after the
missing codec-id and parser links were added. On a same-session 12×6
ffmpeg-produced EXR, both binaries emitted exactly one 1,152-byte RGBA-f32
frame. Reordering ffmpeg's planar `gbrapf32le` result into packed RGBA and
comparing all 288 floats gave a maximum absolute difference of
`0.0004882887` and mean absolute difference of `0.0000732220`; every Vaco
value was finite. This is bounded by one half-float quantisation step for the
generated fixture, not a byte-exact result.

Crate tests cover safe failure, pre-allocation limits, header/decode scope
agreement, compression choices, protocol behavior, and RGBA-f32 round trips.
The reported layout intentionally follows Vaco's native-endian packed RGBA-f32
output while the reference reports planar `gbrpf32le`.
