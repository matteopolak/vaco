# Raw-video codecs

`vaco-codec-rawvideo` decodes and encodes uncompressed and lightly packed video.
The registered codec identity selects configurable raw bytes or a fixed packing;
unknown or unsupported layouts must fail rather than guess their pixel format.

## How it works

A registry-built decoder receives `VideoParameters` through
`Decoder::prime_video_params` before its first packet. Coded dimensions take
precedence over display dimensions, and configurable rawvideo uses the declared
pixel format. The adapters and protocol validator preserve those parameters.
The older `prime_video(width, height)` hook sets dimensions only, while the
legacy `video_extradata` record remains accepted for existing callers.

Each packet is unpacked into bounded frame planes. Encoders read each frame's
actual layout and pack it according to their identity. EOF uses the shared
codec send/receive state machine.

## How to change it

Keep `prime_video_params` and registry-wrapper tests together when extending
container configuration. A successful decoder unit test using
`with_video_params` alone does not prove the generic CLI can configure it.
`vaco-demux-raw` supplies typed identities and Y4M parameters; `vaco-cli` primes
decoders on both ordinary and complex-filter paths.

## Configuration and dependencies

Geometry and pixel format come from the container or an explicit constructor,
not environment variables. Allocation is bounded by `vaco-limits`; packet/frame
storage, pixel layouts and protocol adapters come from `vaco-packet`,
`vaco-frame`, `vaco-pixfmt`, and `vaco-codec-core`.
