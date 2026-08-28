# `vaco-parse-audio-misc`

## What it is

Header parsing for Vorbis, FLAC and ALAC: the Vorbis identification header
(plus the Xiph header-packing convention non-Ogg containers use to carry it),
FLAC's `STREAMINFO` block, and ALAC's `ALACSpecificConfig` magic cookie.
**It does not decode.**

## How it works

| Module | Syntax | Registers |
|---|---|---|
| `vorbis` | identification header, `unpack_headers` | `PARSER_VORBIS` (`CodecId::Vorbis`) |
| `flac` | `STREAMINFO` | `PARSER_FLAC` (`CodecId::Flac`) |
| `alac` | `ALACSpecificConfig` | `PARSER_ALAC` (`CodecId::Alac`) |

Every field layout is verified against real bytes a real encoder wrote
(`ffmpeg -c:a vorbis`/`-c:a flac`/`-c:a alac`, inspected with a throwaway
Python script), not transcribed from specification prose — ALAC has no
published specification at all, so it is measured directly per D6/D7.

### All three `Parser`s share one contract: one call, one packet

Vorbis, FLAC and ALAC are, in every container that carries them, delivered
as discrete already-framed units — an Ogg packet, a Matroska block, an MP4
sample. None of the three `Parser`s here resynchronises a byte stream the
way `vaco-parse-mpegaudio`/`vaco-parse-aac`'s ADTS path does; each `parse`
call treats its whole input as one packet, exactly `vaco-parse-opus`'s
`OpusParser` contract.

**Named cut:** a native, non-containerized `.flac` elementary stream would
need its own frame-sync scanner (FLAC frames carry a sync code and CRC), and
none is built here — no demuxer in this tree reads a bare `.flac` file
today.

### Comment and picture parsing lives in `vaco-format-vorbiscomment`

Vorbis and FLAC's tag/picture metadata is `vaco-format-vorbiscomment`'s job
(`#540`), reused rather than re-parsed here — see that crate's docs for why.
`VorbisParser::comment()` is the one place this crate touches it: when
`set_extradata` is given the full Xiph-packed header blob (identification +
comment + setup), the comment packet is kept and parsed on demand through
`vaco_format_vorbiscomment::VorbisComment::parse_native`.

### The Xiph header-packing convention is measured, and known to be
### duplicated in `vaco-demux-ogg`

`vorbis::unpack_headers` (and the inverse packing it undoes) is the same
`[count-1][lace lengths...][packets...]` shape `vaco-demux-ogg::codec`
already implements as `pack_xiph_headers`/`split_xiph_headers` — both were
independently measured against the same real file and agree byte for byte.
It is not reused from there: a codec-level parse crate depending on a
specific container's demuxer crate inverts D14.1's intended dependency
direction (containers reach codec crates through the registry, not the
other way round), and `vaco-demux-ogg` is not this work's to edit. Recorded
as a known, small duplication rather than worked around — a future change
could have `vaco-demux-ogg` depend on this crate's copy instead, if whoever
owns it chooses to.

### Sample formats and bit depths, measured per codec

| Codec | `sample_fmt` | `bits_per_raw_sample` |
|---|---|---|
| Vorbis | `fltp` | not stated |
| FLAC | `s16` (not planar) | the `STREAMINFO` bit depth |
| ALAC | `s16p` | the magic cookie's `bitDepth` |

### ALAC's magic cookie: anchored on the end, not a fixed prefix

Different containers wrap the 24-byte `ALACSpecificConfig` in a different
amount of framing ahead of it — measured: MP4's nested `alac` box carries
`[size][fourcc][version+flags][24-byte config]` (36 bytes total). Rather than
branch per muxer, `AlacSpecificConfig::parse` reads the **last** 24 bytes of
whatever it is given, which handles a bare config, a version-plus-config
form, and the full boxed form uniformly.

### ALAC's `packet_duration` is a stream constant

ALAC frames state their own real sample count in-band (the last frame of a
stream is typically shorter), but reading it means parsing the frame's
Rice-coding parameters — decode-adjacent work this crate does not do. So
`AlacParser::packet_duration` always answers `frame_length / sample_rate`,
right for every frame except possibly the last, the same shape
`vaco-parse-aac`'s configured path already accepts for AAC.

## How to change it

If a new container wraps ALAC's magic cookie in yet another framing shape,
check whether "read the last 24 bytes" still handles it before adding a
branch.

## Configuration

None. `ParserDesc::make` takes only `vaco_limits::Limits`.

## Dependencies

`vaco-format-vorbiscomment` (for `VorbisParser::comment`), `vaco-codec-core`,
`vaco-chlayout`, `vaco-sampfmt`, `vaco-packet`, `vaco-limits`, `vaco-core`.

Fuzzed: `parse_audio_misc` covers all three header parsers and
`unpack_headers`.
