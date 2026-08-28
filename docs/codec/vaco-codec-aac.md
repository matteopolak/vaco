# `vaco-codec-aac`

Layer 4. AAC-LC decode (epic #53, T3-03), patent-encumbered per D4. **Never
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
"Decode accuracy" below — and only then returns `Error::Unsupported`, honestly,
because turning that parsed syntax into PCM (inverse quantisation, TNS
application, joint stereo, IMDCT/overlap-add) is **#445, not yet started**.
See "Known gaps" for the precise boundary.

## Why this is gated (D4)

AAC is legally **RED**, not merely off-by-default for a portability reason:
the Via LA AAC pool is active and licenses per **decoder or encoder unit**,
not per bitstream (`planning/00-decisions.md` D4,
`planning/research/07-legal-patents-licensing.md` §5.2/§6). AAC *remuxing* —
reading a container's AAC track without ever instantiating a decoder — stays
in the default build and is what `vaco-parse-aac` already delivers; only
decode (this crate) and encode (unstarted) are gated.

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

### Channel-configuration coverage: 1, 2, 6 direct; 0 via PCE; the rest gated

[`DecoderConfig::from_adts_header`] and
[`DecoderConfig::from_audio_specific_config`] both resolve
`channelConfiguration` 1 (mono), 2 (stereo) and 6 (5.1) directly — the
overwhelming majority of real AAC-LC content, and the three configurations
this crate could state the exact `SCE`/`CPE`/`LFE` element ordering for with
confidence. `channelConfiguration == 0` resolves exactly, from a real PCE,
via [`DecoderConfig::try_resolve_pending`].

**Configurations 3, 4, 5, 7, 11, 12 and 14 are rejected with
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

## Decode accuracy — measured, not claimed

**No PCM is produced yet (#445), so there is nothing to compare against a
reference decoder's samples.** What *is* verifiable now, and was verified:
that the parse consumes exactly the bits a real frame declares and leaves
the reader positioned where the next frame begins — a strong invariant that
fails loudly the moment a codebook or band mapping is wrong, exactly as it
did twice during this pass (see "Bugs found and fixed").

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

- **No PCM synthesis of any kind (#445, unstarted).**
  `AacDecoder::send_packet` fully parses `raw_data_block()` and then always
  returns `Error::Unsupported`. This is a decoder that correctly declines
  to produce audio it does not yet know how to produce, not a
  partially-working one; see `vaco-codec-mpegaudio`'s MPEG-2.5 gate for the
  identical shape and reasoning. #445 needs: inverse quantisation (the
  `x_quant`→coefficient power-law, per-band scalefactor applied), TNS
  application (`tns_decode_coef`/`tns_ar_filter`, §4.6.9.3 — the syntax is
  already parsed and kept in `IcsStream::tns`), joint stereo (M/S — already
  read as `ms_used` but not applied — and intensity stereo, whose position
  values are already decoded in `IcsStream::band_values`), and the
  IMDCT/windowing/overlap-add (`vaco-tx::reference::imdct` already exists
  for this, from SP-C6; `vaco-codec-dsp-sinewin` for the window).
- **`coupling_channel_element()` (`CCE`) is not implemented** —
  `Error::Unsupported`. It carries its own `individual_channel_stream()`
  plus a per-coupled-element gain list this crate has not transcribed.
  Rare in real 1/2/6-channel content (this crate's own resolved
  configurations); gated rather than guessed at.
- **`channelConfiguration` 3, 4, 5, 7, 11, 12, 14** are gated (see "Channel-
  configuration coverage" above), pending ISO/IEC 14496-3 Table 42's exact
  element ordering being checked against a primary copy rather than
  recalled.
- **HE-AAC/HE-AACv2 (SBR, Parametric Stereo)** are explicitly rejected at
  the configuration layer — #446/#447, a different (and each individually
  substantial) package, per this issue's own dispatch.
- **A mid-stream `PCE`** (an in-band `program_config_element()` found by
  `raw_data_block`'s own `ID_PCE` case, as opposed to the leading one
  `DecoderConfig::try_resolve_pending` looks for) is parsed but not yet
  threaded back into an in-flight `DecoderConfig` to update the channel
  layout. Rare — a real stream's PCE almost always leads its very first
  frame — but a real gap, not yet exercised by any fixture in this pass's
  corpus.

## How to change it

- **Adding a `channelConfiguration` value (3/4/5/7/11/12/14):** get ISO/IEC
  14496-3 Table 42's element ordering for that value from a primary copy,
  add it to `config.rs`'s `known_channel_count` (renaming/restructuring it
  to carry an element order, not just a count, since that is what a real
  decoder needs), and add a unit test alongside the existing 1/2/6 cases.
  Do not extrapolate an ordering from the ones already here — 1/2/6 were
  chosen specifically because they were confident, not because they
  generalise.
- **Landing #445:** every syntax element reconstruction needs is already
  parsed and kept — `IcsStream::band_values` (scalefactors/intensity
  positions/noise energies, still in log domain), `IcsStream::x_quant` (raw
  quantized coefficients, pulse-adjusted), `IcsStream::tns` (filter
  coefficients, not yet inverse-quantised into LPC form). The natural
  starting point is the power-law scalefactor application
  (`2^(0.25*scalefactor)` per §4.6.2), since M/S, intensity and TNS
  application all consume its output. `vaco-tx::reference::imdct` (SP-C6)
  is the transform to reuse; do not write a second one.
- **Adding `coupling_channel_element()`:** transcribe Table 4.8's syntax
  (`ind_sw_cce_flag`, `num_coupled_elements`, the per-coupled-element gain
  list) alongside `pce.rs`'s pattern; it embeds one
  `individual_channel_stream()` per Table 4.8, reusable as-is.

## Configuration

None inside this crate. The gating feature, `patent-encumbered-aac-decode`,
is set in `vaco-component.toml` and consumed entirely by
`vaco-registry`/`cargo xtask gen-registry`/`cargo xtask patent-gate` — see
"Why this is gated" above.

## Dependencies

`vaco-core`, `vaco-bitstream`, `vaco-limits`, `vaco-codec-core` (the
`Decoder` trait, `DecoderDesc`, `Caps`), `vaco-codec-vlc` (`VlcTable`,
`VlcEntry` — every Huffman table in `spectral_tables.rs`), `vaco-frame`,
`vaco-packet`, `vaco-parse-aac` (`AdtsHeader`, `AudioSpecificConfig`,
`AudioObjectType`, `tables::is_reserved_config`). Not yet depended on,
pending #445: `vaco-codec-dsp-sinewin`, `vaco-tx` — see `Cargo.toml`'s own
comment for why they are not added before the code that needs them exists.

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
