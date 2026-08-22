# `vaco-probe`

## What it is

The `ffprobe` equivalent, and the v0.1 acceptance surface (D5). It turns an
argument vector and a media file into bytes, and the acceptance criterion is
**byte identity** with the reference (D6) — not "equivalent information", not
"structurally the same JSON". A trailing space is a failure.

Four crates do most of the work. This one is the wiring plus **one table**:

| Crate | Owns |
|---|---|
| `vaco-cli-core` | the option table, the stream-specifier grammar, the scope model |
| `vaco-registry` | which demuxers exist |
| `vaco-format-core` | probing, and the `Demuxer` trait |
| `vaco-textformat` | the seven writers, and every number that reaches them |

The table is `src/fields.rs`: **which** fields, in **what order**, with **what
spelling**, and **integer or string**. Nothing else in the tree decides that and
nothing derives it — `channels` is an integer and `sample_rate` is a string,
adjacent, both holding a plain number. Read `fields.rs` first.

## A correction to the wave brief, stated plainly

**`ffprobe file.mp4` with no other options prints nothing on stdout.**

```sh
ffprobe av.mp4 2>/dev/null | wc -c    # 0
```

Everything it shows — the version banner, the configure line, the
`Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'av.mp4':` block — goes to **stderr**,
from the logging layer, not from the section writers. So "`vaco-probe file.mp4`
byte-identical to `ffprobe file.mp4`" is a statement about stderr, and half of
that stderr is FFmpeg's identity (`ffprobe version 8.1`, the Homebrew configure
line, `libavutil 60. 26.100`), which D9 puts outside what we reproduce and which
we must not print regardless.

The stdout acceptance target is therefore `-show_format`, `-show_streams` and
their relatives, which is what this crate implements. `banner()` emits Vaco's
own one-line banner in the same position, so `-hide_banner` means the same
thing. The `av_dump_format` stderr block is not implemented and is tracked as a
separate target.

## How it works

```text
argv ──▶ [cli]      the option set                          (vaco-cli-core)
     ──▶ [listing]  -formats/-sections/… print and exit
     ──▶ [open]     protocol → IoContext → probe → demuxer
                             → Discovery                    (vaco-io, -format-core)
     ──▶ [show]     one section per -show_* flag             (this crate)
     ──▶ [writer]   bytes                                    (vaco-textformat)
```

### Why `Discovery` is composed here and nowhere else

`read_header` is only allowed to report what the header states, and every
container under-describes itself: Matroska leaves `start_time` to the packets,
MPEG-TS leaves the codec parameters, everyone leaves the frame rate.
`vaco_format_core::Discovery` is the shared pass that fills those in — it reads
a bounded prefix, refines what it can, and **replays every packet it consumed**,
so wrapping is transparent to everything downstream.

It has to be composed by the *opener*, because it is a wrapper and not a driver:
a demuxer owns its own I/O, so nothing below this point can run the loop. That
makes `vaco-probe::open` the composition point, and composing it once covers
every container at the same time.

This matters more than it sounds. The alternative — each demuxer deriving
`start_time` itself — actively breaks the shared rule: `Discovery::finish`
guards on `if stream.start_time.is_none()`, so a demuxer that fills the field in
*disables* the shared derivation for every caller that does run discovery, and
the two then disagree with nothing to catch it. `vaco-demux-matroska` declined
to set it locally for exactly this reason. Until discovery was actually
composed, `Discovery` was dead code: every `Discovery::new` in the workspace was
in a test or an example.

Measured effect, `format` section, before and after composing it:

| | files fully identical | matrix |
|---|---|---|
| before | 9 of 10 | 154/170 |
| after | 11 of 12 | 188/204 |

and stream field values went from 336/393 to **748/877** over twelve files.
See *The 2026-08-22 `Stream` widening* below for the numbers after that.

#### Three workarounds this needed, and none of them survived

Composing `Discovery` against the trait layer as it stood required three local
bridges. All three were reported rather than kept, all three were fixed
upstream, and all three are now deleted — which is the outcome to aim for, since
a workaround left in place after its cause is gone hides the next regression
instead of the last one.

| Workaround | Why it existed | Replaced by |
|---|---|---|
| `format_flags()`, a name-keyed table transcribed from each demuxer crate's `FLAGS` | `DemuxerDesc` carried no flags, and reaching them meant a dependency edge on every container crate | `DemuxerDesc::flags` |
| `Boxed`, a seven-method newtype | no `impl Demuxer for Box<D>`, though `vaco-codec-core` had the equivalent for `Parser` | the blanket impl |
| `Input::duration`, read before wrapping | `Discovery::duration()` preferred a `from_pts` input filled from the file's *head* while `estimate_duration` treats it as a tail scan, so every container reported the length of its own probe window | `from_pts` left unset |

The last one was re-measured before removal rather than assumed redundant:
twelve containers, all twelve agreeing with the reference through
`Discovery::duration()` as well as around it. Going through `Discovery` is also
the better answer, because it applies R14 — when a container-level duration and
per-stream durations disagree, the longest stream wins — which reading the inner
demuxer's field directly would bypass.

What did *not* go is the check that guarded the flags table.
`every_registered_demuxer_declares_flags` now asserts against the descriptor:
`DemuxerDesc::flags` is a plain field, so a descriptor written without it
compiles and silently reports `empty()`, and `empty()` is not a neutral answer —
`TS_DISCONT` *suppresses* the monotonic-DTS repair, so a container that lost it
would have genuinely discontinuous timestamps quietly rewritten with nothing in
the output to show for it.

`run(argv, out, err) -> Exit` is the whole program. `main.rs` owns exactly three
things the library must not: the real argv, the real stdio, and the exit code.
That is what makes the binary reachable from a test and from a fuzz target
without a process.

### `fields.rs` — the table, and how it was measured

Every row was read off `ffprobe` 8.1 (Homebrew, arm64 macOS) under `LC_ALL=C`,
and each column has its own experiment. Plan 13 §1b's rule is that the layer
between you and the answer has opinions, so each column was taken through a
route where that layer could not lie.

* **Order** — `-of flat -show_optional_fields always`, which prints every field
  including the unavailable ones, so no field can hide behind being absent.
* **Integer versus string** — cross-checked between two writers that spell the
  distinction differently and would have to be wrong in the same way to agree:
  `json` quotes strings and not integers, `flat` quotes strings and not
  integers. They agree on every row.
* **The placeholder for an absent value** — obtained by finding an input that
  genuinely lacks the value, never by assuming. This is where the surprises are.

```sh
ffmpeg -f lavfi -i testsrc2=size=320x240:rate=25:duration=2 \
       -f lavfi -i sine=frequency=440:duration=2 \
       -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest av.mp4
ffmpeg -f lavfi -i testsrc2=size=32x24:rate=5:duration=0.4 -pix_fmt gray raw.yuv

ffprobe -v quiet -of flat -show_optional_fields always -show_streams av.mp4
ffprobe -v quiet -of json                             -show_streams av.mp4
ffprobe -v quiet -of flat -show_optional_fields always \
        -f rawvideo -video_size 32x24 -pixel_format gray -show_streams raw.yuv
```

The third invocation is the useful one: raw video has no aspect ratio, no colour
description, no level and no stream id, so it reveals every placeholder at once.

#### Probing note: the exit code is a channel too, and it has its own pipe trap

Plan 13 §1b is written about output, but it applies verbatim to the **exit
code**, and that is where this crate got a fact wrong.

`vaco-probe` shipped an early revision asserting — in a code comment and in the
control flow — that `ffprobe` with no input file exits **0**. It exits **1**.
The measurement was:

```sh
ffprobe 2>&1 | tail -5; echo $?      # 0  — this is tail's status
```

`$?` after a pipeline is the *last* command's status, so the harness reported
`tail`'s success and the reference's failure was invisible. Measured with
nothing in between:

```sh
ffprobe </dev/null >/dev/null 2>/dev/null; echo $?    # 1
```

Two rules fall out, both cheap:

1. **Never read an exit code through a pipe.** Redirect to files or
   `/dev/null`, or capture with something that reports the process's own status
   — `subprocess.run(...).returncode` in Python is the instrument this crate's
   corpus uses, and it has no pipeline to be confused by.
2. **`$pipestatus`, not `$PIPESTATUS`, in this project's shell.** The default
   shell here is `zsh`, where the array is lowercase and **1-indexed**;
   `${PIPESTATUS[0]}` expands to the empty string and a comparison against it
   silently succeeds. A second measuring layer with its own opinion, sitting
   directly on top of the first.

Also close stdin. `ffprobe` with no arguments and a terminal on stdin behaves
differently from one with stdin closed, and a harness that inherits an
interactive terminal is measuring its own environment.

Exit codes are conformance surface — a script branches on them — so they are
compared in `tests` and in the corpus below alongside the bytes.

**`N/A` is not universal.** Measured:

| Field | Absent form |
|---|---|
| `sample_aspect_ratio`, `display_aspect_ratio`, `id`, `max_bit_rate`, `nb_frames`, `nb_read_frames`, `nb_read_packets`, `bits_per_raw_sample`, `start_pts`, `start_time`, `duration_ts`, `duration`, `bit_rate`, and `width`/`height` on a subtitle stream | `N/A` |
| `codec_name`, `codec_long_name`, `profile`, `codec_type`, `pix_fmt`, `sample_fmt`, `channel_layout`, `field_order`, `color_range`, `color_space`, `color_transfer`, `color_primaries` | `unknown` |
| `chroma_location` | `unspecified` |
| `level` | the **integer** `-99` |
| `mime_codec_string`, `extradata_size` | not emitted at all, in any writer, at any `-show_optional_fields` setting |

`chroma_location` spelling its unknown differently from the four colour fields
beside it is the reference's inconsistency, not a transcription slip;
`vaco-color` had already recorded the same thing independently, which is a
useful cross-check on both.

### `emit.rs` — the optional-field policy

The placeholder goes through the *optional* path, which is why `json` omits
`color_range` entirely while `flat` prints `color_range="unknown"`. Measured:

| `-show_optional_fields` | `json`/`xml` | every other writer |
|---|---|---|
| `never` | omit | omit |
| `auto` (default) | omit | print the placeholder |
| `always` | print the placeholder | print the placeholder |

`vaco_textformat`'s `str_opt(k, None)` hard-codes `N/A`, so this crate applies
the policy itself rather than calling it. A `str_opt_or(key, value, placeholder)`
on `TextFormat` would let `emit.rs` shrink to nothing; raised, not required.

### `listing.rs` — the exiting options

Pure renderings of the registry and of `vaco-textformat`'s own section table. No
component is instantiated, because a descriptor is inspectable without
constructing anything. Column layouts, measured:

* `-formats`: leading space, three flag columns, a space, the name in fifteen, a
  space, the long name.
* `-codecs`: the same with six flag columns and a twenty-wide name.
* `-sections`: four flag characters, then **3** spaces at the root and
  `4·depth + 2` below it. Measured across all thirteen distinct depths — the
  step is genuinely 3 then 4, not 4 throughout.

## Measured fidelity

Two independent measurements, because they answer different questions.

### With correct inputs (`tests/reference.rs`)

Streams built to match what a correct MP4 demuxer must produce, rendered and
diffed against captured `ffprobe` bytes:

* `format`: **byte-identical**, whole document including trailing newlines, in
  `default`, `ini` and `xml`.
* `stream`: **113 of 116 lines byte-identical** across both streams of `av.mp4`.

The three that differ, each a missing *input* rather than a formatting choice:

| Line | Why |
|---|---|
| `is_avc=true` | h264 decoder private option (`-show_private_data`, on by default). No decoder, and `CodecParameters` has nowhere to put it. |
| `nal_length_size=4` | same. |
| `bits_per_raw_sample=8` on the video stream | the field lives on `AudioParameters` only, so a video stream cannot report it. |

The list is asserted **exactly**, so a divergence that disappears fails just as
loudly as a new one — and it did. `codec_long_name` was a fourth entry
(`vaco-codec-core` said `H.264 / AVC / MPEG-4 AVC` where the reference appends
` / MPEG-4 part 10`); it was reported rather than fixed here, closed upstream
mid-wave, and the assertion failed the moment it was. That is the behaviour to
keep: a closing divergence is a change to observable output and gets reviewed
like any other.

### End to end, live (`vaco-probe` against `ffprobe`, `av.mp4`)

With `vaco-demux-mp4` as it stood at the end of this wave:

**Format section: 188 of 204 byte-identical** — 12 files (MP4, MOV, fragmented
MP4, M4A, MPEG-TS, Matroska, WebM, and Matroska with mixed audio/video/subtitle
tracks) × 17 option sets covering all seven writers, `-pretty`, `-sexagesimal`,
`-unit -prefix`, `-show_optional_fields always`, `-show_entries`, and four
writer-option variants. **Eleven of the twelve files are byte-identical on every
one of the 17.**

Every divergence is one field on one file: `sub.mkv`, a Matroska containing
*only* a subtitle track, where the reference reports `start_time=N/A` and we
report `0.000000`. See the gaps list — it is a shared-rule question, not a
formatting one.

`op_st.opus` is deliberately outside that matrix: no Ogg demuxer is registered,
so it measures a missing component rather than fidelity. We report
`Invalid data found when processing input` and exit 1, which is the right answer
for a build without that demuxer.

**Stream section: 748 of 877 field values** over twelve files, the remainder
being demuxers not yet filling `CodecParameters` (see the list below).

#### `format.bit_rate` truncates — a bug the round durations hid

Found by widening the corpus to a WebM file, and worth recording as a method
note rather than a changelog line.

`format.bit_rate` is `size * 8 / duration`, and the reference **truncates** the
result to whole bits per second before formatting it. Every sample in the
original corpus had a duration that was a round number of seconds, so the
quotient was already an integer and truncation, rounding and no-rounding-at-all
all agreed. The first non-round duration made them disagree:

```
op_st.webm  20846 B / 2.008000 s   raw 83051.792829   ref 83051   ours 83051.792829
op_st.opus                         raw 79401.943683   ref 79401
```

Two independent cases, both truncating, neither rounding. A corpus of exact
durations cannot distinguish three rules; one inexact duration distinguishes
them immediately. Pinned by
`show::tests::format_bit_rate_truncates_to_whole_bits_per_second`.

### The 2026-08-22 `Stream` widening — what it closed

`vaco_format_core::Stream` grew `duration_ts`, an
`r_frame_rate`/`avg_frame_rate` pair and a `side_data` list, and `Program`
grew `program_num`/`pmt_pid`/`pcr_pid`/`pmt_version`. Everything below is
measured against `ffprobe 8.1` under `LC_ALL=C`.

**The corpus is not the one the 748/877 figure was taken on** — that one was
not committed and cannot be reconstructed byte for byte — so two numbers are
given: one on a rebuilt thirteen-file corpus of the same shape (MP4 ×4, MOV,
M4A, MPEG-TS, Matroska ×4, WebM, 1080p MP4), and one on that corpus plus two
files chosen because they *discriminate* rules the old corpus could not see.

| | before | after |
|---|---|---|
| stream field values, 13 files | 1069/1247 | **1083/1247** |
| `format` section matrix, 13 files × 17 option sets | 204/221 | **204/221** |
| files identical on every option set | 12/13 | **12/13** |

and on the fifteen-file corpus, **1191/1371** stream field values, **238/255**
format cells, **14 of 15** files identical on every option set. The one file
that is not is `sub.mkv`, for the one reason recorded below.

The two added files, and why a corpus without them cannot grade this work:

* **`vfr.mp4`** — 1/600 timescale, mostly 60-tick `stts` deltas with a few
  20-tick ones. The reference reports `r_frame_rate=10/1` and
  `avg_frame_rate=300/29`. Every file in the old corpus is constant-rate, where
  the two fields are equal and one field answering both is indistinguishable
  from two fields answering correctly.
* **`odd.mp4`** — a duration that is not a whole number of seconds, so
  `duration_ts` cannot be recovered from a microsecond `Duration`.

The fourteen field values the widening closed on the thirteen-file corpus:

| field | count | what changed |
|---|---:|---|
| `id` | 9 | printed only when the container declares `FormatFlags::SHOW_IDS`. Matroska sets `Stream::id` from `TrackNumber` — `-map 0:#1` needs it — and the reference does not print it. |
| `side_data_type`, `displaymatrix`, `rotation` | 3 | the `[SIDE_DATA]` block for a rotated `tkhd` matrix, which had no representation at all. |
| `duration_ts`, `duration` | 2 | MPEG-TS video, where the demuxer's own frame-duration estimate was measuring a GOP rather than a frame. |

### What is left, and whose it is

Of the 180 stream field values still diverging on the fifteen-file corpus:

* **147 need a bitstream parser** — `profile`, `level`, `pix_fmt`,
  `sample_fmt`, `channels`, `channel_layout`, `has_b_frames`,
  `bits_per_raw_sample`, `is_avc`, `nal_length_size`, `mime_codec_string`, plus
  the nine `ts.ts` fields (`width`, `height`, the two `coded_*`, both aspect
  ratios, `extradata_size`, `sample_rate`, `bit_rate`) that MPEG-TS states
  nowhere at all. None of these is a container-model question; they arrive
  through `ParserProvider` when the parsers are wired.
* **11 are `codec_tag`**, a spelling difference for containers with no
  four-character code: the reference prints `0x0000` where we print
  `0x00000000`.
* **9 are `codec_name`/`codec_long_name`** for codecs `CodecId` has no variant
  for (`subrip`) or whose long name differs (`Opus`).

That leaves thirteen, and they are worth naming individually:

* **`duration_ts`/`duration` on `as.mkv` and `sub.mkv` subtitles** (4). The
  rule that produces them is implemented — a stream with no timing of its own
  takes the container's — but it does not fire, because our discovery loop runs
  until every stream has two DTS deltas and therefore always sees the subtitle
  packet. See below.
* **`duration_ts`/`duration` on MPEG-TS audio** (2). Short by exactly one AAC
  frame; the reference re-frames the PES payload and we do not, which
  `vaco-demux-mpegts`' doc records as a `ParserProvider` gap.
* **`start_pts`/`start_time` on `sub.mkv`** (2), the same cause as the first
  item.
* **`ts_id`, `ts_packetsize`** (2) — MPEG-TS stream tags we do not emit.
* **`TAG:vendor_id`** (1) on a MOV audio track whose `vendor_id` is
  `[0][0][0][0]`.

The fourteen the widening closed and the thirteen container-level ones left are
the whole of what this crate and the three demuxers can affect without a
parser.

#### The `sub.mkv` divergence, now with a cause

The known gap used to read "`Discovery::finish` derives a `start_time` for a
subtitle-only file where the reference reports `N/A`". The cause is now
measured, and it is not about subtitles at all.

The reference's analysis pass stops as soon as every stream's codec parameters
are complete. For a file of subrip — whose parameters come entirely from
`CodecID` and `CodecPrivate` — that is **before it reads a single packet**, so
no stream ever gets a `start_time` from a first PTS, and the container's
timings are handed out instead. Ours stops when every stream has parameters
*and* a first PTS *and* two DTS deltas, so it always reads far enough to see
the subtitle packet and sets `start_time=0` from it.

Four files pin this down. `as.mkv` (opus + subtitle) and `sub.mkv` (subtitle
only) both complete without packets and both get the container fill;
`vs.mkv` and `avs.mkv` contain H.264, whose parameters need packets, so the
loop runs, the subtitle packet is seen, and its `start_pts` comes from the
packet while `duration_ts` stays `N/A`. The presence of a video stream is what
switches the behaviour, and nothing about the subtitle track changes.

Narrowing our stop condition to match is a much larger change than it looks:
`start_time` for every delay-coded audio stream is derived from the first PTS
plus `initial_padding`, and a pass that reads no packets has no first PTS to
work from. Left as a known divergence rather than special-cased, because a
local override of a shared rule is exactly the failure this crate has twice
avoided.

### Failure paths and exit codes

**74 of 74 invocations identical on stdout *and* exit code**, over: no input
file at all (each of the seven writers, plus `-show_error`, `-show_format`, an
unknown `-of` name, and `xml=x=1` with and without `-unit`); five failing opens
(missing file, directory, unreadable file, empty file, one-byte file) crossed
with nine option sets; and successful opens, to prove exit 0 is right where it
is right. Of those, the **45 failing-open invocations are also identical on
stderr**.

Two bugs came out of widening that corpus, and both were shape bugs a narrower
one could not see:

* **No input file exits 1, not 0** — see the probing note above.
* **A failed open still opens the document.** `-of json nope.mp4` must print
  `{\n\n}\n`; we printed a bare `\n`, which is `fini` running without `init` —
  not a missing document but a malformed one. The original corpus tested
  `-show_error` and the `default` writer, and never crossed a document-carrying
  writer with a failure mode.

The eleven extra stream divergences are the demuxer not yet filling
`CodecParameters` (`profile`, `pix_fmt`, `level`, `chroma_location`,
`has_b_frames`, `sample_fmt`, `channels`, `channel_layout`, and
`bits_per_raw_sample` set to 16 on an AAC stream where the reference reports
`N/A`). `mime_codec_string` is absent as a consequence: it is derived from
profile and level, and neither is set.

**We never emit a field the reference does not.** That is checked directly.

## How to change it

A change to observable output needs a reference run in the commit.

* `tests/reference.rs` holds captured `ffprobe` bytes with the invocation that
  produced each one. A change that does not move those bytes did not change
  behaviour; one that does move them needs a new invocation recorded beside the
  new bytes.
* `tests/fields.rs` asserts the emitters walk `fields.rs` in order, per media
  type, and that nothing is emitted that the table does not declare — plan 14
  §4.4's requirement, made mechanical.
* `tests/properties.rs` asserts that the field *order* is a property of the
  table and not of the values, so no combination of present and absent data can
  reorder the output. A reordering that only shows up on some files is the worst
  kind of divergence to find.

## Configuration

No environment variables, no config files. Everything is an option, and every
option is in `vaco_cli_core::table::ffprobe()`.

Exit code: `0` on success, `1` on any failure — including no input file, an
unopenable URL, an unrecognised container and an unknown `-of` name. There is no
exception; an earlier revision of this crate claimed one, and the probing note
above records how it got there.

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-cli-core`, `vaco-textformat`, `vaco-registry`,
`vaco-format-core`, `vaco-codec-core`, `vaco-chlayout`, `vaco-packet`,
`vaco-pixfmt`, `vaco-sampfmt`, `vaco-io`, `vaco-protocol-core`,
`vaco-protocol-file`.

The crate has a `lib` target as well as a `bin`, so `cargo fuzz` can link
against it and `cargo xtask wasm-check` covers it (it passes: the only OS
coupling is `std::fs` behind `vaco-io`, which compiles on wasm and fails
gracefully at runtime).

## Fuzzing

`fuzz/fuzz_targets/probe_argv.rs` drives the whole program from an arbitrary
NUL-separated argument vector — option table, specifier grammar,
`-show_entries`, every writer's option parser, the protocol layer, the probe
engine, the section emitters. Beyond "does not panic" it asserts **determinism**:
the same argv twice must produce the same bytes, because output that depends on
anything but the input cannot be byte-identical to anything.

Last run: `exit=0 execs=2025261`, `find fuzz/artifacts -type f` empty.

## Scoped out this wave

Named, not silently missing.

* **`-show_frames`** — D14.4 moved it to v0.2; it needs decoders. The `frames`
  array is opened and closed so the document shape is right.
* **`-show_packets`** — the `packet` field table and emitter are written and
  tested, but the read loop is not wired: it needs `-read_intervals` and a
  demuxer that returns packets. The `packets` array is opened and closed.
* **`-show_pixel_formats`, `-pix_fmts`, `-sample_fmts`, `-layouts`, `-colors`**
  — the headers are byte-identical; the rows need an "every variant" iterator
  that `vaco-pixfmt`, `vaco-sampfmt` and `vaco-chlayout` do not expose. Writing
  a local list here would duplicate a generated table and start drifting from it.
* **`-show_stream_groups`** — no container in this build produces one.
* **`-count_frames` / `-count_packets` / `-read_intervals` / `-show_data` /
  `-show_data_hash` / `-show_log` / `-analyze_frames`** — parsed and carried,
  not acted on.
* **`av_dump_format` on stderr** — see the correction above.
* **`-h`, `-L`, `-buildconf`, `-version`** — one line each, ours, per D9.

## Known gaps in other crates

Reported, not worked around.

* **`vaco_format_core::DISPOSITION_NAMES` has 15 flags; the reference has 19.**
  Missing `clean_effects`, `timed_thumbnails`, `non_diegetic` and `multilayer`,
  and the order differs from bit 9 onward. Printing the `DISPOSITION` section
  from the container model would produce a 15-field section where the reference
  prints 19. This crate prints from `vaco_cli_core::Disposition::ALL`, which has
  all nineteen in the reference's bit order — the CLI-facing table is the right
  source anyway — so the section is byte-identical, but the container model
  cannot represent four of the flags and they are always zero.
* **`Discovery` reads further than the reference's analysis pass**, which is
  what leaves `sub.mkv` reporting `start_pts=0` where the reference reports
  `N/A`. Cause measured; see *The `sub.mkv` divergence, now with a cause*
  above. Not worked around locally, because a local override of a shared rule
  is the exact failure `vaco-demux-matroska` avoided by not setting
  `start_time` in the first place.
* **`vaco-demux-mpegts` emits a `ts_codec` stream tag the reference does not.**
  The only field we emit that `ffprobe` has no counterpart for.
* **`CodecParameters` has no `max_bit_rate`**, and `bits_per_raw_sample` is on
  `AudioParameters` only. Both are printed for every stream by the reference.
* ~~**`Stream` has no `r_frame_rate` distinct from `avg_frame_rate`.**~~ Closed:
  they are two fields now, and `vfr.mp4` is the regression fixture that would
  notice if they were merged again.
* **`vaco_io::IoContext` cannot give its source back** — no `into_inner`, no
  `into_source`. Probing and demuxing each need to own a `Box<dyn MediaSource>`,
  so a probed open reads the URL **twice**: once through an `IoContext` that
  peeks and is then dropped, once for the demuxer. Correct for a seekable
  transport, wrong for a pipe, which cannot be reopened. `-f` skips probing and
  opens once.
* **`vaco_textformat::sections` nests `stream_group_pieces` under `component`;
  ffprobe 8.1 nests it under `subcomponent`.** Six lines of `-sections` output
  (`pieces` … `block`) come out eight columns left of the reference's. Line
  order is unaffected and no other section differs.
  `listing::tests::the_stream_group_pieces_divergence_still_exists` pins it.
* **`ProtocolFlags` records `network`, `nested_scheme` and `server_capable` but
  not read/write capability**, so `-protocols` cannot split its Input and Output
  lists the way the reference does. Every protocol currently appears under both.
* **`vaco-protocol-file` ships no `vaco-component.toml`**, so
  `vaco_registry::protocol_registry()` is empty and this crate registers
  `file:`/`pipe:` itself.
