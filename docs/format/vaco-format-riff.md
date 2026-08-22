# `vaco-format-riff`

Layer 4. The RIFF chunk layer, and the two structures everything built on
RIFF needs: `WAVEFORMATEX`/`WAVEFORMATEXTENSIBLE` and `BITMAPINFOHEADER`.

This is **not** a demuxer and registers no component — `vaco-demux-wav` and
`vaco-demux-avi` are the eventual demuxers built on this crate, the way
`vaco-demux-mp4` is built on `vaco-format-isom`. It also unblocks
`vaco-demux-matroska`'s `V_MS/VFW/FOURCC` and `A_MS/ACM` tracks: a Matroska
`CodecPrivate` for either is exactly a `BITMAPINFOHEADER` or a `WAVEFORMATEX`
stored verbatim, with no RIFF chunk wrapper — which is why the two structures
parse from a plain byte slice rather than requiring a `fmt ` chunk to exist.

---

## What it is

| Module | Contents |
|---|---|
| `chunk` | `RIFF`/`LIST` chunk headers, flat iteration, word-alignment padding |
| `rf64` | the `RF64`/`ds64` 64-bit-size extension |
| `wave` | `WAVEFORMATEX` / `WAVEFORMATEXTENSIBLE` |
| `bitmapinfo` | `BITMAPINFOHEADER` and `biCompression` |
| `wave_tags` | `wFormatTag` → codec name / `CodecId` |
| `video_tags` | `biCompression` → codec name / `CodecId` |

Written from Microsoft/IBM's *Multimedia Programming Interface and Data
Specifications 1.0* (the RIFF/WAVE chunk grammar), the `WAVEFORMATEX`/
`WAVEFORMATEXTENSIBLE` definitions in `mmreg.h`/`ksmedia.h`, `BITMAPINFOHEADER`
in `wingdi.h`, RFC 2361 (registered `WAVE` format tags), and EBU Tech 3306
(`RF64`). No FFmpeg source was consulted (D7/D15); every codec-name spelling
below came from running `ffmpeg`/`ffprobe` 8.1, with the command recorded next
to it.

---

## How it works

### The chunk grammar

A RIFF file is a flat sequence of `ckID:4  ckSize:u32(LE)  ckData[ckSize]
[pad:u8 if ckSize is odd]` chunks. `RIFF` and `LIST` are not a separate
container grammar — they are ordinary chunks whose `ckData` happens to start
with a four-byte form/list type and continue with more chunks.
`chunk::ChunkIter` walks one container's direct children flat (mirroring
`vaco-format-isom::boxes::BoxIter`'s shape); `Chunk::children()` recurses one
level into a `RIFF`/`LIST` payload.

**Declared sizes are clamped, never trusted.** Unlike an ISOBMFF box size, a
RIFF `ckSize` is not something every writer gets right under all conditions —
a streaming WAV writer that does not know the final length up front commonly
writes `0xFFFFFFFF`, or simply the wrong value, for the outer `RIFF` size and
sometimes for `data`. `ChunkIter` treats a declared size that overruns the
container (including the all-ones convention) as "read to the end of what is
actually there" rather than an error, and reports it on `Chunk::truncated`.
This is deliberately more lenient than `vaco-format-isom`'s box header, which
rejects an overrun outright — the two formats' real-world writers disagree
about how strictly to keep that promise.

### `WAVEFORMATEX` / `WAVEFORMATEXTENSIBLE`

`WaveFormatEx::parse` accepts the 16-byte form (no `cbSize` at all), the
18-byte form (`cbSize = 0`), and anything longer (`cbSize` bytes of
codec-specific `extra` — an MS-ADPCM coefficient table, an IMA ADPCM
samples-per-block count, or a `WAVEFORMATEXTENSIBLE` tail). A declared
`cbSize` larger than what the buffer actually holds is clamped to what is
there, never trusted for an allocation.

`WaveFormatEx::extensible()` decodes the `WAVEFORMATEXTENSIBLE` tail when
`wFormatTag == 0xFFFE`, and `WaveFormatExtensible::sub_format_tag()` recovers
the *real* pre-extensible format tag from the `SubFormat` GUID's `Data1`
field, when the rest of the GUID matches the standard Microsoft media subtype
suffix (`-0000-0010-8000-00AA00389B71`). This matters more than it looks:
probed directly, `ffmpeg`'s `pcm_s24le` and `pcm_s32le` encoders write
`WAVEFORMATEXTENSIBLE` even for a single mono channel — only `pcm_u8`,
`pcm_s16le` and the IEEE-float encoders use the plain form. A parser that
assumes "tag 1 is always plain `WAVEFORMATEX`" misreads exactly the two most
common lossless-beyond-16-bit encodings.

### `BITMAPINFOHEADER`

`BitmapInfoHeader::parse` reads the classic 40-byte prefix every larger form
(`BITMAPV4HEADER`, `BITMAPV5HEADER`) shares, and ignores anything past it —
those extensions carry colour-mask/ICC-profile fields this crate does not
need to interpret a codec identity. `biCompression` is interpreted by
`Compression::from_u32`: the four `wingdi.h` reserved integers (`BI_RGB=0`,
`BI_RLE8=1`, `BI_RLE4=2`, `BI_BITFIELDS=3`) map to their own variants;
anything else that looks like printable ASCII becomes `Compression::FourCc`.

### The tag tables

Both `wave_tags` and `video_tags` split into two kinds of answer:

- **`codec_name`** — the exact `ffprobe` 8.1 `codec_name` string. Only ever
  populated for a tag this crate's author reproduced with a real `ffmpeg`
  encode; everything else is `None` rather than a guessed spelling.
- **`codec_id`** (both modules) — a best-effort `vaco_codec_core::CodecId`.
  `CodecId` is a small, hand-maintained enum that does not yet have a variant
  for most of what these tables name (no ADPCM, AC-3, WMA, MPEG-4 part 2,
  MS-MPEG4, Huffyuv, Cinepak or WMV variant exists in it today) — a tag with
  no representable `CodecId` maps to `None`, the same choice
  `vaco-format-isom::stsd::sample_entry_codec` already made for AC-3.
- **`wave_tags::tag_description`** — the MS/RFC 2361 registered name for a
  `wFormatTag`, independent of any tool's spelling. Deliberately a small,
  high-confidence subset of the registry rather than a full transcription of
  it (see *Fidelity* below); there is no equivalent table for video FourCCs,
  because a FourCC has no independent registry to draw a structural fact from
  the way RFC 2361 gives WAVE tags one.

`wave_tags::codec_name` takes the whole `WaveFormatEx`, not just the tag,
because the tag alone is not enough: `WAVE_FORMAT_PCM` and
`WAVE_FORMAT_IEEE_FLOAT` each cover several `codec_name`s depending on
`wBitsPerSample` (`pcm_u8`/`pcm_s16le`/`pcm_s24le`/`pcm_s32le`, and
`pcm_f32le`/`pcm_f64le`).

---

## Fidelity: what was measured against `ffprobe` 8.1, and what was not

Every entry below is the exact command used; re-run it against whatever
reference version is pinned to re-derive the table.

**Audio (`wave_tags::codec_name`)**, via
`ffmpeg -f lavfi -i sine=frequency=440:duration=0.2 -c:a <encoder> out.wav`
then `ffprobe -show_entries stream=codec_name,codec_tag`:

| encoder | `wFormatTag` | notes | `codec_name` |
|---|---|---|---|
| `pcm_u8` | 0x0001 | 8-bit | `pcm_u8` |
| `pcm_s16le` | 0x0001 | 16-bit, plain `WAVEFORMATEX` | `pcm_s16le` |
| `pcm_s24le` | 0x0001 | 24-bit, written as `WAVEFORMATEXTENSIBLE` | `pcm_s24le` |
| `pcm_s32le` | 0x0001 | 32-bit, written as `WAVEFORMATEXTENSIBLE` | `pcm_s32le` |
| `pcm_f32le` | 0x0003 | 32-bit float | `pcm_f32le` |
| `pcm_f64le` | 0x0003 | 64-bit float | `pcm_f64le` |
| `pcm_alaw` | 0x0006 | | `pcm_alaw` |
| `pcm_mulaw` | 0x0007 | | `pcm_mulaw` |
| `adpcm_ms` | 0x0002 | | `adpcm_ms` |
| `adpcm_ima_wav` | 0x0011 | | `adpcm_ima_wav` |
| `mp2` | 0x0050 | | `mp2` |
| `libmp3lame` | 0x0055 | | `mp3` |
| `wmav1` | 0x0160 | | `wmav1` |
| `wmav2` | 0x0161 | | `wmav2` |
| `ac3` | 0x2000 | `WAVE_FORMAT_DOLBY_AC3_SPDIF` | `ac3` |
| `aac` | 0x00FF | unofficial tag, universally used | `aac` |
| `g722` | 0x028F | not an RFC 2361 name; value confirmed by probe alone | `adpcm_g722` |

**Video (`video_tags::codec_name`)**, via
`ffmpeg -f lavfi -i testsrc=size=64x48:duration=0.2:rate=5 -c:v <encoder>
[-tag:v <fourcc>] -pix_fmt yuv420p out.avi` then
`ffprobe -show_entries stream=codec_name,codec_tag_string`:

| `biCompression` | encoder / `-tag:v` | `codec_name` |
|---|---|---|
| `H264`, `X264` | `libx264` | `h264` |
| `hvc1` | `libx265 -tag:v hvc1` | `hevc` |
| `VP80` | `libvpx` | `vp8` |
| `VP90` | `libvpx-vp9` | `vp9` |
| `MJPG` | `mjpeg` | `mjpeg` |
| `FFV1` | `ffv1` | `ffv1` |
| `HFYU` | `huffyuv` | `huffyuv` |
| `FMP4` | `mpeg4` (default tag) | `mpeg4` |
| `XVID` | `mpeg4 -tag:v XVID` | `mpeg4` |
| `DIVX` | `mpeg4 -tag:v DIVX` | `mpeg4` |
| `MP42` | `msmpeg4v2` | `msmpeg4v2` |
| `MP43`, `DIV3` | `msmpeg4v3` (default tag, and `-tag:v DIV3`) | `msmpeg4v3` |
| `MPNG` | `png` | `png` |
| `I420` | `rawvideo` (default tag) | `rawvideo` |
| `cvid` | `cinepak` | `cinepak` |
| `MSVC` | `msvideo1` | `msvideo1` |
| `WMV1` | `wmv1` | `wmv1` |
| `WMV2` | `wmv2` | `wmv2` |
| numeric `0` (`BI_RGB`) / `3` (`BI_BITFIELDS`) | `rawvideo` | `rawvideo` |
| numeric `1`/`2` (`BI_RLE8`/`BI_RLE4`) | `msrle` | `msrle` |

**Not verified against the reference — deliberately scoped out.** RFC 2361
lists roughly eighty `wFormatTag` values; `tag_description` covers a
high-confidence subset the author was independently sure of, rather than a
wholesale transcription from memory, per the standing warning against
exactly that mistake. Long-tail WAVE tags (`GSM610`, `TRUESPEECH`, the G.72x
ADPCM family, most vendor codes) and long-tail AVI FourCCs (Indeo, DV, the
Voxware/Truespeech family) resolve to `None` from both `codec_name` and
`codec_id` rather than a guess. A consumer that needs one of these should add
a probed entry, not assume the gap is an oversight.

---

## How to change it

- **Add a new WAVE tag or video FourCC**: add one match arm to
  `wave_tags::codec_name` / `video_tags::codec_name` (or `fourcc_codec_name`),
  backed by a probe command in a doc comment and a unit test that pins it.
  Never add an entry from memory alone — see *Fidelity* above.
- **Add a `CodecId` mapping**: only once the identity actually exists in
  `vaco_codec_core::CodecId` (owned by that crate, not this one). Until then,
  `None` is correct, not a placeholder to fill in later.
- **RF64**: `rf64::Ds64` only parses the `ds64` chunk. Deciding which chunk's
  declared size to override with which table entry is a demuxer's job — this
  crate does not resolve that itself, consistent with declared sizes being
  something it clamps rather than a promise it enforces (see `chunk`'s docs).
- **`BITMAPV4HEADER`/`BITMAPV5HEADER`**: `BitmapInfoHeader` parses only the
  classic 40-byte prefix. If a caller needs the colour-mask or ICC-profile
  extension, that is new, additive parsing behind `biSize`, not a change to
  the existing fields.

## Configuration

None. Every parse function takes a `vaco_limits::Budget` where it allocates
anything sized from input (`WaveFormatEx::parse`, `Ds64::parse`); the caller
chooses `Limits::permissive()` or `Limits::strict()` the same way every other
format crate does.

## Dependencies

`vaco-core` (errors), `vaco-bitstream` (`ByteReader`, the checked-tail byte
cursor), `vaco-limits` (`Budget`, for the two places this crate sizes a
buffer from a declared length), `vaco-codec-core` (`CodecId`, for the
best-effort mapping — permitted under D14.1 because it is the trait/identity
layer beneath `vaco-format-core`, not a codec implementation). No
`vaco-format-core` dependency: unlike `vaco-format-isom`, this crate does not
implement container probing, so it never needed `ProbeData`/`ProbeScore`.
