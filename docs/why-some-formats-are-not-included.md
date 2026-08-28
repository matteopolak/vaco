# Why some formats are not included

Vaco does not support everything `ffmpeg` supports. That gap has three very
different causes, and conflating them is the fastest way to misread the project:

| | Meaning | Where it is tracked |
|---|---|---|
| **Not yet** | Nothing stands in the way; it has not been written. The large majority of the gap. | [`format-coverage.md`](format-coverage.md), generated |
| **Not by default** | Written, in-tree, and deliberately absent from binaries we publish. | [Patent posture](#not-by-default-patent-encumbered-encoders) |
| **Not at all** | A decision says no, with a reason. A short list. | [below](#not-at-all) |

If a format you want is missing, it is almost certainly in the first row. This
document is about the other two.

---

## Not by default: patent-encumbered encoders

Rewriting a codec in Rust changes its patent exposure by exactly zero. So the
posture is royalty-free by default, with everything else in the tree but off:

- **In the published binary** — containers and protocols, AV1, VP8, VP9, Opus,
  Vorbis, FLAC, ALAC, PCM/ADPCM, the lossless and utility codecs, every filter
  that is not GPL-derived, and decode-only support for codecs whose essential
  patents have lapsed.
- **Behind a non-default Cargo feature, never in a published binary** — encoders
  for HEVC, VVC, AAC, AC-3/E-AC-3 and DTS, and anything else the legal register
  flags. The feature names say so out loud: `patent-encumbered-hevc-encode`.

You can build these yourself. `cargo build --features patent-encumbered-aac-encode`
is a supported, in-tree configuration — it is simply not the one we ship, because
shipping it is a licensing question about *our* distribution, not about your build.

This is enforced by compilation rather than by paperwork. `cargo xtask patent-gate`
compiles an example inside `vaco-registry`, so it inherits that crate's resolved
feature graph, and asserts that the `ENCUMBERED_ENABLED` slice — every row of
which is `#[cfg]`-gated on its own feature — comes out empty. A manifest reader
could only tell you what somebody wrote down; the slice a build produces is the
compiler's answer.

See `planning/00-decisions.md` D4 and D4.1.

---

## Not at all

### Bindings to C libraries

Vaco links no foreign library. Zero `-sys` crates, zero FFI, zero vendored C or
C++, zero build scripts that compile native code or probe for system libraries.

Measured against the reference build these docs are written against
(`ffmpeg 8.1`, `--enable-gpl --enable-version3`), that rules out its

```
encoders  libsvtav1 libx264 libx264rgb libx265 libvpx libvpx-vp9 libopus libmp3lame
decoders  libdav1d libvpx libvpx-vp9 libopus
```

and every entry like them.

**The important nuance: the binding is excluded, not the format.** AV1, VP8, VP9,
Opus and MP3 are all in scope and are all being written natively — a codec does
not become unavailable because we declined one particular way of reaching it. The
clearest case is the protocols: upstream reaches RIST and SRT through `librist`
and `libsrt`, and Vaco implements both natively from their specifications instead
(issues #63 and #62).

The reason is not purity for its own sake. FFI is the hole through which the
project's other guarantees leak: memory safety stops at the boundary, the licence
of the linked library is not what the wrapper crate declares (the `x264` and
`x265` crates declare MIT while statically linking GPL), cross-compilation breaks,
and the build stops being reproducible. One rule at the boundary removes all of
those at once.

See D10 gate 1.

### ...but *not* the platform video APIs

`h264_videotoolbox`, `hevc_videotoolbox` and their kin look like they belong in
the list above. They do not, and the difference is worth stating plainly, because
the natural inference from "no FFI" is wrong here.

Hardware acceleration **ships by default** wherever it is legally distributable
and the platform supports it. The test is legal distributability and correctness,
never the presence of `unsafe`. Calling the operating system's own video API
through a pure-Rust binding crate — `ash` for Vulkan Video, `objc2-video-toolbox`
for VideoToolbox, `windows` for D3D12 — causes none of the three harms the FFI
ban was aimed at.

It also matters more than convenience: shipping hardware decode by default means
users get H.264 and HEVC through silicon whose vendor already paid the licence,
while the binary contains no software decoder for either.

GPU *compute* is a separate problem with a better answer: filters, scaling,
colour conversion and tone mapping go through `wgpu` in safe Rust, and
`vaco-filter-gpu` stays `#![forbid(unsafe_code)]`. Fixed-function decode and
encode blocks are not reachable that way — `wgpu` does not expose them — and that
is the only reason vendor APIs enter the picture at all.

See D13.

### Rust crates under MPL-2.0

Symphonia and `mp4parse` are pure Rust, well maintained and widely used. They are
excluded on licence grounds — the permitted set is MIT / MIT-0 / Apache-2.0 /
BSD-2 / BSD-3 / BSD-3-Clear / ISC / Zlib / 0BSD / CC0 / Unicode-3.0, and MPL-2.0
is denied. This is why Vaco writes its own demuxers for formats those crates
already cover well.

See D3 and D10 gate 2.

### `fd:`, and `pipe:` with any number other than 0, 1 or 2

Turning an integer into an owned file descriptor requires `FromRawFd::from_raw_fd`,
which is `unsafe` — and justifiably so. Nothing proves the integer names a
descriptor this process owns, and a wrong value closes someone else's socket when
the wrapper drops.

`pipe:0`, `pipe:1` and `pipe:2` work, through `std::io::stdin`/`stdout`/`stderr`,
which own their descriptors legitimately. Any other `pipe:<n>` returns
`Unsupported` naming the reason, and `fd:` does not exist.

The workaround is a shell pipeline:

```bash
ffmpeg -i input.mkv -f matroska pipe:1 | vaco -i pipe:0 -c copy out.mp4
```

D13 admits `unsafe` where it is the only way to reach hardware that has no safe
path. Passing descriptors between processes is a convenience with the workaround
above, so the two are not comparable, and an allowlist is only worth anything if
it stays short. Revisit if a concrete workflow appears that genuinely cannot be
expressed through stdin/stdout.

See D16.

### Capture and playback devices

`avfoundation`, `v4l2`, `dshow`, `alsa`, `x11grab`, `audiotoolbox` and the rest
are out of scope for v1.0. They are `AVFMT_NOFILE` formats that own their own
I/O and are inherently non-deterministic, which puts them at odds with a project
whose correctness story is byte-exact differential comparison. The `NOFILE` flag
exists in `FormatFlags` so the model can express them when that changes.

### DVD and Blu-ray disc structure

`dvdvideo` and `bluray` both require GPL C libraries, and both have an
out-of-process delegation answer if you need them today.

### Four documented behavioural divergences

Not formats, but the same kind of deliberate absence, listed here so they are
findable:

- Ordered chapters and linked Matroska segments
- MP4 edit lists with `media_rate != 1`
- MP4 external `dref` (media data in a separate file)
- Byte-exactness for **live** DASH and HLS — both depend on wall-clock time by
  design and cannot be in a byte-exact corpus, so they are compared for container
  structure instead

Each is in the correctness allowlist with its reason recorded.

---

## What about everything else?

Everything else is the first row of the table: not yet. The generated table in
[`format-coverage.md`](format-coverage.md) is the authority on what exists today —
it is produced from the component registry, so it cannot drift from the binary
the way a hand-written list would.
