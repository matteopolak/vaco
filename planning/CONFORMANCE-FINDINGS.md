# What the differential harness found

`cargo run -p vaco-conformance -- run --tier core` over the three probe suites
in `tests/conformance/probe/`: **198 cases, 42 agreeing** on the first run,
**71 after one fix**. These are the findings, ranked by how much they cost.

A second pass (XF-03/XF-01, findings 8 onward) fixed several bugs in the
harness itself, built `tests/conformance/transcode/` — the remux matrix —
for the first time, and added six more probe suites (AVI, FLV, Ogg, ASF, the
audio-simple family, DV). Current totals, re-measured after every fix in this
document landed: **665 cases, 117 agreeing (17.6%)**, 0 failed to launch, 0
skipped. See "The numbers, before and after" below findings 1–7 for the
breakdown, and findings 8 onward for what changed.

Recorded here rather than filed as issues because the repository owner's
standing instruction is to fix rather than file. Each entry says whether it is
fixed, and if not, who it belongs to.

## 1. `-bitexact` drops every `*_long_name` — **fixed**

```sh
ffprobe -hide_banner -show_format av.mp4 | grep -c long_name           # 1
ffprobe -bitexact -hide_banner -show_format av.mp4 | grep -c long_name # 0
```

Nowhere documented, and obvious only in hindsight: a long name is descriptive
prose that changes between builds, which is exactly what `-bitexact` removes.

The consequence was severe out of proportion to the fix. Every `exact-bytes`
case in the harness runs under `-bitexact`, so this one field made **156 of 198**
cases diverge. Fixed in `vaco-probe`'s `Emit`, matched on the suffix rather than
a list of names so a new section's long name is covered without anyone
remembering to come back.

**42 → 71 agreeing cases from that single change.**

## 2. An MPEG-TS file probed as `av1`, then as `avs2` — **fixed**

`vaco-probe file.ts` reported `format_name=av1`, one stream, no programs. The
reference reports `mpegts`, one stream, one program.

Two independent causes, both in `vaco-demux-raw`, and both instances of the same
mistake: **a probe justified by what a malformed stream might survive rather
than by what a conforming one looks like.**

- **AV1.** `looks_like_obu_stream` accepted any non-reserved OBU type in first
  position. A transport stream opens with `0x47`, which reads as a valid
  `OBU_METADATA` header with a size field and parses cleanly. Now the first OBU
  must be a temporal delimiter or a sequence header, which is what §7.5 requires
  a conforming stream to open with.
- **Start-code formats.** `Framing::StartCode3` scored on `!start_codes(buf).is_empty()`
  — start codes *anywhere*. An MPEG-TS file's PES payloads are full of real
  elementary-stream start codes, so every one of the eleven `StartCode3` formats
  matched. The first start code must now be at offset 0 or 1, where a raw
  elementary stream actually begins.

The one-point margin is what made this bite: a structural raw hit scores **51**
and a confidently-detected transport stream scores **50**. Both numbers are
measured against the reference, so neither is wrong on its own — the bug was
entirely in the raw probes firing where the reference's do not.

## 3. Raw H.264 probes as `avs2` — **fixed**, `vaco-demux-raw`

The same root cause, second half. This entry originally said **eleven**
formats share `Framing::StartCode3`; counted directly from `bitstream.rs` it
is **ten** — `h264`, `hevc`, `vvc`, `m4v`, `mpegvideo`, `cavsvideo`, `avs2`,
`avs3`, `vc1`, `evc`. All ten agreed on any file that opens with a start code,
so ties broke alphabetically and `avs2` won. `vaco-probe raw.h264` said
`avs2`; the reference said `h264`.

The fix is the part of §2 that was deliberately not attempted: match the
start-code **identifier** — the byte or bytes after `00 00 01` — against the
format, in `bitstream.rs`'s new `start_code_identifier`. Measured with
`ffmpeg -f lavfi -i testsrc=d=0.5 -c:v <codec> -f <rawformat> out.bin`, read
back with `xxd`, for every member this `ffmpeg` 8.1 build has an encoder for:

| format | encoder | identifier | reference detects |
|---|---|---|---|
| `h264` | `libx264` | `0x67` (SPS) / `0x09` (AUD, with `aud=1`) | `h264` |
| `hevc` | `libx265` | `0x40 0x01` (VPS) / `0x46 0x01` (AUD, with `aud=1`) | `hevc` |
| `mpegvideo` | `mpeg1video` / `mpeg2video` | `0xB3` (`sequence_header_code`, identical for both) | `mpegvideo` |
| `m4v` | `mpeg4` | `0xB0` (`visual_object_sequence_start_code`) | `m4v` |

`avs2`, `avs3`, `cavsvideo`, `evc`, `vc1` and `vvc` have **no encoder in this
`ffmpeg` 8.1 build** — confirmed via `ffmpeg -codecs`, not assumed — so there
is no reference sample to read an identifier from. Per the brief, these six
make no structural claim and fall back to `ProbeScore::from_extension`; a real
elementary stream in one of these six formats is now honestly undetected by
content rather than dishonestly detected as whichever of the ten sorts first
alphabetically. `crates/format/vaco-demux-raw/tests/probe_matrix.rs` asserts
the whole shape: every one of the ten's own sample wins its own probe, no
`StartCode3` sibling ever outscores it, and the six unverified formats score
`NONE` the moment their filename extension is wrong (proving the win came from
the extension, not from an undisclosed structural claim).

**This was measured, not recalled** — the table above is the second time this
crate's probing was wrong in a way memory would not have caught (see §2): the
lesson repeats because the failure mode is the same shape twice, not because
the lesson was skipped the first time.

## 4. `codec_name=unknown` on most streams — **fixed for MPEG-TS and Matroska**, MP4 not touched

The largest remaining divergence class. `TsCodec::codec_id` in
`vaco-format-mpegts-tables` mapped eight of about thirty variants and returned
`None` for the rest, so `mpeg2video`, `mp2`, `flac`, `vorbis`, `vp8` and `alac`
all printed `unknown` where the reference names them. The MP4 and Matroska
demuxers had the same gap for their own codec tables.

The mapping was mechanical, but not for the reason it looked mechanical:
`vaco_codec_core::CodecId` already had a variant for most of the gap —
`Mpeg2video`, `Mp2`, `Ac3`, `Truehd`, `Vc1`, `Cavs`, `Dirac`, `Vvc`, `Mp1`,
`Alac`, `Ass`, `Ssa`, `Webvtt`, `DvdSubtitle`, `HdmvPgsSubtitle` and more were
sitting unused, not missing. (`Flac`, `Vorbis` and `Vp8` — the finding's own
named examples — are real: Matroska's `A_FLAC`/`A_VORBIS`/`V_VP8` rows were
already mapped before this fix; the unmapped instances of those three codecs
are in MP4, which was not touched — see below.)

* **MPEG-TS (`vaco-format-mpegts-tables`) — fixed.** `TsCodec::codec_id` now
  maps 21 of its ~30 variants (was 8), every one checked against an existing
  `CodecId` variant. Eight variants still have no `CodecId` counterpart at
  all — `Avs2`, `Avs3`, `Jpeg2000`, `DvbSubtitle`, `DvbTeletext`, `Scte35`,
  `TimedId3`, `Klv` — and their exact names/long names, probed from `ffmpeg
  -codecs` 8.1, are in `stream_type.rs`'s module docs for whoever owns
  `vaco-codec-core` next. Confirmed with a real `mpeg2video`+`ac3` transport
  stream: `vaco-probe -show_streams` printed `codec_name=unknown` for both
  before this fix and `codec_name=mpeg2video`/`codec_name=ac3` after it.
* **Matroska (`vaco-demux-matroska`) — fixed.** `src/codec.rs`'s `EXACT` table
  now resolves 28 rows that used to sit on `None` — `V_MPEG1`, `V_MPEG2`,
  `V_CAVS`, `V_DIRAC`, `V_FFV1`, `V_MPEGI/ISO/VVC`, the three `V_MPEG4/ISO/*`
  profiles, `V_MPEG4/MS/V3`, `V_PRORES`, `V_THEORA`, the three `A_AC3*` rows,
  `A_ALAC`, the three `A_DTS*` rows, `A_EAC3`, `A_MPEG/L1`, `A_MPEG/L2`,
  `A_TRUEHD`, `S_HDMV/PGS`, `S_TEXT/ASS`, `S_TEXT/SSA`, `S_TEXT/WEBVTT` and
  `S_VOBSUB`. `V_AVS2`/`V_AVS3` are the only two rows in this crate's scope
  still genuinely blocked on a missing `CodecId` variant (the same `Avs2`/
  `Avs3` gap MPEG-TS reports). One collateral fix: `tests/demux.rs`'s
  `a_track_whose_codec_has_no_codec_id_variant_still_becomes_a_stream` used
  `A_AC3` as its example of an unmappable codec and started failing *because*
  this fix mapped it — exactly the "never pin the absence of something the
  project is building" trap in `planning/AGENT-CONSTRAINTS.md`. Swapped to
  `A_MLP`, which is still genuinely unmapped.
* **MP4 (`vaco-demux-mp4`) — not touched, and could not be from this brief's
  scope.** The brief named `vaco-demux-mp4`'s codec-mapping table, but that
  table does not live in that crate: `SampleEntry::codec`/`sample_entry_codec`
  (the fourcc → `CodecId` table) and `EsDescriptor::codec` both live in
  `vaco-format-isom`, a crate this brief did not grant write access to.
  `vaco-demux-mp4` itself has no codec-mapping table of its own beyond a
  two-entry cover-art image-type lookup in `lib.rs`, unrelated to this
  finding. Per `planning/AGENT-CONSTRAINTS.md` ("If you need a change in a
  crate you do not own, stop and report — do not work around it"), this was
  left alone. `alac`/`flac`/`vorbis`/`vp8` in MP4 specifically (and this
  finding's own `flac`/`vorbis`/`vp8` examples, which read most naturally as
  MP4/Matroska streams rather than MPEG-TS ones — MPEG-TS has no `TsCodec`
  variant for any of the three) still print `unknown` and are
  `vaco-format-isom`'s to fix, the same shape of change as the two tables
  above.

## 5. Packet order and count — **open**, `vaco-demux-mpegps`

On an MPEG-PS file, `-show_packets` gives 101,845 bytes against the reference's
13,169, and starts with a video packet where the reference starts with audio.
Ours reports `pts=N/A` throughout. That is roughly eight times too many packets
with no timestamps, which reads like PES payloads being emitted without
reassembly.

## 6. The CLI reaches none of the 63 muxers — **assigned**, `vaco-cli`

```sh
vaco -hide_banner -i in.mp4 -c copy -f matroska out.mkv
#   Stream mapping:
#     Stream #0:0 -> #0:0 (copy)
#   [out#0/matroska] video:12KiB audio:0KiB … muxing overhead: unknown
echo $?   # 0
ls out.mkv   # No such file or directory
```

Exit 0, a plausible summary, and no file. `exec.rs::muxer_for` returns a format
*name* and the pipeline then always builds `nullmux::NullMuxer`, which counts
bytes and writes nothing. That was correct when D5 put zero muxers in v0.1 — the
module doc says so at length — and has been false since the container wave.

**Silent success is the worst failure mode available.** A user sees nothing
wrong; a test sees exit 0; a differential harness scores it a pass. It is also
why there is no `tests/conformance/transcode/` suite yet: there is nothing on
the writing side to compare.

Found by trying the obvious command while the harness was fresh in mind, which
is worth recording on its own — 2,935 unit tests and eight gates were all green
on a binary that could not write a file.

## 7. MP4's codec table, and why a `FourCc` table cannot work — **fixed**, `vaco-format-isom`

The MP4 half of finding 4. `sample_entry_codec` in
`crates/format/vaco-format-isom/src/stsd.rs` mapped about fifteen FourCCs and
collapsed nine of them onto a single `CodecId::Pcm`, so a QuickTime file with
16-bit little-endian audio printed `codec_name=pcm` where the reference prints
`pcm_s16le`.

Filling the table in was not enough, and that was the interesting part.
Measured 2026-08-23 by encoding one `.mov` per PCM variant and reading back
both the sample-entry FourCC and what `ffprobe` calls it — full table,
including the `enda` box and `sample_size` field measured for each row, now
lives in `docs/format/vaco-format-isom.md` rather than duplicated here.

**The FourCC does not determine the codec.** `sowt` covers both 16-bit and
8-bit; `in24`, `in32`, `fl32` and `fl64` each cover *both endiannesses*. So
endianness is not in the FourCC at all — it comes from the sample entry's
`enda` box, nested inside `wave` the same way `esds` is for old `QuickTime`
audio.

One correction to this finding's own original wording: it said width "comes
from `bits_per_sample`". True in outcome, false in mechanism — the classic
`sample_size` field is a fixed `16` placeholder for `in24`/`in32`/`fl32`/`fl64`
in every file measured, regardless of the real width, so the fix does not read
it for those four fourccs at all. Width is already fixed by the fourcc; only
`enda` was ever open. `sample_size` **is** read, and is accurate, for `sowt`
and `twos`, the two fourccs that actually vary in width. `lpcm`'s flavour
turned out to live somewhere else again — its version-2 body's `formatFlags` +
`constBitsPerChannel`, not the fourcc, `sample_size`, or `enda` (no `enda` box
is present on an `lpcm` entry at all).

Fixed via `SampleEntry::resolve_ambiguous`, which takes the whole entry —
media type, `bits_per_sample`, `enda`, the version-2 body — rather than the
bare fourcc `sample_entry_codec` still handles everything else with. `ulaw` →
`pcm_mulaw`, `alaw` → `pcm_alaw`, `mp4v` → `mpeg4` (via the ESDS
object-type-indication table, which is now the complete MP4RA registry rather
than eight hand-picked rows — see its own module doc for the `0xA5`/`0xA6`
"Withdrawn" trap that completeness caught), `h263` → `h263`, all six ProRes
tiers (`apco`/`apcs`/`apcn`/`apch`/`ap4h`/`ap4x`) → `prores`, and `alac` →
`alac` are now plain rows, all measured. `raw ` resolves to `pcm_u8` in an
audio sample entry and `rawvideo` in a video one, using the media type the
entry already carries; `2vuy`/`yuvs`/`24BG` (measured for free on the way) are
three more `rawvideo` fourccs.

No new `CodecId` variants were needed — every one this fix uses
(`PcmS8`/`PcmU8`/`PcmS16le`/`PcmS16be`/`PcmS24le`/`PcmS24be`/`PcmS32le`/
`PcmS32be`/`PcmF32le`/`PcmF32be`/`PcmF64le`/`PcmF64be`/`PcmAlaw`/`PcmMulaw`/
`Rawvideo`/`Mpeg4`/`H263`/`Prores`/`Alac`/`Vc1`/`Dirac`/`Vp9`/`Dts`/
`Jpeg2000`/`Mpeg1video`) already existed in `vaco-codec-core`, unused.

## The numbers, before and after (XF-03 / XF-01, this pass)

| | Cases | Agreed | Failed to launch |
|---|---|---|---|
| Original three probe suites, first run (findings 1–2 unfixed) | 198 | 42 | — |
| Same, after finding 1's fix | 198 | 71 | — |
| Same, re-measured at the start of this pass | 198 | 77 | 16 (all one `probe-matroska` fixture, finding 13) |
| Same three suites, after finding 13's fix | 198 | 85 | 0 |
| + `tests/conformance/transcode/` (4 new suites, 103 cases) | 301 | 89 | 0 |
| + 6 new probe suites (avi, flv, ogg, asf, audio-simple, dv; 364 cases) | **665** | **117** (17.6%) | **0** |

The aggregate agreement rate *fell* relative to the original three suites
(85/198 = 42.9% → 117/665 = 17.6%) — expected, not a regression. Every new
suite tests either a container the CLI could not write **at all** until this
week (the transcode matrix starts at 0% by construction: no muxer had ever
been checked against the reference before) or a demuxer whose codec table has
the same shape of gap finding 4 already described for MPEG-TS, just for a
different family (finding 24). A falling aggregate percentage that comes from
adding real, previously-untested surface is the harness doing its job, not
losing ground.

## Which containers reproduce themselves byte-for-byte under the reference

The foundation every future remux test rests on. Measured by running the
reference **twice** on the identical command line and diffing its own output
— not assumed. Each row is `-c copy` of a single H.264 (or, for the
audio-only row, PCM/AAC) stream synthesised by `testsrc`/`sine`, with
`-fflags +bitexact` positioned **after** the last `-i` and before the output
path (position matters — see finding 9).

| Container | Two reference runs | Notes |
|---|---|---|
| Matroska, AVI, MPEG-TS, FLV, MP4, MOV, ASF, WAV | **byte-identical**, every one | without correctly-positioned `-fflags +bitexact`, Matroska differs by its random Segment UID (finding 9) |
| HLS | **byte-identical**, playlist and segment both | an earlier run using differently-named outputs looked nondeterministic and was not — the segment filenames were derived from the differing playlist basenames, not from anything nondeterministic |
| DASH | **byte-identical**, manifest and both segments | no wall-clock `availabilityStartTime` leaks through under `-fflags +bitexact` |
| Ogg | not reachable for H.264 | the reference's own `ogg` muxer rejects H.264 outright ("Unsupported codec id in stream 0") — a container limitation, not a determinism question |

**The corollary worth writing down because it contradicts the starting
assumption**: the brief for this matrix predicted container bytes would
rarely be comparable because a muxer stamps a creation time, a random Segment
UID, or an encoder string on every run. Measured, that is true only *without*
`-fflags +bitexact` positioned correctly, and false everywhere it was checked
*with* it positioned correctly — including HLS's and DASH's segmented,
multi-file outputs. `tests/conformance/transcode/remux-bitexact.toml`
therefore uses `exact-bytes` (C0) as its primary tier, with a
`structured-diff` (C6) suite (`remux-structural.toml`) alongside it for the
different, still-real question of whether *our* muxer's differently-shaped-
but-valid bytes describe the same stream (findings 19–21 came from exactly
that suite).

## 8. `vaco` does not parse a bare `-flags` option — not ours to fix, worked around

```sh
$ vaco -hide_banner -nostdin -i in.mp4 -c copy -f matroska -fflags +bitexact -flags +bitexact out.mkv
Unrecognized option 'flags'.
Error splitting the argument list: Option not found
$ echo $?
8
```

The standard "make ffmpeg bitexact" recipe is `-fflags +bitexact -flags
+bitexact`, and the harness's `bitexact` invocation normaliser already
emitted both (pre-dating this pass). `vaco` accepts `-fflags` but rejects a
bare `-flags` outright, so every transcode-tool case using `bitexact` failed
to launch (exit 8) before this was diagnosed — the harness's own exit-code
co-assertion reported "exit codes differ: ours 8, reference 0" on all 80
cases in the first draft of the remux matrix, a true fact that was not the
fact the suite exists to check.

Not patched around silently: `-flags` only affects codecs actually invoked,
and `-c copy` invokes none, so `-fflags +bitexact` alone is provably
sufficient for two-run determinism on a copy (checked directly — see the
determinism table above, not assumed). `crates/tool/vaco-conformance/src/
normalise.rs` gained a second, narrower invocation normaliser,
`bitexact-copy`, emitting only `-fflags +bitexact`; the general `bitexact`
normaliser is unchanged (still both flags) because a case that *encodes*
still needs `-flags`, and quietly dropping it from every case's request
would understate what the harness is asking for. Belongs to `vaco-cli`/
`vaco-cli-core`: a bare `-flags` option is not parsed.

## 9. The `bitexact` normaliser put `-fflags`/`-flags` in the wrong place for transcode cases — fixed, harness bug

`-fflags`/`-flags` are *per-file* options. Placed before `-i` — exactly where
a simple "prepend this to argv" normaliser puts them — they configure the
**input**, not the output:

```sh
$ ffmpeg -y -hide_banner -loglevel error -fflags +bitexact -flags +bitexact -i h264.mp4 -c copy -f matroska pre_a.mkv
$ ffmpeg -y -hide_banner -loglevel error -fflags +bitexact -flags +bitexact -i h264.mp4 -c copy -f matroska pre_b.mkv
$ cmp pre_a.mkv pre_b.mkv
pre_a.mkv pre_b.mkv differ: char 221, line 1        # the muxer's random Segment UID
```

Moving the identical flags to anywhere after `-i` and before the output path
makes two reference runs byte-identical. This is exactly the trap
`planning/AGENT-CONSTRAINTS.md` already names — *"An option's position
decides which object it lands on... which reads as 'the muxer is
nondeterministic'"* — hit for real while building the fix the constraints
page had already warned about.

Fixed in `crates/tool/vaco-conformance/src/normalise.rs`:
`Invocation::is_positional_for(tool)` marks `bitexact`/`bitexact-copy` as
positional for the transcode tools; `Chain::argv_prefix` excludes them,
`Chain::positional_suffix` supplies them, and `Runner::run_case` splices the
suffix in immediately before the last argv element — which every transcode
suite in this repository arranges to be the output path.

## 10. `Capture::OutputFile` was declared but never implemented — fixed, harness bug

The `Compare` enum had an `output-file` capture variant before this pass, and
nothing read it: `compare::exact::compare` only ever looked at
`stdout`/`stderr`, and nothing in `runner.rs` could find a transcode case's
output file at all. There was no way to write a byte-identical remux case —
the exact thing XF-03 exists to produce.

Fixed: `{output}` / `{output:<name>}` is now a token a case's argv can use,
resolved by `Runner::run_case` to a path inside **a subdirectory private to
each side** (so the two binaries, given identical argv, do not overwrite each
other's file before it is compared). `Pair` gained `ours_output_file` /
`theirs_output_file`; `compare::exact::compare_output_file` diffs them as raw
bytes with no text normalisation (every declared `Output` normaliser is
text-shaped, and running one over a container's bytes would rewrite real
data, not hide a meaningless difference), and it distinguishes "both wrote
nothing" (fine) from "only one side wrote a file" — exactly finding 6's
silent-success shape, now caught automatically.

## 11. `behavioural` (C7) inherited a literal exit-code pre-check that made it indistinguishable from C0 — fixed, harness bug

Building `remux-known-incompatible-behavioural.toml` surfaced this:
`compare::evaluate` checked `ours.exit != theirs.exit` and returned a
divergence **before** dispatching to any mode, including `Behavioural`. But
`outcome_class` — the function `behavioural()` actually uses — exists
specifically to be coarser than a literal exit code ("accepted, rejected, or
crashed. Deliberately not the message text", per its own pre-existing doc
comment). Two independently-implemented programs essentially never choose
the same integer for "I rejected this input": measured directly across one
ten-case suite, `vaco` and `ffmpeg` produced 183, 218, 234 and 0 — no two
*failing* codes equal except by coincidence. The pre-check made
`outcome_class`'s coarseness unreachable exactly where C7 exists to apply
it — 6 of 10 known-incompatible combinations reported "exit codes differ"
when the more useful fact was one level down.

Fixed: the literal pre-check in `compare::evaluate` now excludes
`Compare::Behavioural`; the classification still sees both exit codes (via
`outcome_class`) and still diverges across the accept/reject boundary, just
not on every mismatched integer within "both rejected". This also *revealed*
a real product finding — see finding 19.

## 12. `[[exclude]]` had no way to bind the input side of a matrix — fixed, harness feature gap

A remux matrix is "one axis of input containers, one axis of output
containers" in every way except that the input side is `[[media]]`, not
`[[axis]]` — and `Exclusion::matches` only ever checked real axis names.
There was no way to write "AVI cannot reach Matroska" in a manifest at all.

Fixed: `Suite::expand` now also matches exclusions against a synthetic
`media` pseudo-axis (the id of the `[[media]]` entry a case was built from),
documented on `Exclusion` itself. `remux-bitexact.toml`'s nine `[[exclude]]`
stanzas each quote the actual `ffmpeg 8.1` stderr line they encode — Annex-B
vs length-prefixed H.264 (`h264 bitstream malformed, no startcode found`),
ADTS vs raw AAC framing (`ADTS is only supported with codec tag 0x1610`), and
AVI supplying no packet timestamps (`Timestamps are unset in a packet`).

## 13. `probe-matroska`'s `webm` fixture needed `libvorbis`, which this reference build does not have — fixed, suite bug

```
[aost#0:1] Error selecting an encoder
Error opening output file .../vp8-vorbis.webm.
Error opening output files: Encoder not found
```

Homebrew's `ffmpeg` 8.1 (`--enable-gpl --enable-libx264 --enable-libvpx
--enable-libopus --enable-libdav1d --enable-libx265 --enable-libmp3lame
--enable-libsvtav1 …`) does not carry `--enable-libvorbis`. All 16 cases
built on that one fixture failed to even launch — not a `vaco` divergence, a
suite whose fixture assumed an optional encoder. Swapped `libvorbis` →
`libopus` (`tests/conformance/probe/matroska.toml`), exactly as valid a WebM
audio codec and enabled far more often. 8 of the 16 previously-unreachable
cases now agree.

## 14. `vaco-mux-mp4`/MOV: no `avc1` compatible-brand entry, no placeholder atom before `mdat` — **fixed**

```
$ just conformance-run 'transcode-remux-bitexact/v-mp4/output=mp4'
exact-bytes — output file differs at byte 3; ours 8207 bytes, reference 8336 bytes
    ours   |    ftypisom   isomiso2mp41   mdat      F  ..
    theirs |     ftypisom   isomiso2avc1mp41   free  >mdat  ..
```

`crates/format/vaco-mux-mp4/src/brand.rs`'s `MP4` brand hardcodes
`compatible = [isom, iso2, mp41]`; `avc1` appears only in the `F4V` brand.
`crates/format/vaco-mux-mp4/src/progressive.rs::write_header` writes `ftyp`
then the `mdat` largesize header with no intervening box — no `free`/`wide`
placeholder writer exists anywhere in the crate. `-f mov` shares the same
`write_header` (confirmed by reading the call site, not guessed), so its
divergence (byte 23, 88 bytes short) is the identical root cause.

**Fixed** (agent:muxfix, 2026-08-23). `brand::file_type_box` now takes the
muxer's own track list and folds `avc1` into the compatible-brand list
exactly when there is an H.264 video track — measured across `mp4`,
`ipod`/`psp`/`3gp`/`3g2` (all gain it), `mov`/`ismv` (never do), an AAC-only
or HEVC source (no `avc1` either); inserted just before `mp41` where that
entry exists (`mp4`'s measured order is `isom iso2 avc1 mp41`, not `isom iso2
mp41 avc1`), else appended. `progressive::write_header` now writes an 8-byte
`free` (`wide` for `mov`) placeholder between `ftyp` and `mdat` in streaming
mode — `faststart` mode gets none, also measured. Verified byte-for-byte
against `ffmpeg 8.1 -c copy -f mp4` on the exact `v-mp4` fixture: `ftyp`
through the placeholder now match exactly; the only remaining divergence is
`mdat`'s header shape (this crate always uses the 16-byte `largesize` form,
a separate, already-documented, deliberate choice — see
`docs/format/vaco-mux-mp4.md`). Tests: `vaco_mux_mp4::brand::tests` (three
new cases) and `progressive::tests::mov_gets_wide_every_other_brand_gets_free`,
plus `tests/roundtrip.rs::a_non_faststart_file_puts_mdat_before_moov`
updated for the new placeholder box.

## 15. `vaco-mux-matroska`: no `SeekHead` **and no `CRC-32` on any level-1 element** — **fixed, byte gap not fully closed**

```
$ just conformance-run 'transcode-remux-bitexact/v-mp4/output=matroska'
exact-bytes — output file differs at byte 50; ours 7946 bytes, reference 8072 bytes
```

`crates/format/vaco-mux-matroska/src/mux.rs`'s own module doc says `SeekHead`
is "left out too, on purpose rather than by trait limitation" — a scoped
omission, not a bug, but it is the entire reason every Matroska case in the
byte-identical suite diverges (the `Info` element's `MuxingApp`/`WritingApp`
strings start immediately where the reference's `SeekHead` would be).
Recording it here because XF-03 is where the byte cost becomes visible and
measured (126 bytes on this fixture), not because it is news to the crate.

### Measured 2026-08-23 — and `SeekHead` is the smaller half

Dumped the reference's own Matroska output element by element. Two things the
finding did not say:

**1. Every level-1 element begins with a `CRC-32`, and `vaco-mux-matroska`
writes none.**

```
$ ffmpeg -bitexact -f lavfi -i testsrc=size=64x64:rate=25:d=1 \
         -pix_fmt yuv420p -c:v libx264 m.mkv
id=0x114D9B74 SeekHead size=64   first child 0xBF
id=0xEC       Void     size=83
id=0x1549A966 Info     size=75   first child 0xBF
id=0x1654AE6B Tracks   size=151  first child 0xBF
id=0x1254C367 Tags     size=131  first child 0xBF
id=0x1F43B675 Cluster  size=2274 first child 0xBF
id=0x1C53BB6B Cues     size=23   first child 0xBF
```

`vaco-demux-matroska`'s schema knows `CRC32 = 0xBF`; the muxer has no mention
of it anywhere. So *every* level-1 element diverges, not only the region
`SeekHead` would occupy — which makes the byte gap structural rather than the
one-off 126 bytes this finding recorded.

The algorithm is pinned, not guessed. Standard CRC-32 (IEEE, as `zlib.crc32`
computes it), emitted **little-endian**, over the element's payload *excluding
the `CRC-32` element itself*. Verified against two independent elements:

```
SeekHead  declared 32 30 7d 64   computed LE 32 30 7d 64
Info      declared 62 15 80 73   computed LE 62 15 80 73
```

`vaco-hash` is D11's single owner of the `crc` crate, so the implementation has
a home and must not add a second CRC table.

**2. The `SeekHead` layout is fully determined and reproducible.**

64-byte `SeekHead` = one `CRC-32` (6 bytes) + four `Seek` entries, then an
83-byte `Void` padding it out to a reserved region. Positions are relative to
the **Segment payload start**, not the file:

```
Seek -> Info    (0x1549A966)  position 161
Seek -> Tracks  (0x1654AE6B)  position 241
Seek -> Tags    (0x1254C367)  position 398
Seek -> Cues    (0x1C53BB6B)  position 2815
```

Entry sizes vary with the position's encoded width (11 bytes for a one-byte
position, 12 for two), which is precisely why the reference reserves a fixed
region and pads the remainder with `Void` rather than computing an exact size
up front. That resolves the crate's own stated objection — it needs neither "a
second seek-patch pass" nor "fixed-width placeholder arithmetic", because the
reference does not use either. It reserves, writes what it has, and voids the
rest, and `patch_known_size` is already in `vaco-format-ebml`.

Reclassified from "intentional omission, no behavioural gain" to **a real
divergence worth closing**: with the CRC-32s absent as well, no Matroska output
this project writes can ever be byte-identical, which takes an entire container
family out of the byte-identity suite.

### Fixed 2026-08-23 (agent:mkv) — and the byte gap is not fully closed

Both halves landed in `crates/format/vaco-mux-matroska/src/mux.rs`, exactly as
measured above.

**`CRC-32`**: `mux::with_crc32` prefixes a `CRC-32` element to the body of
every Level-1 element (`SeekHead`, `Info`, `Tracks`, `Chapters`,
`Attachments`, `Tags`, `Cluster`, `Cues`) before it is wrapped in its own
`write_element` call — `vaco_hash::crc32` (D11's single owner of the `crc`
crate) supplies the algorithm, so no second table was added anywhere.
Confirmed unconditional in what this crate writes: `ffmpeg -h muxer=matroska`
does list a real `-write_crc32 <boolean> ... (default true)` `AVOption` —
correcting an earlier draft of this entry, which said no such option
existed — but it defaults on, `Muxer` has no per-muxer option channel to
turn it off through anyway, and `-bitexact` does not touch it. A new
`tests/crc32_reference_fixture.rs` checks this crate's implementation against
`tests/fixtures/ffmpeg_reference.mkv` — the reference's *own* output, not
this crate's — recomputing and validating all six CRC-32-bearing Level-1
elements the fixture carries, not only the one this finding was originally
measured against.

**`SeekHead`**: `mux::seekhead_and_void` reserves a **fixed** 161-byte budget
for `SeekHead` plus its padding `Void`
(`mux::SEEKHEAD_RESERVED_BYTES`) — re-measured while implementing this and
found to be even more fixed than the finding above states: **stable across a
3-, 4-, 5- and 6-entry `SeekHead`** (probed with `-metadata`-driven chapters
and an attachment added on top of the original fixture) **and across file
sizes from ~3 KB to ~300 KB**, not merely across the one file this finding
measured. `Info`/`Tracks`/`Chapters`/`Attachments` get a `Seek` entry at
`write_header` time (their positions are fully known — they sit back-to-back
right after the reservation); `Cues`'s position is only known at
`write_trailer`, after every `Cluster`.

One thing this finding did not check, added during implementation:
**non-seekable output**. `ffmpeg -f matroska -` piped into a plain redirect
(the `pipe:` protocol disables seeking regardless of what the receiving
descriptor could technically do) commits to `SeekHead` at `write_header` time
with whatever it already has, and **omits `Cues` entirely** — not merely its
`Seek` entry, the whole element is absent from the piped file. This crate now
reproduces that: `write_trailer` gates writing `Cues` on
`self.out.is_seekable()`, matching the reference measured both ways.

**The byte gap is not fully closed.** `just conformance-run
'transcode-remux-bitexact/v-mp4/output=matroska'` still fails:

```
exact-bytes — output file differs at byte 50; ours 8270 bytes, reference 8072 bytes
```

But the divergence has moved. Byte 50 is inside `Segment`'s own size field —
it differs because the total file sizes differ, which is a downstream
consequence of content differences elsewhere, not evidence of a `SeekHead`/
`CRC-32` construction bug. Checked directly: bytes 52–213 (the entire
`SeekHead`+`Void` reservation) contain only the expected differences —
`SeekPosition` values and the `CRC-32` they roll up into, both of which *must*
differ because the elements they describe are genuinely different sizes in
the two files — and a self-consistency scan (parse every `Seek` entry, follow
its position, confirm the element actually found there has the entry's
target ID) passes on both files, including the `Cues` entry the seekable
patch adds at trailer time.

What actually still differs, measured directly against
`ffmpeg -hide_banner -nostdin -i v-src.mp4 -c copy -f matroska -fflags
+bitexact` on both sides, none of it caused by this finding's two fixes and
none of it this finding's scope:

- **`Info`**: this crate's `MuxingApp`/`WritingApp` is `vaco-mux-matroska`
  (this project's own identity, by design — not `ffmpeg`'s versioned
  `Lavf62.12.100`, which `-bitexact` itself shortens to plain `Lavf`).
  Separately — a real bug, unrelated to identity strings — `Info`'s
  `Duration` element can **never** be written: `info_bytes`'s
  `if self.max_end_ticks > 0` check runs inside `write_header`, before any
  `write_packet` call has had a chance to grow `max_end_ticks` above zero.
  Fixing it needs the same reserve-then-patch shape `SeekHead` just adopted,
  and patching `Info`'s size after the fact would shift every position after
  it — including the ones this finding just taught `SeekHead` to compute —
  so it is a follow-on, not a one-line fix.
- **`Tracks`**: this crate's `TrackEntry` field order and `TrackUID` octet
  width do not match the reference's, and the reference writes two small
  fields (inside `TrackEntry` and inside `Video`) this crate does not.
- **`Tags`**: this crate never forwards MP4-level container metadata
  (`major_brand`/`minor_version`/`compatible_brands`) into file-level `Tags`,
  and — already documented in `docs/format/vaco-mux-matroska.md` — does not
  reproduce the reference's own auto `ENCODER`/`DURATION`/`HANDLER_NAME`
  `SimpleTag`s. Nothing upstream of `vaco-mux-matroska` currently supplies
  either via `Muxer::set_metadata` for a plain `-c copy` remux.

Each is recorded with its exact cause in
`docs/format/vaco-mux-matroska.md`'s *Known gaps* item 6, so the next person
closing one of them is not starting from a re-derivation.

Tests: `cargo test -p vaco-mux-matroska` and `cargo test -p
vaco-demux-matroska` both green (25+1 and 28+7 tests respectively, plus
doctests) — a file this muxer writes still reads back. `cargo clippy -p
vaco-mux-matroska --all-targets` clean. `layer-check`, `dup-check`,
`time-gate`, `owner-gate`, `provenance-check` all green. Every fix falsified
directly: `with_crc32` perturbed to write a wrong CRC (caught by
`every_level1_element_this_muxer_writes_carries_a_validating_crc32`),
`SEEKHEAD_RESERVED_BYTES` changed from 161 to 160 (caught by
`seekhead_reservation_is_the_measured_fixed_budget`, which pins the literal
161 rather than comparing the constant to itself), and the non-seekable
`Cues` gate removed (caught by two tests, both failing with `NotSeekable`
since the muxer then tries to patch a reservation on a sink that cannot seek)
— each restored afterward and re-verified green.

## 16. `vaco-mux-avi`: no length-prefixed-to-Annex-B bitstream conversion at all — **fixed, byte gap not fully closed**

```
$ just conformance-run 'transcode-remux-bitexact/v-mp4/output=avi'
exact-bytes — output file differs at byte 4; ours 8064 bytes, reference 27406 bytes   # 3.4x smaller
```

The largest byte gap in the matrix, and structural rather than cosmetic:
`write_packet` (`crates/format/vaco-mux-avi/src/mux.rs`) writes
`packet.payload()` verbatim with no transformation, and `Cargo.toml` does not
depend on `vaco-format-nalu` at all — unlike `vaco-mux-mpegts`, which does
and calls `length_prefixed_to_annexb` explicitly. An H.264 stream sourced
from MP4 (length-prefixed `avcC`, SPS/PPS only in extradata, never in-band)
has no conversion path into AVI's expected Annex-B-with-in-band-parameter-
sets layout. `idx1` and the RIFF/trailer size patches are present and
correct — this is not an indexing gap, it is a missing bitstream-format
stage.

**Fixed the missing conversion** (agent:muxfix, 2026-08-23), chosen to live
**inside the muxer**, not as a `vaco-sched` bitstream-filter stage: the brief
noted `vaco-sched`'s mux path does not run the BSF chain at all
(`INTERFACE-GAPS.md`), so a muxer-internal conversion is the only place this
could land today, and it is exactly the shape `vaco-mux-mpegts` already uses
for the identical problem (`vaco-mux-avi` now depends on `vaco-format-nalu`
too, mirroring that crate's `Cargo.toml`). `add_stream` records a
`LengthSize` from `nal_length_size` for H.264/HEVC; `write_packet` runs
`length_prefixed_to_annexb` over the payload before it reaches `movi`, and
every downstream size field (`idx1`, chunk length, the odd-byte pad check)
now uses the *converted* length.

Measured after the fix, directly against `ffmpeg 8.1 -c copy -f avi` on the
`v-mp4` fixture: per-packet sizes now match the reference exactly (25
packets, identical byte counts each, confirmed via `ffprobe -show_packets`
on both files), and an Annex-B start code (`00 00 00 01`) now opens every
chunk where a raw length value used to. **The total file-size gap is not
closed**, though: the reference's own `movi` region is larger than the sum
of its packets by a wide margin (an ~192-byte gap appears between every pair
of consecutive real chunks, structured as if reserved space rather than
padding) — measured directly, not yet understood, and not an artefact of
this fix (both `idx1` and `ffprobe`'s packet list confirm the payload bytes
themselves are already correct). Left open as its own, narrower
sub-question rather than folded into this finding's original framing, which
was specifically about the missing bitstream conversion.

Tests: `tests/roundtrip.rs::a_length_prefixed_h264_sample_is_rewritten_to_annex_b`.
Fuzzing: new target `fuzz/fuzz_targets/avi_mux_packet.rs` (D6 — this crate
had no fuzz target at all before this fix, and now has real byte-level
parsing of caller-supplied length prefixes); 30s run, `exit=0`,
`execs≈1,753,000`, `find fuzz/artifacts -type f` empty.

## 17. `vaco-mux-mpegts`: SDT service name/provider default to empty strings — **fixed**

```
$ just conformance-run 'transcode-remux-bitexact/v-mp4/output=mpegts'
exact-bytes — output file differs at byte 2; ours 14288 bytes, reference 14664 bytes
    theirs contains readable "FFmpeg" "Service01" text ours does not
```

The SDT itself is implemented correctly (`crates/format/vaco-mux-mpegts/
src/mux.rs`, real `service_id`/`service_type`/`service_name`/
`service_provider` at PID `0x0011`) — `options.rs`'s
`MpegTsMuxOptions::default()` just sets `service_name`/`service_provider` to
empty strings, and the CLI's default invocation uses that default. Not a
missing feature, a default-value choice that happens to disagree with the
reference's.

**Fixed** (agent:muxfix, 2026-08-23). Re-measured rather than assumed: `-h
muxer=mpegts` has no `-service_name`/`-service_provider` option at all (so
there is no documented default to defer to, unlike every other field this
struct's doc comment says matches that transcript), so the two literal
strings were recovered by probing the SDT's own service descriptor bytes
from a plain `-c copy -f mpegts` — `provider_name="FFmpeg"`,
`service_name="Service01"`. `MpegTsMuxOptions::default()` now writes those.
Test: `tests/roundtrip.rs::default_options_write_the_references_measured_sdt_strings`.

## 18. `vaco-mux-flv`: `onMetaData` carries 3 of the reference's ~10 properties — **fixed**

```
$ just conformance-run 'transcode-remux-bitexact/v-mp4/output=flv'
exact-bytes — output file differs at byte 16; ours 7879 bytes, reference 8074 bytes
```

`write_metadata_tag` (`crates/format/vaco-mux-flv/src/mux.rs`) only ever
populates `duration` and, conditionally, `videocodecid`/`audiocodecid` — the
code's own comment says width/height are "not threaded through
`CodecParameters` back into this muxer in this version." No `videodatarate`,
`framerate`, `audiodatarate`, `audiosamplerate`, `audiosamplesize`, `stereo`,
or `filesize` ever get written, which also changes the ECMA array's element
count — the single byte the harness reports as the divergence point.

**Fixed** (agent:muxfix, 2026-08-23). `add_stream` used to discard
`CodecParameters` entirely after pulling `extradata` out of it; it now
captures an `OnMetaFields` per stream (width/height/frame_rate/sample_rate/
channels/bits/`bit_rate`), and `write_metadata_tag` writes all of
`width height videodatarate framerate videocodecid` (video) and
`audiodatarate audiosamplerate audiosamplesize stereo audiocodecid` (audio),
plus a now-patched `filesize`, in the measured order (`-c copy -f flv` on an
H.264(+AAC) MP4 source, byte-inspected: `duration width height
videodatarate framerate videocodecid [audiodatarate audiosamplerate
audiosamplesize stereo audiocodecid] filesize`). `videodatarate`/
`audiodatarate` are written only when the source's `CodecParameters::bit_rate`
states one, omitted otherwise — an honest "unknown", not a fabricated
number; `major_brand`/`minor_version`/`compatible_brands`/`encoder` also
appear in the reference's own output but are left out (see finding 22 — no
channel for MP4-`ftyp`-sourced format tags or an encoder identity string
reaches this crate today). Verified against `ffmpeg 8.1` directly: the
video-only fixture's key set and order now match exactly, missing only the
four fields just named. Test:
`tests/roundtrip.rs::on_meta_data_carries_the_streams_own_video_and_audio_properties`,
which decodes the real AMF0 body and checks each field's value against the
`CodecParameters` it came from — not merely that more keys exist.

## 19. Six of ten known-incompatible remux pairs **succeed** on `vaco` where the reference refuses — **1 of 6 fixed, root cause characterised, 3 more blocked on a demux-side decision**

```
$ just conformance-run 'transcode-remux-known-incompatible/av-avi/output=to-mpegts'
behavioural — outcome class differs
  outcome: ours="accepted-empty" theirs="rejected"
```

The finding finding 11's fix was built to surface. `vaco` currently writes an
exit-0 file for: AVI→MPEG-TS (untimestamped H.264 packets), ASF→{Matroska,
MPEG-TS, FLV} (same), and MPEG-TS→{AVI, ASF} (ADTS AAC into a container that
requires raw `AudioSpecificConfig` framing). This is precisely the "silent
success" shape finding 6 named: exit 0, no complaint, and — very plausibly —
a file that violates a real constraint of its own container. Not
characterized further per-crate here because the *absence* of a check is the
finding, and it could live in any of several places (the copy-path
validator, or each muxer's `add_stream`); flagged for whichever crate's
owner wants to add the constraint the reference already enforces.

Four of the ten pairs *do* agree — both sides reject, with unrelated numeric
codes (what finding 11 fixed): AVI→Matroska, AVI→FLV, FLV→AVI, ASF→AVI all
already fail the same way on both sides.

**Investigated (agent:muxfix, 2026-08-23).** The "codec support lists" framing
above turned out not to be quite the mechanism: `vaco_format_core::mux::MuxBuilder::add_stream`
*does* call `query_codec` (M15), and it is tested
(`an_unsupported_codec_is_refused_at_add_stream`). The reason it never fires
for these six pairs is that `vaco-sched`'s `PipelineSpec::map`
(`crates/app/vaco-sched/src/spec.rs`) calls `muxer.add_stream(params)`
directly on the raw `Box<dyn Muxer>`, bypassing `MuxBuilder` entirely — a
fact `vaco-cli/src/exec.rs`'s own module docs already state for an unrelated
reason ("`vaco-sched`'s `MuxWork` drives a raw `dyn Muxer`..."). **This is not
mine to fix** (`vaco-sched` is outside this brief's scope), and it would not
have been the fix anyway: `query_codec`/`CodecSupport` answers "can this
container ever hold this codec at all", which is not what either failure
mode here is about — H.264 is fine in AVI/MPEG-TS/FLV in general, and AAC is
fine in AVI in general. Both are narrower, *stream-content* constraints:

1. **A stream's first packet has no PTS at all.** Measured directly
   (`ffmpeg -i <that-format> -c copy -f {mpegts,flv}`): MPEG-TS refuses with
   "first pts and dts value must be set" (exit 183); FLV refuses with
   "Packet is missing PTS" (exit 234). AVI is the concrete source — it has no
   native per-packet PTS field, only a per-stream sample count a reader
   reconstructs timing from — and real `ffmpeg`'s own AVI *demuxer* leaves
   `pts` genuinely unset for such a stream while still deriving a `dts`.
   **Fixed** in `vaco-mux-mpegts` (first packet per stream only, matching the
   reference's own "first" wording) and `vaco-mux-flv` (every packet, since
   its message carries no "first" qualifier) — both now refuse rather than
   silently reusing the previous clock / writing `pts=0`.
2. **ADTS-framed AAC has no legal representation in a container that expects
   raw, `AudioSpecificConfig`-framed AAC.** Measured directly
   (`ffmpeg -i <mpegts-source> -c copy -f avi` refuses at `write_header` with
   "ADTS is only supported with codec tag 0x1610", exit 234). MPEG-TS's own
   AAC convention is ADTS (config repeated per frame, no separate blob), so
   a stream with no `extradata` at all is the observable signal available for
   "this is ADTS, not raw" — MP4/`esds`-sourced AAC always has one. **Fixed**
   in `vaco-mux-avi`'s `add_stream`.

**Measured result of (1) against `vaco`'s own demux side**: it does not yet
change AVI→MPEG-TS's or ASF→{Matroska, MPEG-TS, FLV}'s outcome, because
`vaco-probe`/`vaco-demux-avi` synthesize a `pts` for *every* packet
(confirmed: `vaco-probe -show_packets` on the same AVI fixture prints
`pts=0`, `pts=1`, … for every video packet) where the reference's own AVI
demuxer leaves it unset. The mux-side refusal is correctly implemented and
unit-tested against the *contract* (a genuinely-`None` `pts` is refused,
verified directly), and it is exactly what closed MPEG-TS→AVI (fix 2, which
does not depend on any demuxer's PTS policy) — but full parity on the other
three pairs needs `vaco-demux-avi`/`vaco-demux-asf` to also decline to
fabricate a timestamp they cannot truly derive, which is a demux-side design
question (does synthesizing a PTS from frame count count as "the source
stated one"?) outside this brief's crates (`vaco-demux-avi`, `vaco-demux-asf`
are not `vaco-mux-*`). Flagged for whichever agent owns those two crates.

**Net**: MPEG-TS→AVI now agrees with the reference (both reject). AVI→MPEG-TS
and ASF→{Matroska, MPEG-TS, FLV} still diverge, for the demux-side reason
above — the mux-side half of the fix is in place and tested, waiting on the
other half. ASF→Matroska and MPEG-TS→ASF also remain open in the same shape
as (1)/(2) respectively, since their target muxers (`vaco-mux-matroska`,
`vaco-mux-asf`) are outside this brief's four crates.

## 20. `vaco-mux-mp4`: self-remuxed MP4 reports a ~1600× wrong duration — **fixed**

```
$ just conformance-run 'transcode-remux-structural/v-mp4/output=mp4'
structured-diff — 15 unexplained field difference(s)
  FORMAT.duration: ours="1601.040000" theirs="1.000000"
  STREAM.time_base: ours="1/25" theirs="1/12800"
  STREAM.duration_ts: ours="40024" theirs="12800"
```

Found via the new structural tier (probe the produced file, not the
transcode tool's own stdout — see *harness changes*, below). `vaco`'s MP4
output uses time base `1/25` (looks like the video frame rate, not a proper
media timescale) against a `duration_ts` computed for a finer one, so a
1-second clip reports as 1601 seconds. The highest-severity finding in the
whole session — every downstream `bit_rate` computation is proportionally
wrong too — and it belongs to `vaco-mux-mp4`, most likely in how it derives
or threads through the track timescale on a `-c copy` path. Not present on
the `-f asf` case (`FORMAT.duration: ours="1.040000" theirs="1.080000"`, off
by 40ms, plausibly a rounding/timebase-conversion artifact rather than the
same bug), which narrows the search to MP4/MOV's own timescale handling
specifically.

**Root cause, fixed** (agent:muxfix, 2026-08-23): not the track-timescale
*choice* (which is a separate, real, lower-severity gap — see
`docs/format/vaco-mux-mp4.md`'s gotchas — this crate picks the timescale
from frame rate/sample rate rather than preserving a `-c copy` source's own
`mdhd` timescale) but a units bug in `MovMuxer::write_packet`:
`vaco_packet::Packet::duration` is **always microseconds**
(`Packet::rescale_ts`'s own doc comment: only `pts`/`dts` are rescaled to a
new time base, `duration` deliberately is not), and `write_packet` was
reading `packet.duration.0` straight into `TrackState::last_duration_hint` —
a tick count in the track's own timescale — with no conversion at all.
Confirmed by dumping the produced file's raw boxes: `mdhd`/`stts` showed a
last-sample delta of exactly `40000` (the correct value *in microseconds* —
1/25 second — copied verbatim into a field whose unit is 1/25-second ticks
at this track's timescale of 25, where the correct tick count is `1`).
Fixed with `vaco_core::Duration::to_ticks(track.time_base())`, which already
existed for exactly this conversion and was simply not being called.
Verified against `ffmpeg 8.1 -c copy -f mp4` on the exact fixture:
`duration_ts` now reads `25` (was `40024`), `FORMAT.duration` reads `1.000000`
(was `1601.040000`). Test:
`tests/roundtrip.rs::track_duration_converts_packet_duration_from_microseconds_to_track_ticks`.

## 21. H.264 `profile` is reported inconsistently across probe paths — **the premise was wrong; FLV now agrees**

**This finding's third bullet was false and sent the fix in the wrong
direction.** The reference does *not* print the numeric `profile_idc`. It
prints the decoded **name**, for every container, in every writer:

```sh
$ ffmpeg -f lavfi -i testsrc=size=64x64:rate=10:duration=1 \
         -pix_fmt yuv420p -c:v libx264 -profile:v high -f flv p.flv
$ ffprobe -v quiet -of json -show_entries stream=profile,level p.flv
{ "profile": "High", "level": 10 }
```

So `vaco`'s MP4 path, which this finding recorded as "the wrong *kind* of
value", was right all along. Recorded here because the mistake is the one
`AGENT-CONSTRAINTS.md` keeps warning about: the numeric form was recalled,
not measured.

The ASF/FLV half was real, and FLV is now **fixed** — as a side effect of
finding 23's stream-enumeration fix, not of any work on this field:

```sh
$ vaco-probe -v quiet -of json -show_entries stream=profile,level p.flv
{ "profile": "High", "level": 10 }        # identical to the reference
```

Still open for ASF specifically (`vaco-demux-asf`), which finding 22's own
note already characterises as returning essentially empty H.264 stream info
on read-back.

**Checked, not fixed** (agent:muxfix, 2026-08-23): confirmed this is entirely
a demux/probe-side gap — `profile` is assembled while reading a stream, not
while muxing one, so no muxer this brief owns (`vaco-mux-mp4`,
`vaco-mux-avi`, `vaco-mux-mpegts`, `vaco-mux-flv`) touches this field at all.
Belongs to `vaco-parse-h264` and/or `vaco-demux-asf`/`vaco-demux-flv`, none
of which are in this brief's scope; reported per this brief's own
instruction rather than worked around.

## 22. `vaco`'s copy pipeline does not carry MP4-sourced format tags into ASF

```
$ just conformance-run 'transcode-remux-structural/v-mp4/output=asf'
structured-diff — 29 unexplained field difference(s)
  FORMAT.TAG:major_brand: ours="" theirs="isom"
  FORMAT.TAG:compatible_brands: ours="" theirs="isomiso2avc1mp41"
  FORMAT.TAG:minor_version: ours="" theirs="512"
```

The reference exposes an MP4 source's `ftyp`-derived fields as generic
format-level tags and — because ASF supports an arbitrary tag dictionary —
carries them through a `-c copy` into the ASF output too. `vaco`'s ASF output
has none of these tags at all. Whether this is "should propagate generic
format tags across `-c copy`" (a copy-pipeline question, `vaco-sched` or
`vaco-cli-core`) or "ASF muxer doesn't have a tag-writing path"
(`vaco-mux-asf`) is not resolved by this measurement alone; flagged for
whoever owns the copy-path's metadata handling to decide. The same ASF case
also shows `vaco`'s H.264 stream-info coming back essentially empty on
read-back (`pix_fmt=unknown`, `extradata_size=""`, `is_avc=""`,
`nal_length_size=""`, `codec_tag_string=H264` vs the reference's `avc1`) —
consistent with finding 21 and worth the same crate's attention.

**Checked, not fixed** (agent:muxfix, 2026-08-23): the FLV side of this
finding's own note is now partially addressed as a side effect of finding
18 — `vaco-mux-flv`'s `onMetaData` now carries `width`/`height`/`framerate`/
etc. sourced from `CodecParameters`, which is the *same generic-tag*
question this finding raises, just for a different field set (`onMetaData`
properties vs. `ftyp`-derived `FORMAT.TAG:*` entries and H.264 stream-info
read-back). The core ASF question — whether generic format tags propagate
across `-c copy` — is unresolved and still belongs to `vaco-sched`/
`vaco-cli-core` (the copy pipeline) or `vaco-mux-asf` (the tag-writing path),
neither of which is in this brief's scope (`vaco-mux-mp4`, `vaco-mux-avi`,
`vaco-mux-mpegts`, `vaco-mux-flv`).

## 23. `vaco-probe` finds **zero streams** in every FLV file

```
$ just conformance-run 'probe-flv/h264-and-aac/section=streams,writer=default,input=file'
exact-bytes — stdout differs at byte 0; ours 0 bytes, reference 1172 bytes
```

The most severe finding among the new probe suites. `ffprobe -show_streams`
on a two-stream (H.264 + AAC) FLV lists both streams in full; `vaco-probe
-show_streams` on the identical file prints **nothing at all**, and
`-show_format` still reports `nb_streams=0` — not a partial/wrong stream
list, a complete failure to enumerate any stream in the container. All 56
`probe-flv` cases diverge because of this one root cause. Belongs to
`vaco-demux-flv` or wherever `vaco-probe` wires FLV stream enumeration; not
narrowed further because the harness cannot see past "prints nothing"
without crossing into source-level debugging of a crate this brief does not
own.

## 24. Per-demuxer codec-ID mapping gaps — the same shape as finding 4, four more families — **all four fixed**

Finding 4's shape ("`TsCodec::codec_id` maps eight of about thirty
variants") turns up in every new probe suite, in crates finding 4 never
touched:

| Family | Symptom | Reproduction |
|---|---|---|
| AVI | `codec_name=unknown` for the `FMP4` FourCC (reference: `mpeg4`) | `probe-avi/mpeg4-video/section=both,writer=default,input=file` |
| DV | `codec_name=unknown` (reference: `dvvideo`) | `probe-dv/dv-ntsc/section=both,writer=default,input=file` |
| Ogg | `sample_fmt=unknown` for FLAC (reference: `s16`) | `probe-ogg/flac-audio/section=both,writer=default,input=file` |
| audio-simple (AIFF/AU/CAF) | `codec_name=pcm` generic, not endianness/width-specific (reference: `pcm_s16le`/`pcm_s16be`) — the same shape finding 7 already found in MP4/MOV's PCM handling, in the sibling crate `vaco-format-audio-simple` | `probe-audio-simple/pcm-aiff/section=both,writer=default,input=file` |

Same mechanical fix each time — complete the mapping table for the format —
and the same ownership pattern: whichever demuxer builds the
`CodecParameters` for that family.

**All four rows fixed.** AVI, DV and Ogg in 589e5e3; audio-simple below.
Three of the four turned out not to be missing table entries at all — the
crates held the right answer and had been told there was nowhere to put it:

- **AVI** had `FMP4 -> "mpeg4"` in `codec_name` and no id in `codec_id`,
  and `vaco-probe` prints the name from the *id*. Six more FourCCs were in
  the same gap.
- **DV** set no `codec_id` on either stream, on the strength of a comment
  saying `CodecId` had no DV variant. It did.
- **audio-simple** returned the generic `CodecId::Pcm` — a codec the
  reference never names — for every flavour in AIFF, AIFF-C, AU, CAF and
  RSO. A shared `pcm::codec_id_for(bits, is_float, big_endian, signed)`
  now derives the specific one, and the AIFF-C table's conflation of
  `sowt` (little-endian) with `twos` (big-endian) — invisible while both
  returned the same generic id — is fixed with it.

Verified end to end against the reference on 24 file/encoder combinations
across AIFF, AU and CAF: `codec_name` and `sample_fmt` now agree on all 24.

Two rows nobody would derive without measuring: `pcm_s8` reports
`sample_fmt=u8` (there is no signed 8-bit sample format), and
`pcm_alaw`/`pcm_mulaw` report `s16` while being neither signed nor 16-bit —
the field is the *decoded* format, not the coded one.

The nine `default_audio` declarations were wrong in the same way and are
now what `ffmpeg -h muxer=<name>` states: `pcm_s16be` for aiff/au/caf,
`pcm_u8` for rso, `pcm_s32le` for sox, `pcm_s16le` for ircam/voc/w64/wav.

## 25. `vaco-probe` emits `"profile": "unknown"` where the reference omits the key entirely — **fixed**

Seen on FLAC and DV (finding 24): the reference's JSON writer does not emit a
`profile` key at all for a codec with no meaningful profile concept;
`vaco`'s always does, with the value `"unknown"`. A field-presence
difference, not a field-value one — worth distinguishing because "the field
is wrong" and "the field shouldn't be there" have different fixes.

**Fixed.** Not in the writers: the optional-field machinery in
`vaco-probe`'s `emit.rs` was already correct and already tested, and the
field table already carried `Absent::Word("unknown")` for `profile`. The
fault was one line up, in `show.rs`, which handed the emitter a *present*
`Val::s("unknown")` instead of `Val::Absent` — so the writer never reached
the branch that omits. Measured both ways before and after:

```sh
ffprobe -v quiet -of json    -show_entries stream=profile f.flac   # no key
ffprobe -v quiet -of default -show_entries stream=profile f.flac   # profile=unknown
```

The heading also under-stated the scope: the same one-line fault would have
affected every codec with no profile, not only FLAC and DV.

## 26. `extradata_size` is absent wherever the reference synthesises extradata from the bitstream

Measured on a reference-built AVI, where everything else about the stream
already agrees:

```sh
$ ffmpeg -f lavfi -i testsrc=size=64x64:rate=25:duration=1 \
         -pix_fmt yuv420p -c:v libx264 a.avi
$ ffprobe    -v quiet -of csv=p=0 -show_entries stream=codec_name,profile,pix_fmt,extradata_size a.avi
h264,High,yuv420p,37
$ vaco-probe -v quiet -of csv=p=0 -show_entries stream=codec_name,profile,pix_fmt,extradata_size a.avi
h264,High,yuv420p
```

`profile` and `pix_fmt` are right, so `vaco`'s H.264 parser *is* reading the
in-band SPS. What it does not do is keep the SPS and PPS as `extradata`. The
reference's 37 bytes are not in the file: the `strf` chunk is exactly 40
bytes with `biSize = 40` and nothing after it, checked directly. They are
produced by `avformat_find_stream_info` running the `extract_extradata`
bitstream filter and storing the result on the stream.

So this is not a missing table entry — it is a missing stage. It is the same
`vaco-bsf-*` shaped hole that M6's bitstream-filter stage already names
(reachable but inert), approached from the read side instead of the write
side, and it should be fixed once for both rather than special-cased per
demuxer.

### The ASF half of findings 21 and 22, measured

Same input, muxed to ASF instead:

```sh
$ ffprobe    -v quiet -of csv=p=0 \
    -show_entries stream=codec_name,profile,level,pix_fmt,codec_tag_string a.asf
h264,High,H264,yuv420p,10
$ vaco-probe -v quiet -of csv=p=0 … a.asf
h264,unknown,H264,unknown,-99
```

ASF is *worse* than AVI, not the same: `extradata_size` is 38 on both sides,
so the container's own extradata does reach `vaco` — and yet `profile`,
`level` and `pix_fmt` are all unset, which is exactly what AVI gets right
from the in-band SPS.

**Fixed, and it was not `vaco-demux-asf`.** The demuxer was right: it reads
the `BITMAPINFOHEADER` tail and hands over all 38 bytes. Those bytes are
**Annex B**, not an `avcC` — read straight off the file:

```text
00 00 00 01 67 64 00 0a …     (start code, then NAL type 0x67 = SPS)
```

`H264Parser::set_extradata` parsed everything as an
`AvcDecoderConfigurationRecord`, so it returned an error — and
`vaco-format-core`'s `build_parser` does `let _ = parser.set_extradata(…)`,
so the failure was silent. The parser held a perfectly good SPS's worth of
bytes and never looked at them.

`extradata[0]` discriminates: an `avcC` begins with `configurationVersion`,
which is 1; Annex B begins with the first byte of a start code, which is 0.
Confirmed on files this reference build wrote — `p.mp4`'s `avcC` payload
starts `01 64 00 0a ff e1`, `a.asf`'s tail starts `00 00 00 01 67 64`.

After the fix all four containers agree with the reference exactly:

```text
         ref                        ours
a.asf    h264,High,yuv420p,10       h264,High,yuv420p,10
a.avi    h264,High,yuv420p,10       h264,High,yuv420p,10
p.mp4    h264,High,yuv420p,10       h264,High,yuv420p,10
p.flv    h264,High,yuv420p,10       h264,High,yuv420p,10
```

`vaco-parse-hevc` had the identical fault in the identical place and got the
identical fix. That one is **not** measured: no container/HEVC combination on
this machine produces Annex-B extradata, so it is covered by a unit test that
builds one rather than by a reference file, and the code says so.

This closes the stream-info half of findings 21 and 22. What remains under
finding 22 is only the original question — whether generic format tags
propagate across `-c copy` into ASF.

### The read half — extract_extradata, measured — **fixed**

`vaco-bsf-generic`'s `extract_extradata` (the write half, above) already
computed the exact assembly rule the reference uses. What was missing was
anything on the *read* path calling it: `-show_streams` still reported no
`extradata` at all for AVI/MPEG-TS-carried H.264/HEVC, because nothing ever
ran the rule while probing.

D19 governs the shape of the fix: the assembly rule gets exactly one
definition, not a second copy living beside `vaco-format-core`'s stream
discovery. Three ways to get there, in the order considered:

1. **Move the rule into a crate both sides already depend on.** Chosen.
   `vaco-format-nalu` already owns start-code framing (`units`, `Framing`,
   `NalHeader`) and sits below both `vaco-bsf-generic` and
   `vaco-format-core` in the crate graph (`cargo xtask layer-check` — both
   are `codec | format` = layer 4, and the edge `vaco-format-core ->
   vaco-format-nalu` is same-layer and acyclic, which the checker permits).
   The new `vaco_format_nalu::extradata` module holds `parameter_sets`
   (which NAL units in a payload are SPS/PPS/VPS) and `assemble_extradata`
   (the three-byte-then-four-byte start-code rule); `extract_extradata` and
   `Discovery::synthesize_extradata` both call it and neither keeps a local
   copy.
2. **Route `Discovery` through a `BsfProvider`**, symmetrical with
   `ParserProvider`, and run the real filter. Rejected: no such seam exists
   today, and building a provider, a registry lookup and a `BitstreamFilter`
   instance to reach a function with no state of its own is machinery this
   problem does not need — the filter's only other behaviour
   (`remove=1`) is not reachable through the seam anyway (`vaco-bsf-generic`
   docs, and INTERFACE-GAPS gap 12). Left as an option for whoever needs the
   filter's other behaviour on the read side later.
3. **Have `vaco-parse-h264`/`-hevc` expose the parameter sets they already
   parsed**, and assemble from those. Rejected: strictly more surface for
   the same answer. `Discovery` already holds the exact packet payload it is
   about to feed the parser — reaching into the parser's private state for
   bytes already in hand would only add a reason to widen `Parser`.

The read-side half of the fix, `Discovery::synthesize_extradata`
(`vaco-format-core`'s `discovery.rs`), runs independently of whether a
`Parser` is even registered for the stream's codec — it needs nothing but
`vaco-format-nalu` and the raw payload, so a `--no-default-features` build
with no H.264/HEVC parser compiled in gets the same `extradata_size` a full
build does. It fires once: the first packet in the probe window carrying a
parameter set sets `extradata`, and the container's own record (when one
exists — ASF, MP4) is checked first and never overwritten.

Measured against `ffmpeg 8.1`, `-show_entries
stream=codec_name,profile,pix_fmt,extradata_size`, same `testsrc`/`libx264`
recipe as above, now covering all four shapes finding 26 named plus HEVC:

```text
                              ref                      ours
a.avi   (h264, Annex B)      h264,High,yuv420p,37      h264,High,yuv420p,37
a.ts    (h264, MPEG-TS)      h264,High,yuv420p,37      h264,High,yuv420p,37
a.asf   (h264, avcC record)  h264,High,yuv420p,38      h264,High,yuv420p,38
p.mp4   (h264, avcC record)  h264,High,yuv420p,45      h264,High,yuv420p,45
h.ts    (hevc, MPEG-TS)      hevc,Main,yuv420p,82      hevc,Main,yuv420p,82
```

`a.avi` and `a.ts` are the ones this closes — both were empty before the fix
and both are the exact 37 measured bytes now (three-byte start code on the
SPS, four-byte on the PPS). `a.asf` and `p.mp4` already agreed after findings
21/22's fix above and are unchanged, confirming the new code path does not
touch a stream that already has a container-supplied record. `h.ts` is the
HEVC case finding 26 originally said no local file could exercise — MPEG-TS
turned out to produce one, so this is now measured rather than covered only
by a synthetic unit test.

This closes finding 26.

## 27. `-filters` printed a legend and no rows; `-h filter=` prints one line where the reference prints an option table

Two divergences in the same surface, found by diffing `vaco -filters` against
`ffmpeg -filters` rather than by any test — and the reason no test caught them
is itself the finding.

### `-filters` had zero rows — **fixed**

`write_filters` emitted the seven-line legend and stopped, on the strength of a
comment reading "Zero rows: `FILTERS` is always empty (no filter crate exists
yet)". Twenty filter crates and 142 registered filters existed by then, every
one of which already resolved through `-h filter=<name>`.

The test beside it asserted the output *ended* at the legend:

```rust
assert!(s.ends_with("  ------\n"), "{s}");
```

so it passed for exactly as long as the bug lasted and failed the moment the
bug was fixed. `bsfs_header_with_zero_rows` was the same shape and had already
started failing on its own, because the bitstream-filter work landed rows under
it. This is the "never pin the absence of something the project is building"
trap in `AGENT-CONSTRAINTS.md`, twice, in one file.

Fixed, with the row format measured off `ffmpeg -filters` 8.1 rather than
guessed — including reading the column widths off `colorchannelmixer`, a name
that exactly fills its 17-character field, so the padding could not be mistaken
for a separator. **All 142 of our filters appear in the reference's list and
none is invented**, which is the useful half of the check.

### 133 of 142 descriptions differ — open

Consistent and mechanical: our `long_name` values are lower-case initial and
have no trailing period.

```text
acompressor   ours "audio compressor"                  ref "Audio compressor."
aderivative   ours "compute derivative of input audio"  ref "Compute derivative of input audio."
acopy         ours "Copy the input audio unchanged to the output"
              ref  "Copy the input audio unchanged to the output."
```

The strings live in each crate's `vaco-component.toml`, so this is a
one-command sweep — but those files were owned by five concurrent agents when
this was found, so it is recorded rather than done. It should be a single
mechanical pass regenerating `long_name` from `ffmpeg -filters`, not 133
hand-edits.

### `Mime type:` was missing from `-h muxer=` — **fixed**, 11 -> 34 of 38

Three separate facts, each measured:

* **Muxers print it, demuxers do not.** `-h muxer=aiff` prints
  `Mime type: audio/aiff.`; `-h demuxer=aiff` prints nothing at all after its
  header, for the same format. So the line is emitted from the muxer arm only.
* **Only the first is printed.** `aiff` carries `audio/aiff` and
  `audio/x-aiff`; the reference prints the first alone.
* **It is not on the descriptor.** `MuxerDesc` has no MIME field —
  `vaco_registry::Component` does — so `help.rs` looks it up by name rather
  than reading it off the descriptor it already holds.

`help.rs` never emitted the line at all, and 27 muxer fragments had no
`mime_types` row to emit. Both fixed by a scripted sweep against
`ffmpeg -h muxer=<name>` rather than 27 hand-edits. One of the 27 was not
missing but *wrong*: `mpegts` is `video/MP2T`, with the IANA capitalisation,
and both the mux and demux fragments carried `video/mp2t`.

Three remain — `matroska`, `webm`, `webm_chunk` — skipped because that crate
was owned by a concurrent agent at the time, not because anything about them
is different.

### `Default subtitle codec:` has nowhere to come from — open

```text
$ ffmpeg -h muxer=matroska        $ ffmpeg -h muxer=webm
    Default video codec: h264.        Default video codec: vp9.
    Default audio codec: ac3.         Default audio codec: opus.
    Default subtitle codec: ass.      Default subtitle codec: webvtt.
```

`MuxerDesc` has `default_video` and `default_audio` and no `default_subtitle`.
Adding one is a field on a struct with ~100 exhaustive literal initialisers
across the workspace, which is a breaking sweep and wants a quiet tree — the
same class as `INTERFACE-GAPS.md` gaps 5 and 6, and it should probably land
with them rather than on its own.

### `-h filter=<name>` is a stub — open

```text
$ ffmpeg -h filter=volume            $ vaco -h filter=volume
Filter volume                        Filter volume [change input volume]:
  Change input volume.
    Inputs:
       #0: default (audio)
    Outputs:
       #0: default (audio)
volume AVOptions:
   volume  <string>  ..F.A....T. set volume adjustment expression (default "1.0")
   …
```

Ours prints one line and exits. The reference prints the description, the input
and output pads with names and media types, and the full `AVOptions` table with
type, flag field, help text, default, and the named constants for each enum
option. `-h muxer=` and `-h demuxer=` in this codebase already render an option
table, so the machinery exists and this surface is simply not wired to it.

Every filter registered so far declares its options — that is what the fuzz
targets exercise — so the data is present. Worth doing as one piece of work
across all filters rather than per crate.

## 28. `-h demuxer=` — 56 demuxers declare extensions the reference does not, and none prints an option table

Swept all 131 registered demuxers against `ffmpeg -h demuxer=<name>` 8.1.
Sixteen agree; 115 differ, in two unrelated ways.

### 56 declare `Common extensions:` where the reference declares none

Not a spelling difference — the reference's *demuxer* for these formats has an
empty extension list, and the extensions live on the **muxer** instead. `aiff`,
`au`, `caf`, `asf`, `concat` and every `*_pipe` are in this set:

```text
$ ffmpeg -h demuxer=aiff          $ vaco -h demuxer=aiff
Demuxer aiff [Audio IFF]:          Demuxer aiff [Audio IFF]:
                                       Common extensions: aif,aiff,afc,aifc.
$ ffmpeg -h muxer=aiff
Muxer aiff [Audio IFF]:
    Common extensions: aif,aiff,afc,aifc.
```

This is the same asymmetry `vaco-subtitle-text`'s `ass` already turned out to
have and was fixed for: the demuxer identifies by probing, the muxer picks a
filename.

**But it is not the mechanical sweep it looks like, and that is worth knowing
before someone starts one.** `DemuxerDesc::extensions` is not display-only in
this workspace — it is load-bearing:

```rust
// vaco-registry
pub fn demuxers_for_extension(filename: &str) -> impl Iterator<Item = &'static DemuxerDesc>
// called from vaco-cli/src/exec.rs:443
// and vaco-demux-image2/src/pipe/mod.rs scores a probe with `spec.extensions`
```

So emptying those 56 lists to match the reference's *help* output would change
our *probing* behaviour, and probing correctness matters more than a help
string. The reference gets away with it because its extension hint lives
somewhere our model does not separate.

The fix is therefore a small design decision, not a data edit: either split the
two roles (a `probe_extensions` the engine hints from, and the reference's
narrower `extensions` the help prints), or keep one list and accept the help
divergence with a recorded reason. Whoever takes it should decide that first
and edit second. `ass` was safe to fix in place only because its demuxer had no
probe that depended on the extension.

Four more differ in content rather than presence, and each is worth a look
because none is a simple omission:

```text
ircam    ref sf,ircam                    ours sf
mpl2     ref txt,mpl2                    ours mpl2,txt          (order differs)
webvtt   ref vtt,webvtt                  ours vtt
ogg      ref ogg                         ours ogg,oga,ogv,ogx,opus,spx
```

`ogg` is the interesting one: the reference's Ogg *demuxer* claims only `.ogg`,
while `oga`/`ogv`/`opus`/`spx` are separate **muxers**. `mpl2` shows the list
is ordered, not a set.

### 99 have an `AVOptions` table and we print none

The same gap finding 27 records for `-h filter=`, in a second surface:

```text
$ ffmpeg -h demuxer=concat
Demuxer concat [Virtual concatenation script]:
concat demuxer AVOptions:
  -safe   <boolean>  .D......... enable safe mode (default true)
  …
```

Ours stops after the header line. The demuxers already *have* their options —
`FormatOptions` parses them and the fuzz targets exercise them — so this is
rendering, not data. `-h muxer=` has the same hole. Worth doing once for
filter, demuxer, muxer, bsf and protocol together rather than four times.

## 29. `-filters`' flag and pad columns are wrong on half our filters

Now that `-filters` prints rows (finding 27), the rows themselves can be
compared. All 230 of our filters appear in the reference's 480 and **none is
invented** — that part is clean. The columns are not.

### The flag column: 114 of 230 differ

```text
  85  slice threading    ours `.`  ref `S`
  59  timeline support   ours `.`  ref `T`
  11  timeline support   ours `T`  ref `.`
```

These are not cosmetic. `FilterFlags::SLICE_THREADS` is a claim that the
filter can process independent slices of a frame concurrently, and
`TIMELINE_GENERIC`/`TIMELINE_INTERNAL` is a claim about `enable=` handling
that the framework acts on. 85 filters that *can* be slice-threaded are
declared as if they cannot, and 11 claim timeline support the reference does
not give them — that last direction is the one that could misbehave rather
than merely underperform.

### The pad column: 7 differ, all in the same way

```text
anequalizer   ours A->A    ref A->N        decimate       ours V->V    ref N->V
aphasemeter   ours A->A    ref A->N        guided         ours V->V    ref N->V
ebur128       ours A->A    ref A->N        premultiply    ours VV->V   ref N->V
                                           unpremultiply  ours VV->V   ref N->V
```

Every one is a *dynamic* pad count declared as fixed —
`FilterFlags::DYNAMIC_INPUTS`/`DYNAMIC_OUTPUTS` missing. `premultiply` and
`unpremultiply` are the instructive pair: they take one or two inputs
depending on their options, and we declare two unconditionally, so
`premultiply` alone is unusable in a graph where the reference accepts it.

### How to fix it, and why it is not fixed here

This is per-filter descriptor data spread across every filter crate, most of
which had a live owner when it was measured. The whole table is derivable —
`ffmpeg -filters` states it directly — so it wants one scripted sweep against
the reference rather than hand-edits, in a quiet tree.

The comparison itself is worth keeping as a test: `-filters` is now a stable,
machine-comparable surface, and a row-by-row diff against a captured reference
listing would catch every future regression in three columns at once.

## 30. `sine` is not bit-exact, and nearly shipped claiming it was

`vaco-filter-asource`'s author fitted `floor(4095 * sin(...))` to the
reference's `sine` output and it matched **8 of the first 10 samples**. That
is the "fits 8 of 9 points" shape that has caught most of this project's wrong
formulas, so they extended the comparison to 2000 samples: **51% disagree**.

The residual pattern is a dithered quantiser. No closed-form expression
reproduces it without the reference's own RNG, so `sine` is documented as
algorithmically faithful and *not* bit-exact rather than shipped as a
near-miss. `afdelaysrc` had the same near-miss caught the same way — a
Blackman-windowed guess that was wrong by a factor of ten at the peak.

**Why this one matters beyond its own filter.** `sine` is what almost every
audio probe in this project generates its input with. Any future test that
generates audio with `vaco` and compares against the reference's own `sine`
will diverge for reasons that have nothing to do with what is being tested. So:
generate test material with the **reference**, not with ours, until this is
closed — which is what the conformance harness already does, and now there is a
recorded reason why it must keep doing it.

The video side has the same hazard and mostly avoids it: `allrgb`, `allyuv`,
`yuvtestsrc`, `rgbtestsrc`, `colorspectrum`, `colorchart`, `smptebars` and
`smptehdbars` are all exact — `allrgb`/`allyuv` verified as full bijections
rather than point-matches. But `testsrc` and `testsrc2`, the two most-used
generators of all, are **not implemented**: `testsrc` needs bitmap-font
rendering and `testsrc2`'s animation did not resolve to a verifiable formula.
Neither was guessed at.

## 31. Filter options are not range-checked, and the reference's own ranges are the missing bound

The fuzzer found one instance of this — `cellauto=size=911111x91111` asking for
an 83 GB `Vec<bool>` — and it is a class, not an instance. Reading the option
parsers rather than fuzzing them finds more, and shows the fix is the same
thing as a conformance fix.

`ffmpeg -h filter=<name>` states every option's range:

```text
haas           left_delay      <double>  (from 0 to 40)      default 2.05
stereowiden    delay           <float>   (from 1 to 100)     default 20
asetnsamples   nb_out_samples  <int>     (from 1 to INT_MAX) default 1024
```

Ours read the value and use it:

```rust
// vaco-filter-aeffects/src/haas.rs
common::f64_opt(req, &["left_delay"], 2.05),          // no bound
// vaco-filter-aeffects/src/stereowiden.rs
let delay_ms = common::f64_opt(req, &["delay"], 20.0); // no bound
// vaco-filter-audio/src/asetnsamples.rs
.and_then(|s| s.parse::<usize>().ok()).unwrap_or(1024).max(1);  // lower only
```

and then size a buffer from it:

```rust
self.delay_samples = ((self.delay_ms * sample_rate) / 1000.0) as usize;
self.hist.resize(self.delay_samples.max(1), 0.0);     // Vec<f64>
```

`stereowiden=delay=1e12` at 48 kHz is a 384 TB allocation attempt, and the
reference simply rejects it, because 1e12 is outside `1..100`.

Across the filter crates, **50 of 173 numeric option reads clamp; 123 do
not.** Not every unclamped one is dangerous — most feed arithmetic rather than
an allocation — but every one of them is a *conformance* divergence regardless,
because the reference's parser refuses out-of-range values and ours accepts
them.

`compensationdelay` is the counter-example and shows the shape of the fix: its
`mm`/`cm`/`m` options are clamped to the reference's own `0..10`, `0..100`,
`0..100`, so it is both correct and safe for free.

### Why this is one job, not two

Finding 27 records that `-h filter=<name>` prints no option table. The ranges
needed to *print* that table are exactly the ranges needed to *enforce* these
bounds. Building an option schema with min/max per filter closes both, and
gives the fuzzer a much smaller space to search. Doing them separately means
transcribing every range from the reference twice.

## 32. `framecrc`'s `#tb` follows the input in the reference and the frame rate in ours — which breaks the harness's own comparison mode — CLOSED 2026-08-27

The utility muxers are in better shape than FM-20 (#572) suggests. `crc`,
`md5`, `hash` and `streamhash` are **byte-identical to the reference**,
including the digests:

```text
crc         CRC=0xc72be0fe                      identical
md5         MD5=6da5754c4a6f4c67449a02c903e59232 identical
hash        SHA256=02c27837…937010              identical
streamhash  0,v,SHA256=02c27837…937010          identical
```

`framecrc` is the exception, and it is the one the differential harness
compares with.

### The per-frame CRCs are right; the timestamps are in the wrong base

```text
$ ffmpeg     -i long.mp4 -c copy -bitexact -f framecrc -
#tb 0: 1/12800
0,      -1024,          0,      512,     1516, 0x458c7be9
$ vaco       -i long.mp4 -c copy -bitexact -f framecrc -
#tb 0: 1/25
0,          0,          2,        1,     1516, 0x458c7be9
```

The checksums match exactly — `0x458c7be9` — so the packet payloads are
right. Every timestamp column is wrong, because the base is.

The reference's `#tb` **follows the input stream's time base**; ours derives
`1/frame_rate` from `CodecParameters`. Measured on two containers:

```text
             ref       ours
long.mp4     1/12800   1/25
long.ts      1/90000   1/50
```

The TS row is wrong twice: the wrong *source*, and then `1/50` rather than
`1/25` because an H.264 stream's `CodecParameters::frame_rate` is deliberately
the **tick** rate, which is what issue #632 was about.

### It is `INTERFACE-GAPS.md` gap 9, and this raises its priority

`crates/format/vaco-mux-hash/src/header.rs` explains itself honestly and the
explanation is the diagnosis:

> `Muxer::add_stream` receives only `CodecParameters` — no time base — and
> `Muxer::stream_time_base` is a getter the *caller* queries, never a value the
> caller hands back. A muxer that answers `None` therefore has no channel of
> its own to learn what base ended up governing its packets.

Its fallback (recompute `1/fps` the way the reference's raw/PCM *encoders* do)
is right for the case it was written for — dumping freshly encoded raw media —
and wrong for stream copy, which is exactly what the harness does.

So gap 9 is not only "`-disposition` and `-program` have nowhere to go". It
also silently corrupts `framecrc`, one of the ten comparison modes the
conformance suite runs. That moves it from an interface tidiness item to a
correctness blocker.

### Two smaller `framecrc` header divergences, both now fixable

```text
ref  with -bitexact:     #extradata 0:       45, 0x27ba0f4a
ref  without -bitexact:  #extradata 0:       45, 0x27ba0f4a
                         #software: Lavf62.12.100
ours with -bitexact:     #software: vaco
```

- **`#software:` is suppressed by `-bitexact`**, because it carries a library
  version — the same family as the `*_long_name` suppression already recorded
  in `AGENT-CONSTRAINTS.md`. We have it backwards: we print it *only* under
  `-bitexact`, and print a name where the reference prints a version.
- **`#extradata <n>: <len>, 0x<crc32>`** is printed in both modes and we never
  print it. This became reproducible only today: finding 26's read half now
  synthesises extradata for exactly these copied streams, so the 45 bytes are
  there to measure and CRC.

### Status: closed 2026-08-27 (issue #634)

`INTERFACE-GAPS.md` gap 9 gained `Muxer::add_stream_with(&mut self, params,
spec: &StreamSpec)` — a defaulted method forwarding to `add_stream`, so none
of the ~57 existing implementors changed — plus a second, smaller
`Muxer::set_bitexact(&mut self, bool)` for the `#software` half. Both are
wired through `MuxBuilder::add_stream`/`MuxBuilder::open`; see
`docs/format/vaco-format-core.md`'s "The 2026-08-27 addition" for the full
wiring and the `Box<dyn Muxer>`/`TallyingMuxer` forwarding trap it names.

Re-measured against the same `ffmpeg 8.1`, same fixture shapes:

```text
             ref       ours (before)   ours (after)
long.mp4 #tb 1/12800   1/25            1/12800
long.ts  #tb 1/90000   1/50            1/90000
```

`#extradata`'s hash turned out **not** to be `vaco_hash::crc32` despite the
`0x`-prefixed hex above looking exactly like one — measured by hashing the
real 45-byte `avcC` four ways: `framecrc` uses the same zero-seeded Adler-32
as its packet lines (`0x27ba0f4a`), where real CRC-32 of the identical bytes
is `0x6b488af1`. Using the wrong one would have matched this finding's own
example and silently diverged everywhere else — see
`docs/format/vaco-mux-hash.md`'s `#extradata` section for the full
four-algorithm comparison.

Header lines (`#extradata`/`#software`/`#tb`/`#media_type`/…) now match the
reference byte-for-byte in both `-bitexact` and plain modes, on both an MP4
and an MPEG-TS fixture; a B-frame-free MP4 remux matches on **every** line,
header and packets. `crc`/`md5`/`hash`/`streamhash` were re-diffed and remain
byte-identical (no regression from this change).

**Not closed by this fix**, found while verifying it: a B-frame stream's
absolute `dts`/`pts` differ from the reference by a constant reorder-delay
offset (the *base* now agrees; the values riding on it do not), and MPEG-TS
additionally has an absolute-vs-relative timestamp-origin difference and is
missing a per-packet `S=1, MPEGTS Stream ID, …` side-data field. Neither is
a `#tb`/header fact — both are upstream of this crate (timestamp-origin
resolution and demuxer side-data plumbing, respectively) and are left open
for whichever agent owns those crates.

## 33. `-formats` prints 130 formats twice, and `-demuxers`/`-muxers` do not mask the flag column

**Fixed 2026-08-27** (`a02c76a`). All three rules now hold, and each has a test
that fails without it.

Three separate faults in one function, all in `crates/app/vaco-cli/src/listing.rs`.
Each is the *obvious* implementation, which is why they are worth writing down.

### `-formats` is a union, not a concatenation

```text
$ vaco -formats | sed -n '5p;135,138p'
 DE  aiff            Audio IFF
 D   xpm_pipe        piped xpm sequence
 DE  yuv4mpegpipe    YUV4MPEG pipe
 DE  3g2             3GP2 (3GPP2 file format)     <- the list starts over
 DE  3gp             3GP (3GPP file format)
```

The demuxer pass ran, then the muxer pass ran, so every format that goes both
ways appeared twice and the output was not sorted as a whole. In the reference
that is **130 of 413 names** — not an edge case.

Measured: `ffmpeg -formats` emits one ASCII-sorted row per name, `413` rows
against a `413`-name union of its `-demuxers` and `-muxers` lists.

### `-demuxers` and `-muxers` mask the flag column to the direction asked for

```text
$ ffmpeg -demuxers | grep ' avi '
 D   avi             AVI (Audio Video Interleaved)
$ ffmpeg -muxers   | grep ' avi '
  E  avi             AVI (Audio Video Interleaved)
$ ffmpeg -formats  | grep ' avi '
 DE  avi             AVI (Audio Video Interleaved)
```

`avi` both demuxes and muxes in all three, yet only `-formats` says so. We
printed `DE` in all three, because the row was built by asking the registry
about both directions regardless of which listing was running.

### A both-ways format takes its **muxer's** long name

The two spellings are not always the same, and the reference prefers the
muxer's:

```text
-demuxers   mp3    MP2/3 (MPEG audio layer 2/3)
-muxers     mp3    MP3 (MPEG audio layer 3)
-formats    mp3    MP3 (MPEG audio layer 3)      <- muxer wins
```

They differ for **20 of the 130** both-way formats, including `rtp`/`rtsp`/`sap`
(`… input` vs `… output`), `codec2`/`codec2raw` (`… demuxer` vs `… muxer`),
`g726`/`g726le` (`"left aligned"` vs `"left-justified"`) and `spdif`.

### Also confirmed, and not a bug

The `..d` device slot really is always blank for us, and that is honest: the
reference sets it for exactly three entries in this build (`avfoundation`,
`lavfi`, `audiotoolbox`), all of which are out of scope for v1.0 per plan 18
§9.3 — see `docs/why-some-formats-are-not-included.md`. Devices appear in
`-formats`, `-demuxers` and `-muxers` as well as in `-devices`, so when devices
arrive the slot is the only thing that needs filling, not the row set.

## 34. The banner follows the log level, and `-bitexact` makes `profile=` numeric

**Banner half fixed 2026-08-27** (`a02c76a`), in a shared
`vaco_cli_core::loglevel` both binaries use. The `profile=` half belongs to
P-05 (#275) and is open.

Both found by diffing `ffprobe -v error [-bitexact] -show_streams` against ours
on a plain H.264 MP4. That command produced exactly two differences, and each
turned out to be a rule rather than a one-off.

### `-v`/`-loglevel` below `info` suppresses the banner

We print the banner unless `-hide_banner` is given. The reference also drops it
whenever the log level is below `info`, which is why `-v error` output is clean
on their side and prefixed with a version line on ours.

Measured, counting `ffprobe version` in the output of
`ffprobe -v <level> long.mp4`:

```text
quiet panic fatal error warning   16 24 31    no banner
info verbose debug trace          32 33 40    banner
warn                                          invalid: exit 1, banner printed
level+error   repeat+level+16   +error        no banner
```

Four separate facts fall out of that table:

- The threshold is `>= 32` exactly — `31` is silent, `32` prints.
- Numeric levels are accepted wherever a name is.
- `repeat` and `level` are formatting flags stripped before the level is read,
  in any combination and with or without a leading `+`.
- **`warn` is not a level.** The reference rejects the abbreviation with
  `Invalid loglevel "warn"`, so the accepted set is exactly the nine names.

The invalid-level row is not a special case. The banner is printed *before*
argv is validated — the same ordering already recorded for `ffmpeg -qwerty 3` —
so a value that does not parse simply leaves the level alone and the banner on.
A pre-scan that gives up on an unparseable value reproduces every row above
without a rule of its own.

### `-bitexact` prints the profile number instead of its name

```text
ffprobe -v error           …  profile=High   level=10
ffprobe -v error -bitexact …  profile=100    level=10
```

`level` is unaffected; it is numeric in both. `profile` is the only one of the
pair with a name form, and the name is a library string, so this is the same
family as the `*_long_name` and version-string suppressions already recorded in
`AGENT-CONSTRAINTS.md`. Note it is top-level `-bitexact`, not the positional,
encoding-only `-fflags +bitexact`.

We print `High` in both modes. This is P-05's (#275) to fix, and the constraint
it puts on that work is worth stating: the profile table has to carry the number
*and* the name per row, because a name-only lookup cannot answer the
`-bitexact` question at all.

## 35. MPEG-TS: two stream fields missing, and one we invent

`ffprobe -show_format -show_streams` on an MPEG-TS file now differs in exactly
three lines, and the interesting one is the line we add.

```text
$ diff <(ffprobe … long.ts) <(vaco-probe … long.ts)
< ts_id=1
< ts_packetsize=188
> TAG:ts_codec=h264
```

### `ts_id` and `ts_packetsize` are `[STREAM]` fields

They sit between `nal_length_size` and `id`:

```text
is_avc=false
nal_length_size=0
ts_id=1
ts_packetsize=188
id=0x100
```

`ts_id` is the transport stream id from the PAT; `ts_packetsize` is 188 or 192
depending on whether the file carries timestamped packets. Both are printed in
both modes — `-bitexact` does not touch them. Neither reaches the dump today.

### `TAG:ts_codec` is ours, and the reference has no such tag

The reference prints no `ts_codec` in either mode. `vaco-demux-mpegts` sets it
as stream metadata (`demux.rs:578`, `raw.rs:146`) to carry the TS-level codec
name onward, and stream metadata is exactly what `ffprobe` prints as `TAG:`.

A missing field is a gap; an invented one is a wrong answer, and it is worse
here than it looks — anything comparing our `-show_streams` output against the
reference sees a spurious line on **every** TS stream. The value itself is not
wrong, only the channel: this wants an out-of-band field on the stream rather
than a metadata entry that is user-visible by construction. Note the tests in
`vaco-demux-mpegts/tests/{reference,roundtrip}.rs` read it back through
`metadata_get`, and `vaco-demux-ogg`'s `codec.rs` cites it as precedent, so the
change is not a one-line deletion.

## 36. `-c copy` remux, now that M6 works: what each container still gets wrong

Wiring `vaco_registry::Bsfs` into the CLI (72f555a) made `-c copy` to Annex-B
containers work at all for the first time, which finally made the outputs
comparable. Sizes against the reference, remuxing one H.264 MP4:

```text
              ref      ours     note
mpegts       50760    50760     PSI header byte-identical after f6118c5
mp4           8908     8844
mov           8855     8832
flv           9610     9486
matroska      7907     9412
avi          98464    10126     was a 224-byte stub before M6 worked
```

### MP4: four structural differences, all small and all specific

**1. We always write the 64-bit `mdat`.** The reference writes a 32-bit size
for a 6242-byte payload and we write `size == 1` plus an 8-byte `largesize`.

The 8-byte `free` box we already emit before `mdat` (`wide` for `-f mov` — we
have that right) is not decoration: it is the reference's **reservation** for
exactly this case. It writes a 32-bit placeholder, and if the payload turns out
to exceed 4 GiB it backs up into that box to form a 16-byte 64-bit header in
place. So the two boxes are one mechanism, and we implemented half of it.

**2. `edts`/`elst` is missing** (36 bytes). The reference writes one entry:

```text
elst  segment_duration = 0x1770 (6000)   media_time = 0x400 (1024)   rate = 1.0
```

`1024` is the initial DTS offset — the same `-1024` that shows up as the first
`framecrc` pts. The edit list is how the reference compensates the reorder
delay, so a file without it starts at a different presentation time.

**3. The `avc1` sample entry is missing `pasp` and `btrt`** — 16 + 20 = 36
bytes, exactly the `stsd` size difference (191 vs 155).

```text
pasp   hSpacing = 1        vSpacing = 1
btrt   bufferSizeDB = 0    maxBitrate = 0x2078   avgBitrate = 0x2078
```

**4. `stbl`'s children are in a different order.** The reference writes
`stsd stts stss ctts stsc stsz stco`; we write `stsd stts ctts stss …`. `stss`
and `ctts` are swapped. Order is unconstrained by the specification and load-
bearing for byte-identity.

### The remaining MPEG-TS differences

The PSI header is byte-identical now; 5642 bytes still differ from the first
PES packet on, and the differing offsets cluster at packet offset 4 (the
adaptation field) and 156–168. Three causes, recorded in #636: the PCR base low
bytes are zero in ours, `data_alignment_indicator` is set in our PES flags where
the reference clears it, and the PTS/DTS values differ — the last being the same
family as finding 32.

## 37. Matroska: reordering does not call for a `BlockGroup`

Our `-c copy -f matroska` output was **larger** than the reference's — 9412
bytes against 7907 — which is the interesting direction, because it means we
were writing something extra rather than omitting something.

```text
ref   cluster 6324   {Timestamp: 1, SimpleBlock: 125}
ours  cluster 7736   {Timestamp: 1, BlockGroup: 94, SimpleBlock: 31}
```

The 94 `BlockGroup`s are the reordered frames. The muxer's rule was
`needs_reference = track.reorders && ts != dts`, and its stated reasoning is
the natural one: `SimpleBlock` cannot carry a `ReferenceBlock`, and a B-frame
plainly references other frames, so a reordered frame needs the long form.

**Matroska has no decode timestamp.** A block's timestamp is its presentation
time and decode order is file order, so there is nothing a `ReferenceBlock`
can state that the format does not already imply. Measured on `ffmpeg -c copy
-f matroska`, remuxing reordered H.264, and again with AAC alongside it: every
block is a `SimpleBlock`, zero `BlockGroup`s in either case.

Two costs, and the second is worse than the size:

- 1697 bytes across two clusters.
- **Every wrapped frame lost its keyframe flag.** `block_group` is not passed
  `is_key` at all, because a `BlockGroup` states keyframe-ness only by the
  *absence* of a `ReferenceBlock` — so a keyframe that needed a `BlockGroup`
  for any other reason would have been indistinguishable from a P-frame.

`BlockGroup` is still right when a packet's duration differs from the track's
`DefaultDuration`, which is the subtitle case, and that rule is unchanged.

The remaining Matroska divergences are the other direction — our `Info` (53 vs
75), `Tracks` (109 vs 152) and `Tags` (132 vs 255) are all smaller, so there
are elements we do not write.

## 38. Matroska: what is left after the `SimpleBlock` fix, and one thing that cannot be fixed

First, a correction to my own measurement, worth recording because it is the
trap the suite header already warns about and I walked into it anyway.

Comparing with top-level `-bitexact`, three successive **reference** runs
produced three different files, with a random `SegmentUUID` and `TrackUID`
each time. That reads exactly like "the Matroska muxer is nondeterministic",
and it is not. `-fflags`/`-flags` are *per-file* options: the flags have to sit
**immediately before the output path**, and top-level `-bitexact` does not
reach the muxer. With them placed correctly the reference is byte-identical
run to run, writes no `SegmentUUID` at all, and writes `TrackUID` as a
deterministic 1. `remux-bitexact.toml` says all of this in its header. Measure
with the harness's own command line, not a simplified one.

### The remaining divergences

`-c copy -fflags +bitexact -f matroska`: ref 7841 bytes, ours 7715.

| | reference | ours |
|---|---|---|
| `Info/Duration` | 8 bytes | absent |
| `TrackUID` | `00 00 00 00 00 00 00 01` | `01` |
| `Video/FlagInterlaced` | present | absent |
| `Video/Colour` | 8 bytes | absent |
| `MaxBlockAdditionID` | present | absent |
| `Void` after it | 2 bytes | absent |
| `Tags` | 226, two `Tag`s | 132, one `Tag` |

`TrackUID` is the interesting small one: EBML lets a uinteger be encoded in as
few bytes as it needs, and we take that option. The reference always writes
eight. Both are valid; only one is byte-identical.

`TrackEntry`'s child order also differs, and Matroska does not constrain it:

```text
ref   TrackNumber TrackUID FlagLacing Language CodecID TrackType
      DefaultDuration Video MaxBlockAdditionID Void CodecPrivate
ours  TrackNumber TrackUID TrackType FlagLacing Language CodecID
      DefaultDuration CodecPrivate Video
```

### `MuxingApp`/`WritingApp` cannot be made byte-identical, and should not be

Under bitexact the reference writes `Lavf` — four bytes, no version. We write
`vaco-mux-matroska`, seventeen. That is the right choice: it is our identity,
the same call as `#software` in `framecrc` and the `-version` banner, and
writing `Lavf` into a file we produced would be a false claim about what made
it.

The consequence is that **Matroska output can never be byte-identical to the
reference**, no matter how many of the rows above are closed — there are 26
bytes of honest divergence in every file. Either the harness needs a normaliser
for these two elements specifically, or Matroska belongs in
`remux-structural.toml` rather than `remux-bitexact.toml`. This is a harness
decision, not a muxer bug, and it should be made deliberately rather than
discovered when the last row closes and the comparison still fails.

## Harness changes, summarised

Everything below is a change to `crates/tool/vaco-conformance/`,
`tests/conformance/`, or the repository's `justfile`, made because the
harness or a suite was *wrong*, not merely incomplete — each is also finding
8 through 13 above with its full reasoning:

- `{output}`/`{output:<name>}` argv token + `Capture::OutputFile` actually
  implemented (finding 10) — nothing about the byte-identical tier of XF-03
  was possible before this.
- `bitexact-copy` normaliser, and `bitexact`/`bitexact-copy` made positional
  for the transcode tools (findings 8, 9).
- `Compare::Behavioural`'s exit-code handling fixed to match its own
  documented "outcome class, not literal code" contract (finding 11).
- `[[exclude]]` can bind a synthetic `media` pseudo-axis (finding 12).
- `probe-matroska`'s WebM fixture no longer depends on an optional encoder
  the reference build may not have (finding 13).
- `vaco-conformance run --case <id>` implemented. Every reproduction command
  this harness has ever printed (`just conformance-run '<id>'`) was
  unusable before this pass — the CLI had no `--case` flag, so the recipe's
  `-- run --case "<id>"` silently ran the *whole* declared tier instead of
  reproducing one case. `Tally::record` made `pub` to support it.
- The duplicate `just conformance` recipe (two recipes, same name — a `just`
  parse error, so `just conformance` did not run *at all* before this fix)
  collapsed into one; `just conformance`/`just conformance-run` now build
  both `vaco-probe` and `vaco` and wire `VACO_BIN_PROBE`/`VACO_BIN_VACO` for
  the transcode suites, not only the probe ones.
- `Runner::probe_produced_files`: a `structured-diff` case on the
  `transcode` tool now probes the two *written files* with each side's own
  probe binary and diffs those listings, rather than the transcode tool's
  own progress output — reusing the existing C6 machinery rather than
  building a second one. This is what `remux-structural.toml` runs on, and
  it is what found findings 20–22.

## How to re-run

```sh
cargo build -p vaco-probe -p vaco-cli --offline -j 4 --target-dir /tmp/vaco-conf-v4n8
VACO_BIN_PROBE=/tmp/vaco-conf-v4n8/debug/vaco-probe \
VACO_BIN_VACO=/tmp/vaco-conf-v4n8/debug/vaco \
  cargo run -p vaco-conformance --offline --target-dir /tmp/vaco-conf-v4n8 -- run --tier core
```

Or, more simply, `just conformance` (broken — a duplicate `just` recipe named
`conformance` — before this pass; see *harness changes* above).

Every case prints its own reproduction command, and as of this pass that
command actually works: `vaco-conformance run --case '<id>'` (or
`just conformance-run '<id>'`) reproduces exactly one case, bypassing tier
gating, instead of silently running the whole suite because `--case` was not
a flag the CLI recognised.

The media is synthesised by the reference at run time and discarded (D6) —
nothing FFmpeg-derived is committed, and a file described by a command in the
manifest defends its own provenance in a way a checked-in fixture does not.

## 36. Finding 34's `-bitexact profile=` half — fixed, and three more codecs measured

Finding 34 above left this open for P-05 (#275): `-bitexact` prints the raw
numeric `profile` where a plain run prints the library name, and the fix
needed `Emit` to be able to *change* a field's value under `-bitexact`, not
merely drop it — `Emit::dropped_by_bitexact` only ever answered "omit this
field entirely" (`*_long_name`), which cannot express "print `100` instead of
`High`". Added `Emit::is_bitexact()` and threaded a `bitexact: bool` through
`stream_value`; the `"profile"` arm in `vaco-probe/src/show.rs` now picks
`Profile::value` under `-bitexact` and `Profile::name` otherwise.

Measured on four codecs, not just the H.264 case finding 34 recorded:

```text
                 plain        -bitexact
H.264 (High)     profile=High profile=100
AAC (LC)         profile=LC   profile=1
VP9 (Profile 0)  profile=Profile 0   profile=0
AV1 (Main)       profile=Main profile=0
```

One more thing fell out while fixing this: **a profile with no name at all
prints the number in *both* modes**, not just under `-bitexact`. VP8's
`profile` is a bare `version` number the reference never gives a name (`ffprobe
-show_entries stream=profile` prints `profile=0` whatever `-bitexact` says),
and the same is true for any H.264 `profile_idc` Annex A never assigned —
`vaco_codec_core::Profile::name` is `""` for those by convention, and the old
`Val::opt_s(p.profile.map(|x| x.name))` would have printed an *empty* string
for them rather than falling back to the number. Not previously observable
because no codec crate emitted an empty-named `Profile` before `vaco-parse-vpx`
landed with VP8. Both cases (`bitexact`, empty name) now take the same
numeric-fallback branch in `stream_value`.

`Profile` already carries both `value: i32` and `name: &'static str` — the
type did not need to change, only the printing path did.

## 37. VP9's uncompressed header has no level syntax element — measured

Closing P-06 (#276)/P-05 (#275): `vaco-parse-vpx` is the first parser for VP9,
and its profile/level table (`vaco_parse_vpx::profile`) is the piece P-05 was
waiting on.

Measured directly, since this is exactly the kind of "obviously true" claim
AGENT-CONSTRAINTS warns about: `libvpx-vp9 -level 4.0`, remuxed through both
WebM and MP4 (the latter carrying a `vpcC` box whose second byte *is* a level,
byte-verified against a hex dump), still reports `level=-99` from `ffprobe
-show_entries stream=level` in both containers — `ffprobe`'s own VP9 reader
never looks at `vpcC`'s level byte, and the bitstream's `uncompressed_header()`
has no level field to read at all (unlike AV1's in-band `seq_level_idx`). So
`vaco-parse-vpx::vp9::Vp9Parser` never sets `CodecParameters.level`, matching
the reference's behaviour exactly rather than fabricating a value from `vpcC`
that the reference itself ignores. The Annex A level table
(`vaco_parse_vpx::profile::LEVELS`) exists for the framework requirement and
for a future MIME-string builder, cross-checked against a public secondary
transcription rather than any decoder's source (the same caveat
`vaco-parse-av1::profile` records for its own table) — flagged as
unverified-by-measurement in the P-06 issue-closing comment, since `level`
never surfaces through `ffprobe` to check a row against.

Also measured while building the `vpcC` reader: the box's payload (as
`vaco-format-isom` hands it to `Parser::set_extradata`) includes the `FullBox`
version/flags bytes, not just the record fields — confirmed by hex-dumping a
real `vpcC` box (`01 00 00 00 02 0a a2 02 02 02 00 00`: version, flags,
profile=2, level=10, bitDepth=10/chromaSubsampling=1/fullRange=0 packed into
one byte, three colour code points, then a zero `codecIntializationDataSize`).

## 39. AVI is written on a fixed 600 Hz grid, padded with empty chunks

`-c copy -fflags +bitexact -f avi` gives 98430 bytes from the reference and
10126 from us — nearly ten times the size for the same 150 frames of payload.
The difference is not padding in the ordinary sense; it is the whole timing
model.

```text
              ref                     ours
strh          600/1  length 3600      25/1  length 150
avih          totalFrames 3600        totalFrames 150
              usPerFrame  78          usPerFrame  0
movi          150 real + 3450 empty   150 real
idx1          57600 (3600 x 16)       2400 (150 x 16)
hdrl          4710                    192
```

**AVI has no per-packet timestamp.** It is strictly constant-rate, and a frame's
presentation time is its ordinal. So the reference does not write the stream's
frame rate; it writes a **fixed 600 Hz grid** and places each real frame in the
slot its timestamp falls in, filling every unused slot with a zero-length
`00dc` chunk. 3450 of the 3600 slots are empty here.

600 is constant, not derived. Measured across six source rates:

```text
src fps    10     24     25    29.97    30     50
strh      600/1  600/1  600/1  600/1  600/1  600/1
```

`length` is then `duration x 600` every time.

**`dwMicroSecPerFrame` is not the grid period.** It would be 1667 if it were.
It tracks the *source* time base instead — `1e6 / time_base.den`:

```text
src tb   1/10240  1/12288  1/12800  1/15360  1/30000
avih          97       81       78       65       33
```

That is internally inconsistent with `strh`, and it is what the reference
writes. We write `0`, which is not a plausible value under any reading.

Writing 25/1 and 150 frames the way we do is not a smaller version of this —
it is a different file. A player takes `strh` at its word, so our output plays
at the right speed only because our frame count and rate happen to agree with
each other; the moment the source is variable-rate or has gaps, the two files
diverge in *content*, not just in bytes.

Also missing from `hdrl` (4710 vs our 192): a `vprp` video-properties chunk
(68 bytes) and three `JUNK` paddings — 4120 inside `strl`, 260 after it, and
1016 after `hdrl` itself. The large one is an alignment reservation; the
reference pads `movi` to a 2048-byte boundary.

## 40. FLV: the input's container metadata is dropped, and the end-of-sequence tag is missing

`-c copy -fflags +bitexact -f flv`: ref 9585 bytes, ours 9486. Two causes, and
the 99 bytes divide cleanly between them.

**The `onMetaData` script tag does not carry the input's container metadata**
(238 bytes against our 159):

```text
ref   onMetaData duration width height videodatarate framerate videocodecid
      major_brand minor_version compatible_brands filesize
ours  onMetaData duration width height videodatarate framerate videocodecid
      filesize
```

`major_brand`, `minor_version` and `compatible_brands` are the *input* MP4's
format-level metadata, which `-map_metadata`'s default forwards to the output.
The seven keys we do write are all derived from the stream; none is forwarded.
So this is not an FLV-specific gap so much as the format-metadata channel not
reaching this muxer.

**The stream is not terminated.** The reference's last tag is

```text
type 9 (video), 5 bytes: 17 02 00 00 00
```

— keyframe, codec 7 (AVC), `AVCPacketType = 2`, "end of sequence". Ours ends on
an ordinary NALU tag. A reader that trusts the terminator to know the sequence
is complete sees a truncated file.

## 45. FT-4.13e's remaining audio filters (#485): three cases where black-box probing hits a real wall

Ten filters split across `vaco-filter-audio`, `vaco-filter-adynamics` and
`vaco-filter-aeq` (closing epic #58). Two landed bit-exact; the rest hit one
of three distinct walls worth recording so the next agent does not re-spend
the time finding them again.

**`amultiply` is bit-exact — the easy case, recorded for contrast.** A 1 kHz
and a 500 Hz tone through the reference match the elementwise product of the
two unfiltered inputs at full `f64` precision. No gain stage, no clamp,
nothing to get subtly wrong.

**`acrusher`'s quantiser is exact; four of its options are not, and probing
found *why* they resist a formula rather than just failing to find one.**
`bits`/`mix`/`level_in`/`level_out` at `dc=1, aa=0, samples=1, mode=lin` are
pinned exactly (`round(x * (2^bits-1)) / (2^bits-1)`, with `mix=1` measured
to mean *dry* and `mix=0` *wet* — the reverse of the usual convention). But
`dc != 1` produces an asymmetric quantisation grid — feeding a fine ramp at
`bits=1, dc=2` gives a dead band from `-1` to `+0.24` mapping entirely to
`0`, then evenly `0.5`-spaced steps above it — that does not fit any
bias/scale/clamp combination tried against the `dc=1` formula. `aa != 0`
(the reference's own *default* is `0.5`) replaces the hard staircase with a
continuous curve, not a smoothed version of it. `samples > 1` did not show
the expected "N identical consecutive samples" pattern at `bits=8`. All four
are accepted as options (so a filtergraph string is not rejected) but have
no effect, rather than shipping a guessed curve.

**`aemphasis`/`atilt`: the option surface documents *that* a family of
curves/cascades exists, not *which one*.** `aemphasis`'s `col`/`emi`/`bsi`
(historical 78 rpm de-emphasis curves) have no confidently-available
published time constants — unlike `50fm`/`75fm`/`cd` (standard, well-known
50/75 us broadcast constants) or `riaa` (the standard 3180/318/75 us curve,
here simplified to its single dominant corner). Shipping a made-up number
for three specific curves as if it were their real time constant would be
indistinguishable, to a later reader, from a measured one — so they use an
explicitly-labelled placeholder instead. `atilt`'s `order` (2 to 30) proves
it is a variable-order cascade, but nothing in `-h filter=atilt`'s output
says what `order`/`slope`/`width` map onto structurally; built instead from
a cascade of this crate's own verified `biquad::tilt` construction, which
does something in the right direction (more `order`, steeper transition)
without claiming the reference's exact shape.

**`apsyclip`, `adynamicequalizer`, `adrc`'s non-default path: the option
list names a real subsystem `-h` cannot describe.** A psychoacoustic
clipper's masking model, a per-FFT-bin transfer expression's grammar, and a
dynamic EQ's exact threshold unit are none of them recoverable from an
options table — `-h` gives parameter names, not algorithms. Each ships a
real, working, explicitly-labelled substitute in the right family (an
iterative corrective clipper; a broadband time-domain compressor; a
detector-driven biquad gain, respectively) rather than a guess dressed as
the real thing. `adrc`'s default (`transfer=p`) *is* measured: diffed
against the unfiltered input, it matches to `1e-9` after an ~15-sample
settle, confirming "p" really is "pass".

**One real published algorithm, not a substitute: `adynamicsmooth`.**
Andrew Simper (Cytomic)'s self-modulating dynamic smoothing filter (2014) is
a citable, independent construction that happens to fit this option surface
(`sensitivity`, `basefreq`) exactly. Implemented from the algorithm's own
description and checked against its own mathematical properties (unity DC
gain; `sensitivity=0` degenerates to a plain two-pole low-pass, checked
against an independent from-scratch computation of that case) — this one
is algorithmically faithful by construction, not by black-box measurement,
because there was a real specification to be faithful to.

Not fixed further in this pass; `docs/filter/vaco-filter-{audio,adynamics,aeq}.md`
carry the same breakdown per crate.

## 41. We never print the `Input #0` / `Output #0` dump, and no work package covers it

The single most visible thing `ffmpeg` prints, and we print none of it. `vaco -i
file.mp4` with no output emits only the error:

```text
$ ffmpeg -hide_banner -i long.mp4                    $ vaco -hide_banner -i long.mp4
Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'long.mp4':  At least one output file must be specified
  Metadata:
    major_brand     : isom
    minor_version   : 512
    compatible_brands: isomiso2avc1mp41
    encoder         : Lavf62.12.100
  Duration: 00:00:06.00, start: 0.000000, bitrate: 11 kb/s
  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(progressive), 64x64 [SAR 1:1 DAR 1:1], 8 kb/s, 25 fps, 25 tbr, 12800 tbn (default)
    Metadata:
      handler_name    : VideoHandler
      encoder         : Lavc62.28.100 libx264
At least one output file must be specified
```

The exit code and the error line are both right; everything above them is
missing. A transcode is missing the matching `Output #0` block as well, plus
`Press [q] to stop, [?] for help`.

Grepping `planning/14-cli.md` for this dump finds nothing. CL-17 (#208) covers
`-progress`/`-stats`/`-report`, which is the `frame= fps= Lsize=` line, not
this. **No work package covers the input/output dump at all** — a planning gap
rather than an unstarted item, which is why it has stayed invisible while
narrower CLI work was scheduled and finished.

The data is not the hard part: `ffprobe -show_streams` already produces every
field in that stream line, so this is a formatting job over plumbing we have.
Three shapes to reproduce, all measured:

- **Program blocks.** MPEG-TS input prints `Program 1 ` (note the trailing
  space) with its own indented `Metadata:` before the stream lines, and its
  stream line carries `start 1.480000` where MP4's does not.
- **Per-stream metadata** is indented under each stream, not merged into the
  container block.
- **`-bitexact` does not suppress `encoder : Lavf62.12.100` here.** That looks
  like the version-string rule and is not: this is metadata *read from the
  file*, not our identity. The suppression rule applies to strings we author.

## 42. Every WAV stream reported `sample_fmt=unknown`, and `-c copy` to a PCM container failed outright

`vaco -i in.wav -c copy out.wav` — about as basic as this program gets — did
not work:

```text
Error: the muxer refused a stream: unsupported: wav: sample format must be known
```

So did `-f w64` and `-f caf`. The muxers were right to refuse: the demuxer
handed them a stream whose sample format was `None`.

### One dead condition, in two files

`wav.rs` and `w64.rs` both derived the sample format inside

```rust
if codec_id == Some(vaco_codec_core::CodecId::Pcm) { … } else { (None, None) }
```

and `wave_tags::codec_id` returns the **specific** variant — `PcmS16le`,
`PcmF32le`, `PcmAlaw` — never the generic `CodecId::Pcm`. The branch could not
fire for any real file. `Pcm` is the placeholder for a width the enum has no
variant for, which today is 64-bit integer PCM only.

Measured before, on eight `-c:a` variants of the same source, every one wrong:

```text
            reference                    ours
pcm_u8      pcm_u8,u8,N/A                pcm_u8,unknown,N/A
pcm_s16le   pcm_s16le,s16,N/A            pcm_s16le,unknown,N/A
pcm_s24le   pcm_s24le,s32,24             pcm_s24le,unknown,24
pcm_s32le   pcm_s32le,s32,32             pcm_s32le,unknown,32
pcm_f32le   pcm_f32le,flt,N/A            pcm_f32le,unknown,N/A
pcm_f64le   pcm_f64le,dbl,N/A            pcm_f64le,unknown,N/A
pcm_alaw    pcm_alaw,s16,N/A             pcm_alaw,unknown,N/A
pcm_mulaw   pcm_mulaw,s16,N/A            pcm_mulaw,unknown,N/A
```

All eight match now, and **`-f wav` and `-f w64` are byte-identical to the
reference** for a PCM stream copy.

### Why the lookup is keyed on the codec and not on the width

`sample_fmt_for(bits, is_float)` already existed and is not the right tool here.
Two rows in `codec_id_for`'s own measured table say why: A-law and µ-law are
eight bits coded and decode to `s16`, and `pcm_s8` decodes to `u8` because there
is no signed 8-bit sample format. A width lookup gets both wrong. The new
`pcm::sample_fmt_of(CodecId)` answers `None` for anything not PCM-shaped, so it
doubles as the family test the dead condition was reaching for.

`aiff`, `au`, `caf` and `sox` were already right — they go through a different
path. Only the two RIFF-shaped readers shared the bug, and they shared it
because one was written from the other.

## 43. Four simple-audio muxers wrote files whose audio was wrong

Following finding 42, `-c copy` from a WAV source into `wav`/`w64`/`caf`/`aiff`
worked — and produced files whose decoded audio did not match the input. Not a
byte-identity gap: **corrupt output, no error, and every unit test green.**

Decoded MD5 of our own output against the source, before:

```text
codec       wav      w64      caf      aiff
pcm_u8      ok       ok       CORRUPT  ok
pcm_s16le   ok       ok       CORRUPT  CORRUPT
pcm_s24le   CORRUPT  CORRUPT  ok       —
pcm_alaw    CORRUPT  CORRUPT  CORRUPT  —
pcm_mulaw   CORRUPT  CORRUPT  CORRUPT  —
```

and after: every cell either `ok` or a refusal the reference also makes.

### One mistake, four times

Each muxer built its container header from the **decoded sample format**
instead of from the codec. Those differ in exactly the ways `pcm.rs`'s own
measured table has always said they do:

- `pcm_s24le` decodes to `s32`, so `format.bits_per_sample()` answers 32. The
  WAV and W64 headers claimed 32-bit, and the reference read our output back as
  `pcm_s32le`.
- A-law and µ-law decode to `s16` while storing one byte per sample. WAV tagged
  them `WAVE_FORMAT_PCM` at two bytes per sample; CAF wrote `lpcm`.
- Endianness is not in the sample format at all. AIFF is big-endian by
  definition and we wrote little-endian bytes under a plain `AIFF` header —
  the reference read our own file back as `pcm_s16be`, every sample
  byte-swapped. CAF's `mFormatFlags` was `u32::from(is_float)`, so the
  little-endian bit was never set.

The AIFF guard read `if format.is_float() || format.is_planar() { reject }`
under a comment saying "only big-endian integer PCM is supported for writing".
`pcm_s16le` is neither float nor planar, so it sailed through the guard that
existed to stop it.

### Two more, found on the way

**AIFF's frame count was short.** `write_packet` accumulated
`frames_in(payload_len, bytes_per_frame)` per packet, and `frames_in` floors —
so any packet that is not a whole number of frames loses up to
`bytes_per_frame - 1` bytes, every packet. A 24-bit file declared an `SSND`
three frames shorter than what it had written. Counting bytes and dividing once
at the trailer fixes it; CAF already did that.

**Inserting a chunk moved a patch offset.** CAF's trailer seeks to a computed
position to patch the `data` size. Adding the `chan` chunk shifted it, and the
seek then wrote the data length into *`chan`'s* size field. The file was still
exactly the right length and the header still looked plausible; only walking the
chunks showed it.

### Byte-identity, as a side effect

`caf` is now byte-identical to the reference for all seven codecs tested, and
`aiff` for all six it accepts. `wav`/`w64` are identical for `pcm_s16le` and
`pcm_u8` and differ for the rest, because the reference writes
`WAVE_FORMAT_EXTENSIBLE` with a 40-byte `fmt ` chunk for 24-bit and above where
we write plain PCM with 16 — valid either way, and a separate piece of work.

For `pcm_s24le`/`pcm_s32le`/`pcm_f32le` into AIFF the reference emits an
AIFF-C `compressionType` of `01 00 00 00`, which is not a FourCC and reads as
uninitialised memory. We refuse those instead. Reproducing the reference's
spelling (D9) does not extend to reproducing what looks like its bug.

## 46. #636 (MPEG-TS `-c copy` timestamps) and #635 (`ts_id`/`ts_packetsize`/`ts_codec`) — both closed

### #636: PCR, `data_alignment_indicator`, PTS/DTS, and a fourth cause the issue did not name

Measured on `long.mp4` (one H.264 video stream, one keyframe, two-B-frame
reorder delay), `ffmpeg -v error -y -bitexact -i in.mp4 -c copy -f mpegts` vs
the same through `vaco`: **5642 bytes differing, down to 1491** after this
pass, all four causes fixed in `vaco-mux-mpegts`.

**1. PCR base was always zero.** Measured with `-muxdelay 0.3/0.7/1.0` and
`-max_delay 700000` (all landing on the same on-wire value when the option is
left at its default): the reference's on-wire PCR is `raw_dts +
MUX_DELAY_TICKS`, where `MUX_DELAY_TICKS` is the reference's resolved
`-max_delay`/`-muxdelay` default (0.7 s → 63 000 ticks @ 90 kHz). We wrote raw
`clock` with no offset at all. `MpegTsMuxOptions` has no live path from the
generic `max_delay` format option yet (nothing constructs this muxer with
anything but its defaults today), so this bakes in the reference's *default*;
wiring a real override through is separate follow-up work, noted in
`vaco-mux-mpegts::mux::MUX_DELAY_TICKS`'s doc.

**2. `data_alignment_indicator` was always set; the reference always clears
it**, video and audio PES headers both, even though every packet this muxer
writes does start on an access-unit boundary. `PesHeaderOut::data_alignment`
stays a real field (a future caller might need `true`); `MpegTsMuxer` now
always passes `false`.

**3. PTS/DTS were unshifted.** Same `MUX_DELAY_TICKS` measurement: the on-wire
PTS/DTS is `raw_pts_or_dts + 2 * MUX_DELAY_TICKS`, a pure additive shift
constant across the whole file, independent of the B-frame reorder delay
(which still shows up as the usual PTS-DTS gap on top of the shift, unaffected
by this fix — that gap is finding 32/`#634`'s territory, upstream of this
muxer). Once shifted, `start_pts`/`start_time` on an MPEG-TS stream now match
`ffprobe` exactly (cross-checked independently via `-show_streams`, not just
the byte-level harness).

**4. Not named in the issue: the reference prepends a fixed Access Unit
Delimiter (`00 00 00 01 09 f0`) to every H.264 access unit**, I-frame,
P-frame and B-frame alike, `primary_pic_type` always `7`. Measured on a source
whose samples carry no AUD at all — confirmed specific to the MPEG-TS muxer,
not the `h264_mp4toannexb` conversion: the same BSF applied standalone
(`-bsf:v h264_mp4toannexb -f h264`) produces no AUD. Round-tripping a source
that already carries one AUD per access unit does not double it. This was the
majority contributor to the remaining diff once 1–3 were fixed (5642 → 1491
bytes, this fix alone accounting for most of that drop).

**What is left, and is not a bug**: the residual 1491 bytes are entirely the
already-disclosed `vaco-bsf-h2645::h264_mp4toannexb` divergence — the
reference writes a 3-byte Annex B start code immediately after a
splice-inserted SPS/PPS pair, where that filter always writes 4 (its own
module doc and `matches_the_reference_on_a_real_mp4_sourced_packet` test
already record and accept this as "not worth a knob"). Confirmed by
reconstructing both sides' elementary streams from the muxed `.ts` files
directly: they differ by exactly one `0x00` byte, at exactly that position,
and nowhere else — 7171 vs 7172 bytes over the whole stream, one keyframe in
150 packets. Not this crate's bug and not touched.

### #635: `ts_id`/`ts_packetsize` added, `ts_codec` deleted, no `vaco-format-core` change

`TAG:ts_codec` is gone: `vaco-demux-mpegts::demux::MpegTsDemuxer::add_stream`
and `::raw::MpegTsRawDemuxer::open_with_limits` both used to call
`stream.metadata_set("ts_codec", …)`. Nothing consumed it (checked: `probe`'s
`codec_name` field never fell back to it), and the reference emits no such tag
in either `-bitexact` mode, so it was pure invention. Deleted from both call
sites; the two tests that pinned its presence
(`vaco-demux-mpegts/tests/roundtrip.rs`) now pin its *absence* instead, and a
new `raw::tests::no_invented_ts_codec_tag` covers the `mpegtsraw` path the
issue also named. Left alone: `vaco-demux-ogg::codec`'s `OggCodec` comment
cites `ts_codec` as precedent for its own `"ogg_codec"` metadata key — a stale
citation now, and if `ogg_codec` turns out to have the identical invented-tag
bug, that is `vaco-demux-ogg`'s crate to fix, not this pass's.

`ts_id`/`ts_packetsize` added, as real `[STREAM]` fields (verified against
`ffprobe -show_streams` on both a video and an audio stream of the same file:
both print `ts_id=1 ts_packetsize=188` in the same position, right after each
media type's own block and before `id`, in both `-bitexact` modes). The
channel is deliberately **not** a new field on `vaco_format_core::Stream` —
that crate is outside this pass's ownership, and per
`planning/AGENT-CONSTRAINTS.md` a change needed in a crate not named by the
brief is reported, not worked around. Instead, `add_stream` sets the two
values through the same `Stream::metadata` channel `ts_codec` used to abuse,
and `vaco-probe/src/show.rs` draws the distinction on the *read* side:
`stream_value` answers the two field names by reading `metadata_get`, and
`stream_visible_metadata` filters those two specific keys out of the generic
`tags()` dump before it ever reaches a writer — so they print exactly once,
as fields, never as `TAG:`. A real container tag (`language`, tested
alongside them) is untouched. This keeps the fix inside the two crates that
already agree on the two names, at the cost of `vaco-probe` needing to know
two literal key strings that `vaco-demux-mpegts` also has to spell correctly —
acceptable for two fields; a real `Stream` field would be the right call if a
third demuxer ever needs the same shape.

`ts_packetsize` is 188 for `PacketStride::Ts`/`Rs` and 192 for `M2ts`,
matching the reference's own "188 or 192 depending on whether the file
carries timestamped packets" — verified with a hand-built M2TS fixture in
`vaco-demux-mpegts/tests/roundtrip.rs`, not merely asserted.

### Falsification

Every fix above was broken deliberately and confirmed to fail a test before
being restored: the PCR offset, the PTS/DTS shift, the AUD insertion, and
`data_alignment_indicator` in `vaco-mux-mpegts`; the `ts_id`/`ts_packetsize`
metadata, the `ts_codec` removal (both call sites), and the `tags()` filter
wiring (specifically the call-site wiring, not just the filtering helper in
isolation — the first version of that test called the helper directly and
would have missed the filter being wired to the wrong place) in
`vaco-demux-mpegts`/`vaco-probe`.

### Gate note

`cargo clippy -p vaco-probe --all-targets -- -D warnings` could not be
verified clean at the time of this pass: it failed transitively on two
crates this pass does not own and did not touch — an unused-variable warning
in `vaco-mux-matroska::mux` (`crates/format/vaco-mux-matroska/src/mux.rs:919`)
and a `match_same_arms` lint in `vaco-format-audio-simple::pcm`
(`crates/format/vaco-format-audio-simple/src/pcm.rs:237`) — both mid-edit by
other agents in this tree during this session. `vaco-mux-mpegts` and
`vaco-demux-mpegts` both passed `cargo clippy --all-targets -- -D warnings`
clean on their own. Worth a re-run once the tree is quiet.

## 44. Raw-PCM containers stated no duration, bit rate or frame count

`ffprobe -show_streams` on a one-second WAV, AIFF or CAF file:

```text
reference   duration_ts=44100  duration=1.000000  bit_rate=705600
ours        duration_ts=N/A    duration=N/A       bit_rate=N/A
```

All three are derivable from what `RawPcmDemuxer` already holds — the data
length, the bytes per frame and the sample rate — and none was stated. The time
base for these formats is `1/sample_rate`, so a tick *is* a frame and
`duration_ts` is the frame count; `bit_rate` is `sample_rate x bytes_per_frame
x 8`, which reproduces the reference exactly on every file measured.

**CAF is now byte-identical to the reference across `-show_streams` and
`-show_format` in full**, and AIFF differs in one field.

### `nb_frames` is stated for AIFF and CAF and not for WAV

```text
        duration_ts   nb_frames
wav           44100         N/A
aiff          44100       44100
caf           44100       44100
```

Both come from the same division. The reference states one and not the other,
for WAV alone. Reproduced rather than tidied up, through an explicit
`forget_frame_count` on the shared demuxer so the oddity is named where it
happens instead of being smuggled into a shared constructor.

### Two left open, both deliberately

**`channel_layout`.** The reference prints `unknown` for WAV and AIFF; we print
`mono`, because `pcm::params` calls `ChannelLayout::default_for(channels)`.
Neither container states a layout — WAV carries a channel mask only in its
`EXTENSIBLE` form and AIFF has no layout field at all — so ours is an
invention, and CAF, which really does state one in its `chan` chunk, already
agrees with the reference. Fixing it needs both halves: stop defaulting in the
demuxer, *and* print `unknown` rather than `N/A` for an unstated layout in
`vaco-probe`. That crate is being edited by another agent, so it is recorded
rather than half-done.

**WAV's `start_pts`/`start_time`.** The reference prints `N/A` for WAV and `0`
for AIFF and CAF. Ours prints `0` everywhere, and setting
`Stream::start_time = Timestamp::NONE` does not change it: `discovery.rs`
overwrites the field from the first packet's pts. Reproducing `N/A` means the
WAV demuxer must not stamp timestamps on its packets at all — which is what the
reference does, and which has consequences the harness already documents
(`remux-bitexact.toml` excludes AVI-sourced remuxes precisely because
"Timestamps are unset in a packet for stream 0"). One cosmetic field is not
worth destabilising the timestamps on a path that now remuxes byte-identically.

## 47. AVI and Ogg demuxers: extradata never reaches stream info, and two invented fields

Sweeping `ffprobe -show_streams` across every container we can both read
produced three clean results and three with something to say. `long.mp4` is
**identical**. MPEG-TS became identical earlier today (#635). CAF became
identical with finding 44. The remaining three:

### AVI: the H.264 extradata is never parsed

Eighteen fields differ, and they are all downstream of one thing:

```text
                       reference        ours
profile                100              unknown
level                  10               -99
pix_fmt                yuv420p          unknown
has_b_frames           2                0
sample_aspect_ratio    1:1              N/A
display_aspect_ratio   1:1              N/A
chroma_location        left             unspecified
is_avc                 true             (absent)
nal_length_size        4                (absent)
extradata_size         45               (absent)
mime_codec_string      avc1.64000a      (absent)
bits_per_raw_sample    8                N/A
```

`strf` carries the 45-byte `avcC` and we do not hand it to stream info, so
nothing that depends on the SPS is filled in. Note `is_avc=true` and
`nal_length_size=4`: the reference stores H.264 in AVI **length-prefixed**, not
Annex-B, which is worth knowing before anyone writes the muxer half.

One field is not extradata:

```text
r_frame_rate           25/1             600/1
nb_frames              3600             N/A
```

We read `r_frame_rate` straight out of `strh`, which is the 600 Hz slot grid
(see finding 39, "AVI is written on a fixed 600 Hz grid"). The reference
derives it from the timestamps of the frames that actually carry data, giving
the real 25/1 — while still reporting `nb_frames=3600`, the slot count. Both
numbers are right and they answer different questions.

### Ogg: the serial number is not the stream id

```text
id                     N/A              0xaf733afc
duration_ts            48312            N/A
duration               1.006500         N/A
TAG:encoder            Lavc62.28.100 libopus   (absent)
```

The Ogg bitstream serial is a demultiplexing key, not a container-assigned
stream identifier, and the reference reports none. We expose it as `id`, which
is the same class of mistake as the `TAG:ts_codec` just removed from MPEG-TS:
a real internal value published through a field that means something else.

**And `ogg_codec` is that mistake exactly.** `vaco-demux-ogg`'s `codec` module
records unrecognised codec names in `Stream::metadata` under `"ogg_codec"`, and
its doc comment cites `vaco-demux-mpegts`'s `ts_codec` as the precedent for
doing so. That precedent was removed today because stream metadata is what
`ffprobe` prints as `TAG:`, so the value shows up in output the reference never
produces. `ogg_codec` will do the same on any Theora or Speex stream — the two
cases it exists for — and it has not been measured only because neither is easy
to produce here. The comment citing the removed precedent is now stale either
way.

The Vorbis comment header is also not read, so `TAG:encoder` and everything
else in it is missing.

### FLV

Covered by finding 40 ("FLV: the input's container metadata is dropped"): the
read side is missing `bit_rate` and the same three container tags the write side
fails to forward, which suggests one cause rather than two.

## 48. #641/#208 (CL-17): the `Input #0`/`Output #0` dump and `-stats` are now implemented; `-progress`/`-report` are not

Finding 41 measured that we printed none of `ffmpeg`'s `Input #0, …`/
`Output #0, …` dump. `crates/app/vaco-cli/src/dump.rs` now renders it —
container `Metadata:`, `Duration:`/`start:`/`bitrate:`, `Program N ` blocks,
per-stream lines with codec/profile/tag/pix_fmt/SAR-DAR/bitrate/fps/tbr/tbn,
per-stream `Metadata:`, and disposition parentheticals — and
`crates/app/vaco-cli/src/lib.rs` prints it right after each input opens,
*before* the "no output" check, matching the reference's own order (the whole
reason finding 41 exists: `ffmpeg -i in.mp4` with no output prints the dump
and then that error).

```
$ diff <(ffmpeg -hide_banner -i F 2>&1) <(vaco -hide_banner -i F 2>&1)
```

is empty for all three graded fixtures: `long.mp4` (one video stream),
`media/long_av.mp4` (video+AAC), `long.ts` (MPEG-TS with a `Program` block),
plus `-v warning` (suppresses the whole dump) and `-bitexact` (keeps
`encoder : Lavf…` — finding 41's own point that this is file metadata, not our
identity string). `Vaco-Spec-Ref: ffmpeg-cli-dump-probe` in the landing commit
has the measurements the formatting rules below were built from — 16-char
metadata key padding (`%-16s`, not block-relative), the `av_dump_format`-style
fourcc (uppercase, distinct from `ffprobe`'s own lowercase `codec_tag` field),
`fps`/`tbr` (two decimals when non-integral, none when whole — checked against
four frame rates including a synthesised VFR file to separate `fps`
(`avg_frame_rate`) from `tbr` (`r_frame_rate`), which the three fixtures alone
cannot do since both are equal on all three), `tbn`'s `k`-suffix rule (exact
multiples of 1000 only), and multi-flag disposition parentheticals
(`-disposition:v default+forced` prints `(default) (forced)`, space-joined,
not comma-joined into one).

**What this does not cover, by design:**

- Color-description parentheticals (`(tv, bt709, progressive)`) — only field
  order is reproduced; none of the three fixtures carry non-default color
  info.
- The `Output #0` side's `tbr`/`tbn` are the *source* stream's values, not the
  muxer's own (measured: `long.mp4 -> mpegts` changes `12800 tbn` to
  `90k tbn` on the output line, because the muxer picks its own time base
  before `write_header`, and nothing before this pass hands the CLI a handle
  on the opened muxer at print time). The `Output` block also never carries
  `-map_metadata`'s copied-from-input tags (only this output's own explicit
  `-metadata`) and never a `q=…` segment (there is no encoder in this build
  for that number to describe). None of this is exercised by the graded
  `-i F` (no output) diff loop.
- `Output #0`/`Press [q] to stop, [?] for help` print before
  `exec::run_pipeline` is called, but `Stream mapping:`/the muxing-overhead
  summary still print *after* it returns (unchanged from before this pass) —
  `run_pipeline` computes and writes the mapping and the bytes in one
  blocking call with no earlier hook, so the two blocks are not perfectly
  interleaved with the reference's own order.
- `-stats` (`crates/app/vaco-cli/src/stats.rs`) prints one *final* line, not
  the reference's periodic ones — `vaco_sched::Driver::run` has no progress
  callback to hang a periodic print on. `speed=`/`elapsed=`/`fps=` are real
  `vaco_time::Instant`-derived numbers (routed through `vaco-time`, per
  `cargo xtask time-gate`), not faked; `time=` is approximated from the
  input's stated duration rather than a muxed presentation-time range, because
  `nullmux::StreamTally` carries packet/byte counts and not timestamps.
- `-progress` (the same data as `key=value` pairs to a file or pipe target)
  and `-report` (a full debug log file) are not implemented at all — both are
  separate output sinks, not formatting variants of `-stats`'s line, and are
  a larger job than this pass. Left for a follow-up; #208 stays open for them
  with this note.

Landed: #641 (closed) and the dump/`-stats` half of #208 (left open, scoped to
`-progress`/`-report`).

## 49. #637/#638: `mdat`, `elst`, `btrt`, and six Matroska items closed; two gaps found that neither muxer can fix alone

`-c copy -fflags +bitexact` (flag placed correctly, per finding 38's own
warning), `long.mp4` remuxed:

```text
        before        after
mp4     8860 / 8908   8871 / 8871   (byte count now exact; see the timescale
                                      gap below for why content still differs)
matroska 7715 / 7841  7770 / 7841   (remaining gap is entirely the CLI gap
                                      below, not a muxer bug)
```

### MP4 (#637)

The three items landed as measured: `mdat` now writes a 32-bit size for a
payload that fits (the `free`/`wide` box before it really is the reference's
own reservation for the 64-bit case — backing up into it at patch time
reuses the same 16 bytes rather than moving anything); `edts`/`elst` is
written unconditionally, one entry, `rate` always `1.0`; `btrt` is written
for both video and audio, using the container's own `bit_rate` when there is
one and a total-bits-over-duration fallback when there is not (measured
against a raw H.264 elementary stream, which never has one).

Three more, found while measuring rather than assigned:

- **`Muxer::set_bitexact` had no caller in this crate.** `MuxOptions::bitexact`
  already existed and was already used to suppress `creation_time_unix` —
  nothing overrode the trait's no-op default, so `-fflags +bitexact` on the
  output never reached it. Exactly the "API with no caller" shape
  `planning/AGENT-CONSTRAINTS.md` already names, found the same way it says
  to: running the actual command and comparing bytes, not a unit test.
- **The `udta ▸ meta ▸ hdlr ▸ ilst` shell is unconditional.** Measured across
  four inputs including a raw H.264 stream with no metadata of its own at
  all: every one gets the shell, with an 8-byte childless `ilst` when there
  is nothing to put in it. This crate previously omitted the whole `udta`
  when there were no tags.
- **`encoder` is dropped under `bitexact`, file-level only.** An MP4-sourced
  `encoder=Lavf62.12.100` tag is carried in from the *input's* own metadata
  on a stream copy (not fabricated by this crate — confirmed by remuxing a
  raw H.264 stream with no such tag at all: under `-c copy -f mp4`, *this
  crate's own* muxer never invents one) and reaches `©too` under a plain
  remux, but the reference's own bitexact output omits it. An explicit
  `-metadata title=...` still comes through under bitexact, so this is
  specifically the auto-populated tool tag.
- **`mvhd.timescale` is `1000` whenever any track is video**, not the
  largest track timescale — this crate's own prior rule, written for the
  audio-only case (`AudioParameters.sample_rate`, still correct and still
  the fallback here), silently mis-fired the moment a video track was also
  present. Measured across a reordered stream, a non-reordered one, a raw
  H.264 stream, and a video+audio file: all four keep `1000` at the movie
  level regardless of the video track's own timescale.

**Two gaps neither muxer can close alone**, both found by measuring past the
four assigned items rather than stopping at them:

1. **The pipeline normalizes `dts` to start at `0` before a muxer ever sees
   it.** Instrumented `MovMuxer::write_packet` directly: the first sample of
   a two-frame-reorder H.264 track that `ffprobe`/this crate's own probe both
   report as `pts=0 dts=-1024` on the *input* arrives at the muxer as
   `dts=0 pts=2` (rescaled units) — same `cts_offset`, because a uniform
   shift to both `pts` and `dts` cancels out of their difference, but the
   original negative `dts` baseline `elst.media_time` needs is gone. Nothing
   in `vaco-mux-mp4` or `vaco-mux-matroska` does this shift; it happens
   upstream, before `write_packet`, in the copy pipeline `vaco-cli`
   drives — outside both of my crates. `TrackState::media_time` (mp4) uses
   `cts_offset` as the best available proxy, which is exact whenever the
   original presentation starts at `pts == 0` (every case measured except
   encoder-priming audio) and undercounts by the priming delay otherwise —
   documented in the method's own doc comment rather than silently
   producing a value that looks right and sometimes is not.
2. **MP4's own per-track timescale is derived from `frame_rate`/`sample_rate`,
   never the input container's own timescale.** The reference preserves the
   input's native track timescale exactly on a stream copy (measured: this
   fixture's input `mdhd.timescale` is `12800`; a real `-c copy` output
   keeps `12800`). `vaco-mux-mp4::MovMuxer::track_time_base` has no way to
   see that fact — `CodecParameters` (`vaco-codec-core`) carries no
   `time_base` field, and packets already arrive pre-rescaled into whatever
   timescale `add_stream` picked. This is why the MP4 byte *count* now
   matches exactly (8871 = 8871) while `mdhd`/`stts`/`ctts`'s raw *values*
   still differ: this crate picks `25` (the frame rate) where the reference
   keeps `12800`, and every duration/offset scales consistently within
   that choice, landing on the same real-world durations through `mvhd`/
   `tkhd`/`elst` (all rescaled to the *movie* timescale, which stays `1000`
   either way) but not the same raw per-track tick values. Closing this
   needs a `CodecParameters`/pipeline change neither `vaco-mux-mp4` nor
   `vaco-mux-matroska` owns — flagged rather than worked around, per
   `planning/AGENT-CONSTRAINTS.md`'s scope rule.

### Matroska (#638)

Six of the seven listed items, plus the `TrackEntry` order, landed as
measured: `Info > Duration` (a `0.0` placeholder on a seekable sink, rewritten
whole — not patched in place — once the real total is known at
`write_trailer`, because `Info`'s body carries its own `CRC-32` and a
narrower patch would have left that checksum wrong; omitted entirely on a
non-seekable sink, matching the same asymmetry this crate already had for
`Cues`); `TrackUID` now always 8 bytes; `Video > FlagInterlaced` (`2` for
progressive, measured; the interlaced field orders map to `1` by the field's
own name, unverified — no interlaced sample was available); `Video > Colour`
(`ChromaSitingHorz=1, ChromaSitingVert=2` for `ChromaLocation::Left`, the one
value measured; every other siting is omitted rather than guessed);
`MaxBlockAdditionID` plus its trailing 2-byte `Void` (both video-track-only,
confirmed absent from an audio track in the same file); the `TrackEntry`
child order.

One more, found the same way as MP4's `set_bitexact` gap: **`encoder` is
dropped under `bitexact`, file-level tags only** — same reasoning, same
measurement (an MP4-sourced `encoder=Lavf62.12.100` file tag disappears from
the reference's bitexact output; a *per-track* `encoder` tag, e.g.
`Lavc62.28.100 libx264`, is a different fact — which codec made that
stream's data, not which tool made the container — and is not suppressed,
confirmed present in the reference's own bitexact output right alongside
it). The crate's own module doc previously asserted the opposite: "this
crate does not reproduce the reference's own auto `ENCODER`/`DURATION`
`SimpleTag`s (those stamp the reference's own build identity...)" — reasoned
from the field's name rather than measured, and wrong in the same way the
`BlockGroup` finding (37) was: `ENCODER`'s value here is `Lavc...`, the
*codec's* identity from the *input*, not the muxer's own. That comment is
now corrected in the module docs.

Also found, not assigned: **`TrackEntry`'s own size field is the full
8-octet VINT width**, not the shortest one this crate's general `element()`
helper picks — measured directly (`Tracks`, `Tag` and `SimpleTag` all use
the shortest width right alongside it, so this is specific to `TrackEntry`).

**The remaining `Tags` gap (one `Tag` vs the reference's two) is a `vaco-cli`
bug, not a `vaco-mux-matroska` one.** `MatroskaMuxer::tags_bytes` already
builds a per-track `Tag` from `MuxMetadata::tags_for_stream` — confirmed by
passing `-metadata:s:v:0 foo=bar` through the real CLI pipeline, which
produces exactly the right `Targets ▸ TagTrackUID` + `SimpleTag` shape. The
input's own per-stream tags (`handler_name`, `encoder=Lavc...`) never reach
there: `crates/app/vaco-cli/src/exec.rs`'s `resolve_mapped_metadata` copies
the input's *file-level* tags into the output's `MuxMetadata.tags` (mirroring
`-metadata`) but has no equivalent copy for *per-stream* tags into
`MuxMetadata.stream_tags` — that field is populated only from an explicit
`-metadata:s:` CLI option. `vaco-cli` is outside this brief's two crates, so
this is reported rather than fixed; a task suggestion was filed for it. The
`CueClusterPosition` width difference downstream of this (10 bytes in the
reference vs 7 here) is a direct consequence of the reference's larger file
needing a wider offset encoding, not a separate bug.

**Not touched, and not attempted**: a `long_av.mp4` (video+audio) spot check
surfaced two more, smaller Matroska divergences past the seven-item list —
a `BitDepth` (`0x6264`) element apparently written for the AAC audio track
even though `TrackOut::bit_depth` here is only ever set for PCM codecs, and
`MaxBlockAdditionID` appearing on that same audio track (with no trailing
`Void`), where the H.264-only sample this pass measured against showed it
video-track-only. Both need a second reference sample to resolve correctly
rather than one-off pattern-matching from a single file, and are left for
whoever picks up the next Matroska pass. The same spot check confirms this
crate's `MAX_CLUSTER_MS = 5000` cluster-splitting heuristic is a pre-existing,
already-documented approximation (the module's own comment says as much) —
a three-`Cluster` reference file came back five `Cluster`s here — unrelated
to today's items and not touched.

## 50. #639/#640: AVI's 600 Hz grid and FLV's metadata/terminator gaps closed; one new grid case found on the audio side

`-c copy -fflags +bitexact`, measured against `ffmpeg 8.1` on three fixtures
(an MP4 with H.264+AAC, a video-only MP4, and an MPEG-TS with B-frames and a
non-zero start time):

```text
        before          after (this session)
avi     15944 / 39304   29268 / 39304   (H.264+AAC MP4 fixture)
avi     19070 / 24898   19070 / 24898   (video-only MP4 fixture, unchanged
                                          input, both before/after already
                                          shown post-fix here)
avi     ~113510(*) / 99360   93350 / 99360   (MPEG-TS fixture; the 113510
                                          figure is this fix's own first,
                                          origin-unaware attempt — see below)
flv     15360 / 15459   15459 / 15459   (byte-identical size; content
                                          differs only in the two
                                          pre-existing duration/bitrate
                                          computations finding 44 covers)
```

### AVI (#639): the grid, `dwMicroSecPerFrame`, and `vprp` landed; two things did not

`vaco-mux-avi` now places video packets on a fixed 600 Hz slot grid
(`GRID_RATE = 1/600`, driven through `Muxer::stream_time_base` so the
existing per-packet rescale machinery hands `write_packet` an already-quantised
slot number) and backfills every empty slot with a zero-length placeholder
chunk before writing the real one. `avih.dwMicroSecPerFrame` is threaded
through `Muxer::add_stream_with`'s `StreamSpec::time_base` (the same channel
`vaco-mux-hash`'s `framecrc` already uses for its `#tb` line), since the
600 Hz grid rate and the value this field needs — the *source* track time
base — are two different numbers by design. A `vprp` chunk is now written
per video `strl`, decoded field-by-field from one measured fixture against
the public OpenDML AVI File Format Extensions layout.

Verified directly, not just by size: on all three fixtures, `ffmpeg -f null -`
against our output exits 0 and the decoded **video** MD5 matches both the
source and the reference's own AVI output exactly, byte for byte — including
the MPEG-TS fixture, which has B-frames (`pts != dts`) and a 1.48-second
start offset.

**Found while measuring, fixed before landing: the grid must rebase to the
stream's own first frame.** The first implementation used each packet's raw
(rescaled) timestamp as its slot number directly. That is correct for a
source whose clock starts at zero, but AVI has no absolute-time field
anywhere — position *is* time, starting at the first byte of `movi` — so a
source that does not start at zero (routine for MPEG-TS, whose clock had
already run 1.4s by the first video frame) got ~840 slots' worth of empty
padding at the very start of the file, inflating both the total slot count
and the file size for no reason. Fixed by capturing the first video
packet's own timestamp as the grid's origin and rebasing every later slot
against it. This is the kind of thing a single-fixture measurement can miss
entirely — the original MP4 fixture happens to start at pts 0, so the bug
was invisible until a second fixture with a different clock shape was
tried.

**Left undone, not silently: `JUNK` padding, and one audio case.** `hdrl`'s
three `JUNK` reservations (inside each `strl`, and before `movi`) are not
written — their exact sizing rule was not determined from the one fixture
measured (both `strl` reservations were the same size, 4120 bytes, despite
very different `strf` content, which rules out "pad to N bytes past `strf`"
but does not by itself say what the rule is), and they carry no semantic
content any reader depends on, so this was left as a documented gap rather
than a guess. Separately: on the one fixture with compressed (AAC) audio,
the reference also writes a small number of zero-length *audio* placeholder
chunks (`01wb`, not `00dc`) that this session's fix does not — same
principle as the video grid ("position is time") extended to a VBR audio
stream's own per-frame duration, but the fixture only showed a 3-chunk
discrepancy and no second fixture was available to confirm the exact
mechanism (candidate: AAC's own `1024/sample_rate` frame duration as a
second, audio-specific grid — `avi` already writes `dwScale/dwRate` as
sample-rate-only for VBR audio today, which this finding does not touch).
Not fixed here; needs its own measurement pass before it should be.

The H.264/HEVC Annex-B conversion this crate already had (finding 16) is
unchanged. Finding 47 ("AVI and Ogg demuxers: extradata never reaches
stream info") already recorded, from the demux side, that the reference
actually stores H.264 in AVI length-prefixed (`avc1` FourCC, `avcC` in
`strf`) rather than Annex-B — this session's own fresh measurement of
`ffmpeg -c copy -f avi` output confirms that independently (`is_avc=true`,
`nal_length_size=4`, `strf` FourCC `avc1`, and the first `movi` chunk's
payload is 4-byte-length-prefixed, not start-code-delimited). That is a
real, separate divergence from what this crate's muxer writes today
(Annex-B, `H264` FourCC, no `avcC`), tracked under finding 47's title, not
re-opened here — changing it touches the same `write_packet`/`strf`/
`check_bitstream` machinery finding 16 already shaped, and deserves its own
measurement pass across both directions (mux and demux) rather than a
drive-by change.

### FLV (#640): both causes closed exactly

`onMetaData` now forwards `major_brand`/`minor_version`/`compatible_brands`
(as AMF0 strings, `minor_version` included despite being numeric) when the
caller supplied them via `Muxer::set_metadata` — previously unimplemented on
this muxer entirely, so `-map_metadata`'s default (which already copies
these from `vaco-demux-mp4::meta::file_type_tags` upstream) had nowhere to
land. `write_trailer` now appends the reference's 5-byte AVC end-of-sequence
tag (`17 02 00 00 00`) at the last real video tag's own timestamp for
`Framing::LegacyVideoAvc` streams; Enhanced RTMP's analogous
`PacketTypeSequenceEnd` for HEVC/AV1/VP9 is not implemented, unverified
against the reference for lack of a fixture.

Verified: output size matches the reference exactly (15459 bytes both);
`onMetaData`'s key list and the terminator tag are byte-identical; decoded
video MD5 matches the source; decoded audio MD5 matches the *reference's own
FLV output* exactly, though not the source — an AAC-priming-sample
limitation inherent to FLV (no edit-list mechanism), present in the
reference's own round-trip too, not something this fix introduced.

Tests: `vaco-mux-avi/tests/roundtrip.rs` gained four —
`video_packets_land_on_the_grid_with_empty_slots_between`,
`the_grid_rebases_to_the_streams_own_first_frame`,
`an_implausible_grid_gap_is_rejected_not_looped_forever`, and
`avih_dwmicrosecperframe_tracks_the_source_time_base`.
`fuzz/fuzz_targets/avi_mux_packet.rs` now varies the `pts`/`dts` gap (bounded
to keep one iteration fast) so the new backfill loop and its `Budget`
accounting get exercised; 30s run, `exit=0`, `execs≈1,660,000`,
`find fuzz/artifacts -type f` empty.

## 51. C-13 (#291/#292/#293): BMP/PCX/TGA/SGI/XWD/XBM/PNM-family/QOI decode and encode, measured against the reference codec directly (no registry path exists yet)

Built three new crates — `vaco-codec-qoi`, `vaco-codec-pnm` (pbm/pgm/ppm/pam/pfm/phm),
`vaco-codec-image-simple` (bmp/pcx/tga/sgi/xwd/xbm) — the first real
`Decoder`/`Encoder` implementations in this tree (`vaco-codec-core` had only
`mock.rs` before this). Because no `CodecId`/`EncoderDesc`/CLI dispatch
exists yet for any of the fifteen formats (see `planning/TECH-DEBT.md`'s
C-13 entry), verification was done by calling the pure `decode`/`encode`
functions directly against `ffmpeg`-produced fixtures rather than through
`vaco-cli -c:v <codec>`, which cannot reach these decoders/encoders at all
today. Every fixture below was a real ffmpeg encode/decode of a `testsrc`/
`mandelbrot`/`life`/hand-built source, compared byte-for-byte.

| Codec | Encode byte-identical to reference | Decode pixel-identical to reference | Notes |
|---|---|---|---|
| QOI | yes (5 fixtures: gradient, noise, solid, pattern, RGBA) | yes | full spec implementation |
| PBM | yes | yes | |
| PGM (8-bit) | yes | yes | |
| PGM (16-bit) | yes | yes | reference is always big-endian regardless of source pix_fmt |
| PPM (8-bit) | yes | yes | |
| PPM (16-bit) | yes | yes | |
| PAM (GRAYSCALE) | yes | yes | |
| PAM (RGB_ALPHA) | yes | yes | |
| PAM (GRAYSCALE_ALPHA) | yes | yes | |
| PAM (BLACKANDWHITE) | yes | yes | decodes to `monoblack`, not a literal byte-per-sample copy — see below |
| PFM (gray + colour) | yes | yes | rows are bottom-to-top in the file; scale sign selects endianness |
| PHM (gray + colour) | yes | yes | same layout as PFM, half-float samples treated as opaque 16-bit lanes |
| BMP | yes (24bpp `bgr24`/32bpp `bgra`) | yes | 1/4/8bpp decode expands through the palette to `rgb24` (no round-trip; see TECH-DEBT) |
| PCX | yes (3-plane 8-bit RGB) | yes | single-plane paletted 8bpp not implemented |
| TGA | no reference encoder exists in this ffmpeg build | yes (hand-built fixtures, uncompressed both orientations) | encoder is spec-conformant, unverified against a reference |
| SGI | no (writes uncompressed; reference defaults to RLE) | yes (both RLE and verbatim) | RLE table index order (channel-major) and bottom-up row order confirmed by decoding the reference's own RLE output |
| XWD | no (omits the reference's embedded `lavcxwdenc` window-name string) | yes | that string is an ffmpeg implementation detail, not part of the XWD format |
| XBM | yes | yes | reference converts bit order by reversing each byte whole, carrying trailing padding bits through unchanged rather than zeroing them — matched by doing the same |

Three measurements worth recording independently of the table:

- **PGM/PPM `maxval` rescaling, when `maxval` is not 255/65535.** The
  reference does *not* pass samples through unchanged for other maxvals —
  it rescales to fill the declared output range. Non-tie roundings all
  matched ordinary round-to-nearest, but two probes at different maxvals
  (`maxval=100`, `maxval=4`) disagreed on which way an exact `.5` tie
  rounds (100's ties rounded down, 4's rounded up) — bisecting further did
  not resolve it before the batch had to move on. `vaco-codec-pnm`
  implements round-half-up and documents the discrepancy in
  `netpbm.rs`'s module doc rather than claiming a formula that was not
  actually confirmed at the boundary.
- **PAM's `BLACKANDWHITE` tuple type decodes to the bit-packed
  `monoblack` pixel format, not a literal byte-per-sample copy of its own
  on-disk layout** (which is one `0`/`1` byte per pixel, unlike PBM's
  bit-packing). Confirmed by building a `BLACKANDWHITE` PAM and a `P4` PBM
  from the same source image and finding their reference-decoded output
  byte-identical; `PixFmt::MonoBlack` vs `PixFmt::MonoWhite`'s opposite
  polarity accounts for the differing on-disk bit values representing the
  same image.
- **XBM's LSB-first bit order is a whole-byte reversal of the MSB-first
  `monowhite` layout, including whatever is in a row's trailing padding
  bits** — not a per-pixel remap that leaves padding at zero. A first
  implementation that masked padding to zero produced byte-identical pixel
  content but failed exact-byte comparison against the reference on every
  fixture with a non-multiple-of-8 width; reversing the whole byte fixed it
  and confirmed the padding bits are not "uninitialised", just carried
  through a symmetric transform.

Fuzz targets: `parse_qoi`, `parse_pnm`, `parse_image_simple` (30s each,
`exit=0`, `find fuzz/artifacts -type f` empty — see the closing report for
exact exec counts).

## 52. #647/#648/#649/#650/#651/#643: the container sweep's six leftover issues, one comparison loop

Six issues the format-sweep filed but could not fix in its own pass (MP4 was
excluded from its scope; the rest ran out of budget). All six measured
against `ffmpeg`/`ffprobe` 8.1 with the sweep's own loop — `-c copy` into the
target format, decode-MD5 on the result, `-show_streams`/`-show_format` diff
— five fixed, one partly fixed with the rest identified as belonging to a
crate outside this pass.

### #647 — MP4 `hvcC` never set `nal_length_size`

```text
                          before          after
raw hevc decode md5       Invalid data    matches source
mpegts decode md5         PPS id error    matches source
```

`vaco-format-isom`/`vaco-demux-mp4`'s `track::codec_parameters` never read
`hvcC`'s `lengthSizeMinusOne` (14496-15 §8.3.3.1, byte 21 low two bits, same
relative position `avcC`'s field occupies). `vaco-mux-raw` and
`vaco-mux-mpegts` already keyed their Annex-B conversion on this field for
both H.264 and HEVC — the fix is one field, `track::hvcc_length_size`, parsed
directly from the box bytes.

**New divergence found, not fixed here**: `vaco-parse-hevc` deliberately never
sets `nal_length_size` itself, because `vaco-probe` reads the same field,
unconditionally, to decide whether to print `is_avc`/`nal_length_size` — and
the reference never prints those for HEVC (confirmed: `ffmpeg -h
decoder=hevc` has no such private options). Populating the field in the
demuxer for HEVC now makes `vaco-probe` show them for HEVC where the
reference does not. Filed as #654 (a `vaco-probe` fix: gate on
`codec_id == H264` explicitly) rather than fixed in this pass, since
`vaco-probe` is outside the crates this package touched.

### #648 — ASF + H.264 does not survive `-c copy`

Two independent bugs, both in `vaco-mux-asf`:

1. No Annex-B conversion at all for length-prefixed H.264/HEVC — same bug
   class as the sweep's own raw-muxer fix, a third container. Fixed by
   mirroring `vaco-mux-mpegts`'s `maybe_convert`/`check_bitstream` pair
   exactly.
2. Even after (1), decoded video MD5 did not match: the "Presentation Time"
   field was written from `packet.pts`, which is not monotonic with a
   B-frame source. A real ASF reader requires monotonic Presentation Time and
   decoded a different picture into each slot when it was not — same
   access-unit count, different bytes throughout. Swapping to `packet.dts`
   (monotonic by construction) fixed it.

```text
                     before        after
decode md5 (video)   corrupt       matches source
decode md5 (full)    corrupt       matches source
```

### #649 — image2 pattern sequences: a documented, structural gap, not fixed

`vaco-demux-image2`/`vaco-mux-image2` already implement pattern/sequence
handling completely (`Image2Demuxer::open_pattern`, `Image2MuxWriter`) — this
is not an unimplemented feature. The gap is the registry seam:
`DemuxerDesc::open`/`MuxerDesc::open` receive one already-opened
`MediaSource`/`MediaSink`, with no filename to pattern-match against, so the
CLI has no path to `open_pattern` at all — both crates' own module docs
already say so (`docs/format/vaco-demux-image2.md`'s "the registry seam does
not fit this format", predating this issue). `planning/INTERFACE-GAPS.md`
gap 2 ("`Muxer` is single-sink") already tracks the write half. Reproduced
exactly as filed:

```text
$ vaco -f image2 -i "img_%03d.png" -c copy -f image2 "out_%03d.png"
[in#0] Error opening input: No such file or directory
```

Left open — the fix needs `vaco-io`/`vaco-format-core` (a `MediaSource::path()`
accessor, or a CLI-level special case for the `image2` format name), neither
of which this package's crates are.

### #650 — image2 (single file): several `-show_streams` fields wrong

Fixed, in both entry points (`multi::Image2Demuxer`'s literal-file path and
every `pipe::PipeDemuxer`):

```text
                    reference        before          after
r_frame_rate        25/1             0/0             25/1
avg_frame_rate      25/1             0/0             25/1
time_base           1/25             1/1000000       1/25
field_order         unknown          progressive     unknown
start_time          N/A              0.000000        N/A
duration            N/A              0.040000        N/A
bit_rate            N/A              1137200         N/A
```

Reading it as one cluster, per the issue's own framing: a still image (or a
`_pipe` splitter's whole concatenated run — measured on three PNGs through
`png_pipe`, which the reference *also* reports no timeline for) has no
timeline at all, and `VideoParameters::default()`'s `FieldOrder::Progressive`
was being read as a real answer rather than "not yet stated". `multi::
stream_video`/`time_base_for` state `frame_rate`/`field_order` explicitly and
compute the stream's time base from `-framerate` instead of the generic
`TIME_BASE_Q`; packets from a still-image path carry `Timestamp::NONE`/
`Duration::ZERO` instead of a synthetic `0`/`1 tick`, and `PipeDemuxer` lost
its `Demuxer::duration` override entirely (the default `None` was already
correct — the override was itself the bug, feeding a container-level
duration `adopt_container_timings` then handed to every stream).

**Not fixed, and not this crate's**: `sample_aspect_ratio`/`display_aspect_ratio`
(reference `1:1`) and `color_range`/`color_space` (reference `pc`/`gbr`, PNG
being RGB) come from whichever crate parses the image codec's own header —
`vaco-demux-image2` never touches pixel content. `probe_score` differs by one
point (`99` vs `100`) for a single-file `png_pipe` match specifically, not
run down.

### #651 — RSO over-restricted its accepted codecs, and had an unrelated byte-order bug

`RsoMuxer::add_stream` refused everything but `pcm_u8`, reading
`ffmpeg -h muxer=rso`'s *default*-codec line as an exhaustive list. Measured
via `-c copy` from WAV (little-endian formats) and AIFF (big-endian and
`pcm_s8`, which WAV cannot hold):

```text
accepted:  pcm_u8 pcm_s16le pcm_s24le pcm_s32le pcm_f32le pcm_f64le pcm_alaw pcm_mulaw
refused:   pcm_s8 pcm_s16be pcm_s24be pcm_s32be   (write_header fails on the real muxer too)
```

`rso::accepts` now matches. Same pass also disambiguated the offset-2 header
field as a **byte count**, not a sample count (1000 `pcm_s16le` samples, 2000
bytes, reads back `2000`) — the module doc's earlier claim was only ever
tested against `pcm_u8`, where the two coincide.

**Also found, unrelated to the codec check**: the offset-0 constant field was
written byte-swapped — `0x0100` (bytes `01 00`) instead of the reference's
`0x0001` (bytes `00 01`) — present since the crate's original implementation
and invisible to the existing round-trip test because the demuxer never
validates the field on read. Full-file byte comparison against the reference
(this issue's own verification loop) is what caught it; fixed in the same
commit.

```text
                     before   after
first two bytes      01 00    00 01   (matches reference)
```

**Measurement caveat, not root-caused**: `pcm_s24le` fed through a WAV source
succeeds against the real muxer; bit-identical `pcm_s24le` fed through the
raw `-f s24le` demuxer fails ("incorrect codec parameters?"), despite
`ffprobe` reporting identical `codec_name`/`sample_fmt`/`bits_per_sample`/
`channels`/`sample_rate` for both (only `codec_tag` differs, `0x0001` vs
`0x0000`). The accepted-set table above is built entirely from the
WAV/AIFF-sourced measurements, which agree with each other and with the
issue's own repro; see `TECH-DEBT.md` for the discrepancy, in case a future
re-measurement through the raw demuxer disagrees.

### #643 (remainder) — Ogg `duration`/`duration_ts` needed a real last-page granule scan

Three of the four items #643 reported were already fixed (bitstream serial
no longer published as `id`; `ogg_codec` no longer leaks as a `TAG`; the
Vorbis comment header is read, `TAG:encoder` present) — confirmed still true
in this pass. The fourth:

```text
                reference               before        after
duration_ts     48312 (issue's file)    N/A           44160 (this pass's fixture)
duration        1.006500                N/A           1.001361
```

Ogg has no length field anywhere. `OggDemuxer::scan_tail_for_durations` reads
a bounded 256 KiB window from near the end of a seekable source, finds each
logical stream's last page by its serial number, and states
`Stream::duration_ts` from that page's **raw** granule position —
un-adjusted by `GranuleMapping::timestamp`'s pre-roll subtraction, which is
for a packet's pts, not a summary duration (measured: a file whose last page
reads granule `44160` at 44100 Hz reports `duration_ts=44160`, matching
`44160/44100 = 1.001361 s` exactly, not the pre-roll-subtracted value).
Per-stream, not through `Demuxer::duration()`, so a multiplexed file's
streams keep independent answers. Does nothing on an unseekable source.

**New divergence found, not fixed**: a Vorbis stream's `start_pts`/
`start_time` report the granule mapping's negative initial cursor (measured:
`-1024`/`-0.023220s` on a 44.1 kHz fixture) where the reference reports `0`.
`vaco-demux-ogg` never sets `AudioParameters::initial_padding` for Vorbis the
way it already does for Opus's `pre_skip`, which is what
`vaco-format-core::discovery`'s existing "first_pts + initial_padding, not
first_pts" rule (found for Opus/Matroska) would need to normalise this the
same way — plausible fix, not attempted here, since it needs its own
measurement to confirm Vorbis's priming is the same shape as Opus's rather
than assumed by analogy.

Verification: `cargo test -p vaco-demux-mp4 -p vaco-mux-asf -p
vaco-demux-image2 -p vaco-format-audio-simple -p vaco-demux-ogg --locked`,
all green; `cargo clippy` on the same five, clean.
## 53. #642 and finding 50's four AVI/FLV leftovers: extradata reaches AVI stream info, H.264/HEVC framing mirrors the source, two grid gaps close, and byte-exact output on three of four fixtures

Four items finding 50 and issue #642 left open, all in the AVI mux/demux
pair, closed together because fixing #642's demux-side gap and finding 50's
mux-side framing question touch the same `strf` bytes from opposite
directions, and every fix after that surfaced through the same
byte-for-byte comparison loop.

### #642 (demux): `strf`'s `avcC` now reaches stream info

`vaco-demux-avi::hdrl::parse_strf`'s video branch read `BitmapInfoHeader`'s
fixed 40 bytes and stopped; the audio branch already treated trailing bytes
as extradata. Gave the video branch the same treatment, gated on the
`FourCC` being one that carries a configuration record
(`video_tags::carries_config_record`: `avc1`/`AVC1`/`hvc1`/`hev1`, not
`H264`/`X264`/`HEVC` and their Annex-B aliases, which have nothing after the
header to capture). No codec-specific parsing was added here — the captured
bytes flow through the same generic `vaco-format-core::discovery` pipeline
that already turns MP4's `avcC` into `profile`/`level`/`pix_fmt`/`is_avc`/
`nal_length_size`/`extradata_size`/`mime_codec_string`/`bits_per_raw_sample`.
All eighteen fields #642 named now match on the `avc1` MP4 fixture; the two
fields the issue named as *not* bugs (`r_frame_rate`, `nb_frames`) are
unchanged, as instructed.

### Mux: H.264/HEVC keeps whatever framing its source used

Finding 50 recorded that the reference stores H.264 in AVI length-prefixed
(`avc1`, `avcC`) where this crate's muxer wrote Annex-B (`H264`, no config
record) unconditionally. Measured directly this session, on **both**
directions at once (an `avc1`-tagged MP4 source and an Annex-B MPEG-TS
source): the reference does not convert framing in either direction. A
length-prefixed source stays length-prefixed (`avc1`/`hvc1`, its own
`avcC`/`hvcC` copied into `strf` verbatim); an Annex-B source stays Annex-B
(`H264`/`HEVC`, its own start-code-prefixed SPS/PPS extradata — when the
demuxer synthesized one — copied into `strf` the same way). Neither the
packet payload nor the extradata is ever reframed by this muxer.

This reverses finding 16's length-prefixed-to-Annex-B conversion entirely —
it never matched the reference, and existed because nobody had measured the
Annex-B case against the reference at the same time as the length-prefixed
one. The old conversion path (`maybe_convert`, `check_bitstream`'s M6
`h264_mp4toannexb` request, and the `vaco-format-nalu` dependency it needed)
is removed; `add_stream` now refuses a length-prefixed stream with no
extradata at all (`avc1`/`hvc1` structurally promises a configuration record
`strf` would otherwise not have), but the equivalent Annex-B case is not an
error, since `H264`/`HEVC` makes no such promise.

Two `tests/roundtrip.rs` cases replace the old
`a_length_prefixed_h264_sample_is_rewritten_to_annex_b` and
`check_bitstream_through_mux_writer_gets_the_splice_maybe_convert_alone_cannot`
(both asserted the now-removed conversion): one pins length-prefixed framing
staying length-prefixed with `avc1`/`avcC`, one pins Annex-B staying Annex-B
with plain `H264` and no config record, and a third confirms
`check_bitstream` never asks M6 for a filter (a `BsfProvider` that refuses
every filter name still succeeds).

### Mux: the video grid's tail, and a real audio decode bug found along the way

**Grid tail.** Comparing whole-file byte counts against the reference
surfaced a second bug, independent of framing: the video grid undercounted
every fixture's `dwLength` by exactly one frame-duration's worth of ticks
(`600/25 = 24` on both 25 fps fixtures tried) — real frames land on the
right slots, but nothing accounts for the *last* frame's own duration
extending past it, since nothing arrives afterward to trigger the ordinary
inter-frame backfill. `AviMuxer::backfill_trailing_video_slots`, called once
from `write_trailer`, extends the grid from the slot after the last real
frame to that frame's own duration later, using the packet's own `duration`
rescaled into grid ticks.

**Audio decode bug, found while chasing the above.** With the grid tail and
framing fixes landed, the one fixture with AAC audio still failed to decode
on the reference's own decoder (`Input buffer exhausted before END element
found`, `channel element 3.11 is not allocated`) even though the raw AAC
payload bytes matched the reference byte for byte. Root cause:
`write_strl`'s audio branch built `strf` from the fixed sixteen
`WAVEFORMATEX` fields alone and never wrote the `cbSize`-prefixed extension
— `add_stream` already *required* AAC to carry extradata (refusing
ADTS-framed streams with none), but nothing ever wrote the extradata it
demanded into the file. A decoder with no `AudioSpecificConfig` has no
object type or channel configuration and desyncs on the first frame it
decodes. Fixed by writing `StreamOut::audio_extradata` (already captured at
`add_stream`, never reaching `write_strl`) as `strf`'s trailing `cbSize` +
bytes. Verified: decoded video *and* audio both match the reference's own
AVI output exactly, per stream, on the AAC fixture (previously: hard decode
error).

### Mux: a leading audio gap, tied to video's B-frame depth, not audio's own timing

Finding 50 left "the reference writes a small number of zero-length audio
placeholder chunks this crate does not" as an unconfirmed candidate
mechanism. Measured properly this session with seven synthetic fixtures
(`ffmpeg -f lavfi` sources, `libx264 -bf 0` through `-bf 7`, each with an AAC
track whose own one-frame encoder priming never varies): the gap has
**nothing to do with audio's own sample rate or duration**. Two audio-only
fixtures (44.1 kHz stereo, 48 kHz mono, same priming) wrote zero placeholder
chunks. Holding the audio fixture fixed and varying only the video's
B-frame count reproduced the gap on demand: `has_b_frames` (`ffprobe`'s own
field, which the H.264 parser already derives from the SPS) of 0, 1 and 2
measured a leading gap of exactly 0, 1 and 3 chunks — `2^n - 1` at every
point tried, and `has_b_frames` itself capped at 2 for every `-bf` value
this build of `libx264` produced past that point, so the formula is
unconfirmed above `n = 2`.

The gap is also positioned precisely: it sits immediately in front of the
audio stream's *second* chunk, not immediately after its first — confirmed
by comparing each chunk's position against the surrounding video chunk
count between the two candidate placements, since both produce the same
total chunk count and only interleaving position tells them apart.
`AviMuxer::maybe_backfill_leading_audio_gap`, keyed on `stream.count == 1`
at the top of `write_packet` (i.e. right before that second chunk's own
bytes go out), reproduces the reference's interleaving position exactly.

### `hdrl`'s `JUNK` reservations: fixed sizes, and two of the three are inert index structures

Finding 50 left the three `JUNK` reservations unresolved from one fixture,
where the *within-strl* one measured the same size (4120 bytes) across two
very different `strf` contents — enough to rule out "pad relative to strf
size" but not enough to say what the rule was. Measured across all four
fixtures available this session (`strf` sizes 16, 24, 78, 86 bytes; one or
two streams; PCM, H.264 in both framings, AAC): all three reservations are
**fixed constants** — 4120 bytes after every `strl`'s own content, 260
bytes after the last `strl` (inside `hdrl`), 1016 bytes at the top RIFF
level between `hdrl` and `movi`.

Chasing full byte-exactness (not just sizes) further found that two of the
three are not simply zero: the per-`strl` one is an inert `AVISUPERINDEX`
header (`wLongsPerEntry = 4`, `nEntriesInUse = 0`, this stream's own
`dwChunkId`) that the reference reserves room for but never activates,
tagging it `JUNK` instead of `indx` — confirmed on four separate stream
instances across the fixtures. The `hdrl`-level one is `LIST 'odml'`
holding one `dmlh` (`AVIEXTHEADER`) chunk, `dwGrandFrames` and everything
else left `0` regardless of the file's real frame count. The RIFF-level one
measured genuinely all zero. Also found in the same pass: the per-`strl`
`JUNK` sits between `strf` and `vprp`, not after `vprp` — this crate had
the order backwards on the first attempt, caught immediately by the same
byte comparison.

### Several more `avih`/`strh`/`strf` fields, found while chasing byte-exact output

- `avih.dwFlags` was missing `AVIF_TRUSTCKTYPE` (`0x800`).
- `avih.dwSuggestedBufferSize` is a fixed `1_048_576` (1 MiB) on every
  fixture.
- `avih.dwMaxBytesPerSec` is the sum of every stream's own
  `CodecParameters::bit_rate` (bits/sec, truncated to bytes/sec) — `0` when
  nothing declared a rate.
- `avih.dwTotalFrames` is the *video* stream's own count specifically —
  measured on a PCM-only (no video) fixture, where it stays `0` rather than
  falling back to the audio stream's own sample count.
- `strh.dwSuggestedBufferSize` (the per-stream field, distinct from
  `avih`'s) is the largest single chunk that stream actually wrote —
  confirmed on both video and audio (five independent exact values across
  the four fixtures: 1516, 1559, 8192, 1340, 265).
- `strh.rcFrame` is `{0, 0, width, height}` for video, not all zero;
  `{0, 0, 0, 0}` for audio.
- An audio stream's `strh.fccHandler` is the raw `u32` value `1`
  (`WAVE_FORMAT_PCM`'s own tag number) regardless of the stream's actual
  `wFormatTag` — an AAC-tagged stream measured the same `1` a PCM stream
  did.
- `strf.biSizeImage` (video) is `width * height * 3` — the raw-RGB byte
  count `biBitCount = 24` implies — even though the codec is compressed.
  Confirmed identical on `avc1` and Annex-B `H264` alike.
- A compressed (VBR) audio stream's `strh.dwScale/dwRate` is one *frame's*
  duration, not one sample's: AAC at 44100 Hz reduces to `256/11025`
  (`1024/44100`, AAC-LC's fixed frame size). This is deliberately a
  *different* field from `Muxer::stream_time_base` — the first attempt set
  both from the same value and broke real packet interleaving order between
  audio and video, caught only by comparing muxed bytes against the
  reference (this crate's own tests never touch audio timestamps, so they
  did not catch it). `StreamOut::strh_time_base` now carries the `strh`
  value separately; `StreamOut::sample_rate` carries `strf`'s own
  `nSamplesPerSec`, which stays the true rate throughout.
- A compressed stream's `strf.nAvgBytesPerSec` is the same `bit_rate / 8`
  `avih.dwMaxBytesPerSec` uses, applied per stream.

**Not resolved:** `strf.nBlockAlign` for a compressed stream. The one AAC
fixture measured `3`, matching none of `bytes_per_sample × channels`
(correct for CBR PCM), the sample rate, the bit rate, or the channel count
in any combination tried. No second compressed-audio fixture was available
to isolate the rule; see `write_strl`'s comment and `planning/TECH-DEBT.md`.

### Verification

`-c copy -fflags +bitexact`, `ffmpeg 8.1`, four fixtures (an `avc1` MP4 with
H.264+AAC, a video-only `avc1` MP4, an Annex-B MPEG-TS with B-frames and a
non-zero start time, and a PCM-only WAV):

```text
avi (H.264+AAC MP4)   39304 bytes, byte-identical except strf.nBlockAlign (2 of 39304 bytes)
avi (video-only MP4)  98430 bytes, byte-identical
avi (MPEG-TS)         99360 bytes, byte-identical
avi (WAV)             94092 bytes, byte-identical
```

`cmp` confirms byte-identity on three of the four; on the fourth, exactly 2
bytes differ across the whole file (the unresolved `nBlockAlign`). Decoded
video and audio both match the reference's own AVI output exactly, per
stream, on every fixture with audio, including that one — `ffmpeg -f null -`
and `-f md5 -` on our own output report no decode errors on any fixture.

Tests: `vaco-mux-avi/tests/roundtrip.rs` gained
`a_length_prefixed_h264_sample_keeps_its_framing_and_gets_avc1_avcc`,
`an_annex_b_h264_sample_keeps_h264_and_gets_no_config_record`,
`check_bitstream_never_requests_a_filter_through_mux_writer`,
`avih_flags_suggested_buffer_and_junk_reservations_match_the_measured_constants`,
`strh_suggested_buffer_is_the_largest_chunk_and_avih_sums_bit_rates`,
`audio_fcc_handler_is_the_fixed_value_one_not_the_format_tag`,
`video_rcframe_and_bisizeimage_are_not_left_zero`,
`compressed_audio_strh_time_base_is_one_frame_not_one_sample`, and
`compressed_audio_avg_bytes_per_sec_comes_from_bit_rate` — replacing the two
tests that pinned the removed Annex-B conversion.
`vaco-demux-avi/src/hdrl.rs` gained three unit tests for the `avcC` capture.
`fuzz/fuzz_targets/avi_mux_packet.rs`'s output-growth bound needed
recomputing once the `JUNK` reservations existed (its old `header_budget`
predated them and undercounted by more than 1 KiB); the fuzzer found this
in under a second as a false-positive assertion failure on a 3-byte input,
moved to `fuzz/seeds/avi_mux_packet/` as a regression seed. 30s run after
the fix: `exit=0`, `execs≈N`, `find fuzz/artifacts -type f` empty.

## 54. #362/#363/#364: MPEG-1/2/2.5 Layer I/II/III decode — Layer II verified against `ffmpeg`, Layer III measurably not there yet

`vaco-codec-mpegaudio` implements all three layers behind one
`MpegAudioDecoder` dispatching on the frame header's own `layer` field.
No CLI path exists yet to select a decoder by name (issue #652, being
fixed separately), so verification called the decoder directly against
packets from `vaco-demux-mpegaudio` (issue #644, already landed) and
compared the resulting PCM to `ffmpeg -f s16le -`.

### Layer II: real bugs found and fixed, then verified to a close match

Two bugs were found purely by this comparison, both in the per-subband
sample-decoding loop's structure, not in any formula:

1. The output index advanced by 1 per ungrouped sample read instead of 3,
   so only 384 of each frame's 1152 samples per channel were ever decoded
   (silently a third of the frame) and a third of the bits the frame
   needed were never read.
2. Once (1) was fixed, real-file correlation against `ffmpeg` was still
   only ~0.25 (up from ~0.04, effectively noise). ISO/IEC 11172-3's own
   pseudocode for this step is granule-major:
   `for (gr=0; gr<12; gr++) for (sb...) for (ch...) { ... }` — one sample
   or one grouped codeword per allocated subband for granule 0, then the
   same for granule 1. This crate had it nested the other way (subband
   outside granule), which reads the *right total number of bits* — so a
   frame still ends in the right byte and the demuxer's own framing was
   never in question — but from the *wrong positions* past the first
   allocated subband. Fixing the nesting order took correlation to
   **1.0000** at zero sample shift.

Measured across 32000/44100/48000 Hz × mono/stereo (6 fixtures,
`ffmpeg -f lavfi -i "sine=..." -c:a mp2 ...`): RMS error 1.2-10.7 of a
32767 full-scale `i16`, cross-correlation 1.0000. Not bit-exact (`f32`
decode, not the ISO reference's fixed-point contract), but the remaining
error is plausibly rounding rather than a structural mistake.

### Layer III: two real bugs fixed, but real-file decode is still wrong

- The global-gain requantisation constant: ISO/IEC 11172-3's own text names
  it ("The constant 64 in this formula...") but the actual formula is an
  image this crate's PDF-to-text extraction lost, leaving only that one
  sentence. Implementing literally with `64` produced samples ~10⁷ too
  large. `210` — confirmed empirically against `ffmpeg`, not by citation —
  produces sane magnitudes.
- A silent granule (`big_values == 0`, `part2_3_length == 0`, e.g. the
  "side" channel of an MS-stereo-encoded mono source) was still being fed
  into the Huffman-decode loop, which read real bits belonging to whatever
  came next in the bit reservoir and manufactured spectral energy that was
  never transmitted. Fixed by bounding both Huffman loops with
  `r.bit_pos() < granule_end_bit`, since `part2_3_length` is the only
  authoritative bound — "576 lines decoded" alone is not enough.

Despite both fixes, full end-to-end decode of a real encoded file is still
measurably wrong: a 440 Hz test tone reaches only ~0.44 sample correlation
against `ffmpeg`'s decode after finding the best time alignment, and a
6000 Hz tone comes out at a measurably wrong output frequency (~4316 Hz
instead of 6000 Hz — not noise, a clean wrong frequency). A dedicated unit
test (`layer3::frequency_placement_tests`) rules out half the pipeline
conclusively: it excites one known spectral line with no bitstream
involved at all, runs it through the exact subband-split → IMDCT →
windowing → overlap-add → synthesis-filterbank code `decode` itself calls,
and confirms the output lands at that line's correct frequency. So the
remaining bug is narrowed to the side-information/Huffman-decode half, but
not found — reported as broken, not glossed over as "close."

Layer I has no available encoder on this machine (`ffmpeg`'s build here
has no MP1 encoder, and no other tool was found either) — covered only by
unit tests and by the same, Layer-II-verified `Synthesis::synth_block`.

### Verification

`cargo run -p vaco-codec-mpegaudio --example decode_dump -- <file>` dumps
interleaved `s16le` PCM by driving the demuxer and decoder directly;
diffed against `ffmpeg -i <file> -f s16le -acodec pcm_s16le -` with a
best-alignment search over sample shift (scratch Python, not committed) to
report cross-correlation, max absolute sample difference and RMS. Fixtures
generated via `ffmpeg -f lavfi -i "sine=..." -c:a libmp3lame/mp2 ...`.

### Scope not attempted

Short blocks (`block_type == 2`) decode to silence rather than their real
audio; intensity stereo is unimplemented for all three layers; MPEG-2/2.5
(low sample rate) Layer III returns `Unsupported`; the demuxer's
`SkipSamples` gapless side data is not consulted by this decoder. See
`planning/TECH-DEBT.md` and `docs/codec/vaco-codec-mpegaudio.md`'s "Known
gaps" for each. Encoders (#365/#366) are explicitly out of scope; epic #38
stays open.

## 55. The XF-04 differential prober: `ts_id`/`ts_packetsize` typed wrong, MPEG-TS audio duration off by ~8%, and four mutation-found demux divergences

New tool, `fuzz/src/bin/diff_probe.rs` (`just diff-fuzz <family>`): mutates real
media generated locally with `ffmpeg` and runs both `vaco-probe` and `ffprobe`
with `-v quiet -of flat -show_format -show_streams`, comparing the flat
`key=value` output field by field. "Agreement" is acceptance (exit 0 vs not)
first, content second — never error text. A crash or hang on our side is the
highest-priority class; none occurred in ~4,200 probe pairs across four
families (mp4, matroska, mpegts, wav) at this pass's mutation intensity, which
is a negative result, not a guarantee. Measured throughput: ~55 execs/s
(pairs/s) with both a debug and a release `vaco-probe` — process-spawn
overhead dominates on inputs this small, not decode cost, so the release build
bought nothing here.

### `ts_id`/`ts_packetsize` are the reference's *strings*, not its integers

Reproduces with no mutation at all: `ffprobe -of flat -show_streams` on any
MPEG-TS file prints `streams.stream.0.ts_id="1"` and
`ts_packetsize="188"` — quoted. `vaco-probe` prints them unquoted
(`ts_id=1`), and the same distinction shows up in `-of json` as `"ts_id": "1"`
(reference) versus `"ts_id": 1` (ours). `crates/app/vaco-probe/src/fields.rs`
declares both `f("ts_id", Int, Omit)` and `f("ts_packetsize", Int, Omit)`
(lines 235-236) — the value looks numeric, but finding #46 already established
that these two were retrofitted through `Stream::metadata`, a string channel,
and the reference apparently treats them as its own `AV_OPT_TYPE_STRING`-typed
fields regardless of the digits in them. This is the same trap
`AGENT-CONSTRAINTS.md` names for `codec_tag`'s minimum width: the reference's
own vocabulary ("looks like a number") is not evidence about its declared
type. Not fixed here (fuzz/xtask only, per scope) — the field type in
`fields.rs` needs to change from `Int` to whatever this crate's `Str`-shaped
constructor is, for both fields.

### MPEG-TS audio stream: `duration_ts`/`duration`/`bit_rate` diverge on a well-formed file

Also reproduces with no mutation: a 0.5s H.264+AAC `.ts` file (`fuzz/seeds/diff/mpegts/h264-aac.ts`,
generated by this pass) gets, for the AAC stream:

```
            duration_ts   duration    bit_rate
ours        27168         0.301867    N/A
reference   29257         0.325078    73310
```

~7.7% short on duration and no bit_rate stated at all, on a file with nothing
wrong with it. Not diagnosed further here (scope is fuzz/xtask, not
vaco-demux-mpegts) — worth a differential pass by whoever owns that crate,
since every mutant that doesn't damage the file enough to lose the audio
stream entirely still carries this same gap forward into the mutation
campaign's tally.

### Four new mutation-found divergences (`fuzz/seeds/diff/findings/`)

- **`mp4/dropped-track-15e23e3e.bin`**: a corrupted `moov` still parses as one
  track to `ffprobe` — reported as `codec_type=data`, `codec_name=unknown`,
  a real if useless stream — while `vaco-probe` reports `nb_streams=0`.
  Same shape as `AGENT-CONSTRAINTS.md`'s "detection and demuxing ask
  different questions": ours drops a track it cannot classify instead of
  surfacing it degraded.
- **`mp4/frame-rate-sentinel-173e4e65.bin`**: `r_frame_rate` is `"16/1"`
  (ours, computed) vs `"1/0"` (reference's sentinel for "could not
  determine"). One field, minimal reproducer.
- **`matroska/duplicate-track-01174c21.bin`**: mutation duplicates a byte
  range landing on a `TrackEntry`-shaped span; `ffprobe` ends up with two
  streams (`nb_streams=2`, the second missing an `id`), `vaco-probe` keeps
  one (`nb_streams=1`). Stream 0 also independently loses `is_avc`,
  `mime_codec_string`, `level` and `pix_fmt` on our side, so this file
  exercises two divergences at once.
- **`matroska/avc-fields-drop-07475b65.bin`**: minimal three-field case —
  `is_avc`, `mime_codec_string`, `nal_length_size` all go from stated to
  absent under a mutation `ffprobe` tolerates without losing them.

Every `.bin` replays with `diff_probe replay <file> --vaco-probe
target/release/vaco-probe` per `fuzz/seeds/README.md`.

### Confirms two already-known, deliberately-open findings (#44)

The `wav/` family's mutants overwhelmingly reproduce #44's `channel_layout`
(`mono` vs `unknown`) and `start_pts`/`start_time` (`0`/`"0.000000"` vs
`"N/A"`) divergences, unchanged and still open — expected, not new. One
adjacent field not mentioned in #44: `format.tags.encoder` is stated by the
reference (`"Lavf62.12.100"`, the muxer's own signature) and absent from
`vaco-probe`'s output entirely, for both WAV files in this pass's corpus.

### Correction: finding 55's divergences, fixed, accepted or diagnosed

Fixed, each verified by `diff_probe replay` no longer reporting the field,
then a fresh ~1500-iteration-per-family campaign (mp4, matroska, mpegts, wav;
no crash or hang in any of the four, a larger run than the one that found
these):

- **`ts_id`/`ts_packetsize` typed `Str`, not `Int`**
  (`crates/app/vaco-probe/src/fields.rs`) — both are digits but the reference
  quotes them in `-of flat`/`-of json` regardless. Also filed as a follow-up
  comment on issue #635, which added the two fields with the wrong type.
- **`vaco-format-riff` gained a `LIST`/`INFO` reader**
  (`crates/format/vaco-format-riff/src/info.rs`), and `vaco-format-audio-simple`'s
  `WavDemuxer` now reads its `ISFT` sub-chunk into an `encoder` tag it used to
  skip entirely, closing `format.tags.encoder`.
- **`vaco-demux-mp4` no longer drops a track whose `hdlr` handler is
  unrecognized.** `codec_parameters`'s own codec-id fallback (already there,
  previously shadowed by resolving `media_type` before it could run) now
  classifies the track from its `stsd` sample entry when the handler cannot;
  only a track with *neither* signal is salvaged as `Data` rather than
  dropped. On the file that found this, the stream now appears and is
  correctly typed `video` — a residual gap remains (width/height/extradata
  still absent), traced to the same corruption also touching the `avcC`
  extradata itself; that is a `vaco-format-isom` parsing-robustness question,
  not this fix's mechanism, and is left for whoever next touches that parser.

Diagnosed precisely but **not fixed here**, each because the actual fix
reaches outside this pass's five cleared crates (`vaco-probe`,
`vaco-demux-mp4`, `vaco-demux-matroska`, `vaco-demux-mpegts`,
`vaco-format-riff`) into a shared module with a much wider blast radius than
one finding justifies changing on the strength of a single fuzzed input:

- **MP4 `r_frame_rate` computing a real number where the reference falls back
  to its `1/0` sentinel.** Traced to `vaco-format-core::discovery`'s generic,
  cross-format packet-based frame-rate refinement, which runs when a
  container's own declared rate is undefined and estimates one from observed
  packet deltas — here, from only four samples with a corrupted `stts` delta.
  The reference's own analogous refinement (`avformat_find_stream_info`)
  apparently declines to guess under the same corruption; matching that
  exactly would mean changing a heuristic every format in the tree shares,
  not an MP4-specific fix.
- **Matroska: `is_avc`/`mime_codec_string`/`nal_length_size` disappear
  under a truncated `CodecPrivate`.** Traced to `vaco-parse-h264`'s
  `refresh_parameters` (`crates/codec/vaco-parse-h264/src/parser.rs`), which
  gates populating `nal_length_size` behind having an *active SPS* — but
  `nal_length_size` comes from the AVC configuration record's own header
  byte, not the SPS, so a truncation that only reaches the SPS still loses a
  value that did not need it. Shared by every container that carries H.264
  through this parser, not Matroska-specific. Adjacent to open issue #654,
  which touches the same `is_avc`/`nal_length_size` display logic from the
  HEVC side (that one is about the fields showing when they should not;
  this one is about them being absent when they should not be) — worth
  reading together before either is changed, since both reach the same seam.
- **MPEG-TS: audio `duration_ts`/`duration` short by one AAC frame
  (`bit_rate` unstated), reproducing on a well-formed file, no mutation
  needed.** Already documented in `crates/format/vaco-demux-mpegts/src/demux.rs`'s
  `end_pts` and in `docs/format/vaco-demux-mpegts.md` (measured there as
  23.211 ms on every AAC fixture) as a deliberate gap: video's smallest
  inter-packet delta reproduces the reference's "last packet's own duration"
  exactly because one PES is one access unit, but audio has no equivalent
  signal available to a demuxer with no parser, and the docs already name
  the prerequisite (`vaco-codec-core::AudioParameters` needs a `frame_size`
  field, then `ParserProvider` can supply it). This is the largest single
  divergence class in the campaign — 913/1500 mpegts mismatches carried
  exactly this signature — but it was already known before this pass, not
  newly found by it.

Left as an accepted divergence, deliberately: **a mutation that duplicates a
byte range landing on a `TrackEntry`-shaped span gives the reference two
streams and us one.** A duplicated track is malformed input; creating a
second, phantom stream from it is not obviously the safer behaviour for
attacker-controlled media, and reproducing it would mean teaching
`vaco-demux-matroska` to trust a duplicate element rather than the one
already-clean pass at parsing it took. Not implemented; recorded here as the
classification for this class rather than left to be rediscovered.

Surfaced by the verification campaign, not investigated (out of this pass's
scope — fixing what was already found, not finding more): MP4's
`side_data_list`/`displaymatrix` rotation fields disappearing under some
mutations, and `format.tags.creation_time` divergences in both mp4 and
matroska.

### Dispatch 4: `AudioParameters::frame_size` closed via `CodecId::fixed_frame_size`; `r_frame_rate` sentinel root cause relocated, not fixed

Two items carried forward from the "Diagnosed precisely but not fixed here"
section above.

**1. MPEG-TS audio `duration_ts`/`duration` short by one AAC frame — fixed.**
Closed in `crates/format/vaco-demux-mpegts/src/demux.rs` and
`crates/signal/vaco-codec-core/src/lib.rs`; full detail (including the
two-step truncating rescale needed to match the reference's own 2089-tick
answer, not the 2090 a single-step rescale gives) is in
`docs/format/vaco-demux-mpegts.md`'s "Audio tail, closed" section, updated
alongside this fix. In short: this section's own literal proposal — a
`frame_size` field on `AudioParameters`, reached through `ParserProvider` —
does not survive contact with the tree. Every `AudioParameters` construction
is a full struct literal with no `..Default::default()`, so the field breaks
compilation in thirteen unrelated crates (measured by trying it, then
reverted). The fix used instead is `CodecId::fixed_frame_size() -> Option<u32>`,
a new `const fn` beside the existing `ticks_per_frame` — zero blast radius,
since nothing that already builds a `CodecParameters` literal needed to
change. States 1024/1152/1536 for AAC/MP3/AC-3-E-AC-3 respectively; `None`
for everything else including `AacLatm`, never guessed.

Campaign effect, `fuzz/seeds/diff/mpegts`, 1500 iterations, `--rng-seed 42`,
identical corpus before and after (a pristine `git worktree add --detach HEAD`
build vs. the fix): field-level `duration_ts` mismatches 1162 → 216,
`duration` mismatches 1631 → 675. The file-level tally did **not** move
(`agree=40`/`mismatch=1435` both runs) — most mutated cases in this corpus
carry more than one divergent field simultaneously, so closing one class
rarely flips a whole file's verdict. `bit_rate` mismatches were unchanged
(1531 both times): this section's own framing bundled `duration_ts`/`bit_rate`
as one divergence, but they are empirically independent — `bit_rate` remains
open and is not investigated further here. Searched GitHub issues for an
existing tracker (`ts_id`/`duration`/`bit_rate`/`AudioParameters` searches);
found none specific to this gap, so nothing to close.

**2. MP4 `r_frame_rate` computing `16/1` where the reference states its `1/0`
sentinel — investigated further, root cause relocated, no fix landed.**
This section named `vaco-format-core::discovery`'s packet-mean frame-rate
estimate as the mechanism. That is not what produces this fixture's mismatch:
confirmed by gating that estimate's `r_frame_rate` fill behind "the container
already stated `avg_frame_rate`" (which is true here — the estimate's own
existing doc comment already asserts this container-fills-both invariant, and
the old code violated it for `r_frame_rate` alone) and rebuilding — the
corrupted-`stts` fixture still printed `r_frame_rate=16/1`, unchanged.

The actual source is `crates/app/vaco-probe/src/show.rs`'s `frame_rate()`
display function: it falls back to the *codec-parsed* (VUI-derived, undivided)
rate whenever `Stream::r_frame_rate` is undefined, with no way to tell "this
format never had a mechanism to state one" (a raw H.264 elementary stream,
where the fallback is measured-correct: a fresh `ffmpeg`-encoded raw `.h264`
gives `r_frame_rate=32/1` on both sides) apart from "this format's own
mechanism ran and explicitly declined" (the corrupted-MP4 case: MP4's own
`frame_rate_estimate` in `vaco-demux-mp4` already leaves `r_frame_rate`
undefined here deliberately, while stating `avg_frame_rate=8/1` from the
container's total duration — matching the reference on both fields — and the
VUI's own valid, parseable 16/1 picture rate is what `frame_rate()` then
wrongly surfaces). Both cases produce the identical internal state
(`r_frame_rate` undefined, a defined `video.frame_rate`), so the display
function cannot distinguish them from the field values alone.

The discovery.rs gate change was kept only long enough to disprove the
brief's hypothesis, then reverted in full (`git diff --stat` against HEAD is
empty for `vaco-format-core`) — it does not fix the cited mismatch, and it
regresses ordinary MPEG-TS video: on `fuzz/seeds/diff/mpegts/h264-aac.ts`,
which the reference and both discovery.rs states agree on today
(`r_frame_rate=8/1`), the gated version prints `16/1` instead, because MPEG-TS
video's `avg_frame_rate` also arrives already-stated (from the same
picture-rate refine pass, ahead of the mean-delta estimate in packet order)
and the change silently handed the same show.rs display bug a second format
to misfire on. Caught only by rebuilding a pristine `HEAD` binary
(`git worktree add --detach`) and diffing the same fixture against the
changed one before running the campaign — the campaign's own aggregate
mismatch count for mpegts did not move either way (`agree=40`/`mismatch=1435`
throughout, per the same "compounding divergences" effect as item 1), so the
regression was invisible to the file-level tally and would have shipped
undetected without the direct fixture check.

No safe fix for item 2 was found within this dispatch's cleared crates. A
correct fix needs the display layer (or `Stream` itself) to distinguish
"never had a native rate mechanism" from "had one and it declined" — a new
per-stream signal that is itself the kind of wrapper-forwarding question this
dispatch's own brief warned about (`Box<dyn Muxer>`, `MappedFilter`,
`AsDecoder`), and plumbing it through every demuxer that currently relies on
`show.rs`'s blanket fallback (at least the raw-elementary-stream formats) is
wider than this finding's blast radius justifies on the strength of one
fixture. Left exactly where the "Diagnosed precisely but not fixed here"
section leaves its other items: measured precisely, not fixed, for whoever
next has room to touch `vaco-probe::show` and every format that depends on
its current fallback behaviour. No GitHub issue found tracking this specific
sentinel gap either.

### Dispatch 5: `bit_rate` classified into four causes (1531 mpegts mismatches); none fixed

Per dispatch 4's own residual: `bit_rate` mismatches (1531, unchanged by the
duration fix) were the largest remaining class with no tracking issue.
Classified by grepping the full campaign detail output
(`fuzz/seeds/diff/mpegts`, 1500 iterations, `--rng-seed 42`), not just the
aggregate count:

- **1114 — `streams.stream.1.bit_rate: ours="N/A" reference="<value>"`.**
  The audio (AAC) stream never gets a stream-level `bit_rate` on our side, on
  *every* file including the unmutated seed itself (confirmed directly:
  `fuzz/seeds/diff/mpegts/h264-aac.ts`, no mutation, reference states
  `bit_rate=73310` for the AAC stream, ours states `N/A` — this was already
  true before any mutation, not a mutation-revealed gap). `vaco-demux-mpegts`
  never sets `CodecParameters::bit_rate` for any stream, and no code in
  `vaco-format-core::discovery` estimates one — that module's only per-stream
  estimate is the video-only frame-rate one. Checked whether this is generic:
  it is not. MP4 states an explicit per-stream `bit_rate` in its own `esds`
  box for both video and audio, read directly by both sides and matching
  exactly (`fuzz/seeds/diff/mp4/h264-aac.mp4`: 21568/72367 on both sides) —
  MP4 needs no estimate at all. MPEG-TS carries no equivalent per-ES
  descriptor in these fixtures, so the reference is estimating it, and video
  on MPEG-TS gets no such estimate either (`bit_rate=N/A` on both sides,
  reference included, across six controlled clean fixtures from 0.5s to
  10s) — this is audio/parseable-codec-specific, not a blanket per-stream
  average the reference applies to every codec.

  Attempted to pin the exact formula by reconstructing it from
  `-show_packets` byte sums against the declared stream duration on two
  controlled fixtures (the seed file and a locally-generated 1s CBR AAC-in-TS
  file). The magnitude confirms a bytes-over-duration-style average — sums
  over the packets falling inside the stream's own `duration`/`duration_ts`
  window land within roughly 1% of the reported value on both fixtures — but
  neither a raw byte sum, an ADTS-header-stripped sum, nor a sample-count-based
  duration reproduces the reported value exactly on both. Not fixed: an
  approximate formula would replace a highly visible `N/A` with a plausible
  but subtly wrong number, which is the same trade dispatch 4's `r_frame_rate`
  attempt already showed is a worse outcome than leaving the gap alone.

- **333 + 82 (415 total) — `format.bit_rate` mismatches are not a `bit_rate`
  bug.** Sampled every case in this shape and found `format.duration` (or,
  under heavier corruption, the whole detected format identity —
  `format.format_name`/`format_long_name`) differing in the same case every
  time. Confirmed the arithmetic itself is exact: on three clean, unmutated
  fixtures (1s/3s/10s CBR MPEG-TS), `format.bit_rate` (`size * 8 / duration`,
  truncated — the existing formula in `crates/app/vaco-probe/src/show.rs`)
  matches the reference bit for bit once `duration` matches. Every mismatch
  in this shape is downstream of a pre-existing container-duration or
  format-detection divergence under mutation, not a `bit_rate`-specific
  cause — the same "second symptom of one underlying gap" shape dispatch 4
  asked to watch for, just a different underlying gap (container duration
  under corruption, not the `r_frame_rate` declined-vs-absent one). Not
  investigated further — that gap is a `format.duration`/probe-robustness
  question, outside this dispatch's `bit_rate` scope.

- **2 — `streams.stream.0.bit_rate: ours=<absent> reference="N/A"`.** Noise
  level, both heavily-corrupted-video-stream cases. Not classified further.

**Does `bit_rate` share `r_frame_rate`'s declined-vs-absent shape?** No.
Every Class-A instance observed had the reference stating a real number and
ours stating `N/A` — no case was found where the reference itself declined
with a sentinel the way it does for `r_frame_rate`'s `1/0`. This is a plain
missing capability (an estimate the reference has and we do not), not a
"can't tell two kinds of absence apart" problem, so it is a separate gap from
`planning/INTERFACE-GAPS.md` gap 23, not a second instance of it.

No GitHub issue found tracking the `bit_rate` gap or the `format.duration`/
probe-robustness gap it turns out to mostly be (searched "bit_rate mpegts",
"mpegts audio bitrate", "AAC ADTS bitrate estimate", "format.duration mpegts
corrupted"); nothing to close.

No code changed in `vaco-demux-mpegts`, `vaco-probe` or `vaco-format-core`
this dispatch — investigation only. A workspace-wide `cargo check` was
blocked for part of this session by an unrelated, transient concurrent
change (a new crate directory, `crates/filter/vaco-filter-stack`, mid-created
without its `Cargo.toml` by another agent); not investigated further as it
is not this dispatch's crate.
