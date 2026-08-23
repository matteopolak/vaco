# `vaco-subtitle-text`

Layer 4. Text subtitle demuxers and muxers (FM-34, issue #591): SubRip,
WebVTT, ASS/SSA, and a dozen-plus de-facto formats besides. Built on the
shared cue model in `vaco-format-subtitle`.

---

## What it is

Issue #591 named "fifteen demuxers and six muxers"; the reference
(`ffmpeg 8.1`, `LC_ALL=C ffmpeg -demuxers` / `-muxers`, confirmed with
`ffmpeg -h demuxer=<name>` / `-h muxer=<name>`) names **16 demuxers and 8
muxers** in this family. Two corrections the brief's count missed:

- `sbg` looked like a subtitle format by name (it showed up in a naive
  `-demuxers` grep) and is not one — `ffmpeg -h demuxer=sbg` reports "SBaGen
  binaural beats script", an audio-synthesis format. Out of scope here.
- `ttml` is a **muxer only** in the reference; there is no `ttml` demuxer at
  all. This crate implements one anyway, from the W3C TTML1 spec, because the
  brief asked for TTML support and a mux-only round trip is of limited use —
  but it has no reference to differential-test against, unlike every other
  demuxer here, and that gap is flagged in the module itself.

| Format | Demux | Mux | `CodecId` |
|---|---|---|---|
| `srt` | yes | yes | `SubRip` (the reference's `subrip`; a distinct `srt` codec, "SubRip subtitle with embedded timing", also exists in `ffmpeg -codecs` and is not what this demuxer produces) |
| `webvtt` | yes | yes | `Webvtt` |
| `ass` (`.ass`, `.ssa`) | yes | yes | `Ass` (one demuxer, one codec, for both script versions) |
| `scc` | yes | yes | `Eia608` |
| `microdvd` | yes | yes | `Microdvd` |
| `jacosub` | yes | yes | `Jacosub` |
| `lrc` | yes | yes | `Text` (generic — measured, see below) |
| `ttml` | yes (spec-only, no reference) | yes | `Ttml` |
| `subviewer` | yes | no (reference ships no encoder) | `Subviewer` |
| `subviewer1` | yes | no | `Subviewer1` |
| `mpsub` | yes | no | `Text` (generic — measured, see below) |
| `pjs` | yes | no | `Pjs` |
| `realtext` | yes | no | `Realtext` |
| `sami` | yes | no | `Sami` |
| `vplayer` | yes | no | `Vplayer` |
| `mpl2` | yes | no | `Mpl2` |
| `stl` | yes | no | `Stl` |

## How it works

### `engine.rs` — one demuxer type, one mux driver, for every format

Every demuxer reads its whole input up front (`engine::read_all`, capped at
256 MiB — these are text files), sniffs its BOM
(`vaco_format_subtitle::decode_to_utf8_bytes`), calls the format's own
`fn parse(&[u8]) -> Vec<Cue>`, and hands the result to `engine::CueDemuxer` —
the *only* `Demuxer` implementation in the crate. `read_packet` pops the next
cue; `seek` does a linear scan by timestamp (these files are small; there is
no index to build). Adding a demuxer is: write `probe` and `parse`, wire them
into a `DemuxerDesc` constant.

The mux side is more heterogeneous (per-cue numbering, a running "previous
end", frame-rate state), so it is a small trait, `engine::CueMux`
(`accepts`/`write_header`/`write_cue`/`write_trailer`), driven by the generic
`engine::GenericTextMuxer<F>`. Adding a muxer is: implement `CueMux` for a
small format-specific struct, wire it into a `MuxerDesc` constant.

### Probing: strict on purpose

`planning/AGENT-CONSTRAINTS.md`'s "Detection and demuxing ask different
questions" is the load-bearing rule here — text subtitle formats are
structurally close to each other and to ordinary prose, so a probe that
reuses the lenient parser as its detector claims files it should not. Every
probe in this crate counts lines/blocks that pass the format's own strict
timing parser and scores `ProbeScore::repeating(hits)`, falling back to an
extension-only score when nothing parses. `tests/probe_matrix.rs` is the
automated version of "check every probe against every other format's sample
and against prose": it builds one sample per format and asserts (1) every
probe rejects plain prose, (2) every sample is recognised by its own probe,
and (3) **no other format's probe ever outscores a sample's true owner**.

That third property caught a real bug during development: `vplayer`'s
`HH:MM:SS:text` grammar is a literal prefix of `stl`'s
`HH:MM:SS:hh,HH:MM:SS:hh,text`, so `vplayer`'s naive parser read an STL line's
first timecode as valid and everything after it as "text", tying STL's own
probe score on an STL sample. Fixed in `vplayer::looks_like_stl_line`, which
rejects a line whose "text" opens with the `hh,HH:MM:SS:hh,` shape that only
an STL line's remainder produces. Regression test:
`vplayer::tests::does_not_claim_an_stl_line`. If you add a new format whose
grammar is a prefix or superset of an existing one, `probe_matrix.rs` will
catch the collision the same way — read it before assuming a new probe is
safe.

### The demuxer/decoder boundary

No demuxer here parses ASS override tags, SAMI's HTML fragments, or anything
else inside a cue's text — that is rendering, which belongs to a decoder in
`crates/codec/`, a different wave. Two formats' payloads look unusual as a
result of *not* doing that cleanup, and both are measured rather than
assumed:

- **SAMI and RealText's packet payload includes the demuxer's own timing tag**
  (`<SYNC Start=1000><P>text`, `<time begin="...">text`), not clean text —
  confirmed via `ffprobe -f sami -show_packets -show_data`. A parser that
  "cleaned up" the tag out of the payload would not match the reference.
- **SubViewer 1.0 appends a single trailing `\0`** to a cue's text, for
  reasons this crate has no explanation for beyond the measurement — checked
  on one sample, not a wide survey (see `subviewer1.rs`).

### The `CodecId` gap — closed

`vaco_codec_core::CodecId` is a closed, `#[non_exhaustive]` enum: only
`vaco-codec-core` can add a variant, and it was not in this crate's scope.
Eleven formats here — MicroDVD, JACOsub, TTML, SubViewer, SubViewer 1.0, PJS,
RealText, SAMI, VPlayer, MPL2, Spruce STL — had no `CodecId` of their own at
first, reported rather than worked around (see
`planning/AGENT-CONSTRAINTS.md`, "Scope"). The owning agent then added
`Jacosub`, `Microdvd`, `Mpl2`, `Pjs`, `Realtext`, `Sami`, `Stl`, `Subviewer`,
`Subviewer1`, `Ttml`, `Vplayer`, and a generic `Text`, probed independently
from `ffmpeg -codecs` 8.1 rather than taken from this crate's naming
verbatim — confirming, rather than assuming, that LRC and MPsub genuinely
have no codec of their own in the reference and both measure as `Text`, which
is what this crate had already found and is why no `Lrc`/`Mpsub` variant
exists.

Every format above now carries the codec its demuxer/muxer actually
produces, so `vaco-probe` prints the reference's own `codec_name` rather than
`unknown`.

## How to change it

- **Adding a format**: write `<name>.rs` with a `probe`, a `parse`, a
  `DemuxerDesc` constant (and, if the reference has an encoder, a `CueMux`
  impl plus a `MuxerDesc`), add `pub mod <name>;` to `lib.rs`, add a sample to
  `tests/probe_matrix.rs`'s table, and add a `vaco-component.toml` entry —
  **only after** the `DEMUXER`/`MUXER` const it names actually compiles (see
  `planning/AGENT-CONSTRAINTS.md`, "Export your descriptor before writing
  `vaco-component.toml`").
- **A probe scoring too eagerly**: check `tests/probe_matrix.rs` first; it is
  designed to catch exactly that class of bug across every registered format
  in one run.
- **The 256 MiB whole-file cap** (`engine::MAX_SUBTITLE_BYTES`): raise it if a
  legitimate subtitle file is ever this large, which would itself be
  surprising for a text format.

## Configuration

No CLI-facing options. `MicroDVD`'s frame rate is read per-file from an
optional `{1}{1}<fps>` header line (`microdvd::parse`), falling back to
`vaco_format_subtitle::time::MICRODVD_DEFAULT_FPS` (23.976) absent one.

## Dependencies

`vaco-format-subtitle` (the cue model), `vaco-format-core`, `vaco-codec-core`,
`vaco-packet`, `vaco-io`, `vaco-limits`, `vaco-core`, and `quick-xml` (TTML
parsing only). `#![forbid(unsafe_code)]`, builds for `wasm32-unknown-unknown`.
