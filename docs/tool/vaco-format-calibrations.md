# Format calibration catalogue

## What it is

The v0.1 MP4, Matroska/WebM, and MPEG-TS demuxers need a small set of
reference-behaviour measurements where the relevant container specifications
permit more than one conforming choice. `vaco-conformance` owns the catalogue
in `experiments.rs`; this page explains its scope and how a format agent should
use it.

The catalogue contains 27 named rows. P1–P7, T1–T5, S1, M1–M7, K1–K4, A1, and
N1 are the 26 executable black-box experiments. L1 is a public-document review
that classifies WavPack and TTA; it is deliberately counted separately so a
status report cannot imply that it ran against an oracle.

## How it works

Each entry has a stable identifier, the behavioural question it resolves, an
evidence kind, and a reference-only recipe. The recipe must generate or mutate
its media in a temporary directory, run `ffmpeg`/`ffprobe` as the only reference
binaries, and assert the observed values, counts, or bytes. It must not store
reference output in the repository or inspect reference source.

`scripts/verify-format-calibrations.sh` is the executable home for completed
black-box checks. Run `bash scripts/verify-format-calibrations.sh all` for its
current set, or pass an identifier such as `P3` to run one check. It uses Bash
with `pipefail` deliberately: the reference command's non-zero exit must fail
the calibration instead of being hidden by a formatting command downstream.

After building the harness, `vaco-conformance experiments` lists every stable
handle and its procedure; `vaco-conformance experiments --id P3` selects one.

## Completion status

The catalogue is intentionally ahead of its implementation. As of the
measurements on this page, 22 of the 26 runnable rows have an executable
assertion: `P1`, `P2`, `P3`, `P4`, `P5`, `P6`, `P7`, `T1`, `T2`, `T3`, `T4`,
`T5`, `S1`, `M1`, `M3`, `M5`, `M7`, `K2`, `K3`, `K4`, `A1`, and `N1`. The
remaining runnable rows — `M2`, `M4`, `M6`, and `K1` — are open. `L1` is
complete as a documentary classification, not an executable reference observation.

The remaining rows require genuinely distinct inputs, not a nearby smoke test:
M2 needs ctts v0/v1 plus cslg variants, M4 needs conflicting `chpl` and
`tref/chap` chapter sources, M6 needs incompatible `avc1` and `hvc1` entries in
one `stsd`, and K1 needs an EBML-laced Vorbis Block without `BlockDuration`.
The local native Vorbis muxer can encode stereo Vorbis but emitted un-laced
BlockGroups in this session, so it is not an admissible K1 substitute.

Do not close the format-calibration acceptance work from the partial script
result. Completion requires an assertion and recorded observation for all 26
runnable rows, the L1 source review, and the configured corpus/probe matrix.

The existing probe manifests remain the shared generated-media baseline:

| Family | Manifest | Existing media shapes |
| --- | --- | --- |
| ISO BMFF | `tests/conformance/probe/isobmff.toml` | MPEG-4 video MP4, MPEG-4+PCM MOV, ALAC M4A |
| Matroska | `tests/conformance/probe/matroska.toml` | MPEG-4+FLAC MKV, VP8+Opus WebM |
| MPEG systems | `tests/conformance/probe/mpegts.toml` | MPEG-2 TS, MPEG-2+MP2 TS, MPEG-PS/VOB |

Use those cases where they distinguish the question; add a named calibration
fixture only when its construction varies a behaviour the general probe corpus
cannot expose. A one-frame rate case, a deliberately truncated `mdat`, or an
equal-DTS interleave case are calibration inputs, not duplicate smoke tests.

## How to change it

Add a calibration row in `crates/tool/vaco-conformance/src/experiments.rs`
before adding an implementation rule that depends on it. Keep its identifier
stable. A new runnable row needs a concrete temporary-media recipe and an
automated assertion that fails if the reference command exits non-zero; only a
missing binary may skip.

When an experiment settles a rule, record the ffmpeg version, exact command,
and the measured field/count/byte result in `planning/CONFORMANCE-FINDINGS.md`.
If an experiment is impossible with available local generators, leave its row
open with the failed command and a named owner rather than replacing it with a
nearby case. L1 instead records the public specification URLs and the resulting
classification.

`docs/README.md` is generated; regenerate its index when the shared tree is
quiet rather than editing it alongside concurrent documentation work.

## Configuration

The conformance runner discovers an installed reference through its normal
reference configuration. Corpus media is cache-first and may fetch only when
`VACO_CORPUS_NETWORK=1`; the calibration recipes should prefer locally
generated media and use a temporary directory. P1, T1, and K3 additionally use
the `python3` standard library for controlled fixture plumbing; P1's helper is
a loopback-only, one-request HTTP response with a controlled MIME type.

## Dependencies

`vaco-conformance` supplies hermetic process execution and reference discovery.
`vaco-corpus` resolves a `corpus://` asset only where a local generator cannot
produce the needed construct. The only reference executable used for black-box
measurements is the installed `ffmpeg`/`ffprobe` binary.

## Current measurements

These observations were made on ffmpeg/ffprobe 9.0.1 on 2026-09-04. They are
not substitutes for the executable calibration cases; they establish the first
values the cases must preserve or deliberately version-gate.

| ID | Observation | Evidence |
| --- | --- | --- |
| P1 | A `video/webm` MIME type raises an otherwise zero-score mangled EBML input to Matroska/WebM score 30: ffprobe records `score:0 increased to 30 due to MIME type`, then rejects the malformed header. A final `probe_score` field is unavailable because parsing fails. | A one-request localhost server served a `.bin` file with destroyed EBML magic and an explicit `Content-Type`; ffprobe debug diagnostics recorded selection and score before the expected parse failure. |
| P2 | The initial AAC content probe is 2,048 bytes (`score=51`). For a raw ADTS payload after zero padding, the final accepted first-syncword offset is 1,044,480 bytes (`score=25`); exactly 1,048,576 bytes is rejected. | ffprobe debug reported the initial probe size, then locally generated ADTS files established both sides of the one-MiB boundary. |
| P3 | A normal Matroska auto-probe reports `probe_score=100`; forcing `-f matroska` reports `probe_score=0`, not 100. | A locally generated 64x48 MPEG-4 Matroska file was queried both ways with `-show_format`. |
| P4 | In an MPEG-TS stream that changes from 25 fps to 30 fps, four initial 25 fps frames report `r_frame_rate=30/1`; five initial frames report the `150/1` common cadence. The default rate probe incorporates the initial cadence at five frames. | Each generated 25 fps prefix was concatenated with the same 60-frame 30 fps MPEG-2 TS suffix, then queried for the selected top-level stream’s `r_frame_rate`. |
| P5 | For millisecond-timebase Matroska, 23.976 and 29.97 fps retained `24000/1001` and `30000/1001`; 59.94 and 119.88 fps were represented as `19001/317` and `29011/242`. | Four locally generated 64x48 MPEG-4 Matroska files at 24000/1001, 30000/1001, 60000/1001, and 120000/1001 fps, queried for `r_frame_rate`, `avg_frame_rate`, and `time_base`. |
| P6 | A one-frame 25 fps MP4 and Matroska stream each report `r_frame_rate=25/1`. A one-frame 25 fps MPEG-TS encode reports `r_frame_rate=50/1` (and `avg_frame_rate=0/0`); this case must preserve the codec/container distinction rather than assuming the generator rate. | Locally generated 64x48 one-frame MPEG-4 MP4/MKV and MPEG-2 TS files, queried with `-show_entries stream=r_frame_rate,avg_frame_rate,nb_frames,duration,time_base`. |
| P7 | In a two-program TS, program 2’s audio offset of 7 seconds is analyzed as MP2 at `8.429089` with `1.018778` duration. At offset 8 seconds, its PMT is still declared but no audio payload reaches the default analysis window: it remains generic MP3 at the video stream’s `1.440000` start and `9.000000` duration. | Two otherwise equivalent MPEG-2+MP2 transport streams placed audio PES on program 2 after seven and eight seconds; selected audio fields distinguish actual packet analysis from PMT discovery. |
| T1 | A one-second MPEG-TS with `mpegts_copyts=1` and output offset 95443 crosses the raw 33-bit PTS boundary. ffprobe normalizes it to `start_time=-0.717689`, `duration=1.000000`; a `-read_intervals 0.5%+0.2` seek returns packet PTS,DTS `0.242311,0.202311`. | The fixture parser asserted a decrease in raw PES PTS values before ffprobe checked the container fields and post-wrap seek packet. |
| T2 | MP4 format `start_time` is the earliest stream start: audio at `0.000000` plus video offset by 0.041708 seconds reports format/audio `0.000000` and video `0.041667` (the stream's 24 fps tick). | Locally generated AAC+MPEG-4 MP4 with output-side video input offset, queried for format and per-stream start times. |
| T3 | Changing a 12 s MP4's version-0 `mvhd.duration` to 10 s (at the generated 1000 Hz movie timescale) leaves both format and stream duration at `12.000000`. The longest track, not `mvhd`, wins. | The exact four duration bytes were patched after the generated `mvhd` box type and checked with `xxd`; ffprobe then read format and stream duration. |
| T4 | A 2 s 25 fps MPEG-TS remains `2.000000` after removing its last two 188-byte TS packets. Removing three is the first wrong tail point, reducing duration to `1.960000`. | The complete fixture and two exact tail truncations were queried for format duration. |
| T5 | For a B-frame MPEG-4 MP4 remuxed to MPEG-TS, `auto`, `make_zero`, and `make_non_negative` are byte-identical. `disabled` differs and leaves the first packet 40 ms earlier: `1.400000,1.360000` rather than `1.440000,1.400000` PTS,DTS. | Four `-c copy` MPEG-TS muxes from the same 25 fps two-B-frame source; bytes were compared with `cmp` and first packets queried with `-show_entries packet=pts_time,dts_time`. |
| S1 | A seekable TS at one-hour timestamps returns packet `3600.040000,3600.000000` for a 3600-second interval. The same TS through `pipe:0` consumes all 33,464 bytes, then refuses: it does not forward-discard an unseekable input. | A counted 188-byte producer fed ffprobe’s interval request; the script asserts complete consumption and both refusal diagnostics. |
| M1 | An MP4 whose video chunk precedes a delayed audio chunk emits all 25 video packets before all 45 audio packets, with one stream-index transition across 70 packets. | A one-second MPEG-4 video input and one-second AAC input offset by one second were muxed to MP4; complete packet stream-index output was counted. |
| M3 | A version-0 `elst` changed from media rate 1.0 to 2.0 (`00010000` to `00020000`) has no observed effect: all 50 packet PTS/DTS/durations and the `2.000000` format duration match the original. | A two-second 25 fps MP4 was patched only in the media-rate field; complete packet CSV output was compared byte-for-byte. |
| M5 | With a fixed test key/KID and `-fflags +bitexact`, two CENC AES-CTR MP4 muxes are byte-identical. The generated file contains `schm`, `cenc`, `tenc`, and `senc` and keeps a table-level `nb_frames=25`. | Two independently muxed 25-frame MPEG-4 MP4s were compared with `cmp`; CENC box identifiers and the resulting frame count were asserted. |
| M7 | Halving a faststart MP4's `mdat` leaves its table-level `nb_frames=25` and `duration=1.000000`, while only 13 packets remain readable. | A 64x48 25-frame MPEG-4 MP4 was truncated to half its byte length; `-count_packets` distinguished table count from readable packet count. |
| K2 | An Info `Duration` payload set to the exact eight-byte IEEE-754 representation of `12345.6789` Matroska ticks (`40c81cd6e631f8a1`) reports `12.345678` seconds. The final fractional microsecond is truncated. | A generated eight-byte Duration element was patched in place and its payload bytes plus ffprobe format duration were asserted. |
| K3 | A nested `SimpleTag` named `CHILD` inside parent tag `PARENT` is flattened as `PARENT/CHILD=VALUE`. | A generated Tags master was extended by a 34-byte standards-shaped nested Tag; both finite EBML sizes and the inserted bytes were checked before querying ffprobe format tags. |
| K4 | Two identical Matroska muxes with output-side `-fflags +bitexact` were byte-identical: 9,148 bytes and SHA-256 `95497f31bbe6e5082dda2c22e1b786c38f44a05658f82c4fa4d779fff00eec90`. | Two independent locally generated MPEG-4 Matroska files were compared with `cmp` and SHA-256. |
| A1 | `asf` and `asf_o` are observably distinct. On the same locally generated WMAv2 ASF file, `format_name` is `asf` versus `asf_o`, and the encoder tag is `encoder` versus `WM/EncodingSettings`. | `ffprobe -f asf` and `ffprobe -f asf_o`, both with JSON stream and format output. |
| N1 | With two 25 fps video streams whose DTS values are equal at every 40 ms tick, both Matroska and MP4 emit stream 0 before stream 1. The first four packet pairs are `0@0`, `1@0`, `0@0.04`, `1@0.04`. | Two locally generated MPEG-4 tracks were remuxed without re-encoding to each container, then queried with `-show_entries packet=stream_index,dts_time`. |
| L1 | WavPack and TTA each publish a format description, so neither is source-only by the evidence required for this classification. | The [WavPack binary format](https://www.wavpack.com/WavPack5FileFormat.pdf) and [TTA format description](https://tta.sourceforge.net/en/tta-format-description/) describe their on-disk structures. |

The remaining black-box rows remain open until their listed procedure has an
automated assertion and a recorded result. In particular, do not infer P3 from
an auto-detection probe or A1 from successful decoding; both measurements show
that those superficially similar paths answer different questions.
