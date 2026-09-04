# `vaco-codec-mpegaudio`

Layer 4. MPEG-1/2/2.5 Audio Layer I/II/III decode (issues #362, #363, #364).

## What it is

One decoder crate covering all three MPEG audio layers, registered under
three names (`mp1`, `mp2`, `mp3`) because the registry lists them
separately even though [`MpegAudioDecoder`] dispatches on the frame
header's own `layer` field rather than needing three implementations.
Layers I, II and III share one frame header and bitrate/sample-rate table
set (`vaco-format-mpegaudio`) and one 32-band polyphase synthesis
filterbank (`synthesis.rs`); Layer III adds side information, a bit
reservoir, Huffman decoding, requantisation, MS stereo and the IMDCT
(`layer3.rs`).

Written from the actual ISO/IEC 11172-3 (`dist10` reference distribution)
and ISO/IEC 13818-3 text — not from ffmpeg source — per D6/D7. Constant
tables ≥32 elements are declared in `provenance/vaco-codec-mpegaudio.toml`.

**Encoders (#365/#366) are out of scope for this crate and epic #38 stays
open**; the `vaco-format-mpegaudio`/`vaco-demux-mpegaudio`/
`vaco-mux-mpegaudio` crates (issue #644) cover the container half.

**No CLI path yet.** `vaco-cli` cannot currently select a decoder by name
(issue #652, tracked and fixed separately); this crate's own conformance
numbers below come from calling [`MpegAudioDecoder`] directly (see
`examples/decode_dump.rs`) against `vaco-demux-mpegaudio`'s packets, then
diffing the resulting `s16le` PCM against `ffmpeg -f s16le -`.

**`tests/oracle_ffmpeg.rs` is the first committed regression test against
real audio.** Everything above and below this note was, until now, measured
once by a scratch script against hand-generated fixtures that were never
checked in — real bugs were found and fixed this way, but nothing was left
behind to catch a regression. The committed test covers Layer II (a passing
128 kbit/s fixture and an `#[ignore]`d 64 kbit/s one, see "Known gaps" for
the real bug it caught) and Layer III (one passing multitone stereo
fixture); Layer I still has no fixture (no MP1 encoder is available
anywhere on this machine).

## Decode accuracy — measured, not claimed

None of this is bit-exact: decode runs in `f32`, not the ISO reference
decoder's fixed-point contract, and the specification itself defines a
compliance tolerance rather than one correct output — byte-identical
output was never the target here. Correlation approaching 1.0 with a
small RMS (of a 32767 full-scale `i16`) is. What follows is direct
comparison against `ffmpeg 8.1`-decoded PCM (`-acodec pcm_s16le -f s16le
-fflags +bitexact`, the flag placed immediately before the output path),
per layer, over 12 fixtures generated via `ffmpeg -f lavfi -i "sine=..."
-c:a libmp3lame/mp2 ...` at varied bitrate/sample-rate/stereo-mode/
frequency combinations, aligned per channel by FFT cross-correlation
before scoring (`mp3_compare.py`, scratch script, not committed).

| Layer | Fixtures tested | Result |
|---|---|---|
| I | none (no MP1 encoder available: neither `ffmpeg`'s build here nor any other tool on this machine can produce one) | Not verified against real audio. Header parsing, bit allocation (4-bit index, direct `nb = bal+1` dequant) and the synthesis filterbank are exercised by unit tests only (`layer1.rs`, `synthesis.rs`), plus the shared filterbank's correctness is established transitively by Layer II's real-file results below (same `Synthesis::synth_block` code, unmodified). |
| II | 32000/44100/48000 Hz × mono/stereo: original 6 fixtures plus a 30-case bitrate-boundary sweep | **Matches closely.** The post-fix sweep covered both modes and every MPEG-1 allocation-table boundary: 32/48, 56/64/80, and 96 kbit/s per channel (with 48 kHz's 96 kbit/s still in B.2a). All 30 outputs had exact sample counts, 0.999999993 minimum correlation, 0.871 maximum RMS error, and maximum sample difference 2 versus `ffmpeg 9.0.1`. Not bit-exact (float vs. fixed-point, plus the four MPEG-1 bit-allocation tables are used but the low-sample-rate table and intensity stereo are not — see gaps below), but the remaining scatter is rounding, not a structural mistake. |
| III | 12 fixtures: mono/stereo/independent-stereo/VBR, 32000/44100/48000 Hz, 64k–320k and VBR q2, 220 Hz–15000 Hz tones and a two-tone mix | **Matches closely, one real bug found and fixed this pass.** Correlation 0.975–0.997 across every fixture, RMS 113–441. Before this pass a 440 Hz tone reached only ~0.94–0.98 correlation depending on rate/bitrate and a 6000 Hz tone or a 64 kbit/s 32000 Hz fixture reached ~0.01–0.18 (near-zero — the two failed for genuinely different reasons, exactly as a "positive-but-poor" vs. "near-zero" correlation split predicts: block-type distribution across every fixture was checked first and ruled out short blocks as the cause, since it's ~1.3% short in every fixture regardless of content or bitrate). Root cause: `region0_end`/`region1_end` (the Huffman-table-selection boundaries within a granule's "big values") were computed as `sfb[region_count[0]]`/`sfb[region_count[0]+region_count[1]]` directly, when `region_count[0]`/`[1]` each hold *one less than* the actual scalefactor-band count for that region (`Vaco-Spec-Ref: iso-11172-3`, corroborated independently against a technical description of the format) — the correct index is `sfb[region_count[0]+1]`/`sfb[region_count[0]+region_count[1]+2]`. A signal concentrated in the first couple of bands (a low tone) barely reaches the misclassified boundary; content occupying more of the spectrum (a higher tone, or anything past the first two regions) gets Huffman-decoded there with the wrong table, which looks like plausible garbage rather than a bitstream desync. Still not bit-exact, and short blocks / intensity stereo remain unimplemented (see "Known gaps") — closed on correlation, not on completeness. **A second real bug was found and fixed in a later pass**: the MPEG-1 long scalefactor-band tables were one boundary short, silently zeroing every spectral line above 16.03 kHz at 44100/48000 Hz — see the dedicated section below for the measurement. |

### The MPEG-1 long scalefactor-band tables were one boundary short

Measured against `ffmpeg 9.0.1` on full-band pink noise, decoding to
`-f s16le` and comparing power spectra with an 8192-point Blackman window:

| band | ffmpeg | before | after |
|---|---|---|---|
| 1–4 kHz | 102.4 dB | 102.4 | 102.5 |
| 14–16 kHz | 94.2 dB | 94.2 | 94.0 |
| 16–17 kHz | 93.5 dB | **71.3** | 93.6 |
| 17–19 kHz | 93.3 dB | **23.4** | 93.2 |
| 19–21 kHz | 91.1 dB | **23.4** | 91.3 |

Everything above 16.03 kHz was 70 dB down on the reference at both 44100 and
48000 Hz. `SFB_LONG_32000`/`_44100`/`_48000` held 22 boundaries where the
low-sample-rate tables beside them hold 23 — the final `576` was missing, so
requantisation's `sfb.windows(2)` had no window for the last band and every
spectral line in it stayed zero. The Huffman decode had read those lines
correctly all along (instrumented: it fills to line ~487 of 576); they were
discarded one step later.

The symptom read as a fixed-frequency lowpass rather than a table error,
because the 21st boundary sits at 16005 Hz at 44100 Hz and 16000 Hz at
48000 Hz — the tables are designed around frequency, so two different line
indices land at the same place. 32000 Hz was affected too (its 21st boundary
is 550 of 576, 15278 Hz) but carries little there. The low-sample-rate rates
were unaffected in practice: their last band is above anything a real
encoder codes at 16/22.05/24 kHz.

Whole-file effect, `-f s16le` against the reference, mono pink noise at
128 kbit/s (mean absolute sample error out of 32768):

| rate | before | after |
|---|---|---|
| 32000 | 37.3 | 37.9 |
| 44100 | 343.0 | 46.0 |
| 48000 | 350.3 | 47.5 |

`every_long_sfb_table_reaches_the_last_spectral_line` and
`requantisation_covers_every_band_the_table_declares` (`layer3.rs`) hold both
halves of the invariant; both fail if either half is reverted.

### MPEG-2 low-sample-rate Layer III (issue #364) — landed; MPEG-2.5 explicitly gated off

MPEG-2 Layer III (16000/22050/24000 Hz) is a structurally different
`audio_data()` syntax from MPEG-1, not a parameter change: **one** granule
per frame rather than two, no `scfsi` field at all, an 8-bit
`main_data_begin` (not 9), 1/2-bit `private_bits` for mono/stereo (not 5/3),
a 9-bit `scalefac_compress` (not 4) decomposing into **four**
scalefactor-length groups instead of two, and `preflag` derived from which
of three `scalefac_compress` ranges applies rather than transmitted as its
own bit. `parse_side_info`/`decode_granule` (`layer3.rs`) branch on
`is_lsf = header.version.is_low_sample_rate()` for all of the above; each
granule is still exactly 576 lines regardless of MPEG version, so the
downstream requantisation/stereo/IMDCT/synthesis pipeline is shared
unchanged. Written from ISO/IEC 13818-3's own `audio_data()`/`main_data()`
syntax tables and `scalefac_compress` decomposition formula (§2.4.1.2,
§2.4.2.7), not from a description of the format.

| Fixture | Correlation | max_abs | RMS |
|---|---|---|---|
| mono, 16 kbit/s, 16000 Hz | 0.9849 | 4545.5 | 476.90 |
| mono, 48 kbit/s, 16000 Hz | 0.9569 | 10774.5 | 813.51 |
| stereo, 64 kbit/s, 22050 Hz | 0.9757 | 5415.2 | 429.22 |
| stereo, 80 kbit/s, 24000 Hz | 0.9907 | 3150.4 | 265.26 |

All four generated directly by `ffmpeg -ar <rate> -c:a libmp3lame`, compared
the same way as the MPEG-1 Layer III fixtures above (real encoder output,
not hand-built). Matches closely across every sample rate and both a low
and a higher bitrate — closed on correlation, same standard as MPEG-1.

**MPEG-2.5 (8000/11025/12000 Hz) is deliberately left returning
`Error::Unsupported`, not implemented-and-hoped.** MPEG-2.5 was never an
ISO standard — ISO/IEC 13818-3 defines MPEG-2 only — so there is no primary
text to check its scalefactor-band geometry against. Every public
description claims MPEG-2.5 reuses MPEG-2's own long-block tables unchanged
for the corresponding halved rate (8000↔16000, 11025↔22050, 12000↔24000).
That claim was implemented and then tested against real
`ffmpeg`-produced MPEG-2.5 fixtures, varying bitrate independently of
sample rate to rule out bitrate as a confound:

| Sample rate | Correlation (varying bitrate) | Verdict |
|---|---|---|
| 8000 Hz | 0.10 (16 kbit/s), 0.32 (48 kbit/s) | Wrong — bitrate-independent, so this is a geometry mismatch, not undertrained content |
| 12000 Hz | ~0.79 | Wrong |
| 11025 Hz | ~0.98 | Passes, but read as a fixture that doesn't exercise the mismatched bands rather than confirmation, since the other two rates falsify the shared-table premise outright |

The shared-table assumption is real and disproven for at least two of the
three rates, not merely "unverified." Shipping it silently would mean two
of three MPEG-2.5 sample rates decode to audible garbage while reporting
success. `layer3::decode` therefore rejects `Version::Mpeg25` outright with
a descriptive `Unsupported`, and the (falsified) `SFB_LONG_8000/11025/12000`
constants are kept in `tables.rs` only as a record of the assumption that
was tried, not as active code (see that file's doc comment). Finding the
actual MPEG-2.5 geometry — if one exists independent of MPEG-2's — is future
work; per-issue disposition is in the "Known gaps" section below.

### Bugs found and fixed (for the record, not just interest)

- **Layer II grouped quantisation.** Annex B.2a is already the correct
  allocation table for 56/64/80 kbit/s per channel at 32/44.1 kHz; its header
  was checked directly against ISO/IEC 11172-3 Annex B. The actual failure
  was in `layer2_dequant_grouped`: it mapped each base-3/base-5/base-9 digit
  evenly across `[-1, 1]`, instead of treating it as an implied 2/3/4-bit
  Layer II sample code before applying its quantisation class's `C`/`D`.
  The middle value of a 3-level class consequently decoded as 2/3 rather
  than 0. The committed 64 kbit/s mono oracle test failed at correlation
  0.922275 and RMS 4879.2 before the correction; it now runs normally, and
  the 30-case sweep above checks every related bitrate/sample-rate/mode
  boundary against `ffmpeg`.
- **Layer III Huffman-region boundary off-by-one.** See the accuracy table
  above — this is the fix that took real-file correlation from ~0.01–0.98
  (content-dependent) to a consistent 0.975–0.997.
- **Layer III global-gain constant, re-examined.** A second, independently
  obtained scan of the same ISO CD 11172-3 committee draft made the
  requantisation formula (previously lost to an embedded-image PDF
  extraction) legible: `Vaco-Spec-Ref: iso-11172-3` §2.4.3.4 states the
  gain term's constant as `64`, not `210`. Tried literally: `64` reproduces
  the formula but clips real fixtures to full scale and *drops*
  correlation relative to `210` on content this crate already decoded
  plausibly. Not a contradiction — the spec's own text immediately after
  the formula explains why: "The constant 64 ... is needed to scale the
  output appropriately ... If an implementation with a different power
  transfer characteristic is chosen (different global scaling) then the
  constant has to be changed accordingly." `64` is calibrated to the
  reference decoder's own synthesis-gain convention; `210` remains this
  crate's own empirically-correct constant for the same role, now
  understood rather than treated as an unexplained hack. The formula
  structure and the scalefactor-exponent term were checked term-by-term
  against the primary text and match exactly.
- **Layer III silent-granule bit budget.** A granule with `big_values == 0`
  and `part2_3_length == 0` (a genuinely silent granule — e.g. the "side"
  channel of an MS-stereo-encoded mono source) was still being fed into the
  `count1` Huffman-decode loop, which read real bits belonging to whatever
  came next in the bit reservoir and manufactured spectral energy that was
  never transmitted. Fixed by bounding both Huffman loops with
  `r.bit_pos() < granule_end_bit` (`part2_3_length` is the only
  authoritative bound), not just "576 lines decoded."
- **Layer II sample count.** An early version of the per-subband sample
  loop advanced its output index by 1 per ungrouped read instead of 3,
  decoding only 384 of each frame's 1152 samples per channel and reading a
  third of the bits the frame actually needed — this alone took real-file
  correlation from ~0.04 (effectively noise) to ~0.25.
  See `Vaco-Spec-Ref: iso-11172-3` §2.4.1.6.
- **Layer II loop nesting (the one that mattered most).** ISO/IEC 11172-3's
  own pseudocode for the sample-reading step is granule-major:
  `for (gr=0; gr<12; gr++) for (sb...) for (ch...) { ... }` — one sample
  (or one grouped codeword) per allocated subband for granule 0, then the
  same for granule 1, and so on. An earlier version of this crate nested
  subband outside granule, which reads the *right total number of bits* (so
  a frame still ends in the right byte) but from the *wrong positions* from
  the second allocated subband onward. This is the fix that took real-file
  correlation from ~0.25 to **1.0000**; see `layer2.rs`'s decode step 4 for
  the corrected nesting and the comment explaining why the old order looked
  almost-right.

## How it works

- `header.rs`/`bitrate` tables live in `vaco-format-mpegaudio` (issue
  #644), not here — this crate takes an already-parsed
  `MpegAudioHeader` and a packet's payload.
- `synthesis.rs`: the 32-band polyphase filterbank, one `Synthesis` per
  channel holding a 1024-sample FIFO history. `Nik = cos[(16+i)(2k+1)π/64]`
  and the 512-tap window `Di` (`Vaco-Spec-Ref: iso-11172-3` Annex B Table
  3-B.3), both confirmed against the actual standard text. Shared
  unmodified by all three layers.
- `bitalloc.rs`: the shared "invert the MSB, read as fractional two's
  complement" dequantisation (`code_to_fraction`), Layer I's closed-form
  `C = 2^nb/(2^nb-1)`, `D = 2^(1-nb)`, Layer II's table-driven `(C, D)`
  lookup and its bit-allocation table selection
  ((sample rate, bitrate/channel) → one of `LAYER2_TABLE_A/B/C/D/LSF`).
- `layer1.rs`: 4-bit allocation index directly, no table; 12 granules ×
  32 subbands.
- `layer2.rs`: table-driven allocation, 3-scalefactor-group transmission
  pattern (`SCFSI_PATTERN`), grouped-triple degrouping
  (`layer2_dequant_grouped`), granule-major sample order (see the bug
  writeup above — this is the part that was wrong).
- `layer3.rs`: side info (`parse_side_info`, branching on `is_lsf` for
  MPEG-2's one-granule/no-scfsi/8-bit-`main_data_begin`/9-bit-
  `scalefac_compress` layout — see the MPEG-2 section above), a bit
  reservoir (`Layer3State::reservoir`, capped at 4 KiB — comfortably more
  than the 511-byte maximum `main_data_begin` backward reference plus one
  frame), Huffman decode (`huffman.rs`/`huffman_data.rs`), the
  requantisation formula, MS stereo, alias reduction
  (`apply_alias_reduction`), the IMDCT via `vaco_tx::reference::imdct` (see
  "Known gaps" — this is the O(n²) reference transform, not a fast path),
  four window shapes (`window_value`), overlap-add and frequency inversion
  before the shared synthesis filterbank.
- `huffman.rs`: table lookup by linear scan (correctness-first; a real
  decode tree is future work), `decode_big_value`'s escape (`linbits`) and
  sign handling, `decode_count1`'s quad sign handling. Every one of the 32
  "big values" tables plus both `count1` tables passed a Kraft's-inequality
  and prefix-collision unit test (`every_table_is_a_complete_prefix_free_code`)
  before this crate trusted them — a transcription-error detector that does
  not require knowing any codeword in advance.

## Known gaps

Reported plainly rather than glossed over, per the issue's own acceptance
criteria:

- **Short blocks (`block_type == 2`) decode to silence**, not their actual
  audio, for Layer III — this crate does not implement the short-block
  scalefactor layout (band-major, window-minor, 12 bands × 3 windows) or
  the per-window 12-point IMDCT. A frame containing pure-short or mixed
  granules loses that granule's content; everything else in the frame is
  unaffected (each granule resynchronises independently via
  `part2_3_length`).
- **Intensity stereo is not implemented** for any layer — Layer I/II's
  `intensity_stereo` mode and Layer III's `mode_extension` intensity bit
  are not decoded; only plain stereo, dual-channel, mono and (Layer III)
  MS stereo are handled. Content using intensity stereo will decode
  incorrectly for the shared subbands.
- **MPEG-2 (not 2.5) low-sample-rate Layer III is implemented and verified**
  (issue #364; see the dedicated section above) — 16000/22050/24000 Hz.
  Layer I/II's low-sample-rate bit-allocation table (`LAYER2_TABLE_LSF`) is
  transcribed and referenced but still untested against real Layer I/II
  MPEG-2 audio (no such encoder available).
- **MPEG-2.5 Layer III (8000/11025/12000 Hz) returns `Unsupported`,
  deliberately.** The widely-repeated claim that it reuses MPEG-2's
  scalefactor-band tables was implemented and measured wrong for at least
  two of its three sample rates (correlation 0.10–0.32 at 8000 Hz, ~0.79 at
  12000 Hz) — see the dedicated section above for the full measurement and
  reasoning. Gated off rather than shipped silently wrong.
- **Free-format streams are implemented and verified** (issue #364):
  `vaco-demux-mpegaudio` derives the frame length once by scanning to the
  next sync, validates the candidate against that next frame's own header
  fields (version/layer/sample-rate/bitrate-index) before trusting it, then
  holds the base length constant for the rest of the stream — see
  `docs/format/vaco-demux-mpegaudio.md`. Verified against a hand-built
  fixture (no real free-format encoder was available; provenance noted in
  that doc's comparison table).
- **Gapless playback is implemented and verified** (issue #364):
  `MpegAudioDecoder::send_packet` now reads
  `PacketSideData::SkipSamples { start, end, .. }` off the incoming packet
  (already computed by the demuxer from the LAME/Xing tag plus the fixed
  decoder delay) and trims that many samples from the front/back of the
  decoded `Frame` via `trim_gapless`, which allocates a correctly-sized
  frame and copies the kept byte range rather than attempting an in-place
  shrink (`Plane` has no such operation). Verified exact: a 2-second VBR
  fixture with LAME encoder delay/padding decodes to precisely 88200
  samples, matching `ffprobe`'s own gapless-aware `duration_ts` exactly.
- **Huffman table lookup is a linear scan** over each table's entries
  (up to 256), not a decode tree — correct, not fast. Acceptable for a
  first native implementation; flagged rather than silently accepted as
  "done."

## How to change it

- Layer III's short-block gap is the highest-value follow-up: implement
  the band-major/window-minor scalefactor read and the 3×12-point IMDCT
  reassembly (concatenate three windowed 12-sample blocks with 6-sample
  zero padding at each end per `Vaco-Spec-Ref: iso-11172-3` §2.4.3.4's
  prose — see `layer3.rs`'s module doc for the exact reconstruction this
  crate already worked out but does not yet use).
- Intensity stereo: Layer I/II need the `bound`/`sblimit` split
  (`mode_extension` selects it via a fixed table); Layer III needs the
  `is_ratio = tan(is_pos·π/12)` reconstruction using the "side" channel's
  scalefactors as intensity positions.

## Configuration

None — no feature flags, no options. `DECODER_MP1`/`DECODER_MP2`/`DECODER_MP3`
are unconditional in the default build (MPEG-1/2 Layer I/II/III are all
GREEN: Layer II's patents expired before Layer III's, and MP3's own
programme terminated 2017-04-23).

## Dependencies

`vaco-format-mpegaudio` (header/bitrate tables, `Version::is_low_sample_rate`),
`vaco-bitstream` (`BitReader`, `Mark`/`restore`/`skip_long` for the
bit-reservoir resync), `vaco-tx` (`reference::imdct` — the O(n²)
verification transform, not a fast path; see "Known gaps"),
`vaco-frame`/`vaco-packet`/`vaco-sampfmt`/`vaco-chlayout` (the `Decoder`
trait's data model; `vaco-packet`'s `PacketSideData::SkipSamples` also
drives the gapless trim in `decoder.rs`), `vaco-limits` (`Budget`/`Limits`,
used both for decode and for `Frame::alloc_audio`'s reallocation in the
gapless trim path).
