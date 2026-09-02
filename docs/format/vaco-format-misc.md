# `vaco-format-misc`

Layer 4. Seven demuxers — `ivf`, `ffmetadata`, `roq`, `flic`, `cdg`, `bink`,
`smk` — for FM-59 (planning `18-formats.md` §8.7's T3 remainder). Issues
#623/#624/#625.

The package this crate was scoped from names roughly sixty game-video and
legacy-video containers plus a handful of metadata/caption formats. This
crate implements seven of them: the two named as "worth more than their
size" (`ivf`, the AV1/VP9/VP8 test-vector container, and `ffmetadata`, the
reference's own metadata interchange format); three game-video containers
cheap enough to do well without a real encoder to test against (`roq`,
`flic`, `cdg`); and, in a later pass, `bink` and `smk`, the two id-Software-
era game containers in the deferred list with real users and real files in
the wild, added once `roq`/`flic`/`cdg` had proven the hand-built-fixture
technique out. The remaining ~53 names — `vmd`, `wsvqa`, `4xm`, and the rest
of the list in the original brief — are **not implemented**. See "What was
deferred" below.

---

## What it is

| Module | Format | Demux | Mux | Fixture source |
|---|---|---|---|---|
| `ivf` | On2/Duck IVF (VP8/VP9/AV1 test vectors) | full | full | `ffmpeg -c:v libvpx\|libvpx-vp9\|libsvtav1 -f ivf` |
| `ffmetadata` | `;FFMETADATA1` text metadata | full | **not here** — muxer already exists as `vaco_mux_stream::MUXER_FFMETADATA` | `ffmpeg -f ffmetadata`, plus hand-built files for the grammar's edge cases |
| `roq` | id Software RoQ (Quake III, RTCW) | chunk framing only, no video/audio decode | none (no public encoder exists) | hand-built from `Vaco-Spec-Ref idroq-format-doc`, cross-checked against `ffprobe` |
| `flic` | Autodesk FLI/FLC/FLX | chunk framing only, no pixel decode | none (no encoder in modern use) | hand-built from `Vaco-Spec-Ref compuphase-flic-doc`, cross-checked against `ffprobe` |
| `cdg` | CD+Graphics karaoke subchannel | full (fixed 24-byte packets, no header) | none (no encoder) | hand-built from `Vaco-Spec-Ref cdg-revealed`, cross-checked against `ffprobe` |
| `bink` | RAD Game Tools Bink (`BIK`/`KB2`) | chunk framing only, no video/audio decode | none (no public encoder exists) | hand-built from `Vaco-Spec-Ref multimedia-wiki-bink-container`, measured against `ffprobe`/`ffmpeg` 8.1 |
| `smk` | RAD Game Tools Smacker | chunk framing only, no video/audio decode | none (no public encoder exists) | hand-built from `Vaco-Spec-Ref multimedia-wiki-smacker`, measured against `ffprobe`/`ffmpeg` 8.1 |

"Cross-checked against `ffprobe`" (and, for `bink`/`smk`, "measured
against `ffprobe`/`ffmpeg` 8.1") means the same thing throughout: since no
encoder exists for any of these five formats, a file was hand-built
directly from the public format documentation, then fed to the reference
binaries to see how the *reference* frames it into streams and packets.
That is a black-box measurement of container *framing* (D6/D17) — it never
touches the reference's source, and it is exactly how five genuinely
surprising, undocumented behaviours were found (see each module's doc
comment): RoQ's audio/video packet merging depends on chunk *order*, not
chunk *type*; FLIC's keyframe flag is purely positional; CDG's
`probe_score` is a capped count of well-formed packets, not a fixed
constant; Bink's reference demuxer drifts a byte on odd-length frames and
that drift cascades into a hard failure on every following frame; Smacker's
video-packet payload carries an unidentified ~769-byte prefix beyond the
raw video chunk that this crate could not reverse-engineer with confidence
(see "What was deferred").

`bink`/`smk` needed a different measurement path than `roq`/`flic`/`cdg`:
their real decoders (`binkvideo`, `smackvid`) refuse to open a
framing-only synthetic fixture that lacks a valid Huffman tree or coded
bitstream, so `-show_streams`/`-show_packets` alone were not enough.
`ffmpeg -i FILE -c copy -f framemd5 -` was used instead — a stream-copy
path that only needs the codec found by name, not opened, which was enough
to measure container framing, packet sizes, timestamps and stream counts
without needing valid coded payloads.

No FFmpeg source was consulted (D7/D15). `ivf` and `ffmetadata`'s grammars
are public specifications/documentation (the IVF header, and
`ffmpeg-formats.html`'s own "Metadata" chapter); `roq`/`flic`/`cdg`/`bink`/
`smk` are documented in `Vaco-Spec-Ref idroq-format-doc` /
`compuphase-flic-doc` / `cdg-revealed` / `multimedia-wiki-bink-container` /
`multimedia-wiki-smacker`, all community or original-author references,
none of them FFmpeg's.

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

### `bink`: per-frame table, seek instead of drift

The header's frame index table gives each frame's own absolute byte
offset plus a keyframe bit packed into the low bit
(`raw_start & !1` for the offset, `raw_start & 1` for the flag). One
video stream plus one audio `Stream` per declared track (stereo decided
by flags bit 13, which is authoritative over the separate channels
field). Per-track audio sub-chunks are emitted with their length field
included as-is (unlike `smk`, Bink's length field is not stripped before
becoming packet payload); the remainder of the frame is the video packet.

The reference's own demuxer reads frames **sequentially** rather than
re-seeking to each frame's table offset, so an odd-length frame — legal
per the table's own accounting, just not something a real encoder
produces — causes it to read that frame's video chunk one byte short, and
every following frame inherits the drift, eventually cascading into a
hard "audio size in header > size of packet left" failure. This was
confirmed directly: an even-length final frame reproduces the reference
exactly, while an odd-length non-final frame reproduces the one-byte
drift and the following frame's failure. This crate's demuxer seeks to
each frame's own table offset before reading it, so it reproduces neither
the drift nor the cascading failure — a deliberate, documented divergence
, not a bug worth copying.

### `smk`: frame-size/type tables, and an unresolved packet-payload gap

The header carries fixed-size `FrameSizes`/`FrameTypes`/`AudioRate`
tables (up to `NUM_TRACKS = 7` audio tracks) and a frame-rate field whose
sign selects one of three unit conventions
(`frame_rate_time_base`'s documented three-case `match`). Extradata is
the four track-size `u32`s followed by the packed Huffman-tree bytes
(measured: 16 + 10 = 26 bytes for a minimal single-symbol tree). Per
frame: an optional palette chunk (bit 0 of the frame's size entry) is
skipped by byte count, then one length-prefixed chunk per active audio
track with its length (and, if compressed, `UnpackedLength`) fields
stripped before the payload becomes packet data, then the remainder of
the frame becomes the video packet.

The video-packet content itself has an unresolved divergence: the
reference's video packet for a palette-carrying frame is measurably 769
bytes longer than this crate's raw video-chunk bytes — consistent with a
1-byte flag plus a 256-entry RGB palette (`1 + 256*3 = 769`), but three
independent hash-matching attempts at reconstructing that exact byte
layout from the frame's own palette chunk all failed to match. Rather
than reverse-engineer an undocumented decoder-cooperative packing
convention — a materially different task from measuring container framing
— this crate emits the raw video-chunk bytes only and reports the gap
honestly.

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
| 25 | bink | signature | `BIKi`/`KB2a`-style tags | both accepted | matches |
| 26 | bink | frame table offset/keyframe bit | hand-built, multi-frame | `raw & !1` offset, `raw & 1` keyframe | matches |
| 27 | bink | stereo flag authority | flags bit 13 set, channels field disagreeing | bit 13 wins | matches |
| 28 | bink | even-length final frame | hand-built | framemd5 matches this crate's framing exactly | matches |
| 29 | bink | odd-length non-final frame | hand-built | reference drifts one byte, next frame fails to demux | confirmed via targeted fixture; not reproduced (seeks per-frame instead) |
| 30 | smk | frame-rate formula | positive/negative/zero raw values | three distinct unit conventions | same three-case formula, same outputs |
| 31 | smk | extradata layout | hand-built, minimal tree | 4×u32 (16) + tree bytes (10) = 26 | 26 |
| 32 | smk | audio packet payload | hand-built | length/`UnpackedLength` fields stripped, data only | matches |
| 33 | smk | video packet payload (palette frame) | hand-built | 769 bytes longer than raw video chunk | not reproduced — unresolved, see "What was deferred" |

Rows 1–7, 12–15, and 16–24 are measurements this session took directly
(the `ffprobe` invocations are reproducible from each module's doc
comment); rows 8–11 restate `vaco-mux-stream`'s own independently-measured
`ffmetadata` grammar findings, re-verified against the same reference
binary rather than trusted from that crate's doc comment. Rows 25–33 were
measured in a later session using the `ffmpeg -c copy -f framemd5`
technique described above, since `-show_streams`/`-show_packets` alone
could not get the reference's real decoders to open these two formats'
hand-built fixtures.

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

No format-specific options. All seven demuxers use `FormatOptions::default()`
internally (`ivf`/`roq`/`flic`/`cdg`/`bink`/`smk` do not even take a
`FormatOptions` parameter — nothing about them is configurable) and go
through the standard `vaco_limits::Limits::permissive()` budget for
allocation. `bink`/`smk` additionally bound their own header-declared
sizes structurally before the budget sees them — `MAX_TRACKS`/
`MAX_FRAMES`/`MAX_CHUNK` in `bink.rs`, `NUM_TRACKS` (a fixed 7, not
attacker-controlled) in `smk.rs`.

---

## Dependencies

`vaco-core`, `vaco-io`, `vaco-limits`, `vaco-packet`, `vaco-format-core`,
`vaco-codec-core` (for `CodecId`/`CodecParameters` — D14.1 permits this;
it is the *parser* layer, `vaco-parse-*`, that a format crate may not
depend on, and nothing here does), `vaco-sampfmt` and `vaco-chlayout`
(`roq`'s audio stream description). `bink` and `smk` added no new
dependencies.

---

## What was deferred

**`CodecId` variants: landed and wired.** `vaco-codec-core` gained
`Roq`, `RoqDpcm`, `Flic`, `Cdgraphics`, `Bink`, `BinkAudioDct`,
`BinkAudioRdft`, `Smacker` and `SmackAudio` (interface gap 21), and every
stream in `roq`, `flic`, `cdg`, `bink` and `smk` now sets one —
`-show_streams` prints the reference's own `codec_name` for all five
formats, verified with hand-built fixtures against both `vaco-probe` and a
real `ffprobe` run on the identical bytes. `bink`'s two audio ids are chosen
per track from the flags word's bit 12
(`multimedia-wiki-bink-container`). `smk`'s audio id is not fixed either:
an `AudioRate` entry's `compressed` bit decides whether a track is
`SmackAudio` or raw PCM (`PcmS16le`/`PcmU8`, by the existing bit-depth
flag) — found by running the uncompressed default fixture through
`ffprobe` and seeing `pcm_s16le` where `smackaudio` had been assumed. The
rest of the ~40 undone names in this package's original brief (`vmd`,
`idcin`, `wsvqa`, and so on) would still need their own variants when
someone gets to them.

**`smk`'s video-packet payload omits whatever the reference packages
alongside a palette-carrying frame** (measured ~769 bytes longer than the
raw video chunk; three hash-matching hypotheses about the exact byte
layout all failed). Reported as an honest, unresolved divergence rather
than guessed at further — see `planning/TECH-DEBT.md`.

**`bink` deliberately does not reproduce the reference's odd-length-frame
drift.** The reference reads frames sequentially and drifts a byte after
any odd-length frame, cascading into a hard failure on every following
frame; this crate seeks to each frame's own table offset instead, so it
neither drifts nor cascades. Confirmed mechanism, deliberate divergence —
see `planning/TECH-DEBT.md`.

**~53 container names not implemented**: `vmd`, `ipmovie`, `wsvqa`,
`wc3movie`, `dxa`, `cdxl`, `4xm`, `anm`, `bfi`, `bmv`, `c93`, `dfa`,
`ea`/`ea_cdata`, `film_cpk`, `gdv`, `hnm`, `idcin`, `iss`, `jv`, `mm`,
`mtv`, `mv`, `mvi`, `paf`, `psxstr`, `rl2`, `rpl`, `siff`, `smush`, `thp`,
`tiertexseq`, `tmv`, `yop`, `nsv`, `nuv`, `cine`, `r3d`, `dhav`, `moflex`,
`mgsts`, `aqtitle`, `mcc`, `rcwt`, `tedcaptions`, `tty`, `bin`, `xbin`,
`adf`, `idf`, `sbg`, `ico`, `apng`. `vmd`, `idcin` and `wsvqa` are the
same id-Software-era family as `bink`/`smk` and would likely take the same
technique, but were not pursued in this pass: the returns on `bink`/`smk`
had already dropped into open-ended reverse-engineering territory (the
`smk` palette-payload investigation) rather than straightforward framing
measurement, which was read as the signal to stop rather than push toward
a target count. Everything in this list was not researched at all.

**`roq`/`flic` seeking.** Both return `Error::Unsupported`. Neither format
carries an index; a real implementation would need either a full forward
scan to build one (expensive for `roq`, whose chunk sizes are not
predictable without reading them) or accepting an approximate byte-search
seek, and neither seemed worth it without a real fixture corpus to verify
seek accuracy against. `bink`/`smk` also do not implement seeking, for the
same reason.

**`roq`'s stereo DPCM sample-per-packet count** (`payload_len / 2`) is
reasoned from the interleaved-stereo convention, not independently measured
— no stereo fixture was checked against the reference's `-show_packets
duration` output. Flagged in the module doc as inferred, not measured.
