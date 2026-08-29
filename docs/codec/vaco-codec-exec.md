# `vaco-codec-exec`

Layer 4. Video encoders backed by a user-installed CLI tool, spawned as a
child process (issue #347, `C-46`). Registers `libx264` (H.264) and
`libx265` (HEVC).

## What it is

`vaco` will never carry a software H.264 or HEVC encoder in-tree — x264/x265
are GPL, and both codecs are patent-encumbered in a way this project has no
counterparty to clear (`planning/research/07-legal-patents-licensing.md`
§5.2). This crate is the mitigation the legal register calls "the preferred
escape hatch": spawn the *user's own* installed `x264`/`x265` binary, pipe
raw frames to it, read the resulting Annex-B elementary stream back. No GPL
code and no patent-encumbered implementation ever enters this crate's source,
this project's binary, or this process's address space — the OS process
boundary is a genuine aggregation, not a combined work, which is the whole
legal argument for this design (§4.4 of the legal register).

## How it works

| Module | Job |
|---|---|
| `y4m` | Serialise a `vaco_frame::Frame` (`yuv420p` only) as a YUV4MPEG2 stream: one header line, then a `FRAME\n` marker plus trimmed plane bytes per frame |
| `annexb` | Split a raw Annex-B byte stream into one packet per access unit, using an Access Unit Delimiter NAL (`--aud`) as the exact boundary |
| `process` | Own a spawned child's stdin/stdout/stderr without deadlocking: a background thread drains stdout for the child's entire lifetime, independent of when `Encoder::receive_packet` is actually called |
| `encoder` | The `Encoder` impl (`ExecEncoder`) tying the above together, one per registered tool (`X264`/`X265` in `encoder.rs`) |

### Two different input plumbings, because they had to be measured, not assumed

`x264 --demuxer y4m` reads Y4M from stdin directly — a real pipe works.
`x265` on the machine this was built and tested on does **not**: `--input -`
fails with `unable to open input file`, and so does a named pipe unless its
path ends in `.y4m` — `x265` selects its Y4M reader by filename suffix, not
by sniffing the stream's own magic header, even though its own `--help`
says "auto-detected if Y4M". Measured directly:

```text
$ mkfifo in.y4m && (cat test.y4m > in.y4m &) \
    && x265 --input in.y4m --bframes 0 --aud -o - --bitrate 200 > out.hevc
encoded 5 frames ...                              # works
$ mkfifo in2 && (cat test.y4m > in2 &) && x265 --input in2 ...
x265 [error]: yuv: width, height, and FPS must be specified   # fails, same bytes
```

So `libx264` streams frames over a pipe as they arrive (`InputMode::Stdin`);
`libx265` buffers the whole clip to a real temporary file named
`vaco-codec-exec-*.y4m` and is not spawned until end of stream closes that
file (`InputMode::TempY4mFile`) — a real behavioural difference between the
two backends, not an oversight.

### No B-frames, on purpose

Both backends are invoked with `--bframes 0`. This guarantees the tool's
output access units come back in the same order the input frames were sent,
so `ExecEncoder` can hand back each frame's own `pts`/`duration` for the
matching output packet by simple FIFO correspondence, with no need to parse
a real presentation timestamp back out of the elementary stream or
special-case B-frame reordering. A real cost to compression efficiency; not
a correctness shortcut — the alternative was a PTS/DTS-reordering heuristic
that could be silently wrong in a way nothing here would catch.

### The `Encoder` state machine

`ExecEncoder` embeds a `vaco_codec_core::Machine<Packet>` with `Caps::DELAY`.
`send_frame(None)` (`Accept::Drain`) does not synchronously finish the
machine — it can't, because the child may still be encoding — it only
triggers the drain side effect (close stdin, or flush+spawn for the
temp-file tool). `receive_packet` is what actually blocks, in a loop, on the
child's stdout until either more output or true EOF (a closed channel, which
triggers `proc.wait()` and `machine.finish()`) arrives. This is the intended
use of `Machine::receive`'s `NeedMoreInput`-while-draining case, not a
workaround.

## How to change it

- **A third tool** (e.g. a future AV1 CLI encoder) is a new `ToolSpec` const
  in `encoder.rs` plus a registration in `vaco-component.toml` — the pipe
  mechanics in `process.rs` and the AUD-splitting in `annexb.rs` do not
  change per tool.
- **Real B-frame support** needs a `--tcfile-out`-style real presentation
  timestamp recovered from the tool, not just re-enabling `--bframes`;
  re-enabling the flag without that produces packets whose `pts` no longer
  matches their content.
- **More pixel formats** (10/12-bit, 4:2:2/4:4:4) need a real Y4M `C` tag
  table in `y4m.rs`, not just accepting the pixel format — the current
  `C420jpeg` tag is specific to 8-bit 4:2:0, measured against `ffmpeg`'s own
  Y4M muxer output.

## Configuration

`Encoder::set_option`: `"b"` (bits/second) sets `--bitrate` in kbit/s;
`"crf"`/`"qscale"`/`"global_quality"` sets `--crf` directly (the two tools'
CRF scales are not identical — this option was never meant to normalise
that). Neither set: the tool's own default rate control applies.

## Dependencies

`vaco-codec-core` (`Encoder`, `Machine`, `EncoderDesc`), `vaco-frame`,
`vaco-packet`, `vaco-pixfmt`, `vaco-pool`, `vaco-limits`, `vaco-core`. At
runtime: the `x264`/`x265` binaries on `PATH` — absence is reported as
`Error::Unsupported` at the first frame, not a panic or a silent no-op.
