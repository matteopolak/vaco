# `vaco-demux-matroska`

Layer 4. The Matroska and WebM demuxer, and — for now — the whole EBML layer
with it.

It is the project's **second container**, and that is most of its value.
`vaco-format-core`'s traits were designed against three shapes: MP4
(index-based), Matroska (cue-based) and MPEG-TS (streaming, no index). This
crate is the first cue-based one to land, so what follows records which parts of
that design held and which did not.

---

## What it is

| Module | Contents |
|---|---|
| `ebml` | the Matroska schema (RFC 9559 §5's 220-row `ebml::schema` table) and the functions that read it; the generic RFC 8794 grammar it used to hold inline now lives in `vaco-format-ebml` and is re-exported here unchanged |
| `ebml::schema` | every element RFC 9559 §5 and RFC 8794 §11.2 define, with its ID, type and parent |
| `block` | `Block`/`SimpleBlock` headers and all four lacings |
| `codec` | `CodecID` string → `CodecId`, the whole `draft-ietf-cellar-codec` registry |
| `probe` | content detection |
| `synth` | a minimal EBML **writer**, for fixtures no encoder we have can produce — now a thin wrapper over `vaco-format-ebml`'s writer, keeping only the Matroska-specific block/lacing/fixture builders |
| *(private)* `demux` | `MatroskaDemuxer` |

**2026-08-23: the generic EBML layer moved to `vaco-format-ebml`.** This
crate's own docs used to say the module was "kept behind a module boundary
... so that it can be promoted to `vaco-format-ebml` unchanged if a Matroska
muxer ... wants it" — `vaco-mux-matroska` is that muxer, and the promotion
happened exactly as predicted: `ebml/mod.rs`'s VINT codecs, `Header`/`Size`/
`Caps`, the `Slice` reader, the value accessors, `read_header` and the
mechanical half of `Stack` (frame push/pop/depth/bound, but not the
schema-driven `is_child_of`/`is_root` answers) are now re-exports from
`vaco-format-ebml`, byte-for-byte the same functions under the same names.
What stays here is genuinely Matroska-specific: `ebml::schema`'s 220-row
table (RFC 9559's element tree, not RFC 8794's business), the `ElementKind`/
`ElementDef`/`lookup`/`is_child_of`/`is_root` functions that read it, and a
`Stack` wrapper that closes the generic stack over this crate's own schema so
every existing call site — including the `matroska_ebml` fuzz target — kept
its exact one-argument `terminations_for(id)` shape. All 76 of this crate's
pre-existing tests pass unchanged; see `docs/format/vaco-format-ebml.md` for
the crate the layer moved into, and `docs/format/vaco-mux-matroska.md` for
what it enabled.

## How it works

### Two parsers, split by whether the element is bounded

`Info`, `Tracks`, `Cues`, `Tags`, `Chapters`, `Attachments` and `SeekHead` are
read whole into a budgeted buffer and walked in memory (`ebml::Slice`). They
always carry a known size, they are bounded by the file, and a slice walker is
both simpler and faster than a streaming one.

`Cluster`s are streamed, because one may be of unknown size, may be arbitrarily
large, and in a live WebM arrives on a pipe. That path is a small state machine
over `ebml::Stack` plus a packet queue — a queue because one laced `Block` is
many packets.

### The time base is not the usual one

`TimestampScale` (RFC 9559 §5.1.2.9) is **nanoseconds per tick** and defaults to
1 000 000, so the stream time base is `1/1000`. It is one value for the whole
segment: every track shares it. A file with `TimestampScale = 100` gets
`1/10000000`, and `tests/demux.rs` pins that against `ffprobe`, because it is
the case an implementation that assumed milliseconds gets silently wrong.
Once a `DefaultDuration` or `BlockDuration` has been quantised to that clock,
the resulting tick count remains its rational duration; it is not converted
through microseconds a second time. The one-nanosecond-clock regression keeps
`26,122,448` ticks as `1632653/62500000` seconds.

`Info/Duration` is different: RFC 9559 stores it as a floating-point count of
timestamp-scale units, so it may legitimately include a fractional tick. The
demuxer converts the float's shortest round-trippable decimal spelling directly
to a rational duration after applying `TimestampScale`; it never rounds it to
an integer tick. This preserves both the measured `12345.6789`-tick payload and
scientific-notation sub-tick values without a microsecond intermediate.

### Unknown sizes, and why the schema table exists

RFC 8794 §6.2: an unknown-size element ends at the first element that is *not*
one of its legal children — a parent, a sibling, a root, or the end of a
known-size ancestor. Deciding that needs two things: the schema, and knowing
what is currently open. `ebml::schema::ELEMENTS` is the first;
`ebml::Stack::terminations_for` is the second, and it is a direct transcription
of the rule, including the two cases people get wrong:

* an ID the schema does **not** know terminates nothing — it is skipped by its
  size, because §6.2 lists only *valid* elements as terminators;
* a **known-size** frame is never ended early — an unexpected child inside one
  is a corrupt child to skip, not a terminator, because its size already says
  where it ends.

`Void` and `CRC-32` are global: legal inside any master, terminating nothing.

The table holds the 207 elements RFC 9559 §5 defines plus the 13 EBML-header and
global ones. The registry in §27.1 lists 254; the difference is the deprecated
and reserved set (`Slices`, `TrickTrack*`, `SilentTracks`, `AspectRatioType` …),
left out on purpose so they get exactly the treatment a Matroska v4 reader owes
them.

### Lacing

All four modes, in `block`. The lace header is one octet holding *frames minus
one*, so a lace can never declare more than 256 frames — the one bound the
format hands us for free. Everything else is checked against bytes actually
present:

| Mode | Sizes | Failure mode guarded |
|---|---|---|
| none | — | — |
| Xiph | runs of `0xFF` plus a terminator | a size claiming more than the block holds |
| EBML | first as a VINT, rest as signed deltas | a delta driving the running size negative |
| fixed | none; `total / count` | a division with a remainder |

Timestamps follow RFC 9559 §10.3.5: the block timestamp is the first frame's,
and later frames are spaced by `DefaultDuration` when the track states one and
by `BlockDuration / count` when it does not. Every frame of a lace reports the
*block's* byte position, which is what the reference reports.

### Timestamps and the three trims

* **`CodecDelay`** (ns, per track) is subtracted from every timestamp on that
  track and reported as `AudioParameters::initial_padding` in samples. The
  first packet since the last discontinuity — the open, **or any seek** — also
  carries `SkipSamples { start, end: 0 }`.
* **`SeekPreRoll`** is read and kept on the track (for a future muxer
  round-trip) but **measured to have no effect on anything this crate
  produces**. `ffmpeg -v debug -ss <target> -i opus.webm -f null -` logs
  `demuxer injecting skip 312 / discard 0` — exactly the track's own
  `CodecDelay` sample count — after a seek to `0.0s` and again after a seek to
  `2.0s`, and the number does not move even with `SeekPreRoll` patched to zero
  in the file bytes. So the reference re-arms `CodecDelay`'s own skip on every
  discontinuity; it does not derive a skip from `SeekPreRoll` at all. The gap
  this closes is not "`SeekPreRoll` is unread" — it is that the skip was only
  ever applied once, at open, and a seek left `emitted_delay` permanently
  `true`. `Track::needs_delay_skip` now re-arms on every
  `reset_stream_state`, which every seek path calls, and
  `a_seek_rearms_the_codec_delay_skip_on_the_next_packet` pins the finding
  against the value patch above (an equivalent test, in Rust, is not
  possible without a real Opus stream; the ffmpeg debug log is the primary
  source and is reproduced in the test's doc comment).
* **`DiscardPadding`** (ns, per block) becomes `SkipSamples { start: 0, end }` on
  the last frame of that block, and **does not** shorten the packet duration:
  `BlockDuration` is already the trimmed length. When the same frame is also
  the first one since a discontinuity, `start` and `end` are combined into one
  `SkipSamples` — `Packet::set_side_data` **replaces** rather than merges an
  existing entry of the same kind, so writing the two trims as two calls would
  make the second one silently erase the first for a one-frame lace.

The `CodecDelay` conversion is the one arithmetic decision here that is not in
any specification, and it was measured rather than guessed — see *Measured
against the reference* below.

### Recovery

Two paths, both exercised by fixtures:

1. A level-1 element whose declared end runs past the `Segment` or past the file
   is **refused, not skipped**. Skipping by a corrupt size lands the scan at end
   of input having found nothing.
2. When the scan stops before the `Segment`'s end — at an unknown-size cluster,
   at a corrupt size, or at end of input — `SeekHead` is followed, every
   position validated by reading the element there and checking its ID. If the
   first `Cluster` was never reached, its position is recovered from `Cues` the
   same way. A scan that ran to the end skips both: it has already seen
   everything, and a `SeekHead` that lies could only make things worse.

Both came out of a fuzz finding; see *Fuzzing*.

## Measured against the reference

`ffprobe 8.1`, on files this crate and `ffmpeg` both read. Each row is a fact no
specification states, so each is reproducible from the command given.

| Question | Answer | How |
|---|---|---|
| `format_name` for a `DocType=webm` file | `matroska,webm` — **not** `webm` | `ffprobe -show_entries format=format_name x.webm` |
| Demuxer MIME type | none; extensions `mkv,mk3d,mka,mks,webm` | `ffprobe -h demuxer=matroska` |
| `probe_score` | 100 for both doc types | `ffprobe -show_entries format=probe_score` |
| `CodecDelay` → timestamps | delay is rounded to the nearest **tick first**, then subtracted from the integer block timestamp | an MP3 track with `CodecDelay` 25 056 689 ns on a 1 ms base reports its first packet at **-25**. Converting in the nanosecond domain and flooring gives -26. |
| `CodecDelay` → `initial_padding` | `round(ns × rate / 1e9)` | 25 056 689 ns at 44 100 Hz → 1105, not the 1104 truncation gives |
| Packet duration from `DefaultDuration` | quantised to the time base **before** becoming a duration | 26 122 448 ns reports 26 ticks / `0.026000`, not `0.026122` |
| `DiscardPadding` and duration | side data only; `BlockDuration` is already trimmed | the block with `BlockDuration=7` and `DiscardPadding=13 500 000` reports duration 7 |
| `Packet::pos` | the **block element's data offset** | a `SimpleBlock` whose data starts at 803 reports `pos=803`; a `Block` inside a `BlockGroup` whose data starts at 87 229 reports 87 232 |
| Stream metadata order | `title` (from `Name`), then `language`, then the `Tags` in file order | `sub.mkv` prints `language, ENCODER, DURATION` |
| `Language` of `und` | omitted | the schema default; the reference prints no `language` tag for it |
| Container metadata order | `Info/Title` as `title` first, then `Tags` verbatim | `title, ARTIST, ENCODER` |
| Chapter time base | `1/1000000000` | `ffprobe -show_chapters` |
| `sample_aspect_ratio` with no `DisplayWidth` | `1:1` | RFC 9559 table 8 makes the default the cropped pixel size |
| Element exceeding its parent | refused | `"exceeds containing master element"` in the reference's own log, and it still reads the file |

Verified equal on `av.mkv`, `av.webm`, `live.webm` (unknown-size `Segment`),
`sub.mkv`, `mp3.mka`, `off5.mkv` and seven synthesised files: stream count and
order, `codec_name`, `time_base`, resolution, `r_frame_rate`, dispositions,
metadata, chapter list, container duration, and — packet by packet — `pts`,
`duration`, `size`, `pos`, flags and `SkipSamples` side data. `cargo run -p
vaco-demux-matroska --example mkvdump -- <file> --packets` reproduces our half.

### The two frame rates, and the `duration_ts` we deliberately do not set

`Stream` grew an `r_frame_rate`/`avg_frame_rate` pair on 2026-08-22. Matroska
states exactly one rate — `DefaultDuration`, in nanoseconds per frame — and it
answers **both**: `av.mkv`'s 40 000 000 ns video track reports
`r_frame_rate=25/1` and `avg_frame_rate=25/1`. A track that states no
`DefaultDuration` leaves both at `0/0` and `Discovery` estimates them, which is
the right split: a rate derived from observed packet spacing is not something
this container stated.

`duration_ts` is **not** set here, and the measurements are why.

| file | subtitle `duration_ts` | its own `DURATION` tag | container duration |
|---|---:|---:|---:|
| `sub.mkv` (subtitle only) | 2000 | 2.000 | 2.000 |
| `as.mkv` (opus + subtitle) | **2008** | 2.000 | 2.008 |
| `as2.mkv` (subtitle ends at 1.0 s) | **2008** | 1.000 | 2.008 |
| `live_as.mkv` (piped, no `Duration` element) | `N/A` | 1.000 | `N/A` |
| `av.mkv`, `vs.mkv`, `avs.mkv`, `v.mkv`, `a.mka` | `N/A` | — | — |

The per-track `DURATION` tag is not the source — `as2.mkv` separates them. The
track's own extent is not the source either — `as2.mkv`'s subtitle stops at
1.0 s. What is printed is the *container* duration, handed to a stream that has
no timing of its own, and `live_as.mkv` proves it: remove the `Duration`
element and the field disappears with it.

So this is a container-wide rule, not a per-track statement, and it lives in
`Discovery::finish` — where filling it in a demuxer would have disabled the
shared rule for every caller that does run discovery, which is the same reason
this crate declined to set `start_time`. See `docs/format/vaco-format-core.md`.

### Two numbers we do not produce, and neither is ours to produce

* **Audio packet durations where the track states no `DefaultDuration` and the
  block no `BlockDuration`.** The reference prints 20 ms for Opus in Matroska;
  that comes from parsing the Opus TOC, which reaches a demuxer through
  `ParserProvider` and is `vaco-parse-opus`'s job. We report 0 and let the
  generic layer fill it.
* **`start_time` for a track with `CodecDelay`.** The first Opus packet's `pts`
  is `-7` in both implementations, and the reference nonetheless reports
  `start_pts=0` for that stream. The rule that closes the gap is "first `pts`
  plus `initial_padding`", it belongs in `vaco-format-core`, and it now lives
  there: `Discovery::finish` converts the priming from samples into the stream's
  time base with `rescale_rnd` — exactly, because for Matroska the two units
  genuinely differ — and offsets the first `pts` by it.

  **Verified end to end.** `discovery_turns_a_codec_delay_into_a_zero_start_time`
  pins the synthesised case, and `--discover` on real media agrees with
  `ffprobe 8.1` on every fixture, per-stream and for the container:

  | file | `initial_padding` | ffprobe `start_pts` | ours |
  |---|---|---|---|
  | `av.mkv` audio | 312 | 0 | 0 |
  | `av.webm` audio | 312 | 0 | 0 |
  | `mp3.mka` | 1105 | 0 | 0 |
  | `off5.mkv` | — | 5000 | 5000 |

  **But nothing in the tree runs that pass.** `Discovery::new` has exactly three
  call sites in the workspace and all three are test files. `vaco-probe` reaches
  a demuxer as `(desc.open)(source, &Parsers)` → `Box<dyn Demuxer>` and reads
  `.streams()` straight off it (`vaco-probe/src/lib.rs:275`), so `start_time`
  stays `NONE` there and `format.start_time` — derived from the streams —
  goes with it.

  **This crate deliberately does not fill the gap**, for two reasons. The value
  needs packets, and `read_header` must not consume any: a pipe cannot give them
  back, which is the entire reason `Discovery` buffers and replays. And
  `Discovery` only fills `start_time` when it is `NONE`, so setting it here would
  not merely duplicate the rule — it would *disable* the one in
  `vaco-format-core` for every caller who does run the pass, and the two would
  silently disagree about which ran. The fix belongs at the composition point,
  in `vaco-probe`'s `open`.

## What is not here

Deliberate, and each is a documented divergence rather than an oversight:

* **`V_MS/VFW/FOURCC` and `A_MS/ACM`.** Their `CodecPrivate` is a
  `BITMAPINFOHEADER` / `WAVEFORMATEX` that must be unwrapped, and
  `vaco-format-riff` is still a stub. The tracks appear as streams with the
  right media type and no codec.
* **Encryption.** A track with `ContentEncodingType = 1` is kept as a stream and
  its blocks are skipped. `PacketSideData` has no encryption variant, so
  reporting it faithfully is blocked on `vaco-packet`.
* **bzlib and LZO content compression.** Neither has a permissive pure-Rust
  decoder clearing D10 Gate 3. zlib and header stripping — the two that occur —
  both work.
* **Attachment payloads.** An `AttachedFile` becomes a stream carrying
  `filename` and `mimetype` metadata; its bytes are not emitted as a packet.
* **Ordered chapters, multiple editions, linked segments.** The first
  `EditionEntry` wins; `ChapterFlagOrdered` is not honoured. Plan 18 §3.2.4
  step 13 already scoped this out.
* **`CRC-32` verification.** The element is parsed as a global and ignored;
  wiring it to `IoContext::start_checksum` needs `err_detect=crccheck` plumbing.
* **`webm_dash_manifest` mode.** Plan 18 §3.2.4 step 14 describes it as "a
  demuxer mode, not a separate parser" that reports DASH-relevant properties as
  container metadata, which read as a small, additive piece of work. Probing
  the reference does not bear that out: `ffprobe -f webm_dash_manifest` on a
  plain `ffmpeg`-written WebM — with `Cues` present, confirmed by searching the
  bytes for the `Cues` ID — fails immediately with `Error parsing Cues`, for
  every file tried, `-reserve_index_space`-muxed or not. Whatever shape of
  `Cues` (or preceding structure) the mode actually demands was not found
  within this pass's budget, and D7 rules out reading `libavformat`'s
  `webmdashdec.c` to find out. Left undone rather than guessed at; a follow-up
  needs either more probing budget or a real DASH-muxed corpus to compare
  against.

## Tags, chapters and attachments — the parts that reach each other

* **Target-scoped `Tags`.** `Targets ▸ TagChapterUID` and `▸
  TagAttachmentUID` route a tag's `SimpleTag`s to that chapter's or that
  attachment stream's metadata, the same way `TagTrackUID` already routed to a
  track. Measured against `ffprobe 8.1` on a hand-built file: a
  chapter-targeted `COMMENT` appears in that chapter's own `tags` next to
  `title`, and an attachment-targeted `DESCRIPTION` appears in that
  attachment's stream tags next to `filename`/`mimetype`. `TagEditionUID` is
  read and dropped — no edition is exposed as an entity to attach it to, and
  `ffprobe` has nowhere to show it either. `TargetTypeValue` is read and
  otherwise unused: a `Targets` naming only a type value, with no UID at all,
  measured to be indistinguishable from an untargeted tag — both land as
  container metadata.

  `Tags` is applied only after the whole segment scan (main pass **and**
  `SeekHead` recovery) has run, buffered as raw bytes in `pending_tags` until
  then. RFC 9559 does not order `Tracks`/`Chapters`/`Attachments` relative to
  `Tags`, and resolving a target against a UID table that is not fully built
  yet would silently drop a tag on a file that happens to write `Tags` first.

* **Nested `ChapterAtom`s are ignored, not flattened.** RFC 9559 lets a
  `ChapterAtom` contain child `ChapterAtom`s (sub-chapters). Measured against
  `ffprobe 8.1 -show_chapters` on a file with one top-level atom nesting two
  children: only the top-level atoms are printed, and the nested pair does not
  appear anywhere, flattened or otherwise. This crate's parser already reads
  only `EditionEntry`'s *direct* children — a nested `ChapterAtom` falls into
  the same catch-all as any other unrecognised child of the outer atom — so no
  change was needed here; `nested_chapter_atoms_are_ignored_like_the_reference`
  pins it against a regression toward "fixing" this into flattening.
* **Bisection seeking.** Without `Cues` a timestamp seek restarts at the first
  cluster. The generic index built from packets already covers the common case,
  and `FormatFlags::GENERIC_INDEX` lets the core do the rest.

## How to change it

* **Adding an element**: put its row in `src/ebml/schema.rs` (ID-sorted; a test
  asserts sorted and duplicate-free, and that every parent is itself a master),
  then read it in the relevant `parse_*`. The table is transcribed from RFC 9559
  §5 by section number — the parent column is the section's parent — so a new
  row should be copied from the RFC the same way.
* **Adding a codec**: one row in `src/codec.rs`, transcribed from
  `draft-ietf-cellar-codec` §3.3/§3.4/§3.5. The table is already complete
  against the draft; the rows that resolve to `None` are waiting on
  `vaco_codec_core::CodecId` variants, and filling one in is a one-word change.
  Finding 4 found the table itself sitting
  well behind what `CodecId` already offered — 28 rows, `V_MPEG1`/`A_AC3`/
  `A_TRUEHD` among them, had a matching variant sitting unused — so before
  assuming a row needs a new `CodecId` variant, check it is not simply an
  unwired existing one; `V_AVS2`/`V_AVS3` are the two rows in this crate's
  scope that genuinely still need one.
* **Changing timestamp arithmetic**: everything goes through
  `vaco_core::rescale_rnd` with an explicit `Rounding`. Do not add a second
  path; the rounding modes were measured against the reference and the tests
  that pin them name the file and the value.
* **Gotcha — `advance()` must consume input.** `read_packet` loops until the
  queue is non-empty, so a path through `advance` that neither consumes an octet
  nor sets `eof` is an infinite loop. Closing unknown-size frames costs no
  input, which is why it happens *before* the element is handled rather than by
  returning.
* **Gotcha — a `Block` has no keyframe bit.** Bit 7 is `KEY` only in a
  `SimpleBlock`; in a `Block` it is reserved, and the random-access signal is
  the *absence* of `ReferenceBlock` in the enclosing group (RFC 9559 §10.4).
* **Gotcha — `Tags` target `TrackUID`, not `TrackNumber`.** They are different
  numbers and a file where they coincide will hide the bug.

## Fuzzing

Two targets, because they run at different depths.

* `matroska_demux` — whole-file, `Limits::strict`. Asserts termination, stable
  `Eof`, that every packet names a declared stream, that every stream shares one
  time base, and that chapters are in nanoseconds.
* `matroska_ebml` — the grammar alone: the child walker stays inside its buffer,
  VINT decode and encode agree, and `Stack::terminations_for` is monotone and a
  fixed point.

**One real bug found.** Two mutated octets in a `Void` element's size VINT made
it claim 21 GB; the scan skipped by that, landed at end of input, and never
recorded where the first `Cluster` was — so a linear read produced *zero*
packets while a cue-driven seek produced all 22. The reference reads the same
bytes and reports 22. Both halves of the fix (refuse an over-long element;
recover the first cluster from `Cues`) are pinned by
`a_corrupt_element_size_before_the_clusters_still_yields_every_packet`.

## Configuration

`FormatOptions` as passed to `MatroskaDemuxer::open`: `max_streams` bounds the
track and attachment count (checked against the `Budget`'s own cap as well), and
the index options bound the `Cues`-derived index.
`open_with_limits` takes a `vaco_limits::Limits` explicitly — `Limits::strict`
for untrusted input, `Limits::permissive` (what `open` passes) for the CLI.

Crate-local ceilings, all in `src/demux.rs`:

| Constant | Value | Why |
|---|---|---|
| `MAX_HEADER_ELEMENT` | 256 MiB | attachments are legitimately large; this turns a 2^56 declared size into an error before any allocation |
| `MAX_BLOCK` | 256 MiB | one block element |
| `MAX_LEVEL1_ELEMENTS` | 2^20 | a `Segment` of a million empty `Void`s is an error, not a wait |
| `ebml::MAX_DEPTH` | 16 | `SimpleTag` and `ChapterAtom` are recursive, so a file names its own depth |
| `ebml::MAX_ID_LEN` / `MAX_SIZE_LEN` | 4 / 8 | `EBMLMaxIDLength` and `EBMLMaxSizeLength` are capped here regardless of what the header declares, and a *larger* declaration is refused rather than clamped |

## Dependencies

`vaco-format-core` (traits, probing, seeking), `vaco-format-ebml` (the generic
EBML VINT/reader layer, D19), `vaco-io` (`IoContext`), `vaco-packet`,
`vaco-codec-core`, `vaco-chlayout`, `vaco-color` (the data model),
`vaco-limits` (the budget), `vaco-core` (exact rational and timestamp
arithmetic), and `miniz_oxide` for zlib content encodings. No dependency on any
codec crate: parsers arrive through `ParserProvider` (D14.1).

## Did `vaco-format-core`'s traits fit a cue-based container?

Broadly yes. Five notes, in descending order of how much they matter.

1. **`Demuxer` being self-owning is the right shape.** The demuxer holds its own
   `IoContext` and does its own seeking, and for a format whose header elements
   may sit *after* the clusters — and whose `SeekHead` routinely lies — that
   freedom is what makes recovery expressible at all. A callback-style
   `DemuxCtx` owning the I/O, which plan 18 §1.2 sketched, could not have
   expressed "the scan stopped, now validate these positions and try again".

2. **`PacketIndex` fits `Cues` exactly, with one caveat that is worth writing
   down.** An index entry's `pos` must be a position the parser can *resume*
   from. For Matroska that is the enclosing `Cluster`, never the block — and
   nothing in the trait says so. Recording block positions instead type-checks,
   passes every unit test that does not seek, and produces garbage on the first
   seek. A doc line on `IndexEntry::pos` saying "a position the demuxer can
   restart parsing at" would have saved the round trip.

3. **`Stream::start_time` had no way to say "not from the first packet"** — now
   fixed in `Discovery::finish`, and verified against the reference on four
   files (see *Measured against the reference*). What the round trip exposed is
   a *composition* gap rather than a trait one: the frozen `DemuxerDesc::open`
   hands back a bare `Box<dyn Demuxer>`, and nothing in the design says who is
   supposed to wrap it. Today nobody does outside tests. A trait cannot enforce
   that; a documented opener could.

4. **`Demuxer::duration()` returning `Option<Duration>` fits, but `Stream::duration` has no natural filler.** Matroska has one container-level
   `Info/Duration` and no per-stream duration — and the reference prints none
   for a Matroska stream either, so leaving it `None` is correct. Worth stating
   in `vaco-format-core`'s docs, because "leave it `None`" looks like an
   omission.

5. **`SeekStrategy::choose` takes `has_index` as a bool, which loses a
   distinction Matroska has.** An index from `Cues` is authoritative; one built
   from packets already read covers only what has been seen. Both answer `true`,
   so a cue-less file with a partial index takes the `Index` path and lands
   correctly by luck rather than by design. Not wrong today — the entries it
   holds are real — but a `SeekStrategy` that could tell "complete" from
   "partial" would let a demuxer choose bisection when it should.

Nothing in the frozen traits turned out unsatisfiable.
