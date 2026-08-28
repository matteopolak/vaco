# `vaco-parse-mpegvideo`

Layer 4. MPEG-1/2 and MPEG-4 part 2 video **header** parsing, plus ATSC A/53
closed-caption extraction. No decode.

## What it is

Two header parsers behind the `vaco_codec_core::Parser` trait, and one
caption-extraction module:

| Module | Covers |
|---|---|
| `mpeg12` | MPEG-1/2 `sequence_header()`, `sequence_extension()`, `picture_header()`; access-unit boundaries; `Mpeg12Parser` |
| `mpeg4` | MPEG-4 part 2 `VisualObjectSequence`, `VideoObjectLayer`, `VideoObjectPlane`; `Mpeg4Parser` |
| `a53` | ATSC A/53 CEA-608/708 `cc_data` extraction from picture `user_data()` |

Parsing is not decoding — the line every `vaco-parse-*` crate draws (D5, D7):
header syntax and access-unit boundaries only, never a reconstructed sample.

## How it works

Both parsers are start-code driven, using `vaco_bitstream::annexb::find_start_code`
to locate `00 00 01` prefixes and dispatching on the byte that follows. Header
facts are folded into a `Sequence` as they are seen, so a stream that repeats
its sequence header mid-stream updates rather than restarts.

### `a53` — closed captions (interface gap 18)

MPEG-2 reaches CEA-608/708 captions by a genuinely different mechanism from
H.264/HEVC, which is why this module exists separately from the equivalents in
`vaco-parse-h264` and `vaco-parse-hevc`. There is no SEI and no T.35 country or
provider code: captions ride directly in a `user_data_start_code`
(`0x000001B2`) element whose payload begins at `user_data_identifier`.

```text
user_data() {
    user_data_start_code    32   0x000001B2
    user_data_identifier    32   0x47413934 'GA94'
    user_data_type_code      8   0x03  -> MPEG_cc_data()
    cc_data()                     ANSI/CTA-708 Table 2
    marker_bits              8   '1111 1111'
}
```

`iter_cc_data` walks a buffer and yields each caption element's `cc_count * 3`
triplet bytes — dropping the two-byte `cc_data()` header and the trailing
marker, so the result is exactly what `vaco-codec-subtitle-cc` decodes and what
`FrameSideData::ClosedCaptions` carries. `find_cc_data` returns just the first.
Non-caption user data (`DTG1` active-format description, bar data, a vendor
blob) is skipped silently, because a real stream is full of it.

Structure and constants come from ATSC A/53 Part 4:2009 §6.2.2, §6.2.3 and
§6.2.3.1, read from the standard. Verified against an
`ffmpeg -c:v mpeg2video -a53cc 1` transcode of a real broadcast capture: 120
frames, byte-for-byte identical to the reference's own `A53_CC` side data.

**No emulation-prevention unescaping happens here, and that is correct.** An
H.264/HEVC SEI payload has had its `emulation_prevention_three_byte`s removed
before the equivalent module sees it. MPEG-2 has no such escape — ITU-T H.262
§6.2.2.2.2 instead *forbids* user data from containing 23 or more consecutive
zero bits, so a start code cannot occur inside one. Scanning to the next start
code is exact, not approximate.

**The one caller contract:** consume captions in *presentation* order. CEA-608
is a stateful sequential command language, so concatenating decode-order
payloads interleaves and destroys it — and it fails silently, with zero parity
errors. Measured on the H.264 sibling path: `" its cities now."` becomes
`"    s  itesciti. now"`. Attach each payload to its own picture and let the
normal reorder carry it.

## How to change it

- A new MPEG-2 header field goes in `mpeg12.rs` beside the `Sequence` fold; add
  the start-code constant next to `PICTURE_START`/`SEQUENCE_HEADER` rather than
  inline, so the boundary logic keeps seeing one list.
- A new A/53 `user_data_type_code` (bar data is the obvious next one) goes in
  `a53.rs`: `UserDataIter` already yields every `user_data()` payload, so it is
  a second `cc_data_after_identifier`-shaped function, not a new scan.
- Gotcha: `find_start_code` returns the index of the `00 00 01` prefix, not of
  the start-code *value*, which is at `+3` with the payload at `+4`. Every call
  site here does that arithmetic with `saturating_add`; keep that, since the
  index comes from attacker-controlled data.

## Configuration

None. No options, features or environment variables.

## Dependencies

`vaco-bitstream` (reader and start-code primitive), `vaco-codec-core`
(`Parser`, `CodecParameters`), `vaco-pixfmt`, `vaco-limits` (budget),
`vaco-packet`. No external runtime dependencies.

## Safety on untrusted input

`#![forbid(unsafe_code)]`. The `a53` module allocates nothing at all: every
function returns a borrowed subslice, and `cc_count` is a 5-bit field so no
element can select more than 93 bytes. Fuzzed by
`fuzz/fuzz_targets/mpegvideo_a53_cc.rs`, which asserts termination (the scan
must advance past every start code it examines — a stall would hang rather than
crash, which no panic check would catch), whole-triplet lengths, and the 93-byte
bound. 5.7M executions, zero crashes.
