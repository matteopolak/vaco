# `vaco-parse-h264`

Layer 4. H.264 parameter sets, SEI and slice headers, the access-unit splitter,
and the `CodecParameters` they imply. **No decode.**

## What it is

Everything H.264 puts in front of the picture data: `seq_parameter_set_data()`,
`pic_parameter_set_rbsp()`, `sei_message()`, `slice_header()`, and picture order
count. It stops there — there is no `slice_data()`, no macroblock, no residual,
no motion compensation, no sample of output anywhere in it.

Written from ITU-T H.264 (ISO/IEC 14496-10) version 14 (2020): §7.3 and §7.4 for
syntax and semantics, §8.2.1 for picture order count, Annex A for profiles and
levels, Annex B for the byte stream, Annex D for SEI, Annex E for the VUI and
HRD; and ISO/IEC 14496-15 §5.3.3.1 for `avcC`. Per D7, from the specifications
only.

### The line, and why it is where it is

H.264 is patent-encumbered and its decoders are not in the default build. A
parameter-set parser implements no decoding process and is not "a decoder" under
any pool's definition of a unit, so it ships (D4, D9, plan 15 §1.6 and §6.2).
Anything that drifts across the line takes the crate out of the default build
with it.

The one clause-8 procedure here is **picture order count** (§8.2.1). It is
integer arithmetic over slice-header fields: it needs no reference picture,
touches no macroblock, and produces an output *order* rather than an output
*picture*. A demuxer needs it to synthesise presentation timestamps for an
elementary stream that has none. That is comfortably on the near side.

## How it works

| Module | Syntax |
|---|---|
| `nal` | NAL unit types, Table 7-1 |
| `sps` | §7.3.2.1.1, §E.1.1 `vui_parameters()`, §E.1.2 `hrd_parameters()` |
| `pps` | §7.3.2.2, including the `more_rbsp_data()` tail |
| `slice` | §7.3.3 and its three sub-structures (§7.3.3.1-3) |
| `sei` | §7.3.2.3 framing, and the Annex D payloads worth decoding |
| `poc` | picture order count, §8.2.1, all three types |
| `profile` | Annex A profiles and levels |
| `avcc` | `AVCDecoderConfigurationRecord`, ISO/IEC 14496-15 §5.3.3.1 |
| `params` | the parameter-set store and the derived `CodecParameters` |
| `parser` | `H264Parser`, the streaming access-unit splitter |

### Parameter-set dependencies, which are not optional

Three structures cannot be parsed in isolation, and a parser that guesses
produces plausible nonsense rather than an error:

- A **slice header** needs its PPS *and* SPS. `frame_num`'s width in bits comes
  from the SPS; whether `delta_pic_order_cnt_bottom` exists at all comes from
  the PPS. `peek_pps_id` reads the first three `ue(v)` elements — which the
  format puts first precisely because they depend on nothing — to find the right
  pair before the full parse.
- A **PPS**'s optional tail needs its SPS: the scaling-list count is
  `6 + 2` or `6 + 6` depending on `chroma_format_idc`. Passing `None` parses up
  to that point and stops, which is the best answer for a PPS that arrives
  before its SPS — a real situation in a stream joined mid-flight.
- A **`pic_timing` SEI** needs the active SPS: whether it begins with
  `cpb_removal_delay`, and how many *bits* that field is, are HRD facts. Without
  one it is returned as raw bytes rather than guessed at.

### Where an access unit ends

Nothing in an Annex B stream marks a picture boundary; it is derived from
§7.4.1.2.3 and §7.4.1.2.4. `SliceHeader::starts_new_picture` implements the
seven syntax conditions; the parser adds the §7.4.1.2.3 rule that an AUD,
parameter set or SEI *following* a VCL unit begins the next access unit.

A slice whose parameter sets have not been seen is treated as a new picture.
That is the safe direction: it splits where a boundary might be rather than
merging two pictures into one.

### Two entry points, because there are two kinds of source

- `Parser::parse` — the byte-stream path (MPEG-TS, raw elementary streams),
  where boundaries must be derived.
- `H264Parser::push_access_unit` — the container path (MP4, Matroska), where the
  container already knows where each sample begins and ends. Re-deriving it
  there would be wasted work *and* a chance to disagree with the container.

### The `parse` contract, which has one sharp edge

A call either **consumes everything it is given**, or **consumes nothing and
hands back a queued access unit**. A caller must therefore re-present the input
until it is consumed. `ParserDriver` does this for you; a hand-rolled loop must.

This is worth stating loudly because it is easy to get wrong: the first version
of this crate's own benchmark wrote `off += used.max(1)`, advancing past a byte
that had not been parsed, and measured **19.15 ms against 120 µs** on the same
megabyte. A 160x error from four characters.

Two design points behind it:

- **The parser owns its buffering.** `ParserDriver` discards whatever a parser
  declines to consume once end of stream is reached, so a parser that leaves its
  trailing NAL unit in the driver's buffer loses the last unit of *every file*.
  Hence: consume everything, buffer internally.
- **Queued units are handed back before more input is taken**, which is what
  bounds the buffer to one access unit rather than to the caller's push size.

## How to change it

- **Adding an SEI payload type.** One arm in `sei::decode_payload` and one
  variant in `SeiPayload`. Unrecognised payloads are already kept whole as
  `Other`, so nothing breaks by not adding one.
- **The scaling-list fall-back rules (§7.4.2.1.1.1, Tables 7-3 and 7-4) are
  deliberately absent.** Only a decoder needs the *effective* matrices; the raw
  lists and the `UseDefaultScalingMatrix` flags are stored, and deriving the
  rest would import two authorial-looking tables for no parsing benefit. Leave
  it that way unless a decoder lands.
- **MVC and 3D-AVC slice extensions (types 20, 21) are refused, not
  mis-parsed.** Annex G/H put a three-byte extension header in front of the
  slice header and change how the rest is read. `Error::Unsupported` beats a
  plausible wrong answer.
- **Every `ue(v)` must keep its bound.** The bound is the specification's own
  range constraint wherever it states one, and the comment says which clause.
  A bound removed is a fuzz finding waiting to happen.
- **Gotcha: `ChromaArrayType` is not `chroma_format_idc`.** With
  `separate_colour_plane_flag` the three 4:4:4 planes are coded as separate
  monochrome pictures and `ChromaArrayType` becomes 0. Use
  `Sps::chroma_array_type` for anything that affects decoding geometry (the crop
  unit, the pred-weight table) and `Sps::chroma_format` for anything that
  describes the *signal* (the pixel format).

## The three numbers that are easy to get wrong

### 1. Resolution — cropping, and the 1088 case

`pic_width_in_mbs_minus1` and `pic_height_in_map_units_minus1` give a
macroblock-aligned size. The displayed size subtracts the four
`frame_crop_*_offset` values, each scaled by a crop unit that depends on the
chroma format **and** on frame/field coding (§7.4.2.1.1):

| `ChromaArrayType` | `CropUnitX` | `CropUnitY` |
|---|---|---|
| 0 (monochrome) | 1 | `2 - frame_mbs_only_flag` |
| otherwise | `SubWidthC` | `SubHeightC × (2 - frame_mbs_only_flag)` |

So `frame_crop_bottom_offset = 4` removes 8 luma rows from a progressive 4:2:0
stream, 4 from 4:2:2 or 4:4:4, and 16 from an interlaced 4:2:0 one. The
canonical case: 1080 lines are coded as **1088** (68 macroblock rows) and
cropped by 8.

A crop that would leave zero or negative width or height is **rejected** —
`Sps::parse` returns `InvalidData`. The reference does the same, printing
`crop values invalid 0 320 0 4 / 640 368` and dropping the stream.

`Sps::coded_width`/`coded_height` still give the aligned numbers.
`// D17:` `CodecParameters`'s `coded_width`/`coded_height` are set **equal to
the cropped ones**, because that is what the reference prints: `ffprobe` on a
1918x1078 stream reports `coded_width=1918`, not 1920.

### 2. Frame rate — the factor of two

§E.2.1 defines a clock tick as `num_units_in_tick / time_scale` seconds and
makes one tick the distance between two consecutive **fields**. A frame is two
fields, so the picture rate is `time_scale / (2 × num_units_in_tick)`. A 24 fps
stream from `libx264` carries `num_units_in_tick = 1, time_scale = 48`.

`// D17:` the reference reports the **unhalved** rate. `ffprobe -f h264
-show_streams` prints `r_frame_rate=48/1` for that file, `50/1` for 25 fps and
`60000/1001` for 30000/1001. That is defensible rather than wrong —
`r_frame_rate` is documented as the lowest rate that can represent every
timestamp, and a field arrives at the tick rate — but it is not the frame rate.

Both are exposed: `VuiParameters::tick_rate` (what the reference prints, and
what goes into `CodecParameters`) and `Sps::frame_rate` (what §E.2.1 defines).
`tests/reference.rs` pins the exact factor-of-two relationship.

> Not the parser's, and worth knowing: `avg_frame_rate` in the raw Annex B path
> is the **demuxer's** `-framerate` option, defaulting to 25. It reads back as
> `25/1` for a 24 fps and a 30000/1001 stream alike, and changes to `30/1` if
> you pass `-framerate 30`. Nothing was parsed. (Plan 13 §1b rule 2.)

### 3. Pixel format

`// D17:` two deviations, both reproduced:

- **Monochrome is reported as 4:2:0.** `chroma_format_idc == 0` has no chroma
  arrays, so `gray` is the faithful answer; `ffmpeg 8.1` reports `yuv420p` — or
  `yuvj420p`, since `libx264` also sets the full-range flag for such a stream.
  The same answer comes back from an actual decode, so it is not a parse-only
  shortcut on the reference's part. `Sps::chroma_format` still says
  `Monochrome`.
- **The `yuvj` family is 8-bit only.** Full range at 8 bits gives
  `yuvj420p`/`yuvj422p`/`yuvj444p`; full range at 10 bits gives plain
  `yuv420p10le` with `color_range=pc` beside it.

High-bit-depth formats are reported **little-endian**. The reference reports the
*host's* endianness — `AV_PIX_FMT_YUV420P10` is a compile-time alias — so on a
big-endian host it would print `be`. Every target this project ships to is
little-endian and a `PixFmt` has to name one.

## Known deviations from the specification (D17)

Every one is annotated `// D17:` where it is implemented, with the probe that
established it.

| Deviation | Standard says | Reference does | Where |
|---|---|---|---|
| `chroma_location` with **no VUI at all** | inferred 0, i.e. `left` (§7.4.2.1.1) | `unspecified` | `Sps::color_info` |
| `profile_idc == 44` | *CAVLC 4:4:4 Intra profile* (A.2.11) | prints `CAVLC 4:4:4` | `profile::profile_name` |
| `profile_idc == 100` with `constraint_set4` | *Progressive High* (A.2.4.1); with `cs4`+`cs5`, *Constrained High* (A.2.4.2) | prints `High` for all three | `profile::profile_name` |
| Level 1b | `level_idc == 11` + `cs3`, or `level_idc == 9` (A.3.1) | prints `level_idc` verbatim — 11 stays 11 | `profile::level`, `is_level_1b` |
| Monochrome pixel format | 4:0:0 has no chroma | reports `yuv420p` | `params::pixel_format` |
| `r_frame_rate` | picture rate is `time_scale / (2·num_units_in_tick)` (§E.2.1) | reports the unhalved tick rate | `Sps::frame_rate` |
| `coded_width` / `coded_height` | (not a specification concept) | equal to the cropped size, not the macroblock-aligned one | `params::codec_parameters` |
| Sample aspect ratio | no rejection rule exists | reduced, then **discarded** when the scaled dimension truncates to zero | `params::sample_aspect_ratio` |

### The sample-aspect-ratio rejection rule

The least obvious of these, and fully measured. The reference reduces the ratio
and then requires the shorter axis, scaled by it, to stay above zero:

```text
scaled = num < den ? (width  * num) / den
                   : (height * den) / num      (truncating)
usable iff scaled > 0
```

On a 320x240 picture that admits everything from `1:320` to `240:1` and rejects
`1:321` and `241:1`. Nineteen rows either side of both boundaries were probed
and all nineteen agree, including that the reduction happens *first*: `480:2` is
accepted (it reduces to `240:1`) and `482:2` is not. Pinned in
`tests/reference.rs::the_sar_rejection_boundary`.

### What a parser structurally cannot reproduce

`has_b_frames` is `max_num_reorder_frames` from the VUI's
`bitstream_restriction`. The reference will **raise** it if it observes deeper
reordering while decoding — probed by patching the field to 0 in a stream that
does reorder, which then reports 1 — but raising it requires decoding and is
outside a parser's reach. Where the VUI states nothing, this crate reports 0.

## How the reference table was obtained

Nineteen H.264 streams generated with `ffmpeg 8.1` across resolution, chroma
format, bit depth, colour signalling, aspect ratio, frame/field coding, POC type
and profile; each stream's SPS NAL unit lifted out verbatim and embedded in
`tests/reference.rs`; each stream probed with:

```sh
ffprobe -v error -f h264 -show_entries \
  stream=width,height,coded_width,coded_height,sample_aspect_ratio,pix_fmt,\
profile,level,color_range,color_space,color_transfer,color_primaries,\
chroma_location,field_order,has_b_frames,r_frame_rate,bits_per_raw_sample \
  -of csv=p=0 sd.264
```

**Through `-f h264` on the raw Annex B demuxer** — one option, straight to the
parser, no container with its own opinion (plan 13 §1b rule 1). Probing the same
content inside MP4 gives `r_frame_rate=25/1` from the container's `stts` rather
than `48/1` from the VUI.

The profile-name and constraint-flag tables were recovered by patching
`profile_idc` (byte 5) and the constraint byte (byte 6) of a real stream and
reading `stream=profile` back — 30 combinations, all pinned in
`profile::tests::profile_names_match_the_probed_reference`.

Field values inside the bitstream were read with `-bsf:v trace_headers`, which
is black-box observation of a shipped binary's output and therefore compatible
with D6/D7. It is what established that `x264` writes `pic_struct = 3` for an
interlaced stream (hence `field_order=tt`), that the `bt709` test stream
actually carries `colour_primaries = 2` (so the `unknown` read-back is faithful
rather than a bug), and that `chroma_format_idc = 0` really is what the
monochrome stream contains.

**One probe went wrong and is worth recording.** Patching the Extended_SAR
fields to `0:1` writes `00 00 00 01` into the SPS — a start code — which
truncated the NAL unit. `ffprobe` then reported `1:128`, a number with no
relationship to anything. Verifying with `trace_headers` showed the parser never
saw the value. Plan 13 §1b's rule about the layer between you and the answer
applies to the *bytes you inject* as much as to the output you read.

## Safety on untrusted input

This crate parses fully untrusted data and is the classic decoder-vulnerability
surface.

- **Every `ue(v)` has a bound**, taken at the read site through
  `vaco_codec_golomb::BoundedGolomb`, which also charges fuel per syntax
  element. The bound is the specification's own range constraint wherever it
  states one.
- **Every count that sizes an allocation goes through `vaco_limits::Budget`**,
  and the loop is charged against fuel *before* it runs — so a declared count of
  four billion fails immediately rather than after four billion reads.
- **Every `do … while` has an explicit ceiling.** The two in the slice header
  (§7.3.3.1 `ref_pic_list_modification`, §7.3.3.3 `dec_ref_pic_marking`) and the
  two `ff_byte` runs in an SEI header (§7.3.2.3.1) are unbounded in the syntax
  and bounded here — `MAX_SYNTAX_COMMANDS` and `MAX_SEI_FF_BYTES` in `util`.
- **Picture order count is wrapping, not saturating.** §8.2.1 is written over
  unbounded integers; wrapping is panic-free *and* leaves the modular
  comparisons in §8.2.1.1 meaning what they mean. Saturating would silently
  reorder pictures instead.
- **The access-unit buffer is capped** at `DEFAULT_MAX_ACCESS_UNIT` (8 MiB), so
  a stream that never produces a boundary is refused rather than buffered.

## Configuration

No features and no environment variables. Two knobs:

- `H264Parser::new(limits)` — every allocation and every input-driven loop is
  bounded by this `Limits`.
- `H264Parser::with_max_access_unit(bytes)` — the per-access-unit ceiling,
  8 MiB by default.

## Dependencies

`vaco-bitstream` (reader), `vaco-codec-golomb` (bounded Exp-Golomb),
`vaco-format-nalu` (framing and RBSP extraction), `vaco-codec-core` (the
`Parser` trait, `CodecParameters`, the profile/level table types), `vaco-color`
and `vaco-pixfmt` (signalling enums), `vaco-limits` (`Budget`), `vaco-packet`
(emitted packets), `vaco-core` (errors, `Rational`). Dev only: `proptest`,
`divan`.

No external runtime dependencies. `#![forbid(unsafe_code)]`. Builds for
`wasm32-unknown-unknown` (D18).

## Testing

- `tests/reference.rs` — 19 real SPS units against every number `ffprobe 8.1`
  prints for them, plus the crop-unit matrix, the factor-of-two frame-rate
  relationship, the SAR rejection boundary and the SEI-derived field order.
- Unit tests per module, over fixtures taken byte-for-byte from real streams.
- `fuzz/fuzz_targets/parse_h264.rs` — the whole crate against arbitrary bytes,
  asserting chunk-invariance, that access units are a subsequence of the input,
  that a call always makes progress, and that derived geometry is
  self-consistent.
- `fuzz/fuzz_targets/limit_h264.rs` — the same input space under `Limits::tiny`,
  asserting that every failure is a typed error rather than a panic (plan 13
  §2.2.2).
- `benches/sps.rs` — parameter-set cost, and the whole-stream path against a
  scan-only floor.

### End-to-end, against five real files

Not a committed test — it needs media that is not in the repository — but the
check that closes the loop, and worth being able to repeat:

| file | `ffprobe -count_frames` | access units emitted | bytes accounted for |
|---|---|---|---|
| 640x360 @ 24 | 24 | 24 | 89 234 / 89 234 |
| 1920x1080 @ 25 | 25 | 25 | 694 154 / 694 154 |
| 1280x720 @ 30000/1001 | 30 | 30 | 378 757 / 378 757 |
| 720x480 interlaced | 30 | 30 | 140 412 / 140 412 |
| 320x240 Constrained Baseline, POC type 2 | 25 | 25 | 39 359 / 39 359 |

Identical at chunk sizes of one byte, 4 KiB and the whole file, with every byte
of every file landing in exactly one access unit, and with the reported
dimensions, frame rate, pixel format and field order matching the table in
`tests/reference.rs` — including `tt` for the interlaced stream, which comes
from the SEI rather than the SPS.

### What the fuzzer found

Three findings, all of them in the streaming path and none reachable by a
whole-buffer test:

1. **End of stream did not drain the buffer.** It finalised the trailing NAL
   unit and emitted everything left as *one* access unit, without first draining
   the boundaries the previous `parse` call had already found. Fed whole, a
   four-slice stream gave three access units; fed one byte at a time, four.
   Regression: `parser::tests::eos_drains_every_queued_access_unit`.
2. **A queued unit must be handed back before more input is taken**, or the
   access-unit buffer grows by one unit per push until it hits its cap.
   Regression: `parser::tests::queued_access_units_are_handed_back_before_more_input`.
3. Two further inputs that were the *target's* assertion being stale rather than
   the parser being wrong; both are kept in the corpus.

## Performance

Measured with `divan`, Apple M5, `aarch64-apple-darwin`, min of 100 samples.
Ratios rather than verdicts, per plan 12's PF-0.1 rule.

| Benchmark | Time |
|---|---|
| `sps_parse` (RBSP ready) | 135 ns |
| `sps_parse_with_deescape` | 204 ns |
| `pps_parse` | 47 ns |
| `derive_codec_parameters` | 73 ns |
| `scan_only`, 1 MiB | 68 µs (15.4 GB/s) |
| `parse_elementary_stream`, 1 MiB | 1.42 ms (741 MB/s) |
| `parse_chunked` 1 KiB / 4 KiB / 64 KiB | 1.40 / 1.38 / 1.41 ms |

Two things those numbers settled:

- **The bounded `ue(v)` family is not a problem.** A whole SPS — around forty
  syntax elements, each with a range check and a fuel charge — costs 135 ns.
  The concern that two layers of bookkeeping would dominate the reads was worth
  measuring and turned out to be unfounded, so the safe API stays.
- **A `Vec::drain` per access unit was quadratic in the push size.** Dropping a
  3 KiB access unit off the front of a megabyte buffer moves the remaining
  megabyte, once per unit. Replacing it with a read cursor and amortised
  compaction took `parse_elementary_stream` from **19.29 ms to 1.42 ms —
  13.6x** — and flattened the chunk-size curve, which had been the only visible
  symptom. A chunk-fed test never sees this; only the whole-buffer benchmark
  did.

The remaining 20x gap between `scan_only` and the full parse is the per-access-
unit work: RBSP extraction, two slice-header parses (§7.4.1.2.4 needs the next
slice's header to decide the current unit has ended), and a `Packet` allocation
and copy. Caching the boundary-decision parse is the obvious next move if this
ever matters.
