# `vaco-demux-mpegts`

Layer 4. The MPEG-TS demuxer: packet framing, PES assembly, the 33-bit clock,
duration estimation and seeking. The PSI/SI layer it stands on is
`vaco-format-mpegts-tables`, which has no I/O at all.

---

## What it is

MP4 ships an index. Matroska ships cues. **MPEG-TS ships nothing.** It was
designed to be broadcast, so a decoder joining halfway through has to bootstrap
itself from the stream, and every awkward thing about this crate follows from
that:

| Fact | Consequence |
|---|---|
| No duration field | The length of a file is *estimated* by reading its tail |
| No table of contents | Seeking bisects byte positions and reads whatever timestamp it lands near |
| No stream list, only a repeating PAT and PMT | Discovery is progressive; a stream can appear minutes in |
| 33-bit timestamps at 90 kHz | A recording past ~26.5 hours wraps |
| An adaptation field that can declare a jump | Some discontinuities are *legitimate* and must survive untouched |

| Module | Contents |
|---|---|
| `probe` | content detection, scores measured against ffprobe 8.1 |
| `pes` | PES packet headers and the 33-bit timestamp field |
| `demux` | framing, PES assembly, the clock, duration, seeking |
| `raw` | the `mpegtsraw` registration: one raw 188-byte transport packet per output `Packet`, no PES reassembly at all |

This crate ships **two** registry descriptors, [`DEMUXER`] (`mpegts`) and
[`raw::RAW_DEMUXER`] (`mpegtsraw`). `m2ts` is **not** a third: confirmed with
`ffmpeg -demuxers`, which lists `mpegts` and `mpegtsraw` as distinct entries
but no `m2ts` at all. Blu-ray's four-byte-timestamp-prefixed stride is one of
`PacketStride`'s three variants and is autodetected the same way 188 and 204
are — there is nothing for a third descriptor to do.

---

## How it works

### Probing — measured, not derived

Truncating a muxed `.ts` file to `N` packets and reading `format.probe_score`
from ffprobe 8.1:

| packets present | reference `probe_score` |
|---|---|
| 1-2 | rejected outright (no streams) |
| 3-10 | **2** |
| 11 and up | **50** |

Two things follow. First, **`ProbeScore`'s convention table cannot express
50**: `repeating(n)` is `min(100, 25 + 8n)`, which takes the values 33, 41,
49, 57 — it steps over the reference's answer. `EXTENSION` happens to equal 50
but means something else. `TS_SCORE_STRONG` is therefore declared here as a
measured constant, and the gap is reported below.

Second, **the low-confidence answer is 2, not 25**, which is below
`ProbeScore::RETRY` — so a short TS prefix does not ask the probe loop for more
data the way a weak guess normally would. Reproduced because `probe_score` is a
printed field and D6 makes it conformance surface.

### Framing and resynchronisation

The stride (188 / 192 M2TS / 204 with Reed-Solomon parity) and the offset of
the first sync byte are found once, from a 64 KiB peek, by counting consecutive
strided `0x47`s. 188 wins ties, because a 192-byte file is also
188-consistent at some offsets and a 204-byte file trivially contains 188-byte
packets.

Losing alignment mid-file triggers a byte-at-a-time scan for the next sync
byte, bounded by `MAX_RESYNC_BYTES` (1 MiB). Bounded, because an unbounded
scan over attacker-chosen bytes is the same denial-of-service shape plan 13
§1c records for start-code scanning.

204-byte packets have their sixteen parity bytes ignored; we do not FEC-correct.
M2TS's four-byte `TP_extra_header` is skipped — see *Deliberately deferred*.

### Continuity

The four-bit counter advances only on packets that carry a payload. Three
outcomes:

* **Match** — normal.
* **Repeat** — the spec permits one exact duplicate; its payload is dropped
  rather than appended twice, which is the difference between a working stream
  and one where every PES packet has a doubled tail.
* **Gap** — the PES packet in progress is flagged `CORRUPT` and the section
  assembler on a PSI PID abandons what it holds *and its alignment*.

A `discontinuity_indicator` in the adaptation field resets the expectation
without flagging anything: it is a splice, not a loss.

### PES assembly

Per PID, accumulate from a `payload_unit_start` packet until either
`PES_packet_length` bytes have arrived or the next `payload_unit_start` does.
For video with `PES_packet_length == 0` — legal, and universal — only the next
one terminates it, so the last packet of a file exists solely because end of
input completed it. `MAX_PES_BYTES` (6 MiB) bounds the accumulation, and every
appended byte is charged to the `Budget` besides, so a `Limits::strict` caller
gets a smaller ceiling still.

Once a PES completes, `flush_pes` turns it into one or more `Packet`s. For
`CodecId::Aac` it is *one or more*: `split_adts` walks the payload as ADTS
frames (`parse_adts_header`, ISO/IEC 13818-7 Annex B) and one `Packet` comes
out per frame, `pos` set only on the one that opened the PES. Every other
codec — including LATM AAC, which this does not parse as ADTS and correctly
declines to split — gets the one-packet-per-PES behaviour this always had.
See *Audio re-framing* below for why this lives here rather than behind
`ParserProvider`, and for the one ordering divergence it did not close.

Every packet this function produces also carries a `PacketSideData::
MpegtsStreamId` — the PES `stream_id` byte itself (`0xe0` for the first video
stream, `0xc0` for the first audio stream, ITU-T H.222.0 Table 2-22) —
matching the reference's own `MPEGTS Stream ID` side-data block on every
packet, measured against `ffprobe 8.1`.

### The clock

`time_base` is fixed at 1/90000 by the format. Wrap state is a
`vaco_format_core::WrapState` **per program, not per stream** (R7): a multiplex
shares one clock, and correcting video while leaving audio uncorrected
desynchronises them permanently.

`WrapState::correct` is used as `vaco-format-core` shaped it — the pivot
applies to the first value only and everything after is delta tracking in raw
space. Plan 18's model applies both the pivot and a cumulative offset, which
double-counts and makes the stream sawtooth; that correction is
`vaco-format-core`'s and this crate simply inherits it. A named test walks a
synthetic file across `2^33` and asserts every inter-packet delta is exactly one
frame.

A packet with PTS and no DTS feeds its PTS through the DTS tracker as well, so
a stream that starts PTS-only and later gains DTS does not jump.

After a seek, `WrapState::resync` recomputes the offset from the seek target
and the first raw value seen (R10) — the rule that stops a seek into the second
half of a thirty-hour recording reporting timestamps 26.5 hours in the past.

### Duration — the part with no container field at all

`read_header` does two bounded scans and then rewinds:

1. **Head scan**, bounded by `min(probesize, 5 MiB)`, stopping as soon as every
   PMT the PAT names has arrived and every stream has shown a first PTS. This
   sets `Stream::start_time`.
2. **Tail scan** (R15), seeking back `250_000 << retry` bytes for `retry` in
   `0..=6` and reading to end of input, stopping early once every stream has a
   last PTS.

**The read-back cap is measured, and plan 18 R15 is wrong about it.** The plan
says "retry from a position twice as far back, up to three times". Padding a
`.ts` file with trailing null packets and asking ffprobe for its duration:

| trailing padding | reference duration |
|---|---|
| 9.4 MB | correct |
| 15 MiB | correct |
| 17 MiB | `N/A` |

Narrowed to a packet: the boundary sits at exactly **16,000,000 bytes** of
read-back, which is `250_000 << 6` — seven attempts, not four.

Then, per stream, `end = last_pts + tail`, and:

```
stream.duration    = end - stream.start_time
container.duration = max(end over streams) - min(start over streams)
```

Both formulas are measured against the reference, and the second is the
non-obvious one: it is *not* the maximum per-stream duration. On the fixture
below the longest stream lasts 3.000000 s and the container reports 3.023222,
because the container spans from the earliest start to the latest end and the
two streams do not start together.

`tail` is where a demuxer with no parser cannot quite reach. The reference uses
`last_packet.pts + last_packet.duration`, and that duration comes from the
codec — the frame rate for video, `frame_size / sample_rate` for audio — which
`find_stream_info` establishes. For **video** the smallest observed
inter-packet PTS delta reproduces it exactly, because one video PES packet is
one access unit and the *smallest positive* delta survives B-frame reordering.
For **audio** nothing here can: a PES packet holds a dozen frames, so the
smallest PES-to-PES gap is a dozen frame durations. The last audio frame's own
duration is therefore left out and the per-stream audio duration is short by
exactly one audio frame. Measured: 23.211 ms on every AAC fixture, which is
`1024/44100`.

### `mpegtsraw` — the PID-level view

A second, real demuxer (`raw.rs`), confirmed distinct from `mpegts` by
`ffmpeg -demuxers` rather than assumed. Everything about its contract was
measured against `ffprobe 8.1 -f mpegtsraw`, not derived:

* **Never auto-detected.** `ffprobe -i file.ts` (no `-f`) reports
  `format_name=mpegts`; `mpegtsraw` is reached only by naming it. Its probe is
  `ProbeScore::NONE`, unconditionally — the same shape `vaco-demux-asf`'s
  `asf_o` already uses for exactly this "same bytes, explicit-only" case.
* **Exactly one stream**, `MediaType::Data`, `time_base = 1/27_000_000` — the
  27 MHz PCR clock, not the 90 kHz PES clock `mpegts` uses.
* **One `Packet` per transport packet**, always 188 bytes: an M2TS-strided
  file's 4-byte `TP_extra_header` is stripped, measured by muxing an
  `-mpegts_m2ts_mode 1` fixture and reading `size` back with `-show_packets`.
* **`pos` is the offset *after* the packet**, including any stride prefix —
  `192, 384, 576, …` on the M2TS fixture, not `188, 376, …`. Every packet
  carries `flags=K__` (`PacketFlags::KEY`), matching the reference exactly.
* **No timestamps by default.** `-show_packets` on a 105-packet fixture shows
  `pts=N/A` on all of them. The reference's `-compute_pcr` option (default
  `false`, per `ffmpeg -h demuxer=mpegtsraw`) turns this on, but even then the
  values are a byte-position-interpolated PCR — not monotonic across PIDs, and
  not implemented here (see *Deliberately deferred*). `FormatFlags::NOTIMESTAMPS`
  is the honest declaration.
* **Resynchronisation is bounded by 65536 bytes** (`ffmpeg -h demuxer=mpegtsraw`'s
  `resync_size` default), reused as a fixed constant since `FormatOptions` has
  no per-demuxer option slot to carry it through.
* **Duration is `None`.** The reference derives an estimate from file size and
  bitrate (`"Estimating duration from bitrate, this may be inaccurate"`), which
  needs a bitrate this no-PES view has no way to learn — that lives one layer
  up, in `mpegts`. Reported rather than guessed at.
* **`codec_name` prints `unknown`, not `mpegts`.** `vaco-probe`'s `codec_name`
  field reads `CodecParameters.codec_id` only, with no metadata-tag fallback —
  confirmed by reading `crates/app/vaco-probe/src/show.rs` and
  `fields.rs`, and consistent with how every other `MediaType::Data` stream in
  this workspace (ASF, Ogg, Matroska's unmapped track types) already prints.
  `vaco_codec_core::CodecId` has no "raw bytes" variant to point at; inventing
  one would be worse than the honest gap. Same shape as `TsCodec::codec_id()`
  returning `None` elsewhere in this crate — reported, not worked around.

### PMT version changes

`ffmpeg -h demuxer=mpegts` names the option directly: `-merge_pmt_versions
<boolean>` — *"reuse streams when PMT's version/pids change"* — **default
`false`**. That is a measured correction from inference to fact for what this
doc already suspected: the reference's *default* behaviour is **not** to merge
a version change into the existing stream set, but to create fresh stream
entries for it, which is why a long recording of a re-multiplexing channel
ends up with a dozen streams over time in the reference. This crate always
merges (a PID already carrying a stream keeps it, permanently) and does not
implement the option — see *Deliberately deferred* — because doing the
default's thing needs the PID-to-many-streams mapping this crate does not
have anywhere else either (teletext, subtitling: same gap, different
descriptor).

What *is* implemented: `DemuxStats::pmt_updates` counts a genuine
`version_number` change on an already-known program, so a caller can at least
observe that a splice happened instead of inferring it from a stream list that
never shrinks. The counter is tracked separately from the public
`Program::pmt_version` field and only advances once `read_header`'s internal
scans are done (`!self.scanning`): both the head scan and the duration tail
scan re-read PSI the caller never sees a `Packet` for, and on a file small
enough to fit inside either scan's window, comparing directly against
`pmt_version` double-counts the same live change, or invents one purely
because a scan rewound to the start ahead of where the real read is. A named
test pins both — the version bump is counted exactly once, and the PID it
introduces is picked up — and a repeated identical section (the ordinary case;
a PMT repeats roughly every 100 ms) is confirmed not to count at all.

### Seeking

Three paths, and one of them deliberately bypasses `vaco-format-core`:

* **Index** — once packets have gone past, `PacketIndex` holds every keyframe
  seen, including those the bisection itself probed.
* **Bisection** — `seek::binary_search` with a probe closure that scans forward
  from a byte position for the next `payload_unit_start` packet carrying a
  timestamp.
* **Byte** — `SeekTarget::Byte`, resynchronised to the next PES header.

**`SeekStrategy::choose` is not used, and that is a reported gap, not an
oversight.** It returns `Byte` for any format declaring `TS_DISCONT`, because
`FormatFlags` conflates "timestamps may jump" with "byte position and time are
unrelated". MPEG-TS needs the first and not the second: a recording is
overwhelmingly monotonic and the reference bisects it. Calling `binary_search`
directly is the minimum divergence that gets the right behaviour without
touching a frozen interface.

---

## How to change it

* **The order in `read_header` matters.** `Stream::start_time` is set from the
  head scan *before* the tail scan runs, because the tail scan resets the scan
  state — and it must, or the head scan's end timestamps satisfy the retry
  condition and the reported duration is the length of the probe window. That
  was a real bug, caught by the trailing-null-packet test.
* **`Eof` must stay sticky.** `read_packet` consumes bytes before it can tell
  whether a packet follows.
* **A new descriptor that splits streams** (teletext, subtitling) needs a
  PID-to-many-streams mapping this crate does not have. See the gap list.
* **Do not set `NOBINSEARCH`.** It would make `FormatFlags::allows_binary_search`
  false and remove the only accurate seek this container can have.
* **Gotcha — `pump()` is used by three callers** (`read_packet`, the head scan,
  the tail scan) and the `scanning` flag is what stops the last two allocating
  a `Packet` per PES packet they are about to throw away. A tail scan reads up
  to 16 MB; without the flag, opening a file allocates its way through all of
  it.
* **Gotcha — the program clock index is not the program index.** Clock zero is
  the synthetic one for streams belonging to no program. `program_slot` creates
  both together for exactly this reason; a drift between them corrects one
  stream's wrap and not another's.

---

## Configuration

`FormatOptions` fields honoured: `probesize`, `duration_probesize`,
`skip_estimate_duration_from_pts`, `max_streams`, `correct_ts_overflow`,
`indexmem` (through `PacketIndex::with_options`), `seek2any` and `fflags`
(through the core's helpers).

Constants that are ours:

| Constant | Value | Basis |
|---|---|---|
| `MAX_PES_BYTES` | 6 MiB | Exceeds any real access unit; a `PES_packet_length` of zero has no other bound |
| `MAX_PSI_PIDS` | 64 | Each assembler costs a fixed 4 KiB |
| `MAX_RESYNC_BYTES` | 1 MiB | Bounds the byte-at-a-time sync scan |
| `MAX_HEADER_SCAN` | 5 MiB | So an input of only null packets terminates |
| `DURATION_READ_BACK` | 250 000 | **Measured** against ffprobe 8.1 |
| `DURATION_MAX_RETRY` | 6 | **Measured**; the cap is 16 000 000 bytes |
| `TS_SCORE_STRONG` / `TS_SCORE_WEAK` | 50 / 2 | **Measured**; `ProbeScore`'s table has no value for either |
| `raw::RESYNC_SIZE` | 65 536 | **Measured** (`ffmpeg -h demuxer=mpegtsraw`'s `resync_size` default); `mpegtsraw` only |

### What opening costs

Every scan is bounded and linear. The worst case is a file that declares a
stream which never carries a packet: the tail-scan retry loop then runs all
seven attempts to end of input, reading about `2 × 16 MB` in total whatever the
file's size. Measured at **1.3 s for a 21 MB pathological file** in the dev
profile, and it is `O(size)` rather than `O(size × retries)` because each
retry's read-back doubles. `mpegts_demux` fuzzed for three minutes at 200 KB
inputs produced no `slow-unit` artifact.

---

## Dependencies

`vaco-core`, `vaco-io`, `vaco-limits`, `vaco-packet`, `vaco-format-core`,
`vaco-format-mpegts-tables`, `vaco-codec-core`. `proptest` as a
dev-dependency. No external media crate, and no codec crate — parsers arrive
through `ParserProvider`.

---

## Measured fidelity against ffprobe 8.1

Five files muxed by `ffmpeg -f mpegts`, from 3 to 60 seconds, one H.264 video
and one AAC audio stream each.

| Field | Result |
|---|---|
| `probe_score` | **exact** (50) |
| `format.duration` | **exact** on all five, to the microsecond |
| `format.bit_rate` | **exact** on all five (`size * 8 * 1e6 / duration_us`, floored) |
| `format.start_time` | **exact** (minimum stream start) |
| stream `id` (PID), `codec_tag` | **exact**, including the registration-identifier case |
| video `start_pts`, `duration_ts` | **exact** — see *The `min_delta` bug* below |
| video packet count, keyframe count, `pos` | **exact** |
| audio `start_pts` | **exact** |
| program `program_num`, `pmt_pid`, `pcr_pid`, service tags | **exact**, and in the right section since the `Program` fields landed |
| audio `duration_ts` | **short by one audio frame** (23.211 ms, every file) — unaffected by the re-framing fix below; see `end_pts`'s own note |
| audio packet count | **exact — 131 against 131, fixed 2026-08-23**; see below |

`tests/reference.rs` is the harness that produced these; it is `#[ignore]`d and
takes a file through `VACO_TS_FIXTURE`, so the numbers can be re-measured
against a newer reference rather than trusted.

### The `min_delta` bug: a frame duration that was a whole GOP

`end_pts` reports `last_pts + min_delta` for video, where `min_delta` is
documented as "the smallest positive PTS increment — for video this *is* the
frame duration, and it survives B-frame reordering, where consecutive deltas
alternate but the smallest positive one is still one frame".

The code measured the increment against `last_pts`, which is the running
**maximum**, not the previous packet. Under reordering those are different
numbers on most packets, and the smallest positive jump *above the running
maximum* is a whole GOP. On a 25 fps two-B-frame file it measured 14 400 ticks
where the frame is 3 600:

```
PTS in file order: 133200 147600 140400 136800 144000 162000 …
|consecutive deltas|:    14400   7200   3600   7200  18000 …    -> 3600
deltas above the max:    14400      –      –      –  14400 …    -> 14400
```

`duration_ts` came out three frames long — 100 800 against the reference's
90 000. Fixed by keeping `prev_pts` alongside `last_pts` and taking the
smallest non-zero |difference| between consecutive packets. Re-measured on four
files (1 s, 3 s, 7 s, 20 s): video `duration_ts` is now exact on all four,
where before it was exact on the two longest only. The implementation had
drifted from its own doc comment, and only a short fixture showed it — a long
one has a GOP jump small enough to coincide with the frame duration often
enough to hide the difference.

### Audio re-framing — fixed 2026-08-23 (issue #632)

On the three-second fixture the reference emits **131 audio packets** from
**10 PES packets**, each 2089 ticks long with `pos` set only on the first of
each group. It is splitting each PES payload into AAC frames.

The earlier text here concluded this needed a `vaco-format-core`/
`ParserProvider` change and would put codec knowledge in a container crate,
violating D14.1. That conclusion does not survive contact with what an ADTS
frame boundary actually is: `aac_frame_length` is a fixed-position 13-bit
field in a 7- or 9-byte header (ISO/IEC 13818-7 Annex B), not something that
needs a bitstream *parser* to find — the same sense in which Matroska's own
lacing already splits one `Block` into several packets inside that demuxer,
with no codec crate involved. `flush_pes` now parses ADTS headers directly
(`parse_adts_header`/`split_adts`) and emits one `Packet` per frame whenever
`CodecId::Aac` is the stream's codec; every other codec — including
`CodecId::AacLatm`, which is not ADTS-framed and does not parse as one —
keeps the original one-packet-per-PES behaviour untouched. `pos` is set only
on the frame that opens the PES, `None` on every frame synthesised from it,
matching the reference exactly. Verified on the doc's own three-second
fixture: 131 audio packets against 131, 206 total against 206.

Per-frame timestamps accumulate from the PES's own header PTS using
`round(samples_so_far * 90000 / sample_rate)`, which matches the reference
for the overwhelming majority of frames in a PES and is measured to be within
**one tick (≤ 11 µs) of it** on a handful of others — the reference's own
carry/rounding rule was not fully root-caused within a black-box budget (D7
forbids reading its source) and is recorded here rather than hidden.

Video is unaffected — one video PES packet is one access unit — which is why
every video number above is exact.

**A related divergence remains, characterised but not fixed: MPEG-TS packet
ordering across streams still disagrees with the reference on files that mix
audio and video, one packet at a time, every time a video access unit's own
PES (`PES_packet_length == 0`) sits between two runs of the other stream's
packets.** Concretely, on a real `av.ts` (25 fps H.264 + 44.1 kHz AAC): TS
packet trace confirms this crate's `flush_before` fires exactly when the
*next* same-PID `payload_unit_start` arrives, which is the documented and
spec-correct trigger — and does so identically to the reference for the two
video packets immediately before an audio block starts. But the reference
defers releasing the *first* of those two video packets until **after** the
entire following audio PES has been read and returned, while this crate
releases it as soon as its own trigger condition is met, one packet early.
The order re-synchronises a few packets later and does not compound — it is a
bounded, repeating one-packet swap, not a growing divergence — but it is real:
on a 1 s `av.ts`, 22/70 packets land in a different position than the
reference because of it, though every packet's own field values are correct
once matched to its true reference counterpart. Reproduction: mux 25 fps
H.264 + 44.1 kHz AAC to `-f mpegts`, compare `-show_packets` position-by-
position; the swap recurs at every audio-PES boundary in the file.

---

## Signature gaps

Interfaces are frozen (plan 19 §6), so these are **reported, not changed**,
in descending order of cost.

1. **Packet ordering across streams still disagrees with the reference by one
   packet at a time, at every audio/video interleave boundary.** The
   remaining piece of the former "no packet-reframing layer" gap — AAC
   re-framing itself is fixed; see *Audio re-framing* above for the exact
   reproduction and what is and is not understood about it.
2. **`Discovery` snapshots the stream list at construction.** `Discovery::new`
   does `inner.streams().to_vec()` and never re-reads, so a stream the demuxer
   discovers *during* the pass never appears. That is the progressive-discovery
   case MPEG-TS makes ordinary — a PMT arriving after the first packets is
   normal, not pathological. Worked around by doing the PSI scan inside
   `open()`, which covers every stream the PAT names; a genuinely late stream
   still will not show.
3. **`Discovery::duration` prefers its own forward-scan `from_pts` over the
   demuxer's answer.** `estimate_duration` takes `FromPts` before `FromStream`
   unless `authoritative` or `skip_estimate_duration_from_pts` is set, and
   `Discovery` fills `from_pts` from the *probe window*, not from a tail scan.
   For a ten-second file with a five-second analyse window that reports about
   five seconds. Since R15's tail scan exists precisely for MPEG-TS, and
   `DurationInputs::authoritative` is documented as "MPEG-TS never sets this",
   there is no correct value this crate can pass. `MpegTsDemuxer::duration()`
   is exact; a caller that wraps it in `Discovery` gets the probe window
   instead. **This needs a `DurationInputs` field distinguishing "scanned the
   tail" from "scanned the prefix".**
4. **`FormatFlags::TS_DISCONT` conflates two properties.** It correctly
   suppresses the monotonic-DTS repair and incorrectly disables bisection.
   Handled by calling `binary_search` directly; a separate `NOBINSEARCH`-style
   bit, or splitting the flag, would let `SeekStrategy::choose` be used as
   intended.
5. **`Program` had no `pmt_pid`, `pcr_pid` or `pmt_version` fields — closed
   2026-08-22.** All four (`program_num` too) are `Program` fields now and the
   demuxer sets them there; they used to travel as metadata entries, which is
   why the `[PROGRAM]` section printed them as `TAG:pmt_pid=…` — the right
   values in the wrong section.

   One correction to the request: `pmt_version` is **not** printed by
   `ffprobe 8.1`. Measured with
   `-of flat -show_optional_fields always -show_programs`, which shows every
   field a section defines including the unavailable ones, the section is
   `program_id`, `program_num`, `nb_streams`, `pmt_pid`, `pcr_pid` and the tags.
   Plan 18 §1.1 says otherwise and is wrong. The field is kept because this
   demuxer needs it to notice a PMT change, not because anything prints it.
6. **`ProbeScore`'s convention table has no value for a self-synchronising
   container.** `repeating(n)` steps over 50, which is what MPEG-TS actually
   scores, and there is no constant for the reference's low-confidence 2.
7. **`DemuxerDesc::open` takes no options and no `Limits`**, so
   `MpegTsDemuxer::open_with_limits` exists as a second, non-`dyn`
   constructor — the same workaround `vacoraw` needed.
8. **`Stream` has no `pts_wrap_bits`**, so the demuxer holds the `WrapState`.
   Correct here anyway, since the state is per *program*.
9. **One teletext or subtitling descriptor can declare several logical subtitle
   streams on one PID**, and nothing in the model maps one PID to many streams.
   The count is recorded as `teletext_pages` / `subtitle_streams` metadata so
   the gap is visible rather than silent. This is why a single teletext PID
   should produce five subtitle streams and here produces one.

### Wanted from other crates

* **`vaco-codec-core`: `CodecId` variants for what MPEG-TS carries.** MPEG-2
  video, mp1/mp2, AC-3, E-AC-3, DTS, TrueHD, VC-1, DVB subtitle, DVB teletext,
  SCTE-35, timed ID3. See `vaco-format-mpegts-tables`' doc file. Until then
  those streams are reported with the right media type, PID and language but
  `codec_id = None` and a `ts_codec` metadata tag.
* **`vaco-codec-core`: `AudioParameters` has no `frame_size`.** With it, the
  audio `duration_ts` short-by-one-frame divergence noted in the fidelity
  table above (unaffected by the ADTS re-framing fix, which is a packet-count
  and per-packet-timing fix, not a stream-duration one) could be closed
  through `ParserProvider` without the container learning anything about AAC.

---

## Deliberately deferred

* **`mpegtsraw`'s `-compute_pcr`.** The default (`false`) behaviour — no
  timestamps at all — is what `raw.rs` implements. The reference's
  `compute_pcr=true` mode interpolates an "exact PCR" per packet from byte
  position and the nearest two real PCR occurrences on that PID; it is not
  monotonic across PIDs (measured: consecutive packets from different streams
  can report *decreasing* `pts`), and `FormatOptions` has no per-demuxer
  option slot to switch it on through anyway.
* **M2TS arrival timestamps.** The 192-byte stride is detected and demuxed;
  the four-byte `TP_extra_header`'s 27 MHz arrival timestamp is skipped rather
  than exposed, because `PacketSideData` has no variant for it and plan 18
  §3.3.3 item 14 says it is "used for nothing else".
* **`merge_pmt_versions` / `skip_changes` / `skip_clear`.** A PID already
  carrying a stream keeps it; a PMT version change does not currently create new
  streams. The reference's *default* (`merge_pmt_versions=false`, measured via
  `ffmpeg -h demuxer=mpegts`) does create them, which is why a long recording
  of a re-multiplexing channel ends up with a dozen streams. Matching that
  needs the option set, and the option set needs the PID-to-many-streams
  mapping above. `DemuxStats::pmt_updates` at least makes the change visible;
  see *PMT version changes* above.
* **NIT, EIT, TDT/TOT.** Framed and CRC-checked by the tables crate, not acted
  on. Only SDT changes printed output today.
* **CAT and descrambling.** The CAT is parsed and discarded. Scrambled packets
  are counted, flagged and never framed; the stream is still reported, which is
  what a user needs to see.
* **`fix_teletext_pts`, `compute_pcr`, CBR position derivation.** All need
  streams this crate does not yet split or a bitrate model it does not build.

---

## Testing

* **62 tests**: 30 unit (9 of them `raw`'s), 25 named integration cases, 6
  property tests, 1 doctest, plus the `#[ignore]`d reference harness.
* **Every fixture is built in-process** by `tests/roundtrip.rs`'s `TsWriter`.
  A committed `.ts` file would be larger and less specific: the whole
  difficulty of this container is in cases — a wrap, a mid-stream PMT, an
  unbounded PES packet, a lost packet, a 192-byte stride — that a recorded file
  happens to contain or happens not to.
* **The wrap invariant is a property, not just a named case.**
  `wrapping_across_the_thirty_three_bit_boundary_stays_monotonic`
  (`tests/roundtrip.rs`) fuzzes the frame delta, the run length and how far
  before the boundary the run starts, and asserts every consecutive decoded
  PTS difference equals the fixed delta — i.e. the crossing is invisible in
  the decoded timeline, whatever the parameters. The hand-written
  `a_thirty_three_bit_wrap_stays_monotonic` above it now pins one instance by
  name for a quick, readable regression case; the property is what actually
  covers the invariant D6 asks for.
* **Three fuzz targets** (D6): `mpegts_packet` for the stateless views,
  `mpegts_demux` for whole-file `mpegts` demux (opens with `Limits::strict`,
  reads to the end, asserts `Eof` is stable, then seeks three ways), and
  `mpegts_raw_demux` for `mpegtsraw` — its own target because it is a
  different open path, a different resync bound (`RESYNC_SIZE`, not
  `MAX_RESYNC_BYTES`) and no PSI/PES layer at all, so `mpegts_demux`'s coverage
  does not reach it.
