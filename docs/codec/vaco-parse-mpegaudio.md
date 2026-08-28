# `vaco-parse-mpegaudio`

## What it is

Byte-stream header parsing for MPEG-1/2/2.5 audio (Layer I/II/III — `mp1`,
`mp2`, `mp3`) and for AC-3/E-AC-3. Like `vaco-parse-aac` and `vaco-parse-opus`,
it splits a stream into access units and reports `CodecParameters`. **It does
not decode.**

## How it works

Neither format's frame syntax is re-derived here. `vaco-format-mpegaudio`
already parses the 4-byte MPEG audio header (`MpegAudioHeader`) for its
demuxer and decoder, and `vaco-format-ac3` already parses `syncinfo()`/`bsi()`
for the same reason (see D19: one definition per concept). This crate's own
job is the resynchronising `vaco_codec_core::Parser` loop on top:

| Module | Wraps | Registers |
|---|---|---|
| `mpegaudio` | `vaco_format_mpegaudio::MpegAudioHeader` | `PARSER_MPEGAUDIO` (`Mp3`, `Mp2`, `Mp1`) |
| `ac3` | `vaco_format_ac3::{syncinfo, bsi}` | `PARSER_AC3` (`Ac3`), `PARSER_EAC3` (`Eac3`) |

`MpegAudioParser` answers for all three MPEG audio codec identities from one
implementation: the frame syntax is identical across layers, and the `layer`
field in the header (not the requested `CodecId`) decides which codec a given
frame's `CodecParameters` names. A container that already stated its own
codec identity is unaffected — `CodecParameters::fill_from` only fills a field
the container left unset — so this only helps a container that does not know,
such as a raw `.mp3`/`.mp2` elementary stream.

`Ac3Parser` does the equivalent for `PARSER_AC3`/`PARSER_EAC3`: one
implementation, told apart per frame by `bsid` inside
`vaco_format_ac3::syncinfo::parse`, exactly as the reference reports two
different `codec_name`s for what is one syntax family.

### Resynchronisation

Both parsers follow `vaco-parse-aac`'s `AdtsParser` shape: while not known to
be in sync, a candidate frame is only accepted once a second sync word is
found exactly `frame_len()`/`frame_size` bytes later; once synced, frames are
taken as they come so the last frame of a file is still emitted via the
`parse(&[])` end-of-stream convention.

**A resync scan must search for the sync word one byte at a time, never as a
multi-byte window.** A fuzz-found regression: `Ac3Parser`'s scan originally
searched for the whole two-byte `0x0B77` pair with `windows(2)`, and a `parse`
call that saw only the pair's first byte (`0x0B`) at the very end of its input
reported "no match, consume it all" — discarding the byte and losing the
syncframe forever when the confirming `0x77` arrived in the next call. Fixed
by searching for `SYNCWORD[0]` alone, the same shape
`MpegAudioParser`/`AdtsParser` already use. `ac3_finds_a_frame_fed_one_byte_at_a_time`
pins it, and `parse_mpegaudio_stream` is the fuzz target that found it (drive a
byte-stream parser through `ParserDriver` one chunk at a time and compare
against feeding it whole).

### MP1/2/3 free-format frames are not supported

`bitrate_index == 0` ("free format") states no `frame_len()` — measuring one
means finding the *next* sync word, which this resync loop cannot do without
first assuming a length. Such a frame is treated as a sync failure. Named cut,
not silently wrong: no fixture reachable here exercises free-format MP3, and
the reference itself handles it by a different mechanism entirely.

### Channel layout and sample format

AC-3's layout comes from `vaco_format_ac3::tables::acmod_layout(acmod, lfeon)`
— already measured against real `ffmpeg -c:a ac3` encodes, including that
`acmod` 6/7 map to *side* channels (`5.1(side)`), not back. MPEG audio's comes
from `channels()` alone (mono/stereo — the format has no surround channel
mode). `sample_fmt` is `fltp` for both, measured with `ffprobe 8.1` against
real `libmp3lame`/`ac3`/`eac3` encodes — the decoder's output format, not
anything the bitstream states, the same convention `vaco-parse-aac` and
`vaco-parse-opus` document.

E-AC-3's `bit_rate` is computed from the frame's own size
(`frame_size * 8 * sample_rate / samples`) rather than looked up, because
`frmsiz` is a free byte count with no companion bit-rate table the way classic
AC-3's `frmsizecod` has.

## How to change it

* The frame-syntax tables (bit rates, sample rates, `bsi()` fields) belong in
  `vaco-format-mpegaudio`/`vaco-format-ac3`, not here — this crate only folds
  their output into `CodecParameters` and drives the byte-stream loop.
* If a new resync bug turns up, check whether the scan is byte-based or
  window-based first; see the fuzz-found regression above.

## Configuration

None. `ParserDesc::make` takes only `vaco_limits::Limits`, which bounds packet
allocation and the reassembly buffer via `vaco_codec_core::ParserDriver`.

## Dependencies

`vaco-format-mpegaudio` and `vaco-format-ac3` for the frame syntax;
`vaco-codec-core` for the `Parser`/`ParserDesc` seam; `vaco-chlayout`,
`vaco-sampfmt`, `vaco-packet`, `vaco-limits`, `vaco-bitstream`, `vaco-core`.

## What is left open

ADTS/LATM/`AudioSpecificConfig` — the other half of work package P-03 — ship
in `vaco-parse-aac`, already registered separately as `CodecId::Aac`/`AacLatm`.
