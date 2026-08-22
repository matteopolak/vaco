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
| audio `duration_ts` | **short by one audio frame** (23.211 ms, every file) |
| audio packet count | **10 against 131** — the reference re-frames; see below |

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

### The largest divergence: the reference re-frames audio and we do not

On the three-second fixture the reference emits **131 audio packets** from
**10 PES packets**, each 2089 ticks long with `pos` set only on the first of
each group. It is splitting each PES payload into AAC frames.

That splitting happens *inside* the reference's demux layer, driven by a
per-stream "needs parsing" flag that MPEG-TS sets for essentially every stream.
Our architecture has no equivalent: `ParserProvider` supplies parsers, and
`Discovery::refine` drives them to fill in `CodecParameters`, but nothing
re-frames a packet. Without it, `-show_packets` on a TS file cannot match for
audio, and the audio duration is short by the last frame for the same reason.

Video is unaffected — one video PES packet is one access unit — which is why
every video number above is exact.

**This is a `vaco-format-core` gap, reported and not worked around.** Doing it
here would put codec knowledge in a container crate, which D14.1 exists to
prevent.

---

## Signature gaps

Interfaces are frozen (plan 19 §6), so these are **reported, not changed**,
in descending order of cost.

1. **There is no packet-reframing layer.** The largest gap; see above.
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
  audio duration divergence above could be closed through `ParserProvider`
  without the container learning anything about AAC.

---

## Deliberately deferred

* **`mpegtsraw`** — the PID-level view that exposes raw 188-byte packets as one
  data stream and skips the PES layer. A second `DemuxerDesc` over the same
  framing; nothing in it is hard, and nothing depends on it yet.
* **M2TS arrival timestamps.** The 192-byte stride is detected and demuxed;
  the four-byte `TP_extra_header`'s 27 MHz arrival timestamp is skipped rather
  than exposed, because `PacketSideData` has no variant for it and plan 18
  §3.3.3 item 14 says it is "used for nothing else".
* **`merge_pmt_versions` / `skip_changes` / `skip_clear`.** A PID already
  carrying a stream keeps it; a PMT version change does not currently create new
  streams. The reference's default *does* create them, which is why a long
  recording of a re-multiplexing channel ends up with a dozen streams. Matching
  that needs the option set, and the option set needs the PID-to-many-streams
  mapping above.
* **NIT, EIT, TDT/TOT.** Framed and CRC-checked by the tables crate, not acted
  on. Only SDT changes printed output today.
* **CAT and descrambling.** The CAT is parsed and discarded. Scrambled packets
  are counted, flagged and never framed; the stream is still reported, which is
  what a user needs to see.
* **`fix_teletext_pts`, `compute_pcr`, CBR position derivation.** All need
  streams this crate does not yet split or a bitrate model it does not build.

---

## Testing

* **51 tests**: 21 unit, 24 named integration cases, 5 property tests, 1
  doctest, plus the `#[ignore]`d reference harness.
* **Every fixture is built in-process** by `tests/roundtrip.rs`'s `TsWriter`.
  A committed `.ts` file would be larger and less specific: the whole
  difficulty of this container is in cases — a wrap, a mid-stream PMT, an
  unbounded PES packet, a lost packet, a 192-byte stride — that a recorded file
  happens to contain or happens not to.
* **Two fuzz targets** (D6): `mpegts_packet` for the stateless views and
  `mpegts_demux` for whole-file demux, which opens with `Limits::strict`,
  reads to the end, asserts `Eof` is stable, and then seeks three ways.
