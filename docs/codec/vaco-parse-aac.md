# `vaco-parse-aac`

## What it is

Header parsing for AAC: the ADTS transport, the LOAS/LATM transport, and the
`AudioSpecificConfig` that MP4 carries in `esds`. It splits a byte stream into
access units and works out the `CodecParameters` a container reports — profile,
sample rate, channel count and layout. **It does not decode**: no spectral
reconstruction, no PCM, no output samples.

That boundary is legal as well as architectural. D9 classifies AAC as **RED**,
but the Via LA pool charges per encoder and per decoder *unit*, not per
bitstream — so AAC remuxing stays in the default build while encode and decode
are gated behind non-default features. Reading a header implements no decoder,
so this crate ships by default. Do not grow it past that line.

## How it works

| Module | Syntax | Specification |
|---|---|---|
| `asc` | `AudioSpecificConfig`, `GetAudioObjectType` | ISO/IEC 14496-3 subpart 1 §1.6.2.1, Table 1.15 |
| `adts` | `adts_frame`, `adts_fixed_header`, `adts_variable_header` | ISO/IEC 14496-3 subpart 4 §4.4.1.1 |
| `latm` | `AudioSyncStream`, `AudioMuxElement`, `StreamMuxConfig`, `LatmGetValue` | ISO/IEC 14496-3 subpart 4 §1.7, Tables 1.28, 1.41, 1.42, 1.44 |
| `tables` | the sampling-frequency index and channel-configuration tables | as above |

MP4 carriage is ISO/IEC 14496-14 §5.6: the demuxer walks `esds` down to the
`DecoderSpecificInfo` and hands its payload to `AudioSpecificConfig::parse`.

### Where the reported sample rate actually comes from

This is the part that is easy to get subtly wrong, and it is what `ffprobe`
prints, so it is the user-visible contract.

**With an `AudioSpecificConfig`** — MP4 or LATM — the answer is entirely in the
header:

* `output_sample_rate()` is the **extension** sampling frequency whenever
  `sbrPresentFlag` is set, and the core frequency otherwise. It is *not* "double
  the core rate": a configuration with `samplingFrequencyIndex = 4` (44100) and
  `extensionSamplingFrequencyIndex = 3` reports **48000**, not 88200. Measured.
* An extension frequency of zero falls back to the core frequency.
* `output_channels()` doubles a one-channel core to two when SBR is present
  **and PS is not explicitly denied**. The `Unknown` case is the one that
  matters, and it is why `Signal` is three-valued rather than a `bool`:

  | `channelConfiguration` | SBR | PS | reported |
  |---|---|---|---|
  | 1 | unknown | unknown | 1 |
  | 1 | present | unknown | **2** |
  | 1 | present | absent | 1 |
  | 1 | present | present | 2 |
  | 2 | present | present | 2 |

* `profile()` comes from the **core** object type after any hierarchical SBR/PS
  wrapper (`audioObjectType` 5 or 29) has been unwrapped, and the value is
  `audioObjectType - 1`. So an HE-AAC stream described only by its configuration
  reports `LC`.

**With raw ADTS** there is no configuration at all, and this is where a
parse-only build cannot match the reference — see *Known divergences* below.

### Framing and resynchronisation

`AdtsParser` and `LoasParser` implement `vaco_codec_core::Parser`. Each emits
**whole frames, header included**, which is the packetisation the reference
produces (confirmed by comparing `ffprobe -show_packets` positions and sizes
against the frame-length chains in the headers).

A twelve-bit ADTS sync word occurs by chance about once every 4 KiB of random
data, so accepting the first `0xFFF` is how a parser gets a false-positive
storm. The rule both parsers use:

1. Validate the entire candidate header — sync word, `layer == 0`, a
   sampling-frequency index in 0..=12, and a declared length at least as long as
   its own header.
2. **While out of sync**, additionally require a sync word exactly
   `aac_frame_length` (or `audioMuxLengthBytes + 3`) bytes later. If those bytes
   have not arrived, wait rather than guess.
3. Once in sync, take frames as they come, so the last frame of a stream is
   still emitted.

The condition in step 2 is `!synced` and nothing else — deliberately. Every path
that advances the scan cursor clears `synced` first, so the decision depends on
the position in the stream and not on how the bytes were chunked. That is what
makes the `parse_aac_adts` fuzz target's chunk-invariance property hold.

The last frame of a stream has nothing after it to confirm it with. Rather than
lose it, the parser **defers** it: the candidate is copied into a fixed-size
buffer (bounded by the 13-bit length field, so at most 8191 bytes) and emitted
on the end-of-stream call, `parse(&[])`. The reference does accept a file
containing exactly one ADTS frame — probed with `ffprobe -f aac` — so dropping
it would have been a divergence.

## Registration: how a demuxer reaches this crate

This crate ships a `vaco-component.toml` naming `vaco_parse_aac::PARSER`, a
`vaco_codec_core::ParserDesc`. `cargo xtask gen-registry` collects it into
`vaco_registry::PARSERS` — two rows, because the reference treats LATM/LOAS
as a separate codec (`codec_name=aac_latm`) and `CodecId` mirrors that, so
`PARSER_LATM` registers `LoasParser` beside it. `vaco_registry::Parsers` — the one
`ParserProvider` in the build — answers `parser_for(CodecId::Aac)` with a
`Box<dyn Parser>` built from it.

**No demuxer names this crate.** D14.1 and `cargo xtask layer-check` forbid a
`crates/format/` crate from depending on a `crates/codec/` one; the indirection
is what makes `-show_streams` able to report bitstream fields without that edge.

Two consequences worth knowing when changing anything here:

* **Everything a demuxer can see goes through `dyn Parser`.** `parse`,
  `parameters` and `set_extradata` are the whole surface. An inherent method,
  however useful, is invisible from a container. `tests/provider.rs` is written
  entirely against `Box<dyn Parser>` for that reason — a version written against
  the concrete type would pass while the seam stayed broken.
* **`ParserDesc::make` takes `Limits`.** A parser on the probe path is handed
  attacker-controlled bytes before anything has validated them, so there is no
  no-argument constructor to reach for.

### AAC is where the two paths genuinely differ

In MPEG-TS every frame carries an ADTS header and `Parser::parse` finds
everything. In MP4 and Matroska the samples are **raw AAC** with no ADTS header
at all, and the whole description — object type, sampling frequency, channel
configuration — is in the `esds` `DecoderSpecificInfo` / `CodecPrivate`.
`Parser::set_extradata` reads it as an `AudioSpecificConfig`.

A parser that has been configured that way keeps the whole
`AudioSpecificConfig` and refuses to let a later ADTS header replace its
parameters. That is not a preference: a
raw AAC sample contains no ADTS header, so any sync word the scanner finds in
one is a coincidence, and a coincidence must not overwrite a record the
container stated. `tests/provider.rs` pins it with a synthetic 48 kHz frame fed
to a parser configured at 44.1 kHz.

### Packet duration, and why it does not care about SBR

`Parser::packet_duration` returns one frame's length in seconds, as an exact
`Rational`, and it takes the same two paths the paragraph above describes:

* **Configured** (MP4 `esds`, Matroska `CodecPrivate`) — a stream constant off
  the `AudioSpecificConfig`: `frame_length()` over `sampling_frequency`. The
  payload is not read at all, and must not be, for the coincidence reason above.
* **In-band** (MPEG-TS, a raw `.aac` file) — the payload is walked and each ADTS
  frame's `raw_data_blocks × 1024` samples are summed at that header's rate, so
  a PES payload holding several frames reports several frames.

**The `sampling_frequency` in both is the *core* rate, and that is the whole
trick.** The brief for this work described the quantity as "1024 samples per
frame, or 2048 for SBR"; both are true and they are the *same duration*, because
SBR doubles the output rate along with the sample count. `1024/22050` and
`2048/44100` are one `Rational`. A caller that reached for the reported
`sample_rate` — which is the *extension* rate for an SBR stream — and multiplied
by 1024 would halve every duration on every HE-AAC file. Answering in seconds,
inside the parser that holds both halves, removes the trap rather than
documenting it. `packet_duration_is_unchanged_by_sbr` pins it against the real
HE-AAC `esds` (`13 90 56 e5 a0`).

Measured against `ffprobe 8.1`:

| file | stream base | reference | exact |
|---|---|---:|---|
| AAC 44100 in Matroska (no `DefaultDuration`) | 1/1000 | 23 | 1024/44100 s |
| AAC 48000 in Matroska (no `DefaultDuration`) | 1/1000 | 21 | 1024/48000 s |
| AAC 44100 in MPEG-TS | 1/90000 | **2089** | 1024/44100 s |
| AAC 44100 in LOAS/LATM | 1/28224000 | 655360 | 1024/44100 s |
| AAC 44100 in MP4 | 1/44100 | 1024 | from `stts` |

The MPEG-TS row is a rounding witness as well as a value: `1024 × 90000 ÷ 44100`
is 2089.79 and the reference prints the truncation. **So AAC has the same gap
Opus does**, in Matroska and in MPEG-TS; MP4 does not, because `stts` states a
duration per sample and the container's statement always wins.

`LoasParser` answers from the `StreamMuxConfig`: `numSubFrames × frame_length`
over the first layer's core rate. It returns `None` until a configuration has
been read, because `useSameStreamMux` lets a frame omit one entirely and
guessing 1024 would be a fabrication.

### `sample_fmt` is the decoder's output format

`to_codec_parameters` reports `fltp`, on both the ADTS and the `AudioSpecificConfig`
paths. There is no sample format anywhere in AAC's syntax; `ffprobe` fills
`sample_fmt` from the decoder's chosen output, and it prints `fltp` for every
AAC stream measured — MP4, MOV, M4A, Matroska and MPEG-TS.

A parse-only crate naming a decoder's output format is a real wrinkle. The
alternative is worse: `sample_fmt` is inside the D6 byte-identity contract, and
leaving it `unknown` diverges on every AAC stream there is.

## How to change it

* **Adding a field to a header struct** is safe: both are `#[non_exhaustive]`.
* **The tables are format-dictated** (D7/D9 merger). Take a change from the
  standard, not from anywhere else, and update the corresponding
  `channel_configurations_match_the_reference` /
  `sampling_frequency_table_matches_the_reference` case in `src/tests.rs`.
* **`AudioSpecificConfig::read` stops after `GASpecificConfig`'s first three
  flags.** Everything past that — the program config element, the
  error-resilience flags, the layer descriptions — is decoder configuration and
  cannot change what a container reports. If you need it, add it *after* the
  existing fields and keep `bits_read` accurate, because LATM's
  `audioMuxVersion == 1` path uses the declared `ascLen` to skip and would
  desynchronise on a miscount.
* **Do not "fix" the D17 items.** They are annotated in place and pinned by
  tests whose names say so.
* **Gotcha: `GetAudioObjectType`'s escape adds 32, not 31.** The escape value
  itself is 31, so the extension range starts one above it. Getting this wrong
  shifts every object type from 32 up by one, which shows as ER AAC ELD
  reporting an unnamed profile instead of `ELD`.
* **Gotcha: ADTS `channel_configuration` is three bits.** It cannot express the
  11..=14 configurations an `AudioSpecificConfig` can, so
  `AdtsHeader::channel_configuration` is always 0..=7.

## Known divergences

### The first AAC packet of a single-track Matroska has no duration

Not reproduced, and the measurements are why. `ffprobe 8.1` prints
`duration=N/A` on the **first** packet of an AAC track in some Matroska files
and the codec-derived value on every packet after it:

| file | first packet | rest |
|---|---|---|
| `aac.mka` (AAC only) | `N/A` | 23 |
| `aac48.mka` (AAC only, 48 kHz) | `N/A` | 21 |
| two AAC tracks in one file | `N/A` on **both** tracks | 23 / 21 |
| AAC + Opus in one file | `N/A` on the AAC track only | 23 / 20 |
| **`av.mkv` (AAC + H.264)** | **23** | 23 |
| `opus.mka`, `flac.mka` | value | value |
| AAC in MPEG-TS | value | value |

It is not the priming: patching `CodecDelay` to zero, so the packet carries no
`Skip Samples` side data at all, leaves the `N/A` in place. It is not a seek or
a read position: `-read_intervals '2%…'` produces a duration on the first packet
it emits. It is not `probesize` or `analyzeduration`, which change nothing. And
`av.mkv` shows it is not even universal for AAC in Matroska — adding a video
track makes the value appear.

The pattern across the whole set is "the answer comes from the configuration
rather than from the packet, and the reference has not established it yet when
the first packet leaves its queue" — an artefact of its probing order, not a
rule about the format. Reproducing it would mean teaching `Parser` to report
*where* its answer came from, to serve one field value per track, and the
`av.mkv` row says even that would not be enough. Recorded here instead; it is
the single remaining `duration` divergence on Matroska AAC, 1 of 40 packets on
`aac.mka` and 0 of 40 on `av.mkv`.

### D17: `samplingFrequencyIndex` 15 is rejected in the core position only

ISO/IEC 14496-3 subpart 1 §1.6.2.4 Table 1.16 defines index 15 as an escape
followed by an explicit 24-bit `samplingFrequency`. The reference **rejects** it
in the core position — `invalid sampling rate index 15`, and the stream
disappears from `-show_streams` — while **accepting** the same escape in the
extension (SBR) position, where an explicit 12345 Hz reads back as
`sample_rate=12345`.

We reproduce both halves. `core_escape_index_is_rejected` and
`extension_escape_index_is_accepted` pin the asymmetry so a change in either
direction shows up as a test failure rather than silently.

### D17: ADTS `profile_ObjectType` 3 is `LTP`, not reserved

Table 4.79 lists the value as reserved for MPEG-4 ADTS. The reference maps it to
`audioObjectType` 4 and prints `LTP`. We do the same, because `profile()` is
uniformly `audioObjectType - 1`.

### `adts_error_check()` multi-block headers are not accounted for

When `protection_absent == 0` and `number_of_raw_data_blocks_in_frame > 0`, the
specification puts `raw_data_block_position[]` entries and one CRC *per block*
in the header. The reference frames on `aac_frame_length` alone and treats the
header as seven bytes plus two for the CRC; `AdtsHeader::header_len` does the
same. This affects `payload_len()` only, never framing.

### The reference reports HE-AAC in raw ADTS; we cannot

**This is the one place a parse-only build cannot match the reference, and it is
structural rather than a bug.**

Raw ADTS has no `AudioSpecificConfig`. HE-AAC in ADTS is signalled *implicitly*,
inside the payload, as an SBR extension element in a fill element — so the only
way to see it is to decode. Measured, on files the reference's own AudioToolbox
encoder produced:

| File | ADTS header says | `ffprobe` prints |
|---|---|---|
| AAC-LC | `sfi=4` (44100), `chancfg=2`, profile LC | 44100, 2 ch, `LC` |
| HE-AAC | `sfi=7` (22050), `chancfg=2`, profile LC | **44100**, 2 ch, **`HE-AAC`** |
| HE-AACv2 | `sfi=7` (22050), `chancfg=1`, profile LC | **44100**, **2 ch**, **`HE-AACv2`** |

Two further probes establish that this comes from the decoder and not from any
header parsing:

* Rewriting a plain LC stream's `samplingFrequencyIndex` to 7 makes the
  reference report 22050 — no doubling. So the doubling depends on payload
  content.
* Zeroing the payload of a valid file, header intact, makes the reference report
  `sample_rate=0`. So even the *un*-doubled rate is coming from the decoder.

Vaco ships no AAC decoder in the default build (D9), so for raw ADTS we report
the header's own values: 22050 Hz, the header's channel configuration, and `LC`.
An MP4 or LATM stream carrying the same audio *is* reported correctly, because
there the SBR and PS signalling is explicit in the configuration.

The same applies to the profile string for MP4: with a working payload the
reference upgrades `LC` to `HE-AAC`/`HE-AACv2` after its decoder has seen an SBR
or PS element; with a dead payload and an intact `esds` it prints `LC`, which is
what we print. Rate and channel count are unaffected — those come from the
configuration in both cases.

**If an AAC decoder is ever added under `patent-encumbered-aac-decode`, this is
the gap to close**, and the right shape is a payload-inspection pass exposed
from the decoder crate rather than anything added here.

### `aac_latm` is reported as `aac`

The reference has a distinct codec id for LATM-carried AAC and prints
`codec_name=aac_latm`. `vaco_codec_core::CodecId` has only `Aac`, and that enum
is not ours to extend. `LoasParser` therefore reports `CodecId::Aac`. Adding
`CodecId::AacLatm` to `vaco-codec-core` would close it.

## Performance

`cargo bench -p vaco-parse-aac`, on an Apple Silicon laptop. Numbers are medians.

| Benchmark | Time | Throughput |
|---|---|---|
| `adts_header` (one header) | 4.8 ns | 1.45 GB/s |
| `asc` (`12 10`, plain LC) | 25 ns | — |
| `asc` (7-byte explicit PS) | 21 ns | — |
| `adts_stream_frames/512` | 36.7 µs | 5.36 GB/s |
| `adts_resync_zeros/65536` | 17.7 µs | 3.71 GB/s |
| `adts_resync_near_misses/65536` | 21.1 µs | 3.10 GB/s |

The two resync rows are the ones to watch. `near_misses` plants a sync word every
251 bytes so that a full header parse is attempted and rejected constantly; it
costs 20% over a plain scan, and it stays *linear*. If that ratio ever grows,
something in the rejection path has started re-scanning bytes it already looked
at, which is exactly how a corrupt file turns into a hang.

## Configuration

None. No features, no environment variables, no build-time options.

Both parsers take a `vaco_limits::Limits` at construction and charge every packet
allocation against it, which is the only knob. `Limits::strict()` is right for
untrusted input; the deferred-frame buffer is bounded by the format's own 13-bit
length field rather than by the budget.

## Dependencies

| Crate | Why |
|---|---|
| `vaco-bitstream` | `BitReader` — the padded-body/checked-tail reader every syntax table is read through |
| `vaco-codec-core` | the `Parser` trait, `CodecParameters`, `Profile`, `CodecId` |
| `vaco-chlayout` | channel layouts. Its vocabulary, never a redefinition |
| `vaco-core` | the error taxonomy |
| `vaco-limits` | `Budget` for packet allocation |
| `vaco-packet` | `Packet` |

Dev-only: `proptest`, `divan`.

## Testing and probing

`cargo test -p vaco-parse-aac` runs 34 unit and property tests. Every expected
value carries a `// measured:` comment naming the `ffprobe 8.1` observation it
came from.

Fuzz targets: `parse_aac_adts`, `parse_aac_asc`, `parse_aac_latm`.

### How the reference was probed

Plan 13 §1b says to pick the entry point with the fewest layers between the
input and the parser. For an `AudioSpecificConfig` that is a real MP4 with its
`esds` rewritten in place:

1. Encode a file: `ffmpeg -f lavfi -i sine=f=440:r=44100:d=2 -ac 2 -c:a aac_at
   -profile:a 4 -b:a 64k out.m4a`. `aac_at` (AudioToolbox) is the only AAC
   encoder in this build that produces HE-AAC; profile `4` is HE-AAC and `28` is
   HE-AACv2, as numbers, because the named constants are not registered for that
   encoder.
2. Zero the `mdat` payload, leaving the `moov` intact. This isolates
   configuration parsing from anything the decoder contributes — with a dead
   payload the reported rate and channel count still come from the `esds`, while
   the profile string falls back to the core object type.
3. Replace the `DecoderSpecificInfo` payload, fixing the descriptor lengths and
   every enclosing box size. The `moov` follows the `mdat` in these files, so
   chunk offsets are unaffected by a size change. The patcher round-trips the
   original bytes exactly, which is the check that it is not itself the thing
   being measured.
4. `ffprobe -v error -show_streams -of json`.

For ADTS, the same idea one level simpler: rewrite header fields in place, which
never changes a length. For LOAS, generate with `-f latm` and read the
`StreamMuxConfig` back out of the first frame.

**Watch for the confounds.** ADTS `channel_configuration` is three bits, so a
patcher that masks with `& 7` silently rewrites configuration 11 as 3 — which
looks like a table disagreement and is not. And a base file's `mp4a` box carries
its own channel count and sample rate, so probe channel-count questions against
a base file whose `mp4a` count differs from the answer you expect, or you cannot
tell which one you measured.
