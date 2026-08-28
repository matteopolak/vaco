# `vaco-demux-avi`

Layer 4. The AVI demuxer: the `hdrl`/`strl`/`movi` walk, the `idx1`/OpenDML
index, and seeking. Built on `vaco-format-riff`, which owns the RIFF chunk
grammar and the `BITMAPINFOHEADER`/`WAVEFORMATEX` structures — this crate is
the AVI-specific walk on top of it, the way `vaco-demux-mp4` is the box walk
on top of `vaco-format-isom`.

---

## What it is

| Module | Contents |
|---|---|
| `hdrl` | `avih` (`AVIMAINHEADER`), `strh` (`AVISTREAMHEADER`), `strf`, `strn` |
| `index` | `idx1` (`AVIOLDINDEX`), OpenDML `indx`/`ix##`, the offset-ambiguity probe |
| `demux` | the `movi` walk, the per-stream clock, seeking |

Written from Microsoft's *AVI RIFF File Reference* (part of the Multimedia
Programming Interface and Data Specifications) and the *OpenDML AVI File
Format Extensions, v1.02* (published by the OpenDML committee). No FFmpeg
source was consulted (D7/D15); every field layout was measured against
`ffmpeg 8.1`'s own AVI muxer output, with the command recorded next to it.

---

## How it works

### AVI has no per-packet timestamp

This is the one fact that shapes the whole crate. A `movi` chunk is a tag,
a declared size, and payload — nothing else. A reader recovers a timestamp
from `strh.dwScale`/`dwRate` (the stream's time base) and `dwSampleSize`:

- `dwSampleSize == 0` (video; VBR audio): the *n*-th chunk of a stream has
  timestamp `n`.
- `dwSampleSize != 0` (CBR audio): the timestamp is the running **byte**
  count for that stream, divided by `dwSampleSize`.

`demux::AviDemuxer::read_one` runs this arithmetic while walking `movi`
sequentially; `index::build_from_idx1` runs the *identical* arithmetic while
replaying `idx1` at open time, which is why a well-formed file's packet
timestamps agree with what its own index implies.

### The `idx1` offset ambiguity — measured, not assumed

`idx1`'s `dwOffset` is documented as relative to the start of the `movi`
list's data. Real files also exist where it is relative to the start of the
file. Measured directly (`docs` trace below): `ffmpeg 8.1`'s own writer uses
the documented convention — `dwOffset` relative to the byte at which the
four-character `"movi"` text itself begins, *not* the chunk header before it
and not file byte zero.

```
$ ffmpeg -f lavfi -i testsrc=size=64x48:rate=10:duration=1 \
         -f lavfi -i sine=frequency=440:sample_rate=8000:duration=1 \
         -c:v mpeg4 -c:a pcm_s16le sample.avi
$ python3 - <<'EOF'
data = open('sample.avi', 'rb').read()
# movi LIST header at 9970; "movi" text at 9978; first child at 9982
# idx1 entry 0: ckid=b'00dc' dwOffset=4
# candidate A (movi text @9978 + 4) = 9982 -> data[9982:9986] == b'00dc'  MATCH
# candidate B (absolute 4)          = 4    -> data[4:8]     == garbage
EOF
```

`index::detect_offset_base` does not hard-code this — it probes the first few
entries against both candidates and adopts whichever one's bytes at the
computed position equal the entry's own `dwChunkId`, falling back to the
documented (movi-relative) convention only when neither can be confirmed
(a non-seekable source, or a corrupt entry).

### OpenDML (`indx`/`ix##`)

The >2 GiB extension: a `strl`'s `indx` (`AVISUPERINDEX`) names a sequence of
standalone `ix##` (`AVISTDINDEX`) chunks elsewhere in the file, each carrying
entries relative to its own `qwBaseOffset` — an absolute file offset, so this
level has no equivalent ambiguity. `index::parse_super_index` and
`index::parse_std_index` implement the byte layout and have unit tests
against hand-built bytes, but **nothing here has been exercised against a
real multi-gigabyte file** — building one to test against is not practical in
this environment. `index::resolve_opendml` only populates the keyframe map
(`pos -> is_key`); the seek-time timestamp index for OpenDML-only positions
is left to the generic `GENERIC_INDEX` fallback, because replaying OpenDML
entries into an accurate common timeline requires interleaving them with
`idx1`'s in true file order, which nothing here has verified.

### Why `open` is not a two-pass reader for every source

`idx1` sits *after* the `movi` data it describes. Reading it up front means
seeking past the entire media payload and back — only a seekable source can
do that. On a non-seekable source, `AviDemuxer::open_with_limits` opens with
no index at all and relies on `FormatFlags::GENERIC_INDEX` to build a coarse
one from whatever keyframes it happens to read, the same trade every other
index-based demuxer in this workspace makes (`vaco-demux-mpegts`,
`vaco-demux-matroska`).

### Seeking

`FormatFlags::NOBINSEARCH` is set deliberately: an AVI chunk's timestamp is a
running count since the start of its own stream, so landing on an arbitrary
byte offset tells a reader nothing about where in that count it is —
bisection cannot recover a timestamp the way it can for MPEG-TS, where every
packet states one directly. Seeking goes through the index (`idx1`/OpenDML
keyframe map, or the generic index if neither was available) or falls back to
`resync`, a forward byte scan for the next chunk whose tag parses as
`NN` + a two-character kind.

### `strf`'s trailing bytes are extradata for both audio and video

`hdrl::parse_strf`'s audio branch has always treated any bytes past
`WAVEFORMATEX`'s fixed fields as `CodecParameters::extradata` (MS-ADPCM
coefficients, AAC's `AudioSpecificConfig`). The video branch used to stop at
`BitmapInfoHeader`'s fixed 40 bytes and never look further — but a real
`avc1`/`hvc1`-tagged `strf` carries a configuration record there too
(measured: `ffmpeg -c copy -f avi` on an MP4 source writes a 45-byte
`AVCDecoderConfigurationRecord` immediately after the base header), and this
crate simply had no code path for it. `video_tags::carries_config_record`
gates the capture to the `FourCC`s that follow the ISO-BMFF convention
(`avc1`, `AVC1`, `hvc1`, `hev1`); `H264`/`X264`/`HEVC` and their aliases
carry Annex B in-band and have nothing after the header to capture. The
captured bytes reach `-show_streams`' `profile`/`level`/`pix_fmt`/`is_avc`/
`nal_length_size`/etc. entirely through the generic
`vaco-format-core::discovery` pipeline this crate already fed via
`extradata` for audio — no codec-specific parsing lives here; this module
only decides *that* the bytes are extradata, not what they mean.

### Measured field layouts worth recording

- `AVISTREAMHEADER.rcFrame` is four `i16`s (8 bytes), not the four `i32`s a
  Win32 `RECT` would suggest — this is why `strh` is 56 bytes, not 64.
  Confirmed byte-for-byte against `ffmpeg 8.1`'s own writer.
- `ffprobe 8.1 -show_streams` prints `id=N/A` for every AVI stream — unlike
  an MP4 track id or an MPEG-TS PID, there is no container-level identifier
  independent of stream order, so `Stream::id` stays `None`.
- RIFF `INFO` list tag keys, measured against `ffprobe`'s `format_tags`:
  `INAM`→`title`, `IART`→`artist`, `ICMT`→`comment`, `ICOP`→`copyright`,
  `IGNR`→`genre`, `ICRD`→`date`, `ISFT`→`software`. `strn` (a stream's own
  name) maps to the stream's `title` tag, also measured.

---

## What was exercised, and what was not

- **Exercised**: a two-stream (video + audio, `dwSampleSize` both zero and
  non-zero) file built by hand in `tests/roundtrip.rs`, covering stream
  discovery, packet order/timestamps/keyframe flags, the `idx1` offset
  detection against the measured movi-relative convention, index-based
  seeking, byte-seek resync, and (in `hdrl::tests`) `parse_strf` capturing
  an `avc1`-tagged `strf`'s trailing `avcC` bytes as extradata while leaving
  a plain `H264`-tagged one (Annex-B, no config record) alone. Verified end
  to end via `vaco-cli`'s own `-c copy -f avi` round trip against `ffmpeg
  8.1`'s output on an `avc1` MP4 source: every one of the eighteen
  previously-divergent `-show_streams` fields this gap caused now matches.
- **Structurally present, not exercised end-to-end**: OpenDML `indx`/`ix##`
  (unit-tested on synthetic bytes only — see above), multi-`RIFF` `AVIX`
  continuation chunks (not implemented at all: a second top-level `RIFF`
  chunk after the first one's declared end is skipped as an unrecognised
  chunk rather than treated as a `movi` continuation), `strd` (codec-specific
  extra data — parsed by nothing, silently skipped), palette-change chunks
  (`##pc`, recognised by `index::parse_chunk_tag` but not turned into any
  packet or side data).

---

## How to change it

- **Add a video/audio codec mapping**: this crate does not define its own
  tag tables — it calls `vaco_format_riff::video_tags`/`wave_tags`. Add the
  mapping there, backed by a probe, following that crate's own rules.
- **Change the clock**: `hdrl::StreamHeader::sample_size` (renamed from
  `dwSampleSize` for clarity) drives both `demux::AviDemuxer::read_one` and
  `index::build_from_idx1` — they must stay in lock-step, or a file's `idx1`
  will disagree with a linear read of the same file.
- **Support `AVIX` continuation**: `demux::AviDemuxer::open_with_limits`'s
  top-level scan loop would need to recognise a second `RIFF...AVIX` chunk
  after the first `RIFF`'s declared end, extend `movi_end`/`movi_children`
  tracking to span both, and merge their `idx1`s (`AVIX` chunks carry their
  own trailing index). Not attempted here for lack of a file to test against.
- **Resolve OpenDML into the timestamp index, not just the keyframe map**:
  `index::resolve_opendml`'s doc comment explains exactly what is missing —
  interleaving OpenDML entries with `idx1`'s in true file order.

## Configuration

None beyond the generic `vaco_format_core::FormatOptions` every demuxer
takes (`max_streams`, `indexmem` via `PacketIndex::with_options`,
`fflags`/`ignidx` via `vaco_format_core::seek::use_container_index`).

## Dependencies

`vaco-core`, `vaco-bitstream` (`ByteReader`, for parsing chunk payloads
already in memory), `vaco-io` (`IoContext`, `Seekability`), `vaco-limits`
(`Budget`), `vaco-packet`, `vaco-format-core` (`Demuxer`, `DemuxerDesc`,
`FormatFlags`, the seek machinery — `PacketIndex`/`IndexEntry`/
`SeekStrategy`), `vaco-format-riff` (the RIFF chunk grammar,
`BitmapInfoHeader`, `WaveFormatEx`, the codec tag tables), `vaco-codec-core`
(`CodecParameters`, `CodecId`). No `vaco-parse-*` dependency (D14.1) — this
crate never needs to look inside an elementary stream's own bitstream.
