# `vaco-parse-hevc` — H.265/HEVC header parsing

## What it is

Reads the syntax HEVC puts in front of the picture data — VPS, SPS, PPS, SEI and
slice segment headers — and turns it into the `CodecParameters` a demuxer
reports. It **decodes nothing**: no coding unit, no residual, no sample.

That line is load-bearing (D7, D15, plan 15 §1.6). HEVC is the most
patent-encumbered codec in the inventory and its decoders are not in the default
build, but a parameter-set parser implements no decoding process and ships.

Layer 4 (`crates/codec/`). Registers no component — a header parser is a shared
helper and the *demuxer* registers, which is the conclusion `vaco-parse-h264`
reached too. There is no `vaco-component.toml`.

## How it works

| Module | Syntax |
|---|---|
| `nal` | NAL unit types, Table 7-1; the two-byte header §7.3.1.2 |
| `ptl` | `profile_tier_level()` §7.3.3 |
| `vps` | `video_parameter_set_rbsp()` §7.3.2.1 |
| `sps` | `seq_parameter_set_rbsp()` §7.3.2.2, VUI §E.2.1, HRD §E.2.2, scaling lists §7.3.4 |
| `pps` | `pic_parameter_set_rbsp()` §7.3.2.3 |
| `rps` | `st_ref_pic_set()` §7.3.7 and the derivation of §7.4.8 |
| `slice` | `slice_segment_header()` §7.3.6.1 and its three sub-structures |
| `sei` | `sei_message()` §7.3.5 and the Annex D payloads worth decoding |
| `poc` | picture order count, §8.3.1 |
| `profile` | profiles, tiers and levels, Annex A |
| `hvcc` | `HEVCDecoderConfigurationRecord`, ISO/IEC 14496-15 §8.3.3.1 |
| `params` | the parameter-set store and the `CodecParameters` an SPS implies |
| `parser` | `HevcParser`, the streaming access-unit splitter |
| `cbs` | the `CbsCodec` implementation — split, edit, re-assemble |

Two entry points, because there are two kinds of source. `Parser::parse` is the
byte-stream path (MPEG-TS, raw elementary streams), where access-unit boundaries
must be derived. `HevcParser::push_access_unit` is the container path (MP4,
Matroska), where the container already knows them.

### Where HEVC is easier than H.264

Worth stating, because it shapes the parser:

- **`first_slice_segment_in_pic_flag` is the first bit of every slice segment
  header**, so access-unit boundary detection is a bit test rather than H.264
  §7.4.1.2.4's seven-field comparison — and it needs no parameter sets at all.
  See `slice::peek_first_slice_in_pic`.
- **Nothing in an HEVC PPS is sized by an SPS field**, so parameter sets parse
  independently and may arrive in any order. `vaco-parse-h264`'s store has to
  resolve the SPS before it can finish a PPS, and re-parse if it guessed wrong.

### Where HEVC is harder

- **`profile_tier_level()`** is 96 bits with three separate traps; they are
  written out at the top of `ptl.rs`. The one that bites hardest: the
  `reserved_zero_2bits` padding is present **only** when
  `maxNumSubLayersMinus1 > 0`, so a parser that always reads it desynchronises
  every single-layer stream — which is nearly every stream.
- **Short-term reference picture sets can be coded by reference to each other**,
  so §7.4.8's derivation is required in order to *parse* the next set. This is
  the one place a derivation is load-bearing for parsing. It also has a trap:
  §7.3.7's `delta_idx_minus1` is present only when
  `stRpsIdx == num_short_term_ref_pic_sets`, and inferring that count from "how
  many sets have been read" fires on every set inside the SPS. Caught by a unit
  test; see `rps::parse_st_ref_pic_set`.

## Measured fidelity against `ffprobe 8.1`

`tests/reference.rs` embeds nineteen SPS units lifted from `libx265` streams and
asserts twelve fields of each against what `ffprobe` printed for the same file.
All nineteen rows match on all twelve fields. The generator command is in that
file's header so the corpus can be rebuilt when the pinned reference moves.

Axes covered: 1920x1080, 1280x720, 640x360, 1918x1078, 66x34, 720x576, 320x240;
4:2:0/4:2:2/4:4:4/monochrome; 8/10/12 bit; tv and pc range; unspecified, BT.709
and BT.2020 colour; Table E-1 and Extended SAR; 24, 25, 30000/1001 and 60000/1001;
reorder depth 0 and 2.

### The four numbers where HEVC's answer differs from H.264's

Each is a `// D17:` note at the code that produces it. Each was measured, not
assumed, and each has a plausible-looking wrong answer.

| | H.264 (`vaco-parse-h264` measured) | HEVC (measured here) |
|---|---|---|
| `coded_width`/`coded_height` | equal to the **cropped** size | the **coded** size |
| `r_frame_rate` | twice the picture rate | the picture rate |
| monochrome `pix_fmt` | `yuv420p` | `gray` |
| `chroma_location` with no VUI info | `left` at every chroma format | `left` for 4:2:0 only |
| `field_order` from headers alone | `progressive` | `unknown` |

**Resolution.** `pic_width_in_luma_samples` is the coded size, already a multiple
of `MinCbSizeY`; the conformance window's four offsets are in **chroma units**,
so for 4:2:0 a `conf_win_right_offset` of 1 removes *two* luma columns. `ffprobe`
prints both sizes: a 1918x1078 stream reports `coded_width=1920 width=1918`, and
a 66x34 one reports `66x34` from a coded `72x40`. Note this is a *variable*
granularity, unlike H.264's fixed 16-sample macroblock alignment — an encoder
with a 64-sample minimum coding block pads to 64.

**Frame rate.** `vui_time_scale / vui_num_units_in_tick`, not halved. Verified
both ways: `-bsf:v trace_headers` on a 24 fps `x265` stream gives
`vui_num_units_in_tick = 1, vui_time_scale = 24`, and `ffprobe -f hevc` prints
`r_frame_rate=24/1`. The H.264 encode of the same source prints `48/1`.

**Pixel format.** Probed across the whole matrix of {gray, 420, 422, 444} ×
{8, 10, 12} × {tv, pc}. Two rules came out: monochrome is `gray{depth}`, and
`yuvj` exists at 4:2:0/8-bit/full-range and nowhere else. Both are the opposite
of the H.264 answer. The `pc` on the monochrome row is `x265` setting
`video_full_range_flag` for a gray input, not a parser rule — patching the flag
to 0 in the same stream gives `tv`.

**Chroma location.** §7.4.3.2 infers `chroma_sample_loc_type_top_field = 0`
("left") whenever `chroma_loc_info_present_flag` is 0, at any chroma format. The
reference applies it for 4:2:0 only. Confirmed the flag really is 0 in all four
streams with `trace_headers`, so this is the parser's rule and not the encoder's.

**Field order.** All nineteen streams report `unknown`, including plainly
progressive ones, and patching `field_seq_flag` to 1 changes nothing. HEVC has no
equivalent of `frame_mbs_only_flag`, and the reference declines to infer an
order; only a `pic_timing` SEI's `pic_struct` supplies one.

### The profile rule, recovered by probe

`general_profile_idc` was patched to each of 0..=11 with the compatibility flags
cleared, and then to 0 with exactly one compatibility flag set — 24 rows, at the
RBSP level with the emulation prevention recomputed, because patching the escaped
bytes directly creates an invalid EBSP and the reference then rejects the whole
stream. (The first attempt did exactly that and produced 24 rows of `unknown`,
which looked like a finding and was an artefact.)

The rule: **the profile is `general_profile_idc` when it is non-zero, and
otherwise the lowest set `general_profile_compatibility_flag[i]`.** With neither,
`ffprobe` prints `0`.

Names, and what the reference does *not* name:

| `profile_idc` | printed |
|---|---|
| 1 | `Main` |
| 2 | `Main 10` |
| 3 | `Main Still Picture` |
| 4 | `Rext` |
| 6 | `Multiview Main` |
| 9 | `Scc` |
| 0, 5, 7, 8, 10, 11 | the number |

Five of those unnamed values *are* named by Annexes G, H and I (Scalable Main,
3D Main, Screen-Extended Main, and two Multiview/Scalable 10 profiles). D17 says
to reproduce the reference's spelling, so they print as numbers here too.

### The sample-aspect-ratio rejection rule

`x265` clamps its own `--sar`, so the boundary could not be probed by encoding.
It was recovered by splicing `aspect_ratio_idc = 255` and a 32-bit
`sar_width`/`sar_height` pair into the VUI of a 640x360 stream at the *bit* level,
with the emulation prevention recomputed. Thirty-four rows.

The rule is H.264's, unchanged: reduce first, then

```
scaled = num < den ? (width  * num) / den
                   : (height * den) / num      (truncating)
usable iff scaled > 0
```

On 640x360 that admits `360:1` and rejects `361:1`, admits `1:640` and rejects
`1:641`. `720:2` is accepted because it reduces to `360:1`; `722:2` is not. That
it is the same rule at a different picture size says it is the reference's general
aspect-ratio handling rather than anything codec-specific.

## How to change it

- **A new SEI payload**: add a constant to `sei::payload_type` and an arm to
  `decode_payload`. An unrecognised payload becomes `SeiPayload::Other` with its
  bytes intact, so nothing is lost by not adding one.
- **A new profile name**: `profile::profile_name`. Re-derive from the reference
  first — the table is what `ffprobe` prints, not what Annex A says.
- **The write path**: `cbs::HevcCbs::write_unit` handles raw units and returns
  `Unsupported` for typed parameter sets. Filling that in means writing
  `profile_tier_level()`, every reference picture set and the whole VUI back out
  **bit-exactly**; a writer that is not bit-exact corrupts a stream silently.
  Plan 15's D-19 budgets it separately.

### Gotchas

- **`Parser::parse` returns a queued access unit with a consumed count of
  zero.** A caller that writes `off += used.max(1)` advances past a byte that has
  not been parsed. `vaco-parse-h264`'s first benchmark measured a 160x slowdown
  from exactly that.
- **`pred_weight_table()`'s presence conditions are approximated.** §7.3.6.3
  reads `luma_weight_l0_flag[i]` only when the *i*-th reference picture differs
  from the current picture — a question about the decoded picture buffer, which a
  header parser does not have. The flags are read unconditionally, which is exact
  for every stream that is not screen-content coding.
  `SliceHeader::weight_table_exact` says which case applied.
- **`default_display_window` is parsed but not applied** to the reported size.
  It is a display hint, and the reference does not apply it either unless its
  `apply_defdispwin` option is set, which is off by default.
- **Multi-layer syntax is not described.** The SPS's and PPS's multilayer and 3D
  extensions carry syntax this crate does not read, so anything behind them —
  including the SCC extension in that ordering — is not reached. Base-layer
  streams are unaffected; `nuh_layer_id != 0` units are skipped by the parser and
  kept whole by the CBS layer.

## Performance

Measured with `cargo bench -p vaco-parse-hevc` (divan) on an Apple M5. The
machine was under load from other work, so read the **ratios**, not the absolute
numbers — plan 12's PF-0.1 rule.

| | fastest |
|---|---|
| `profile_tier_level` | 25 ns |
| `pps_parse` | 82 ns |
| `vps_parse` | 92 ns |
| `slice_header` (real) | 208 ns |
| `slice_header` (random tail) | 263 ns |
| `sps_parse` | 218 ns |
| `sps_parse_with_deescape` | 328 ns |
| `derive_codec_parameters` | 74 ns |
| `parse_elementary_stream` | 1.35 ms / MiB |
| `scan_only` | 69 µs / MiB |

Three things worth recording:

1. **An HEVC SPS costs about 1.6x an H.264 one** (218 ns against the 135 ns
   `vaco-parse-h264` measured), which is roughly the ratio of their syntax
   element counts. The bounded-`ue(v)`-plus-fuel machinery does not dominate.
2. **The whole-stream path is 19.5x the start-code scan**, which is the opposite
   of what was expected. It is not a bug: the fixture packs an access unit into
   every 520 bytes, so a megabyte holds ~2000 of them, each costing a ~210 ns
   slice-header parse plus two copies of the unit. A real 1080p stream carries
   ~30 KB access units — sixty times fewer per megabyte — so the fixture is a
   deliberate worst case for header density.
3. **A prediction that measured backwards**, recorded per PF-0.1. Giving
   `num_entry_point_offsets` §7.4.7.1's geometry-derived bound (16 for a 1080p
   wavefront stream) instead of a flat 8192 was expected to be a large speedup on
   malformed headers. Measured A/B: **252 ns under the flat bound, 263 ns under
   the tight one** — within noise, and if anything the loose bound was faster.
   The bound stays, on hardening grounds: it refuses a hostile value 500x
   earlier. It is not a speedup, and the first version of the comment claiming it
   was has been corrected.

## Safety on untrusted input

- Every `ue(v)` goes through `BoundedGolomb`: an explicit ceiling at the read
  site, the specification's own range constraint wherever it states one, plus one
  unit of fuel per syntax element.
- Every count that sizes a loop is charged against fuel **before** the loop runs.
- Every structure the syntax leaves open-ended has an explicit ceiling: the SEI
  header's `ff_byte` run, the entry-point offset list, the slice header
  extension, the VPS's layer-set flag matrix.
- Two fuzz targets, `parse_hevc` and `cbs_hevc`. The first asserts that chunk
  size does not change the access-unit sequence — the property `vaco-parse-h264`'s
  fuzzer found three separate streaming bugs against, none of them reachable by a
  whole-buffer test. The second asserts the reframing round trip, and found both
  Annex B expressiveness limits recorded below.

## Known divergences

**`ANNEXB_EXPRESSIVENESS_DIVERGENCE`** (`cbs.rs`). Annex B is a strictly less
expressive container than a length prefix. A NAL unit whose bytes end in `0x00`
loses them (§B.1's `trailing_zero_8bits` are indistinguishable from payload
zeros), and a unit containing `00 00 01` splits in two. Both are impossible in a
conforming stream — §7.4.1.1's `rbsp_trailing_bits()` guarantees a non-zero last
byte, and emulation prevention guarantees no embedded start code — and both are
detected by `cbs::annexb_safe`. Found by the `cbs_hevc` fuzzer, pinned by
`a_unit_annex_b_cannot_express_is_reported`.

**`mime_codec_string` is not pinned against the reference.** `ffprobe 8.1`
accepts `-show_entries stream=mime_codec_string` and prints nothing for an HEVC
track, so there is no observed output to match. `hvcc::mime_codec_string` follows
RFC 6381 §3.3 alone — including the reversed compatibility-flag field, which is
the part everyone gets wrong. If a future reference version starts printing one,
this is the first thing to re-derive.

## Configuration

None. No features, no options, no environment. `Budget`/`Limits` come from the
caller; `HevcParser::with_max_access_unit` overrides the 8 MiB per-access-unit
ceiling.

## Dependencies

`vaco-bitstream` (reader), `vaco-codec-golomb` (bounded Exp-Golomb),
`vaco-format-nalu` (framing and RBSP extraction), `vaco-codec-cbs` (the
read/modify/write layer), `vaco-codec-core` (`Parser`, `CodecParameters`),
`vaco-color` and `vaco-pixfmt` (signalling enums), `vaco-limits` (budget),
`vaco-packet` (emitted packets), `vaco-core` (errors, rationals). No external
runtime dependencies.

## Specification

ITU-T H.265 (ISO/IEC 23008-2): §7.3 and §7.4 for the syntax and semantics, §8.3.1
for picture order count, Annex A for profiles, tiers and levels, Annex B for the
byte stream, Annex D for SEI, Annex E for the VUI and HRD. ISO/IEC 14496-15
§8.3.3.1 for `hvcC`. RFC 6381 §3.3 for the codec parameter string. Nothing here
was taken from any implementation (D7).
