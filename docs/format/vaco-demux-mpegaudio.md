# `vaco-demux-mpegaudio`

Layer 4. The `mp3` demuxer: MPEG-1/2/2.5 Layer I/II/III elementary streams,
registered under the reference's own name `mp3` (long name `MP2/3 (MPEG
audio layer 2/3)`), with extensions `mp3`, `mp2`, `m2a`, `mpa`.

---

## What it is

| Module | Contents |
|---|---|
| `demux` | `MpegAudioDemuxer`: `ID3v2`/`ID3v1` handling, frame-by-frame packetisation, duration and gapless side data |
| `probe` | detection: chained, header-consistent frame syncs, not a single sync match |

---

## How it works

### Probing chains frames instead of trusting one sync

MPEG audio's sync is eleven set bits, which an enormous amount of non-audio
data matches by chance — a JPEG `APPn` marker (`0xFFEx`) passes on its own.
`probe::probe` never scores a single header: it walks forward at the exact
byte stride each header's own bit rate and sample rate imply, requires the
next header to agree on version/layer/sample rate, and only reaches the
reference's own measured score (`ffprobe -show_format` reports
`probe_score=51` on a real `ffmpeg`-encoded file, with or without a leading
`ID3v2` tag) after four such frames in a row. Fewer than two scores nothing.
Unit tests exercise this against prose, a synthetic AC-3 sync, a synthetic
MPEG-TS sync pattern, and a run of JPEG `APPn` markers — none of them chain.

### The Xing/Info/VBRI header frame is metadata, not audio

Measured against a real `ffmpeg -c:a libmp3lame` VBR file: `ffprobe
-show_packets` never lists the Xing frame as a packet — the first packet it
prints is the frame *after* it, at `pts=0`. This demuxer reproduces that:
`read_first_frame_tag` recognises the tag by position (right after the
Layer III side info, or at [`vaco_format_mpegaudio::vbri::FRAME_OFFSET`]) and
skips the whole frame before packetising. VBRI recognition is explicitly
limited to Layer III: its four-byte magic can occur in Layer I/II payload, but
must not turn a real non-MP3 audio frame into metadata.

### Gapless trim is derived from the LAME tag plus a fixed decoder delay

The LAME extension's encoder delay/padding are not what a real decoder
discards — measured against the same file, `ffprobe`'s
`skip_samples`/`discard_padding` on the first/last packet were the LAME
values **plus a fixed 529-sample decoder delay** (`576 + 529 = 1105`,
`1080 − 529 = 551`, both confirmed exactly). `DECODER_DELAY` in `demux.rs`
is that constant, applied in both directions.

### Free-format frame length is derived once, then held constant — and validated, not trusted on sight

`bitrate_index == 0` means the frame length isn't in any table; it has to be
found by scanning forward for the next sync word and measuring the gap. The
trap (issue #364): a false sync inside the payload — eleven set bits occur by
chance often enough in compressed audio — gives a plausible but wrong length,
and if that's accepted immediately it poisons every later frame, since a
free-format stream's length is otherwise constant for its whole duration.

`measure_free_format_len` fixes this by treating the first sync-looking match
as a *candidate*, not an answer: it parses a full header at that offset and
requires `version`/`layer`/`sample_rate_index`/`bitrate_index` to agree with
the frame being measured **and** for the claimed stride to reach the next
matching header. A final candidate may end at EOF, but a matching byte pattern
inside payload cannot establish its own continuation. A free-format stream's
frame length can vary by exactly one byte, the padding byte, so `padding_bit`
is intentionally excluded from the derived base length and re-added per frame.
Once derived, `Demuxer::free_format_len` caches the padding-exclusive base
length for the rest of the stream — later frames just add `padding_bit` rather
than re-scanning, which also means a false sync later in the stream can no
longer retroactively corrupt an already-derived length.

Verified against a hand-built fixture (real `ffmpeg` CBR output with every
frame's `bitrate_index` zeroed except the first, so the true frame length is
known and constant): decoded output is sample-count-exact and
correlation-identical (0.9921, matching the un-mutated CBR original) to
decoding the same audio at its real bitrate. No real free-format encoder was
available on this machine — `ffmpeg`/`lame` do not emit free-format MP3
without a patched build — so this is the only fixture provenance for this
path; see `docs/codec/vaco-codec-mpegaudio.md` for the full comparison table
and its provenance column.

### The stream time base is a fixed constant, not `1/sample_rate`

Measured at three different sample rates (32000/44100/48000 Hz): every one
reported `time_base=1/14112000`. `14112000` is the least common multiple of
the nine valid MPEG sample rates, so every one converts to a whole number of
ticks; `TIME_BASE` is hard-coded to it rather than derived per stream.

When a Xing or VBRI tag supplies a total frame count, the aggregate duration
is retained on that native clock through `duration_exact()`. The legacy
`duration()` API continues to expose its rounded microsecond value for
compatibility.

---

## Fidelity: what was measured against `ffprobe` 8.1, and what was not

| Field | How confirmed |
|---|---|
| `probe_score` | Exact match (`51`) on a real VBR file with and without a leading `ID3v2` tag |
| Packet `pos`/`pts`/first-packet skip, second/third packet sizes | Byte-for-byte match against `ffprobe -show_packets` on a real VBR file |
| `duration_ts`/`start_time` (from Xing frame count and LAME delay/padding) | Exact match against `ffprobe -show_streams` |
| Packet `duration` (the raw tick count `-show_packets` prints) | Exact: the demuxer retains native ticks with `Packet::set_duration_ts`, while `Packet::duration` remains the microsecond convenience view |
| `ID3v2`/`ID3v1` metadata merge | Structural; not fully cross-checked key-by-key against `ffprobe`'s `TAG:` output |
| Seeking | Byte-offset estimate from the average byte rate, then resync — **not** sample-accurate, and does not yet consult the Xing TOC |

## How to change it

- The Xing/VBRI detection and the ID3 handling are both in `demux.rs`;
  keep new duration/gapless logic next to `configure_stream`, which is
  where every derived `Stream` field is computed in one place.
- A more accurate seek needs the Xing TOC, currently parsed by
  `vaco-format-mpegaudio` but not consulted here.

## Configuration

Opens under `Budget::new(Limits::permissive())`, matching every other
format crate in the workspace; `FormatOptions` is accepted but not yet
read (the demuxer has no option-driven behaviour of its own).

## Dependencies

`vaco-format-mpegaudio` (frame headers, Xing/VBRI/LAME), `vaco-format-id3`
(`ID3v2`/`ID3v1`), `vaco-format-core`, `vaco-io`, `vaco-limits`,
`vaco-packet`, `vaco-codec-core`. No `vaco-codec-mpegaudio` — this crate
finds frame boundaries; it does not decode.
