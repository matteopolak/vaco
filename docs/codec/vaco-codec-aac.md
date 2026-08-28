# `vaco-codec-aac`

Layer 4. AAC-LC decode (epic #53, T3-03), patent-encumbered per D4. **Never
shipped**: gated behind the `patent-encumbered-aac-decode` feature, off by
default, and this doc will keep saying so until D4's legal posture changes.

## What it is

Configuration resolution only, today: `AudioSpecificConfig` (reusing
`vaco-parse-aac`'s existing parser, which already covers this fully for
container-reporting purposes), a new full `program_config_element()` parser
(`pce.rs`), a unifying `DecoderConfig` (`config.rs`), and a registered but
functionally stubbed [`AacDecoder`] (`decoder.rs`) that resolves a packet's
configuration completely and then honestly reports that spectral decode is
not implemented.

**This is #443 (T3-03a) only.** #444 (T3-03b, core syntax: window sequences,
scalefactor bands, section data, spectral Huffman decode) and #445 (T3-03c,
reconstruction: inverse quantisation, TNS, joint stereo, IMDCT/overlap-add)
are unstarted — see "Known gaps" below for exactly what that means for a real
packet handed to this decoder today.

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

## Known gaps

- **No spectral decode of any kind.** `AacDecoder::send_packet` always
  returns `Error::Unsupported` once configuration is resolved — #444/#445.
  This is a decoder that correctly declines to produce audio it does not yet
  know how to produce, not a partially-working one; see
  `vaco-codec-mpegaudio`'s MPEG-2.5 gate for the identical shape and
  reasoning.
- **`channelConfiguration` 3, 4, 5, 7, 11, 12, 14** are gated (see above),
  pending ISO/IEC 14496-3 Table 42's exact element ordering being checked
  against a primary copy rather than recalled.
- **HE-AAC/HE-AACv2 (SBR, Parametric Stereo)** are explicitly rejected at
  the configuration layer — #446/#447, a different (and each individually
  substantial) package, per this issue's own dispatch.
- **No fuzz target yet for this crate specifically** — `set_extradata` and
  `AacDecoder::send_packet` both parse attacker-controlled bytes
  (`AudioSpecificConfig::parse`, `AdtsHeader::parse`,
  `ProgramConfigElement::read`) and should get one; not yet wired at the
  time this doc was written. `vaco-parse-aac`'s own ADTS/LATM/ASC parsing —
  the code this crate's config layer sits directly on top of — already has
  fuzz coverage.

## How to change it

- **Adding a `channelConfiguration` value (3/4/5/7/11/12/14):** get ISO/IEC
  14496-3 Table 42's element ordering for that value from a primary copy,
  add it to `config.rs`'s `known_channel_count` (renaming/restructuring it
  to carry an element order, not just a count, since that is what a real
  decoder needs), and add a unit test alongside the existing 1/2/6 cases.
  Do not extrapolate an ordering from the ones already here — 1/2/6 were
  chosen specifically because they were confident, not because they
  generalise.
- **Landing #444:** the natural next module is `raw_data_block.rs` —
  `id_syn_ele` dispatch over `SCE`/`CPE`/`CCE`/`LFE`/`DSE`/`PCE`/`FIL`/`END`,
  which `pce::find_leading_program_config_element` already has half of (the
  `id_syn_ele` read and the `PCE` case); the other seven cases are #444's.
  `vaco-codec-vlc` (#152) and `vaco-codec-dsp-sinewin` (#256), both landed
  alongside this crate as its prerequisites, are what #444/#445's Huffman
  decode and window generation should build on rather than re-deriving.
- **Landing #445:** `vaco-tx::reference::imdct` (SP-C6, already built and
  already used by `vaco-codec-mpegaudio`'s Layer III) is the transform to
  reuse; do not write a second one.

## Configuration

None inside this crate. The gating feature, `patent-encumbered-aac-decode`,
is set in `vaco-component.toml` and consumed entirely by
`vaco-registry`/`cargo xtask gen-registry`/`cargo xtask patent-gate` — see
"Why this is gated" above.

## Dependencies

`vaco-core`, `vaco-bitstream`, `vaco-limits`, `vaco-codec-core` (the
`Decoder` trait, `DecoderDesc`, `Caps`), `vaco-frame`, `vaco-packet`,
`vaco-parse-aac` (`AdtsHeader`, `AudioSpecificConfig`, `AudioObjectType`,
`tables::is_reserved_config`). Not yet depended on, pending #444/#445:
`vaco-codec-vlc`, `vaco-codec-dsp-sinewin`, `vaco-tx` — see `Cargo.toml`'s
own comment for why they are not added before the code that needs them
exists.

## Specification

ISO/IEC 14496-3 (`iso-14496-3`, `provenance/sources.toml`): subpart 1
§1.6.2.1 (`AudioSpecificConfig`, in `vaco-parse-aac`) and §1.A.6.2
(`program_config_element()`, `pce.rs`, this crate). ISO/IEC 13818-7
(`iso-13818-7`) backs the ADTS transport syntax, also in `vaco-parse-aac`.
