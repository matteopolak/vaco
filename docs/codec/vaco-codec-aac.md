# `vaco-codec-aac`

Layer 4. AAC-LC decode and narrow ADTS silence encode (epic #53, T3-03/T3-04), patent-encumbered per D4. **Never
shipped**: gated behind the `patent-encumbered-aac-decode` feature, off by
default, and this doc will keep saying so until D4's legal posture changes.

## What it is

Configuration resolution (#443) **and** the full `raw_data_block()` syntax
layer (#444): `AudioSpecificConfig` (reusing `vaco-parse-aac`'s existing
parser), a full `program_config_element()` parser (`pce.rs`), a unifying
`DecoderConfig` (`config.rs`), and — new this pass — `ics_info()` (`ics.rs`),
`section_data()` (`section.rs`), `scale_factor_data()`'s three DPCM chains
(`scalefactor.rs`), `pulse_data()` and its application (`pulse.rs`),
`tns_data()`'s syntax (`tns.rs`), all eleven spectral Huffman codebooks plus
the differential-scalefactor codebook (`spectral_tables.rs`), the ESC
codebook's escape mechanism (`spectral.rs`), the thirteen `swb_offset`
tables (`swb_tables.rs`), and the `individual_channel_stream()`/
`raw_data_block()` drivers that tie all of it together
(`ics_stream.rs`/`raw_data_block.rs`).

[`AacDecoder`] (`decoder.rs`) fully parses every bit a real frame declares —
verified bit-exact against 677 real `ffmpeg`-encoded frames, see
"Decode accuracy" below — and, as of #445, reconstructs it into actual PCM:
inverse quantisation (`reconstruct.rs`), TNS application (`tns_apply.rs`),
joint stereo (M/S and intensity, `reconstruct.rs`), and the
IMDCT/windowing/overlap-add filterbank (`reconstruct.rs`, `vaco-tx`'s IMDCT,
`vaco-codec-dsp-sinewin`'s sine and KBD windows). **With #445 landed, AAC-LC
decode is complete end to end** — see "Decode accuracy" for what that is
measured against and "Known gaps" for what is still approximated or
unsupported.

### AAC-LC ADTS silence encode (#449)

`AacLcSilenceEncoder` is the first encoding slice, deliberately kept smaller
than a general AAC encoder: it accepts exactly one packed `S16`, mono or
stereo, 22.05, 24, 32, 44.1, or 48 kHz frame containing exactly 1024 zero samples per
channel. Mono uses a single long-window `SCE`; stereo uses one `CPE` with
`common_window = 0` and two such channel streams. Both use one `ZERO_HCB`
section per channel and end with `ID_END`. Non-silent PCM, another sample
format, another layout/rate, or a short/long frame is refused by name instead
of being encoded as silence.

The ADTS header uses AAC-LC profile, sampling-frequency index 3 (48 kHz) or 4
(44.1 kHz), 5 (32 kHz), 6 (24 kHz), or 7 (22.05 kHz), the matching mono/stereo channel
configuration, no CRC, and its exact access-unit length. Three emitted mono 48
kHz packets concatenated as an `.aac` elementary stream were read by `ffprobe
9.0.1` as `codec_name=aac`, `sample_rate=48000`, and `channels=1`; `ffmpeg
9.0.1` decoded exactly 3,072 samples / 12,288 `f32le` bytes. The independently
emitted 48 kHz, 44.1 kHz, 32 kHz, 24 kHz, and 22.05 kHz stereo streams each report their
matching rate and `channels=2`, then decode to exactly 3,072 samples per
channel (6,144 interleaved samples) / 24,576 `f32le` bytes. Matching 44.1 kHz,
32 kHz, 24 kHz, and 22.05 kHz mono streams likewise decode to exactly 3,072 samples /
12,288 `f32le` bytes. The crate's own decoder also reads each packet as one
1,024-sample frame with the matching rate and channel count. Silence has no
lossy-quality claim to compare — these checks establish framing and stream
shape, not general quantisation quality.

`AacLcSilenceAccessUnit::from_frame` is the separate mux-facing boundary. It
returns the unframed `raw_data_block()` bytes and the exact two-byte AAC-LC
`AudioSpecificConfig`: `11 88`/`11 90` for 48 kHz mono/stereo and
`12 08`/`12 10` for 44.1 kHz mono/stereo, and `12 88`/`12 90` for 32 kHz
mono/stereo, `13 08`/`13 10` for 24 kHz mono/stereo, and `13 88`/`13 90` for
22.05 kHz mono/stereo. A container writer
must attach that config as AAC extradata and use the raw payload as its packet
body; it must not place ADTS bytes in such a container. The existing
`AacLcSilenceEncoder` delegates to this raw builder and only adds ADTS, so it
remains useful for playable elementary streams. The stereo raw API tests wrap
three returned raw payloads only for the independent ADTS oracle: `ffprobe`
reports AAC, the matching 22.05/24/32/44.1/48 kHz rate, and two channels; `ffmpeg`
emits exactly 24,576 zero `f32le` bytes; Vaco also decodes each raw payload
directly after its matching ASC is set as extradata.

Both are direct APIs only, intentionally not registered generic encoders or
container integrations. Full AAC-LC encoding still needs transform,
quantisation, rate control, psychoacoustics, channel tools, and mux wiring.

**#446 (SBR/HE-AAC) is in progress, not landed.** This pass built and
verified the QMF analysis/synthesis filterbanks (`qmf.rs`) and all ten
SBR envelope/noise Huffman tables (`sbr_huffman_tables.rs`). An initial
verification round reported a broadband phase-coherence defect in the QMF
round trip; a follow-up round found it was a test-methodology false
alarm (a lag-search window that missed the filter's actual delay) rather
than a real bug, and the QMF foundation is now verified clean at
correlation > 0.99 for tones, two widely-separated tones together, white
noise, and a round-tripped impulse. `sbr_data()`'s bitstream syntax, HF
generation and envelope adjustment remain unbuilt regardless — each is a
substantial remaining piece, not blocked on anything found this pass. See
"SBR (#446) — what landed and what did not" below for the full account.

## Why this is gated (D4)

AAC is legally **RED**, not merely off-by-default for a portability reason:
the Via LA AAC pool is active and licenses per **decoder or encoder unit**,
not per bitstream. AAC *remuxing* —
reading a container's AAC track without ever instantiating a decoder — stays
in the default build and is what `vaco-parse-aac` already delivers; only
decode and the narrow encoder in this crate are gated.

**This is the first `encumbered = true` component in the tree.** Every
`cargo xtask patent-gate` run before this one reported "no component in the
tree is marked `encumbered = true` yet" — see that gate's own doc comment.
The shape chosen here, since there was no prior example to copy:

- `vaco-component.toml` sets `default = false` **and** `encumbered = true`
  together — D4.1 requires both; `gen-registry` refuses an encumbered
  component that is not also `default = false`.
- The feature name is `patent-encumbered-aac-decode`, matching epic #53's
  own title verbatim rather than inventing a variant spelling.
- [`DECODER_AAC`] additionally sets `caps: Caps::PATENT_ENCUMBERED` in code.
  This is a **second, independent** signal from the registry fragment: the
  fragment controls whether `vaco-codec-aac` is compiled into a given build
  at all (what `cargo xtask patent-gate` actually asserts on — see D4.1's
  own reasoning for checking the compiled artefact rather than a manifest's
  stated intent), while the `Caps` bit is a property of the `DecoderDesc`
  value itself, readable by any code that walks the registry's descriptors
  rather than inspecting the Cargo feature graph (a `-h decoder=` listing,
  a future gate). Neither substitutes for the other.

Verified structurally rather than by a full `cargo xtask patent-gate` run:
`cargo xtask gen-registry` regenerates `vaco-registry`'s `Cargo.toml` and
`generated.rs` correctly (`patent-encumbered-aac-decode` absent from the
`default` feature list; `vaco-codec-aac` an `optional` dependency gated
behind it; `ENCUMBERED_ALL` now `&["aac"]`, `ENCUMBERED_ENABLED`'s one row
`#[cfg]`-gated on the same feature), and `cargo check -p vaco-registry
--no-default-features --features patent-encumbered-aac-decode` compiles
cleanly. The live probe (`cargo xtask patent-gate`'s own compiled example)
could not be run to completion in the session that built this: two
*unrelated* in-progress crates elsewhere in this shared tree
(`vaco-format-misc-audio`, `vaco-filter-artistic`) did not compile at the
time, which blocks any workspace-wide default-feature build, this one
included — not a problem this crate's own gating introduced.
`crates/registry/vaco-registry/Cargo.toml` and `generated.rs` are
regenerated in the working tree to make this check possible but are not
committed by this crate's own commits — both are the orchestrator's sweep,
per `planning/AGENT-CONSTRAINTS.md`.

## How it works

### `AudioSpecificConfig` reuse, and the one piece it does not carry

`vaco-parse-aac::asc::AudioSpecificConfig` already parses everything a
container needs to report (sample rate, channel count, SBR/PS signalling)
and stops deliberately at the point its own doc names: "the program config
element ... is decoder configuration and cannot change what a container
reports." A decoder needs to go further exactly there, which is what this
crate's [`pce`] module is for.

### `program_config_element()` (`pce.rs`)

A full, new parser — no other crate in this workspace had a reason to read
one. Every field the syntax carries is kept (front/side/back channel element
lists as `(is_cpe, tag)` pairs, LFE tags, mixdown flags) except the comment
field's actual text, which has no effect on decode and is skipped by its own
declared length. [`ProgramConfigElement::channel_count`] and
[`ProgramConfigElement::element_order`] turn the parsed lists into what a
decoder actually needs: how many channels, and in what order to expect their
`SCE`/`CPE`/`LFE` bitstream headers.

The PCE's compact two-bit profile is expanded to the same `AudioObjectType`
vocabulary as `AudioSpecificConfig` (Main/LC/SSR/LTP), rather than being
mistaken for that vocabulary's numeric value. Before a pending configuration
accepts a PCE, it validates the resulting object type and the PCE's sampling
frequency index against the enclosing ADTS/ASC configuration. A mismatch is a
named invalid-data error, not permission to decode under conflicting settings.

For a PCE-derived configuration, the decoder retains that exact sequence,
including each element's four-bit tag. `raw_data_block()` retains the same
identity for every audio-bearing element and compares it before any PCM is
reconstructed. A different type, tag, count, or order is rejected as a PCE
channel-element-sequence mismatch rather than being decoded under a layout it
did not declare. Direct configurations do not use this check because their
element sequence is implied by `channelConfiguration`, not a PCE.

**[`find_leading_program_config_element`] only ever looks at the very first
syntax element of a `raw_data_block`.** A PCE that follows one or more
channel elements cannot be found this way: `SCE`/`CPE`/`CCE`/`LFE` carry no
length prefix, so skipping past one to keep looking requires actually
decoding it — #444's scope, not this crate's. Real encoders place a stream's
PCE first in its very first frame (the conventional, and by far the common,
placement); a nonconforming stream that puts its PCE later is a disclosed
gap — the caller gets `Ok(None)` and can report "channel layout
undetermined" honestly, rather than this function guessing at where an
element it cannot parse might end.

#### Direct 5.0: plane order verified against `ffmpeg`

An independently generated 48 kHz AAC-LC ADTS fixture with five distinct sine
tones (220/330/440/550/660 Hz) reports `channelConfiguration = 5` and a `5.0`
layout. Its 48 packets decode to **983,040 bytes** in both Vaco and
`ffmpeg -bitexact`: 48 × 1024 samples × 5 channels × 4-byte `f32` samples.
The raw element order is centre, front pair, back pair; Vaco permutes that to
native front-left/front-right/front-centre/back-left/back-right order. Comparing
each resulting plane to `ffmpeg` with `axcorrelate` produced RMS correlation
levels from −0.060 to −0.054 dB from unity, so the distinct tones rule out a
silent channel permutation while allowing AAC's normal floating reconstruction
differences.

#### Direct 4.0: plane order verified against `ffmpeg`

An independently generated 48 kHz AAC-LC ADTS fixture with four distinct sine
tones (220/330/440/550 Hz) reports `channelConfiguration = 4` and a `4.0`
layout. Its 48 packets decode to **786,432 bytes** in both Vaco and
`ffmpeg -bitexact`: 48 × 1024 samples × 4 channels × 4-byte `f32` samples.
The raw element order is centre, front pair, back-centre; Vaco permutes that to
native front-left/front-right/front-centre/back-centre order. Per-plane
`axcorrelate` RMS levels were −0.000283 to −0.000015 dB from unity, confirming
both the output count and the otherwise easy-to-miss centre/front-pair swap.

#### Direct 3.0: plane order verified against `ffmpeg`

Three separate 48 kHz sine sources (220/330/440 Hz) were joined explicitly as
`3.0` before AAC-LC ADTS encoding, so all three reference planes carry energy.
`ffprobe` reports `channelConfiguration = 3`, a `3.0` layout, and 48 packets.
Vaco and `ffmpeg -bitexact` each emitted **589,824 bytes**: 48 × 1024 samples
× 3 channels × 4-byte `f32` samples. The syntactic centre/front-pair order is
permuted to native front-left/front-right/front-centre; per-plane `axcorrelate`
RMS levels were −0.012389 to −0.012037 dB from unity, which verifies the map
with three independently non-silent signals.

### Channel-configuration coverage: 1, 2, 3, 4, 5, 6 direct; 0 via PCE; the rest gated

[`DecoderConfig::from_adts_header`] and
[`DecoderConfig::from_audio_specific_config`] both resolve
`channelConfiguration` 1 (mono), 2 (stereo), 3 (3.0), 4 (4.0), 5 (5.0) and 6
(5.1) directly — the overwhelming majority of real AAC-LC content, and the
six configurations this crate could state the exact `SCE`/`CPE`/`LFE` element
ordering for with confidence. `channelConfiguration == 0` resolves exactly,
from a real PCE,
via [`DecoderConfig::try_resolve_pending`].

The decoder also keeps a PCE's native output layout when its element order
already matches output plane order: front-centre mono, front stereo, and a
front `CPE` plus one `LFE` (`2.1`), or a front `CPE` plus back `CPE`
(`quad`). It also recognises the exact `3.0` PCE shape of one front `SCE` plus
one front `CPE`, whose centre/front-pair raw order is permuted into
`FL/FR/FC`. The corresponding `5.1` PCE shape adds one back `CPE` and one
`LFE`; its centre/front-pair/back-pair/LFE raw order is permuted into native
`FL/FR/FC/LFE/BL/BR`. The exact `7.1(wide)` PCE shape of one front `SCE`, two
front `CPE`s, one back `CPE`, and one `LFE` is likewise verified: its raw order
is front-centre, front-wide pair, front-left/right pair, back pair, LFE, and it
is permuted into native `FL/FR/FC/LFE/BL/BR/FLC/FRC` order. More complex PCEs
retain their exact channel count but remain layout-unspecified until their
required plane permutation is implemented and verified.

For raw ADTS, that PCE is normally present only in the first packet. Once a
leading PCE resolves a configuration, the decoder retains it for later ADTS
packets that repeat `channelConfiguration == 0` without repeating the PCE. A
later leading PCE still takes precedence, so an in-band configuration change
is not hidden by the cache; a cached PCE is reused only when object type,
sample rate, and frame length still match the current ADTS header.

The decoder consumes a leading PCE's exact byte span before it starts the
`raw_data_block` element loop. A PCE that follows audio elements is explicitly
rejected as `mid-stream program_config_element()` rather than silently ignored
while decoding under a stale configuration. Supporting a mid-stream update
requires carrying the new PCE through the in-flight configuration, output
layout, channel order, and overlap state; none of those can safely be changed
halfway through an already-decoded block.

**Configurations 7, 11, 12 and 14 are rejected with
`Error::Unsupported` rather than resolved from a recalled element ordering.**
ISO/IEC 14496-3's Table 42 states the exact `SCE`/`CPE`/`LFE` sequence each of
these implies, and this crate does not have that table's text in hand to
check a recollection against — the same situation, and the same "gate rather
than guess" resolution, this workspace already reached twice this session
(AC-3's exponent tables, MPEG-2.5's scalefactor bands): a wrong element-count
assumption here would desync every channel element's decode after the first,
silently. Extending this table is future work with a clear acceptance test
(the ISO/IEC 14496-26 conformance vector set #443's own issue names), not a
guess to ship now.

### Object-type gating

Only `AudioObjectType::AAC_LC` passes `DecoderConfig`'s internal
`gate_object_type`. Everything else — Main/SSR/LTP, the whole ER family,
and critically SBR/PS (`cfg.has_sbr()`) — is rejected with a specific
`Error::Unsupported` message naming what it is and which future issue
(#446/#447) covers it, rather than silently decoded as if it were LC.

### ADTS/LATM handover

`AacDecoder::send_packet` reuses whatever `Decoder::set_extradata` was given
(an MP4 `esds`'s `DecoderSpecificInfo`, or a LATM `StreamMuxConfig`'s inline
copy — both already `AudioSpecificConfig` by the time they reach this crate,
via `vaco-parse-aac`) when present, and otherwise parses a leading
`AdtsHeader` straight off the packet's own payload — the raw-ADTS case,
which carries no out-of-band configuration at all. Both paths converge on
the same `DecoderConfig`, so nothing downstream needs to know which framing
produced it.

### `raw_data_block()` (`raw_data_block.rs`) — the frame-level driver (#444)

Dispatches `id_syn_ele` (0..=7, ISO/IEC 14496-3 subpart 4 Table 4.68) in a
loop until `ID_END`: `SCE`/`LFE` each read one
`individual_channel_stream()`; `CPE` reads `common_window` and, when set, a
shared `ics_info()` plus `ms_mask_present`/`ms_used` (consumed, not applied
— M/S application is #445's "joint stereo") before its two channels;
`DSE`/`FIL` are skipped wholesale by their own declared byte count (both
are self-delimiting by construction, so nothing is lost by not decoding
what's inside); `PCE` is parsed in full; `CCE` is refused with
`Error::Unsupported` (see "Known gaps"). The outer loop reads `id_syn_ele`
with `BitReader::try_get`, not `get` — see "Bugs found and fixed" for why
that distinction is load-bearing, not stylistic.

### `individual_channel_stream()` (`ics_stream.rs`) — one channel, in bitstream order

`global_gain` (8 bits, absolute PCM — the seed for the regular-scalefactor
DPCM chain) → `ics_info()` (skipped if `common_window`, reusing the shared
one) → `section_data()` → `scale_factor_data()` → `pulse_data_present`
(+`pulse_data()`) → `tns_data_present` (+`tns_data()`) →
`gain_control_data_present` (refused if set — SSR-only, never legal for
LC) → `spectral_data()`, one call per window group, each producing a
full-length, zero-filled-where-untransmitted `x_quant` array (so
`pulse::apply`, and eventually #445, can index it by absolute frequency
line rather than needing separate band-offset bookkeeping).

### `section_data()` (`section.rs`) — the MP3 `region_count` trap, in another codec

`sect_cb` is **4 bits**, not 5 (the 5-bit form is an Error-Resilient-profile
feature, `aacSectionDataResilienceFlag`, never set for plain AAC-LC/ADTS).
`sect_len_incr`'s escape width is **3 bits for `EIGHT_SHORT_SEQUENCE`, 5
bits otherwise**, both checked against the primary text directly (ISO/IEC
14496-3 subpart 4 Table 4.52) rather than recalled, after the coordinator
named this exact failure shape — a band-boundary run-length field silently
producing plausible garbage instead of an error — as the one to watch for.
Runs once per window group, not once per frame: `EIGHT_SHORT_SEQUENCE` with
grouping can mean up to 8 independent `section_data()` calls in one block,
each covering its own `0..max_sfb`.

### `scale_factor_data()` (`scalefactor.rs`) — three DPCM chains sharing one codebook

Regular scalefactors (seeded from `global_gain`), intensity stereo
positions (seeded at 0, `sfb_cb` 14/15), and PNS noise energies (`sfb_cb`
13; the *first* occurrence in the whole channel is a raw 9-bit PCM value,
not Huffman-coded — `noise_pcm_flag` — seeded via `global_gain -
NOISE_OFFSET(90) - 256`) each keep their own running predecessor and are
never conflated, per §4.6.2/§4.6.8.2.3/§4.6.13.3's explicit "the decoder
ignores interposed [other-kind] values." Produces integer log-domain
values, not linear gains — the power-law conversion is #445's.

### `spectral_data()` (`spectral.rs`, `spectral_tables.rs`) — where `vaco-codec-vlc` earns its keep

All 12 Huffman tables (`SCALEFACTOR_HUFFMAN`, 121 entries; `SPECTRUM_HCB_1`
through `_11`, 64–289 entries each) transcribed from two independently
hosted copies of ISO/IEC 14496-3 (2001 and 2009 editions; two of the seven
long `swb_offset` tables were also cross-checked byte-for-byte between
editions) and each wrapped directly in `vaco_codec_vlc::VlcTable` — **no
interface changes were needed** to decode codebook 11's 289-entry alphabet,
answering the question of whether the linear-scan design generalises to
AAC's symbol rates. Every table additionally passes an exact-size check
against its own `(unsigned, dim, lav)` formula (Table 4.151) plus
`is_prefix_free`/`kraft_numerator` — the same transcription-error net this
workspace already held MP3's tables to, now proven on tables three times
larger. The index-to-n-tuple formula and the ESC codebook's variable-length
escape sequence are reproduced from §4.6.3.3's own pseudo-C, in the module
doc.

### Reconstruction (`reconstruct.rs`, `tns_apply.rs`) — #445

Turns #444's fully-parsed syntax into actual `f32` PCM, in the order
§4.5.2.2.5's own block diagram requires (joint stereo feeding TNS, not the
reverse — the diagram is why `reconstruct.rs` exposes three separate
functions, `deinterleave_channel`/`apply_joint_stereo`/`finalize_channel`,
rather than one that processes a channel pair one channel at a time):

1. **Inverse quantisation + scalefactor rescale** (§4.6.1.3/§4.6.2.3.3):
   `x_rescal = sign(x_quant)*|x_quant|^(4/3) * 2^(0.25*(sf-100))`, folded
   into one pass per raw coefficient (`inverse_quantize_and_rescale`).
2. **Perceptual noise substitution** (§4.6.13.3): a 32-bit LCG (Numerical
   Recipes' constants — the spec is explicit that PNS's RNG is
   non-normative, "a suitable random number generator can be realized
   using one multiplication/accumulation per random value") generates each
   noise band's samples, normalised to the transmitted energy
   (`dpcm_noise_nrg`, already decoded in #444's `scalefactor.rs`).
3. **Joint stereo** (`apply_joint_stereo`): M/S (§4.6.8.1.3, sum/difference
   on bands where `ms_used`/`ms_mask_present==2` is set, skipping
   intensity/noise bands — the three are mutually exclusive per band) then
   intensity (§4.6.8.2.3, deriving the right channel from the left at
   every intensity-coded band — no data of its own was ever transmitted
   there).
4. **TNS application** (`tns_apply.rs`): `tns_decode_coef`'s sign-extend +
   arcsine inverse-quantisation + Levinson-style LPC conversion, then
   `tns_ar_filter`'s in-place all-pole IIR filter, per §4.6.9.3's
   pseudocode — the syntax was already parsed and kept in #444; this pass
   adds the second half, actually filtering the spectrum.
5. **IMDCT** (`vaco_tx::{Plan, Tx}`, `TxKind::Mdct`/`Direction::Inverse`/
   `TxFlags::FULL_IMDCT`, C1, 2026-09-01): confirmed phase-compatible with
   §4.6.11.3.1's formula (`(n + 0.5 + N/4)(k+0.5)`) without modification,
   needing only an explicit `2/N` scale factor the caller applies (neither
   `vaco-tx` convention has a built-in normalisation here). Originally
   wired to `vaco_tx::reference::imdct` (SP-C6) — an `O(n²)` direct
   evaluation kept in the crate only as an oracle for tests — which made
   AAC decode measure 217x slower than `ffmpeg`, 88% of it in that one
   function and the `libm::cos` it called per coefficient pair. `ImdctPlans`
   (`reconstruct.rs`) now holds one `Tx<f64>` per block length (2048, 256)
   built once with `Plan::<f64>::new(Mdct, Inverse, n, 1.0, FULL_IMDCT)` —
   `f64` so the output is the reference's to `rms_rel < 1e-12`
   (`vaco-tx/tests/oracle.rs`, extended to `n = 2048` by this change; 256
   was already covered) rather than to a widened tolerance. Measured
   end to end: interleaved A/B against the pre-change binary, 44/44 rounds
   faster across 5 fixtures (mono/stereo/5.1, 22050/44100/48000 Hz),
   median wall-clock speedup 61x–99x, decoded PCM byte-identical to the
   pre-change output on every fixture (`shasum -a 256` over
   `-f s16le`, native rate, no resample) — confirming the wiring, not just
   the numerical agreement, since a differing sample there means the
   plan's convention was mismatched, not that rounding differs. Ratio
   against same-session `ffmpeg -threads 1`: was 147x–496x depending on
   fixture (machine under load from concurrent agents; same order as the
   217x baseline), now 2.3x–5.1x. The Amdahl ceiling from the 88%-in-IMDCT
   profile share predicted ~8x (217x → ~27x); the measured ~61x–99x
   speedup exceeds it substantially, because at these block sizes the
   `O(n²)` reference so dominates wall time that removing it collapses
   total runtime almost to the non-IMDCT remainder rather than to the
   fraction the sampling profiler attributed to it — see
   `planning/E2E-GAPS.md` §23 for the full measurement.
6. **Windowing + overlap-add** (§4.6.11.3.2): sine and — see below — KBD
   window halves, assembled per `WindowSequence`
   (`OnlyLong`/`LongStart`/`LongStop`/`EightShort`), then added to the
   previous frame's stored second half (`OverlapState`, one per output
   channel, reset by `flush`).
7. **Final PCM-scale normalisation**: `inverse_quantize_and_rescale`'s
   formula produces samples on the same scale as a 16-bit PCM decoder
   (matching FAAD2 and other reference implementations' convention), not
   the `[-1, 1]` range `SampleFmt::F32P` represents — `finalize_channel`
   multiplies by `1/32768` as the very last step. Found empirically. before
   this scale, correlation against `ffmpeg -bitexact` was already close
   (~0.95–0.996 per fixture) but every fixture's RMS ratio against the
   reference was a consistent ~32768 (2^15) regardless of content, sample
   rate or channel count — the signature of a missing fixed normalisation,
   not per-frame drift.

**A second finding, not anticipated going in: `vaco-codec-dsp-sinewin` was
sine-only, and that assumption was wrong.** The crate's own scope (D-06)
covered only the sine window on the working assumption that `ffmpeg`'s AAC
encoder — this workspace's only source of real AAC fixtures — never emits
`window_shape == 1` (KBD). Several real fixtures genuinely set it partway
through the stream (confirmed not a parsing artefact: re-running #444's own
677/677 bit-consumption check after the M/S-mask-storage refactor still
passed exactly). `kbd_window::<N>(alpha)` was added to that crate rather
than gated as unsupported — see
`docs/signal/vaco-codec-dsp-sinewin.md` for the construction, and its own
"square root" pitfall (dropping it still produces a symmetric, monotonic,
bounded window that *looks* plausible and fails Princen-Bradley by up to
2%, exactly the "smooth and plausible, not a desync" failure shape this
whole codec-decoding effort keeps finding).

**A third finding: `raw_data_block`'s syntactic element order is not
output channel order.** `channels_for_config(6)`'s bitstream order is
`[C, L, R, Ls, Rs, LFE]` (one `SCE`, one front `CPE`, one back `CPE`, one
`LFE`, per Table 42), but the conventional output order (and
`ffmpeg -bitexact`'s own) is `[FL, FR, FC, LFE, BL, BR]`. Before
`decoder.rs`'s `reorder_to_output_channel_order` permuted this, the 5.1
fixture's *per-channel* correlation was already solid (~0.98 for the one
channel carrying content) but the interleaved, whole-frame correlation was
effectively zero, because the content sat on a different channel index in
each stream. Only configurations 1, 2, 3, 4, 5 and 6 — the ones this crate resolves
without a program config element — are permuted; anything else is left in
parsed order rather than guessed at.

**Known, disclosed approximations in this step** (see "Known gaps" for the
full list): intensity stereo always assumes the in-phase codebook
(`IcsStream` does not retain which of the two intensity codebooks a band
used); `LongStart`/`LongStop`'s exact transition-boundary sample counts
follow the standard, widely-implemented convention rather than a clean
primary-text citation (the PDF extraction this crate's spec citations are
otherwise drawn from garbled that one boundary's fraction).

## SBR (#446) — what landed and what did not

T3-03d: Spectral Band Replication, the tool that turns AAC-LC into HE-AAC
by regenerating high-frequency content the encoder deliberately discarded.
The largest single piece left in epic #53, and — per the dispatch that
opened it — a genuinely different kind of work from #444/#445: a QMF
filterbank pair, a large bitstream syntax surface (its own ten Huffman
tables), and a frequency-domain reconstruction algorithm (patching,
envelope adjustment) with no equivalent in AAC-LC's own decode.

### What's implemented and verified

- **The QMF analysis and downsampled-synthesis filterbanks** (`qmf.rs`,
  §4.6.18.4.1/§4.6.18.4.3): `AnalysisBank` (32-band complex, 320-sample
  shift register) and `DownsampledSynthesisBank` (32-band, same-rate
  inverse, 640-sample shift register), transcribed directly from the
  flowcharts in Figures 4.42/4.44. The 640-tap prototype filter (Table
  4.A.89) is transcribed at half length (indices 0-320) and mirrored at
  runtime, since the table is exactly symmetric by construction — a
  decision that paid for itself immediately: cross-checking the *directly*
  extracted 640-entry text against that mirror rule found two indices
  (384, 512) whose printed sign disagreed with their mirror partners (256,
  128), the same "PDF extraction drops or adds a minus sign" failure shape
  as #445's `LongStart`/`LongStop` boundary fractions.
- **The rate-doubling `SynthesisBank`** (§4.6.18.4.2, 64-band, 1280-sample
  shift register) is also transcribed but not independently tested —
  `DownsampledSynthesisBank` was used for verification instead, since
  it's the variant the specification defines as `AnalysisBank`'s
  same-rate inverse; `SynthesisBank` shares the same modulation formula
  structure and is believed correct on that basis, but its actual
  consumer (HF generation) doesn't exist yet to test it against.
- **All ten SBR envelope/noise-floor Huffman tables** (`sbr_huffman_tables.rs`,
  Tables 4.A.79-4.A.88), transcribed the same way #444 transcribed AAC-LC's
  own spectral codebooks: every table independently verified prefix-free
  and Kraft-complete. All ten passed on the first transcription — unlike
  the QMF window table, which needed the mirror cross-check to catch its
  two flipped signs.

### The verification result: a false alarm, found and resolved

**Update, superseding an earlier finding in this same section:** a first
verification pass reported a "broadband phase-coherence defect" here —
tones round-tripping at correlation > 0.99 while white noise round-tripped
under 0.1 at every delay in the (arbitrarily chosen) 500-700 sample window
that first pass searched. That defect did not exist. Two more targeted
tests found the actual explanation:

- **A round-tripped impulse** (`AnalysisBank` through
  `DownsampledSynthesisBank` — the pair the specification defines as
  same-rate inverses) comes back as a single, clean, unity-gain delayed
  impulse at exactly **289 samples**, with no other energy anywhere else
  in the output. An impulse has no period, so unlike a sustained tone it
  cannot alias against a wrong delay — this is as unambiguous as a
  filterbank verification gets.
- **Two widely-separated tones summed together** (300 Hz and 9500 Hz — far
  enough apart to land in different subbands entirely) round-trip at
  correlation > 0.99 at that same 289-sample delay.

The original "593-sample" tone delay was real but misleading: a single
sustained tone's correlation against a lagged copy of itself is periodic
in the lag (a wrong delay off by a whole number of the tone's own periods
correlates just as well as the true one), so "593" was one alias among
many equally-plausible candidates within the window that first search
happened to cover — and the window did not include 289, the actual delay.
Once white noise is checked at the delay the impulse test names, it
reconstructs at correlation **> 0.99**, same as everything else.

**The lesson generalises past this one module.** Correlating a periodic
test signal against itself over an arbitrarily-chosen lag range can
manufacture a false negative indistinguishable from a real bug. An
impulse or a broadband signal doesn't have that failure mode, and should
be the first check reached for, not the last. `qmf.rs`'s own module doc
carries the full account; the test that once failed and was marked
`#[ignore]`
(`analysis_then_downsampled_synthesis_reconstructs_white_noise`) now
passes outright and the `#[ignore]` is gone.

### Where this leaves #446

The QMF foundation is now verified clean: impulse, single tones across
200 Hz-10 kHz, two widely-separated tones together, and white noise all
reconstruct at correlation > 0.99 at one consistent delay. **Nothing
downstream was built this pass regardless** — not because the foundation
is in doubt any longer, but because `sbr_data()`'s bitstream syntax
(`sbr_header`, the frequency band table algorithm §4.6.18.3.2 depends on,
`sbr_grid`'s four frame classes, the delta-coded envelope/noise values the
Huffman tables above decode), HF generation, and envelope adjustment are
each a substantial remaining piece in their own right — comparable in
scope to everything landed this pass, not a quick follow-on. Continuing
into them is real, valuable next work, not blocked on anything found this
pass.

### Fixture access for #446

Neither `ffmpeg`'s native `aac` encoder nor `libfdk_aac` (not compiled
into this environment's `ffmpeg` build) can produce HE-AAC. Earlier work
used macOS AudioToolbox output to confirm real implicit SBR signalling, but
the currently installed `afconvert` rejects its former `aach`/`aacp` encoder
format names and `ffmpeg`'s `aac_at` exposes AAC-LC only. This pass therefore
does not claim a newly generated HE-AAC oracle or any HE-AAC PCM result.

### Implicit and explicit SBR signalling — confirmed against real content

`vaco-parse-aac::asc::AudioSpecificConfig` already parses explicit SBR
signalling in full (`has_sbr()`, `output_sample_rate()`, the hierarchical
and backward-compatible sync-extension forms) — this predates #446 and
needed no changes. Decoded by hand from a real `afconvert`-produced HE-AAC
`.m4a`'s `esds` box: `audioObjectType=2` (AAC-LC core),
`samplingFrequencyIndex=7` (22050 Hz core), `syncExtensionType=0x2b7`,
`extensionAudioObjectType=5` (SBR), `sbrPresentFlag=1`,
`extensionSamplingFrequencyIndex=4` (44100 Hz — the doubled output rate).
`DecoderConfig::from_audio_specific_config` still rejects `cfg.has_sbr()`
with `Error::Unsupported` as of this pass — accepting it usefully needs
the QMF/HF pipeline this pass could not verify, so the gate stays in
place rather than accepting a configuration this crate cannot yet act on.
Replacing decoder extradata clears the prior extradata configuration, cached
in-band configuration, and overlap state before parsing the replacement. That
also applies when the replacement is rejected: a caller cannot submit a
headerless HE-AAC access unit after its explicit-SBR error and have it decoded
under the preceding AAC-LC configuration; it fails packet configuration and
queues no frame.

Explicit **Parametric Stereo** uses the second backward-compatible sync
extension after present SBR: `syncExtensionType=0x548`, followed by
`psPresentFlag`. The decoder checks that parsed `Signal::Present` before the
generic SBR check and returns a named HE-AACv2 refusal. This matters because PS
would otherwise make a mono core appear as stereo output; the refusal is made
before any packet is accepted, so no AAC-LC frame is reconstructed from that
configuration. The deterministic extension fixture verifies this parser route;
it does not claim a newly generated HE-AACv2 sample or PS PCM result.

A real `afconvert`-produced ADTS HE-AAC file confirmed the **implicit**
case directly: its raw ADTS header declares plain `profile=1` (AAC-LC),
`sampling_frequency_index=7` (22050 Hz) — nothing in the ADTS header
itself signals SBR at all, since ADTS carries no `AudioSpecificConfig`
extension fields. `ffmpeg`'s own decoder recognises this file as HE-AAC
and reports 44100 Hz output purely by finding an `EXT_SBR_DATA`/
`EXT_SBR_DATA_CRC` extension payload inside the frame — confirming
`raw_data_block`'s `FIL` element is exactly where implicit detection has to
happen, and that cannot be resolved at the configuration layer alone for a
raw-ADTS stream. It now reads the first `extension_type` nibble: Table 4.121's
`EXT_SBR_DATA`/`EXT_SBR_DATA_CRC` values 13/14 return a named
`Error::Unsupported` before any AAC-LC frame is reconstructed. Other fill
payloads retain their exact declared-length skip. The declared payload length
is checked before that nibble is classified, so a truncated `FIL` cannot be
misreported as implicit SBR: it returns `Error::UnexpectedEof` and queues no
frame. This is deliberately a refusal boundary, not SBR parsing or HE-AAC PCM
support.

## Decode accuracy — measured, not claimed

AAC, like every lossy codec this workspace has decoded, defines a
compliance tolerance rather than one correct output — reconstruction is
compared by correlation/max_abs/rms against `ffmpeg -bitexact`'s own
decode, not chased for bit-exactness. **The parse layer (#444) is still
held to the stricter, exact invariant** — bits consumed — since that one
*is* exact regardless of any lossy compliance tolerance.

### Parse: bit-consumption, exact

9 fixtures generated directly by `ffmpeg -c:a aac`, spanning every axis
that matters for this package: sample rate (16000/22050/32000/44100/48000
Hz), channel configuration (mono/stereo/5.1 — `channelConfiguration` 1/2/6),
bitrate (48k–320k), and window sequence (steady tones exercise
`ONLY_LONG`/`LONG_START`/`LONG_STOP`; white noise and a 440→6000 Hz jump
cut exercise `EIGHT_SHORT`):

| Fixture | Frames bit-exact |
|---|---|
| mono, 44100 Hz, 128 kbit/s | 88/88 |
| stereo, 44100 Hz, 128 kbit/s | 88/88 |
| stereo, 44100 Hz, 128 kbit/s, transient (440→6000 Hz cut) | 88/88 |
| stereo, 44100 Hz, 192 kbit/s, white noise (forces short blocks) | 88/88 |
| 5.1, 44100 Hz, 320 kbit/s (`channelConfiguration` 6) | 88/88 |
| stereo, 48000 Hz, 192 kbit/s | 95/95 |
| stereo, 32000 Hz, 96 kbit/s | 64/64 |
| mono, 22050 Hz, 64 kbit/s | 45/45 |
| mono, 16000 Hz, 48 kbit/s | 33/33 |
| **Total** | **677/677** |

Verification method: a throwaway test (not committed — it read fixture
files from this session's scratch directory, which is not part of the
repository) walked each file's real ADTS headers, decoded every frame's
`raw_data_block()`, and asserted the bit reader landed within 7 bits of the
frame's own declared length (the slack accounts for legitimate padding to
the next frame's byte boundary). 677 of 677 real frames matched exactly.
This invariant was re-checked (still 677/677) after #445's M/S-mask-storage
refactor changed how `raw_data_block.rs` represents `Element::Pair`,
confirming that change did not silently desync the parse.

### PCE persistence and layouts: exact output count against `ffmpeg`

`ffmpeg` 9.0.1 generated a 1.024-second, 48 kHz, three-channel AAC-LC ADTS
fixture from three distinct sine channels. Its ADTS header declares
`channelConfiguration == 0`; the first packet supplies the PCE and the other
47 packets rely on it. `ffprobe` reported 48 AAC packets and a `2.1` layout.

`ffmpeg -bitexact -i three.adts -f f32le -acodec pcm_f32le ref.f32` and this
crate's `decode_dump three.adts` each emitted **589,824 bytes**: 48 packets ×
1024 samples × 3 channels × 4 bytes/sample. Before retaining the PCE, the
decoder emitted only the first packet's 12,288 bytes and rejected all later
packets as layout-undetermined. This is an exact reachability/count check;
the general AAC correlation table above remains the reconstruction-quality
evidence.

For that PCE's front-`CPE` plus `LFE` shape, Vaco now emits a native `2.1`
frame layout (mask `0x0b`) as well as the exact three-plane/sample count. This
matches `ffprobe`'s layout label without pretending that every PCE can use the
same plane order.

After the leading-PCE handoff was made explicit, a fresh one-second,
three-non-silent-tone `2.1` fixture again reported 48 packets and produced
**589,824 bytes** in each decoder. Its three plane correlations against
`ffmpeg -bitexact` were −0.008787, −0.004685 and 0.000000 dB from unity.
The same code now rejects a `program_config_element()` after audio elements
with a named error rather than silently treating it as metadata.

A separate three-non-silent-tone `3.0` AAC-LC ADTS fixture was encoded with
`-aac_pce 1` to force this PCE route instead of the direct configuration. It
reported 48 packets; Vaco and `ffmpeg -bitexact` each emitted **589,824 bytes**
(48 × 1024 samples × 3 channels × 4-byte `f32` samples). Its raw
centre/front-pair order now maps to native `FL/FR/FC`, with per-plane
correlations from −0.056688 to −0.056060 dB from unity.

After the PCE profile expansion and profile/frequency cross-checks were added,
a fresh three-tone forced-PCE `3.0` AAC-LC ADTS fixture reported 25 packets.
Vaco and `ffmpeg -bitexact` each emitted **307,200 bytes** (25 × 1024 samples
× 3 channels × 4-byte `f32` samples). Its three diagonal plane correlations
were −0.109392 to −0.090468 dB from unity, while every nonmatching plane pair
was at or below −20.297475 dB. Synthetic profile and sampling-frequency
mismatches both reject with named errors before raw audio is decoded.

A six-non-silent-tone `5.1` AAC-LC ADTS fixture, also encoded with `-aac_pce
1`, reported 48 packets; Vaco and `ffmpeg -bitexact` each emitted
**1,179,648 bytes** (48 × 1024 samples × 6 channels × 4-byte `f32` samples).
Its raw centre/front-pair/back-pair/LFE planes now map to native
`FL/FR/FC/LFE/BL/BR`. Each native plane correlated from −0.061930 to
−0.000003 dB from unity; the full cross-plane matrix kept every nonmatching
pair at or below −20.107762 dB, so the distinct tones prove the permutation
rather than merely its channel count.

The same PCE route was regenerated with a distinct six-tone fixture after
channel-element binding was added. It again reported 48 packets and both
decoders emitted **1,179,648 bytes**. The binding accepted the encoder's
declared `SCE`/`CPE`/`CPE`/`LFE` tags and order; diagonal plane correlations
were −0.049902 to −0.000101 dB from unity, while every off-diagonal pair was
at or below −20.310847 dB. A synthetic tag mismatch is rejected before PCM is
emitted, so this acceptance evidence cannot be mistaken for a count-only
check.

An eight-non-silent-tone `7.1(wide)` AAC-LC ADTS fixture exercises a leading
PCE shape that ordinary ADTS `channelConfiguration` cannot label directly.
`ffprobe` reported 47 packets; both decoders emitted **1,540,096 bytes**
(47 × 1024 samples × 8 channels × 4-byte `f32` samples). The PCE's raw plane
order was centre, wide pair, front pair, back pair, LFE; Vaco maps it to
native `FL/FR/FC/LFE/BL/BR/FLC/FRC`. Its eight plane correlations against
`ffmpeg -bitexact` ranged from −0.000654 to 0.000000 dB from unity, proving
the map with independent signals rather than channel-count agreement alone.

An independently generated 1.024-second, 48 kHz `quad` ADTS fixture exercised
the front-`CPE` plus back-`CPE` PCE shape. `ffprobe` reported 48 packets and a
`quad` layout; both decoders emitted **786,432 bytes** (48 packets × 1024
samples × 4 channels × 4 bytes/sample). That shape now emits the native `quad`
frame layout (mask `0x33`), whose ascending plane order is already the PCE's
front-pair, back-pair order.

### Reconstruction: correlation/max_abs/rms against `ffmpeg -bitexact`

Same 9 fixtures. `decode_dump` (`examples/decode_dump.rs`) decodes each one
end to end and dumps interleaved `f32le` PCM; `ffmpeg -bitexact -i <fixture>
-f f32le -acodec pcm_f32le` produces the reference. Both are the same
length (no encoder-priming/decoder-latency trim needed for a raw-ADTS
stream — there is no edit-list metadata to apply one from). All 9 decode
past every frame with no `Unsupported` error, including the ones with
mid-stream KBD windows and the 5.1 configuration:

| Fixture | Correlation | max\_abs | RMS |
|---|---|---|---|
| mono, 16000 Hz, 48 kbit/s | 0.942 | 0.270 | 0.0293 |
| mono, 22050 Hz, 64 kbit/s | 0.949 | 0.243 | 0.0276 |
| mono, 44100 Hz, 128 kbit/s | 0.979 | 0.255 | 0.0178 |
| stereo, 32000 Hz, 96 kbit/s | 0.970 | 0.208 | 0.0150 |
| stereo, 44100 Hz, 128 kbit/s | 0.983 | 0.132 | 0.0113 |
| stereo, 44100 Hz, 128 kbit/s, transient (440→6000 Hz cut, exercises `LONG_START`/`LONG_STOP`) | 0.969 | 0.199 | 0.0154 |
| stereo, 44100 Hz, 192 kbit/s, white noise (`EIGHT_SHORT`) | 0.979 | 1.501 | 0.0768 |
| stereo, 48000 Hz, 192 kbit/s | 0.997 | 0.089 | 0.0049 |
| 5.1, 44100 Hz, 320 kbit/s (`channelConfiguration` 6, after channel reorder) | 0.981 | 0.223 | 0.0070 |

Read alongside each other: correlation clusters at 0.94–0.997 regardless of
channel count or window sequence, which is consistent with the disclosed
approximations above (intensity phase, PNS's own non-normative RNG,
`LongStart`/`LongStop`'s unverified boundary) being small, band-local
effects rather than a structural desync — a structural bug (wrong band
mapping, wrong window halves, a channel swap) reads as correlation near
zero, the shape the pre-fix 5.1 row actually had. The white-noise fixture's
larger `max_abs`/RMS is expected: PNS's own RNG is explicitly non-normative
per §4.6.13.3, so a noise-heavy fixture is exactly where this decoder and
`ffmpeg`'s are least likely to agree sample-for-sample, without either
being wrong.

**Not verified**: ISO/IEC 14496-26's conformance vector set — the
acceptance criterion #443's own issue names — was not accessible in this
session (as disclosed for #443/#444). This table is a real, reproducible
measurement against a real decoder's output; it is not a substitute for
that conformance suite, and this doc will keep saying so until that suite
is run.

## Bugs found and fixed (for the record, not just interest)

Both found by the corpus check above, not by code review — the same
pattern this workspace's other codec work has already established: a
plausible-looking implementation that a real bitstream falsifies.

- **ESC codebook sign-bit/escape-sequence ordering.** §4.6.3.3 states the
  data order as "Huffman codeword followed by 0 to 2 sign bits followed by
  0 to 2 escape sequences" — **both** sign bits before **either** escape
  sequence. The first implementation interleaved them per value (sign,
  then that value's own escape, then the next value's sign) rather than
  reading both signs, then both escapes. Every real fixture using codebook
  11 with two escape-triggering values in the same tuple desynced from
  that point forward — plausible-looking corruption, not a bitstream
  error, exactly the failure class this session keeps finding. Pinned as a
  unit test
  (`spectral::tests::sign_bits_are_grouped_before_both_escape_sequences_not_interleaved`)
  that only the wrong ordering can fail.
- **`raw_data_block()`'s element loop could allocate without bound.**
  `BitReader::get` pads exhausted input with zero bits rather than
  erroring; the loop's `id_syn_ele` read used `get`, so a stream truncated
  (or simply malformed) before it ever presented `ID_END` read `id == 0`
  (`ID_SCE`) forever, decoding and pushing one all-zero `Element` onto an
  unbounded `Vec` every iteration. Found by fuzzing (`aac_config`, a real
  `libFuzzer` out-of-memory artifact — 11 adversarial bytes, not a
  contrived case), not by inspection. Fixed by reading with `try_get`,
  which errors once real data is exhausted rather than padding; regression
  test:
  `raw_data_block::tests::a_stream_that_never_presents_id_end_errors_instead_of_looping_forever`.

## Known gaps

- **HE-AAC/SBR (#446) is not decoded — `Error::Unsupported` for any
  configuration signalling it.** The QMF filterbanks are transcribed and
  fully verified (correlation > 0.99 for tones, two widely-separated tones
  together, white noise, and a round-tripped impulse — see "SBR (#446)"
  above) and the ten envelope/noise Huffman tables are transcribed and
  verified prefix-free/Kraft-complete, but `sbr_data()`'s own bitstream
  syntax (which needs the frequency band table algorithm, §4.6.18.3.2, to
  parse correctly), HF generation and envelope adjustment were not
  attempted — each is a substantial remaining piece, not blocked on
  anything found this pass. `DecoderConfig::from_audio_specific_config`
  still rejects `cfg.has_sbr()`. Implicit signalling (SBR data inside a
  `FIL` element with no `AudioSpecificConfig` hint) is also rejected by name
  before frame output; decoding it still requires the same downstream
  pipeline.
- **Intensity stereo always assumes the in-phase codebook
  (`INTENSITY_HCB`).** Both intensity codebooks (in-phase `INTENSITY_HCB`
  and out-of-phase `INTENSITY_HCB2`) decode to the same
  `BandValue::IntensityPosition` shape in #444's `scalefactor.rs`;
  `IcsStream` does not retain which one a given band actually used, only
  the consequence (`band_values`). `reconstruct::apply_intensity_stereo`
  always takes the `+1` (in-phase) sign as a result. Fixing this means
  threading `sfb_cb` (or a per-band phase bit derived from it) through
  `IcsStream` from `section_data()`'s already-decoded codebook assignment
  — the information exists earlier in the pipeline, it is just not carried
  this far yet.
- **`LongStart`/`LongStop`'s window-transition boundary arithmetic is the
  standard, widely-implemented convention, not a clean primary-text
  citation.** The 1024/448/128/448-sample split
  (`reconstruct::build_window`) could not be reliably extracted from this
  crate's own (partially garbled by PDF extraction) copy of §4.6.11.3.2
  part b — one fraction in particular did not survive extraction cleanly.
  The measured correlation table's transient fixture (440→6000 Hz cut,
  which forces exactly this transition) is the empirical check in place of
  that citation; it reads no worse than the other fixtures, but "no worse"
  is not the same confidence as a verified formula.

  **That check turned out to be blind to the failure it was standing in
  for.** `overlap_add_eight_short` placed short window `j` at `768 + 128j`
  where the same file's `LongStop` (`w[448..576]`) and `LongStart`
  (`w[1472..1600]`) arms require `448 + 128j` — every `EIGHT_SHORT` block
  reconstructed 320 samples late, and the transition's time-domain alias
  never cancelled. A 440→6000 Hz cut cannot see that: correlation of a
  lagged sinusoid against itself is periodic in the lag, the same
  false-negative shape `AGENT-CONSTRAINTS.md` already records for this
  crate's QMF work. Measured against `ffmpeg -bitexact` on a fixture of
  20 ms noise bursts at exact 0.5 s boundaries, every burst onset was +320
  before the fix and exactly on ffmpeg's after it. The three boundaries now
  come from one `SHORT_START` constant with `const` assertions pinning the
  two literals to it, and
  `eight_short_fills_exactly_the_span_the_transition_windows_leave` derives
  the expected span from `build_window`'s own output.

  The test that pinned that span then failed on a *second* defect in the
  same function: `LongStart` built its window with
  `copy_range(&mut w, &long_left_full, 0)`, which writes all 2048 samples,
  so the long window's descending tail stayed in `w[1600..]` where the
  sequence is defined to be zero and nothing later overwrote it. Only the
  left half is copied now.

  Both fixed together, measured against `ffmpeg -bitexact` 9.0.1 on raw
  ADTS (no container trim on either side), `examples/decode_dump` vs
  `-f f32le`, correlation at lag 0 and RMS of the difference:

  | fixture (5 s) | before | + overlap-add | + `LongStart` tail |
  |---|---|---|---|
  | 20 ms noise bursts, 48 kHz mono | 0.037 (best lag −320) | 0.947 | **0.949** |
  | white noise, 48 kHz mono | 0.991 | 0.9989 | **0.9990** |
  | 20 Hz→20 kHz chirp, 44.1 kHz mono | 0.977 | 0.9856 | **0.9859** |
  | RMS, bursts | 0.143 | 0.0335 | **0.0327** |
  | RMS, white noise | 0.0376 | 0.0131 | **0.0123** |
  | RMS, chirp | 0.107 | 0.0846 | **0.0837** |

  Both long-block-only fixtures improve too, because `ffmpeg`'s encoder
  still emits occasional short blocks in noise and chirp content.

  **Any fixture in the correlation table below is still blind to a constant
  time shift.** Use aperiodic content with sharp attacks when checking the
  filterbank.
- **`coupling_channel_element()` (`CCE`) is not implemented** —
  `Error::Unsupported`. It carries its own `individual_channel_stream()`
  plus a per-coupled-element gain list this crate has not transcribed.
  Rare in real 1/2/3/4/5/6-channel content (this crate's resolved
  configurations); gated rather than guessed at.
- **`channelConfiguration` 7, 11, 12, 14** are gated (see "Channel-
  configuration coverage" above), pending ISO/IEC 14496-3 Table 42's exact
  element ordering being checked against a primary copy rather than
  recalled. `reorder_to_output_channel_order` (`decoder.rs`) only knows the
  output permutation for 1, 2, 3, 4, 5 and 6 for the same reason.
- **HE-AAC/HE-AACv2 (SBR, Parametric Stereo)** are explicitly rejected at
  the configuration layer — #446/#447, a different (and each individually
  substantial) package, per this issue's own dispatch.
- **A mid-stream `PCE`** (an in-band `program_config_element()` found by
  `raw_data_block`'s own `ID_PCE` case, as opposed to the leading one
  `DecoderConfig::try_resolve_pending` looks for) is not a live configuration
  update yet. It is explicitly refused, so the decoder cannot silently apply
  a stale layout/order/overlap state. A leading PCE is retained for later ADTS
  packets; this limitation concerns only PCEs that follow audio elements in a
  raw data block.
- **PCE layouts whose element order differs from native output order** retain
  their channel count but are layout-unspecified unless they match the one
  verified `7.1(wide)` structure above. In particular, a centre `SCE` before
  a front `CPE` needs an explicit plane permutation before it can safely be
  named; assigning a native layout first would label the decoder's planes
  incorrectly.
- **ISO/IEC 14496-26's conformance vector set was not accessible in this
  session** (as already disclosed for #443/#444) — the "Decode accuracy"
  table above is a real measurement against a real decoder's output, not a
  substitute for that suite.

## How to change it

- **Extending AAC-LC encode beyond silence:** preserve
  `AacLcSilenceAccessUnit` as the raw-payload/`AudioSpecificConfig` source of
  truth, then add a real transform/quantisation path before accepting nonzero
  PCM. Keep the ffmpeg elementary-stream oracle and add an independently
  generated, non-silent reference for every new channel/rate/frame shape. Do
  not register either direct API until a generic encoder and concrete muxer
  hand-off own timestamps, stream parameters, and framing together.
- **Fixing intensity stereo's phase:** thread `sfb_cb`'s actual value (14
  vs 15) from `section.rs`/`scalefactor.rs` through to `IcsStream`, then
  have `reconstruct::apply_intensity_stereo` branch on it instead of always
  taking `+1`. Add a fixture-independent unit test with a hand-built
  out-of-phase band before trusting it against real content — none of this
  pass's 9 fixtures were confirmed to exercise `INTENSITY_HCB2` specifically.
- **Verifying `LongStart`/`LongStop`'s exact boundary arithmetic:** get a
  clean (non-OCR-garbled) copy of §4.6.11.3.2 part b, or find an existing
  from-primary-text transcription (e.g. a from-spec reference decoder's own
  source, read for its citation rather than copied) to check the
  1024/448/128/448 split in `reconstruct::build_window` against.
- **Adding a `channelConfiguration` value (7/11/12/14):** get ISO/IEC
  14496-3 Table 42's element ordering for that value from a primary copy,
  add it to `config.rs`'s `known_channel_count` (renaming/restructuring it
  to carry an element order, not just a count, since that is what a real
  decoder needs), a unit test alongside the existing 1/2/3/4/5/6 cases, and a
  matching entry in `decoder.rs`'s `reorder_to_output_channel_order`. Do not
  extrapolate an ordering from the ones already here — 1/2/3/4/5/6 were chosen
  specifically because they were confident, not because they generalise.
- **Adding `coupling_channel_element()`:** transcribe Table 4.8's syntax
  (`ind_sw_cce_flag`, `num_coupled_elements`, the per-coupled-element gain
  list) alongside `pce.rs`'s pattern; it embeds one
  `individual_channel_stream()` per Table 4.8, reusable as-is.
- **Extending the reconstruction pipeline generally:** `reconstruct.rs`
  exposes three composable functions rather than one monolithic
  per-channel routine — `deinterleave_channel`, `apply_joint_stereo`,
  `finalize_channel` — specifically so a future change (CCE's own gain
  list, an SBR hook ahead of the filterbank) can insert itself at the right
  point in the pipeline without restructuring the whole thing. Keep that
  shape rather than collapsing it back down.
- **Continuing SBR now that the QMF banks are verified:** the frequency
  band table algorithm (§4.6.18.3.2.1's `fMaster` flowcharts, Figures
  4.39/4.40 — logarithmic band spacing with a two-region split and an
  `NINT`-rounding sort step, genuinely one of the more intricate pieces of
  the whole tool) is the next dependency: `sbr_grid`/`sbr_envelope`/
  `sbr_noise`'s own bit-level structure (how many bands each envelope/noise
  loop reads) cannot be parsed correctly without it. `sbr_huffman_tables.rs`
  is ready to consume once that's in place. `sbr_data()`'s syntax
  (`sbr_header`, `sbr_grid`'s four frame classes, `sbr_dtdf`,
  `sbr_envelope`/`sbr_noise`'s delta-coded Huffman decode) was read from
  the primary text during this pass but not transcribed into code, so
  that reading is not yet captured anywhere except this doc and should be
  redone against the primary text directly rather than trusted from
  memory. `raw_data_block.rs` now inspects `extension_payload()`'s
  `extension_type` and refuses `EXT_SBR_DATA`/`EXT_SBR_DATA_CRC` (values
  `0b1101`/`0b1110`, Table 4.121). Full parsing still needs the sibling
  `SCE`/`CPE` a `FIL`'s SBR data belongs to tracked across the element loop,
  since `sbr_extension_data(id_aac, crc_flag)` selects
  `sbr_single_channel_element()` versus `sbr_channel_pair_element()`.
- **If a future QMF change ever needs re-verifying:** round-trip an
  impulse first, not a tone. `qmf.rs`'s own module doc explains why a
  sustained tone's correlation against a lagged copy of itself is
  ambiguous (periodic in the lag) in a way an impulse's is not — this
  pass lost real time to exactly that trap before finding it.

## Configuration

`AacLcSilenceEncoder` has no runtime options: its fixed input contract is
packed `S16`, mono or stereo, 22.05, 24, 32, 44.1, or 48 kHz, 1024 all-zero samples per
channel. The gating feature,
`patent-encumbered-aac-decode`, is set in `vaco-component.toml` and consumed entirely by
`vaco-registry`/`cargo xtask gen-registry`/`cargo xtask patent-gate` — see
"Why this is gated" above.

`AacLcSilenceAccessUnit` has the same input contract. Its `payload()` and
`audio_specific_config()` are paired: use both or neither. It does not create
stream parameters, timestamps, packet buffers, container framing, or a generic
encoder descriptor.

## Dependencies

`vaco-core`, `vaco-bitstream`, `vaco-limits`, `vaco-codec-core` (the
`Decoder`/`SendReceive` traits, `DecoderDesc`, `Caps`), `vaco-codec-vlc` (`VlcTable`,
`VlcEntry` — every Huffman table in `spectral_tables.rs`), `vaco-frame`,
`vaco-packet`, `vaco-parse-aac` (`AdtsHeader`, `AudioSpecificConfig`,
`AudioObjectType`, `tables::is_reserved_config`, `tables::layout_for_config`
— reconstruction's channel-layout tagging), `vaco-chlayout`, `vaco-sampfmt`.
Added by #445: `vaco-codec-dsp-sinewin` (`sine_window`, `kbd_window` — see
that crate's own doc for the KBD addition this pass made to it) and
`vaco-tx` (`Plan`, `Tx`, `TxKind::Mdct`, `TxFlags::FULL_IMDCT` — since C1,
2026-09-01; `reference::imdct` is no longer on the production path, kept
only as `vaco-tx`'s own oracle).

## Specification

ISO/IEC 14496-3 (`iso-14496-3`, `provenance/sources.toml`): subpart 1
§1.6.2.1 (`AudioSpecificConfig`, in `vaco-parse-aac`) and §1.A.6.2
(`program_config_element()`, `pce.rs`); subpart 4 Table 4.3
(`raw_data_block()`), Table 4.6 (`ics_info()`), Table 4.7 (`pulse_data()`),
Table 4.50 (`individual_channel_stream()`), Table 4.52 (`section_data()`),
Table 4.53 (`scale_factor_data()`), Table 4.54 (`tns_data()`), Table 4.56
(`spectral_data()`), Tables 4.68/4.72/4.150/4.151/4.155/4.156 (element ids,
window sequences, codebook parameters, TNS field widths), Tables
4.73-4.85/4.129-4.141 (`swb_offset`, both the 2001 and 2009 numbering — see
`swb_tables.rs`'s own doc), Tables 4.A.1-4.A.12 (Huffman codebooks — see
`spectral_tables.rs`'s own doc). ISO/IEC 13818-7 (`iso-13818-7`) backs the
ADTS transport syntax, also in `vaco-parse-aac`.

Added by #449 (narrow ADTS silence encode): ISO/IEC 14496-3 subpart 1 Table
1.A.5 (ADTS fixed/variable header fields) and subpart 4 Table 4.3
(`raw_data_block()` element sequence) and Table 4.50
(`individual_channel_stream()`) — `encoder.rs` writes only the mono-SCE or
stereo-CPE `ZERO_HCB` forms documented above. The raw API also writes the
matching `AudioSpecificConfig` from subpart 1 §1.6.2.1/Table 1.15.

Added by #445 (reconstruction): §4.6.1.3 (inverse quantisation),
§4.6.2.3.3 (scalefactor gain), §4.5.2.3.5 (`quant_to_spec()`), §4.6.8.1.3
(M/S stereo), §4.6.8.2.3 (intensity stereo), §4.6.9.3
(`tns_decode_coef`/`tns_ar_filter`/`tns_decode_frame` pseudocode), §4.6.13.3
(perceptual noise substitution), §4.6.11.1 and §4.6.11.3.1-2 (the IMDCT
formula and windowing/block-switching, including the KBD construction now
in `vaco-codec-dsp-sinewin`), Table 4.156/4.157 (`TNS_MAX_ORDER`/
`TNS_MAX_BANDS`), and Table 42 (channel-configuration element ordering,
`13818-7` — backing `decoder.rs`'s `reorder_to_output_channel_order` for
configurations 1, 2, 3, 4, 5 and 6).

Added by #446 (SBR, in progress — see "SBR (#446)" above): §4.6.18.4.1-3
(QMF analysis/synthesis/downsampled-synthesis filterbanks, `qmf.rs`),
Table 4.A.89 (QMF window coefficients), Annex §4.A.6.1 and Tables
4.A.78-4.A.88 (SBR Huffman tables and their `(df_env_flag, df_noise_flag,
amp_res, LAV)` parameters, `sbr_huffman_tables.rs`), §1.6.5.2 (implicit
and explicit SBR signalling, confirmed against real content — see "SBR
(#446)" above), Table 4.121 (`extension_type` values used by the
`FIL`-element implicit-SBR refusal), and Tables 4.57/4.62-4.74
(the `sbr_data()` syntax read during this pass but not yet transcribed
into code).
