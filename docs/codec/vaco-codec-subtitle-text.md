# `vaco-codec-subtitle-text`

Layer 4. Text subtitle decode: `SubRip`, ASS/SSA, `WebVTT`, 3GPP timed text
(`mov_text`), raw text and TTML.

## What it is

The markup half of six subtitle codecs. `vaco-subtitle-text` (the demuxer
side, `crates/format/`) turns a file into `vaco_format_subtitle::Cue`s — a
span of time and the bytes shown during it — and stops there, deliberately:
its own `Cue` docs say markup "is rendering, and rendering is a decoder's
job". This crate is that job.

**Everything here outputs ASS override markup, not plain text.** That is not a
design choice, it is what the reference does. Measured with
`ffmpeg -bitexact -i fixture -f ass -`: the `subrip`, `webvtt`, `mov_text` and
`text` decoders each emit an ASS `Dialogue:` line, so `<i>x</i>` in a `.srt`
comes back as `{\i1}x{\i0}` and a line break as `\N`. Six formats, one output
language.

## How it works

`decode(TextCodec, &[u8]) -> Option<String>` dispatches to one module per
format. Each is a small scanner; there is no shared "generic angle-bracket"
helper, because the formats disagree in ways that would make one:

| Module | Input | The thing that makes it its own module |
|---|---|---|
| `srt` | `SubRip` markup | entities **not** decoded; `</font>` closes in *opening* order |
| `webvtt` | `WebVTT` cue text | entities **are** decoded; `<v>`/`<c>`/`<ruby>` contribute text only |
| `movtext` | tx3g binary sample | binary; `styl` offsets are *character* offsets; spans close `{\r}` |
| `text` | raw bytes | line breaks only |
| `ass` | nine-field dialogue chunk | the shared output language; `parse_chunk` |
| `ttml` | TTML `<p>` inline content | XML; no reference decoder exists |

Three measured facts are worth carrying in your head, because a
from-first-principles implementation gets each one wrong:

- **`SubRip` and `WebVTT` disagree about entities.** `&amp;` survives verbatim
  through `subrip` and becomes `&` through `webvtt`. Implementing one from the
  other silently corrupts every ampersand in a subtitle file.
- **`WebVTT`'s `&nbsp;` becomes ASS `\h`, not U+00A0.** A *literal* U+00A0 in
  the payload is not converted — so the mapping belongs to the entity, not to
  escaping. Measured both ways.
- **`mov_text`'s `text_len` counts bytes but its `styl` offsets count
  characters** — and a character is a Unicode scalar, not a UTF-16 code unit.
  For `"😀 ital end"` the reference wrote `start_char = 2`; UTF-16 counting
  gives 3 and byte counting gives 5.

## How to change it

`ass.rs` owns the output language: `escape_plain` (line breaks to `\N`, braces
passed through unescaped) and `push_color` (RGB to ASS's blue-green-red order,
leading zeroes stripped). A change there moves every format at once, so it
wants the differential run below before and after.

Each format module owns its own tag table and its own tests. Adding a tag is a
match arm plus a fixture in `tests/differential.py`. `srt.rs`'s `attributes`
preserves source order because `</font>` closes in it.

`movtext.rs` is the only binary parser: bounds-checked accessors (`be16`,
`be32`) over a byte slice, never indexing. A declared `styl` entry count larger
than its box truncates at the last whole entry rather than trusting the count.

## Configuration

None. Every allocation is bounded by input length — the widest expansion is
`<i>` (3 bytes) to `{\i1}` (5), and `mov_text`'s text length is `u16`-bounded —
so neither `vaco-core` nor `vaco-limits` is a dependency and no `Budget` is
threaded. `tests/properties.rs` pins that bound as a property rather than
leaving it as a claim.

## Dependencies

`quick-xml` (workspace dependency, already used by `vaco-subtitle-text`) for
TTML only. Nothing else.

## Verification

### Differential against the reference — 25/25 exact

`tests/differential.py` is one loop over every fixture: it builds the exact
packet payload the reference's decoder would receive, runs it through both this
crate (`examples/decode_one.rs`) and `ffmpeg -f ass -`, and diffs the ASS text.

```sh
cargo build -p vaco-codec-subtitle-text --example decode_one
python3 crates/codec/vaco-codec-subtitle-text/tests/differential.py \
    target/debug/examples/decode_one
```

| Group | Fixtures | Match | Provenance |
|---|---|---|---|
| `subrip` | 10 | 10 | reference-generated |
| `webvtt` | 8 | 8 | reference-generated |
| `ass` | 2 | 2 | reference-generated |
| `mov_text` | 5 | 5 | reference-generated (real MP4 round trip) |
| `text` | — | — | **no reference path** — hand-built unit tests only |
| `ttml` | — | — | **no reference decoder exists** — hand-built, spec-derived |

The `mov_text` rows are real `tx3g` samples read back out of an MP4 the
reference wrote, split by `ffprobe` packet sizes — not hand-assembled bytes.

### Where the reference could not be used

- **TTML**: `ffmpeg -decoders` has no `ttml` row (`-h decoder=ttml` answers
  "known to FFmpeg, but no decoders for it are available") and `-demuxers` has
  none either — the reference ships a TTML *muxer* only. `ttml.rs` is
  implemented from the W3C TTML1 recommendation and is exactly as good as its
  own tests. `vaco-subtitle-text`'s own `ttml.rs` records the same finding for
  the demuxer side.
- **`text`**: there is no `text` demuxer to reach the decoder from a file, so
  the codec is covered by unit tests only. It is also the trivial case — line
  breaks and nothing else.

### Tests and fuzzing

59 tests: 52 unit, 7 `proptest` properties (no panic on arbitrary bytes for
every codec; plain text preserved; output bounded by input; no raw CR/LF
survives; dialogue-chunk round trip).

`subtitle_text_decode` fuzz target: **exit=0, execs=1,602,684**, and
`find fuzz/artifacts -type f` empty.

An earlier 45-second run **found a real bug**: the ASS path returned the
dialogue chunk's `Text` field verbatim, skipping the line-break escaping every
other codec applies, so a chunk carrying a raw CR produced output that would
serialise as two dialogue lines. Fixed; the input is kept as
`fuzz/seeds/subtitle_text_decode/regression-ass-raw-cr` and pinned by a unit
test.

## Not a `Decoder` implementation

`vaco_frame::FrameData::Subtitle` and `SubtitleContent::{Text, Ass}` fit these
formats exactly — `SubtitleContent::Ass` is the natural target for everything
this crate emits, and that type's own docs name ASS/SSA as its reason for
existing. This crate does not implement `vaco_codec_core::Decoder` against it
anyway, because at the time it was written that variant was **uncommitted work
in another agent's tree**, and a crate at `HEAD` calling into it would not build
for anyone else.

Wiring it is small and mechanical: each `to_ass` returns the `String` that
`SubtitleRect::ass(0, 0, 0, 0, false, …)` wants. See `planning/TECH-DEBT.md`.

## Known gaps

- **`WebVTT` character references are the pre-2015 six** (`&amp;`, `&lt;`,
  `&gt;`, `&lrm;`, `&rlm;`, `&nbsp;`). The current spec (§4.2.2, §6.4) delegates
  to HTML's full ~2,200-name table plus numeric references. **So does not the
  reference**: measured, `&quot;`, `&apos;`, `&hellip;`, `&#65;` and `&#x42;`
  all come back verbatim. Matching the reference and matching the current spec
  are different targets here, and this crate matches the reference.
- **`mov_text` UTF-16 text is not decoded.** TS 26.245 §5.1 allows a sample's
  text to be UTF-16 behind a BOM; the reference's own encoder writes UTF-8, so
  this was not reachable in testing.
- **`mov_text` modifier boxes other than `styl` are skipped** — `hlit`, `hclr`,
  `krok`, `dlay`, `href`, `tbox`, `blnk`, `twrp`, `disp`. Skipping unrecognised
  boxes is what TS 26.245 §5.17 tells a reader to do.
- **TTML referenced styles are not resolved** — only inline `tts:` attributes on
  a `<span>`, not `<style>`/`<region>` definitions elsewhere in the document,
  which a decoder handed one `<p>` does not have.
