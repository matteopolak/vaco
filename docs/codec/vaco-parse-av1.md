# `vaco-parse-av1` — AV1 header parsing

## What it is

Reads the syntax AV1 puts in front of the coded picture — OBU framing, the
sequence header, enough of the frame header to know a picture's type and
size, metadata OBUs — and turns it into the `CodecParameters` a demuxer
reports, plus `AV1CodecConfigurationRecord` (`av1C`) for MP4/Matroska
carriage. It **decodes nothing**: no tile, no prediction, no film-grain
synthesis, no reconstruction of any kind.

That line is load-bearing the same way it is for H.264/HEVC (D7, D15, plan 15
§1.6), though AV1 needs no patent-posture gate on top of it: the AOM Patent
License covers it and D3 makes it default-on regardless. Header parsing ships
unconditionally.

Layer 4 (`crates/codec/`). Registers no component — a header parser is a
shared helper and the *demuxer* registers, the same conclusion
`vaco-parse-h264`/`vaco-parse-hevc` reached. There is no
`vaco-component.toml`.

## How it works

| Module | Syntax |
|---|---|
| `leb` | `leb128()`, `uvlc()`, `su(n)`, `ns(n)` — §4.10.3–§4.10.7 |
| `obu` | `obu_header()`, `open_bitstream_unit()`, both OBU framings — §5.2–§5.3, Annex B |
| `seq` | `sequence_header_obu()`, `color_config()`, `timing_info()`, `decoder_model_info()` — §5.5 |
| `frame_header` | `uncompressed_header()`'s common prefix and the intra `frame_size()`/`render_size()` path — §5.9 |
| `metadata` | `metadata_obu()` and its five payload shapes — §5.8 |
| `profile` | profiles, tiers and levels — Annex A |
| `av1c` | `AV1CodecConfigurationRecord` — AV1 Codec ISO Media File Format Binding §2.3.3 |
| `params` | the `CodecParameters` a sequence header implies, and the pixel-format/colour mapping |
| `parser` | `Av1Parser`, the streaming temporal-unit splitter |
| `cbs` | the `CbsCodec` implementation, and this crate's verdict on it (see below) |

### AV1 has no NAL start codes

Every OBU carries its own type in a fixed header byte and either sizes itself
(`obu_has_size_field`) or is sized by whatever wraps it. There is nothing to
scan for, so `obu::units` walks declared lengths with checked arithmetic
rather than searching for a byte pattern the way `vaco-format-nalu`'s scanner
does for `00 00 01`.

Two framings exist, and only one has ever been observed in this crate's test
corpus:

- **`Av1Framing::ObuStream`** — OBUs concatenated directly, each self-sized.
  This is what MP4/Matroska sample data use, what `av1C`'s `configOBUs` use,
  and — measured — what `ffmpeg -f obu` writes for a raw elementary stream.
- **`Av1Framing::LowOverheadBitstream`** — the specification's actual Annex B:
  nested `temporal_unit_size`/`frame_unit_size`/`obu_length` `leb128()`
  wrappers. No fixture in this crate's corpus uses it; it is implemented from
  the specification text and exercised by hand-built unit tests and the
  fuzzer only. Treat it as the less-verified half of `obu.rs`.

### Where a temporal unit ends, for the streaming parser

`OBU_TEMPORAL_DELIMITER` (empty payload, §5.6) is the specification's own
marker for "a new temporal unit starts here", and every encoder measured for
this crate emits one before every frame. `Av1Parser` splits on it, the direct
analogue of `vaco-parse-hevc`'s use of `first_slice_segment_in_pic_flag` — an
even lower-friction boundary than the HEVC bit test, since it needs no
parameter sets, no bit position, and no lookahead: it is a type comparison
on a value already extracted by `obu::units`. A stream that never emits one
degrades to treating every OBU as its own access unit rather than hanging.

### Frame headers: the intra path only

`uncompressed_header()`'s syntax forks after a shared prefix (frame type,
show flags, screen-content/integer-mv choice, frame id, order hint,
`refresh_frame_flags`): a key/intra-only frame calls `frame_size()` directly,
which is fully self-contained given the sequence header; an inter frame calls
`frame_size_with_refs()`, which can copy a **reference frame's** dimensions —
state this crate does not track, because tracking it means modelling the
reference-frame lifetime across the whole stream, which is decoder-shaped
state, not parser-shaped state. `frame_header::FrameHeader::parse` reads the
shared prefix for every frame type (so nothing downstream of it misaligns)
and returns `FrameHeader::Inter` without a fabricated size when it hits that
fork. See the module's doc comment for the full argument and the one inter
path this crate *does* handle (`frame_size_override_flag &&
error_resilient_mode`, which calls `frame_size()` directly).

This is not a gap in what `ffprobe` needs: every fixture measured for this
crate reports its resolution from the *sequence* header, because ordinary
encoder output leaves `frame_size_override_flag` at 0 (see "Measured
fidelity" below).

## The `vaco-codec-cbs` verdict

This crate's brief specifically asked it to settle whether the
read/modify/write layer proved against H.264 and HEVC — both NAL-based —
serves a codec that is not. The short answer: **mostly yes, and the one place
it does not is a property of one AV1 framing's own specification, not of
`vaco-codec-cbs`'s flat-`CbsFragment` design.**

- **`Av1Framing::ObuStream` fits the flat-unit-list model at zero cost.**
  `Av1Cbs::split`/`assemble` are simpler than HEVC's Annex B case — there is
  no start-code escaping to get right — and the "OBUs nest inside temporal
  units" worry the brief raised does not bite here: a temporal delimiter is
  just another `CbsUnit` with a recognisable `unit_type`, exactly like an
  HEVC AUD.
- **`Av1Framing::LowOverheadBitstream` genuinely does not round-trip
  byte-for-byte in the general case**, but the reason is not nesting either.
  A `frame_unit_size` boundary in Annex B is **framing metadata the encoder
  chose, with no meaning derivable from the OBUs it wraps** — nothing says a
  frame unit corresponds to one decoded frame. `CbsUnit` has nowhere to
  record which grouping the source used, so `Av1Cbs::assemble` always
  reconstructs one `frame_unit` per temporal unit. That is always
  conformant — a decoder reads the identical OBUs in the identical order —
  but is not always the same *bytes*. `cbs::FRAME_UNIT_GRANULARITY_DIVERGENCE`
  names this, and `cbs::tests::low_overhead_frame_unit_granularity_does_not_round_trip`
  pins it with a hand-built two-`frame_unit` stream that collapses to one.

If `LowOverheadBitstream` round-tripping is ever needed, the fix is
codec-side — carrying group boundaries in `Av1Content` or a sibling type —
not a change to `vaco-codec-cbs` itself. Nothing encountered while
implementing this crate suggests the trait needs a new capability.

## Measured fidelity against `ffprobe 8.1`

Generated with `ffmpeg -c:v libsvtav1` (the only AV1 encoder available in
this environment — `ffmpeg -codecs` here lists no `libaom-av1` encoder, so
every measurement below is `libsvtav1`-sourced) into MP4, WebM and raw
`.obu`, then probed with `ffprobe -show_streams`. A 642x358 `yuv420p` 8-bit
stream at level 2.1:

```
width=642 height=358 coded_width=642 coded_height=358
pix_fmt=yuv420p profile=Main level=1 color_range=tv
r_frame_rate=25/1 avg_frame_rate=25/1
```

This crate's `SequenceHeader::parse` + `params::codec_parameters` reproduce
every one of those fields exactly from the same stream's `OBU_SEQUENCE_HEADER`
payload (`params::tests::a_real_sequence_header_matches_the_measured_ffprobe_output`,
and the bit-by-bit trace in `seq.rs`'s own test fixture comment).

### Two things the brief warned against assuming, checked and found false

1. **No `yuvj` family.** H.264 maps full-range 4:2:0/4:2:2/4:4:4 at 8 bits to
   `yuvj420p`/`yuvj422p`/`yuvj444p`; HEVC narrows that to 4:2:0 only. **AV1
   has neither.** Probed:

   ```
   ffmpeg -f lavfi -i testsrc -color_range pc -pix_fmt yuv420p -c:v libsvtav1 full.mp4
   ffprobe -show_entries stream=pix_fmt,color_range full.mp4
   # pix_fmt=yuv420p  color_range=pc
   ```

   Full range stays `yuv420p`, with `color_range` reported alongside it
   rather than folded into the format name. See `params::pixel_format`.

2. **Resolution has no separate coded/display split the way HEVC's
   conformance window does.** AV1's `max_frame_width`/`max_frame_height` *is*
   the coded size and, absent an explicit `render_size()` override, the
   displayed size too — there is no cropping-offset syntax at all in the
   sequence header. Every fixture measured here reports `width == coded_width`.

### What is unverified, and why

- **Monochrome, 4:2:2, 4:4:4, and 12-bit pixel formats.** `libsvtav1` accepts
  only `yuv420p`/`yuv420p10le` as input (`ffmpeg -h encoder=libsvtav1` lists
  exactly those two); a `-pix_fmt gray` source fed through it was silently
  converted and the resulting stream still had `mono_chrome = 0`. The
  `gray`/`gray10le`/`gray12le` and 4:2:2/4:4:4 mappings in
  `params::pixel_format` follow the naming convention `vaco-pixfmt` already
  defines for every other codec rather than a black-box measurement against
  `ffprobe`. This is the one part of the fidelity claim that is unverified
  by measurement; a probe with `libaom-av1` (`aomenc --monochrome`,
  `--input-bit-depth=12`, etc.) would close it.
- **High/Professional profile and non-2.1 levels.** `libsvtav1` only ever
  emitted profile `Main`/level `2.1` for the small fixtures generated here.
  The profile and level *names* (`Main`/`High`/`Professional`, and the
  `"2.0"`.."7.3" naming rule) come from the specification's own Annex A
  rather than from a probe, which is sound under D7/D15 (format-dictated
  names are merger-doctrine territory) but is not independently confirmed
  against `ffprobe`'s spelling the way the pixel-format table is.
- **`render_size()` overrides and superres.** No encoder available here
  signals `render_and_frame_size_different` or `use_superres`; both are
  implemented from the specification (§5.9.6–§5.9.7) and unit-tested by hand,
  not measured.

## Registration: how a demuxer reaches this crate

This crate ships a `vaco-component.toml` naming `vaco_parse_av1::PARSER`, a
`vaco_codec_core::ParserDesc`. `cargo xtask gen-registry` collects it into
`vaco_registry::PARSERS`, and `vaco_registry::Parsers` — the one
`ParserProvider` in the build — answers `parser_for(CodecId::Av1)` with a
`Box<dyn Parser>` built from it.

**No demuxer names this crate.** D14.1 and `cargo xtask layer-check` forbid a
`crates/format/` crate from depending on a `crates/codec/` one; the indirection
is what makes `-show_streams` able to report bitstream fields without that edge.

Two consequences worth knowing when changing anything here:

* **Everything a demuxer can see goes through `dyn Parser`.** `parse`,
  `parameters` and `set_extradata` are the whole surface. An inherent method,
  however useful, is invisible from a container. `tests/provider.rs` is written
  entirely against `Box<dyn Parser>` for that reason — a version written against
  the concrete type would pass while the seam stayed broken.
* **`ParserDesc::make` takes `Limits`.** A parser on the probe path is handed
  attacker-controlled bytes before anything has validated them, so there is no
  no-argument constructor to reach for.

### Three ways AV1 differs, all measured rather than assumed

None of these transfers from H.264 or HEVC, and `tests/provider.rs` asserts all
three so a change shows up as a failure:

* **No coded/display split.** `coded_width == width`. H.264 also reports the
  cropped size as its coded size but HEVC reports the *coded* one — 1918 against
  1920 on the same 1918x1080 source.
* **No `yuvj` pixel-format family**, at any range. H.264 has one, at 8 bits only.
* **Neither `bits_per_raw_sample` nor `nal_length_size`.** The reference prints
  `N/A` for the first and omits the second entirely; both are `None` here.

There is also **no framing switch**. AV1's low-overhead bitstream format is the
same OBU stream in MP4, in Matroska and in a raw file, so unlike H.264 and HEVC
`Parser::parse` needs no adjustment after `set_extradata`.

`mime_codec_string` is `av01.<profile>.<level><tier>.<depth>`, probed as
`av01.0.00M.08` at 8 bits and `av01.0.00M.10` at 10. `vaco-probe` builds it;
`CodecParameters` does not carry `seq_tier`, so the tier is emitted as `M` and
that is a recorded gap rather than a derivation.

## How to change it

- **The CBS write path (D-20)**: `cbs::Av1Cbs::write_unit` re-encodes
  `Av1Content::SequenceHeader` and `Av1Content::Metadata` bit-exactly —
  `sequence_header_obu()` (profile, operating points, frame size, every
  tool-enable flag, `color_config()`) and all four `metadata_obu()` shapes.
  Verified against the real `libsvtav1` sequence header this crate's own
  tests already carry: read with no edit, write back, byte for byte. One
  documented, detectable exception: a sequence header with
  `decoder_model_info_present_flag` set returns `Error::Unsupported`, because
  `SequenceHeader::parse` already discards `decoder_model_info()`'s fields on
  read (pre-existing, not introduced by the writer). `initial_display_delay`
  is not even tracked as a flag, so it is always written absent — see
  `cbs.rs`'s write-side module doc for both.
- **A new sequence-header field**: add it to `seq::SequenceHeader`, read it
  in `SequenceHeader::parse` at the exact bit position §5.5.1's table gives
  it — every field after it in the syntax depends on the read cursor landing
  correctly, so an inserted read in the wrong place desynchronises the rest
  of the structure silently rather than erroring. The real-fixture test in
  `seq.rs` (`a_real_sequence_header_matches_the_measured_stream`) is the
  fastest way to notice a misalignment: its doc comment has the full bit
  trace against the fixture bytes.
- **Extending `frame_header` past the intra path**: the shape to fill in is
  `frame_size_with_refs()`, which needs `RefUpscaledWidth[i]`/
  `RefFrameHeight[i]` tracked per reference-frame slot (`NUM_REF_FRAMES = 8`)
  across the whole stream, keyed by `refresh_frame_flags`. That state
  belongs in `Av1Parser`, not in `frame_header`, which should stay a pure
  function of one frame's bytes plus the sequence header.
- **A new metadata payload**: add a `metadata_type` constant and a match arm
  in `metadata::parse`; unrecognised types already fall through to
  `Metadata::Other` rather than erroring, so adding a decoded shape is purely
  additive.
- **Annex A level/profile numbers**: `profile.rs`'s `level_table!` macro is
  the single place the numbers live, cross-checked against a public
  secondary transcription rather than any implementation (see the module
  doc). Do not add a second copy of the table anywhere else in this crate —
  that is exactly the kind of drift D19's `dup-check` and this crate's own
  `profile::tests` exist to catch.

## Configuration

None. Every entry point (`SequenceHeader::parse`, `FrameHeader::parse`,
`metadata::parse`, `Av1CodecConfigurationRecord::parse`, `Av1Parser::new`)
takes an explicit `vaco_limits::Budget`/`Limits` rather than reading any
global or environment state. `Av1Parser::with_max_access_unit` overrides the
8 MiB per-temporal-unit cap (`parser::DEFAULT_MAX_ACCESS_UNIT`).

## Dependencies

`vaco-bitstream` for `BitReader`/`ByteReader` (this crate supplies its own
`leb128`/`uvlc`/`su`/`ns` in `leb.rs` — AV1 needs neither Exp-Golomb nor NAL
framing, and no other codec in the workspace needed these yet, so they were
not worth lifting into `vaco-bitstream` for one caller), `vaco-limits` for
the budget, `vaco-codec-cbs` for the read/modify/write layer, `vaco-codec-core`
for the `Parser` trait and `CodecParameters`, `vaco-color`/`vaco-pixfmt` for
the signalling enums, `vaco-packet` for the emitted packets. No external
runtime dependencies.

## Safety on untrusted input

- `leb128()`/`uvlc()` are built on `BitReader`'s sticky-overrun model and cap
  their own iteration against the specification's own limits (`leb128`'s
  eight-byte cap, `uvlc`'s 32-bit `leadingZeros` cap), not on attacker input.
- Every bitstream-driven loop count is bounded before the loop runs:
  `operating_points_cnt_minus_1` (5 bits, ≤32) is fuel-charged up front; every
  other loop in this crate has a compile-time bound
  (`seq::NUM_REF_FRAMES`, the three colour primaries, the metadata fixed
  shapes).
- `obu::units` cannot resynchronise past a corrupt length the way a NAL
  scanner resynchronises past a bad start code — AV1 has no start codes — so
  every size computation uses checked arithmetic and rejects a unit whose
  declared length would run past the buffer, rather than trusting it.
- `#![forbid(unsafe_code)]`; `unwrap`/`expect`/`panic`/`indexing_slicing` are
  denied workspace-wide and nothing in non-test code here is exempted from
  that.

### Fuzzing

Two targets, both `cargo +nightly fuzz run <target> -- -max_total_time=30`:

- **`parse_av1`** — the streaming `Av1Parser` (chunking-invariance, the
  consume-everything-or-hand-back-a-unit contract), plus every OBU fed
  directly to `SequenceHeader::parse`, `FrameHeader::parse`, `metadata::parse`
  and `Av1CodecConfigurationRecord::parse`. Measured: `exit=0`,
  `execs=378448` (single 45-second run; corpus at `fuzz/corpus/parse_av1`
  holds ~2000 inputs from earlier runs), `find fuzz/artifacts/parse_av1 -type f`
  empty.
- **`cbs_av1`** — `Av1Cbs` under both framings: unit-origin correctness,
  `ObuStream`'s exact round trip (`out.len() <= data.len()` always; byte
  equality specifically when nothing was dropped as an unparseable tail —
  see the target's own comment for why the weaker bound is the one that
  holds unconditionally), and `LowOverheadBitstream`'s content-preserving,
  framing-lossy round trip. Measured: `exit=0`, `execs=2944062` (single
  45-second run; corpus at `fuzz/corpus/cbs_av1` holds ~1150 inputs),
  `find fuzz/artifacts/cbs_av1 -type f` empty.

`cbs_av1`'s first run against this crate found a real bug — in the fuzz
target itself, not the crate: the initial assertion claimed every successful
`split` of `ObuStream` data round-trips byte-for-byte through `assemble`,
which is false for a single truncated byte (`split` correctly, silently
drops an OBU it cannot parse — see `obu::units`'s own "stops at the first
unit that fails to parse" documentation — so the assembled output is
*shorter*, not different). Fixed to assert the invariant that actually holds
unconditionally (never grows) plus the stronger one only when the whole
input was consumed, mirroring `cbs_hevc`'s fuzz target.
