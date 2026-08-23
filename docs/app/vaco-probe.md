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
     ──▶ [packets]  one pass: -show_packets, -count_packets,
                    -select_streams, -read_intervals         (this crate)
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

### `packets.rs`, `intervals.rs`, `dump.rs` — the `[PACKET]` section

Three modules, one pass over the file.

Before this wave `-show_packets` opened the `packets` array and closed it again:
a stub that printed nothing and exited 0. That is the worst failure shape
available, because silence with exit 0 is indistinguishable from "this file has
no packets" and a differential harness records it as a pass.

`packets::read` is the loop, and it serves `-show_packets`, `-count_packets`,
`-select_streams` and `-read_intervals` together because the reference serves
them together:

```text
for interval in intervals:
    seek if it has a start
    loop:
        packet = demuxer.read_packet()      -- an error or EOF ends everything
        skip it unless -select_streams admits it
        cursor.admit(pts) -> Show: emit and count
                          -> Stop: this packet is DROPPED, next interval
```

The observable consequence of it being one pass is that `-count_packets`
`-read_intervals '%+#3'` reports 3, not the file's total — the counter counts
what was *shown*.

#### Five rules that are not derivable and were measured

The commands are in each module's `# Provenance` block. In summary:

| Rule | Observed |
|---|---|
| An interval boundary **eats one packet** | `-read_intervals '%+#1,%+#1'` prints the packets at offsets 48 and **7675**, skipping 5219. The packet that ends an interval has already been consumed. |
| `#N` counts **selected** packets only | `-select_streams v -read_intervals '%+#1,%+#1'` skips the second *video* packet, not the second packet. |
| `#` is legal only after `%+` | `#5`, `+#5` are start errors; `%#5` is an end error. |
| A malformed `#N` is a **warning**, not an error | `-read_intervals '%+#-1'` prints `Invalid or negative value '-1' …`, shows nothing, and **exits 0**. |
| An offset end is measured from the position **found** | `1%+0.04` on a file whose only keyframe is at 0 ends at 0.04, not 1.04. |

And one that cost a wrong first implementation:

> **`pos` is a plain integer string, not a byte *value*.** Under `-unit
> -prefix`, `size` prints `5.171000 Kbyte` and `pos` — the next field, also a
> byte count — prints `48`. Typing it `Ty::Size` because it holds a byte count
> looks obviously right and is wrong in four of the seven formatting modes.
> Pinned by `tests/packets.rs::size_scales_under_pretty_and_pos_does_not`.

#### `flags` is `K`/`D`/`C`, in that order

`vaco_textformat::num::packet_flags(key, discard, corrupt)` already existed; it
was checked rather than trusted, because the `PacketFlags` bits are numbered
KEY/CORRUPT/DISCARD and a helper written from the bit order would be wrong in
the middle character only. Three files settle it:

| File | Packet | `flags` |
|---|---|---|
| `av.mp4` | first AAC packet (encoder delay) | `KD_` |
| `av.ts` with one 188-byte TS packet removed | the packet spanning the gap | `K_C` |
| any | ordinary | `K__` / `___` |

So the middle character is D and the last is C. The helper is correct.

#### `-show_data`, `-data_dump_format`, `-show_data_hash`

`dump.rs`. The hexdump geometry was measured on rawvideo files of exactly *n*
bytes for n ∈ {1,2,3,4,5,15,16,17,31,32,33}, which pins the partial-line padding
at every position within a group and at both group boundaries: the ASCII column
starts at byte **51** of every line, a missing byte contributes two spaces so
the group separator survives, and `isprint` is the C-locale range 0x20–0x7e so
a space prints as a space.

**base64 wraps at 80 characters**, and that was missed the first time: a 17-byte
file produces one short line and looks unwrapped. It only appears on a payload
long enough to exceed 80 base64 characters — the 5 171-byte first video packet
of `av.mp4` renders as 87 lines, 86 of exactly 80 and a last of 16. *A short
input is not a small version of a long one.*

The hash names are matched **case-insensitively** and printed in the
reference's own spelling, which is not uniform: `md5` → `MD5`, `crc32` →
`CRC32`, `ADLER32` → **`adler32`** (lower case, alone among the fifteen), and
`sha1` is **rejected** — the name is `SHA160`.

Ten of the fifteen are implemented, from crates already in
`[workspace.dependencies]`: MD5, SHA160/224/256/384/512, SHA512/224,
SHA512/256, CRC32 (the ordinary reflected IEEE polynomial, checked against
Python's `zlib`) and adler32 (nine lines, written out rather than adopted).
**`murmur3` and RIPEMD128/160/256/320 have no pre-declared pure-Rust crate**, so
`-show_data_hash RIPEMD160` **fails with `Unsupported`** naming them. The first
version returned `None` and, because `data_hash` is `Absent::Omit`, printed a
perfectly ordinary packet with no `data_hash` line and exit 0 — the same
silent-success shape this whole wave replaced.

#### `-show_frames` fails loudly

D5 gives v0.1 zero decoders and a frame section reports *decoded* frame
properties; D14.4 moved `-show_frames`, `-count_frames` and `-analyze_frames` to
v0.2. So they return `Error::Unsupported` naming the decision and the work
package, and exit 1, before a byte is written:

```console
$ vaco-probe -show_frames av.mp4; echo $?
unsupported: -show_frames/-count_frames need a decoder; v0.1 has none (D5, D14.4 — roadmap CL-34b/v0.2)
1
```

`vaco-cli` set the precedent with `AvError::ENOSYS` for its unimplemented
listings. A gap you can see beats a gap that looks like an empty answer.

#### Bounding the work

Two bounds, answering different questions.

* **`-read_intervals` is the user's bound.** Without it a packet dump is
  unbounded by construction.
* **A `vaco_limits::Budget` is the safety bound.** One unit of fuel per packet
  read, so a demuxer that returns packets without consuming input terminates
  instead of spinning. `run` uses `Limits::permissive()` (2³² packets, four
  orders of magnitude above any real file); `run_with_limits` exists so the
  fuzz target can pass `Limits::tiny()`.

A read error needs no bound: the reference stops the whole read on any
`av_read_frame` failure, so a corrupt file terminates by the same path a
well-formed one does.


## Measured fidelity

Two independent measurements, because they answer different questions.

### With correct inputs (`tests/reference.rs`)

Streams built to match what a correct MP4 demuxer must produce, rendered and
diffed against captured `ffprobe` bytes:

* `format`: **byte-identical**, whole document including trailing newlines, in
  `default`, `ini` and `xml`.
* `stream`: **all 116 lines byte-identical** across both streams of `av.mp4`.

It was 113 of 116 until the parser wiring landed. The three that used to differ,
each a missing *input* rather than a formatting choice, and what closed each:

| Line | Why it was missing | Closed by |
|---|---|---|
| `is_avc=true` | h264 decoder private option (`-show_private_data`, on by default). No decoder, and `CodecParameters` had nowhere to put it. | `VideoParameters::nal_length_size`, filled by `vaco-parse-h264` from `avcC` |
| `nal_length_size=4` | same | same |
| `bits_per_raw_sample=8` on the video stream | the field lived on `AudioParameters` only, so a video stream could not report it | `VideoParameters::bits_per_raw_sample` |

The list is asserted **exactly**, so a divergence that disappears fails just as
loudly as a new one — and it has, three times now. `codec_long_name` was a fourth
entry (`vaco-codec-core` said `H.264 / AVC / MPEG-4 AVC` where the reference
appends ` / MPEG-4 part 10`); it was reported rather than fixed here, closed
upstream mid-wave, and the assertion failed the moment it was. The three above
did the same. That is the behaviour to keep: a closing divergence is a change to
observable output and gets reviewed like any other.

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

### The packet section, measured per container

`ffprobe` 8.1 against `vaco-probe`, `-of json -show_packets -read_intervals
'%+#40'`, comparing all eleven field values packet by packet over seventeen
files. Every divergence below is a **demuxer** fact, not a section-emitter one.

| Container | Field values matched | Files |
|---|---|---|
| **MP4 / MOV / M4A** | **2 431 / 2 431** | `av.mp4`, `frag.mp4`, `prog.mp4`, `hd.mp4`, `mono.m4a`, `small.mov`, `subs.mp4`, `col.mp4` |
| **Matroska / WebM** | 1 292 / 1 562 | `av.mkv`, `op_st.webm`, `vs.mkv`, `as.mkv`, `sub.mkv` |
| **MPEG-TS** | 186 / 550 | `av.ts`, `ts.ts` |
| Total | 3 909 / 4 543 | |

Per field:

| Field | MP4 | Matroska | MPEG-TS |
|---|---|---|---|
| `codec_type` | 221/221 | 142/142 | 28/50 |
| `stream_index` | 221/221 | 142/142 | 28/50 |
| `pts` / `pts_time` | 221/221 | 142/142 | 17/50 |
| `dts` / `dts_time` | 221/221 | **111/142** | 17/50 |
| `duration` / `duration_time` | 221/221 | **38/142** | **0/50** |
| `size` | 221/221 | 142/142 | 17/50 |
| `pos` | 221/221 | 142/142 | 17/50 |
| `flags` | 221/221 | 142/142 | 28/50 |
| `side_data_list` present | 1/1 | 1/1 | **0/25** |

**MP4 is byte-identical**, including `pos` — the field the wave brief flagged as
the one containers disagree about. `pos=48` is the offset of the sample data in
`mdat`; Matroska's `pos` is the block's own offset and also matches;
MPEG-TS's is the 188-byte-aligned offset of the TS packet that begins the PES,
and matches on every packet whose ordering matches.

The three divergences, all upstream of this crate:

1. **`vaco-demux-matroska` sets no packet `dts` and no packet `duration`** on
   most packets. Never a wrong value — always absent. 31 `dts` and 104
   `duration` field values on the corpus.
2. **`vaco-demux-mpegts` reports half the packet duration.** `3600` ticks at
   1/90000 in the reference, `1800` in ours — a factor of exactly two, which is
   the field rate standing in for the frame rate. The first few packets carry no
   duration at all.
3. **`vaco-demux-mpegts` interleaves differently.** From packet 7 onward it
   emits video where the reference emits audio, so every field after that point
   is compared against the wrong packet. The 17/50 and 28/50 columns above are
   an *ordering* difference, not eleven separate field bugs.
4. **`vaco_packet::PacketSideData` has no `MPEGTS Stream ID` variant**, which
   the reference attaches to every MPEG-TS packet. `Skip Samples` is modelled
   and matches on both MP4 and Matroska.

### The `-show_packets` option matrix

Full cross-product on `av.mp4`: 14 writer specs × 6 formatting modes × 4
`-select_streams` values × 5 `-read_intervals` values × 3
`-show_optional_fields` values = **5 040 invocations**, compared as exact
stdout bytes plus exit code.

**4 620 / 5 040 byte-identical.**

All 420 failures are one class: **a seeking interval combined with
`-select_streams a`**. `ffprobe` seeks with stream index `-1`, letting
libavformat pick a stream and then reposition *each track* to its own nearest
sample at or before the target; `vaco_format_core::SeekTarget` has no such
spelling, so `packets::seek` names the first video stream (which is what
`av_find_default_stream_index` picks). `vaco-demux-mp4` then rewinds the audio
track to the start of the file rather than to its own nearest sample, so
`-select_streams a -read_intervals '1%+#3'` starts at 0 where the reference
starts at 0.998458. Both halves are recorded under *Known gaps in other
crates*.

### `-show_optional_fields never` suppressed nothing, and now suppresses everything

Found by the matrix above, and it is **not specific to packets** — it was wrong
for `[FORMAT]` and `[STREAM]` in exactly the same way, so this is a
pre-existing crate-wide divergence that the packet work happened to expose.

The option's name suggests it hides *unavailable* fields. Measured, it hides
**every** field:

```console
$ ffprobe -v error -of default -show_format -show_optional_fields never av.mp4
[FORMAT]
[/FORMAT]
$ ffprobe -v error -of xml -show_packets -show_optional_fields never … av.mp4
        <packet />
```

`filename`, `index`, `codec_type` and `flags` all go, and so do the `TAG:` and
`DISPOSITION:` lines. But the **sections** stay — `json` still emits
`"tags": {}` and `xml` still emits `<side_data type="Skip Samples">`. So the
rule is "no fields", not "no content", and the `type` attribute of a typed
section is not a field.

`Emit` now enforces that in one place: `Emit::put`, `int`, `int_opt`, `str`,
`ts`, `duration` and `tag` all return early, and `Emit::tf()` is documented as
being for section open/close **only**. Every field emitter in `show.rs` was
moved onto those wrappers, so a new call site cannot bypass the policy by
reaching for the formatter directly. This closed 1 428 of the 1 848 matrix
failures on its own.

Re-measured across the stream and format sections — 10 files × 7 writers × 5
section combinations × 3 policies, 1 050 invocations — `never` now scores
**321/350**, ahead of `auto` (269) and `always` (273), where before the fix it
could only have matched on a section with no fields at all. Of the 29 that
remain, **21 are the `ini` writer omitting the blank line after an *empty*
section**, a shape that was unreachable until `never` started producing empty
sections; the other 8 are the known `ts_codec` tag on MPEG-TS streams, which
opens a `tags` section where the reference has none. Both belong to other
crates and are listed below.

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

### The 2026-08-22 parser wiring — what it closed

`vaco_registry::Parsers` used to return `None` for every codec, so no
`vaco-parse-*` crate was reachable from a demuxer at all. It is a real
`ParserProvider` now. Measured on a sixteen-file corpus of the same shape as the
one above (MP4 ×5 including a 1080p and a variable-rate file, MOV, M4A, MPEG-TS,
Matroska ×4, WebM, plus an HEVC and an AV1 MP4), `ffprobe 8.1` under `LC_ALL=C`:

| | before | after |
|---|---|---|
| stream field values | 1193/1376 | **1334/1375** |
| `format` section matrix, 16 files × 17 option sets | 240/272 | **240/272** |
| files identical on every format option set | 14/16 | **14/16** |

**141 of the 183 diverging values closed.** The `format` section is untouched by
design: nothing it prints comes from a bitstream. The denominator drops by one
because we stopped emitting a `mime_codec_string` for HEVC, which the reference
does not print at all.

What closed, and where the work actually was:

| field | count | source |
|---|---:|---|
| `profile` | 20 | the bitstream, through the container's configuration record |
| `pix_fmt` | 12 | ditto |
| `level` | 12 | ditto |
| `has_b_frames` | 11 | ditto — and it is an integer *reorder depth*, `max_num_reorder_frames` for H.264 and `max_num_reorder_pics` for HEVC, not a boolean |
| `sample_fmt` | 8 | the decoder's output format, which the parser now states |
| `bits_per_raw_sample` | 17 | **video** `VideoParameters`, plus float-audio suppression |
| `is_avc` / `nal_length_size` | 20 | the container's `avcC`, read by the parser |
| `chroma_location` | 9 | the VUI, once `VideoParameters::fill_from` merged colour per property |
| the nine `ts.ts` geometry and rate fields | 9 | in-band SPS, which MPEG-TS states nowhere |
| `channels` / `channel_layout` | 10 | `AudioSpecificConfig` and `OpusHead` |
| `mime_codec_string` | 13 | derived from profile and level, plus AV1's four-part form |
| `codec_long_name` | 3 | `vaco-codec-core`'s table: Opus and HEVC both had short names |

#### Three things measured per codec that did not transfer

Assuming any of these from another codec would have been wrong, and the brief
was right to warn about it:

* **`coded_width` is the *display* size for H.264 and the *coded* size for
  HEVC.** The same 1918x1080 source: H.264 reports `coded_width=1918`, HEVC
  reports `1920`. AV1 has no coded/display split at all.
* **`bits_per_raw_sample` is H.264-only.** `8` for 8-bit H.264 and `10` for
  10-bit, and `N/A` for HEVC, AV1 and VP9 at the same depth.
* **The private-data block between `field_order` and `id` changes shape per
  codec.** h264 prints `is_avc` and `nal_length_size`; hevc prints
  `view_ids_available=""` and `view_pos_available=""`; av1 prints nothing.

#### Which of the "147 need a parser" were really container fields

Four of them, and they were worth separating:

* **`is_avc` and `nal_length_size` (20 values) are container facts**, not
  bitstream ones — they describe how the *container* frames its NAL units. The
  reference prints them for every H.264 stream, `true`/`4` in MP4 and
  `false`/`0` in MPEG-TS. Only a parser can read them, because they live inside
  `avcC`, but they are not derived from the coded video at all. They reach
  `vaco-probe` through `VideoParameters::nal_length_size`, where `Some(0)` is a
  value and `None` means the codec has no such option.
* **`chroma_location` (9 values) was neither**: the parser had it all along and
  `VideoParameters::fill_from` replaced the whole `ColorInfo` block rather than
  merging it property by property, so a container that stated primaries and
  transfer (MP4's `colr`, which has no chroma siting field) blocked the
  parser's chroma location from ever landing.
* **`field_order` on HEVC in MP4 is a container field we still miss.** Probed
  both ways: `ffprobe -f hevc` on the raw Annex B stream reports
  `field_order=unknown`, the same content in MP4 reports `progressive`, and the
  difference is the MOV **`fiel` atom**, which the file carries and
  `vaco-demux-mp4` does not read. `vaco-parse-hevc`'s D17 note was correct and
  the earlier agent measured it correctly; the MP4 value comes from somewhere
  else entirely.

### What is left, and whose it is

Of the 41 stream field values still diverging on the sixteen-file corpus:

* **12 are `codec_tag`**, a one-character fix in a crate this work does not own:
  `vaco_textformat::num::codec_tag` formats `0x{v:08x}` and the reference uses a
  minimum width of four. Measured: `avc1` prints `0x31637661` in both, MPEG-TS
  stream type 27 prints `0x001b` in the reference and `0x0000001b` here.
  Reported, not worked around — formatting helpers live in `vaco-textformat` and
  duplicating one here to fix twelve values is how the two start drifting.
* **8 are `subrip`**, four `codec_name` and four `codec_long_name`. `CodecId`
  has no `SubRip` variant, and adding one needs `vaco-demux-matroska` to map
  `S_TEXT/UTF8` to it, so it is a two-crate change.
* **7 are the `sub.mkv` and `as.mkv` subtitle timings** — `duration_ts`,
  `duration`, `start_pts`, `start_time` — the known `Discovery` stop-condition
  divergence recorded below, unchanged.
* **6 are MPEG-TS container facts**: `ts_id` and `ts_packetsize` (not emitted),
  `tags.ts_codec` (emitted and the reference has no counterpart), and the AAC
  `duration`/`duration_ts`/`bit_rate`, which are short by one frame because the
  reference re-frames the PES payload and `vaco-demux-mpegts` does not.
* **3 are VP9** — `profile`, `pix_fmt` and the full `mime_codec_string`
  (`vp09.00.10.08`). There is no `vaco-parse-vp9`, and `ParserProvider` correctly
  answers `None`, so the container's own fields are reported. This is the seam
  working, not failing.
* **1 is `ts.ts`'s `extradata_size`.** The reference synthesises Annex B
  extradata for an MPEG-TS H.264 stream from the in-band parameter sets and
  reports 38 bytes — exactly `4 + SPS(26) + 4 + PPS(4)`. `H264Parser` stores
  parsed parameter sets, not the raw NAL bytes, so it cannot rebuild them.
  Deferred deliberately: it is one value on one file and reaching the raw bytes
  means touching the access-unit machinery.
* **1 is `hevc.mp4`'s `field_order`**, the `fiel` atom above.
* **1 is `av.mov`'s `TAG:vendor_id`** on an audio track whose `vendor_id` is
  `[0][0][0][0]`.
* **1 is `av1.mp4`'s `mime_codec_string`** — closed, in fact; the remaining AV1
  row is the VP9 one above.

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

Those eleven extra divergences were the demuxer not filling `CodecParameters`
(`profile`, `pix_fmt`, `level`, `chroma_location`, `has_b_frames`, `sample_fmt`,
`channels`, `channel_layout`, and `bits_per_raw_sample` set to 16 on an AAC
stream where the reference reports `N/A`), with `mime_codec_string` absent as a
consequence. All but the last are closed by the parser wiring; see *The
2026-08-22 parser wiring* above.

`bits_per_raw_sample=16` on AAC is the one that is **not** a missing input but a
misfiled one, and it is worth stating precisely because it looks like a bug in
this crate. `vaco-demux-mp4` and `vaco-demux-matroska` fill
`AudioParameters::bits_per_raw_sample` from the container's own sample depth —
MP4's `stsd` sample entry `sample_size`, Matroska's `BitDepth`. Probed on a WAV,
`pcm_s16le` reports `bits_per_sample=16` and `bits_per_raw_sample="N/A"`, so
that number is `bits_per_coded_sample`, a **different field** that
`CodecParameters` has nowhere to hold. Until it does, `show::bits_per_raw_sample`
suppresses the value for a stream whose decoded sample format is floating point:
a raw-sample bit count is meaningless for a float decoder, and every
float-output stream measured (AAC in MP4, MOV, Matroska and MPEG-TS; Opus in
Matroska and WebM) reports `N/A`. Integer audio is untouched — `pcm_s24le` in
MOV reports `24` and must keep doing so. Reported upstream rather than called a
fix.

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
* `tests/packets.rs` holds captured `[PACKET]` bytes for the default, json and
  compact writers, plus the `-pretty` line that pins `size` scaling and `pos`
  not scaling, plus the two loud-failure paths (`-show_frames` and an
  unimplemented hash). The packets are replayed rather than demuxed, for the
  same reason `reference.rs` replays streams: this file is about which fields in
  what order, and mixing a demuxer in would make every demuxer change a failure
  here.

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
`vaco-pixfmt`, `vaco-sampfmt`, `vaco-io`, `vaco-limits`, `vaco-protocol-core`,
`vaco-protocol-file`.

Four third-party crates, all pre-declared in `[workspace.dependencies]`, all
pure Rust (D10 Gate 1), all reachable from this crate and no other (D11):
`md-5`, `sha1`, `sha2` and `crc`, for `-show_data_hash`. They cover ten of the
reference's fifteen algorithm names; `adler32` is written out here because a
dependency that only ever computes nine lines is a D10 adoption for no
reduction in code. `murmur3` and the four RIPEMD variants are refused by name
rather than adopted — see above.

`vaco-limits` moved from a dev-dependency to a real one: `packets::read` needs a
`Budget` to bound the read loop, and a bound that only exists in tests is not a
bound.

The crate has a `lib` target as well as a `bin`, so `cargo fuzz` can link
against it and `cargo xtask wasm-check` covers it (it passes: the only OS
coupling is `std::fs` behind `vaco-io`, which compiles on wasm and fails
gracefully at runtime).

## Fuzzing

Two targets now matter for this crate, and the second is not in it.

`fuzz/fuzz_targets/registry_discovery.rs` (`fuzz-crate: vaco-registry`) drives
arbitrary bytes through a real demuxer into a **real bitstream parser** — the
composition `vaco-probe` actually runs. Before the parser wiring, no target
covered it: `dem_mp4`, `matroska_demux` and `mpegts_demux` all run with
`NoParsers`, deliberately, so demuxer fuzzing stays fast and independent of
codec code. The composition is where the bounds multiply, because a hostile file
chooses *which* parser runs over its payloads and *what* configuration record
that parser is handed. Last run: `exit=0 execs=302393`,
`find fuzz/artifacts -type f` empty.

Its first run found a bug in its own assertion rather than in the code:
`Discovery::run` marks itself as having run before the loop, so a pass that ends
in an error still reports `Ok` on a second call. That is the documented no-op
behaviour; the target now asserts on *work done* (`packets_read`, `bytes_read`)
instead of on the return value, and the input is kept in the corpus.

`fuzz/fuzz_targets/probe_argv.rs` drives the whole program from an arbitrary
NUL-separated argument vector — option table, specifier grammar,
`-show_entries`, every writer's option parser, the protocol layer, the probe
engine, the section emitters. Beyond "does not panic" it asserts **determinism**:
the same argv twice must produce the same bytes, because output that depends on
anything but the input cannot be byte-identical to anything.

Last run: `exit=0 execs=1779115`, `find fuzz/artifacts -type f` empty.

`fuzz/fuzz_targets/probe_packets.rs` starts where `probe_argv` stops. `probe_argv`
never reaches a packet, because the paths it invents do not exist and the run
ends at the open; this target supplies an arbitrary `-read_intervals` spec, an
arbitrary packet payload and an arbitrary writer, which is the combination
`-show_packets` actually runs. Beyond "does not panic" it asserts that
`intervals::parse` is total, that the hexdump's ASCII column stays at byte 51 at
every payload length, that no base64 line exceeds the wrap width, that every
`HashAlg` agrees with its own `implemented()` flag, that no writer emits
non-UTF-8 (a payload byte reaching the sink means a dump was bypassed), and that
the counts never exceed what the intervals allow.

Last run: `exit=0 execs=265611`, `find fuzz/artifacts -type f` empty.

**Its first version measured 1 exec/s** and that is worth recording. The packet
source never ended, so every iteration ran the full `Limits::tiny()` budget —
65 536 packets, each emitting a section. libFuzzer needs thousands of execs to
be worth running. That is not a finding about `vaco-probe`; it is the harness
paying the bound's full price on every input, and the bound is already pinned
deterministically by
`packets::tests::a_demuxer_that_never_ends_is_bounded_by_the_budget`. The source
is now finite and derived from the input length — what a real file looks like —
and the target explores option shapes instead of re-proving one constant.
155 361 execs in the first 60 s after the change.

## Scoped out this wave

Named, not silently missing.

* **`-show_frames`, `-count_frames`** — D14.4 moved them to v0.2; they need
  decoders. They no longer print an empty `[FRAMES]` array: they return
  `Error::Unsupported` naming D14.4 and exit 1. See above.
* ~~**`-show_packets`**~~ — done this wave, together with `-read_intervals`,
  `-count_packets`, `-select_streams` over packets, `-show_data`,
  `-data_dump_format` and `-show_data_hash`.
* **`-read_intervals` grammar deferred: nothing.** The full grammar is
  implemented — `START`, `+START_OFFSET`, `%END`, `%+END_OFFSET`, `%+#COUNT`,
  and comma-separated lists, with the duration grammar
  `[ws][sign]D+[:D+[:D+]][.D*][s|ms|us]`. What is *approximated* is the
  execution of a seeking start: see the option matrix above.
* **`-show_pixel_formats`, `-pix_fmts`, `-sample_fmts`, `-layouts`, `-colors`**
  — the headers are byte-identical; the rows need an "every variant" iterator
  that `vaco-pixfmt`, `vaco-sampfmt` and `vaco-chlayout` do not expose. Writing
  a local list here would duplicate a generated table and start drifting from it.
* **`-show_stream_groups`** — no container in this build produces one.
* **`-show_log` / `-analyze_frames`** — parsed and carried, not acted on.
* **`-show_data_hash murmur3` and the four RIPEMD variants** — refused by name
  with `Unsupported`, because no pure-Rust crate for them is pre-declared and
  D10 makes adding one a reviewed decision.
* **`-show_entries <section>=<fields>` combined with `-show_<section>`** — the
  reference *ignores the field filter* in that case and prints every field;
  we honour the filter. Measured on both `stream` and `packet`. Pre-existing
  and general, not a packet issue; fixing it needs an additive
  `EntryFilterSet::show_all(SectionId)` in `vaco-textformat`, which this crate
  does not own.
* **The two extra `-read_intervals` diagnostic lines.** The reference prints
  `Invalid interval start specification 'x'` and `Error parsing read interval
  #0 'x'` before the `Failed to set value …` line; we print only the last.
  Plan 14 §5.6 makes the exit code conformance surface here and not the
  message.
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
* **`CodecParameters` has no `max_bit_rate`**, printed for every stream by the
  reference. ~~`bits_per_raw_sample` is on `AudioParameters` only.~~ Closed:
  `VideoParameters` carries one too, and that is the half the reference
  actually prints. What remains is the *audio* half being filled with the
  container's coded sample depth; see above.
* **`vaco_textformat::num::codec_tag` formats `0x{v:08x}`; the reference uses a
  minimum width of four.** `avc1` prints `0x31637661` in both, but MPEG-TS
  stream type 27 prints `0x001b` there and `0x0000001b` here. Twelve field
  values on the corpus, and a one-character fix in a crate this one does not
  own. Not worked around locally: `fields.rs`'s own rule is that formatting
  helpers live in `vaco-textformat` and none may be duplicated here.
* **`vaco-demux-mp4` does not read the MOV `fiel` atom.** Probed both ways:
  `ffprobe -f hevc` on a raw Annex B stream reports `field_order=unknown`, and
  the same content in MP4 reports `progressive`. The bitstream parser is right
  to report `unknown` — the MP4 value comes from `fiel`, which the file carries.
* **`CodecId` has no `SubRip` variant**, so a Matroska `S_TEXT/UTF8` track
  reports `codec_name=unknown` where the reference prints `subrip`. Eight field
  values on the corpus, and a two-crate change: the variant in
  `vaco-codec-core` and the mapping in `vaco-demux-matroska`.
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
* **`vaco-demux-matroska` sets no packet `dts` and no packet `duration`.** The
  reference reports both from `BlockGroup`/`DefaultDuration`. 31 `dts` and 104
  `duration` field values on the packet corpus; never a wrong value, always
  absent.
* **`vaco-demux-mpegts` reports exactly half the packet duration** — `1800`
  ticks at 1/90000 where the reference reports `3600`. A factor of two is the
  field rate standing in for the frame rate. It also emits no duration at all
  on the first packets, and its packet *ordering* diverges from packet 7 of
  `av.ts`, emitting video where the reference emits audio.
* **`vaco_packet::PacketSideData` has no `MPEGTS Stream ID` variant.** The
  reference attaches one to every MPEG-TS packet, so every `[PACKET]` from a
  transport stream is missing a `[SIDE_DATA]` block. `Skip Samples` is modelled
  and byte-identical on MP4 and Matroska.
* **`vaco_format_core::SeekTarget` cannot express "the default stream".**
  `ffprobe` seeks with stream index `-1` against `AV_TIME_BASE_Q`; ours must
  name a stream and a timestamp in that stream's base. `packets::seek` picks the
  first video stream, mirroring `av_find_default_stream_index`, and says so.
* **`vaco-demux-mp4` rewinds every track to the start when seeking.** Seeking
  the video track to 1 s on a file whose only keyframe is at 0 also puts the
  *audio* track back at the start; the reference leaves audio at its own nearest
  sample, 0.998458. This is the single remaining class in the `-show_packets`
  option matrix — 420 of 5 040 invocations.
* **`vaco_textformat`'s `ini` writer omits the blank line after an empty
  section.** The reference prints one, so
  `-of ini -show_streams -show_optional_fields never` differs by two blank lines
  per stream. Unreachable until `never` began producing empty sections; 21 of
  the 29 residual `never` failures across the stream/format matrix.
* **`vaco_packet::PacketSideData::SkipSamples` has no `skip_reason` /
  `discard_reason`.** The reference prints both, always 0 on every file
  measured, so this crate emits 0 to keep the block's shape. If a container ever
  sets them it becomes a divergence.
