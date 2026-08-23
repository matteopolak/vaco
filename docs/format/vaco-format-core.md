# `vaco-format-core`

Layer 3b. The container framework: what a demuxer and a muxer *are*, and the
five models they all share — probing, stream discovery, timestamps, seeking and
interleaving.

Per D14.1 this crate sits **above** `vaco-codec-core`, because `Stream` carries
`CodecParameters`. It reaches bitstream parsers through an injected
`ParserProvider`, so **no format crate ever depends on a codec crate**.

---

## What it is

| Module | Contents |
|---|---|
| `probe` | `ProbeData`, `ProbeScore`, and the score-based detection engine |
| `options` | `FormatOptions` — the generic format-level option table |
| `flags` | `FormatFlags` — what a container declares it can do |
| `time` | wraparound, timestamp generation and repair, duration estimation |
| `seek` | `SeekTarget`, `PacketIndex`, and the two generic seek strategies |
| `discovery` | `Discovery<D>` — the bounded, replayable stream-discovery pass |
| `interleave` | `InterleaveQueue` and the muxer-side timestamp chain |
| `metadata` | `MuxMetadata` — file/stream tags, chapters, attachments for a muxer |
| `vacoraw` | a worked-example container that drives every one of the above |

### The one idea worth reading first

**The core does not own the demuxer.** `Demuxer` is a self-contained object: it
holds its own I/O, reads its own packets, performs its own seeks. Everything
generic in this crate is therefore either

* a **library the demuxer calls** — `SeekStrategy::choose`, `binary_search`,
  `PacketIndex`, `TimestampFixer`, `WrapState`; or
* a **wrapper the caller composes** — `Discovery<D>`, which is itself a
  `Demuxer`.

It is never a driver that reaches into a demuxer through callbacks.

That is the opposite of `planning/18-formats.md` §1.2, which sketched a
`DemuxCtx` owning the I/O with the demuxer as a set of callbacks. The frozen
trait forced the inversion, and the inversion is better: `Discovery` can be
applied or not applied, tested against a mock, and stacked, and no demuxer has
to know it exists. The cost is real and is listed under *Signature gaps* below.

---

## The design justification: MP4, Matroska and MPEG-TS

The brief for this crate asked for exactly one thing above all others — that the
traits be implementable by all three of MP4, Matroska and MPEG-TS without
contortions, because those three cover the design space: index-based random
access, cue-based with the cues sometimes missing, and streaming with no index
at all. This section is that analysis. It is the justification for the shape of
`seek`, `probe` and `time`, and it is what to re-read before changing any of
them.

### MP4 / MOV — a complete index, built once

*Probe.* `ProbeScore::MAGIC_CHECKED` on `ftyp` at offset 4 with a recognised
brand; `ProbeScore::MAGIC` for a bare `moov`/`mdat` at offset 4 without one.
`ProbeData`'s zero-padding matters here: on a file shorter than 12 bytes the
brand read must yield zeros rather than a short read, or the score differs from
the reference's.

*Open.* Walk the box tree; build one `Stream` per `trak`; `mdhd.timescale`
becomes `Stream::time_base`; `tkhd.duration` and `mdhd.duration` become
`Stream::duration`; the edit list gives `Stream::start_time` authoritatively, so
discovery must not overwrite it (and `TimestampFixer` does not — it only fills
in what is absent).

*Index.* `stts` + `ctts` + `stss` + `stsc` + `stco`/`co64` + `stsz` expand into
one `PacketIndex` per stream at `read_header`. `PacketIndex::add` updates rather
than duplicates on an equal timestamp, which is what makes it safe to feed the
same sample twice from two tables. `indexmem` decimation applies to very long
files; for a fragmented file the `sidx` supplies a sparser index and
`fflags +fastseek` is the switch that says "use `sidx`, do not walk the `moof`s".

*Seek.* `SeekStrategy::choose` returns `Index` and `PacketIndex::search` does
the work. MP4 declares neither `TS_DISCONT` nor `NOBINSEARCH`, so a file whose
`stbl` was discarded by `fflags +ignidx` falls through to `binary_search`
cleanly.

*What is missing.* Nothing structural. `Stream` has no `attached_pic` field, so
MP4 cover art (`covr`) has to be surfaced as an `ATTACHED_PIC` stream whose
single packet arrives through `read_packet` — workable, and slightly different
from the reference, which pre-loads it. Listed under *Signature gaps*.

### Matroska / WebM — cues, when there are any

*Probe.* `ProbeScore::MAGIC_CHECKED` on the EBML magic plus a `DocType` of
`matroska` or `webm`; `ProbeScore::MAGIC` on the magic alone. Confirmed
empirically that a corrupted magic scores zero even when the transport supplies
`Content-Type: video/x-matroska`, so the MIME bonus must not rescue it — see
*Probing* below.

*Open.* `Info/TimestampScale` gives every track the same time base (usually
1/1000, i.e. milliseconds); `Info/Duration` is a **float**, which determinism
rule DD3 says must be converted to a rational exactly once and never
accumulated. `CodecDelay` and `DiscardPadding` land on `AudioParameters` and on
packet side data respectively.

*Index.* `Cues` populate `PacketIndex` — sparse, keyframe-only, and *frequently
absent* in files written by a live encoder or truncated mid-write.

*Seek.* This is the case that decided the module's shape. With cues,
`SeekStrategy::choose` returns `Index`. Without them it returns `BinarySearch`,
and the demuxer supplies a probe closure that scans forward for a Cluster ID and
reads its `Timestamp` element. `binary_search` populates the index as it
bisects, so the second seek into a cue-less file is cheap — which is exactly the
behaviour a scrubbing UI needs and is why the bisection returns entries at all
rather than just a position.

*The unknown-size cluster.* Matroska permits a cluster whose size is the
all-ones VINT, meaning "until the next cluster". A forward scan for the next
Cluster ID handles it, and it is the demuxer's business; nothing here assumes a
container can state its own element sizes.

*What is missing.* Nothing structural. Lacing means one Block yields several
packets, which `Packet::sub_packet` covers at the cost of a copy — a known
`vaco-packet` limitation recorded in its own docs, not this crate's.

### MPEG-TS — no index, and timestamps that legitimately jump

*Probe.* The `ProbeScore::repeating(n)` row of the convention table: count
consecutive `0x47` sync bytes at the 188/192/204-byte strides. Measured against
the reference, an 18 KB TS file scores **50** and a 20-second one also scores
50, so the reference's TS probe saturates well below `MAX` — a calibration fact
worth having before `vaco-demux-mpegts` is written, and the reason
`repeating(n)` tops out where it does rather than reaching 100 on any real file.

*Open.* PAT and PMT give `Program`s and their `Stream`s. Time base is fixed at
1/90000. `Stream::id` is the PID, and `FormatFlags::SHOW_IDS` is what makes
`vaco-probe` print it.

*Timestamps.* The hard case, and the one `time` is shaped by:

* PTS/DTS are **33 bits**, so `WrapState::new(33)` applies and a 26.5-hour
  recording crosses the wrap. The state is **per program, not per stream**
  (R7) — correcting video and leaving audio uncorrected desynchronises them
  permanently, and a multiplex shares one clock.
* A `discontinuity_indicator` in the adaptation field is a *legitimate* jump.
  MPEG-TS therefore declares `FormatFlags::TS_DISCONT`, which suppresses R22's
  monotonic-DTS repair entirely. That split — the format layer never repairs a
  declared discontinuity, and the CLI decides what to do about it — is the
  single most important boundary rule in the model.
* `TS_DISCONT` also disables `binary_search`, because bisection assumes
  timestamps increase with byte position and the flag is the declaration that
  they do not. `SeekStrategy::choose` encodes exactly that: with `TS_DISCONT`
  and no index, it returns `Byte`, and the demuxer resynchronises on the sync
  byte at the packet stride.

*Seek.* Three paths, all reachable: `Index` once packets have gone past and the
`GENERIC_INDEX` flag let the core record them; `BinarySearch` for a
well-behaved recording without discontinuities; `Byte` plus resync otherwise.

*What is missing.* `Stream` has no `pts_wrap_bits` field, so the demuxer holds
the `WrapState` itself rather than the core deriving it. That is a downgrade
from the plan and it is listed under *Signature gaps* — but it is not a blocker,
because the wrap state is per *program* anyway and `Program` could never have
carried it as a per-stream field.

### What the three together prove

* **Index, bisection and byte-plus-resync are all needed**, and no container
  needs a fourth. `SeekStrategy` has exactly four variants and each is reachable.
* **The index must be usable when partially populated**, because Matroska
  without cues and MPEG-TS both build theirs incrementally. `PacketIndex::search`
  returning `None` is a fact the caller acts on, never an error.
* **Timestamp repair must be suppressible per format**, or MPEG-TS is
  unreadable. Hence `TS_DISCONT` and hence the boundary rule.
* **Probing must tolerate a short prefix**, because a TS probe wants 188·N bytes
  and an MP4 probe wants 12. Hence the retry loop and the padding window.
* Every one of these is exercised end to end by `vacoraw`, which implements the
  index path, the bisection path and the byte path in one 700-line format.

---

## How it works

### Probing

Score-based, bounded, and total. Each registered `DemuxerDesc` gets a prefix and
returns a `ProbeScore` in `0..=100`; the highest wins; zero never wins.

Two rules that `planning/18-formats.md` marked as needing verification were
**measured against the pinned reference (ffmpeg/ffprobe 8.1)** rather than
guessed, and one of the two contradicts the plan:

| Question | Plan's guess | Measured | Where |
|---|---|---|---|
| `probe_score` for `-f <name>` | `MAX` (100) | **0** | `ffprobe -f matroska a.mkv` |
| Does a matching MIME rescue a zero content score? | open (VERIFY-P1) | **No** | HTTP `Content-Type: video/x-matroska` over a file with corrupted EBML magic — detection fails |

Calibration data from the same reference, useful when writing a real probe:
MP4 100, Matroska 100, WAV 99, raw H.264 51, MPEG-TS 50. The score space is
genuinely used across its whole range, which is why `ProbeScore` publishes a
convention table (`MAGIC_CHECKED`, `MAGIC`, `VARIABLE_OFFSET`, `repeating(n)`,
`EXTENSION`, `weak(n)`) rather than three constants. `formatprobesize`'s
documented default of `1048576` fixes `PROBE_BUF_MAX`.

`ProbeData` reproduces the reference's 32 zero bytes past the end of the buffer.
Not for safety — nothing here can read out of range — but for **fidelity**: on a
six-byte file, a probe reading a sixteen-byte header sees ten zeros upstream and
would see a short read here, giving a different score, a different chosen format
and a different `probe_score` line.

`Probe::detect` runs the retry loop against a live `IoContext` using `peek`, so
the source's position is unchanged on return whatever the outcome. That is what
makes detection work on a pipe and why a failed detection needs no undoing.

### Stream discovery

`Discovery<D>` wraps a demuxer, reads a bounded prefix, refines what it can, and
replays every packet it consumed. Five termination conditions, checked in a
fixed order, each reported as a `StopReason`: `Complete`, `ProbeSize`,
`AnalyzeDuration`, `PacketCap`, `NoStreams`, plus `Eof`, `NoProgress` and
`Error`. `StopReason` is the single most useful diagnostic when a reported field
comes out wrong — "the loop stopped at `ProbeSize`" explains a missing profile
far better than the missing profile does.

Determinism rules, all because D6 requires identical output across runs and
machines: no wall clock, no unordered iteration (per-stream state is a `Vec`
indexed by stream index), no float accumulation, no threading.

#### How a parser is reached, and the two things that were wrong about it

`Discovery` asks the injected `ParserProvider` for a parser the first time a
stream produces a packet worth parsing, and **keeps it for the whole pass** in a
`Vec<Option<ParserDriver<Box<dyn Parser>>>>` beside the per-stream state. Two
corrections to the previous shape, both of them correctness rather than speed:

1. **One parser per stream, not one per packet.** An H.264 NAL unit ends where
   the *next* start code begins, so a parser thrown away at the end of each
   payload never sees the end of its last unit; and an MPEG-TS stream's
   parameter sets arrive in one packet while the fields they describe are wanted
   for all of them. Holding it is also the safer shape under D6's threat model:
   one `Budget` accumulates across the pass instead of each packet getting a
   fresh full allowance.
2. **The container's record is handed over before any packet.** `build_parser`
   calls `Parser::set_extradata` with `stream.params.extradata`. Without this
   step the whole seam is inert in MP4 and Matroska: the H.264 sequence
   parameter set is in `avcC` and in **no sample**, the AAC configuration is in
   `esds`, the Opus identification header is in `dOps`. Measured on `av.mp4`,
   8 of 8 bitstream-derived stream values come from the record and 0 from the
   packet path.

A record that fails to parse is not fatal. Discovery is *offering* the parser
whatever the container happened to carry; a malformed record means "this told me
nothing", and the container's own fields still stand.

The parser held here is used for **two** things, not one: `refine` drives it to
learn stream parameters, and `fill_codec_duration` asks it — through
`Parser::packet_duration`, `&self`, without driving it — how long each packet
is. See R21b under *Timestamps*. That second use is why the parser is reached
again on every packet past the discovery prefix, in `read_packet`, where the
first use has long since stopped.

`Discovery` therefore has a hand-written `Debug`: a `Box<dyn Parser>` is not
`Debug`, and the parsers are summarised by how many were built.

The direction of the merge is load-bearing and unchanged — `CodecParameters::
fill_from` lets the container win and the parser only fill holes. Inverting it
is how a stream whose bitstream header disagrees with its container ends up
reported wrongly.

### Timestamps

Rules are numbered as `planning/18-formats.md` §1.7 numbers them, so the two
documents compose by citation. The boundary is stated once:

> **This crate owns** field decoding, wraparound, absent-timestamp
> normalisation, PTS generation from DTS, per-stream monotonic-DTS repair,
> packet duration fill-in, `start_time` derivation and duration estimation.
>
> **The CLI owns** `-itsoffset`, `-itsscale`, `-isync`, discontinuity *policy*,
> `-ss`/`-t`/`-to` trimming, output-base normalisation, `-fps_mode` and encoder
> time bases.

Nothing goes through `f64`. Rescaling is `Timestamp::rescale`, which multiplies
in `i128` and divides once with a named rounding mode; cross-base comparison
cross-multiplies rather than converting to seconds. A 1/90000 stream and a
1/1001 stream compared through seconds order *nearly*, and "nearly" is a desync.

**One divergence from the plan, and it is a correction.** The plan applies R8's
pivot rule and R9's cumulative offset to every timestamp. Doing both
double-counts: a raw value the pivot lifted by a period, followed by one it did
not, reads as a jump backwards, and the stream sawtooths. `WrapState::correct`
therefore applies the pivot to the **first** value only — the only one with no
history to take a delta against — and folds it into the offset. Everything after
it is delta tracking in raw space. A property test walks a wrapping clock across
three periods and asserts strict monotonicity with the delta preserved exactly.

#### R21b — the codec's own packet duration

R21 fills a missing duration from the stream's frame rate, which answers for
video and for nothing else: an audio stream's `avg_frame_rate` is `0/0`, and the
reference prints `0/0` for it too. R21b is the other source — the *codec's*
statement — and it closed the largest single gap in the `[PACKET]` section.

**The gap.** Matroska writes no `DefaultDuration` element for an Opus track and
no `BlockDuration` on its blocks. The element is absent from the file, so there
is nothing for `vaco-demux-matroska` to have misread; the reference derives
20 ms from Opus's own TOC byte. The same holds for AAC, which has no in-band
header in Matroska at all and whose frame length lives in `CodecPrivate`.

**Where it lives, and why here.** `Discovery` already holds one parser per
stream, already seeded from the container's configuration record by
`build_parser`, and is already wrapped around every demuxer `vaco-probe` opens.
D14.1 forbids the demuxer from naming a codec crate, so the rule cannot live
there; `TimestampFixer` is a pure state machine over `(stream_index, pts, dts,
duration)` with no parser and no payload, so it cannot live there either — and
making it carry `Box<dyn Parser>` would cost it both its `Clone` and its derived
`Debug`. `Discovery::absorb` and `Discovery::read_packet` therefore both call
one private `fill_codec_duration`, so the prefix and the tail behave the same.

`absorb`'s parser block moved **ahead** of `fixer.fix` to make that work, with
the frame rate read out first so R21's input is byte-for-byte what it was
before. Two things improve as a side effect: R22's `last_duration` becomes the
real step instead of 1, and `analyzed_us`/`last_end` stop under-counting an
audio stream whose container states no duration at all.

**The container always wins.** `fill_codec_duration` only ever writes over
`Duration::ZERO`, the model's spelling of "absent", so a `BlockDuration`, an
`stts` delta or a `DefaultDuration` stands untouched.

**The quantisation is the measured half.** `time::quantise_duration` truncates
the parser's exact `Rational` towards zero into the stream's time base.
Three independent measurements, each on a file whose container states nothing:

| input | exact ticks | `ffprobe 8.1` | nearest would give |
|---|---:|---:|---:|
| 960 samples @48 kHz, base 3/1000 | 6.667 | **6** | 7 |
| 960 samples @48 kHz, base 7/10000 | 28.571 | **28** | 29 |
| 1024 samples @44.1 kHz, base 1/90000 | 2089.79 | **2089** | 2090 |

The first two are a Matroska `TimecodeScale` patched to 3 ms and to 0.7 ms; the
third is an ordinary `-f mpegts` file, so truncation is not a Matroska rule.

**Why the parser's answer has to be exact.** A 2.5 ms Opus packet is exactly
half a Matroska tick. From `120/48000` the truncation gives 2, which is what the
reference prints. From a microsecond-rounded 2500 it gives 3. Half a tick of
error changes the answer whenever the exact value lands just below an integer,
which is most of the time for a 1024-sample frame against a 1 ms base. That is
the entire argument for `Parser::packet_duration` returning a `Rational` rather
than a `Duration`.

**Where the packet model still loses.** `Packet::duration` is microseconds, so
the tick count is stored as its microsecond equivalent and recovered by the
printer's round-to-nearest. That round trip is exact for **every time base whose
tick is longer than 2 µs** — 1/1000, 1/44100, 1/48000, 1/90000, 1/1000000, which
is every container base in the corpus. It is not exact for a finer one: 655360
ticks of 1/28224000, which is what a raw ADTS stream reports, stores as 23220 µs
and reads back as 655361. No demuxer in the tree produces such a base, and
`the_microsecond_round_trip_is_exact_above_two_microseconds` pins both halves so
a future tick-valued `Packet::duration` deletes an assertion rather than
discovering a problem. `quantise_duration` also refuses a positive tick count
that rounds to *zero* microseconds, because `Duration::ZERO` means absent and
returning it would be a duration that silently vanished — found by the
`format_timestamps` fuzz target, not by review.

**Measured, on an eleven-file corpus** (`-of json -show_packets
-read_intervals '%+#40'`, all eleven `[PACKET]` field values compared packet by
packet against `ffprobe 8.1`):

| | before | after |
|---|---|---|
| `duration` | 191 / 420 | **360 / 420** |
| `duration_time` | 191 / 420 | **360 / 420** |
| all fields, Matroska/WebM | 2454 / 2860 | **2792 / 2860** |
| all fields, MP4 | 1320 / 1320 | 1320 / 1320 |
| all fields, MPEG-TS | 98 / 440 | 98 / 440 |
| all fields, total | 3872 / 4620 | **4210 / 4620** |

Per file, `duration` alone: `opus.mka` 0/40 → 40/40, `opus.webm` 0/40 → 40/40,
`op_st.webm` 13/40 → 40/40, `av.mkv` 16/40 → 40/40, `aac.mka` 1/40 → 39/40. MP4
is unchanged at 40/40 on every file, which is the regression that mattered:
`stts` states a duration and the container still wins.

Two files did not move, and both name a gap elsewhere:

* **`flac.mka` 0/20.** FLAC in Matroska has exactly the same gap — no
  `DefaultDuration`, and the reference reports 104 ms from the in-band frame
  header — but there is no `vaco-parse-flac` in the tree. The seam now exists,
  so that crate closes this for free when it lands.
* **`av.ts` 1/40.** `vaco-demux-mpegts` hands over whole PES payloads rather
  than codec frames: one packet of 2836 bytes where the reference emits
  thirteen of ~265. Our duration for it is 27167 ticks, which is exactly
  thirteen frames and therefore *correct for the packet as framed* — the
  divergence is the framing, not the number. The video half is the separately
  recorded field-rate-for-frame-rate halving (1800 against 3600).

### Seeking

Four strategies, one decision function, and a per-stream index. See the
three-container analysis above for why each exists. `binary_search` is bounded
twice — by `log2(size / MIN_SEEK_STEP)` and again by a hard iteration cap — so a
pathological probe closure cannot hang, which is a real fuzzing concern rather
than a theoretical one.

### Interleaving

`MuxTimestamps` runs M1–M4 (rescale, `output_ts_offset`, the
`avoid_negative_ts` shift, the monotonicity check) and `InterleaveQueue` runs N1–N5
(readiness, `(dts, stream, seq)` selection, the sparse-stream escape, EOF, chunk
grouping). The `avoid_negative_ts` offset is computed **once**, from the first
packet across all streams, and applied uniformly — a per-stream offset would
desynchronise them.

M4 is an **error, never a repair**. Silently repairing a non-monotonic DTS here
is how files with subtly wrong durations get made, and the caller is in a far
better position to decide what to do about it.

N6 and N7 are the two escapes from the queue. `interleave_none` is the
pass-through policy MPEG-TS wants — it multiplexes at the 188-byte level against
a PCR clock, so anything the queue reordered first would only be reordered again
— and `Muxer::interleave` is the defaulted hook a container overrides to install
it, or any other policy. `MuxWriter::write_frame` is the caller-ordered path;
M4 still applies to it, so a caller that gets the order wrong is told.

### The muxer state machine

`MuxBuilder` and `MuxWriter` (`src/mux.rs`) own the `Box<dyn Muxer>` and expose
only the operations that are legal next:

```
MuxBuilder ──add_stream──▶ MuxBuilder ──open()──▶ MuxWriter ──write_packet──▶ MuxWriter
                                       init+header    │
                                                      └──finish()──▶ MuxReport
                                                          drain+trailer
```

`MuxBuilder` has no `write_packet`; `MuxWriter` has no `add_stream`; `open` and
`finish` **consume** what they transition from. So "the header is written
exactly once" and "no packet after the trailer" are not runtime checks that a
caller might skip — they have no spelling that compiles.

**Why a wrapper and not a trait change.** Five container crates were being
written against `Muxer` in parallel when this landed. A trait change lands
underneath all five at once, so every addition is a *defaulted* method
(`init`, `interleave`, `check_bitstream`, `query_codec`, `write_flush`) or a new
type. The wrapper gives callers the guarantee without asking implementors for
anything. A phantom typestate (`Mux<Building>`) would give the same guarantee
and put a type parameter in every signature that touches a muxer, buying nothing
that a consuming transition does not. Runtime checks on the trait is what we had:
`VacoRawMuxer` still carries its own `header_written`/`trailer_written` guards,
and five more containers would have meant five more copies, each with its own
error string.

The honest cost: an implementor can still be driven directly through
`dyn Muxer` and get the old, unpoliced behaviour. The wrapper is the supported
path, not the only one — and `tests/mux_session.rs` asserts the two produce
byte-identical files, so adopting it is a no-op for an existing caller.

**The rule numbering.** `planning/18-formats.md` §8.2 names FW-08 as "M1–M28"
and §7.1 repeats the span, but §1.7.7 defines **M1–M7** and nothing else. M8
upward do not exist in the plan under any spelling. The table in `src/mux.rs`'s
module doc is therefore *ours*, with each row citing the plan section that
motivates it; if a real M8–M28 list turns up, renumber against it rather than
re-deriving.

### Bitstream-filter-in-muxer (§1.10)

`Muxer::check_bitstream` is asked on a stream's first packet and the answer is
cached for the file (B3). `BitstreamAction::Insert { name }` stacks a filter and
re-asks, to `MAX_BSF_DEPTH` (4); asking for the same filter twice is treated as a
loop and refused. Filters arrive through `BsfProvider`, the mux-side mirror of
`ParserProvider` and the same D14.1 seam — no format crate names a `vaco-bsf-*`
crate. `NoBsfs` is the default and **errors** when a filter is requested rather
than passing the packet through unfiltered: a container that needed
`aac_adtstoasc` and did not get it produces a file no player opens, and that is
much cheaper to discover at mux time. `fflags -autobsf` disables the stage (B1).

`global_header_action` is the one condition every `GLOBALHEADER` container
shares — extradata wanted out of band and absent means `extract_extradata` —
written once so each muxer does not re-derive it.

---

## How to change it

* **Adding a container.** Write a `DemuxerDesc` with a `probe` function drawn
  from `ProbeScore`'s convention table, implement `Demuxer`, and call
  `SeekStrategy::choose` at the top of `seek`. `vacoraw` is the worked example;
  read it before writing the second one.
* **`Eof` must be sticky.** `read_packet` generally consumes bytes before it can
  tell whether a packet follows, so a demuxer that does not latch end of stream
  reports the middle of its own trailer as corruption on the *second* call. That
  is a real bug this crate's integration tests caught in `vacoraw`, and the
  frozen `Demuxer` trait does not say `Eof` has to be stable. It should. Until
  it does, every demuxer needs the flag `VacoRawDemuxer` has.
* **Changing a probe score changes observable output.** `probe_score` is printed
  by `vaco-probe`, so any change to the scoring model is a conformance-matrix
  change, not an internal one.
* **Adding a `FormatFlags` bit** means adding a row to `FORMAT_FLAG_NAMES`; a
  test asserts the two never drift.
* **Adding a `FormatOptions` field** means adding it in the reference's own
  order, because `-h demuxer=…` prints in declaration order and a test pins the
  whole list. If the reference does not have the option, say so in its doc
  comment — `recursion_limit` is the one such field today.
* **Do not add a `HashMap` anywhere.** Iteration order is output order (DD2).
* **Adding a `Muxer` method.** Default it. Container crates are written against
  this trait in parallel with the core, and an undefaulted method breaks all of
  them at once. If a method genuinely cannot be defaulted, that is a
  coordination event, not an edit.
* **`Muxer::query_codec` is `&self` and object-safe**, unlike plan 18 §1.3's
  `where Self: Sized` spelling. A `Self: Sized` method cannot be called through
  `dyn Muxer` and every caller in this workspace holds one.
* **Plan 18 §1.3 spells the flush marker `write_packet(None)`.** We use a
  separate defaulted `write_flush`, because changing `write_packet`'s signature
  was the one thing that could not be done while five muxers were being written
  against it. It is gated on `FormatFlags::ALLOW_FLUSH`, so a muxer that would
  read `None` as end-of-stream never sees one.
* **Gotcha — `TS_DISCONT` is load-bearing in three places**: it suppresses the
  monotonic repair, it disables bisection, and it changes what
  `SeekStrategy::choose` returns. Setting it on a format that does not need it
  degrades seeking silently.

---

## Configuration

`FormatOptions` is the whole table: 38 options reproduced from the reference by
name, type, default and named constants, plus one of ours. Values were read from
`ffmpeg -h full`'s `AVFormatContext AVOptions` block on the pinned reference —
black-box observation of a shipped binary, which is what D6 and D7 permit.

Three corrections to `planning/18-formats.md` §1.11, which was written from an
older survey:

* `fflags` has **twelve** constants, not fourteen: there is no `nonblock` and no
  `shortest`.
* `fdebug` has **one** constant, `ts`. There is no `id3v2`.
* `recursion_limit` does not exist on the reference at all. We keep it as a
  security bound on nested demuxer opens (concat lists, HLS variants), enforced
  here so no nested demuxer can forget it. Being a strict superset breaks no
  script — D17's converse case.

Constants defined here rather than taken from the reference, each recorded as a
choice rather than presented as reproduction:

| Constant | Value | Basis |
|---|---|---|
| `PROBE_BUF_MIN` | 2048 | Our starting window for the retry loop |
| `PROBE_BUF_MAX` | 1 MiB | **Measured** — `formatprobesize` defaults to `1048576` |
| `DEFAULT_DURATION_PROBESIZE` | 250 KiB | Ours. The reference's value is unmeasured (VERIFY-T4) |
| `MIN_SEEK_STEP` | 64 KiB | Ours. Bounds the bisection and the iteration count |
| `PacketIndex` decimation | drop every second non-key, then every second key | Ours. Not observable through any output field |

### `avoid_negative_ts`, measured

Four modes and an `auto` whose resolution depends on the container, so it was
measured rather than recalled. Method: mux the same input at each of the four
values and **compare the output bytes**, because reading a timestamp back
through `ffprobe` measures the *demuxer* — an MP4 written with a negative start
reads back as `-0.040000` rather than `-1.000000` because the edit list is
applied on the way out (plan 13 §1b's "the field you read back is not the field
you set").

```sh
ffmpeg -y -i src.mp4 -c copy -copyts -fflags +bitexact \
       -output_ts_offset -1 -avoid_negative_ts MODE -f FMT out
```

| `auto` resolves to | Muxers measured (ffmpeg 8.1) |
|---|---|
| `disabled` (container declares `TS_NEGATIVE`) | `mov`, `mp4`, `ismv`, `3gp`, `psp`, `ipod` |
| `make_non_negative` | `matroska`, `mpegts`, `avi`, `asf`, `wtv`, `nut` |
| unobservable — all four modes byte-identical | `wav`, `adts` (they store no timestamps) |

Two further results:

* **`make_zero` and `make_non_negative` differ only when the first timestamp is
  positive.** At `-output_ts_offset +5`, `make_non_negative` produced bytes
  identical to `disabled` while `make_zero` shifted back to zero. With a
  negative first timestamp the two are identical. That is the modelled
  behaviour, now measured in both directions.
* **The shift is derived from `min(pts, dts)`, not from `dts`.** On an mpeg4
  stream encoded `-bf 2` whose first packet reports `dts=0.000000
  pts=-0.040000`, a dts-only rule shifts by nothing; every one of mp4, matroska,
  mpegts, nut and avi shifted by exactly `+0.040000`. `MuxTimestamps` was
  computing the offset from DTS alone and now takes the minimum. Only the
  pts-negative branch was observed directly — the toolchain to hand could not
  construct `dts < 0 <= pts` from a real file — and `min` degrades to the old
  behaviour whenever `pts == dts`, which is every packet of every stream that
  does not reorder.

A measurement error worth recording, since it nearly became a finding:
`-fflags +bitexact` placed **before** `-i` sets the flag on the *input*, and
Matroska then writes random `SegmentUID`/`TrackUID` values, so two runs of the
same command differ in 60 bytes. Placed as an output option it is deterministic.
The flag is positional and the wrong position looks like nondeterminism in the
muxer.

`container_start_time` implements the plan's stated **minimum**-over-streams
rule. The plan marks min-versus-max as VERIFY-T2 and we have not measured it, so
this is unverified rather than reproduced. It is worth an hour with an MP4 whose
audio starts at 0.000000 and video at 0.041708.

---

## Dependencies

`vaco-core` (errors, `Rational`, `Timestamp`, exact rescaling), `vaco-io`
(`MediaSource`/`MediaSink`/`IoContext`), `vaco-packet`, `vaco-codec-core`
(`CodecParameters`, `CodecId`, `Parser`), `vaco-opts` (the option derive),
`vaco-limits` (`Budget`, `ProgressGuard`), `bitflags`, `smallvec`.

No external media crate. No codec crate — that is the `ParserProvider` seam's
whole purpose.

---

## One approved change to a frozen interface

`Muxer::stream_time_base(&self, u32) -> Option<Rational>`, defaulting to `None`,
was **added after the freeze with the orchestrator's approval**.

`add_stream` takes only `&CodecParameters`, and the muxer — not the caller —
decides what the container can express: MP4 wants the media timescale, MPEG-TS
is fixed at 1/90000, Matroska derives one from `TimestampScale`. But step M1 of
the muxer-side chain rescales every packet *into* that base, so a caller holding
a `dyn Muxer` that cannot ask what it is could not use the interface correctly
at all. That is not drive-by churn; it is a signature that does not work, which
is the one thing the freeze is not there to protect.

The default returns `None` — "assume `TIME_BASE_Q`" — so no implementation
breaks and a muxer with no opinion need not invent one. `VacoRawMuxer`
implements it, and the round-trip fixtures now ask the muxer for the base rather
than assuming it, which is what a real caller has to do.

## The 2026-08-22 widening: `duration_ts`, the frame-rate pair, side data

Three things `ffprobe` prints had no home on `Stream`, so all three demuxers
kept them in private side tables reachable only through inherent methods on
their concrete types — and `DemuxerDesc::open` returns `Box<dyn Demuxer>`, so
`vaco-probe` could not reach any of them. `vaco-demux-mp4`'s author called it
"now blocking". The fix is on `Stream`; the interesting part is *which* of the
three became a field and which did not.

### `duration_ts` replaces `duration`, rather than joining it

`Stream::duration` was an `Option<Duration>` — microseconds. A media timescale
does not survive that: 25 500 ticks at 1/12800 is 1 992 187.5 µs, and the
reference prints `duration_ts=25500`.

Adding `duration_ts` *beside* `duration` would have put one concept in two
fields, which D19 exists to stop, and left every writer free to set one and not
the other. So the field is now

```rust
pub duration_ts: Option<i64>,          // ticks of `time_base`, stored
pub fn duration(&self) -> Option<Duration>   // microseconds, derived
```

The lossy view is the derived one. `set_duration_ts` refuses a negative tick
count rather than clamping it: no container states a negative length, so one
means the arithmetic that produced it was wrong, and `None` keeps that visible
as `N/A` instead of printing a confident `0`.

### The frame rates are a pair because they genuinely differ

`r_frame_rate` and `avg_frame_rate` both used to be answered by
`params.video.frame_rate`, which is one field, so the two could not diverge —
and they do. A 1/600-timescale MP4 whose `stts` holds mostly 60-tick deltas
with a few 20-tick ones reports `r_frame_rate=10/1` and `avg_frame_rate=300/29`
on the same track. `params.video.frame_rate` is still set, because parsers and
filters want *a* rate; the two printed fields are now their own.

Both are plain `Rational`, not `Option<Rational>`: the reference prints `0/0`
for a stream with no rate — including every audio stream — never `N/A`, so
there is no third state to model.

### The display matrix is side data, and that is a deliberate refusal

It would have been one line shorter as `display_matrix: Option<[i32; 9]>`. It
is a `Vec<StreamSideData>` instead, for reasons written out in the [`sidedata`]
module docs: the reference prints a *list* whose length varies, the eight other
members plan 18 §1.1 names would each want their own mostly-`None` field, and
the matrix means the same thing whether it arrived in an ISOBMFF `tkhd`, a
Matroska `Projection` or an H.264 SEI.

`StreamSideData` is deliberately **not** `#[non_exhaustive]`. Everything that
consumes it is in this workspace, and `non_exhaustive` would force a catch-all
arm into `vaco-probe`'s printer — turning "a new side-data kind is unprinted"
from a compile error into a silently missing `[SIDE_DATA]` block.

`display_rotation` is measured, not derived from first principles. It
normalises each *column* to unit length before taking the angle, and the file
that proves it is `[65536, 66000, 0, 0, 65536, 0, …]`: the reference reports
`-35`, where the obvious `-atan2(b, a)` predicts `-45`. A corpus of pure
rotations cannot tell the two rules apart.

### `Program` gained four MPEG-TS fields

`program_num`, `pmt_pid`, `pcr_pid`, `pmt_version`. `vaco-demux-mpegts` was
putting the last three in `Program::metadata`, where they printed as
`TAG:pmt_pid=…` — the right values in the wrong section.

`pmt_version` is on the struct and is **not printed**. Plan 18 §1.1 and the
brief that asked for it both say `-show_programs` prints it; measured with
`-of flat -show_optional_fields always -show_programs` on `ffprobe 8.1`, the
section is `program_id`, `program_num`, `nb_streams`, `pmt_pid`, `pcr_pid` and
the tags, and nothing else. The field stays because a demuxer needs it to
notice a PMT change, not because anything prints it.

### The new shared rule in `Discovery::finish`

**A stream the pass never saw a timestamp for takes the container's start time
and duration**, each rescaled into its own time base.

This is a container-wide rule wearing a per-stream disguise, and it was
measured on Matroska because Matroska is where it shows:

| file | subtitle `start_pts` | subtitle `duration_ts` |
|---|---|---|
| `sub.mkv` — subtitle only; container start `N/A`, duration 2.000 | `N/A` | **2000** |
| `as.mkv` — opus + subtitle; container start 0, duration 2.008 | 0 | **2008** |
| `as2.mkv` — as above, but the subtitle's last event ends at 1.0 s | 0 | **2008** |
| `live_as.mkv` — as above, muxed to a pipe so there is no `Duration` element | 0 | `N/A` |

`as2.mkv` rules out the stream's own extent — the value ignores where the
subtitle stops. `live_as.mkv` rules out a packet scan — remove the container's
statement and the field goes with it. And the per-track `DURATION` *tag* is not
the source either: `as2.mkv`'s says 1.0 s where the printed field says 2.008.

It is here and not in `vaco-demux-matroska` because it needs the container
duration and the whole stream list, neither of which one track knows, and
because a demuxer that filled it locally would disable the shared rule for
every caller that does run discovery — the hazard plan 18's composition
amendment records.

**It does not fire on today's corpus**, and that is worth stating plainly. Our
discovery loop runs until every stream has *two DTS deltas*, so it always sees
the subtitle packet and sets `start_time` from it; the reference's loop stops as
soon as every stream's codec parameters are complete, which for a file of
subrip and Opus is before it reads anything at all. That difference is the
whole of the remaining `sub.mkv` divergence — see `docs/app/vaco-probe.md` — and
narrowing the stop condition to match is a much larger change than this one,
with `start_time` for every delay-coded audio stream riding on it.

## The 2026-08-23 wave: four interface gaps closed, one substituted

`planning/INTERFACE-GAPS.md` recorded six gaps found independently by agents
building containers against this crate. Four of them (1, 4, 5, 6) were this
wave's; two (2, `Muxer` being single-sink, and 3, `write_packet` taking packets
where `uncodedframecrc` wants frames) are shape changes and stay open — see that
file's own record for why.

**None of the four required editing an implementor.** `cargo check --workspace
--all-targets --offline` was run after all four landed; every muxer, demuxer,
`vaco-sched`, `vaco-cli` and `vaco-probe` still compiled unmodified. (One
unrelated crate, `vaco-protocol-socket`, failed in the same run with missing
files and missing dependencies — a concurrent agent's in-progress work, nothing
to do with `Muxer`/`Demuxer`; the failure is confined to that crate alone.)

### Gap 1 — a metadata channel for `Muxer`

[`metadata::MuxMetadata`] bundles file tags, chapters (reusing [`Chapter`]
verbatim, so a demuxed chapter list needs no conversion to remux), attachments
(the new [`metadata::MuxAttachment`]), and per-stream tags indexed by declared
position. [`Muxer::set_metadata`] is a new defaulted trait method — the default
does nothing, which is exactly what every muxer already did before this
existed, since there was no channel to drop anything from. [`mux::MuxBuilder::
with_metadata`] queues a bundle; `MuxBuilder::open` calls `set_metadata` once,
after `init` and after stream time bases are read but before the header (M30) —
the same point M12 settles anything else that depends on the whole stream set.

`vaco-mux-matroska`, `vaco-mux-mp4` and `vaco-mux-stream`'s `ffmetadata` can now
override `set_metadata` to actually write `Tags`/`Chapters`/`Attachments`,
`udta▸meta▸ilst`/`chpl`, and a real `;FFMETADATA1` body respectively — that
work is theirs, in a later wave; this wave only opens the door.

### Gaps 4 and 5 — `open` sees neither `Limits` nor options

**Not closed as specified**, and that is a finding, not an evasion. Both gaps
proposed widening `DemuxerDesc::open`/`MuxerDesc::open`'s signature. Checked
directly: `open` is a bare `fn` pointer, and every one of the ~90 registered
descriptors already supplies its own free function coercing to today's exact
signature. A function item only coerces to a function-pointer type with a
*matching* parameter list — there is no version of `fn(A, B) -> R` that also
accepts `fn(A, B, C) -> R` — so widening it requires editing every one of those
functions, not merely the descriptor literals that reference them. That is the
edit this wave forbids, so it was not made.

The substitute: [`Demuxer::reconfigure(&mut self, limits: &Limits, opts:
&FormatOptions)`][Demuxer::reconfigure] and [`Muxer::set_option(&mut self, name:
&str, value: &str)`][Muxer::set_option], both defaulted, both callable
*after* `(desc.open)(..)` returns instead of *during* the call. `Discovery::run`
now calls `reconfigure` once before reading anything, so wrapping a demuxer in
`Discovery` is enough to reach it; a fuzz target driving `open` directly can and
should call it too. `MuxBuilder::open` calls `set_option` for every pair queued
through the new `with_private_options`, before `init` runs (M29) — the seam
`vaco-mux-mp4`'s `MovMuxer::with_options`/`movflags` needs, mirroring
`vaco_opts::OptionsExt::set_str`'s name/value-string contract on purpose so no
second option-passing convention is needed.

**What this does not fix**, honestly: neither method can bound or configure
work `open` already did before returning — a header or index a container reads
eagerly, or an `init` decision `set_option` did not exist in time to influence
had the caller not queued it beforehand. `vacoraw::VacoRawDemuxer::open`'s
`Budget::new(Limits::permissive())` is exactly that case, and it is unchanged.
Closing that half needs the `open`-signature change these methods stand in for,
and that is only possible in a wave that edits every implementor at once —
which is what gap 6 below explains further.

### Gap 6 — `MuxerDesc` has no `flags` field, and cannot get one for free

**Also not closed as specified**, for the same class of reason. `DemuxerDesc`
has `flags: FormatFlags`; the brief proposed the same field on `MuxerDesc`.
Checked two ways before concluding it is not additive:

* Every one of the ~90 registered `MuxerDesc` constants lists every current
  field with no `..base` update syntax (grepped, confirmed) — Rust requires a
  struct literal with no base expression to name every field, so any new field,
  regardless of type or a "sensible" default, must be added at every one of
  those call sites.
* Default field values (`x: T = default`, RFC 3681) would remove that
  requirement. Checked directly against this workspace's pinned `rustc 1.97.1`:
  still `error[E0658]`, gated behind `#![feature(default_field_values)]`, not
  reachable on the stable toolchain this project pins.

The substitute is [`MuxerDesc::probe_flags`][MuxerDesc::probe_flags] — a method,
not a field. It does exactly what `vaco-cli`'s `exec::open_output` already did
by hand (construct against a throwaway [`vacoraw::MemorySink`], read `.flags()`,
keep the answer), except written once, here, instead of once per caller. It
does not remove `exec::open_output`'s double construction of a real output — a
non-`NOFILE` format is still opened again against its real sink — it removes
the *duplication of the probing logic itself*. Landing the field for real needs
a wave that touches every `MuxerDesc` literal at once, the same wave
`DemuxerDesc.flags` itself must have needed, before any implementor existed to
edit.

### `INTERFACE-GAPS.md` corrections

Its "Sequencing" note claimed 1, 4, 5 and 6 "can land together behind
default-implemented trait methods and a new struct field, so existing muxers
and demuxers keep compiling." Gap 1 is exactly that. Gaps 4, 5 and 6 are not:
they involve either a function-pointer field's signature or a plain field on a
struct every implementor constructs by literal, and neither is addable without
touching every one of those literals — confirmed against the actual `rustc`
this workspace pins, not assumed. The substitutes above are the closest
additive answer to each; `planning/INTERFACE-GAPS.md` records the same finding
next to each gap's original entry, per this wave's brief ("leaving the entries
and their reasoning, since the record of *why* an interface changed is worth
more than the entry").

## Signature gaps

Interfaces are frozen (plan 19 §6), so these are **reported, not changed**. In
descending order of how much they cost.

1. **`DemuxerDesc::open` takes no options and no limits — partially closed.**
   See *The 2026-08-23 wave* above: [`Demuxer::reconfigure`] reaches an
   already-constructed demuxer with the caller's `Limits`/`FormatOptions`, which
   is enough for anything `Discovery` or a fuzz target does after `open`
   returns. It is *not* enough for what a demuxer allocates *during* `open`
   itself — `VacoRawDemuxer::open`'s hardcoded `Budget::new(Limits::
   permissive())` is unreached by design, and closing that needs the
   `open`-signature change a whole-workspace wave would take.
2. **`ParserProvider` has only `parser_for`.** The plan's `refine` is reachable
   by driving the parser and reading `Parser::parameters`, so nothing is lost
   there. `probe_codec` — content-sniffing a payload to a `CodecId` — is
   genuinely absent, and raw elementary streams and MPEG-TS private streams
   need it.
3. **`Demuxer` has no `read_timestamp` hook**, so the core cannot drive a
   bisection on a demuxer's behalf. Handled by inverting the relationship: the
   demuxer calls `binary_search` and supplies the probe closure. Arguably better,
   and recorded here because it is a visible departure from plan 18 §1.8.2.
4. **`SeekTarget` has no range form.** The plan wanted `(min_ts, ts, max_ts)`,
   which is how `-ss` expresses "do not overshoot" without `BACKWARD`. The
   single-target-plus-flags form covers every case we can currently test, but
   `-ss` precision will want the range.
5. **`Stream` is missing two of the five fields** the plan specifies.
   `duration_ts`, the `avg_frame_rate`/`r_frame_rate` pair and stream side data
   are now present — see *The 2026-08-22 widening* below. Still absent, and
   still nothing asks for them: `pts_wrap_bits` (the demuxer holds the
   `WrapState` instead), a container-level `sample_aspect_ratio` override,
   `discard`, and `attached_pic` (the `ATTACHED_PIC` disposition plus a normal
   stream covers every case `vaco-probe` has met).
6. **`Demuxer::read_packet` does not promise `Eof` is stable.** See *How to
   change it*. Costs one flag per demuxer and one class of bug per demuxer that
   forgets it.

### Wanted from other crates

* **`vaco-codec-core`: `Parser::set_extradata` — done.** The trait grew a
  defaulted `set_extradata(&[u8]) -> Result<()>`, which is what makes a parser
  useful at all in MP4 and Matroska. See *How a parser is reached* above.
* **`vaco-codec-core`: `Parser::packet_duration` — done.** A defaulted
  `packet_duration(&self, &[u8]) -> Option<Rational>`, returning an exact
  duration in seconds. It is what closes R21b: Matroska states no duration for
  an Opus or AAC track, so the number only exists in the bitstream, and D14.1
  keeps the demuxer from reaching it directly. See R21b under *Timestamps* for
  the measurements and for why the return type is an exact ratio.
* **`vaco-codec-core`: `impl Parser for Box<dyn Parser>` — done.** The
  orchestrator added `impl<P: Parser + ?Sized> Parser for Box<P>`, so
  `ParserDriver<P>` now accepts what `ParserProvider::parser_for` returns.
  `discovery::refine` drives the parser through the driver, and the hand-rolled
  reassembly and byte-accounting it used to carry is gone — that code existed
  only to work around the gap, and keeping it would have meant two
  implementations of the end-of-stream convention drifting apart.
  `Discovery::with_limits` is the new knob that caps what an injected parser may
  allocate, since `DemuxerDesc::open` still takes no `Limits`.
* **`vaco-io`: a seekable in-memory `MediaSink` — deferred, by decision.**
  `MemorySource` exists for reading; there is no writable counterpart, and a
  muxer's header-patch path cannot be tested without one. `vacoraw::MemorySink`
  stays here for now: a `MemorySink` is better specified by the second real
  muxer than by one example container. **Candidate for promotion into `vaco-io`
  once `vaco-mux-mp4` or `vaco-mux-matroska` says what it actually needs.**

---

## Testing

* **193 tests**: 150 unit, 23 named integration cases (14 `roundtrip.rs`, 9
  `mux_session.rs`), 19 property tests, 1 doctest. The unit count includes the
  gap-closure tests added in *The 2026-08-23 wave*: for each of `Muxer::
  set_metadata`, `Muxer::set_option` and `Demuxer::reconfigure`, one test
  pinning the default's harmless behaviour and one pinning an override's real
  one, plus coverage of `MuxerDesc::probe_flags` and `MuxMetadata` itself.
* **The state machine's guarantees are not tested, because they are not
  testable.** `MuxBuilder` has no `write_packet` and `MuxWriter` has no
  `add_stream`, so the illegal sequences have no spelling that compiles.
  `tests/mux_session.rs` covers what is left: that the M-chain reaches a real
  muxer, that the file reads back, and that a session-written file is
  byte-identical to a directly-written one given the same base.
* **The worked example is the proof.** `vacoraw` is muxed and demuxed end to
  end, with the index path, the bisection path and the byte path each covered by
  a test that fails if the strategy is not the one taken.
* **`tests/properties.rs` is `proptest`.** The properties were first written
  against a fixed xorshift generator, because adding a dev-dependency rewrote
  `Cargo.lock` and `--locked` refused it. That restriction was lifted — a
  pre-declared dependency adds an edge, not a package — and they were ported.
  Shrinking paid for itself immediately: see below.
* **`tests/roundtrip.rs` is named cases.** A specific file, a specific seek, a
  specific truncation, each chosen because it pins one rule down. The two files
  share fixtures deliberately.
* **Four fuzz targets** (D6): `format_probe`, `format_vacoraw_demux`,
  `format_interleave`, `format_timestamps`. The last two fuzz a *call sequence*
  rather than a byte stream, the same shape as `codec_send_receive`, because the
  ordering and timestamp machinery is shared by every muxer and demuxer in the
  project. `format_interleave` has a second phase that drives the same op
  sequence through `MuxBuilder` over a real muxer and a real sink, so the state
  machine, the M6 filter stage and the muxer's own byte writing are reachable
  from one corpus: `exit=0 execs=#1916517`, `find fuzz/artifacts -type f` empty.
  `format_timestamps` after the same change: `exit=0 execs=#3440192`, artifacts
  empty.

### What the generated tests found

**`format_timestamps` (fuzz), on the R21b widening.** `quantise_duration` could
return `Some(Duration::ZERO)` for a positive tick count on a time base finer
than 2 µs a tick. `Duration::ZERO` is the model's spelling of *absent*, so the
value would have been a duration that silently disappeared one line later. The
target asserts the postcondition — a filled-in duration is positive and never
longer than the exact ratio it came from — and found it in under a minute;
review had not. `exit=0 execs=#1579527` after the fix, with
`find fuzz/artifacts -type f` empty.

**`format_timestamps` (fuzz), first run.** R22's monotonic repair could saturate
at `i64::MAX` and then claim to have repaired a stream it had left
non-increasing. Fixed by *reporting* it — `FixReport::dts_overflow` — rather
than by weakening the invariant, because "DTS strictly increases after a repair"
is precisely what a scheduler leans on.

**`the_index_stays_well_formed` (proptest), first run.** `PacketIndex::add`
computed its insertion point, *then* decimated if the index was full, then
inserted at the position it had computed. Decimation shortens the vector, so the
position was stale and the entry landed in the wrong slot, silently unsorting
the index — after which every seek is wrong. Clamping to the new length hid the
symptom for an ascending insertion order and for no other, which is exactly why
the unit tests missed it: they all insert in timestamp order, and real
containers mostly do too. Fixed by decimating *before* choosing the position.

The shrunk counterexample was four entries at timestamps `0, 1, -1, 2` with a
two-entry cap, and it is now a named regression test. It is worth noting that
the xorshift version of this property had been running for the whole first pass
of this crate without finding it: the bug needs a small cap *and* an
out-of-order insertion *and* the search to land mid-vector, and random cases hit
that combination rarely. Shrinking is what turned it from a 400-entry failure
nobody would read into four entries.

Two further proptest failures were **the properties being wrong, not the code**,
and both are recorded here because they are easy to re-derive incorrectly:

* A backward seek into a file with *no keyframes at all* carries no index, so it
  takes the bisection path, whose documented fallback is the first sync point in
  the range. Landing after the target is correct there — it is the best a
  backward seek can do when there is nothing behind you.
* `Error::InvalidData` is recoverable by design: the demuxer skips the bad
  header and resynchronises, so reading past one is correct and only `Eof` is
  terminal. Only `Eof` is required to be stable.
