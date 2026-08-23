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

## 14. `vaco-mux-mp4`/MOV: no `avc1` compatible-brand entry, no placeholder atom before `mdat`

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

## 15. `vaco-mux-matroska`: no `SeekHead` — open, but recorded as intentional in-crate

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

## 16. `vaco-mux-avi`: no length-prefixed-to-Annex-B bitstream conversion at all

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

## 17. `vaco-mux-mpegts`: SDT service name/provider default to empty strings

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

## 18. `vaco-mux-flv`: `onMetaData` carries 3 of the reference's ~10 properties

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

## 19. Six of ten known-incompatible remux pairs **succeed** on `vaco` where the reference refuses — open, cross-cutting

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

## 20. `vaco-mux-mp4`: self-remuxed MP4 reports a ~1600× wrong duration

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

## 21. H.264 `profile` is reported inconsistently across probe paths

Three different behaviours, same field:

- Probing a *reference-built* ASF/FLV file: `profile=unknown` (`vaco`'s
  H.264 parser or its ASF/FLV integration never extracts `profile_idc`).
- Probing `vaco`'s own MP4 remux of the same stream: `profile=High` (a
  decoded *name*, not the numeric `profile_idc` the reference prints —
  `profile=100`).
- The reference: always the numeric `profile_idc` (`100`, `244`, …) for
  every container.

Two separate small gaps sharing one field: the ASF/FLV path never fills the
value in at all, and the MP4 path fills it in with the wrong *kind* of value
(a name where a number is expected). Owning crate depends on where the field
is assembled for each container — `vaco-parse-h264` for the SPS read, or
each demuxer's stream-info translation for the ASF/FLV gap specifically.

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

## 24. Per-demuxer codec-ID mapping gaps — the same shape as finding 4, four more families

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

## 25. `vaco-probe`'s JSON/writer output emits `"profile": "unknown"` where the reference omits the key entirely

Seen on FLAC and DV (finding 24): the reference's JSON writer does not emit a
`profile` key at all for a codec with no meaningful profile concept;
`vaco`'s always does, with the value `"unknown"`. A field-presence
difference, not a field-value one — worth distinguishing because "the field
is wrong" and "the field shouldn't be there" have different fixes. Belongs
to `vaco-probe`'s writers, not the demuxers that were the direct cause of
findings 4/24.

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
