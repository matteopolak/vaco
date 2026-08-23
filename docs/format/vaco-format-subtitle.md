# `vaco-format-subtitle`

Layer 4. Shared text-subtitle model: cue timing, text representation, and
per-format parse/serialise helpers. Consumed by `vaco-subtitle-text` (FM-34,
issue #591), which owns the demuxers and muxers themselves.

---

## What it is

Sixteen-plus text subtitle formats share exactly three hard problems —
representing a cue, parsing/printing its timestamp, and deciding what a line
of "text" actually is as bytes — and disagree about almost everything else.
This crate is that shared 20%. It has no `Demuxer`/`Muxer` implementations and
no dependency on `vaco-format-core`, `vaco-packet` or `vaco-codec-core` at
all — just `vaco-core`, for `Duration`. That is deliberate: a caller who only
wants to parse a timestamp string, or sniff a BOM, should not have to pull in
the container framework to do it.

## How it works

| Module | Contents |
|---|---|
| `cue` | [`Cue`] — `{ start: Duration, end: Duration, text: Vec<u8> }`, the one shape every format demuxes into |
| `time` | one parse/format pair per timestamp grammar, named for its format |
| `encoding` | BOM sniffing and the UTF-16→UTF-8 conversion the reference performs at demux time |
| `text` | byte-level line/block splitting that does not require the input to be valid UTF-8 |

### `Cue::text` is bytes, not a `String`

Measured against the reference (`ffprobe -f srt -show_packets -show_data`): a
`.srt` file with a raw, unlabelled `0xE9` byte in its text demuxes to a packet
carrying that exact byte, unmarked and unmodified. `String::from_utf8` would
reject the file outright and `from_utf8_lossy` would corrupt the byte into
`\u{FFFD}`; neither is what happens. `Cue::text` is `Vec<u8>` so the whole
crate can carry that byte through unchanged, and every parser in
`vaco-subtitle-text` finds cue *structure* (timestamps, counters, brace tags —
all pure ASCII) by scanning bytes or via a lossy `str` view used only for
locating structure, never for deciding what the payload bytes are.

### Timestamp grammars — the measured table

Every function in `time` is named for the one format it serves, on purpose: a
parser that quietly accepted the wrong punctuation for a format would happily
mis-time a whole file, which is worse than rejecting it. None of them guess
from string shape; the caller picks the function that matches the format it
is reading.

Measured against `ffmpeg 8.1` (`ffprobe -f <fmt> -show_packets`, reading
`pts_time`/`duration_time` off known inputs) rather than assumed from the
on-disk shape:

| Format | Field syntax | What it actually counts |
|---|---|---|
| SubRip | `HH:MM:SS,mmm` | milliseconds, comma mandatory |
| WebVTT | `HH:MM:SS.mmm` or `MM:SS.mmm` | milliseconds, period mandatory, hour optional |
| ASS/SSA | `H:MM:SS.cc` | **centi**seconds; one demuxer, one `codec_name=ass`, for both script versions |
| JACOsub | `H:MM:SS.hh` | centiseconds — same shape as ASS, a different format |
| MicroDVD | `{start}{end}` | **frame numbers**; default rate absent `{1}{1}<fps>` is **23.976 (24000/1001)**, not 25 |
| SubViewer | `HH:MM:SS.mmm,HH:MM:SS.mmm` | milliseconds |
| SubViewer 1.0 | `[HH:MM:SS]` | whole seconds, start-only |
| MPL2 | `[n][n]` | **tenths of a second** |
| PJS | `n,n,"text"` | tenths of a second |
| VPlayer | `HH:MM:SS:` | whole seconds, start-only |
| LRC | `[mm:ss.xx]` | hundredths, start-only, `mm` unbounded |
| Spruce STL | `HH:MM:SS:hh` | **hundredths**, despite the field's resemblance to an editing timecode's frame slot |
| SAMI | `Start=n` | milliseconds, start-only |
| RealText | `HH:MM:SS` | whole seconds; no `end=`/`dur=` → **60-second default duration**, not "borrow the next cue's start" |
| MPsub `FORMAT=TIME` | `gap duration` | seconds, **both relative to the previous cue's end** |
| SCC | `HH:MM:SS:FF` | frames at a flat 30000/1001, non-drop-frame (approximation — see `vaco-subtitle-text`'s `scc` module) |
| TTML | `HH:MM:SS(.fff)?` or `Ns`/`Nms` | clock-time or offset-time (spec; no reference demuxer exists — see `vaco-subtitle-text::ttml`) |

Two rows are the ones worth remembering if you only remember two: **MicroDVD's
default rate is 23.976, not 25**, and **MPsub's two fields are relative to the
previous cue's end, not to file start** — a file with two identical
`"1.0 2.0"` lines produces cues at `[1.0, 3.0]` and `[4.0, 6.0]`, not two
cues both at `[1.0, 3.0]`.

### Encoding — a BOM sniff, not a charset detector

Measured (same method): the reference strips a UTF-8 BOM, fully converts a
UTF-16LE/BE-BOM'd file to UTF-8, and otherwise **passes bytes through
completely unchanged** — no legacy single-byte charset auto-detection, no
substitution of invalid UTF-8, no rejection. `encoding::decode_to_utf8_bytes`
reproduces exactly that three-way branch and nothing more.

## How to change it

- **Adding a timestamp grammar**: write a new `parse_<format>_time` /
  `format_<format>_time` pair in `time.rs`, even if it shares a shape with an
  existing one (see `parse_jacosub_time`, which delegates to
  `parse_ass_time`'s logic but stays a separate function so a future change
  to one cannot silently reach the other). Add a round-trip `proptest`.
- **Changing `Cue`**: it is deliberately minimal. If a format needs to carry
  something `Cue` cannot express, that is usually a sign the field belongs in
  a side channel the *muxer* re-derives (as JACOsub cue numbering or SAMI
  &nbsp; filler would), not a reason to widen the shared type every format
  must satisfy.
- **`combine_hms`'s range check**: `m`/`s` are rejected outside `0..60`. If a
  future format's minutes/seconds genuinely exceed that range (unlikely — LRC
  and STL both already special-case the field that does), add a format-local
  parser rather than loosening the shared one.

## Configuration

None — every function here is pure and stateless. `MicroDVD`'s frame rate is
a parameter (`microdvd_frame_to_duration(frame, fps)`), not a global; the
caller (`vaco-subtitle-text::microdvd`) is where a per-file `{1}{1}<fps>`
header line is read and threaded through.

## Dependencies

`vaco-core` only, for `Duration`. `#![forbid(unsafe_code)]`, builds for
`wasm32-unknown-unknown` (no time/threading, no I/O — see
`cargo xtask wasm-check`).
