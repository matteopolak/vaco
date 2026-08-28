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

## Decode accuracy — measured, not claimed

None of this is bit-exact: decode runs in `f32`, not the ISO reference
decoder's fixed-point contract, so byte-identical output was never a goal.
What follows is what direct comparison against `ffmpeg 8.1`-decoded PCM
actually showed, per layer, generated via
`ffmpeg -f lavfi -i "sine=..." -c:a libmp3lame/mp2 ...` fixtures (a 440 Hz
tone unless noted) and diffed sample-for-sample after finding the
best-aligning offset (`compare.py`/`compare2.py`, scratch scripts, not
committed).

| Layer | Rate × channels tested | Result |
|---|---|---|
| I | none (no MP1 encoder available: neither `ffmpeg`'s build here nor any other tool on this machine can produce one) | Not verified against real audio. Header parsing, bit allocation (4-bit index, direct `nb = bal+1` dequant) and the synthesis filterbank are exercised by unit tests only (`layer1.rs`, `synthesis.rs`), plus the shared filterbank's correctness is established transitively by Layer II's real-file results below (same `Synthesis::synth_block` code, unmodified). |
| II | 32000/44100/48000 Hz × mono/stereo (6 fixtures) | **Matches closely.** RMS error 1.2–10.7 (of a 32767 full-scale `i16`), cross-correlation 1.0000 at zero sample shift once a real bug (below) was fixed. Not bit-exact (float vs. fixed-point, plus the four MPEG-1 bit-allocation tables are used but the low-sample-rate table and intensity stereo are not — see gaps below), but close enough that the remaining error is plausibly rounding, not a structural mistake. |
| III | 44100 Hz mono/stereo, 440 Hz and 6000 Hz tones | **Not accurate.** A dedicated unit test (`layer3::frequency_placement_tests`) proves the subband-splitting → IMDCT → windowing → overlap-add → synthesis-filterbank half of the pipeline places a known spectral line at its correct output frequency in isolation. Full end-to-end decode of a real encoded file is still wrong: a 440 Hz tone reaches only ~0.44 sample correlation against `ffmpeg`'s decode after finding the best time alignment, and a 6000 Hz tone comes out at a measurably wrong frequency (~4316 Hz instead of 6000 Hz). Two real bugs were found and fixed this session (below); the remaining error is narrowed to the side-information/Huffman-decode half (verified independently correct in isolation from the transform half) but not yet found. **Report this as broken, not "close."** |

### Bugs found and fixed this session (for the record, not just interest)

- **Layer III global-gain constant.** ISO/IEC 11172-3's own text names the
  formula's scaling constant "64" (`2.4.3.4`), but that section's actual
  formula is a lost image in this crate's PDF-to-text extraction — only the
  surrounding prose survived. Implementing literally with `64` produced
  samples ~10⁷ too large. `210` (confirmed empirically against `ffmpeg`,
  not by citation — see `layer3.rs`'s `decode_granule`) produces sane
  magnitudes.
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
- `layer3.rs`: side info (`parse_side_info`), a bit reservoir
  (`Layer3State::reservoir`, capped at 4 KiB — comfortably more than the
  511-byte maximum `main_data_begin` backward reference plus one frame),
  Huffman decode (`huffman.rs`/`huffman_data.rs`), the requantisation
  formula, MS stereo, alias reduction (`apply_alias_reduction`), the IMDCT
  via `vaco_tx::reference::imdct` (see "Known gaps" — this is the
  O(n²) reference transform, not a fast path), four window shapes
  (`window_value`), overlap-add and frequency inversion before the shared
  synthesis filterbank.
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

- **Layer III does not work correctly on real content yet** (see the table
  above). Root cause not yet found; narrowed to the side-info/Huffman half
  by `layer3::frequency_placement_tests` proving the transform half correct
  in isolation.
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
- **MPEG-2/2.5 (low sample rate) Layer III returns `Unsupported`.** The
  low-sample-rate extension's different `scalefac_compress` decomposition
  (three ranges of the 9-bit field, further split under intensity stereo)
  is not implemented. Layer I/II's low-sample-rate bit-allocation table
  (`LAYER2_TABLE_LSF`) is transcribed and referenced but untested against
  real audio (no MPEG-2/2.5 encoder available either).
- **Free-format streams**: the demuxer measures free-format frame length
  by scanning for the next sync (`vaco-demux-mpegaudio`); this crate's
  decoders take whatever payload length the demuxer hands them and have no
  free-format-specific logic of their own, so this should work but is
  untested (no free-format encoder available).
- **Gapless playback**: `PacketSideData::SkipSamples` (the demuxer's own
  LAME-tag-derived trim) is not consulted by `MpegAudioDecoder` — decoded
  output includes the encoder's priming delay and any padding. Trimming
  belongs at the point that owns both the packet's side data and the
  decoded frame, which today is neither this crate nor `vaco-cli` (#652).
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
- The remaining Layer III real-file bug: start by comparing `is[]`
  (post-Huffman, pre-requantisation) against a known-good reference for a
  single, hand-constructed granule, since `layer3::frequency_placement_tests`
  already rules out the transform half.
- Intensity stereo: Layer I/II need the `bound`/`sblimit` split
  (`mode_extension` selects it via a fixed table); Layer III needs the
  `is_ratio = tan(is_pos·π/12)` reconstruction using the "side" channel's
  scalefactors as intensity positions.

## Configuration

None — no feature flags, no options. `DECODER_MP1`/`DECODER_MP2`/`DECODER_MP3`
are unconditional in the default build (MPEG-1/2 Layer I/II/III are all
GREEN per `planning/research/07-legal-patents-licensing.md`: Layer II's
patents expired before Layer III's, and MP3's own programme terminated
2017-04-23).

## Dependencies

`vaco-format-mpegaudio` (header/bitrate tables), `vaco-bitstream`
(`BitReader`, `Mark`/`restore`/`skip_long` for the bit-reservoir resync),
`vaco-tx` (`reference::imdct` — the O(n²) verification transform, not a
fast path; see "Known gaps"), `vaco-frame`/`vaco-packet`/`vaco-sampfmt`/
`vaco-chlayout` (the `Decoder` trait's data model), `vaco-limits`
(`Budget`/`Limits`).
