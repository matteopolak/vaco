# `vaco-codec-ffv1`

## What it is

FFV1 lossless video decode and encode (RFC 9043): the range coder and
Golomb-Rice bitstream layers, per-plane/per-channel context modelling, the
median predictor, and the Configuration Record that carries everything a
Configuration Record's own "external means" clause requires (frame
geometry, quantization tables, the state-transition delta). Decode handles
all three `coder_type` values (Golomb-Rice, range coder with the default
state transition table, and range coder with a custom one) across
`Yuv420p`/`Yuv422p`/`Yuv444p`/`Gray8`/`Gbrp`; this crate's own encoder only
ever emits the range coder with the default table (`version = 3`), one slice
per frame, matching the shape `codec.rs`'s `encode_frame` builds.

## How it works

### The per-sample loop

Both directions walk one `Line` at a time (`slice.rs`: `encode_line_range` /
`decode_line_range` / `decode_line_golomb`), with `codec::line_order`
deciding the `(plane, y)` sequence for the whole slice — Plane-then-row for
`colorspace_type == 0`, **row-then-Plane** for the JPEG 2000 RCT, whose
Lines interleave across planes (RFC 9043 §3.7.2, §4.7). One function
answers that question for the encoder and the decoder, so the two cannot
drift apart; they did, and every real `-pix_fmt gbrp` file decoded wrong
while the crate's own `Gbrp` round trip passed. Per sample: fetch six
border-aware neighbours (`SliceBuf::neighbours` /
`border`, RFC 9043 §3.1-§3.2), quantize the five gradients into a context
index (`quant::compute_context`, §3.5), predict with the median of three
(`quant::median_predictor`, §3.3), then code the wrapped difference through
the adaptive range coder (`rangecoder::put_symbol`/`get_symbol`, Figure 21)
or, in Golomb-Rice mode, through `rice.rs`'s run-mode-aware coder (§3.8.2).

### The Configuration Record

`codec::build_extradata`/`Ffv1Config::from_extradata` read/write the plain
RFC 9043 §4.3.3 record — deliberately *not* an internal envelope, so a
container sees exactly the bytes the spec describes. The encoder builds
this once per session (`Ffv1Encoder::send`, guarded by `sent_extradata`)
and attaches it to the first packet as `PacketSideData::NewExtradata`,
since FFV1's own geometry and quantization tables are only known once the
first frame's pixel format is seen.

### Known gap: the Configuration Record does not reach a muxed file's `CodecPrivate`

**Found while profiling D1 (2026-09-01), not introduced by it — confirmed
by rebuilding the pre-D1 encoder from `HEAD` against the same tree
snapshot and getting byte-identical output.** A real transcode
(`vaco -i h264.mp4 -c:v ffv1 -f matroska out.mkv`) produces a file whose
`CodecPrivate` is the **input** H.264 stream's `AVCDecoderConfigurationRecord**
byte for byte (confirmed by walking the output's EBML: `TrackEntry
CodecPrivate` is `01 64 00 28 ff e1 00 1b 67 64 00 28 ac d9 40 78 02 27
e5 c0 44 00 00 03 00 04 00 00 03 00 c8 3c`, an `avcC` — `version=1,
AVCProfileIndication=0x64, ..., 1 SPS of length 0x1b starting 0x67`), not
the FFV1 Configuration Record. Consequence: neither `ffmpeg` (`Invalid
version in global header`) nor this crate's own decoder (`ffv1: decoder
has no configuration; call set_extradata first`) can open the file at
all — this violates D1's own round-trip requirement completely, on every
FFV1 transcode, independent of anything this profiling pass changed.

Root cause, traced to `vaco-codec-core`/`vaco-cli`: `Encoder::extradata()`
(`vaco-codec-core/src/lib.rs`) is the channel `Muxer::add_stream` actually
reads *before* the first frame — see its own doc, and the parallel FLAC
fix it documents (`prime_audio` + `extradata()`, closing
`planning/E2E-GAPS.md` #2's audio-side negotiation gap). `Ffv1Encoder`
does not override `extradata()` (it stays `None`, the default), so
`vaco-cli`'s output `CodecParameters` never gets FFV1's record from that
channel and instead keeps whatever the pipeline seeded `out_params` with
from the *input* stream. The `PacketSideData::NewExtradata` this crate
attaches to the first packet is real and correctly RFC-9043-shaped, but
`vaco-mux-matroska` never reads packet-attached `NewExtradata` to patch a
track's `CodecPrivate` after the Tracks element is already written, so
that side channel is a dead end for this container today.

There is no `Encoder::prime_video` (only `Decoder::prime_video` exists,
added for FFV1's own decode-side geometry gap) to give a video encoder an
early, pre-`add_stream` opportunity to answer `extradata()` the way
`prime_audio` does for audio. Closing this needs, in order: (1) an
`Encoder::prime_video(width, height, format)` on `vaco-codec-core`'s
`Encoder` trait, mirroring `prime_audio`; (2) `vaco-cli` calling it before
`add_stream` for video encoders, mirroring the existing audio call; (3)
`Ffv1Encoder` overriding both `prime_video` (build `Ffv1Config` early) and
`extradata()` (return the built record) instead of relying solely on
`PacketSideData::NewExtradata`. All three touch files with concurrent,
unrelated edits in flight as of this writing
(`vaco-codec-core/src/lib.rs`, `vaco-codec-core/src/protocol.rs`,
`vaco-cli/src/exec.rs`), so it was reported rather than fixed under D1 —
see the spawned follow-up task.

### Known gap: no fuzz target

Decode parses untrusted bitstreams (`Ffv1Decoder::decode`,
`set_extradata`) and has no `fuzz/fuzz_targets/*ffv1*` entry as of this
writing — a gap by this project's own "no fuzz target, not done" rule
(D6). Not created under D1 (out of scope for an encoder-performance
profile); flagged as a follow-up.

## How to change it

- The per-sample loop is where D1's profile puts almost all of the
  encoder's time: ~40% in `SliceBuf`'s bounds-checked neighbour/sample
  fetches (`border`/`neighbours`/`get`, all `.get(..).copied().unwrap_or(0)`
  over a flat `Vec<i32>`), ~30% in the range coder itself
  (`put_symbol`/`put_rac`/`renormalize`/the per-context state array), ~8%
  in `median_predictor` (not eliminated by `#[inline]` at every call
  site — LLVM chose not to fold it into the `AsEncoder::send_frame`
  monomorphization the way it folded the rest of the loop), and a small
  remainder in context modelling and per-plane orchestration. A future
  change to the plane-traversal shape (interior vs. border fast paths,
  the two-row/two-column border checks currently re-evaluated on every
  interior pixel) is the largest remaining lever and was **not**
  attempted under D1's profile stage by design — see the plan.
- `.ok_or(Error::X)` inside any of the three per-pixel loops
  (`encode_line_range`, `decode_line_range`, `decode_line_golomb`)
  must stay `.ok_or_else(|| Error::X)`: `vaco_core::Error` carries a
  `String` variant elsewhere in the enum, so it is not trivially
  droppable, and an eager `.ok_or` measurably left a real
  (non-inlined-away) `drop_glue::<Error>` call in the hot loop — see the
  `#[allow(clippy::unnecessary_lazy_evaluations, reason = "...")]` next to
  each site for the measurement it cites. Do not "clean this up" back to
  `.ok_or` without re-measuring.
- One slice per frame is a deliberate simplification, not a spec
  requirement — RFC 9043 slices are independently decodable/encodable by
  construction and are the reference encoder's own threading mechanism
  (`slices`/`threads` in a real `ffmpeg -h encoder=ffv1`). Splitting into
  multiple slices is unstarted; it changes the bitstream's slice count
  (an observable, `ffprobe`-visible property), so it needs its own
  measured commit and a decision about whether it becomes the default.
- This crate's own encoder only ever emits the range coder
  (`coder_type = 1`) with the default state transition table; the
  reference's default for 8-bit content is
  Golomb-Rice, whose run mode is close to free on flat regions. Adding an
  encoder-side Rice coder (decode-side support already exists in
  `rice.rs`) is a separate, unstarted item — see `planning/PERF-PROGRAMME.md`
  D1's "Change" list.

## What the tests actually prove

`tests/roundtrip.rs` holds two kinds of test and only one of them is
evidence about *FFV1*.

The round trips (`round_trips_*`, `proptest_round_trips_yuv420p`) run this
crate's encoder into this crate's decoder. They catch a decoder that cannot
read what the encoder writes, and nothing else. Three separate bugs shipped
underneath a green suite of them, each one a case of the encoder and the
decoder sharing a wrong reading of the spec: Golomb-Rice run mode, the
Sentinel/Closed-mode slice length, and the RCT Line interleave.

The `decodes_real_ffmpeg_*` tests are the conformance evidence. Each decodes
a committed `ffmpeg`-produced fixture from `tests/fixtures/` and compares
every sample against `ffmpeg`'s own raw decode of the same file, which was
itself confirmed byte-identical to the unencoded source before being
committed. They cover `coder_type` 0/1/2, single- and multi-slice geometry,
a three-frame stream in which only the first frame is a keyframe, RGB via
the RCT under both coders, and gray. `provenance/vaco-codec-ffv1.toml`
records the exact `ffmpeg` command behind each one.

**When you add a format, a coder, or a colorspace, add a fixture.** A new
round-trip case costs compile time and proves nothing a reference decoder
would agree with.

## Configuration

- `-coder`, `-context`, `-slices` are read on decode (`params.rs`) from
  whatever a real encoder's Configuration Record states; this crate's own
  encoder does not yet expose them as CLI-settable options beyond what
  `tests::set_option_coder_*`/`set_option_slices_*` in `lib.rs` cover.
- No env vars or feature flags beyond the crate's own `patent-` posture
  (FFV1 is unencumbered; it ships in the default build).

## Dependencies

`vaco-bitstream` (range coder byte I/O, Golomb-Rice bit reads),
`vaco-core` (`Error`/`Result`), `vaco-limits` (`Budget` — every
allocation this crate makes, decode or encode, is budget-bounded),
`vaco-codec-core` (the `Decoder`/`Encoder`/`SendReceive` traits),
`vaco-pixfmt`, `vaco-packet`, `vaco-frame`, `vaco-pool`.
