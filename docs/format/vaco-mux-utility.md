# `vaco-mux-utility`

Layer 4. Utility sink muxers: `null`, `mkvtimestamp_v2` (FM-20, issue #572).

---

## What it is

Two terminal sinks that either discard every packet (`null`) or dump one
plain-text timestamp line per video frame (`mkvtimestamp_v2`). Neither owns
another muxer and neither parses untrusted input, which is what separates
this crate from `vaco-mux-stream`'s meta-muxers — see that crate's doc for
why the two were split rather than merged into one crate.

`uncodedframecrc`, the third registration issue #572 names, is **not**
implemented here — see [§ Why `uncodedframecrc` is out of scope](#why-uncodedframecrc-is-out-of-scope).

## How it works

### `null`

Discards every packet. `-f null -` is the workhorse of nearly every test
that exercises this project's demux/decode/mux spine without wanting a real
file on disk, so this is more load-bearing than its size suggests.

`vaco-cli` already carries a local, hand-rolled `NullMuxer`
(`crates/app/vaco-cli/src/nullmux.rs`), written when `crates/format/` had
zero `vaco-mux-*` crates at all. This crate's `NullSinkMuxer` is the real,
registered version. It is deliberately **not** the same type: `vaco-cli`'s
copy also keeps an `Arc<Mutex<OutputTally>>` for the CLI's own summary line
(`video:7KiB audio:16KiB …`), which is a CLI-shaped concern, not a property
of the container format — `NullSinkMuxer` here has no state beyond a stream
counter, and a caller wanting counted-and-discarded output wraps this muxer
(or its sink) itself.

Measured (`ffmpeg -h muxer=null`, ffmpeg 8.1, `LC_ALL=C`):

```
Muxer null [raw null video]:
    Default video codec: wrapped_avframe.
    Default audio codec: pcm_s16le.
```

`pcm_s16le` has a `CodecId` in this workspace and `MUXER_NULL.default_audio`
reproduces it exactly. `wrapped_avframe` is the reference's pseudo-codec for
"an undecoded `AVFrame` handed straight to a muxer with no real encoder" — it
has no bitstream, no extradata, and no `CodecId` variant here, so
`default_video` is `None` rather than a guess. This is a deliberate
difference from `vaco-cli`'s local copy, which declares `None` for *both*
fields (true of the whole workspace when it was written, but not what this
container itself wants).

Flags (`FormatFlags::NOFILE | VARIABLE_FPS | TS_NONSTRICT | TS_NEGATIVE |
NOTIMESTAMPS`) are carried over verbatim from `vaco-cli`'s local copy rather
than independently re-probed — there is no CLI surface that prints the
reference's internal `AVFMT_*` bits, so this is an architectural judgement
call (the most permissive set `vaco-format-core`'s M18 rule accepts), not a
fresh measurement.

### `mkvtimestamp_v2`

One line per video frame: the frame's PTS in milliseconds, after a fixed
header. Measured (`ffmpeg -f lavfi -i testsrc=r=25:d=1 -f mkvtimestamp_v2 -`,
byte-inspected with `od -c`):

```
# timecode format v2
0
40
80
⋮
960
```

* The header is exactly `"# timecode format v2\n"`, written once by
  `write_header`, even with zero frames.
* Every following line is one decimal integer, LF-terminated, no padding.
* Rounding is to the nearest millisecond — confirmed against a `24000/1001`
  stream (`0, 42, 83, 125, 167, …`; frame 1's exact time 41.7083ms rounds up
  to 42, frame 3's 125.125ms rounds *down* to 125).

`MkvTimestampV2Muxer` does not reimplement that rounding. It declares
`stream_time_base` as `1/1000` once a stream exists, and
`vaco-format-core`'s M1 rescale step
(`interleave::MuxTimestamps::apply`) converts every incoming packet's `pts`
into that base with `Rounding::NearestAwayFromZero` (the framework default)
*before* `write_packet` ever sees it. The value arriving in `packet.pts` **is**
the millisecond count to print.

**Single video stream only.** Measured: a lone audio stream is rejected
outright (`Output file does not contain any stream`). Video-plus-audio and
two-video-stream probes each produced a *different* runtime shape (a
per-packet "More than one stream unsupported" warning in one case, a
header-only file with zero data lines in the other), and the two did not
resolve into one consistent tie-break rule under the probing time available.
Rather than encode a guess, `MkvTimestampV2Muxer::add_stream` takes the
unambiguous reading the single-stream case supports: exactly one stream,
and it must be video; every other case is `Error::Unsupported` at
`add_stream` time rather than a per-packet warning. This is a deliberate,
documented divergence from the reference's more lenient (and, on this
measurement, inconsistent) behaviour.

## How to change it

* **`null`**: `crates/format/vaco-mux-utility/src/null.rs`. The whole type
  is nine lines of trivially-correct code; the only things worth
  double-checking on a change are the flags constant and the
  default-codec pair, both documented inline with their probe transcript.
* **`mkvtimestamp_v2`**: `crates/format/vaco-mux-utility/src/mkvtimestamp.rs`.
  If a future measurement pins down the reference's actual multi-stream
  tie-break, replace the `Err(Unsupported)` in `add_stream` with whatever
  that turns out to be — the module doc names exactly what was ambiguous.
  Do **not** add your own rounding logic to `write_packet`; the framework's
  M1 rescale is what produces the correct value, and a second rounding pass
  would only be a second place for the two to disagree.

### Regenerating the mkvtimestamp_v2 probe transcript

```sh
ffmpeg -f lavfi -i testsrc=r=25:d=1 -f mkvtimestamp_v2 -
ffmpeg -f lavfi -i testsrc=r=24000/1001:d=1 -f mkvtimestamp_v2 -   # rounding check
```

## Configuration

None. Neither registration takes options through
`vaco_format_core::MuxerDesc::open` (`fn(Box<dyn MediaSink>) -> Result<Box<dyn
Muxer>>` — a bare sink, no channel for anything else). `mkvtimestamp_v2`'s
video-only, single-stream contract is not configurable; a caller who needs
different behaviour constructs `MkvTimestampV2Muxer` directly and drives it
outside the registry path (there is nothing to configure even then — the
contract is fixed).

## Dependencies

`vaco-core`, `vaco-io`, `vaco-limits`, `vaco-packet`, `vaco-format-core`,
`vaco-codec-core` — the same baseline every sibling mux crate takes. No
third-party crates.

## Why `uncodedframecrc` is out of scope

It hashes *decoded frames* and needs per-frame geometry (width/height/pixel
format for video; sample format/channel layout/sample count for audio) that
in the reference comes from the `AVFrame` itself.
`vaco_format_core::Muxer::write_packet` receives one `vaco_packet::Packet`
and whatever `vaco_codec_core::CodecParameters` were frozen at `add_stream`
— no per-call frame geometry, and no guarantee a packet's bytes are a
stride-free, tightly-packed plane the way an `AVFrame` filled a raw
encoder's packet in the reference. Implementing this honestly needs a
frame-level hook the `Muxer` trait does not have, which is a frozen-interface
change belonging to the orchestrator, not this crate. `vaco-mux-hash`
documents the identical gap for the same reason.
