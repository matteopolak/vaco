# `vaco-mux-avi`

Layer 4. The AVI muxer: `hdrl`/`strl` header, `movi` chunks, `idx1`. The
write-side companion to `vaco-demux-avi`, verified against it directly — this
crate's own tests mux a small file and demux it back with the sibling crate,
rather than hand-decoding bytes a second time.

---

## What it is

One file, `mux.rs`, implementing `vaco_format_core::Muxer` as `AviMuxer`.

---

## How it works

### Video sits on a fixed 600 Hz grid

AVI has no per-packet timestamp field. A frame's presentation time is its
ordinal position, so a video stream's `stream_time_base` is a fixed
`GRID_RATE` of `1/600` regardless of the source's own frame rate — measured
constant across every frame rate tried. `write_packet` places each real
video packet at the slot its (rescaled) `dts`/`pts` rounds to, and
`AviMuxer::backfill_grid_slots` fills every slot skipped since the last real
one with a zero-length placeholder chunk. `strh.dwLength`/`avih.dwTotalFrames`
fall out of this for free: a video `StreamOut::count` is the next unfilled
slot throughout, which is already the right value once the file is done.

Because AVI has no absolute-time field either, the very first video packet's
own timestamp becomes slot zero (`StreamOut::grid_origin`) — a source clock
that does not start there (routine for MPEG-TS) is rebased against it,
rather than pushing every slot out by however far that clock had already
run. The gap a video timestamp implies is attacker-controlled the ordinary
way (it comes straight from the input container), so `backfill_grid_slots`
checks it against a `Budget` before writing or indexing anything — see
`grid_budget`'s doc comment for the numbers.

The grid extends past the *last* real frame by that frame's own duration:
`write_packet` remembers the most recent video packet's `duration` (rescaled
into `GRID_RATE` ticks) as `StreamOut::last_video_duration_ticks`, and
`write_trailer` calls `backfill_trailing_video_slots` once, before `movi`
closes, to extend `count` from "one past the last real slot" to "one past
where that frame's own span ends" — backfilling the same zero-length
placeholder chunks as the inter-frame case. Without this, a video track's
final frame is on the grid but the time it occupies is not, and
`strh.dwLength` comes up one frame short of the reference on every fixture
tried (measured: two independent 25 fps sources, both exactly one frame
short — `600 / 25 = 24` grid ticks — before this existed).

Audio has no per-frame grid of its own: one chunk is one frame (VBR) or a
fixed number of samples (CBR PCM), and `write_packet` only needs packets to
arrive in the order the caller wants them replayed. There is one exception,
covered in *A compressed audio stream's second chunk can need a leading gap*
below.

### What gets patched, and how

`dwTotalFrames`/`dwLength` and `movi`'s own declared `LIST` size are not
known until every packet has been written:

- `hdrl` (`avih` + one `strl` per stream) is fully determined by `add_stream`
  alone, so it is built in an in-memory `Vec<u8>` first and written as one
  chunk — the only way to get its `LIST` size right without requiring the
  sink to seek. While building it, the exact byte offset of `dwTotalFrames`
  and each stream's `dwLength` is recorded.
- If the sink can seek, `write_trailer` goes back and patches those offsets,
  plus `movi`'s declared size and the outer `RIFF` size.
- If it cannot, they are left at placeholder values `vaco-format-riff`'s own
  chunk reader already documents as legitimate: `0` for frame counts (a real
  decoder discovers the truth from `idx1`/EOF), `0xFFFF_FFFF` ("length
  unknown, read to EOF") for `movi`'s size.
- `idx1` itself needs no seeking and is **always** written — nothing about
  appending it after `movi` depends on the sink's ability to go backwards.
  Its `dwOffset` is written movi-relative, the convention `vaco-demux-avi`
  measured `ffmpeg 8.1`'s own writer using.

### `dwMicroSecPerFrame` and `vprp` come from the source, not the grid

`avih.dwMicroSecPerFrame` tracks the *source* time base (`1e6 × num / den`,
truncating) rather than `GRID_RATE` — internally inconsistent with `strh`'s
own `dwRate`, but that is what a real writer states. The source time base is
only available through `Muxer::add_stream_with`'s `StreamSpec`, so
`AviMuxer` overrides that method to capture it into `StreamOut::source_time_base`;
a caller that drives `add_stream` directly (every test in this crate) never
supplies one, and the field stays `0`.

Each video `strl` also gets a `vprp` (`OpenDML` video-properties) chunk,
built from `CodecParameters` alone: dimensions, aspect ratio (`1:1` when the
source declared none), and a single progressive `VIDEO_FIELD_DESC`.
Interlaced sources get the same single-field shape, which is a known gap —
this crate has no interlace-aware `vprp` layout yet.

### H.264/HEVC keeps whatever framing its source used — this crate never reframes it

Measured directly against the reference (`ffmpeg -c copy -f avi` on an
`avc1`-tagged MP4 source and, separately, an Annex-B MPEG-TS source): AVI
mirrors the source's own framing rather than picking one. A length-prefixed
source (MP4's `avcC`/`hvcC`) stays length-prefixed, tagged `avc1`/`hvc1`,
with the source's own configuration record copied into `strf` verbatim after
`BITMAPINFOHEADER`. An Annex-B source (MPEG-TS, raw elementary streams) stays
Annex-B, tagged plain `H264`/`HEVC` — and its own start-code-prefixed SPS/PPS
extradata, when the demuxer supplied one, is copied into `strf` exactly the
same way. Neither the packet payload nor the extradata shape is ever
converted; `add_stream` only decides which `FourCC` family to write
(`video_fourcc`'s `length_prefixed` parameter) and whether `strf` gets an
extra blob (`StreamOut::video_extradata`).

The one case `add_stream` refuses outright: a length-prefixed stream with no
extradata at all. `avc1`/`hvc1` structurally promise a configuration record
right after `strf`'s fixed header, and writing the tag with nothing behind it
would be a container-level lie this crate can't back up. `H264`/`HEVC`
(Annex-B) makes no such promise, so the same situation there is not an error
— `strf` is just the classic 40-byte `BITMAPINFOHEADER` with nothing after
it.

An older version of this crate converted every length-prefixed H.264/HEVC
sample to Annex B before writing it, on the theory that AVI has no
out-of-band configuration record. That theory was wrong on both halves: AVI
*does* have somewhere to put one (`strf`, exactly like MP4's `stsd` sample
entry), and the reference never performs the conversion this crate used to.

**AAC arriving in ADTS framing (no `AudioSpecificConfig` in `extradata`) is
refused, not silently written**: AVI's `WAVE_FORMAT_AAC` entry expects raw,
config-out-of-band-framed AAC, and MPEG-TS's own AAC convention is ADTS
(config repeated per frame, no separate config blob) — measured directly
(`ffmpeg -i <mpegts-source> -c copy -f avi` refuses at `write_header` with
"ADTS is only supported with codec tag 0x1610"). `add_stream` refuses AAC
with empty/absent `extradata` for the same reason finding 19
(`planning/CONFORMANCE-FINDINGS.md`) named this "silent success": writing the
chunk anyway produces a technically-malformed audio stream no real AVI reader
expects.

**A packet with no PTS at all is refused, not silently muxed with a
`pts=dts=0` chunk timestamp** — also finding 19, measured the same way
(`ffmpeg -i <avi-source-with-no-pts> -c copy -f {mpegts,flv}` refuses; AVI
itself has no PTS field to lose, so this specific check lives in
`vaco-mux-mpegts`/`vaco-mux-flv` rather than here, but is documented in both
places since it is one finding).

### A compressed audio stream's second chunk can need a leading gap

Measured on a fixture with H.264 (default `libx264`, `has_b_frames = 2`) and
AAC audio whose first packet is the encoder's own priming frame (negative
`pts`): the reference interleaves `2^has_b_frames - 1` zero-length `01wb`
placeholder chunks immediately in front of the audio stream's *second* real
chunk — not immediately after the first. Skipping this is not cosmetic: the
audio bytes are identical either way, but decoding the reference's own AVI
output for a file shaped this way fails outright (`ffmpeg -f md5` reports
"Input buffer exhausted before END element found") when the gap is missing,
because it also shifts where the interleaver places every subsequent chunk.

`StreamOut::has_b_frames` (set from `VideoParameters::has_b_frames`, video
only) is read back by `AviMuxer::maybe_backfill_leading_audio_gap`, keyed off
`stream.count == 1` at the top of `write_packet` — i.e. right before the
second call for that (VBR, `sample_size == 0`) stream, before that packet's
own bytes go out. `2^n - 1` is confirmed at `n = 0, 1, 2` (0, 1 and 3 gap
chunks respectively, `ffprobe`'s own `has_b_frames` field capping at 2 for
every B-frame count `libx264` was asked for here); the shape beyond `n = 2`
is inferred, not independently measured. Candidate mechanisms tied to
audio's own timing were ruled out first: two audio-only fixtures with the
same one-frame encoder priming wrote no placeholders at all, and holding the
audio fixed while only changing the video's B-frame count changed the gap on
its own.

### Audio's own `AudioSpecificConfig`/config bytes reach `strf`

`WAVEFORMATEX`'s trailing `cbSize`-prefixed extension — AAC's raw
`AudioSpecificConfig`, mirroring what MP4's `esds` carries — is written from
`StreamOut::audio_extradata` (`CodecParameters::extradata`, captured at
`add_stream`) whenever present; absent, `strf` stays the classic 16-byte
`WAVEFORMATEX` with no `cbSize` at all. This was missing entirely until
measured: `add_stream` already refused ADTS-framed AAC (no extradata) but
never actually wrote the extradata it required for the case it accepted, so
every AAC-in-AVI stream this crate produced decoded with "channel element ...
is not allocated" — the *config*, not the payload, was missing (payload bytes
were already correct; a decoder with no object type or channel configuration
simply can't parse them).

### Three `JUNK` reservations are two inert index structures and one true pad

Measured across four fixtures (H.264 in both `avc1` and Annex-B framing,
with and without AAC audio, and PCM-only) that varied stream count, codec,
and every `strf` size from 16 to 86 bytes: `hdrl` reserves a `JUNK` chunk
after every `strl`'s own content (`STRL_JUNK_LEN`, 4120 bytes, between
`strf` and `vprp` for a video stream — not after `vprp`), one more after the
last `strl` (`HDRL_JUNK`, 260 bytes), and the top RIFF level reserves one
more between `hdrl` and `movi` (`RIFF_JUNK`, 1016 bytes). All three sizes
are identical on every fixture regardless of content, so they are sized as
literal constants, not derived from anything — no reader depends on the
exact byte count, since `JUNK` is skipped identically regardless of length.

The *bytes inside* two of the three are not simply zero, though — each
reserves room for a structure the reference never actually activates,
tagging it `JUNK` instead of the real chunk id rather than leaving the
space out:

- The per-`strl` one (`build_strl_junk`) is an `AVISUPERINDEX` header:
  `wLongsPerEntry = 4`, `nEntriesInUse = 0`, and this stream's own
  `dwChunkId` (`00dc`, `01wb`, …) — confirmed on four separate stream
  instances across the four fixtures, all with `nEntriesInUse = 0` and the
  remaining ~4096 bytes of reserved entry space genuinely zero.
- The `hdrl`-level one (`HDRL_JUNK`) is `LIST 'odml'` holding one `dmlh`
  (`AVIEXTHEADER`) chunk declared 248 bytes, `dwGrandFrames` and every other
  field left `0` regardless of the file's real frame count.
- The RIFF-level one (`RIFF_JUNK`) measured genuinely all zero on every
  fixture — no header of any kind.

### Several more `avih`/`strh`/`strf` fields turned out to be measurable

Comparing whole files byte for byte against the reference (not just sizes)
surfaced these, all confirmed on the same four fixtures unless noted:

- `avih.dwFlags` includes `AVIF_TRUSTCKTYPE` (`0x800`) alongside the two
  flags this crate's own demuxer reads.
- `avih.dwSuggestedBufferSize` is a fixed `1_048_576` (1 MiB), unrelated to
  any stream's actual chunk sizes.
- `avih.dwMaxBytesPerSec` is the sum of every stream's own
  `CodecParameters::bit_rate`, divided by 8 and truncated; a stream with no
  declared bit rate contributes `0`.
- `avih.dwTotalFrames` is the *video* stream's own count specifically —
  measured on a PCM-only (no video) fixture, where it stays `0` rather than
  falling back to the audio stream's own sample count.
- `strh.dwSuggestedBufferSize` (per stream, not `avih`'s file-level one) is
  the largest single chunk that stream actually wrote — tracked as
  `StreamOut::max_chunk_size` and patched at `write_trailer` the same way
  `dwLength` is. Confirmed on both video and audio (five independent values
  across the four fixtures, all exact).
- `strh.rcFrame` is `{0, 0, width, height}` for a video stream, not all
  zero; `{0, 0, 0, 0}` for audio, which has no frame rectangle.
- An audio stream's `strh.fccHandler` is the raw `u32` value `1`
  (`WAVE_FORMAT_PCM`'s own tag number) regardless of the stream's actual
  `wFormatTag` — an AAC-tagged stream measured the same `1` a PCM stream did.
- `strf.biSizeImage` (video) is `width * height * 3` — the raw-RGB byte
  count the header's own `biBitCount = 24` implies — even though the actual
  codec is compressed. Confirmed identical on `avc1` and Annex-B `H264`
  alike, so it tracks the declared bit count, not the real sample size.
- A *compressed* (VBR) audio stream's `strh.dwScale`/`dwRate` is one
  **frame's** duration, not one sample's: an AAC stream at 44100 Hz reduces
  to `256/11025` (`1024/44100`, AAC-LC's fixed frame size), not `1/44100`.
  This is a *different* field from `Muxer::stream_time_base`
  (`StreamOut::time_base`, still per-sample) — widening the interleaving
  time base to match broke real packet interleave order the first time this
  was tried, caught only by comparing muxed bytes against the reference, not
  by this crate's own tests, since nothing here reads audio timestamps
  itself. `StreamOut::strh_time_base` is the separate field that exists
  because of that finding. `strf`'s own `nSamplesPerSec` stays the true
  sample rate throughout, from a new `StreamOut::sample_rate` field, since
  it means something different from `strh`'s time base.
- A compressed stream's `strf.nAvgBytesPerSec` is the same `bit_rate / 8`
  `avih.dwMaxBytesPerSec` uses, applied per stream rather than summed.
- **Not resolved:** `strf.nBlockAlign` for a compressed stream. The
  reference's own AAC fixture measured `3`, which matches none of
  `bytes_per_sample × channels`, the sample rate, the bit rate, or the
  channel count in any combination tried, and a second fixture was not
  available to narrow it further — see `write_strl`'s comment beside the
  field. This is the one remaining byte-level difference against the
  reference on the one two-stream fixture available (2 bytes out of 39304).

### Codec support

Video: H.264, HEVC, VP8, VP9, MJPEG, PNG — the codecs
`vaco_codec_core::CodecId` has a variant for *and* this crate has a
`biCompression` FourCC for (`video_fourcc`). Audio: the generic `Pcm` bucket
plus every little-endian PCM flavour (`PcmU8`, `PcmS16le`, `PcmS24le`,
`PcmS32le`, `PcmF32le`, `PcmF64le`, `PcmAlaw`, `PcmMulaw`) — all written with
`dwSampleSize` set to the block size, the only case AVI can express as
constant-bitrate, and with `wBitsPerSample` forced to the width the specific
flavour implies rather than trusted from the caller (see `pcm_bits_per_sample`
in `src/mux.rs`) — plus MP3 and AAC (both variable-bitrate; one chunk per
frame). The big-endian PCM flavours have no mapping and are refused, same as
any other codec `add_stream` does not recognize: a `WAVEFORMATEX` in a RIFF
file is little-endian by definition, so there is nothing correct to write.
Anything unmapped is `Error::Unsupported` from `add_stream` — never a guessed
tag a reader would misidentify.

Note: `vaco-format-riff`'s `wave_tags::codec_id` (which `vaco-demux-avi`
reuses on the read side) resolves a parsed `WAVEFORMATEX` to the *specific*
flavour, never the generic `Pcm` bucket — so a stream added here as
`CodecId::Pcm` demuxes back as e.g. `CodecId::PcmS16le`. That is intentional
and is exactly what `tests/roundtrip.rs` pins.

---

## What was exercised, and what was not

- **Exercised** (`tests/roundtrip.rs`, which demuxes with `vaco-demux-avi`
  and inspects raw bytes directly for fields the demuxer does not model):
  a two-stream (H.264 video + PCM audio) file, packet order and per-packet
  keyframe/PTS derivation on the read side, the seekable trailer-patch path,
  the 600 Hz grid's placeholder backfill and origin rebasing, an implausible
  grid gap being rejected, `dwMicroSecPerFrame` when driven through
  `MuxBuilder`, length-prefixed vs. Annex-B framing preservation with and
  without a configuration record, every fixed-constant/summed/tracked
  `avih`/`strh`/`strf` field above including the `JUNK` reservations'
  content, and the audio `fccHandler` placeholder. Verified end to end
  against `ffmpeg 8.1 -c copy -f avi` on four fixtures (an MP4 with
  B-frame-free H.264+AAC, a video-only `avc1` MP4, an MPEG-TS with B-frames,
  Annex-B framing and a non-zero start time, and a PCM-only WAV): output is
  **byte-identical** to the reference on three of the four. On the fourth
  (the two-stream H.264+AAC fixture) exactly 2 of 39304 bytes differ —
  `strf.nBlockAlign`, the one field above marked not resolved. Decoded video
  and audio both match the reference's own AVI output exactly, per stream,
  on every fixture with audio, including that one.
- **Not exercised**: the non-seekable-sink path (placeholder `0`/
  `0xFFFF_FFFF` values never actually read back by anything in this crate's
  tests), more than two streams, `strn` or `LIST INFO` metadata output (this
  crate does not write either — see below), interlaced `vprp`, the
  leading-audio-gap formula beyond `has_b_frames = 2`, and `strf.nBlockAlign`
  for a compressed stream (see above — both are genuinely open, not merely
  untested).

## How to change it

- **Add a codec**: extend `video_fourcc`/`audio_format_tag`. Only add an
  entry that `vaco-format-riff`'s read-side tables (`video_tags`,
  `wave_tags`) would map back to the *same* `CodecId` — writing a tag this
  project's own demuxer cannot demux again is worse than refusing to.
- **Write `strn`/`LIST INFO` metadata**: not implemented. `add_stream` only
  sees `CodecParameters`, which carries no stream metadata; threading title/
  artist/etc. through would need a signature change this crate's frozen
  `Muxer` trait does not have room for today (see `vaco-format-core`'s docs
  on the same gap for `vacoraw`).
- **Width/height for video `strf`**: read from `CodecParameters.video`
  directly in `add_stream` — already wired. If a caller passes a video
  stream with `width`/`height` still `0`, this crate does not reject it; the
  demuxer will simply read back `0x0`.
- **Add another codec's extradata to `strf`**: `video_fourcc`'s
  `length_prefixed` parameter and `StreamOut::video_extradata` are the two
  places that matter. A codec whose AVI convention carries a configuration
  record needs a `length_prefixed`-style tag split like H.264/HEVC's; one
  that never carries anything extra (VP8, VP9, MJPEG, PNG today) needs
  neither.

## Configuration

None beyond `vaco_format_core::FormatOptions`, accepted but currently unused
by this muxer (no per-file option changes its output).

## Dependencies

`vaco-core`, `vaco-io` (`IoWriter`, `MediaSink`), `vaco-packet`,
`vaco-format-core` (`Muxer`, `MuxerDesc`), `vaco-codec-core`
(`CodecParameters`, `CodecId`), `vaco-limits` (`Budget`, for the grid and
leading-audio-gap backfills' bounds). `vaco-format-nalu` is no longer a
dependency: it was added only for the length-prefixed-to-Annex-B conversion,
which this crate no longer performs (see above). Dev-only: `vaco-demux-avi`,
to verify this crate's own output demuxes as intended.

## Tests and fuzzing

`tests/roundtrip.rs` covers: length-prefixed framing staying length-prefixed
with `avc1`/`avcC`
(`a_length_prefixed_h264_sample_keeps_its_framing_and_gets_avc1_avcc`);
Annex-B framing staying Annex-B with no config record
(`an_annex_b_h264_sample_keeps_h264_and_gets_no_config_record`);
`check_bitstream` never requesting a bitstream filter
(`check_bitstream_never_requests_a_filter_through_mux_writer`, driven
through `MuxBuilder` with a `BsfProvider` that refuses every filter name);
the fixed `avih` constants and JUNK reservation sizes
(`avih_flags_suggested_buffer_and_junk_reservations_match_the_measured_constants`);
`strh.dwSuggestedBufferSize` tracking the largest chunk actually written and
`avih.dwMaxBytesPerSec` summing declared bit rates
(`strh_suggested_buffer_is_the_largest_chunk_and_avih_sums_bit_rates`); the
audio `fccHandler` placeholder
(`audio_fcc_handler_is_the_fixed_value_one_not_the_format_tag`); and the
finding-19 refusals (`adts_framed_aac_with_no_extradata_is_rejected`,
`raw_aac_with_extradata_is_accepted`), alongside the pre-existing shape/
order/trailer-patch/grid tests.

`fuzz/fuzz_targets/avi_mux_packet.rs` no longer fuzzes a framing conversion
(there is none left to fuzz) — it exercises `write_packet` over an arbitrary
payload with the 600 Hz grid's inter-packet backfill (a `pts`/`dts` gap
bounded to `MAX_GRID_GAP` slots, so one iteration cannot spend its time
writing an unbounded run of placeholder chunks) and, when the fuzzer's flag
byte asks for a length-prefixed stream, a fixed non-empty `extradata` blob
just large enough to satisfy `add_stream`'s requirement and exercise the
`strf`/`video_extradata` write path — the blob's own internal structure is
not this target's concern (that is `vaco-parse-h264`'s, and, for the
container framing, `avi_demux`'s whole-file fuzzing). Output growth is
asserted to stay within payload length plus a small fixed overhead, since
nothing amplifies a payload here any more. 30-second run: `exit=0`,
`execs≈1,452,247` (varies run to run), `find fuzz/artifacts -type f` empty.
The budget-rejection path for a genuinely implausible gap is covered by a
unit test instead (`an_implausible_grid_gap_is_rejected_not_looped_forever`),
where an exact input is worth more than a slow corpus entry.

`just fuzz <target>` (no `secs` argument) runs with **no time limit** —
`justfile`'s `fuzz` recipe passes none to `cargo fuzz run`, unlike `fuzz-all`
— so it must be invoked directly with `-- -max_total_time=30` (or killed
manually) rather than left to `just fuzz`'s own default. Found the hard way
this session: an unbounded run sat at 100% CPU for several minutes before
being noticed and killed.

A stale `header_budget` in this target (predating the three `JUNK`
reservations) caused a real false-positive: its output-growth assertion
failed on a 3-byte input within the first second of fuzzing once those
reservations existed, since the assumed bound no longer covered the header's
own fixed size. Not a muxer bug — the assertion was simply out of date —
fixed by recomputing the bound, and the 3-byte reproducer is kept at
`fuzz/seeds/avi_mux_packet/crash-header-budget-underestimate` as a
regression seed.
