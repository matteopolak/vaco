# `vaco-format-dv`

## What it is

DV (SMPTE 314M / IEC 61834): both the demuxer and the muxer, in one crate
because there is barely a container here to split across two. A DV
elementary stream is a sequence of fixed-size frames — 120000 bytes at
NTSC, 144000 at PAL — with no header, no index, and no separate audio
stream at the byte level: audio is interleaved into fixed positions inside
every video frame. Registers as `dv`, extensions `dv`/`dif`.

## How it works

### Layout

| Module | Contents |
|---|---|
| `profile` | frame-size/system (NTSC/PAL) detection from one bit |
| `demux` | fixed-size frame reads, one video packet per frame |
| `mux` | the inverse: write whole frames back out verbatim |

### Profile detection: the `dsf` bit

Every DIF block begins with a 3-byte ID; the first block of every frame is
always a Header block. Bit 7 of the fourth byte (`profile::DvProfile::detect`)
is the `dsf` (digital sequence format) flag: `0` → 525-60 (NTSC, 10 DIF
sequences/frame, 120000 bytes), `1` → 625-50 (PAL, 12 sequences, 144000
bytes). Measured directly against `ffmpeg -f dv` output for three real
captures (NTSC 4:1:1, NTSC 4:2:2, PAL 4:2:0) — see the module's doc table.

**A real, documented gap**: DVCPRO50/DVCPRO HD (double/quad the data rate,
for 4:2:2/4:2:2-plus chroma) share the *identical* first four bytes as
plain DV25 but a different actual frame size — measured at 240000 bytes for
NTSC DVCPRO50, not the 120000 the `dsf` bit alone implies. `DvProfile::detect`
only knows standard-rate DV25. `DvDemuxer::open` does not trust it blindly:
it peeks past the computed frame size and checks that the *whole four-byte
header pattern* repeats there (not just the single `0x1F` byte — a
double-rate stream can coincidentally have `0x1F` at the halfway point too,
because it is itself built from two interleaved tracks; checking only that
byte was tried first and found to pass on a real DVCPRO50 sample it should
have rejected). A mismatch is refused with `Error::InvalidData` rather than
silently misframing every frame after the first.

### What is demuxed, and what is not

`DvDemuxer` always declares two streams — video and audio, matching every
measured `ffprobe` output for an ordinary DV file — but **only the video
stream produces packets**. Each `read_packet` call reads exactly one whole
frame (`profile.frame_size` bytes) via `io.read_exact` and emits it as a
single, always-keyframe (`PacketFlags::KEY`) video packet; DV is all-intra,
so this is exactly right, not an approximation.

**Audio packet extraction is deliberately not implemented.** This was
checked, not assumed: the AAUX "Audio Source" pack this crate would need to
decode (sample rate/quantization/channel count) was probed on a real
`ffmpeg -f dv` capture and found filled with `0xFF` — the DV convention for
"not set" — rather than the bit pattern public technical descriptions of
SMPTE 314M/IEC 61834 predict. The demuxer declares the audio stream with a
conventional default (48 kHz, 16-bit, stereo, locked — matching what
`ffprobe` reports for the one sample checked) rather than a decoded fact,
and produces zero audio packets. Shipping a sample-deinterleaving routine
this crate cannot verify against a byte-exact reference would be worse than
not shipping one: wrong audio silently looks like working audio.

For a seekable file, the video aggregate duration is derived from the count
of whole fixed-size frames on DV's native 60,000 Hz clock. The
`duration_exact()` view preserves NTSC's 2,002 ticks per frame; the legacy
`duration()` API remains the rounded microsecond value for compatibility.

### Muxing

`DvMuxer::write_packet` writes a video packet's payload verbatim — there is
no header, no trailer, nothing to multiplex, so "muxing" is exactly "the
bytes you get are the bytes you gave it, once video-frame-sized". An audio
packet is refused at write time (`Error::Unsupported`) rather than silently
dropped, because interleaving PCM into a DV frame's AAUX blocks needs the
same sample-interleaving this crate's demuxer defers.

## How to change it

* **DVCPRO50/DVCPRO HD support**: `profile::DvProfile::detect` needs a second
  detection path for the double/quad-rate variants. This needs either the
  actual SMPTE 314M text or a wider byte-for-byte comparison across real
  samples than this pass did — see the module's doc comment for exactly
  what was checked and what was not.
* **Audio packet extraction**: needs the real AAUX-block sample-interleave
  table (SMPTE 314M/IEC 61834), verified byte-exactly against
  `ffmpeg`-decoded PCM as a black-box oracle (D6) before shipping — probing
  one synthetic sample and finding `0xFF` filler is not evidence either way
  about a real camcorder recording, only about what this project's own test
  fixture contains. Do not ship a best-guess interleave without that
  verification step; see the module docs for why.
* **Chroma format / pixel format**: not read at all — `VideoParameters` here
  only carries width/height/frame_rate/coded dimensions, derived from
  `DvProfile`, not from the VAUX video-source-control pack. There is nowhere
  to put a pixel format yet anyway (`vaco_codec_core::CodecId` has no DV
  video variant — see "Dependencies").

## Configuration

No crate-specific options; `DvDemuxer::open`/`open_with_limits` and
`DvMuxer::new` are the whole interface.

| Constant | Value | Meaning |
|---|---|---|
| `demux::DEFAULT_AUDIO_SAMPLE_RATE` | 48000 | Declared, not decoded — see "What is demuxed" |
| `demux::DEFAULT_AUDIO_CHANNELS` | 2 | Same |
| `profile::DvProfile::NTSC/PAL` | 120000/144000-byte frames | 10/12 DIF sequences × 150 blocks × 80 bytes |

## Dependencies

`vaco-core`, `vaco-io`, `vaco-limits`, `vaco-packet`, `vaco-format-core`,
`vaco-codec-core`. No `vaco-parse-*` or concrete codec crate. No
`ParserProvider` use at all: DV carries no in-band codec configuration
beyond the fixed frame itself, and there is no `CodecId` for it to refine
into yet (`vaco_codec_core::CodecId` has no DV video or DV audio variant as
of this survey, 2026-08-23) — `open_demuxer` in `lib.rs` accepts a
`&dyn ParserProvider` to satisfy `DemuxerDesc::open`'s frozen signature and
does not call it.

## What was and was not measured

Verified directly against real `ffmpeg 8.1 -f dv` output (2026-08-23),
embedded as small `tests/fixtures/*.dv` captures:

* `dsf`-bit profile detection and the resulting frame size, for NTSC 4:1:1.
* Reported width/height/frame_rate matching `ffprobe`'s output exactly
  (720×480, 30000/1001).
* Exactly one whole-frame, always-keyframe packet per 120000-byte frame.
* The DVCPRO50 rejection path, against a real (truncated) DVCPRO50 capture
  that the naive single-byte check was measured to *not* catch, and the
  strengthened four-byte check was measured to catch.

**Not measured, and known to be absent, not merely approximate**:

* Audio sample extraction — zero packets are ever produced for the audio
  stream; see "What is demuxed" for why this is a deliberate gap rather
  than an oversight.
* PAL and DVCPRO50/4:2:2 frame *reading* (detection is measured; full
  frame-by-frame reads of a PAL or 4:2:2 file were not exercised end to end
  in a test, only the profile/frame-size detection was).
* Timecode/subcode extraction (the DIF Subcode blocks) — not read at all.
* Muxer output was checked only for verbatim byte pass-through into a
  `DynBuf`/`SharedDynBuf`, not against any real DV playback tool.
