# `vaco-demux-mp4`

Layer 4. The MP4 / MOV / 3GP / fragmented-MP4 **demuxer**: the crate that turns
a file into streams and packets.

It is not the box parser. [`vaco-format-isom`](vaco-format-isom.md) owns the box
grammar, the sample tables, the edit list, the fragment boxes and the
four-character-code tables, and nothing here re-parses what that crate already
parses. What is here is demuxing *policy* — which tracks become streams, what
numbers they report, in what order packets come out, and where a seek lands.

Written from **ISO/IEC 14496-12** (base file format), **14496-14** (MP4),
**14496-15** (`avcC`/`hvcC` carriage), **14496-1** (`esds`), **3GPP TS 26.244**
(the `udta` asset boxes), **ISO/IEC 23008-12** (HEIF image items), and Apple's
published *QuickTime File Format Specification*. No FFmpeg source was consulted
(D7/D15); every behavioural fact below was measured by running `ffprobe 8.1`
and is recorded with its command.

---

## What it is

| Module | Contents |
|---|---|
| `lib` | `Mp4Demuxer`, the `DEMUXER` descriptor, opening, packet emission, seeking |
| `track` | one `trak` → one `Stream`: codec parameters, aspect ratio, bit rate, tags |
| `meta` | `ftyp` brands, `udta ▸ meta ▸ ilst`, `keys`, the 3GPP boxes, `chpl`, `covr` |
| `read` | the per-track sample queue and the rule that decides which packet is next |
| `options` | `Mp4Options` — the demuxer-level option table |

```rust
use vaco_format_core::{Demuxer, FormatOptions, discovery::NoParsers};
use vaco_io::{MediaSource, MemorySource};
use vaco_demux_mp4::{Mp4Demuxer, Mp4Options};

let src: Box<dyn MediaSource> = Box::new(MemorySource::new(std::fs::read("clip.mp4")?));
let mut demux = Mp4Demuxer::open(src, &NoParsers, &FormatOptions::default(), Mp4Options::default())?;
for s in demux.streams() {
    println!("{i} {:?} {:?}", s.params.codec_id, s.duration_ts);
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

---

## How it works

### Opening

1. Scan the top level with `isom::scan::TopLevelScanner`, reading **only** box
   headers, until the `moov` is found. `ftyp` is read on the way past.
2. The `moov` **payload is read into memory and kept**, because every sample
   table borrows from it. `MAX_MOOV_BYTES` (16 MiB) caps it — that is a `stbl`
   of roughly four million samples, i.e. a thirty-hour recording.
3. Parse the `moov`, decide whether the file is fragmented (`mvex`), and if it
   is, walk the rest of the top level collecting `moof` payloads.
4. Build one `Stream` per usable `trak`, then read `udta` for metadata,
   chapters and cover art.

A `moov` that follows `mdat` on a source that cannot seek is refused with an
error naming the fix (`-movflags +faststart`) — plan 18 §3.1.3 is right that
this is the single most common user question about MP4, and an error that says
"invalid data" wastes the user's afternoon.

### Why samples are produced in batches

This is the one structural decision worth reading before changing anything.

`vaco-format-isom`'s tables **borrow** the `moov` bytes: `SampleTable<'a>` is a
set of `&'a [u8]` slices plus decimated summaries, which is exactly what makes
it allocate nothing proportional to the sample count. A `Box<dyn Demuxer>` is
`'static`, so the demuxer owns those bytes and **cannot also hold a structure
that borrows them** — safe Rust has no way to express that without a
self-referential helper crate (`yoke`, `ouroboros`, `self_cell`; none is in
`[workspace.dependencies]`) or leaking the buffer.

So the demuxer re-parses one track's `stbl` per **refill** and takes a batch of
samples into an owned queue. Parsing is O(samples) — `vaco-format-isom`'s own
benchmark measures 1.3 ns per sample, 64 µs for a 50 000-sample table — so a
*fixed* batch would make the total work quadratic in the sample count. The batch
therefore **grows geometrically**, from `BATCH_MIN` (4096) to `BATCH_MAX`
(128 Ki), which bounds the number of refills at
`log2(BATCH_MAX / BATCH_MIN) + count / BATCH_MAX` and the total re-parse work at
a small multiple of one parse. A three-hour 30 fps track costs about eight
refills and 3 ms of re-parsing across the whole file.

Residency is bounded by a constant: at most `BATCH_MAX` queued samples per
track, about 6 MiB, against the ~13 MB *per track* a materialised
`Vec<Sample>` would cost. **This is worth reporting, not working around** — see
*Wanted from other crates* below.

### The numbers `ffprobe` prints

Everything in this section was measured, not inferred, per plan 13 §1b. The
fixtures:

```sh
ffmpeg -f lavfi -i testsrc2=size=160x120:rate=25:duration=2 \
       -f lavfi -i sine=frequency=440:duration=2 \
       -c:v libx264 -preset ultrafast -g 15 -bf 2 -pix_fmt yuv420p -c:a aac \
       -movflags +faststart prog.mp4
ffmpeg … -movflags +frag_keyframe+empty_moov                  frag.mp4
ffmpeg -itsoffset 0.5 … -bf 0                                 delay.mp4
ffmpeg … -bf 2 -movflags +negative_cts_offsets                ncts.mp4
ffmpeg … -metadata title=… -disposition:v attached_pic        cover.m4a
ffmpeg … -f mov                                               rot.mov
```

and the comparison is

```sh
ffprobe -v error -show_streams -show_format -show_packets file.mp4
cargo run -p vaco-demux-mp4 --example mp4dump -- file.mp4 packets
```

`examples/mp4dump.rs` exists for exactly this: a measurement nobody can re-take
is a measurement nobody can re-check.

#### `duration_ts` — three sources, and `mdhd` is not one of them

The container duration is the latest stream end, compared as exact rational
seconds across track clocks. Keep native ticks through that comparison: a
microsecond intermediate loses 44.1 kHz audio and NTSC periods even when every
sample-table count is correct. Display formatting rounds only at the sink.

The synthetic `attached_pic` cover-art stream receives that same aggregate
duration by direct rational rescaling into its `1/90000` clock. It must not
pass through a microsecond timestamp first, because that changes edge-case
rounding at the cover stream's tick boundary.

> `duration_ts = min(non-empty elst total rescaled, min(mdhd.duration, Σ sample durations))`

| File | `mdhd` | Σ `stts` | `elst` → media | `duration_ts` |
|---|---:|---:|---:|---:|
| `prog.mp4` video | 26 112 | 25 600 | 25 600 | **25 600** |
| `prog.mp4` audio | 89 224 | 89 224 | 88 200 | **88 200** |
| `stts` delta patched to 510 | 26 112 | 25 500 | 25 600 | **25 500** |
| `elst` segment patched to 1000 | 26 112 | 25 600 | 12 800 | **12 800** |
| `edts` removed, `mdhd` patched to 20 000 | 20 000 | 25 600 | — | **20 000** |
| `edts` removed, `mdhd` patched to 40 000 | 40 000 | 25 600 | — | **25 600** |

The last two rows are the discriminating pair. `vaco-format-isom`'s
`Track::reported_duration` returns `mdhd.duration` when there is no edit list
and the raw `elst` total when there is; **both are wrong** against the binary,
which is why the calculation lives here. Its measurement of the *edit-list* half
is right and is reproduced.

#### `bit_rate` — divides by the media limit, not by `duration_ts`

> `bit_rate = Σ sample sizes × 8 × timescale / min(mdhd.duration, Σ sample durations)`

| File | bytes | divisor | exact | printed |
|---|---:|---:|---:|---:|
| `prog.mp4` video | 71 107 | 25 600 | 284 428.00 | 284 428 |
| `stts` 513 | 71 107 | 25 650 | 283 873.56 | **283 873** |
| `stts` 600 | 71 107 | 26 112 *(mdhd wins)* | 278 850.5 | 278 850 |
| `prog.mp4` audio | 17 616 | 89 224 | 69 655.06 | 69 655 |
| `frag.mp4` audio | 17 616 | 91 728 | 67 753.86 | **67 754** |
| `frag2.mp4` audio | 26 347 | 134 138 | 69 295.96 | **69 296** |

Two facts fall out of that table and both are reproduced:

* The **sample-table path truncates** and the **fragment path rounds to
  nearest**. They are different code paths in the reference and they round
  differently; `track::bit_rate` takes the mode as an argument rather than
  pretending otherwise.
* `esds` and `btrt` are **ignored**. Patching `esds` `avgBitrate` to 12 345 and
  `btrt` `maxBitrate` to 999 999 changed nothing. The plans assume `esds`
  supplies the audio bit rate; measured, it does not.

#### `start_pts` and the timestamp shift

> `pts = dts_raw + ctts + shift`, `dts = dts_raw + dts_shift + shift`,
> where `shift = empty_offset − max(media_time, min PTS)` when there is an edit
> list, and `0` when there is not.

`dts_shift` is `min(0, min(ctts))`, or `cslg` when present — a **D17 deviation**
reproduced from the reference, which subtracts from decode times where ISO/IEC
14496-12 §8.6.1.4 adds to composition times. `vaco-format-isom` measured it and
its doc file says not to "correct" it; that stands.

The `max(media_time, min PTS)` is the part no specification states and the part
plan 18's rule MP4-T1 got wrong. Measured by patching one file's
`elst.media_time` to 0, 512, 1024 and 2048:

| `media_time` | first packet |
|---:|---|
| 0 | `pts=0 dts=-1024` |
| 512 | `pts=0 dts=-1024` |
| 1024 | `pts=0 dts=-1024` |
| 2048 | `pts=-1024 dts=-2048`, `flags=KD_` |

1024 is that track's minimum raw presentation time, and the shift is
`-max(media_time, 1024)`. MP4-T1's "max of the `ctts` offsets in the first
`delay+1` samples" would give 2048 for this file, which matches none of the four
rows.

`start_pts` is then the edit list's leading **empty-edit** offset when there is
an edit list — not the first packet's PTS, which is `-1024` on `prog.mp4`'s
audio while `start_pts` reads `0` — and the first sample's presentation time
when there is not.

#### Trimming

A sample whose presented time ends at or before the edit start is emitted with
`PacketFlags::DISCARD`; an **audio** sample additionally carries
`PacketSideData::SkipSamples`, in time-base ticks, which for an MP4 audio track
are samples because its time base is `1/sample_rate`. Both are byte-visible in
`-show_packets` and both are reproduced exactly on `prog.mp4` and `delay.mp4`.

#### Zero-duration samples — a subtitle rule, not a general one

A sample whose `stts` delta is `0` is dropped from the packet stream **only on
a subtitle track**. That is the trailing "clear the subtitle" entry many
`mov_text` writers append after the last real cue: measured on a real
`-c:s mov_text` file, `ffprobe -show_packets` on the reference never surfaces
it, and it is not a zero-*size* sample either (its `stsz` entry is a real 2
bytes, `mov_text`'s big-endian `u16` zero-length string), so a size-based rule
would miss it.

Gating on duration alone was measured **wrong** everywhere else. On a video
track whose final `stts` run is `(1, 0)` — the shape this repository's own MP4
muxer wrote for the last sample of every progressive file until that muxer was
fixed — `ffprobe -count_packets` reports all 20 samples, substituting a
`duration=100` for the declared `0`; the unconditional skip reported 19 and
silently deleted the last frame. Regression:
`tests/demux.rs::a_trailing_zero_duration_video_sample_is_still_a_packet`
beside the `mov_text` one it mirrors.

The skip happens in `next_packet`, not when the sample table is first read, so
seeking and duration accounting still see every table entry.

#### `r_frame_rate` and `avg_frame_rate`

> `r_frame_rate = timescale / most common stts delta`,
> `avg_frame_rate = sample count × timescale / Σ sample durations`

Measured on a file whose `stts` is `[(9, 60), (1, 20)]` at timescale 600:
`r_frame_rate=10/1` — the *most common* delta, not the smallest, which would
have given 30/1 — and `avg_frame_rate=75/7`. Note that `avg_frame_rate` divides
by the raw `stts` total while `duration_ts` and `bit_rate` divide by the media
limit: patching `mdhd.duration` to 20 000 on a track whose `stts` totals 25 600
moved the other two and left `avg_frame_rate` at 25/1.

Both are computed over a bounded prefix (`ANALYSE_SAMPLES`, 4096), as the
reference's own `fpsprobesize` does.

#### `sample_aspect_ratio`

`pasp` wins. Failing that, a `tkhd` whose display size disagrees with the sample
entry's coded size states the ratio — a `tkhd` width of 320 over a coded width
of 160 printed `2:1`. When the two agree, **nothing is set here** and the value
comes from the elementary stream, which is why an unmodified file still prints
`1:1` (from the H.264 VUI) rather than `N/A`.

#### Packet order — rule MP4-O1, corrected against the binary

> While two tracks' decode times are within one second of each other, emit in
> **file order**. Outside that window, decode time decides.

Plan 18 has it the other way round ("smallest DTS, ties broken by file offset").
The discriminating file is `frag.mp4`, whose first `moof` holds thirteen video
samples spanning 0.56 s and twenty-two audio samples starting at 0.000000. Pure
DTS order interleaves them; the reference emits all thirteen video packets
first, because the audio data sits later in the `mdat`. Both orders are
monotonic per track, so both are "correct" — but `-show_packets` prints the
order, so only one of them matches.

A cover-art packet has no decode time at all and is emitted **first**, which is
what the reference's pre-loaded `attached_pic` amounts to.

#### Container-level fields

`format.duration` is the largest `start_time + duration` over the streams —
**not** `mvhd.duration`, which was patched to 5 s and changed nothing.
`format.start_time` is the **minimum** over the streams, which settles plan 18's
VERIFY-T2 (`delay.mp4` has video at 0.52 and audio at 0.0 and prints 0.000000).
`format.bit_rate` is `file size × 8 / duration`, truncated.

#### Metadata

| Source | Handling |
|---|---|
| `ftyp` | `major_brand`, `minor_version`, `compatible_brands`, printed first |
| `udta ▸ meta ▸ ilst` | the iTunes four-character keys, mapped to canonical names; `trkn`/`disk` are binary and become `n/total`; `gnre` indexes the ID3v1 genre table; `----` freeform atoms use their `name` verbatim |
| `udta ▸ meta ▸ keys` + `ilst` | reverse-DNS keys, mapped where a canonical name exists and passed through otherwise |
| `udta ▸ ©swr` and friends | **`QuickTime` string atoms, which are not full boxes**: a 16-bit length, a 16-bit language, then the text. A `.mov` written by `ffmpeg` carries its writing application here and nowhere else, and reading it as a full box loses it |
| 3GPP `udta` boxes (`titl auth perf gnre dscp albm cprt yrrc kywd`) | full boxes: language then a null-terminated string |
| `udta ▸ chpl` | Nero chapters, 100 ns units |
| `hdlr` name | the `handler_name` stream tag |
| `mdhd` language / `elng` | the `language` stream tag — **omitted entirely** for a Macintosh language code, measured on a `.mov` whose `mdhd` language is `0x7FFF` |
| `stsd` `compressorname` | the `encoder` stream tag. `prog.mp4` prints `TAG:encoder=Lavc62.28.100 libx264`, and those bytes are the sample entry's compressor name |
| sample entry `vendor` | the `vendor_id` stream tag, on `.mov` only in practice. `vaco-format-isom` does not expose the field, so it is read locally |

#### Cover art

`udta ▸ meta ▸ ilst ▸ covr` becomes a stream with `Disposition::ATTACHED_PIC`,
`codec_tag = 0x00000000` (which the reference prints as `[0][0][0][0]`), a
time base of `1/90000`, the container's duration, and one packet with no
timestamps. Its `width`, `height` and `sample_aspect_ratio` come from the image
itself and are therefore the bitstream parser's to supply, not this crate's.

### Fragmented files

`mvex` makes a file fragmented. Every `moof` payload is read into memory —
bounded by `MAX_FRAGMENTS` (131 072) and the caller's `Budget` — and each track
gets a list of `FragEntry` rows: which fragment, which `traf`, the fragment's
first decode time, and how many samples it declares. Resolving samples from a
`traf` is cheap (borrowed tables, no summaries), so fragments are re-parsed per
refill without the geometric-batch machinery the sample table needs.

A `trun` may declare samples while carrying no per-sample entry bytes at all:
every duration, size and flag then comes from `tfhd`/`trex`. ffmpeg 9.0.1 uses
that legal zero-stride shape for `+frag_every_frame` AAC. The box layer honors
the declared count while capping the whole `traf` at
`MAX_SAMPLES_PER_TRAF` (1,048,576); this keeps default-only runs reachable
without letting a four-byte count drive an unbounded walk. The demuxer reuses
that same cap rather than maintaining another value.

`tfdt` supplies the decode time when present (`-use_tfdt`, default on);
otherwise the running total carries over. A source that cannot seek pulls one
`moof` at a time and **buffers the `mdat` that follows it**, because a
fragment's samples are emitted in decode order and therefore not read in file
order.

A non-seekable source starts every fragmented track's `FragEntry` list empty —
`collect_fragments` is a no-op without a seekable source, by design, so there
is nothing to list yet — and grows it one `moof` at a time as `refill` pulls
them. Marking a reader `finished` just because that starting list is empty
made every fragmented track on a pipe permanently empty: `ensure_head` checks
`finished` *before* ever calling `refill`, so the pull-one-more-`moof` retry
loop that exists for exactly this case never ran. Fixed by only drawing that
conclusion when `collect_fragments` already ran to completion — i.e., the
source is seekable — where an empty list really does mean "no fragments
anywhere", not "haven't looked yet". Caught by
`a_non_seekable_source_gets_no_fast_path_but_still_demuxes` in
`tests/fragmented.rs`, which is a plain two-fragment file with no `sidx`/`mfra`
involved at all — this was not a fast-path bug, it predates this pass.

### `sidx` and `mfra`

Both box-layer structures (`frag::SegmentIndex`, `frag::TrackFragmentRandomAccess`)
are now read. `sidx` boxes are collected — there can be more than one; a
`+dash` file writes one immediately before *each* `moof`, not one whole-file
index the way this crate's own muxer does (measured against `ffmpeg 8.1
-movflags +frag_keyframe+empty_moov+dash`; see `vaco-mux-mp4`'s doc file for
the muxer's side of that difference) — and exposed via
`Mp4Demuxer::segment_index`. Nothing in this crate's own seek path consults
them yet; a caller that wants DASH-style subsegment addressing can already
read `SegmentIndex::subsegments`.

`mfra` is read once, from the file's trailer, at open: `mfro`'s own fixed
sixteen bytes state `mfra`'s size, which is the one place ISO/IEC 14496-12
lets a random-access structure be found without scanning anything
(`Mp4Demuxer::read_mfra_trailer`). Absent, truncated or malformed trailers all
resolve to "no fast path" rather than an error — a demuxer that can only seek
by scanning is still a working demuxer. `Mp4Demuxer::fragment_random_access`
exposes the parsed tables.

### Seeking

Progressive files seek through the sample tables directly: invert the edit
shift, `sample_at_dts`, then walk back to the sync sample at or before it, then
place every other track at the instant the reference landed on.
`SeekStrategy::choose` is consulted so that the byte path reports the right
error, but the timestamp path never leaves this crate — `FormatFlags` therefore
carries neither `GENERIC_INDEX` nor `NOBINSEARCH`.

Fragmented files do it in **two** steps, and the second is the one that is easy
to leave out: pick the `moof` whose first sample is at or before the target,
*then* walk that fragment for the last sync sample at or before it. Stopping at
the fragment boundary lands a whole fragment early on any file whose fragments
are longer than its keyframe interval. Verified against
`ffprobe -read_intervals` at five targets on `frag.mp4` and three on
`frag2.mp4`, byte-identical.

Fragment start times are **scanned, not bisected**, and every entry is examined
rather than stopping at the first one past the target: `tfdt` is written by the
file, so a corrupt one can make the starts non-monotonic and an early exit would
then land somewhere arbitrary — this is still the fallback, and it is still
what a `tfra`-free file gets.

When `mfra` was readable at open, `place_fragment` tries it first:
`TrackFragmentRandomAccess::at_or_before` resolves a target time straight to a
`moof_offset` in one step, which is looked up in the (offset-sorted)
`fragments` list by binary search rather than the linear scan above. A miss —
no `tfra` for this track, a target before its first entry, or an offset this
demuxer has not collected — falls through to the scan unconditionally, so a
wrong or absent `tfra` costs only the speedup, never correctness. Checked both
ways: `tests/fragmented.rs`'s `fast_path_and_fallback_always_agree` proptest
and `seeking_agrees_whether_or_not_mfra_is_present` build the identical
fragment layout with and without a trailer and assert every seek target in
range lands on the same sample regardless of which path answered; the unit
test `tfra_and_the_fallback_scan_agree_on_every_landing` in `lib.rs` checks the
same thing on one instance by clearing `tfra` mid-test, which is the version
that can also assert on the *code path taken*, not just the outcome.

A `moof_offset` beyond every fragment `collect_fragments` was allowed to keep
(`MAX_FRAGMENTS`, 131072) is fetched directly rather than triggering a
rescan — `fetch_fragment_at` seeks straight to it, reads that one `moof`, and
appends it. This only ever *appends*: `fragments` is kept in ascending
file-offset order everywhere else in this crate, and `FragEntry::fragment` is
an index into it that is handed out once and never renumbered, so accepting an
offset that would have to be inserted into the middle would silently
invalidate every entry recorded past the insertion point. `mfra` naming an
exact offset is what makes the append safe: there is nothing to guess at.
Verified with the unit test `a_seek_past_the_collected_tail_is_fetched_via_tfra`,
which truncates a real four-fragment file's `fragments` list down to one
fragment to stand in for hitting the cap — building an actual
`MAX_FRAGMENTS`-sized fixture is not practical, but the fetch primitive itself
does not care how the tail got short.

### Bounding a uniform `stsz` — the gap the box layer left

`vaco-format-isom` clamps every declared count against its box payload, with one
exception it documents: a **uniform** `stsz` has no per-sample payload, so
twelve bytes can legally declare `sample_count = 0xFFFF_FFFF`. Nothing allocates
for it, but iterating it is a denial of service on a seventy-byte file, and the
crate says explicitly that bounding it is the demuxer's job.

`read::sample_limit` closes it with an argument rather than a magic number:

> Distinct samples of one track occupy disjoint byte ranges and a sample
> occupies at least one byte, so a file of `n` bytes holds at most `n` samples.

`sample_limit(declared, source_size) = min(declared, size + 1, 1 << 24)`. The
`1 << 24` backstop only applies when the source cannot state its own size — a
pipe — where reaching it requires actually reading that many bytes.
`nb_frames` still reports the **declared** count, because that is what the
reference prints. The same bound applies per `traf`, capped at `1 << 20`.

---

## How to change it

* **Adding a codec.** Nothing here. `stsd::sample_entry_codec` and
  `esds::object_type_codec` in `vaco-format-isom` are the tables, and both are
  waiting on `CodecId` to grow past its thirteen-variant stub — see *Wanted
  from other crates*.
* **Adding a metadata key.** `meta::ILST_KEYS`, `meta::QUICKTIME_UDTA_KEYS`,
  `meta::THREEGPP_KEYS` or `meta::keys_name`, whichever namespace it belongs to.
  All four are flat tables and adding a row cannot break an existing parse.
* **Changing a reported number.** Every one of them is a conformance-matrix
  change, not an internal one: `duration_ts`, `bit_rate`, `start_pts`,
  `r_frame_rate`, `avg_frame_rate` and the packet order are all printed by
  `vaco-probe`. Re-measure with the commands above before and after.
* **Changing the batch policy.** `read::BATCH_MIN`, `read::BATCH_MAX`,
  `read::FRAG_BATCH`. Raising `BATCH_MAX` costs 48 bytes per queued sample per
  track and reduces the number of re-parses; lowering it does the reverse. A
  *fixed* batch is the one thing not to do — it makes total work quadratic in
  the sample count.

### Gotchas

* **`finished` and `blocked` are different states.** A reader is `finished` when
  it has run out of samples and `blocked` when the track is unreadable at all —
  today, a `dref` pointing at another file. A seek clears the first and must not
  clear the second; conflating them let a seek resurrect a track a straight read
  had refused. The `dem_mp4` fuzz target found it as *"a seek produced a packet
  a straight read never did"*.
* **An absent `dref` is not an external reference.** `DataReferences::default()`
  reports `all_self_contained = false` because it has seen nothing, and a
  `trak` with no `dinf` at all is perfectly ordinary. Only a *declared* external
  entry is refused.
* **A refill that produces nothing is still progress.** A batch whose samples
  all lie outside the file yields no packets and still advances the cursor.
  Reporting it as a stall turns a truncated file into a spurious "no progress"
  error thousands of samples before the end of its table.
* **`Eof` must be sticky.** The frozen `Demuxer` trait does not require it;
  `vaco-format-core`'s doc file says it should, and every demuxer needs the flag
  until it does.
* **Two timescales.** `elst.segment_duration` is in the *movie* timescale and
  `elst.media_time` in the *media* timescale, in the same record. Every
  conversion goes through `isom::edit::rescale_movie_to_media`, which takes both
  explicitly.
* **A declared box size is a claim.** `read_payload_incremental` is the only way
  a box payload enters memory here, and it grows as bytes arrive rather than
  reserving what the header asked for.
  `vaco-format-isom`'s `TopLevelScanner::read_payload` reserves up front, which
  is right for a caller that has already bounded the claim against something —
  and on a pipe there is nothing to bound it against. Do not reintroduce it.
* **Packet payloads are charged to a separate budget** and released immediately.
  The demuxer retains no packet, so a cumulative cap would refuse to read a file
  larger than the cap — which is what `vacoraw` does today. `max_alloc_single`
  still applies, which is the check that matters for a declared length.
* **An empty `FragEntry` list means two different things.** On a seekable
  source it means the track truly has no fragments (`collect_fragments` already
  ran). On one that cannot seek it means "nothing pulled yet" — marking the
  reader `finished` in that case, which used to happen unconditionally, made
  every fragmented track on a pipe permanently empty. See *Fragmented files*.
* **`fragments` must stay sorted by file offset, always.** `fetch_fragment_at`
  (the `mfra`-beyond-`MAX_FRAGMENTS` fetch) only ever appends past the current
  tail for exactly this reason: `FragEntry::fragment` is an index into this
  list handed out once and never renumbered, so a mid-list insertion would
  silently invalidate every later entry. Do not add a second way to grow
  `fragments` without preserving this.
* **`hvcC`'s `lengthSizeMinusOne` is read directly in `track.rs`, not through a
  codec parser.** `avcC`'s equivalent field reaches `nal_length_size` via
  `vaco-parse-h264`'s own `set_extradata`, but `vaco-parse-hevc` deliberately
  never reports `nal_length_size` on its own `CodecParameters` — the reference
  genuinely never prints `is_avc`/`nal_length_size` for an HEVC stream, in any
  container, so that crate's `None` is correct for *display*. `vaco-mux-raw`
  and `vaco-mux-mpegts` read the same field to decide whether to Annex-B-convert
  a copied stream, though, and need the true answer regardless of what
  `vaco-probe` shows — so `track::hvcc_length_size` parses the 22-byte
  `HEVCDecoderConfigurationRecord` header itself (byte 21, low two bits, exactly
  where `avcC`'s field sits in its own shorter header) and sets
  `VideoParameters::nal_length_size` before any codec parser ever runs.
  `CodecParameters::fill_from` only fills a field that is still `None`, so this
  container-stated value is never overwritten.

---

## Configuration

`Mp4Options`, read from the reference with `ffmpeg -h demuxer=mov`. The
reference exposes 21 options; these are the ones this crate acts on.

| Option | Default | Effect |
|---|---|---|
| `ignore_editlist` | `false` | report raw media timestamps: no trim, no delay, no discard |
| `ignore_chapters` | `false` | skip `chpl` |
| `use_tfdt` | `true` | trust `tfdt` for a fragment's base decode time |
| `enable_drefs` | `false` | **refused even when set** — following an external `dref` is a file-system read triggered by file content |
| `interleaved_read` | `true` | emit one DTS/position-ordered stream rather than draining track by track |
| `seek_streams_individually` | `true` | after a seek, place every track at its own nearest sync sample |
| `max_stts_delta` | `4294487295` | accepted and recorded; no `stts` delta is currently rejected |
| `decryption_key` | none | fallback AES-128 key for literal `cenc` tracks |
| `decryption_keys` | empty | AES-128 keys selected by each track's `tenc.default_KID` |

Not implemented, and named so their absence is a decision rather than an
oversight: `use_absolute_path`, `advanced_editlist`, `use_mfra_for`,
`export_all`, `export_xmp`, `activation_bytes`, `audible_key`, `audible_iv`,
`audible_fixed_key`.

`use_mfra_for` is a different feature than it sounds like next to this crate's
own `mfra` reading (below): the reference uses it to *correct a fragment's
decode/presentation time* from `tfra`'s recorded time when `tfdt` is missing
or distrusted. This crate reads `mfra` only as a seek index — `tfdt`/the
running total still decide every sample's actual timestamp, unconditionally.
The two are independent: implementing one does not imply the other.

Constants:

| Constant | Value | Bounds |
|---|---:|---|
| `MAX_MOOV_BYTES` | 16 MiB | the resident `moov`, and any one `moof` |
| `MAX_FTYP_BYTES` | 64 KiB | the `ftyp` payload |
| `MAX_BUFFERED_MDAT_BYTES` | 64 MiB | an `mdat` buffered for a non-seekable source |
| `MAX_FRAGMENTS` | 131 072 | `moof` boxes collected |
| `MAX_TOP_LEVEL_BOXES` | 1 048 576 | boxes inspected while scanning |
| `MAX_SIDX_BOXES` | 4096 | `sidx` boxes collected between `moov` and the first `moof` |
| `MAX_SIDX_BYTES` | 1 MiB | one `sidx` payload (its own reference count is 16 bits, so this is generous) |
| `ANALYSE_SAMPLES` | 4096 | samples read to estimate a frame rate or the minimum PTS |
| `INTERLEAVE_WINDOW_US` | 1 000 000 | how far apart two tracks may be before DTS beats file order |
| `read::BATCH_MIN` / `BATCH_MAX` | 4096 / 131 072 | queued samples per track |
| `read::MAX_SAMPLES_PER_TRACK` | 16 777 216 | backstop when the source cannot state its size |
| `read::MAX_SAMPLES_PER_FRAGMENT` | 1 048 576 | samples walked per `traf` |
| `meta::MAX_VALUE_BYTES` | 64 KiB | one metadata value |
| `meta::MAX_ENTRIES` | 4096 | `ilst`/`keys`/`chpl` entries |

---

## Dependencies

| Crate | Why |
|---|---|
| `vaco-format-isom` | the entire box layer: boxes, sample tables, edit lists, fragments, `stsd`, `esds`, language, probing |
| `vaco-format-core` | `Demuxer`, `Stream`, `SeekTarget`, `PacketIndex`, `FormatFlags`, `ProbeData` |
| `vaco-codec-core` | `CodecParameters`, `CodecId` |
| `vaco-core` | `Error`, `Rational`, `Timestamp`, exact rescaling |
| `vaco-io` | `IoContext`, `MediaSource` |
| `vaco-packet` | `Packet`, `PacketFlags`, `PacketSideData` |
| `vaco-limits` | `Budget`, `ProgressGuard` |
| `vaco-bitstream` | `ByteReader` for the handful of fields read locally |

Dev only: `proptest`. No external dependency, nothing wrapped, so D11 does not
apply and there is no fidelity grade.

---

## Tests, benchmarks and fuzzing

```sh
cargo test -p vaco-demux-mp4 --locked
just fuzz dem_mp4
just fuzz dem_mp4_chunked
cargo run -p vaco-demux-mp4 --example mp4dump -- file.mp4 packets
```

49 tests: 12 unit (in `lib.rs`, `read.rs`, `track.rs` — two of the twelve are
the `tfra` fast-path/fallback-agreement and past-the-collected-tail-fetch
checks, which need private field access an integration test cannot reach), 22
named integration cases in `tests/demux.rs`, 8 in `tests/fragmented.rs`
(`sidx`/`mfra`, including a `proptest` generalising the fast-path/fallback
agreement over random fragment layouts and seek targets), 6 property tests in
`tests/properties.rs`, 1 doctest. Every fixture is built box by box through
`vaco_format_isom::build` (progressive) or by hand with `bx`/`fullbx`
(fragmented, in `tests/common/mod.rs`'s `frag_moov`/`frag_unit`/`mfra`/`sidx`),
so the suite needs no media files and no reference binary.

Two fuzz targets, because the crate has two distinct surfaces:

| Target | Input | Looking for |
|---|---|---|
| `dem_mp4` | raw bytes = a whole file | non-termination, packets outside the file, non-monotonic decode times, unstable `Eof`, seeks that land wrongly |
| `dem_mp4_chunked` | structured: chunk size, seekability, options, bytes | a chunk size that changes the packets, and the forward-only path |

`dem_mp4` asserts against the file's own packet sequence rather than against an
absolute rule, and that is the finding worth carrying forward.

### What the fuzzer found

Five findings across six runs. **Two were in the crate and three were in the
oracle**, which is itself the result:

| # | What | Where |
|---|---|---|
| 1 | A seek resurrected a track whose `dref` points at another file: `place` cleared `finished` without knowing the track was *refused* rather than *exhausted*. Fixed by splitting the two states; the `blocked` flag exists for this. | **the crate** |
| 2 | "The first packet after a backward seek has `dts <= target`" is false when the track's edit list starts after the target. | the oracle |
| 3 | …and false again for a *non-reference* stream, which is placed relative to where the reference landed, not relative to the target. | the oracle |
| 4 | …and false a third time on a truncated file, where the samples between the landing point and the first *readable* one are dropped, so a correctly-placed seek still reports a later packet. | the oracle |
| 5 | **An `oom-` artifact, which exits 0.** A fifteen-byte input on a source that could not state its own size declared a huge `ftyp` and got a 512 MiB allocation — the classic declared-length amplification (plan 13 §2.2.2 rule 3), found in twenty-three executions. Fixed by reading every box payload **two-phase**: `read_payload_incremental` grows the buffer as the bytes actually arrive, so a claim nothing bounds costs nothing. | **the crate** |

The rule that came out of it: **compare a seek against the file's own packet
sequence, not against an absolute claim about timestamps.** "Every packet a seek
produces appears in the sequence a straight read produces" is both stronger than
the timestamp rule and actually true — and it is the form that caught finding 1,
which three runs of the weaker rule had walked straight past.

A further input was a *fragmented* file whose sample durations had been mutated so
that a fragment overran the next fragment's `tfdt`, producing a genuinely
backwards decode time. That one is not a bug: `tfdt` places each fragment
absolutely, the file states the jump, and repairing it is `vaco-format-core`'s
job (rule R22) on the far side of a boundary this crate does not cross. The
monotonicity assertion is therefore scoped to progressive files, where `stts`
deltas are unsigned and monotonicity is guaranteed by construction.

---

## Common Encryption — reported always, decrypted given a key

**2026-08-23.** `sinf ▸ schm` and `sinf ▸ schi ▸ tenc` are read through
[`vaco_format_isom::stsd::SampleEntry::cenc`], which returns
[`vaco_format_isom::cenc::CencInfo`]. When a track is protected, its `Stream`
gets `encryption_scheme` (e.g. `cenc`), `encryption_scheme_version` (the raw
decimal 16.16 `schm` version — `65536` for the measured `1.0`),
`encryption_key_id` (the `default_KID`, lower-case hex),
`encryption_is_protected` (the decimal `default_isProtected`, `1` for the
measured CENC fixture), and `encryption_iv_size` (the decimal
`default_Per_Sample_IV_Size`) tags —
`codec_name` already reads as the *original* codec via `effective_format` — and `pssh` boxes become
container-level `encryption_system_id` tags. A version-1 `pssh` additionally
emits one `encryption_key_id` tag for every declared KID, in declaration order;
the same helper handles `pssh` under `moov` and top-level `pssh` beside `moof`.
The opaque DRM-system `Data` stays uninterpreted.

`tenc`/`seig` constant IVs retain their declared 8- or 16-byte size in the box
layer and reject truncated tails, but are not yet tags: the reference muxer
fixture used here emits only per-sample IVs. Reporting a hand-mutated encrypted
sample as though it proved the constant-IV packet semantics would be misleading;
add exact `encryption_constant_iv_size` and `encryption_constant_iv` tags only
with a standards-conforming fixture that exercises them.

Measured: `ffprobe 8.1` on such a file surfaces **no** encryption tag at all,
and `ffmpeg -i` decodes the still-encrypted bytes into visibly corrupt frames
without refusing the file. Reporting the scheme is therefore new behaviour
relative to the reference, not a reproduction of it.

**2026-08-28: decryption, given a caller-supplied key.** `Mp4Options::decryption_key`
(one AES-128 key, matching the reference's own `-decryption_key`) turns on
real decryption for a protected track: `SampleEntry::tenc`'s
`per_sample_iv_size` plus `senc`'s per-sample IV records give every sample's
real IV, and `read::Decryptor::decrypt` applies full-sample AES-128-CTR
(`vaco-crypto`) in place before the packet is handed back. Without a key, or
with `per_sample_iv_size == 0` (`constant_iv`, not implemented), refusal still
applies: `reader.encryption_error` and `reader.decrypt` are mutually exclusive,
and `ensure_head` returns that named `Error::Unsupported` before packet bytes
are read.

This is not merely self-consistent with this crate's own muxer: a file
`vaco-mux-mp4` wrote with `-encryption_scheme cenc-aes-ctr` was decrypted by
both this crate (byte-identical to the plaintext muxed in) *and* a real
`ffmpeg 8.1 -decryption_key <hex>` — see `vaco-mux-mp4`'s doc file's
*Common Encryption* section for that cross-check.

**2026-09-03: subsample encryption, against ffmpeg's own encryptor.**
`ffmpeg -encryption_scheme cenc-aes-ctr` writes *subsample* encryption for
H.264 (`senc` flags `0x2`, one `(BytesOfClearData, BytesOfProtectedData)`
pair per NAL unit, 8-byte IVs) and full-sample encryption for AAC, so the
"full-sample only" decryptor above could decrypt ffmpeg's audio and not
its video. `read::Decryptor::parse` now pre-resolves every `senc` record
(IV plus subsample table) once at track build, and `decrypt` treats a
sample's protected ranges as **one** continuous AES-CTR stream (ISO/IEC
23001-7 §9.5 — the block counter is not reset between subsamples and a
partial block carries over), gathering, decrypting once and scattering back.
Measured, `tests/cenc_ffmpeg.rs`: a clear H.264+AAC file stream-copied by
ffmpeg into a `cenc` file decrypts back to the clear file's packets byte
for byte, all 10 video and 20 audio packets; the same key through
`ffmpeg -decryption_key` decodes that file to pixels and samples identical
to the clear file's, which is the cross-check that the *file* is what the
test says it is.

Only an exact `schm.scheme_type == "cenc"` enters that AES-CTR path.
`cens` requires patterned CTR, while `cbc1` and `cbcs` require CBC; all three
remain reported by `encryption_scheme` and refused even when the caller gives
a key. This gate is deliberate: the old path checked only that *some* CENC
scheme, `tenc`, key and `senc` existed, so changing a real ffmpeg-encrypted
fixture's `schm` from `cenc` to `cbcs` still returned a packet after applying
the wrong cipher. `tests/cenc_ffmpeg.rs` now makes that single-box change and
checks both the `cbcs` metadata value and the named `Error::Unsupported`
refusal.

**2026-09-04: fragmented `senc`.** ISO/IEC 23001-7 §7.2 places each
sample-encryption table inside its `moof ▸ traf`. `TrackFragment` retains that
box, and every fragment refill replaces the track's active `Decryptor` records
before it queues samples. Packet indices are zero-based within the whole
`traf` (including refills larger than one batch), so a seek clears the queue
and the same refill path selects both the destination fragment's `senc` and
the matching record. A missing box, a `sample_count` mismatch, truncated IV
records, or a subsample range outside its packet is refused before ciphertext
can be returned.

Measured against `ffmpeg 9.0.1`: an AAC stream encoded once and stream-copied
into clear and `cenc-aes-ctr` files with
`+empty_moov+frag_every_frame` produced one nested `traf ▸ senc` per fragment.
All packet stream indices, PTS, DTS, sizes and payload bytes matched after
decryption, both through EOF and after a backward seek into the middle.

**2026-09-04: binary reachability.** Input-scoped `vaco -decryption_key
<32 hex digits> -i enc.mp4` now reaches the same typed `Mp4Options` path.
The registry still chooses the demuxer; only when that chosen descriptor is
MP4/MOV does the CLI use `Mp4Demuxer::open` with the supplied key. A malformed
key, output-scoped key, or key attached to another demuxer is refused instead
of silently ignored. This is intentionally narrower than general demuxer
private-option plumbing: every other `Mp4Options` field (`-ignore_editlist`,
`-use_tfdt`, …) remains library-only.

Measured against `ffmpeg 9.0.1`, which wrote clear and `cenc-aes-ctr` AAC
stream copies from one encoding: both binaries' `framemd5` output covered 20
packets. Each binary's clear and keyed-encrypted `framemd5` output was
byte-identical, and Vaco's 20 `(duration, size, MD5)` packet tuples matched the
reference exactly. The full lines deliberately are not compared across
binaries: the existing edit-list convention shifts Vaco's first AAC DTS from
`-1024` to `0`, which is independent of encryption. The integration test
invokes the public CLI entry point and names real output files, so the key must
traverse argument grouping, probing, MP4 open, packet decryption, stream copy,
and the hash muxer.

**2026-09-04: keys selected by track KID.** Input-scoped
`-decryption_keys KID=KEY[:KID=KEY...]` decodes each identifier and AES-128
key from exactly 32 hexadecimal digits. The demuxer selects the last matching
entry for the protected track's `tenc.default_KID`, with `decryption_key` as a
fallback when no entry matches. The lookup occurs once at the shared track
decryptor construction point, so progressive and fragmented `senc` paths use
the same selected key.

The CLI integration test asks `ffmpeg 9.0.1` to write two 20-packet AAC files
with distinct KID/key pairs, one progressive and one fragmented at every
sample. Both ffmpeg and Vaco receive the same two-entry dictionary; for each
file, all 20 decrypted `(duration, size, MD5)` tuples match the clear reference.
This proves the identifier value selects the key in both container layouts,
not merely that a dictionary-shaped string reached an existing single-key
path.

**2026-09-04: version-1 `seig` key rotation.** ISO/IEC 23001-7 §6 associates
each protected sample with either `tenc.default_KID` or the KID in its mapped
`CencSampleEncryptionInformationGroupEntry`. The box layer retains
`sgpd(seig)` descriptions and compact `sbgp` runs from both `stbl` and `traf`;
the decryptor resolves a key and IV size for every `senc` record before it can
return ciphertext. Track-level descriptions use ordinary 1-based indices;
fragment-local descriptions use ISO/IEC 14496-12 §8.9.4's indices beginning at
`0x10001`. A fragment refill and a seek both replace the active mapping through
the same path.

The integration fixture encodes AAC once, then asks ffmpeg 9.0.1 to encrypt two
layout-identical copies under different KID/key pairs. The progressive case
keeps the first key's ciphertext for ten samples and substitutes the second
key's ciphertext for ten, with a two-run `sbgp`. The fragmented case alternates
the two real encrypted `moof`/`mdat` pairs and adds a fragment-local
`sgpd`/`sbgp` mapping to the second-key fragments. Before assembly,
`ffmpeg -decryption_keys` decrypts each single-key source to the clear file's
exact 20-packet `framemd5` in both layouts. Vaco then decrypts the combined
two-key files: packet count, PTS, DTS, sizes and payloads match the clear demux,
including a backward seek into a second-key fragment.

This slice deliberately supports the deployed version-1 `sgpd` grammar only.
Version-0/2 descriptions, duplicate or malformed maps, out-of-range indices,
missing mapped keys, clear `seig` overrides, pattern fields and constant-IV
entries all produce named refusals. `cens`, `cbc1` and `cbcs` likewise remain
scheme-named refusals even when keys exist, so none can enter the literal
`cenc` AES-CTR path.

**Not implemented, named explicitly**: `cbcs`/`cens`/`cbc1` decryption and
their applicable constant-IV/pattern modes; version-0/2 `sgpd(seig)` and clear
sample groups. Top-level fragmented-file `pssh` is collected. Reporting is
deliberately *more* than
the reference:
`ffprobe 9.0.1` prints no encryption field at all for a `cenc` file, and
`ffmpeg -i` without a key decodes the ciphertext into garbage without
complaint; this crate tags the stream (`encryption_scheme`,
`encryption_key_id`, container-level `encryption_system_id`) and refuses to
read it.

## HEIF/AVIF items — a `meta` box instead of a `moov`

**2026-09-03.** A still-image file (`ftyp` brand `avif`/`heic`/`mif1`) has
no `moov`: its pictures are *items* in a top-level `meta` box. `open` now
keeps that `meta` (bounded by `MAX_META_BYTES`) and, when no `moov` follows,
`items::build` reads it (`hdlr` must say `pict`; a QuickTime `mdta` `meta`
still means "no movie"). The box grammar is `vaco-format-isom::heif`'s; this
crate decides what becomes a stream:

* **Every coded item becomes one video stream** — hidden tiles included,
  because a grid's tiles are what actually has to be read — in `iinf`
  order, with `time_base 1/1`, `r_frame_rate`/`avg_frame_rate 1/1`,
  `nb_frames 1`, no duration, `id` = `item_ID`, `title` = `item_name` when
  non-empty, `default` on the `pitm` item, `dependent` on each member of an
  accepted `TileGrid`. Codec parameters come through the *same*
  sample-entry reader tracks use (`track::codec_parameters_with_display`):
  the item's `ipco` properties are re-serialised as the entry's extension
  boxes, because `av1C`/`hvcC`/`colr`/`pasp`/`pixi` are literally the same
  boxes in both worlds. A malformed present `ipma` table, including an index
  beyond its `ipco` property list, a duplicate ID, or a descending ID refuses
  the item file rather than applying a partial property configuration. A
  supported HEIF item FullBox also has to use its declared version and flags:
  `iinf`, `pitm`, `iloc`, and `iref` have no flags; `ipma` permits only
  `large_index`; and `infe` permits only `hidden_item`. A reserved header bit
  is not an extension this demuxer can safely reinterpret. A
  duplicate `iloc` item ID likewise refuses the item file rather than letting
  first-match range resolution select arbitrary item bytes. A present `pitm`
  primary ID must name a declared image item, rather than
  silently leaving all streams non-default. An `iinf` table must contain every
  declared, valid `infe` entry: a truncated catalogue is refused rather than
  treated as its valid prefix. The fixed `infe` fields and `pitm` item ID must
  also be completely present; truncation is not interpreted as a zero ID or
  coding type. An
  ordered association may still name an entity group rather than an `iinf`
  item. Duplicate `dimg` records from one grid are malformed rather than an
  invitation to select the first tile graph. An essential property the resolver cannot
  apply discards only its associated item. Exactly one `ispe` supplies each
  item's size; an item with no `pasp`
  reports `sample_aspect_ratio 1:1` (measured — the reference prints `1:1`
  for an AVIF item where the same codec in a track prints nothing).
* **Every `grid` item becomes a `TileGrid` stream group**, not a stream:
  `dimg` names the tiles in raster order, the descriptor (read from `idat`
  for `construction_method 1`, from the file for 0) gives rows, columns
  and output size; the descriptor version must be `0` and its only permitted
  flag is `large_field` (`0x01`), while the grid's single `ispe` must be
  version `0` with no flags and agree with that output. Unknown descriptor
  fields refuse the group rather than guessing their layout. The
  tiles' `ispe` gives the canvas (`coded_*`) and per-tile offsets. A grid whose
  tile count is not `rows × columns`, whose tiles are not streams or do not
  share one `ispe` size, whose `ispe` disagrees with its descriptor, or whose
  output exceeds its canvas produces **no group** rather than a wrong one, and
  its coded items retain no stale `dependent` membership. An
  associated `clap` property is
  resolved over the grid's reconstructed output using HEIF §6.5.9 and
  ISOBMFF §12.1.4 centre-offset semantics, then folded into the group's
  integer `width`/`height` and `horizontal_offset`/`vertical_offset`. A
  fractional edge or out-of-bounds aperture likewise produces no group, so
  neither the probe nor the CLI can advertise a crop it cannot perform.

An `iinf` table with duplicate `item_ID` values refuses the item file before
the `iinf`, `iloc`, and `ipma` joins can alias one item's properties or bytes
onto another item's stream.
* One packet per stream, `pts 0 dts 0 duration 1`, keyframe, `pos` at the
  first extent; several `iloc` extents are concatenated. A seek re-arms
  the single frame.

**Measured against `ffprobe 9.0.1`**, `-show_streams -show_stream_groups
-show_format -of json` flattened and compared key by key:

| fixture | how made | fields ref / ours | agree |
|---|---|---|---|
| `single.avif` | `ffmpeg -c:v libsvtav1 -f avif`, 128×96 | 58 / 59 | 58 |
| `grid.avif` | 2×2 of 64×64 AV1 tiles, `grid` in `idat` (self-built, see below) | 415 / 423 | 415 |
| `grid_thumb.avif` | the same plus a non-hidden `thmb` item | 461 / 470 | 461 |
| `grid_jpeg.heif` | 2×2 of 64×64 JPEG items (`jpeg`/`mif1`) | 423 / 423 | **423** |

Every field the reference prints, this crate prints with the same value —
stream count and order, `id`, sizes, `pix_fmt`, `r_frame_rate`,
`time_base`, `nb_frames`, `extradata_size`, `disposition.default/dependent`,
`title`, the group's `id`/`nb_streams`/`type`/`coded_*`/`width`/`height`,
every tile's `stream_index`/`tile_horizontal_offset`/`tile_vertical_offset`,
`nb_stream_groups`. The one extra field on the AV1 files is `field_order`
(`progressive`), which the AV1 parser sets through `Discovery` for a track
and an item alike; the reference prints it for the track and not the item.
Left as is — it is a parser fact, not a container one.

**The grid fixtures are self-constructed** (`ffmpeg` cannot write a grid),
which is weaker evidence than a reference-written file *until* the
reference reads them: `ffprobe` reports the expected four tiles and one
`Tile Grid` group, and `ffmpeg -i grid.avif -f rawvideo` decodes each to a
128×128 frame that is **byte-identical** to the four tile decodes laid out
at the group's own offsets — so the files are what they claim, and
ffmpeg's composite is the oracle for ours.

**End to end through the binary** (`vaco -i grid_jpeg.heif -f rawvideo`,
the CLI composing the primary grid — see `docs/app/vaco-cli.md`): the
128×128 output equals the composition of this project's own decodes of
the four JPEG tiles byte for byte (tile placement exact; a transposed or
offset tile would show as whole blocks differing), and differs from
ffmpeg's composite by ±1 on 172 of 24 576 bytes — the same 6-of-6144
±1 difference `vaco -i tile.jpg -f rawvideo` shows against ffmpeg on each
tile alone, i.e. `vaco-codec-jpeg`'s IDCT rounding, not anything in the
item path. AV1 items are reported but not decodable in the default build
(no AV1 decoder is registered), and HEVC's decoder is feature-gated as
patent-encumbered, so the pixel check uses `jpeg` items.

**Clean aperture reachability (2026-09-04).** A real 64×48 JPEG wrapped in a
primary 1×1 grid was decoded twice through `vaco`: without `clap` it wrote
4,608 bytes of `yuv420p`; with a 32×24 centred aperture it wrote 1,152 bytes,
byte-for-byte equal to the exact `(16,12)` planar crop of the first output.
`ffmpeg 9.0.1` accepted the same cropped fixture but wrote 4,608 bytes, so that
version ignores a grid-associated `clap`. Vaco deliberately follows ISO/IEC
23008-12 §6.5.9 here; the black-box result is recorded as a reference
divergence, not copied into the demux contract.

## Deferred

Named so the next author knows what is absent rather than broken:

* ~~**HEIF/AVIF.**~~ Done 2026-09-03 — see *HEIF/AVIF items* below. Still
  absent there, by name: `iovl`/`iden` derived items, `auxl` alpha/depth
  planes as anything but ordinary streams, `irot`/`imir` (parsed as
  properties, not applied), `clap` on coded items, `construction_method 2`, `dref`-external items,
  and a file that has *both* `moov` tracks and `meta` items (an image
  sequence with a primary still) — the `moov` wins and the items are ignored.
* **`sidx` for seeking.** Collected (see *`sidx` and `mfra`* above) but not
  consulted by `place_fragment` — `mfra`, when present, already answers the
  same question more precisely (per-sample, not per-subsegment). A source
  that has `sidx` but no `mfra` — plausible for a DASH segment produced by
  something other than this project's own muxer, which always writes both —
  still falls back to the O(fragments) scan rather than the coarser
  subsegment-level jump `sidx` could offer.
* **Multiple `stsd` entries.** The first is reported; extradata does not switch
  mid-stream through `NewExtradata` side data.
* **`media_rate != 1` edits** and **multi-segment edit decision lists.**
  `EditList::is_simple` is not consulted; the single shift is applied.
  `vaco-format-isom` has `EditList::resolve` and a `Timeline` ready for the
  general case. Both are on plan 18's documented-divergence list already.
* **`cmov`** (zlib-compressed `moov`), **`tapt`**, **hint tracks** and **`uuid`
  extension boxes.** Not attempted.
* **`rICC`/`prof` colour profiles.** `colr`'s CICP path (`nclx`/`nclc`) is read
  and mapped onto `VideoParameters::color`; an embedded ICC profile is
  reported by `colour_type` only (`ColourInfo::primaries` etc. are `None`) and
  never parsed or exposed as a `StreamSideData`.
* **A benchmark.** The re-parse-per-refill policy is argued from
  `vaco-format-isom`'s own measured parse cost rather than from a measurement of
  this crate. A `divan` benchmark over a synthetic 300 000-sample table would
  turn that argument into a number.

## QuickTime chapter tracks — done, with one unmeasured precedence rule

**2026-08-23.** A video (or any) track's `tref ▸ chap` naming a track whose
`stsd` entry is Apple's plain `text` type (not 3GPP `tx3g`, a different sample
shape) is read as a chapter list: each sample is a big-endian length then that
many UTF-8 bytes, and its decode time becomes the chapter's start. Nero `chpl`
wins when both are present — `chpl_chapters_take_precedence_over_a_quicktime_chapter_track`
pins that ordering — but this is an **assumption**, not a measurement: no file
combining both was available this pass, so plan 18's VERIFY-M4 is still
unmeasured, just handled defensibly rather than left undecided. There is no
round-trip partner for this on the mux side yet: `vaco-mux-mp4::meta::build_chapter_tref`
exists but nothing calls it, so the muxer writes only `chpl` today — see
*Wanted from other crates*.

## `colr` and `tmcd` — colour side data and timecodes

**2026-08-28.** `colr ▸ nclx`/`nclc`'s three CICP codes (ISO/IEC 23091-2 —
the same numeric space H.264/HEVC VUI and Matroska's `Colour` element already
use) map onto `VideoParameters::color` via `vaco_color`'s `from_u8`;
`full_range` sets `ColorRange::Full`. Measured against a real
`ffmpeg -movflags write_colr -colorspace bt709` file — see
`vaco_format_isom::stsd`'s `colr_matches_a_real_ffmpeg_nclx_atom`.

A `tmcd` track's fixed fields (`time_scale`/`frame_duration`/
`number_of_frames`/the drop-frame flag) are now exposed by
`vaco_format_isom::stsd::SampleEntry::tmcd`, and this crate turns the track's
one sample (a big-endian frame count) into a `timecode` tag — placed on the
`tmcd` track's own stream *and* propagated to every track whose
`tref ▸ tmcd` names it, matching a real `ffmpeg -timecode 01:00:00:00 .mov`
file where `ffprobe` prints the same `TAG:timecode` on both. Drop-frame uses
`;` before the frame count rather than `:`, the reference's own formatting.

---

## Wanted from other crates

Reported, not worked around (plan 19 §6).

1. **`vaco-mux-mp4`: `meta::build_chapter_tref` is never called.** It builds a
   correct `tref ▸ chap` box, but no code path in `progressive.rs`/
   `fragmented.rs` invokes it or writes an accompanying chapter *track* — the
   muxer's chapter support is `chpl` only. This crate's chapter-track reader
   therefore has no muxer output of its own to round-trip against; the
   `chpl`-wins-when-both-are-present rule above is an assumption because of
   this gap, not despite it.
2. **`vaco-format-isom`: `build::stbl` double-wraps a caller-supplied `stsd`
   that already carries its own box header.** `TrackSpec::stbl.stsd` is
   documented nowhere as to whether it wants a bare fullbox body or a complete
   box, and `avc1_stsd()` (and every fixture built the same way, across two
   crates' test suites) supplies a *complete* `stsd` box via `build::fullbx`,
   which `stbl()` then wraps in a second `bx(b"stsd", ..)`. The result is a
   `stsd` whose real first child is misparsed as a box named by the inner
   entry count's raw bytes — reproduced while writing this pass's chapter-track
   test, which needed `stsd::parse_stsd` to see a real `format`, not a
   coincidence no existing assertion happened to depend on. Worked around
   locally by passing the fullbox *body* instead of calling `fullbx` in the new
   test helpers, which is the shape `stbl()` actually wants; not fixed at the
   source because every existing caller would need re-auditing under this
   pass's time budget, and a wrong fix here is silent (it produces a *different*
   plausible-looking box, not a compile error).
3. **`vaco-format-isom`: an owned, resumable cursor state.** The batch machinery
   in `read` exists entirely because `SampleTable<'a>` borrows and a
   `Box<dyn Demuxer>` is `'static`. Everything the cursor needs to resume is
   four integers — `(index, dts, chunk, within_chunk)` — and exposing them as a
   `Copy` struct plus `SampleTable::resume(state) -> SampleCursor` would let the
   demuxer keep a position across re-parses and drop the queue entirely. This is
   the single highest-value change to that crate for this one.
2. **`vaco-format-isom`: `Track::reported_duration` is wrong** against
   `ffprobe 8.1` in two ways — it uses `mdhd.duration` when there is no edit
   list (measured: the `stts` total wins when it is smaller) and does not clamp
   the edit-list total to the media (measured: it is clamped). The measurements
   are in the table above. The demuxer computes its own and does not call it.
3. **`vaco-format-isom`: the sample entry's `vendor` field is not exposed.**
   `ffprobe` prints it as the `vendor_id` stream tag on `.mov` files, so it is
   an output field; this crate reads it out of the raw `stsd` bytes, which is
   the one place it re-parses a box the box layer already parsed.
4. **`Stream::start_time` for MP4 is the edit list's, not the first PTS.**
   `Discovery::finish` derives `first_pts + initial_padding` — correct where the
   container states nothing, and **not** what MP4 reports. Measured: on
   `prog.mp4` the audio track's first packet is `pts=-1024` and `ffprobe` prints
   `start_pts=0`, which is the edit list's leading empty-edit offset; on
   `delay.mp4` it is 6656 for a track whose first packet is also at 6656. The
   demuxer therefore sets `start_time` itself, and `Discovery` correctly leaves
   it alone — it only fills in what is `None`, which its own doc file records as
   deliberate ("the edit list gives `Stream::start_time` authoritatively, so
   discovery must not overwrite it"). Nothing to change; recorded because a
   later reader may otherwise "fix" the demuxer into disagreeing with the
   reference on four of the four calibration files.
5. **`vaco-format-core`: `Stream` could not hold what `ffprobe` prints —
   **closed 2026-08-22.** `duration_ts`, the `r_frame_rate`/`avg_frame_rate`
   pair and the `tkhd` display matrix used to live in a private `TrackFacts`
   table reached through inherent methods on `Mp4Demuxer`, which
   `DemuxerDesc::open`'s `Box<dyn Demuxer>` made unreachable from `vaco-probe`.
   `Stream` now carries `duration_ts: Option<i64>`, the two rates, and a
   `side_data` list; `TrackFacts` is deleted, and the three accessors with it.

   The rules this crate measured are unchanged and are now simply written where
   a caller can see them: `duration_ts` is
   `min(elst sum, min(mdhd.duration, Σ sample durations))`, `avg_frame_rate`
   divides by the *raw* `stts` total while `duration_ts` and `bit_rate` divide
   by the clamped limit, and `r_frame_rate` is `timescale / most-common delta`.
   A `covr` stream keeps its measured `r_frame_rate=90000/1` with
   `avg_frame_rate=0/0`.

   The display matrix became **side data** rather than a field. The reasoning
   is in `vaco-format-core`'s `sidedata` module: `ffprobe` prints a list whose
   length varies, the eight other members plan 18 §1.1 names would each want
   their own field, and the matrix means the same thing whichever container
   carried it. `track::display_matrix` still returns `Option<[i32; 9]>` and
   still suppresses the identity, which is what the reference does.
6. **`vaco-packet`: `Packet::duration` has no "absent" value.** It is a
   `Duration`, so zero is the only representation of "unknown", and `ffprobe`
   prints `N/A` for the cover-art packet. A printer that maps zero to `N/A`
   reproduces it, but the model cannot tell the two apart.
7. **`vaco-codec-core`: `CodecId` is a thirteen-variant stub.** `mp4v` with an
   `esds` object type of `0x20` is MPEG-4 Visual and `ffprobe` prints
   `codec_name=mpeg4`; we print nothing, because the variant does not exist.
   The same applies to AC-3, E-AC-3, ALAC, AMR, `tx3g` and ProRes, all of which
   `vaco-format-isom`'s tables already map to `None` deliberately. This is the
   only field-level conformance gap left on the fixtures above.
