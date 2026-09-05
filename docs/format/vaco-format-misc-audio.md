# `vaco-format-misc-audio`

Layer 4. Demux-only: `wv`, `tta`, `amr`/`amrnb`/`amrwb`, `adx`,
`nistsphere`, `pvf`, `g723_1`, `sbc`, the headerless ITU-T/3GPP2
speech-codec tail (`gsm`, `sln`, `dfpwm`, `g722`, `g726`, `g726le`, `g728`,
`g729`, `aptx`, `aptx_hd`), and — added across two later passes at #620's
chiptune-adjacent game-audio containers — `vag`, `svag`, `xwma`, `xa`, and
bounded `bfstm`/`brstm` subsets, plus bounded XM, ProTracker MOD, and FSB4/FSB5
structural readers. Thirty registered demuxers in one crate (FM-58).
These are
containers: the job is finding frame/block boundaries and reporting stream
parameters, not decoding audio.

---

## What it is

| Module | Formats | Framing |
|---|---|---|
| `wavpack` | `wv` | 32-byte little-endian block header states the next block's byte offset directly (`ckSize + 8`); no decode needed to walk the chain |
| `tta` | `tta` | fixed 22-byte header + a seek table of per-frame byte lengths |
| `amr` | `amr`, `amrnb`, `amrwb` | one TOC byte per frame; its 4-bit mode field indexes a fixed payload-size table |
| `adx` | `adx` | fixed-field header states `block_size` and `copyright_offset` (→ header length); data is then constant-size ADPCM blocks |
| `nistsphere` | `sph`, `nist` | ASCII `key -type value` header of a stated total length, then raw PCM |
| `pvf` | `pvf` | one text line (`channels rate bits`), then raw big-endian PCM |
| `g723` | `g723_1` | headerless; each frame's first two bits select one of four fixed sizes (24/20/4/1 bytes) |
| `sbc` | `sbc` | headerless; each frame's own header states blocks/subbands/bitpool, which a published formula turns into the frame's exact byte length |
| `rawcodec` | `gsm`, `sln`, `dfpwm`, `g722`, `g726`, `g726le`, `g728`, `g729`, `aptx`, `aptx_hd` | headerless, constant bytes-per-block : frames-per-block ratio, fixed by the codec's own bitrate |
| `vag` | `vag` | fixed 48-byte header (`VAGp` magic, big-endian `data_size`/`sample_rate`), then 16-byte PS-ADPCM blocks; a bespoke, non-`BlockDemuxer` loop — see below |
| `svag` | `svag` | 20-byte consumed header (`VAGm` magic plus little-endian `data_size`/`sample_rate`/`channels`/`interleave`), then interleaved PS-ADPCM; reads physical EOF in `channels * interleave` byte packets while the declared size controls duration only — see below |
| `bfstm` | `bfstm`, `bcstm` | Nintendo `FSTM`/`CSTM` with either byte order and `INFO`/`SEEK`/`DATA` sections, measured only for stereo DSP-ADPCM with 16/32/64/96/256-byte channel blocks and full/half final blocks; packets synthesize file-endian raw-byte/sample counts, two coefficient sets, and an 8-byte SEEK history entry before unpadded channel payload — see below |
| `brstm` | `brstm` | Nintendo `RSTM`/`HEAD`/`ADPC`/`DATA`, measured only for stereo DSP-ADPCM with 32/64/96/256-byte channel blocks and full/half final blocks; packets synthesize `be32(raw bytes)`/`be32(samples)`, two 32-byte coefficient sets, and one 8-byte ADPC history entry before the unpadded channel payload — see below |
| `xwma` | `xwma` | RIFF container (`fmt `/`dpds`/`data` chunks); packets are `nBlockAlign`-aligned reads of `data`, not the `dpds` table's declared split — see below |
| `xm` | `xm` | FastTracker 2 v1.04 little-endian module header, pattern blocks, instrument/sample headers, and one packet per non-empty sample payload; structural only — no tracker playback or sample decoding |
| `protracker` | `mod` | ProTracker four-channel `M.K.` header, order table, fixed 64-row pattern blocks, and one packet per non-empty signed 8-bit sample payload; structural only |
| `fsb` | `fsb` | FMOD FSB4/FSB5 sample-bank headers and bounded sample entries; one stream per entry and stored payload packets, with FSB4 limited to measured Nintendo THP and FSB5 to PCM8/PCM16/PCM32/MPEG/Vorbis |
| `xa` | `xa` | fixed 24-byte header (2-byte `"XA"` magic, little-endian `WAVEFORMATEX` tail), then EA-ADPCM blocks (`15`/`30` bytes mono/stereo, `28` samples each); packet count is `ceil(dwOutSize / block_bytes)` clamped to the blocks on disk, but `duration`/`duration_ts` ignore `dwOutSize` and reflect the file's own full block count instead — a real, measured disagreement in the reference itself, reproduced rather than "corrected" — see `xa.rs`'s module doc |
| `block` | shared | `BlockDemuxer` — the fixed-ratio block engine `adx`, `nistsphere`, `pvf` and every `rawcodec` entry reduce to |

### Deliberately not in this crate

**#621 (legacy compressed-audio containers):** only `wv` and `tta` landed
with real confidence. `ape` (Monkey's Audio) and `mpc`/`mpc8` (Musepack SV7/
SV8) were not attempted — this build's `ffmpeg` has no encoder for any of
the three, and `true-audio.com`'s WavPack-adjacent "format description" page
turned out to be domain-squatted (redirects to unrelated content), so
getting their several header-version layouts right would have meant more
spec study than this pass's time budget allowed. `shn` (Shorten) is a
continuous Rice-coded bitstream with no separable frame boundary at the
container level at all — demuxing it without decoding is not really
possible, which is a different kind of gap from "not yet done". `tak`,
`rka`, `osq`, `wavarc`, `bonk` were not attempted.

**#622 (T3 tail):** `codec2`/`codec2raw` were skipped — this build's
`ffmpeg` has neither the codec nor a documented default mode, and the
container needs one to know its frame size. `hcom`, `epaf`, `mmf`, `qcp`,
`vqf`, `oma`, `dsf`, `avr`, `aea` were not attempted; `mmf` specifically
turned out to nest a further `Atsq`/`Awa` sub-chunk structure once measured,
past what this pass's time budget covered.

**`nistsphere`/`pvf`'s raw-PCM packet batching**, unlike everything above,
is not an unattempted format — it is a real, partially-measured divergence
left unresolved on purpose. See "The `BlockDemuxer` batching bug" below.

**#620 (tracker/module and chiptune-adjacent):** XM now has a bounded
structural demux path. It accepts only the published v1.04 layout, reports
sample payload boundaries, and deliberately does not render patterns or
decode tracker sample data. Tracker playback and the remaining tracker formats
(IT, S3M, and the rest of the family the reference reaches through
`libopenmpt`) remain excluded — see
`docs/why-some-formats-are-not-included.md`. Of the twelve chiptune-adjacent
game-audio containers, `vag` and `xwma` landed in an earlier pass, followed by
`xa`, `svag`, bounded `brstm`/`bfstm` subsets, and FSB4/FSB5; five remain, each for a specific, recorded
reason:

- `binka` (raw Bink Audio) and `genh` (a generic fixed-struct header) — the
  only independent public documentation found was either explicitly hedged
  (a community writeup of `binka`'s header flagged several fields as
  "probably" and admitted to never having seen one of its own claimed frame
  markers in a real file) or explicitly deferred to a specific decoder's
  own source code for the formal definition (`genh`). Building a
  "spec-conformant" fixture from either would mean guessing at the
  uncertain parts or reading another project's implementation — the second
  of which risks exactly the kind of contamination D7/D9 exist to prevent,
  even though the source in question is not FFmpeg's. Neither was
  attempted.
- `hca` — public write-ups describe the container at a high level (magic,
  a subkey byte for AWB archives) but no independently-sourced byte-level
  chunk table (`fmt`/`comp`/`dec`/`vbr`/`ath`/`loop`/…) was found; not
  attempted.
- `xvag`, `msf` — same family, same technique as `vag`/
  `xwma`/`xa` would likely apply, but not researched this pass either;
  the same "no independently reachable byte-level spec found yet" bar
  `binka`/`genh`/`hca` sit behind is the working assumption, not confirmed
  per-format.

### `fsb` — bounded FMOD sample-bank demux

The reader accepts FSB4's published 48-byte header and 80-byte directory
entries only when the entry's measured Nintendo THP mode is present. It emits
one 16-byte packet per stored THP block, with 14-sample packet durations, the
same packet geometry reported by `ffprobe` 9.0.1 on the hand-built fixture.
FSB5 follows the MIT-licensed `python-fsb5` compact header: each 64-bit sample
header carries a frequency code, mono/stereo bit, 16-byte data offset, and
sample count; optional metadata chunks can override frequency and channels.
PCM8, PCM16, PCM32, MPEG, and Vorbis are indexed as one stream and one stored
payload packet per sample. Names are intentionally not surfaced because the
current stream model has no per-stream title slot.
The committed `tests/fixtures/fsb4-thp.fsb` and `fsb5-pcm16.fsb` fixtures are
opened through the public demux descriptor; the FSB4 fixture's 44100 Hz,
stereo, 16-byte/14-sample packet geometry was checked with `ffprobe` 9.0.1.

Encrypted FSB4 banks, FSB4 modes other than the measured THP mode, and FSB5
PCM24/float/IMA/VAG/HEVAG/XMA/CELT/AT9/XWMA/FADPCM or unknown modes are named
refusals. The reader never decodes or guesses a payload codec.

### `xm` — bounded FastTracker 2 structural demux

The reader follows the pinned [Kaitai FastTracker XM declarative format
description](https://formats.kaitai.io/fasttracker_xm_module/), whose page
identifies the KSY as Unlicense. The acquired KSY is pinned at commit
`f6a82f70fa9ca8cbd3f7f4b39b2cc215668f0fb4` with SHA-256
`3997da4f6aab2d3d7e8458569ee0361f42c40b37296f797eb803d5a0ced8f837`; the
acquisition record is `kaitai-fasttracker-xm` in `provenance/sources.toml`.

The bounded implementation accepts version `0x0104`, the 276-byte main
header, 2–32 even channels, pattern headers of size 9 with packing type zero,
instrument headers up to 1 MiB, and 40-byte sample headers. It checks
all declared ranges before seeking, caps cumulative pattern bytes at 64 MiB,
sample payloads at 1 GiB, and exposes each non-empty sample payload as one
packet on a mono structural stream. Pattern events and delta-coded sample bytes
are not interpreted; no audio sample format or tracker codec identity is claimed.
Because payload packets are emitted after the complete structural scan, the
reader requires a seekable input source.
Older revisions, non-standard variable header sizes, nonzero pattern packing,
reserved sample flag bits, malformed counts, and truncated ranges are explicit
refusals. This is container framing, not playback or a claim of ffmpeg
interoperability.

### `protracker` — bounded four-channel MOD structural demux

The reader follows the published [Protracker Module
layout](https://wiki.multimedia.cx/index.php/Protracker_Module), cross-checked
against [Greg Kennedy's field table](https://greg-kennedy.com/tracker/modformat.html).
It accepts the `M.K.` four-channel revision: 20-byte title, 31 fixed 30-byte
sample headers, one-byte song/restart fields, 128-byte order table, and 64-row
patterns of four 4-byte events. Sample lengths are big-endian words; payloads
are exposed as signed 8-bit mono structural streams, without claiming a
playback rate or decoding periods/effects.

The implementation bounds pattern indexes to 64, validates sample volume and
loop ranges, checks every declared extent before skipping, caps cumulative
sample bytes at 1 GiB, and requires seekable input because payload packets are
emitted after the pattern scan. Non-`M.K.` revisions, malformed loops, and
truncated structures are explicit refusals. The source-built minimal fixture
was independently identified by `file 5.41` as `4-channel Protracker module
sound data Title: "Minimal Vaco MOD"`; the checked-in test then compares its
sample packet bytes and offset against the same fixture.

### `bfstm` — measured stereo DSP-ADPCM packet synthesis

The Custom Mario Kart 8 Wiki's BFSTM layout
(`Vaco-Spec-Ref mk8-bfstm-format`) and 3dbrew's BCSTM layout
(`Vaco-Spec-Ref 3dbrew-bcstm-format`) specify the 0x40-byte header,
byte-order marker, sized references, `INFO` stream/channel tables, per-channel
DSP coefficients, `SEEK` histories, and block-interleaved `DATA`. The BFSTM
page explicitly gives one channel's stored size as
`(block_count - 1) * block_size + final_block_padded_size`.

Hand-built `FSTM` and `CSTM` fixtures in both byte orders establish the packet
contract through `ffprobe` 9.0.1:

- exactly two DSP-ADPCM channels and channel blocks of 16, 32, 64, 96, or 256
  bytes, with a full or half-sized final block physically padded to the full
  block size;
- one packet per interleaved block. The 80-byte prefix is file-endian
  `u32(raw payload bytes)`, file-endian `u32(sample count)`, 32 coefficient
  bytes per channel, and the packet's 8-byte interleaved SEEK history;
- each channel contributes only its unpadded bytes. The checked-in 512-byte
  fixture therefore produces packet sizes 144 and 112, PTS 0 and 56,
  durations 56 and 28, and aggregate duration 84 samples at 32 kHz.

PCM, IMA ADPCM, non-stereo streams, region tables, and unmeasured geometry are
named refusals. Vaco leaves codec identity absent because its shared `CodecId`
does not contain the reference's endian-specific `adpcm_thp`/`adpcm_thp_le`
names; inventing a nearby identity would be worse than reporting none.

### `brstm` — measured stereo DSP-ADPCM packet synthesis

WiiBrew's BRSTM layout (`Vaco-Spec-Ref wiibrew-brstm-format`) specifies the
big-endian `RSTM` file header and `HEAD`/`ADPC`/`DATA` chunks. It states that
each DATA frame stores one padded block per channel, but it does not specify
the elementary-packet wrapper the reference exposes. Hand-built fixtures
measured through `ffprobe` establish the bounded contract this demuxer
registers:

- exactly two DSP-ADPCM channels, sample rate from `HEAD`, and channel block
  sizes 32, 64, 96, or 256 bytes;
- normal blocks and a final block of either the full block size or half of it,
  with the final block physically padded to a full block on disk;
- one packet per interleaved frame. Its payload is `be32(raw payload bytes)`,
  `be32(sample count)`, 32 coefficients for each channel from `HEAD`, the
  packet-indexed 8-byte ADPC history entry, then each channel's **unpadded**
  block bytes. The wrapper is 80 bytes, so a 32-byte stereo normal block is a
  144-byte packet and a 16-byte-per-channel final block is 112 bytes;
- PTS/DTS are sample offsets, duration is the current block's sample count,
  and `ffprobe` explicitly reports packet `pos=N/A`. Vaco consequently leaves
  packet position absent rather than inventing a byte offset for synthesized
  payload.

The source-built mono fixture is rejected by the reference, so mono and every
unmeasured codec/channel/block geometry are named refusals. This is a
deliberately smaller promise than the broad BRSTM file specification, not a
claim that the other variants are malformed.

`vaco-probe` agrees with `ffprobe` on the container name, duration, stream
rate/channel count/time base, and each packet's timestamps, sizes, position
absence, and MD5. It reports no stream `codec_name`: the shared `CodecId`
vocabulary does not yet contain `adpcm_thp`, so this demux-only crate does not
invent a codec identity outside that vocabulary. This is the one documented
metadata divergence from the reference for the measured subset.

---

## How it works

Sample-count durations use the stream's native `1/sample_rate` clock for
both packets and aggregate lengths. `duration()` and `duration_exact()`
retain the same rational seconds; only display formatting rounds. WavPack
and TTA fixtures with 1,024 samples at 44.1 kHz both retain `256/11025`
seconds, with their complete 54-byte and 194-byte packets checked against
the reference file bytes. Their decoded reference payloads each contain 2,048
zero bytes.

The differential fixture loop also checks that every packet duration lies
exactly on its declared native sample clock. This catches fractional-sample
artifacts from microsecond conversion without weakening existing packet-size
checks. Keep sample counts and stream clocks together when adding a format.

### `block::BlockDemuxer` — the shared engine

Once a format's header (if it has one) is parsed down to `(sample_rate,
channels, bytes_per_block, frames_per_block, data_start, declared_len)`,
`BlockDemuxer` does the rest: clamps `declared_len` against the source's own
size, reads packets sized to a **required, per-caller `target_packet_bytes`**
rounded down to a whole number of blocks (discarding a partial trailing
block, never emitting a short final block as if it were whole), and stamps
`pts`/`duration` from a running block count. `adx`, `nistsphere`, `pvf`, and
every entry in `rawcodec` are this engine plus their own header.

**`target_packet_bytes` is a measured constant, not a shared default.**
This used to be a single hardcoded 4096-byte target for every consumer —
found wrong via `vag` (see below), which does not use `BlockDemuxer` at
all specifically because its reference packet size (16 bytes, one per
block) is nothing like 4096. Measuring every `BlockDemuxer` consumer
individually against `ffprobe -show_packets` found the reference emits
**one packet per block** for `adx`, `gsm` and `g729` (18/33/10 bytes), and
a *different fixed byte count per format* for everything else in
`rawcodec`: 1024 for `g722`/`aptx`/`sln`, 1020 for `g726`/`g726le`/`g728`,
512 for `dfpwm`, 1536 for `aptx_hd`. These do not reduce to one formula —
`g722` and `g726` share the same 1:2 byte:frame ratio and the same fixed
sample rate family, yet batch into different byte counts — so each is its
own hardcoded, measured constant (`RawCodecSpec::target_packet_bytes`, or
the literal block size for `adx`). `nistsphere`/`pvf`'s raw-PCM tail still
pass the old, **unmeasured** `4096` (`block::DEFAULT_TARGET_PACKET_BYTES`)
— see "Deliberately not in this crate" below for why that one was not
chased to ground.

The two G.726 raw formats share rate, channel count, packet sizing, and the
four-bit / 32 kbit/s algorithm, but not their decoder identity:
`g726` routes to `CodecId::AdpcmG726` for high-nibble-first bytes and
`g726le` routes to `CodecId::AdpcmG726le` for low-nibble-first bytes. Keep
those IDs distinct when adding a container mapping; otherwise the stream is
reachable but every byte's two codes are decoded in the wrong order.

### `vag`, `svag`, `xwma` and `xa` — hand-built fixtures, measured against `ffprobe`

None of the four formats has an `ffmpeg` encoder, so every fixture was
hand-built directly from public documentation (`Vaco-Spec-Ref
vag-format-doc` / `svag-format-note` / `multimedia-wiki-xwma` /
`microsoft-riff-xaudio2` / `maxis-xa-format-doc`) and then measured against
`ffprobe`/`ffmpeg` —
the same technique `vaco-format-misc`'s
`roq`/`flic`/`cdg`/`bink`/`smk` used, and, like `bink`/`smk`, it surfaced
behaviour a reading of the format documentation alone would not have
predicted:

- **`vag` emits one packet per 16-byte block**, not batched — confirmed
  directly against `-show_packets` (ten blocks, ten packets, `pts`
  advancing by 28 samples each). `vag.rs` does not reuse this crate's own
  `BlockDemuxer` (it predates the fix below and its own small loop was
  simpler than threading a new parameter through). Finding this here is
  what surfaced that `BlockDemuxer` itself batched every other consumer
  into oversized packets — a real, pre-existing divergence affecting
  `adx` and all ten `rawcodec` formats, since fixed (see
  `block::BlockDemuxer`'s entry above and `planning/TECH-DEBT.md`).
- **`svag` consumes 20 header bytes and reads packets to physical EOF**, even
  though the independent community note reports a 32-byte header and its own
  `data_size` can disagree with the bytes present. Field sweeps against
  `ffprobe` 9.0.1 established little-endian `data_size`/`sample_rate`/
  `channels`/`interleave`; `channels * interleave` packet bytes; 28 samples per
  16-byte interleave unit; and duration derived from `data_size` in 16-byte
  channel blocks, independent of packet grouping. A short final packet is
  emitted corrupt without timestamps rather than discarded. `svag.rs`
  reproduces all of these measured behaviors.
- **`xwma`'s packets are `nBlockAlign`-aligned reads of `data`, not the
  `dpds` chunk's declared split.** `dpds` looks exactly like a per-packet
  byte-offset table and MultimediaWiki describes it that way, but a
  fixture whose `dpds` declares four packets of `100/150/120/90` bytes
  still demuxes, in the reference, to packets of `100/100/100/50` bytes —
  block-aligned, matching `nBlockAlign = 100`, and ignoring `dpds`
  entirely. `xwma.rs` parses `dpds` only far enough to skip over it.
- **A `WMAv2` stream with an empty `fmt` chunk gets 6 bytes of extradata
  the reference synthesises, not zero.** Verified byte-exact via
  `ffprobe -show_data_hash md5`: `00 00 00 00 1F 00`. A `WMAv1` stream in
  the same situation gets none. `xwma.rs` reproduces this for `WMAv2`
  only.
- **`xwma`'s stream-level `duration_ts` depends on whether a `dpds` chunk
  exists at all — and the exact rule is now pinned down and reproduced.**
  Sweeping `channels`/`bits_per_sample`/`data_len` independently found:
  `duration_ts = data_len / (channels * bytes_per_sample)` whenever a
  `dpds` chunk is present, i.e. the reference treats the raw compressed
  bytes as if they were already decoded PCM at the `fmt` chunk's own
  channel count and bit depth. Confirmed across mono/stereo, 8/16-bit,
  and `wmav1`/`wmav2`. `xwma.rs` reproduces this exactly, falling back to
  the byte-rate formula only when no `dpds` chunk was seen.
- **`xa`'s byte order is confirmed, not assumed.** The public write-up
  (`maxis-xa-format-doc`) documents `dwOutSize`/`wChannels`/
  `dwSampleRate`/… as a Win32 `WAVEFORMATEX` tail but never states
  endianness explicitly; a big-endian reading of the same field values
  produced an implausible multi-GHz "sample rate" through the real `xa`
  demuxer, little-endian did not, at every field checked.
- **`xa`'s `dwOutSize` gates packet count, not duration — and the two
  genuinely disagree in the reference.** An initial reading (floor-divide
  it as decompressed PCM bytes) was wrong; sweeping the field from `0` to
  `559` against a 20-block stereo fixture found `ceil(dwOutSize /
  block_bytes)` packets exactly, clamped to the blocks actually on disk,
  with `dwOutSize = 0` giving **zero** packets outright rather than
  "unbounded". But `duration`/`duration_ts` ignore `dwOutSize` completely
  and instead reflect the file's own full block count — the same fixture
  emits 4 packets (from `dwOutSize`) while reporting `duration_ts` for all
  20 blocks. `xa.rs` reproduces both halves of this disagreement rather
  than resolving it into a number the reference itself never produces
  (`Vaco-Spec-Ref vaco-format-misc-audio-xa-fixtures-probe`).

### The measured comparison table

Fourteen of the twenty-four registered demuxers have a fixture under
`tests/fixtures/` exercised by `tests/differential.rs` (a real `ffmpeg
8.1`-produced one for `wavpack`/`tta`/`g722`/`g726`/`g726le`, hand-built and
measured against the reference's own demuxer for the rest, per the
sections above); the remaining formats are headerless codecs this build's `ffmpeg`
cannot encode, or (`g723_1`) can encode but the reference states no
duration for. `tests/differential.rs` opens each fixture through its real
`DemuxerDesc` and checks it against `ffprobe -show_entries
stream=sample_rate,channels -show_entries format=duration`:

| Fixture | `sample_rate` | `channels` | reference duration | reference packet sizes | notes |
|---|---:|---:|---:|---|---|
| `wavpack.wv` | 44100 | 2 | 0.300 s | not checked | |
| `tta.tta` | 44100 | 2 | 0.300 s | not checked | |
| `adx.adx` | 8000 | 1 | 0.304 s | 76×18 bytes | reference `duration_ts` is block ticks (`time_base=1/250`), not samples — see below. One packet per block, not batched (fixed, see below) |
| `g722.g722` | 16000 | 1 | 0.150 s | 1024, 176 bytes | |
| `g726.g726` | 8000 | 1 | 0.300 s | 1020, 180 bytes | reference `time_base=1/90000` (a generic raw-audio fallback), not samples |
| `g726le.g726le` | 8000 | 1 | 0.300 s | 1020, 180 bytes | as `g726` |
| `aptx.aptx` | 48000 | 2 | *(reference: N/A)* | 12×1024, 944 bytes | this crate estimates a duration from the file size; the reference's raw `aptx` demuxer declines to |
| `aptx_hd.aptx_hd` | 48000 | 2 | *(reference: N/A)* | 14×1536, 96 bytes | as `aptx` |
| `sbc.sbc` | 16000 | 1 | *(reference: N/A)* | not checked | self-delimited, no declared total |
| `g723_1.g723_1` | 8000 | 1 | *(reference: N/A)* | not checked | self-delimited, no declared total |
| `vag.vag` | 22050 | 1 | 0.012698 s | 10×16 bytes | one packet per 16-byte block, matching the reference exactly |
| `svag.svag` | 44100 | 2 | 0.006349 s | 10×32 bytes | packet stream runs to physical EOF while duration comes from declared `data_size` |
| `xwma.xwma` | 8000 | 1 | 0.350 s | 100, 100, 100, 50 bytes | fixture deliberately has no `dpds` chunk, so this exercises the byte-rate duration formula, not the (also reproduced) `dpds`-present PCM-frame-size one — see above |
| `xa.xa` | 22050 | 2 | 0.006349 s | 5×30 bytes | `dwOutSize` set to exactly 5 blocks' worth of PCM bytes, so packet count and duration agree here; the `dwOutSize`-vs-duration disagreement above is unit-tested in `xa.rs`, not in this fixture |

`tests/differential.rs` asserts every "reference packet sizes" cell
byte-for-byte, not just total bytes or duration — this is the check that
was missing when `BlockDemuxer`'s batching bug first shipped (see below),
and it now exists specifically so a future regression here fails a test
instead of surfacing as a silent divergence again.

Two genuinely measured divergences are recorded here rather than "fixed",
because fixing them would mean discarding information rather than
correcting an error:

- **`adx` and `g726`/`g726le` report a duration in the same wall-clock
  seconds as the reference, at a different tick rate.** The reference's
  `duration_ts` is a *ticks* value in whatever `time_base` that format's own
  demuxer picked (block count for `adx`, a generic `1/90000` for the raw
  ADPCM formats); this crate's `time_base` is always `1/sample_rate`. Both
  land on the same duration in seconds — `0.304 s` and `0.3 s` respectively
  — which is the invariant the comparison table checks.
- **`aptx`/`aptx_hd` get a duration where the reference reports `N/A`.**
  Both have a fixed, spec-mandated bytes-per-sample ratio, so this crate's
  `BlockDemuxer` estimates a duration from the file size the same way
  `vaco-format-audio-simple`'s `RawPcmDemuxer` does for headerless PCM; the
  reference's own raw `aptx`/`aptx_hd` demuxer just does not bother.

### The `BlockDemuxer` batching bug, found and fixed

`BlockDemuxer` originally batched many blocks into one packet, targeting
roughly 4096 bytes, for every consumer — `adx`, `nistsphere`, `pvf`, and
all ten `rawcodec` formats. Building `vag` and measuring its packet
granularity against `ffprobe -show_packets` (one packet per 16-byte
block) surfaced that this batching itself was a divergence from the
reference, not a design choice: on `adx.adx`'s own 76-block fixture, the
reference emits 76 separate 18-byte packets, where this crate emitted a
single 1368-byte one. `#621` and `#622` closed on the strength of
stream-level fields and "at least one packet produced" — packet-level
shape was never checked, which is exactly how this survived.

Fixed by measuring **every** `BlockDemuxer` consumer individually rather
than assuming `vag`'s one-packet-per-block answer generalised: it does
not. `adx`, `gsm` and `g729` get one packet per block; `g722`, `g726`,
`g726le`, `g728`, `dfpwm`, `aptx`, `aptx_hd` and `sln` each batch into
their own distinct, measured, fixed byte count (see the table above and
`block.rs`'s module doc for the full readout). `BlockDemuxer::new` now
takes `target_packet_bytes` as a required argument instead of picking one
itself, so every call site states its own measured answer explicitly.

**Not fixed, for a different and better-justified reason:** `nistsphere`
and `pvf`'s raw-PCM tail still use the old, unmeasured `4096`-byte
default (`block::DEFAULT_TARGET_PACKET_BYTES`). Their batching was
measured to depend on sample rate — a clean "~64 ms per packet, rounded
to a power of two" formula held from 250 Hz through 16 kHz, then broke
between 20.4 kHz and 20.6 kHz in a way that did not match any
closed-form rule tried (not "64 ms", not a simple power-of-two-of-rate
bracket). Shipping a guessed formula here would risk exactly what this
whole fix was about avoiding — trading one silent divergence for
another — so it is recorded honestly in `planning/TECH-DEBT.md` instead.

### Measured field layouts, not recalled ones

`wv`'s block header and `tta`'s header/seek-table were reconstructed by
generating a real fixture with `ffmpeg -c:a wavpack -f wv` / `-c:a tta -f
tta` and reading the bytes back with a script, then cross-checking every
field against `ffprobe`'s own reported `sample_rate`/`channels`/
`duration_ts` — not from `true-audio.com`, which turned out to be
domain-squatted (see "Deliberately not in this crate"). `adx`'s header was
recovered the same way. `amr`'s frame-size table was cross-checked against
IETF RFC 4867's own Table 1 (total speech bits per mode), fetched directly;
the AMR-WB half of that table is the standard 3GPP TS 26.201 byte sizes,
reproduced from the widely-published numbers rather than a freshly fetched
copy of that specific document. `sbc`'s frame-length formula was verified
against a real fixture: the measured header fields (`blocks=16`,
`subbands=8`, `bitpool=60`, mono) give exactly `128` bytes by the formula,
and the fixture is `128 * 37` bytes exactly.

---

## How to change it

- **A new entry in the fixed-ratio `rawcodec` family**: add a
  `RawCodecSpec` const and one `raw_codec_format!` macro invocation. The
  macro generates the probe, the `open` wrapper and the `DemuxerDesc`;
  nothing else needs touching except the `vaco-component.toml` fragment and
  `cargo xtask gen-registry`.
- **Reviving one of the deferred #621/#622 formats**: read "Deliberately
  not in this crate" above first — most of them are blocked on a spec
  source, not on difficulty. `ape`/`mpc`/`mpc8` need a genuinely reachable
  primary specification (`monkeysaudio.com`'s own SDK docs are one
  candidate not yet tried); `codec2`/`codec2raw` need a decision about
  which mode a headerless file is assumed to be.
- **Extending `brstm`/`bfstm`**: add new codec/channel/block geometries only
  after an independent source establishes their layout and a real reference
  accepts the fixture; both current paths intentionally refuse beyond their
  measured stereo DSP-ADPCM subsets.
- **The five still-unresearched game-audio containers** (`binka`, `genh`,
  `hca`, `xvag`, `msf`): each remains gated on an independent byte-level
  source and a reference-accepted fixture; the bounded FSB4/FSB5 subset is
  documented above.
- **`amr`'s multichannel interleaved variant** (`#!AMR_MC1.0\n` and
  friends): not implemented; only mono narrowband/wideband are.
- **A `Limits` injection point for `open`**: as
  `vaco-format-audio-simple`, none of these `open` functions take a
  `vaco_limits::Limits` override; each opens under a fixed internal
  `Limits::permissive()`.

## Configuration

None. Every format's `open` function takes only the source (or, for the
registered `DemuxerDesc`s, the `ParserProvider` no module here actually
calls — none of these codecs is reached through a bitstream parser).

## Dependencies

`vaco-core`, `vaco-io` (`IoContext`, the byte-order-aware readers every
module is built on), `vaco-limits` (`Budget`), `vaco-packet`,
`vaco-sampfmt`, `vaco-chlayout`, `vaco-format-core` (`Demuxer`,
`DemuxerDesc`, `ProbeData`/`ProbeScore`), and `vaco-codec-core` for
`CodecId` — including seven variants (`WavPack`, `Tta`, `Dfpwm`, `Aptx`,
`AptxHd`, `Sbc`, `AdpcmAdx`) added there by this crate, following the
precedent set by the RTP/FLV/subtitle-text crates of adding to that
hand-maintained enum directly when a new format needs an identity it does
not yet have. `xwma` needed no new variant (`wFormatTag` maps onto the
existing `Wmav1`/`Wmav2`/`Wmapro`); `vag`'s codec (`adpcm_psx`) and `xa`'s
(`adpcm_ea_maxis_xa`) still have none — see `planning/INTERFACE-GAPS.md`
gap 21, extended with this crate's tenth and eleventh entries rather than
a new gap, since gap 21 is the established place this family of finding
gets tracked (`vaco-format-misc` found the first nine). Both streams
carry `codec_id: None` until their variants land. `svag` uses the same
`adpcm_psx` identity as `vag`, so it shares that existing gap rather than
creating another.
