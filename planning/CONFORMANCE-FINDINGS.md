# What the differential harness found on its first run

`cargo run -p vaco-conformance -- run --tier core` over the three probe suites
in `tests/conformance/probe/`: **198 cases, 42 agreeing** on the first run,
**71 after one fix**. These are the findings, ranked by how much they cost.

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

## 7. MP4's codec table, and why a `FourCc` table cannot work — **open**, `vaco-format-isom`

The MP4 half of finding 4. `sample_entry_codec` in
`crates/format/vaco-format-isom/src/stsd.rs` maps about fifteen FourCCs and
collapses nine of them onto a single `CodecId::Pcm`, so a QuickTime file with
16-bit little-endian audio prints `codec_name=pcm` where the reference prints
`pcm_s16le`.

Filling the table in is not enough, and this is the interesting part. Measured
2026-08-23 by encoding one `.mov` per PCM variant and reading back both the
sample-entry FourCC and what `ffprobe` calls it:

| encoder | `codec_tag_string` | `codec_name` |
|---|---|---|
| `pcm_s16le` | `sowt` | `pcm_s16le` |
| `pcm_s8` | `sowt` | `pcm_s8` |
| `pcm_s24le` | `in24` | `pcm_s24le` |
| `pcm_s24be` | `in24` | `pcm_s24be` |
| `pcm_s32le` | `in32` | `pcm_s32le` |
| `pcm_s32be` | `in32` | `pcm_s32be` |
| `pcm_f32le` | `fl32` | `pcm_f32le` |
| `pcm_f32be` | `fl32` | `pcm_f32be` |
| `pcm_u8` | `raw ` | `pcm_u8` |

**The FourCC does not determine the codec.** `sowt` covers both 16-bit and
8-bit; `in24`, `in32`, `fl32` and `fl64` each cover *both endiannesses*. So
endianness is not in the FourCC at all — it comes from the sample entry's
`enda` box — and width comes from `bits_per_sample`. A table keyed on FourCC
alone cannot reproduce the reference no matter how many rows it has, which is
what makes this a design finding rather than a data-entry task.

Two more measured on the way, both plain table rows: `ulaw` → `pcm_mulaw` and
`alaw` → `pcm_alaw` (currently both collapse to `pcm`), and `raw ` means
`pcm_u8` in an **audio** sample entry but `rawvideo` in a **video** one — the
current code lumps `raw ` into the PCM group regardless of which it is in.

Also missing and straightforward: `mp4v` → `mpeg4` (through the ESDS
object-type indication), `h263` → `h263`, `ap4h` → `prores`, `alac` → `alac`.

## How to re-run

```sh
cargo build -p vaco-probe
VACO_BIN_PROBE=target/debug/vaco-probe \
  cargo run -p vaco-conformance -- run --tier core
```

Every case prints its own reproduction command. The media is synthesised by the
reference at run time and discarded (D6) — nothing FFmpeg-derived is committed,
and a file described by a command in the manifest defends its own provenance in
a way a checked-in fixture does not.
