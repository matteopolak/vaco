# `vaco-format-misc`

Layer 4. Five demuxers — `ivf`, `ffmetadata`, `roq`, `flic`, `cdg` — for
FM-59 (planning `18-formats.md` §8.7's T3 remainder). Issues #623/#624/#625.

The package this crate was scoped from names roughly sixty game-video and
legacy-video containers plus a handful of metadata/caption formats. This
crate implements five of them: the two named as "worth more than their
size" (`ivf`, the AV1/VP9/VP8 test-vector container, and `ffmetadata`, the
reference's own metadata interchange format) plus three of the game-video
containers that are cheap enough to do well without a real encoder to test
against (`roq`, `flic`, `cdg`). The remaining ~55 names — `bink`, `smk`,
`vmd`, `wsvqa`, `4xm`, and the rest of the list in the original brief — are
**not implemented**. See "What was deferred" below.

---

## What it is

| Module | Format | Demux | Mux | Fixture source |
|---|---|---|---|---|
| `ivf` | On2/Duck IVF (VP8/VP9/AV1 test vectors) | full | full | `ffmpeg -c:v libvpx\|libvpx-vp9\|libsvtav1 -f ivf` |
| `ffmetadata` | `;FFMETADATA1` text metadata | full | **not here** — muxer already exists as `vaco_mux_stream::MUXER_FFMETADATA` | `ffmpeg -f ffmetadata`, plus hand-built files for the grammar's edge cases |
| `roq` | id Software RoQ (Quake III, RTCW) | chunk framing only, no video/audio decode | none (no public encoder exists) | hand-built from `Vaco-Spec-Ref idroq-format-doc`, cross-checked against `ffprobe` |
| `flic` | Autodesk FLI/FLC/FLX | chunk framing only, no pixel decode | none (no encoder in modern use) | hand-built from `Vaco-Spec-Ref compuphase-flic-doc`, cross-checked against `ffprobe` |
| `cdg` | CD+Graphics karaoke subchannel | full (fixed 24-byte packets, no header) | none (no encoder) | hand-built from `Vaco-Spec-Ref cdg-revealed`, cross-checked against `ffprobe` |

"Cross-checked against `ffprobe`" means: since no encoder exists for
`roq`/`flic`/`cdg`, a file was hand-built directly from the public format
documentation, then fed to `ffprobe` 8.1 to see how the *reference* frames
it into streams and packets. That is a black-box measurement of container
*framing* (D6/D17) — it never touches the reference's source, and it is
exactly how three genuinely surprising, undocumented behaviours were found
(see each module's doc comment): RoQ's audio/video packet merging depends on
chunk *order*, not chunk *type*; FLIC's keyframe flag is purely positional;
CDG's `probe_score` is a capped count of well-formed packets, not a fixed
constant.

No FFmpeg source was consulted (D7/D15). `ivf` and `ffmetadata`'s grammars
are public specifications/documentation (the IVF header, and
`ffmpeg-formats.html`'s own "Metadata" chapter); `roq`/`flic`/`cdg` are
documented in `Vaco-Spec-Ref idroq-format-doc` / `compuphase-flic-doc` /
`cdg-revealed`, all community or original-author references, none of them
FFmpeg's.

---

## How it works

### The shape every format here shares

Small (or absent) fixed header, then a flat sequence of typed,
length-prefixed chunks. Demuxing is "read a header, then loop: read a
chunk header, decide what it means, emit zero or one packets, repeat until
EOF." None of these formats need a parser reached through `ParserProvider`
— there is no bitstream to hand off to, only container framing — so none of
`ParserProvider`'s three `open` functions ever call it.

### `ivf`: existing `CodecId`s, real mux+demux

The only format in this crate where the elementary codec (`Vp8`/`Vp9`/`Av1`)
already has a `CodecId` variant, so it is the only one with a working
`Muxer` as well as a `Demuxer`. Per-frame keyframe detection reads the
codec's own bitstream header directly rather than guessing:

* VP8: the frame tag's low bit (`Vaco-Spec-Ref rfc-6386` §9.1).
* VP9: the `uncompressed_header`'s `frame_type` bit, correctly skipping the
  profile-3 reserved bit (`Vaco-Spec-Ref vp9-bitstream-spec-v0.6` §6.2).
* AV1: presence of a sequence-header OBU in the temporal unit
  (`Vaco-Spec-Ref aom-av1-spec` §5.3.1) — a documented heuristic, not a full
  `frame_type` parse, since that needs the sequence header's own state.

### `ffmetadata`: demuxer only, on purpose

`vaco_mux_stream::MUXER_FFMETADATA` already exists and is registered under
the name `ffmetadata`. Writing a second muxer here would either collide
with that registration or duplicate its (already-measured) escaping logic
for no reason. This crate supplies only the read side, independently
re-derived from `ffmpeg-formats.html`'s grammar description and its own
round of measurement against `ffmpeg`/`ffprobe` 8.1 — not by reading
`vaco-mux-stream`'s source, so the two implementations are independent
checks of the same grammar rather than one copied from the other.

Chapters and global/per-stream tags are exact. `[STREAM]` sections'
phantom zero-information data streams (the reference's own quirk: a
`1/90000`-time-base stream whose duration is derived from the *chapter*
list, not from anything the `[STREAM]` section itself states) are read as
tags but not surfaced as `Stream`s — see the module doc for the measurement
and why reproducing it was judged not worth the chapter/stream coupling it
would introduce.

### `roq`: order-dependent packetisation

A video packet accumulates whole chunks (header + payload, byte for byte)
until a `RoQ_QUAD_VQ` chunk arrives, then flushes. A
`RoQ_SOUND_MONO`/`RoQ_SOUND_STEREO` chunk becomes its own audio packet
**only if nothing is currently accumulating** — put a sound chunk between a
codebook and its VQ chunk (legal chunk-id sequencing, just not what any
real encoder writes) and it silently becomes part of the video packet
instead, and no audio stream appears at all. This was found by building the
same three chunks in both orders and diffing `ffprobe`'s stream count.

### `flic` and `cdg`: positional keyframes

Both mark exactly packet/frame index 0 as a keyframe and nothing else —
confirmed by putting a "boring" chunk type (FLIC's `BLACK`, a full-black
frame with no image data) at index 0 and a "real" one (`BYTE_RUN`, a
complete image) at index 1, and watching the flag stay with the position,
not the chunk type.

### `cdg`'s `probe_score`

Not a constant. It is `min(n, 85)`, where `n` counts complete 24-byte
packets in the probed prefix whose command byte's low six bits equal
`0x09`. Measured by holding a file's packet count fixed at values from 20
to 1000: the score tracks the count 1:1 up to 80, lands on 85 at 90, and
stays there through 1000. A file of all-zero bytes loses to an unrelated
format's probe outright — this is a genuinely weak content test, which
fits a format with no magic number at all.

---

## The N-row comparison table

One comparison loop, run over every fixture this crate has, diffing
`vaco-probe -show_streams -show_format -show_packets` output shape against
hand-recorded `ffprobe` 8.1 measurements (a live differential run through
`vaco-probe` itself was not possible this session — see "What is deferred"
in the crate-level agent report for why). Each row is a fact this crate's
code encodes and a fixture exists to check it against.

| # | Format | Fact checked | Fixture | Reference value | This crate |
|---|---|---|---|---|---|
| 1 | ivf | `probe_score` | `ffmpeg -c:v libvpx -f ivf` | 98 | 98 |
| 2 | ivf | header field layout (32-byte, `rate`/`scale` at 16/20) | same | confirmed via `xxd` | matches |
| 3 | ivf | `r_frame_rate` vs `avg_frame_rate` | same | `25/1` / `0/0` | `25/1` / `0/0` |
| 4 | ivf | frame header (12 bytes: size, then 8-byte ts) | same | confirmed via `xxd` | matches |
| 5 | ivf | VP8 keyframe flag | 25-frame clip | `K` on frame 0 only | frame-tag low bit, same result |
| 6 | ivf | VP9 keyframe flag | 25-frame clip | `K` on frame 0 only | `uncompressed_header` parse, same result |
| 7 | ivf | AV1 keyframe flag | 25-frame clip | `K` on frame 0 only | sequence-header-OBU heuristic, same result |
| 8 | ffmetadata | probe magic | `;FFMETADATA1`/`2`/no digit/no header line at all | all score 100 once forced; `;FFMETADAT` (one char short) scores 0 | same for all five |
| 9 | ffmetadata | escaping | `title=bike\\shed` | `\\` → `\` | matches |
| 10 | ffmetadata | escaped newline | `comment=multi\`+newline+`line` | continues the value | matches |
| 11 | ffmetadata | chapter `END < START` | hand-built | reference refuses to open | `Error::InvalidData` |
| 12 | roq | signature | `84 10 FF FF FF FF 1E 00` | accepted | matches |
| 13 | roq | sound-before-codebook | hand-built | 2 streams, 2 packets/group | 2 streams, 2 packets/group |
| 14 | roq | sound-between-codebook-and-vq | hand-built | 1 stream, 1 packet/group | 1 stream, 1 packet/group |
| 15 | roq | audio sample rate | hand-built | fixed 22050 Hz | 22050 Hz |
| 16 | flic | probe magic | `0xAF11`/`0xAF12`/`0xAF44` accepted, `0xAF30`/`0xAF31` rejected | matches | matches |
| 17 | flic | packet framing | 2-frame file | 1 packet per `0xF1FA` chunk, whole bytes | matches |
| 18 | flic | keyframe | `BLACK` at 0, `BYTE_RUN` at 1 | `K` on 0 only | matches |
| 19 | flic | `frames` header field | claims 100, file holds 3 | 3 packets, `nb_frames` absent | 3 packets |
| 20 | flic | frame rate formula | FLI `speed=7`, FLC `speed=66` | `10/1`, `500/33` | `10/1`, `500/33` |
| 21 | cdg | packet framing | mixed valid/invalid command bytes | every 24 bytes is a packet | matches |
| 22 | cdg | `probe_score` formula | 20/50/90/1000-packet files | `20`/`50`/`85`/`85` | same formula, same outputs |
| 23 | cdg | keyframe | 10-packet file | `K` on packet 0 only | matches |
| 24 | cdg | dimensions | any file | fixed `300×216` | `300×216` |

Rows 1–7, 12–15, and 16–24 are measurements this session took directly
(the `ffprobe` invocations are reproducible from each module's doc
comment); rows 8–11 restate `vaco-mux-stream`'s own independently-measured
`ffmetadata` grammar findings, re-verified against the same reference
binary rather than trusted from that crate's doc comment.

---

## How to change it

* **Adding a format**: one new module, one `probe`/`DEMUXER` (and `MUXER`
  if genuinely symmetric — see "demux-only" note below), one entry in
  `vaco-component.toml`, `cargo xtask gen-registry`. Follow an existing
  module's shape; `ivf.rs` is the most complete template (probe, demux,
  mux, seek, tests), `roq.rs` the most complete demux-only one (lookahead
  buffering, lazy stream discovery).
* **Demux-only is the default for this family.** Only write a `Muxer` when
  a real encoder exists to check the byte layout against (`ivf`) or the
  reference names *this crate* as the place to put it (it does not, for
  `ffmetadata` — see above). A muxer nobody can differentially test against
  is a liability, not a feature.
* **Every chunk-length field is attacker-controlled.** Route it through
  `vaco_limits::Budget` (`alloc`/`incremental`, never `Vec::with_capacity`),
  and bound it structurally too (`MAX_FRAME`/`MAX_CHUNK`/`MAX_FILE`
  constants in each module) before the budget even sees it — the crate-wide
  `clippy.toml` denial catches the `with_capacity` spelling but not
  `vec![x; n]`/`resize`/an unbounded loop, which this family's own past
  incidents (plan 18's `isom_sample_table` slow unit) show is exactly where
  a length-prefixed chunk format hides a denial-of-service.
* **A new game-video/audio codec needs a `CodecId` variant added to
  `vaco-codec-core` first** — see "What is deferred" below. This crate does
  not and should not add one itself (D14.1: it would be a `vaco-format-*`
  crate touching a `vaco-codec-core` decision that other format crates
  share).

---

## Configuration

No format-specific options. All five demuxers use `FormatOptions::default()`
internally (`ivf`/`roq`/`flic`/`cdg` do not even take a `FormatOptions`
parameter — nothing about them is configurable) and go through the standard
`vaco_limits::Limits::permissive()` budget for allocation.

---

## Dependencies

`vaco-core`, `vaco-io`, `vaco-limits`, `vaco-packet`, `vaco-format-core`,
`vaco-codec-core` (for `CodecId`/`CodecParameters` — D14.1 permits this;
it is the *parser* layer, `vaco-parse-*`, that a format crate may not
depend on, and nothing here does), `vaco-sampfmt` and `vaco-chlayout`
(`roq`'s audio stream description).

---

## What was deferred

**Missing `CodecId` variants — the interface gap that matters most here.**
`vaco-codec-core` has no variants for any game-video or game-audio codec:
`Roq`/`RoqDpcm`, `Bink`/`BinkAudio`, `Smacker`/`SmackerAudio`, `Flic`,
`Cdgraphics`, and the rest of the ~55 undone names would need the same.
Every stream in `roq`, `flic` and `cdg` therefore carries `codec_id: None`,
so `-show_streams` prints `codec_name=unknown` where the reference prints a
real name — reported here rather than worked around, per this crate's
scope (`vaco-codec-core` is not owned by this package). See
`planning/INTERFACE-GAPS.md` for the full entry.

**~55 container names not implemented**: `bink`, `smk` (Smacker), `vmd`,
`ipmovie`, `wsvqa`, `wc3movie`, `dxa`, `cdxl`, `4xm`, `anm`, `bfi`, `bmv`,
`c93`, `dfa`, `ea`/`ea_cdata`, `film_cpk`, `gdv`, `hnm`, `idcin`, `iss`,
`jv`, `mm`, `mtv`, `mv`, `mvi`, `paf`, `psxstr`, `rl2`, `rpl`, `siff`,
`smush`, `thp`, `tiertexseq`, `tmv`, `yop`, `nsv`, `nuv`, `cine`, `r3d`,
`dhav`, `moflex`, `mgsts`, `aqtitle`, `mcc`, `rcwt`, `tedcaptions`, `tty`,
`bin`, `xbin`, `adf`, `idf`, `sbg`, `ico`, `apng`. `bink` and `smk` are
close enough to in-scope to research: their chunk/frame-index-table
framing is publicly documented on the MultimediaWiki well enough to
demux structurally (see the agent report for the specific layouts found),
but implementing, testing and fixture-building both to the same standard
as the five above did not fit this session. Everything else was not
researched at all.

**`roq`/`flic` seeking.** Both return `Error::Unsupported`. Neither format
carries an index; a real implementation would need either a full forward
scan to build one (expensive for `roq`, whose chunk sizes are not
predictable without reading them) or accepting an approximate byte-search
seek, and neither seemed worth it without a real fixture corpus to verify
seek accuracy against.

**`roq`'s stereo DPCM sample-per-packet count** (`payload_len / 2`) is
reasoned from the interleaved-stereo convention, not independently measured
— no stereo fixture was checked against the reference's `-show_packets
duration` output. Flagged in the module doc as inferred, not measured.
