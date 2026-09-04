# `vaco-format-isom`

Layer 4. The ISO base media file format **box layer** — the structural parser
behind MP4, MOV, 3GP, CMAF and fragmented MP4.

This is not the demuxer. `vaco-demux-mp4` is, and it is built on this crate:
everything here is structure, tables, and the arithmetic that turns them into
answers. No I/O policy, no packet emission, no opinion about which sample to
play next.

---

## What it is

| Module | Contents |
|---|---|
| `boxes` | box headers (32/64-bit sizes, `uuid`, full-box version/flags), flat iteration, two bounded searches |
| `fourcc` | `FourCc` and the box-type constants |
| `scan` | locating top-level boxes over `vaco_io::IoContext` without reading their payloads |
| `movie` | `ftyp`, `mvhd`, `trak ▸ mdia ▸ minf ▸ stbl` assembly into `Movie`/`Track` |
| `stbl` | the sample tables and the sample → byte-offset mapping |
| `table` | fixed-stride table views and their decimated summaries |
| `edit` | `elst` and the presentation ↔ media timeline |
| `frag` | `mvex`/`trex`, `moof ▸ traf ▸ tfhd/tfdt/trun`, `sidx`, `mfra ▸ tfra` |
| `stsd` | sample entries, configuration boxes, the four-character-code tables |
| `cenc` | Common Encryption (ISO/IEC 23001-7): `pssh`, `schm`/`tenc`, `saiz`/`saio`, `senc` — reports the scheme and key id, decrypts nothing |
| `esds` | the MPEG-4 descriptor tree and the object type indications |
| `fixed` | 16.16 / 8.8 / 2.30 fixed point and the 3×3 display matrix |
| `lang` | the packed ISO-639-2/T language field |
| `probe` | content scoring, with the measurements behind it |
| `build` | fixture construction, shared by the tests, benchmarks and fuzz targets |
| `writer` | production box writers — `ftyp`/`mvhd`/`tkhd`/`mdhd`/`hdlr`, the sample tables, sample entries and their config boxes, `moof`/`traf`/`trun`, `sidx`, `mfra`, `udta`/`meta`/`ilst`. Used by `vaco-mux-mp4`. |

Written from **ISO/IEC 14496-12** (base file format), **14496-14** (MP4),
**14496-15** (`avcC`/`hvcC` carriage), **14496-1** (`esds`), and Apple's
published *QuickTime File Format Specification* for the MOV-only sample-entry
versions. No FFmpeg source was consulted (D7/D15); behavioural facts came from
running the binary, and each one is recorded below with its command.

---

## How it works

### The sample tables are the point

Five tables compose into one answer:

```text
stsc   sample  -> chunk, and position within the chunk
stco   chunk   -> file offset
stsz   sample  -> size          (so the within-chunk offset is a running sum)
stts   sample  -> decode time   (run-length coded deltas)
ctts   sample  -> pts - dts
stss   sample  -> is it a sync sample (absent => every sample is)
```

`SampleTable::sample(n)` resolves all six for an arbitrary `n`. Verified
byte-exactly against the reference: on the calibration file, `ffprobe
-show_packets` reported the first five video packets at `pos = 3017, 7839, 9765,
11181, 12226`, and those are the offsets the crate produces from a `stsc` of
`[(1, 2, 1), (2, 1, 1)]` and a `stco` starting `3017, 9765, 11181`.

### Two access paths, deliberately separate

| Path | Cost | Used by |
|---|---|---|
| `SampleTable::sample(n)` / `sample_at_dts(t)` | O(log n) | seek |
| `SampleTable::cursor()` | O(1) amortised per sample | the demux loop |

A seek asks the random-access questions repeatedly — find the sample at a time,
walk back to the sync sample, resolve it, then repeat for every other track —
so it must never walk from sample zero. The cursor carries the `stts` run, the
chunk and the within-chunk byte offset, so `next` is a table read and three
additions rather than three binary searches.

Measured on a 50 000-sample table (`cargo bench -p vaco-format-isom`, M-series,
per operation):

| | compact | fragmented tables | uniform |
|---|---:|---:|---:|
| one seek (`sample_at_dts` → `sync_at_or_before` → `sample`) | **135 ns** | 252 ns | 14 ns |
| one random sample lookup | **92 ns** | 192 ns | 10 ns |
| one sequential sample | **30 ns** | 73 ns | 3.6 ns |
| whole-table parse | 64 µs | 178 µs | 0.17 µs |

`compact` is one `stts` run and one sample per chunk — a normally-muxed file.
`fragmented tables` is **one `stts` run per sample and one `stsc` run per
chunk**, the shape the decimated summaries exist for; it costs about 2×, not
50 000×, which is the whole point. The cursor is ~3× faster than random access,
so keeping the two paths separate earns its keep in the demux loop.

Parse cost is the eager summary build over `stsz`; a 3-hour 30 fps track
(324 000 samples) is about 420 µs, paid once at open. A uniform `stsz` builds no
summary at all, which is why its column is three orders of magnitude cheaper.

They are cross-checked everywhere: a unit test, a property test and two fuzz
targets. On a well-formed table the invariant is `table.sample(i) ==
table.cursor().nth(i)`; on an adversarial one the cursor may skip indices random
access cannot resolve, so the fuzz targets assert the weaker-but-always-true
form (every yielded sample round-trips through random access, indices strictly
increase, nothing resolvable is skipped). If those two paths ever diverge, a
seek lands somewhere a sequential read never would, and nothing else in the
pipeline would notice.

### What the cursor does with a damaged table

Not "stop at the first problem". A sample fails to resolve for exactly two
reasons, and they are bounded differently:

* **The chunk has no `stco`/`co64` entry.** Chunk numbers are non-decreasing in
  the sample index, so once one chunk is past the end of the offset table every
  later one is too. That is the **end** of iteration.
* **`chunk_offset + within` overflows `u64`.** Monotone inside the chunk (the
  running offset only grows) but says nothing about the next one, so the cursor
  jumps to the next chunk's first sample in **one step**.

Both are bounded by the number of chunks, which is bounded by the `stco`
payload. One bad chunk offset costs one chunk, not the rest of the track —
which is also what `planning/18-formats.md` §3.1.10 says about samples past the
end of `mdat`.

### The memory decision, with the arithmetic

**Nothing proportional to the sample count is allocated.** A sample table is a
`&[u8]` plus a stride; entry *i* is decoded on demand. Two rules make that safe:

1. **Declared counts are clamped against the payload.** Every one of these
   tables has a fixed entry width, so `usable = min(declared, payload_len /
   stride)` is exact rather than a heuristic. A `stsz` claiming `0xFFFF_FFFF`
   samples in a twelve-byte box describes one sample.
2. **Nothing is decoded up front.** Parsing an entire `stbl` allocates only the
   run summaries below.

For a 3-hour 30 fps video track — 324 000 samples, one chunk per sample, the
worst realistic case:

| Table | In the file | Resident |
|---|---:|---:|
| `stsz` | 1.30 MB | 0 (borrowed) |
| `stco` | 1.30 MB | 0 (borrowed) |
| `stts`, one run | 8 B | 0 (borrowed) |
| `stsc`, two runs | 24 B | 0 (borrowed) |
| summaries | — | ≤ 4 × 96 KiB |

Against a materialised `Vec<Sample>` at 40 bytes each — 13 MB for that track,
and 130 MB for a nine-hour surveillance recording — this is a 20× to 400×
reduction, and it is bounded by a **constant** rather than by the input.

### Why the summaries exist, and why they are decimated

Borrowing gives O(1) access to entry *i*, but the questions are cumulative:
*which sample is at DTS t*, *what is the byte offset of sample n*. Answering
those from run-length tables is O(runs) or O(samples) per query.

`RunIndex` is a **decimated prefix sum**: a checkpoint every `stride` entries,
at most `MAX_CHECKPOINTS` (4096) of them, so `stride = ceil(runs / 4096)`.
Lookup is a binary search over the checkpoints plus at most `stride` linear
steps.

A full prefix sum would be simpler and is the wrong choice: it costs 16 bytes
per run, and the pathological `stts` — one run per sample — has as many runs as
samples, so the "index" would be larger than the table it indexes. Decimation
makes both memory and query cost independent of the input. A normal file has
one `stts` run and gets stride 1, i.e. an exact index, for 24 bytes.

### No recursion is reachable from input

Box parsing is the textbook stack-overflow surface. This crate answers it
structurally, not with a counter:

* `BoxIter` is **flat** — it walks one container's direct children and never
  descends;
* the known tree (`moov ▸ trak ▸ mdia ▸ minf ▸ stbl`) is assembled by nested
  `for` loops, a compile-time depth no input can deepen;
* the two generic searches (`boxes::find_path`, `boxes::walk`) are iterative
  with an explicit worklist and a hard `MAX_DEPTH` of 16.

There is a test that builds a megabyte of nested `moov` boxes — 20 000 deep —
and asserts the walk visits at most `MAX_DEPTH` of them. A counter has to be
correct at every call site; this has to be correct once.

### Termination

`BoxHeader::parse` guarantees `size >= header_len >= 8`, so every iteration step
advances by at least eight bytes. That single invariant is what makes box
iteration, the top-level scan and the recursive-free walk all terminate, and it
is asserted directly by the `isom_box_walk` fuzz target.

Where a loop's trip count is genuinely input-derived — the top-level scan, the
generic walk — the caller's `vaco_limits::Budget` is charged one fuel per box,
so exhaustion is deterministic and reproducible rather than wall-clock.

### Arithmetic

Every accumulation saturates rather than wrapping. `stts` decode times and
cumulative byte counts are both non-decreasing sequences, so saturation
preserves the ordering the binary searches depend on; wrapping would not, and
panicking is not available to a parser of untrusted input (`unwrap`, `expect`,
`panic` and `indexing_slicing` are all denied workspace-wide).

### Writers

`writer` is the production counterpart to every reader above it, and the only
place `vaco-mux-mp4` builds a box from typed fields. Split from `build`
deliberately: `build` is a fixture maker that will write a shape the spec
forbids on request (half its callers are negative tests); `writer` never does.
Both compose the same box-framing primitives, `build::bx`/`build::fullbx` —
the one place the four-byte-size-plus-fourcc-plus-payload concept lives (D19).

Two conventions worth knowing before extending it:

* **Config records are opaque.** `avcc`/`hvcc`/`av1c`/`vpcc`/`dops`/`dfla` wrap
  `CodecParameters::extradata` verbatim — no NAL or `AudioSpecificConfig`
  parsing happens here or in the caller (D14.1: a `vaco-format-*` crate cannot
  depend on a `vaco-parse-*` one). A caller with no extradata yet is a
  bitstream-filter problem (`extract_extradata`), not this crate's.
* **`stsz` is always non-uniform.** One shape to write correctly rather than
  two; the uniform (constant-size) form is a valid but unwritten optimisation.

`movie::from_unix_time` is the writer's counterpart to `movie::to_unix_time` —
Unix seconds to the 1904-epoch `u64` `mvhd`/`tkhd`/`mdhd` store, saturating
rather than panicking outside that range. Neither it nor anything else in
`writer` reads the wall clock; the caller supplies (or omits) a timestamp.

---

## Reference behaviour (D17)

Every row below was **measured**, not inferred. Plan 13 §1b's rule applies: each
came from the most direct entry point available — `ffprobe` reading a real file
written by `ffmpeg` — with the file's own bytes dumped independently so the
input side of each experiment is known rather than assumed.

Fixtures, all `ffmpeg` 8.1:

```sh
ffmpeg -f lavfi -i testsrc2=size=160x120:rate=25:duration=2 \
       -f lavfi -i sine=frequency=440:duration=2 \
       -c:v libx264 -preset ultrafast -g 15 -bf 2 -pix_fmt yuv420p -c:a aac \
       -movflags +faststart prog.mp4

ffmpeg ... -movflags +frag_keyframe+empty_moov                     frag.mp4
ffmpeg ... -movflags +frag_keyframe+empty_moov+default_base_moof+omit_tfhd_offset dbm.mp4
ffmpeg -itsoffset 0.5 ... -bf 0                                    delay.mp4
ffmpeg ... -bf 2 -movflags +negative_cts_offsets                   ncts.mp4
```

### 1. Sample offsets — confirmed exactly

```sh
ffprobe -v error -show_packets -select_streams 0 -of compact prog.mp4
```

`pos = 3017, 7839, 9765, 11181, 12226`. Reproduced from `stsc`/`stsz`/`stco`.
Fragmented: `frag.mp4` reported `pos=1775` from `tfhd.base_data_offset = 1259`
plus `trun.data_offset = 516`; `dbm.mp4` reported `pos=921` from `moof` at 769
plus `data_offset = 152`.

### 2. The DTS shift — a spec deviation we reproduce

ISO/IEC 14496-12 §8.6.1.4 defines `cslg.compositionToDTSShift` as a value
**added to composition times** so that every composition time is at or above its
decode time. The reference instead **subtracts** the equivalent quantity from
every decode time, leaving presentation times where the file put them.

Measured on `ncts.mp4` (`ctts` version 1, runs `(1,0) (1,1024) (2,-512)`,
`elst media_time = 0`):

```text
ffprobe -show_packets  ->  pts=0 dts=-512 | pts=1536 dts=0 | pts=512 dts=512
```

The applied shift is `min(ctts) = -512` on DTS. The specification's reading
would have produced `pts=512 dts=0`. Both express the same `pts - dts`
relationship; only one matches `-show_packets`, and D6 makes that the contract.

`SampleTable::dts_shift` returns `min(0, least_offset)` accordingly, preferring
`cslg` when present. **This must not be "corrected" to the specification's sign
convention** — the annotation on the method exists to stop exactly that. A
`ctts` version 0 table cannot hold a negative offset, so such a track always
gets zero; confirmed on `prog.mp4`, whose `ctts` v0 minimum is 512 and whose
reported shift came entirely from its edit list.

### 3. Edit lists

| File | `elst` (movie ts, media ts) | `start_pts` | first packet |
|---|---|---:|---|
| `prog.mp4` video | `[(2000, 1024, 1.0)]` | 0 | `pts=0 dts=-1024` |
| `prog.mp4` audio | `[(2000, 1024, 1.0)]` | 0 | `pts=-1024`, `skip_samples=1024`, discard |
| `delay.mp4` video | `[(520, -1), (2000, 0)]` | 6656 | `pts=6656` |
| `frag.mp4` video | none | 1024 | `pts=1024 dts=0` |

So a non-empty first edit with `media_time = M` shifts **both** PTS and DTS by
`-M`, and a leading empty edit shifts both by `+segment_duration` rescaled from
the movie timescale into the media timescale. `EditList::simple_shift` is that
sum and reproduces all four rows.

Audio samples before the edit start are **not dropped**; they are emitted with a
`skip_samples` trim and a discard flag. That is the demuxer's decision. What
this crate owes it is `Timeline::media_to_presentation` returning `None` for
exactly those samples, which it does.

### 4. `duration_ts` comes from the edit list, not from `mdhd`

| Track | `mdhd.duration` | non-empty `elst` | `duration_ts` |
|---|---:|---:|---:|
| `prog.mp4` video | 26 112 | 2000 movie @12800/1000 | **25 600** |
| `prog.mp4` audio | 89 224 | 2000 movie @44100/1000 | **88 200** |
| `delay.mp4` video | 25 600 | 2000 (empty edit excluded) | **25 600** |

The third row discriminates: had the empty edit counted, `duration_ts` would be
32 256. `Track::reported_duration` implements it; with no edit list at all,
`mdhd.duration` is used.

`nb_frames` is the `stsz` sample count, and is `N/A` for a fragmented file,
which has no `stsz`.

### 5. Probe scores — three of plan 18's four rows were wrong

Plan 18 §3.1.3 predicts 100 for a recognised major brand, 90 for an unknown one
and 75 for a leading `moov`/`mdat`. Measured by mutating one file four ways:

```sh
ffprobe -v quiet -show_entries format=probe_score -of default=nw=1:nk=1 <file>
```

| File | Prediction | Measured |
|---|---:|---:|
| `ftyp` brand `isom` | 100 | **100** |
| `ftyp` brand `zzzz`, all compatible brands overwritten | 90 | **100** |
| `ftyp` removed, file starts with `moov` | 75 | **100** |
| `ftyp` removed, `mdat` first, `moov` last | 75 | **100** |

The reference's ISOBMFF probe does not grade brands at all. `probe::probe`
returns `MAGIC_CHECKED` for any structural opening box, and `CONTENT` for a
padding-only opening (`free`/`wide`/`skip`) — that last row is a **choice**, not
a reproduction: such files are detected by the reference (the error carries the
`mov,mp4,m4a,3gp,3g2,mj2` context) but have no streams, so no `FORMAT` section
and no score is printable.

`KNOWN_BRANDS` therefore affects no score. It exists for callers that want to
report or filter on brands.

### 6. Fragment byte addressing

§8.8.7.1's three cases are implemented as written, and rows 2 and 3 of the
measurement table above confirm the first two. **Plan 18 §3.1.10 is wrong about
the third**: it says a `tfhd` with neither `default-base-is-moof` nor
`base_data_offset` bases on "the start of the previous `mdat`". 14496-12 says
the first track fragment bases on the enclosing `moof` and each later one on the
end of its predecessor's data. This crate follows the specification; the two
agree for a single-`traf` fragment and disagree for any file with more than one,
and no fixture that distinguishes them could be produced with `ffmpeg` (it
always writes an explicit base or sets the flag).

### 7. Common Encryption box layouts — confirmed against a real encrypted file

```sh
ffmpeg -f lavfi -i testsrc2=size=64x64:rate=10:duration=1 -c:v libx264 -preset ultrafast \
       -encryption_scheme cenc-aes-ctr \
       -encryption_key 0123456789abcdef0123456789abcdef \
       -encryption_kid 00000000000000000000000000000001 \
       enc.mp4
```

Every field `cenc` reads was checked against this file's actual bytes, not just
the spec text: `schm`'s `scheme_type`/`scheme_version` (`cenc`, `0x00010000`);
`tenc` version 0's `default_isProtected=1`, `default_Per_Sample_IV_Size=8`,
`default_KID` ending `…0001` exactly as passed to `-encryption_kid`; `senc`'s
`sample_count` and per-sample `(IV, subsample_count, (clear, encrypted)*)`
records with `flags & 2` set; and `saiz`/`saio`, which point at precisely the
byte range `senc` itself occupies after its own `sample_count` field —
`saio`'s single offset (11 819) is exactly `senc`'s file offset (11 803) plus
its 8-byte box header plus 4 bytes of `sample_count`, confirmed by computing
both independently and comparing. `pssh` and `tenc` version 1's
`default_crypt_byte_block`/`default_skip_byte_block` were not exercised by this
file (`ffmpeg`'s encoder does not emit a `pssh` on its own) and are transcribed
directly from ISO/IEC 23001-7 instead — noted as such in the module doc
comment, not presented as measured.

Also measured, and recorded in `vaco-demux-mp4`'s doc file rather than here
because it is about demuxing policy, not box shape: `ffprobe 8.1` surfaces
**no** encryption information for this file at all, and `ffmpeg -i` decodes the
still-encrypted bytes into corrupted frames rather than refusing to open it.

### 8. The PCM codec table, and why a `FourCc` table cannot work

**The finding a future reader most needs before touching `stsd::sample_entry_codec`
or `esds::OBJECT_TYPE_TABLE`:** a sample entry's four-character code alone does
not name a PCM codec. `sowt` covers both 8- and 16-bit; `in24`, `in32`, `fl32`
and `fl64` each cover *both* byte orders; `raw ` means `pcm_u8` in an audio
entry and `rawvideo` in a video one; and `lpcm`'s entire layout — width,
signedness, float-ness, byte order — lives in its version-2 body, not in the
fourcc at all. No amount of filling in a `FourCc -> CodecId` table fixes this;
the table has to take the sample entry's *context* as an input.

Measured 2026-08-23 by encoding one `.mov` per `ffmpeg` PCM encoder
(`ffmpeg -f lavfi -i "sine=frequency=440:duration=0.2" -c:a <encoder> -f mov
p.mov`), reading back `codec_tag_string`/`codec_name` with `ffprobe`, and
reading the sample entry's raw bytes directly for `enda`, `sample_size` and
(for `lpcm`) the version-2 body:

| encoder | `codec_tag_string` | `codec_name` | `enda` box | `sample_size` field |
|---|---|---|---|---:|
| `pcm_s16le` | `sowt` | `pcm_s16le` | absent | 16 (accurate) |
| `pcm_s8` | `sowt` | `pcm_s8` | absent | 8 (accurate) |
| `pcm_s16be` | `twos` | `pcm_s16be` | absent | 16 (accurate) |
| `pcm_s24le` | `in24` | `pcm_s24le` | `0001` (little) | 16 (**placeholder**) |
| `pcm_s24be` | `in24` | `pcm_s24be` | `0000` (big) | 16 (**placeholder**) |
| `pcm_s32le` | `in32` | `pcm_s32le` | `0001` (little) | 16 (**placeholder**) |
| `pcm_s32be` | `in32` | `pcm_s32be` | `0000` (big) | 16 (**placeholder**) |
| `pcm_f32le` | `fl32` | `pcm_f32le` | `0001` (little) | 16 (**placeholder**) |
| `pcm_f32be` | `fl32` | `pcm_f32be` | `0000` (big) | 16 (**placeholder**) |
| `pcm_f64le` | `fl64` | `pcm_f64le` | `0001` (little) | 16 (**placeholder**) |
| `pcm_f64be` | `fl64` | `pcm_f64be` | `0000` (big) | 16 (**placeholder**) |
| `pcm_u8` | `raw ` | `pcm_u8` | absent | 8 (accurate) |
| `pcm_alaw` | `alaw` | `pcm_alaw` | absent | 16 (irrelevant — fixed format) |
| `pcm_mulaw` | `ulaw` | `pcm_mulaw` | absent | 16 (irrelevant — fixed format) |

The "placeholder" column is the correction to make to finding 7 as originally
written: it said "width comes from `bits_per_sample`", which is true in
outcome but not in mechanism. For `in24`/`in32`/`fl32`/`fl64` the classic
`sample_size` field is a **fixed 16 regardless of the real width** — every
measured file above reports it — so this crate does not read it for those four
fourccs at all. The width is already fixed by the fourcc itself; only the byte
order is open, and that comes from `enda`. `sample_size` is trustworthy only
for `sowt`/`twos`, where it really does vary (8 vs. 16) and really is read.

`enda` (found via `SampleEntry::endian`) is a `QuickTime` atom nested inside a
`wave` extension box, alongside `frma` — the same nesting `esds` uses for old
`QuickTime` audio, found the same way (`SampleEntry::config`/`SampleEntry::endian`
both check the top-level extensions first, then fall back into `wave`). Its
payload is one big-endian `u16`: `0` is big-endian, `1` is little-endian. When
it is absent entirely — measured for every `sowt`/`twos`/`raw `/`ulaw`/`alaw`
file, since those fix their byte order (or have none) in the fourcc — this
crate defaults to big-endian per the QTFF specification, for the unmeasured
case of an `in24`/`in32`/`fl32`/`fl64` entry that omits it.

`lpcm` was measured separately, since none of the encoders above ever produce
it: an 8-channel, 192 kHz `pcm_s32le` track is the smallest input this
`ffmpeg` build promotes to a version-2 `lpcm` entry rather than `sowt`/`in32`.
Its version-2 body carried `formatFlags = 0x0C` (bit 2 signed, bit 3 packed,
bit 1 clear so little-endian) and `constBitsPerChannel = 32`, and `ffprobe`
called the result `pcm_s32le` — confirming that `formatFlags` +
`constBitsPerChannel`, not the fourcc or `sample_size`, decide an `lpcm`
entry's flavour. `SampleEntry::codec` resolves this via the private
`lpcm_pcm` helper; `AudioSampleEntry::format_flags` and
`::const_bits_per_channel` are the two fields that make it possible, kept
where the rest of `parse_audio`'s version-2 branch used to discard them.

Two rows are unmeasured and documented as such at the call site rather than
guessed silently: `twos` at 8-bit (no `ffmpeg` encoder writes it; handled
symmetrically with `sowt` per the QTFF spec, since byte order is irrelevant at
8 bits either way) and the `NONE` compression type (no `ffmpeg` path emits it
at all; treated the same as `twos` — width from `sample_size`, byte order from
`enda` defaulting to big-endian — because that is what the QTFF spec says
`NONE` means).

`raw ` in a video sample entry resolves to `rawvideo` regardless of pixel
format; three more fourccs came out of the same measurement for free and are
now plain rows in `sample_entry_codec`: `2vuy` (UYVY422), `yuvs` (YUYV422) and
`24BG` (BGR24) all report `codec_name=rawvideo`.

#### The ESDS object-type-indication table

`esds::OBJECT_TYPE_TABLE` is the complete registry — ISO/IEC 14496-1 Table 5
plus every extension the MP4 Registration Authority has since assigned
(`mp4ra.org/registered-types/object-types`) — transcribed in full rather than
trimmed to the rows this workspace can currently name. That completeness is
what caught a real trap: `0xA5`/`0xA6` used to mean AC-3 and Enhanced AC-3, and
a table built only from "which `CodecId`s do we have" would plausibly have
mapped them there. The registry itself marks both **Withdrawn**, and measuring
confirms why it does not matter in practice: `ffmpeg -c:a ac3 -f mov` and
`-c:a eac3 -f mov` never put their stream behind `mp4a`/`esds` at all — they
write their own sample-entry fourccs, `ac-3` and `ec-3`, exactly as
`sample_entry_codec` already expected.

The one addition confirmed by measurement rather than by transcription:
`ffmpeg -c:v mpeg4 -f mov` writes an `mp4v` entry with `esds` object type
`0x20`, and `ffprobe` calls it `codec_name=mpeg4`. Everything else that could
plausibly route through `mp4a`/`mp4v` + `esds` in this `ffmpeg` build — MPEG-2
video, MPEG-1 video (no encoder in this build at the tested frame rate), AC-3,
E-AC-3, DTS, MP3 — was measured and turned out **not** to use `esds` at all,
each getting its own dedicated fourcc instead (`m2v1`, `ac-3`, `ec-3`, `dtsc`,
`.mp3`). Those are `sample_entry_codec` rows, not `OBJECT_TYPE_TABLE` ones.

`h263` and the six ProRes quality tiers (`apco`/`apcs`/`apcn`/`apch`/`ap4h`/
`ap4x`) were measured the same way and never go through `esds` either — `h263`
writes its own fourcc directly, and every ProRes tier reports
`codec_name=prores`, distinguished only by `codec_tag_string`.

### 9. `ipcm`/`fpcm` (ISO/IEC 23003-5), and why `sample_fmt` needed a second fix

Found by the container sweep that followed finding 8 above: `ffprobe` reads
`pcm_s16le` MP4 tracks written by a modern `ffmpeg` (9.0.1) as `codec_tag_string
=ipcm`, not `sowt`. `ipcm` (integer) and `fpcm` (float) are ISO/IEC 23003-5's
own uncompressed-PCM sample entries, a *different* scheme from every QuickTime
flavour in finding 8 — neither fourcc had a row anywhere in this crate before
this fix, so `codec_name` printed `unknown` and the track did not decode at
all (0 bytes out against the reference's 88200 for a 1-second 44.1 kHz mono
fixture).

Measured 2026-09-02 the same way as finding 8 (`ffmpeg -c:a pcm_s16le|
pcm_s16be|pcm_f32le -f mp4`, `ffprobe` plus raw sample-entry bytes):
`sample_size`/`enda` play no part here at all. The entry carries its own
`pcmC` ("PCM Configuration Box") extension instead — a `FullBox` (version 0,
flags 0) followed by two more bytes, `format_flags` (bit 0: `1` little-endian,
`0` big-endian) and `PCM_sample_size` (an 8-bit true bit depth). `pcm_s16le`
wrote `01 10`, `pcm_s16be` wrote `00 10`, `pcm_f32le` wrote `01 20`. This is
*not* the same box `lpcm`'s own version-2 body uses despite the similar name
— `lpcm` has no `pcmC` at all, and `ipcm`/`fpcm` have no version-2 body.
`SampleEntry::resolve_ambiguous` reads it through `find_pcmc`; the classic
`AudioSampleEntry.samplesize` field alongside it is, once again, a fixed `16`
placeholder regardless of the real width — the same shape finding 8 already
documented for `in24`/`in32`/`fl32`/`fl64`.

**The second, separate defect this sweep found:** `codec_tag_string=ipcm`
already matched the reference with `codec_name` still wrong, and fixing
`codec_name` alone was not the end of it. `channels` read `0` against the
reference's `1` — a gap in `vaco-demux-mp4::track::codec_parameters`, not this
crate: nothing there ever read `AudioSampleEntry::channel_count` into
`AudioParameters::layout`, for *any* codec, not just PCM. And `sample_fmt`
read `unknown` against the reference's `s16` — nothing anywhere in this crate
or `vaco-demux-mp4` ever populated `AudioParameters::format` for an MP4 PCM
track, because PCM has no bitstream header for
`vaco_format_core::discovery`'s generic parser-refinement pass to read (that
pass is what makes `sample_fmt` correct for AAC without either crate doing
anything special for it). `stsd::pcm_decoded_format` closes this — one
`CodecId::Pcm*` to `SampleFmt` row per variant an MP4/MOV entry can actually
produce, duplicated from `vaco-codec-pcm::table::PCM_FORMATS`'s `decoded`
column rather than depended on (D14.1: a format crate does not name a codec
crate — `vaco-demux-matroska::codec::pcm_format` and
`vaco-demux-raw::pcm::PCM_FORMATS` each already carry their own copy of the
same table for the same reason).

Verified end to end, not just probed: `vaco -i t.mp4 -map 0:a:0 -f s16le
out.raw` on the `pcm_s16le` fixture above produced output byte-identical to
`ffmpeg`'s own decode of the same file (88200 bytes both sides), and the same
held for a `pcm_f32le` fixture and a 2-channel `pcm_s16le` fixture.

---

## How to change it

### Adding a box

Add its four-character code to `fourcc::boxes`, then handle it in whichever
module owns its parent — `movie` for `moov` descendants, `stbl` for sample
tables, `frag` for `moof` descendants, `stsd` for sample-entry extensions.
Unknown boxes are already skipped correctly by their declared size, so adding
one is additive and cannot break an existing parse.

### Adding a codec four-character code

Start by asking whether the fourcc alone determines the codec. If it does —
most of them do — add it to `stsd::sample_entry_codec`. Codes with no
`vaco_codec_core::CodecId` map to `None` deliberately rather than to a near
miss; the caller keeps the raw four-character code either way and `ffprobe`
prints it as `codec_tag_string` regardless.

If it does not — see "The PCM codec table, and why a `FourCc` table cannot
work" above — it needs a case in `SampleEntry::resolve_ambiguous` instead,
which has access to the whole entry (media type, `bits_per_sample`, `enda`,
the version-2 body), not just the four bytes. `sample_entry_codec` still gets
a row for that fourcc as a **fallback** for when the context does not resolve
it (a malformed entry, or a bit depth this workspace has no exact `CodecId`
for) — `resolve_ambiguous` returning `None` is what lets `SampleEntry::codec`
fall through to it.

`mp4a`/`mp4v` are the third case: refined through `esds`'s
`OBJECT_TYPE_TABLE`, which is the complete MP4RA object-type-indication
registry, not a hand-picked subset — see above for why that completeness
matters. Measure before adding a row to any of the three: encode with the
relevant `ffmpeg` codec into `.mov`, and check whether it actually goes
through `mp4a`/`mp4v` + `esds` at all before assuming an object type — several
codecs that look like `esds` candidates (AC-3, DTS, MP3, MPEG-2 video) turn
out to use their own dedicated fourcc instead.

### Changing the memory/latency trade-off

`table::MAX_CHECKPOINTS` is the single knob. Raising it makes random access
faster on pathologically fragmented tables and costs 24 bytes per checkpoint per
summary per track; lowering it does the reverse. The current value makes the
worst-case linear tail `ceil(runs / 4096)` steps, which for a ten-million-run
table is about 2400 — microseconds, not milliseconds.

### Gotchas

* **The two timescales.** `elst.segment_duration` is in the **movie** timescale;
  `elst.media_time` is in the **media** timescale, in the same record. Using one
  for the other desynchronises by the ratio of the two — 12.8× for video and
  44.1× for audio in the calibration file, which presents as drift rather than
  as a broken file. Every conversion goes through
  `edit::rescale_movie_to_media`, which takes both explicitly.
* **`stss` is one-based**, `stco` chunk numbers are one-based, `stsc.first_chunk`
  is one-based, and sample numbers in this crate's API are **zero-based**. The
  conversion happens at the table boundary and nowhere else.
* **`ctts` version decides the sign.** Version 0 is unsigned, version 1 signed.
  Getting it backwards moves every presentation timestamp on the track.
* **An absent `stss` and an empty `stss` are different.** Absent means every
  sample is a sync sample; present-but-empty means none are. There are two tests
  for exactly this.
* **`stsc` is the one structural refusal.** A `first_chunk` that does not start
  at 1 or does not strictly increase makes the run's extent undefined, and every
  offset derived from it would be invented. `SampleTable::parse` returns
  `InvalidData`; the demuxer should drop that track and keep the others.
* **`build` is public.** It is fixture construction, not a muxer, and nothing in
  the parse path calls it. It is public so `vaco-demux-mp4`'s tests, the
  benchmarks and the fuzz targets share one definition of "an MP4 shaped like
  this".

---

## Configuration

No options, no environment variables, no features. Two constants bound
residency and one bounds work:

| Constant | Value | Bounds |
|---|---:|---|
| `table::MAX_CHECKPOINTS` | 4096 | summary size, and therefore the linear tail of a lookup |
| `boxes::MAX_DEPTH` | 16 | worklist depth in the generic searches |
| `boxes::FUEL_PER_BOX` | 1 | fuel charged per box header inspected |
| `edit::MAX_EDIT_ENTRIES` | 65 536 | `elst` entries kept (16 bytes each) |
| `movie::MAX_TRACKS` | 4096 | tracks kept from one `moov` |
| `movie::MAX_FRAGMENTS` | 65 536 | `moof` boxes collected by `IsoFile::parse` |
| `frag::MAX_RUNS_PER_TRAF` | 4096 | `trun` boxes kept per `traf` |
| `frag::MAX_TRAF_PER_MOOF` | 1024 | track fragments per `moof` |
| `scan::MAX_TOP_LEVEL_BOXES` | 1 048 576 | boxes a scan inspects before giving up |
| `esds::MAX_DESCRIPTORS` | 64 | descriptors walked in one `esds` |

Callers pass a `vaco_limits::Budget` to `boxes::walk`, `scan::TopLevelScanner`
and `scan::read_payload`; those are the only entry points that allocate or that
do input-derived amounts of work.

---

## Dependencies

| Crate | Why |
|---|---|
| `vaco-core` | `Error`, `Result`, `Rational`, `rescale_rnd`, `MediaType` — exact time-base arithmetic, never hand-rolled |
| `vaco-bitstream` | `ByteReader`, whose sticky-overrun model is exactly right for box parsing: read the fields, check once per box |
| `vaco-limits` | `Budget` — fuel for the walkers, allocation ceilings for payload reads |
| `vaco-io` | `IoContext` for `scan` |
| `vaco-format-core` | `ProbeData`/`ProbeScore` for `probe` |
| `vaco-codec-core` | `CodecId` for the four-character-code and object-type tables |

Dev only: `proptest`, `divan`.

No external dependencies. Nothing here is a wrapper, so D11 does not apply and
there is no fidelity grade.

---

## Tests and benchmarks

```sh
cargo test  -p vaco-format-isom --locked
cargo bench -p vaco-format-isom
just fuzz isom_file
just fuzz isom_sample_table
just fuzz isom_box_walk
```

The benchmark (`benches/sample_lookup.rs`, divan) measures the seek path on
three table shapes: `compact` (one `stts` run, one sample per chunk — what a
normally-muxed file looks like), `fragmented_tables` (one run per sample — the
adversarial shape the decimation exists for), and `uniform` (constant sample
size, where the within-chunk offset is a multiplication). `seek_by_time` is the
number that matters: `sample_at_dts` → `sync_at_or_before` → `sample`, which is
what one seek does per track.

Three fuzz targets, because the crate has three distinct surfaces:

| Target | Input | Looking for |
|---|---|---|
| `isom_box_walk` | raw bytes | non-terminating iteration, unbounded depth, `esds` length overflow |
| `isom_sample_table` | structured tables in a valid box wrapper | cross-table contradictions, overflow in the offset arithmetic |
| `isom_file` | raw bytes | everything, end to end |

All three assert the central invariant — random access and the cursor agree —
rather than only "does not panic".

### What the fuzzer found

Four findings, all in the sample-table layer, all now pinned by named
regression tests. They are recorded here because three of the four are
**design** findings rather than patches, and the fourth is a lesson about
oracles.

| # | Found by | Executions | What it was |
|---|---|---:|---|
| 1 | `isom_sample_table` | 25 | A *uniform* `stsz` is the one declared count with no payload to clamp it: twelve bytes can legally declare four billion samples. Not a bug in the crate — nothing allocates for it — but the target asserted a bound that does not exist. Documented on `SampleSizes::uniform`; bounding it is the demuxer's job and happens naturally, because such a file's chunk offsets point past its own end. |
| 2 | `isom_sample_table` | 27 | **Real bug.** The cursor stopped at the first unresolvable sample. A `co64` whose first chunk offset was `u64::MAX` made samples 1..43 overflow and the cursor never reached sample 44, which random access resolved at an ordinary offset in the second chunk. Fixed by skipping holes; test `one_bad_chunk_offset_costs_one_chunk_not_the_rest_of_the_track`. |
| 3 | `isom_sample_table` | slow-unit | **Real design bug, and the most valuable of the four.** The fix for #2 skipped one sample at a time, so a `stsc` declaring 4.2 billion single-sample chunks with no offsets took **13.8 seconds on a 78-byte input** — superlinear expansion of exactly the kind the brief warned about. `cargo fuzz` **exits 0 on a slow unit**, so the artifact on disk was the only evidence. Localised by timing the target's sections: parse and every point query were under 400 µs and the cursor was all of the remaining 13.8 s — the first hypothesis (that the oracle was at fault) was wrong, and measuring rather than reasoning is what found it. Fixed by observing that chunk numbers are monotone, which makes a missing offset terminal; 13.8 s → 0 ms. Test `a_stsc_declaring_billions_of_offsetless_chunks_ends_at_once`. |
| 4 | `isom_sample_table` and `isom_file`, within 30 executions of each | 25 / 26 | **Real bug.** `stss` sample numbers are one-based, so the zero-based sample `u32::MAX` has one-based number 2^32. Computing `n + 1` in `u32` saturated, and `sync_at_or_after(u32::MAX)` returned `u32::MAX - 1` — a sync sample *before* the one asked for, which is a seek landing backwards. Fixed by widening the search key to `u64`; test `sync_queries_at_the_top_of_the_index_space_do_not_go_backwards`. |

Finding 3 is worth restating as a rule, because it is the one that generalises:
**a slow unit is the fuzzer answering the eager-versus-lazy question
empirically.** The exit code says nothing about it. Plan 19 §13's rule — an
artifact on disk is a finding whatever the log says — is what surfaces it.

The throughput change is the clearest evidence the fix landed. Same target,
same machine, same seed corpus:

| After | executions in ~200 s |
|---|---:|
| finding 3 present, skipping one sample at a time | **350** |
| oracle bounded, crate unchanged | 13 360 |
| chunk numbers treated as monotone | **7 990 326** |

A 23 000× difference between the second and third rows, from a three-line
change, on inputs the fuzzer was already generating.

---

## Deferred

Named so the demuxer's author knows what is not here:

* **Metadata.** `udta ▸ meta ▸ ilst`, the `keys`-indexed QuickTime form, the
  3GPP `udta` boxes, `chpl` chapters and the iTunes key-conversion table.
  `Movie::udta` hands over the box unparsed. Plan 18 §2928 assigns the
  conversion table to this crate; it is a table with no dependency on anything
  else here and can be added without touching the parse path.
* ~~**Common encryption.**~~ Done 2026-08-23, extended 2026-08-28: `cenc`
  (below) parses `pssh`, `schm`, `tenc` (both versions), `saiz` and `saio`
  structurally, plus `senc`'s shape (`sample_count`, where its per-sample
  records start) **and now, `SampleEncryption::iv`, an individual sample's
  real IV** (full-sample encryption only — `has_subsamples` refuses, since a
  subsample table's variable record length needs a sequential scan this
  method does not do). `Movie::pssh` and `IsoFile::top_level_pssh` collect
  the two locations a `pssh` legally occupies. `SampleEntry::cenc()` ties
  `schm`/`tenc` to one sample entry, alongside the pre-existing
  `original_format`. The **write** side landed 2026-08-28 too:
  `writer::sinf_cenc`/`senc`/`saiz`/`saio` are `vaco-mux-mp4`'s only intended
  caller, same as every other `writer` function — see that crate's doc file
  for the encrypt-and-mux path built on them, and `vaco-demux-mp4`'s for the
  decrypt-given-a-key path built on `SampleEncryption::iv`.
* ~~**`colr` and `tmcd`.**~~ Done 2026-08-28: `stsd::ColourInfo::parse` reads
  `colr`'s `nclx`/`nclc` CICP codes (an ICC profile type reports only its
  `colour_type`), and `SampleEntry::tmcd`/`TimecodeSampleEntry` expose a
  `tmcd` sample entry's fixed fields plus a `format()` helper that turns one
  sample's frame count into `HH:MM:SS:FF`. Both are box-layer only — mapping
  the CICP codes onto `vaco_color`'s enums and reading the `tmcd` track's
  actual sample is `vaco-demux-mp4`'s job.
* **Sample groups.** `sbgp`/`sgpd` are skipped.
* ~~**HEIF/AVIF item model.**~~ Box-layer parsing done 2026-08-28 (`heif`),
  and wired into `vaco-demux-mp4` on 2026-09-03: a `meta`+`mdat` file with
  no `moov` now opens, every coded item is a one-packet stream and a `grid`
  item is a `TileGrid` stream group — see `docs/format/vaco-demux-mp4.md`'s
  *HEIF/AVIF* section for what was measured. `ItemInfo` gained the
  `item_name` (a stream's `title` tag) and `ItemLocation` its
  `data_reference_index` for that. Still box-layer only here: `iovl`/`iden`
  derived items, `auxl`/`thmb` reference semantics, `irot`/`imir`/`clap`
  transformative properties (parsed as ordinary property boxes, not
  applied).
* **`cmov`.** zlib-compressed `moov`; plan 18 already tiers it as v0.2.
* ~~**Box writing.**~~ Done: `writer` (below). `build` still exists separately
  for deliberately-invalid fixtures; `writer` is the validated production path
  `vaco-mux-mp4` drives.
* **`tapt`, `gmhd`, hint tracks, `stsh`, `padb`, `subs`** — parsed structurally
  as unknown boxes, i.e. skipped by size.
