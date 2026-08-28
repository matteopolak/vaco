# `vaco-format-spdif`

## What it is

IEC 61937 (compressed audio encapsulated for transport over S/PDIF) and
SMPTE 337M (the broader standard IEC 61937 is a 16-bit-word profile of):
`spdif` demux + mux, and `s337m` demux only (no muxer — matches the
reference). Registers as `spdif` (extension `spdif`, muxer only — the
demuxer has none) and `s337m` (no extension, no MIME type either way).

## How it works

### The shared burst shape (`iec61937`)

Every burst is `Pa Pb Pc Pd <payload, byte-swapped> <zero padding>`:

* `Pa`/`Pb` = `0xF872`/`0x4E1F`, fixed sync words.
* `Pc`'s low 7 bits are the data type. Measured off real `ffmpeg -f spdif`
  captures: AC-3 = 1, MPEG-1 layer 2/3 = 5, DTS = 11, E-AC-3 = 21.
* `Pd` is a length code — measured to count **bits** for AC-3 specifically
  (a 192 kb/s and a 384 kb/s AC-3 frame at 48 kHz produced `Pd` = 6144 and
  12288, exactly 8x their real byte lengths). Not assumed to generalise to
  the other three data types without measuring them the same way.
* The payload's bytes are written with every adjacent pair swapped — AC-3's
  own sync word `0x0B77` appears in a burst as bytes `77 0B`. This falls out
  of writing the elementary stream as 16-bit big-endian-grouped words and
  then encoding *those* words in the burst's own byte order (little-endian
  by default).

There is no shared `Endian` type here — `vaco-scale::geometry::Endian`
already owns that concept (D19), and pulling a video-scaling crate into an
audio-container crate's dependency graph for one two-variant enum was worse
than a `bool`. See `iec61937.rs`'s module docs.

### `spdif`: demux + mux, byte-identical

**The burst size is a measured constant (6144 bytes for AC-3), not a scan
for the next sync word.** Three separate captures — 192 kb/s and 384 kb/s
AC-3 at 48 kHz, 192 kb/s at 44.1 kHz — all produced exactly 6144-byte
bursts regardless of AC-3's own frame byte length, matching the spec's
"1536 samples x 4 bytes" repetition period for that data type specifically.
`SpdifDemuxer::read_burst` reads whole 6144-byte bursts; a payload
declared longer than `6144 - 8` is refused before anything is read for it.

`SpdifMuxer::write_packet` validates the payload starts with a real AC-3
sync frame header (`ac3::parse`) before wrapping it, so a caller handing
non-AC-3 bytes under an AC-3 stream fails loudly rather than producing a
burst nothing can read.

**Remuxing a real reference `.spdif` file is byte-identical**
(`tests/reference_files.rs::remuxing_a_real_sample_reproduces_it_byte_for_byte`),
verified against a 4-burst fixture (distinguishing input: more than one
burst, so the fixed-size read is actually exercised past the first).

Stream metadata (`sample_rate`, channel layout) comes from a minimal,
fixed-position read of the AC-3 sync frame's own header (`ac3.rs`) — `fscod`
for sample rate, `acmod` for channel count. **`lfeon` (the LFE channel) is
not read**: it sits at a bit offset that depends on `acmod`'s own value, so
reading it needs a real bit cursor, not a fixed byte offset. A 5.1 stream
reports 5 channels, not 6. Documented, not guessed at.

### `s337m`: a thin, honestly-scoped wrapper

`S337mDemuxer` delegates entirely to `SpdifDemuxer` today (see `s337m.rs`'s
module docs for the full reasoning). In short: this reference build's own
`-f s337m` refuses every data type this crate could generate a sample for —
AC-3, MPEG-1 layer 2/3, DTS and E-AC-3 all print `Data type 0x.. in SMPTE
337M is not implemented`, even though the *identical bytes* open cleanly
under `-f spdif`. That is a decode-completeness gap in the reference, not a
framing difference to reproduce, and there is no successful `-f s337m` run
in this environment to verify anything wider against. This crate supports
exactly the one case it can verify (16-bit AC-3 bursts, byte-identical to
`spdif`'s own) and is explicit about not supporting the 20/24-bit
"professional" SMPTE 337M word-packing the standard also defines — no muxer
anywhere can produce a sample of that to test against.

`s337m::probe` always returns `ProbeScore::NONE`: `ffprobe` picks `spdif`
unprompted (`probe_score=100`) for every sample this crate can generate, so
there is no positive evidence for what should make `vaco` auto-select
`s337m` instead. `-f s337m` still opens content by explicit name.

`S337M_DEMUXER` went unregistered for a while after it was written: FM-54
landed before `vaco-demux-raw`'s own (much less complete) `s337m` demuxer
was noticed, and `gen-registry` refuses two components claiming one name.
Resolved in `planning/TECH-DEBT.md`'s "`s337m` is registered twice" entry —
this crate's parser is the one that actually reads the burst preamble, so
it kept the name; `vaco-demux-raw` dropped its registration.

## How to change it

* **MPEG-1 layer 2/3, DTS, E-AC-3 support**: needs the same measurement
  `ac3_payload_len_bytes` got — real captures at more than one bitrate,
  confirming whether `Pd` is bits or bytes for that data type and what the
  fixed (or computed) burst size is. Do not guess it symmetric with AC-3.
* **`lfeon`/6-channel reporting**: needs a real bit-cursor read of
  `acmod`'s variable-length preamble (`cmixlev`/`surmixlev`/`dsurmod` are
  present or absent depending on `acmod`'s value) — see `ac3.rs`'s module
  docs for exactly what is and is not read today.
* **20/24-bit SMPTE 337M ("professional") support**: would need a real
  sample from hardware or a muxer this workspace does not have; do not
  invent the word-packing convention from the published standard's text
  alone without a byte-level measurement to check it against (D17).

## Configuration

| Item | Default | Meaning |
|---|---|---|
| `SpdifMuxer::big_endian()` | off (little-endian) | Mirrors `-spdif_flags be` — one-directional, see docs above |
| `demux::AC3_BURST_BYTES` | 6144 | Measured AC-3 burst size |
| `iec61937::DATA_TYPE_AC3` | 1 | The only data type either demuxer decodes packets for |

## Dependencies

`vaco-core`, `vaco-io`, `vaco-limits`, `vaco-packet`, `vaco-format-core`,
`vaco-codec-core` (`CodecId::Ac3`), `vaco-chlayout` (`ChannelLayout::default_for`
for the declared channel count). No `vaco-parse-*` dependency and no
`vaco-scale` dependency — see "How it works" on the `Endian` decision.

## What was and was not measured

Verified directly against real `ffmpeg 8.1 -f spdif` output (2026-08-27):

* `Pa`/`Pb` sync words, `Pc` data-type values for four codecs, `Pd`'s
  bits-not-bytes convention for AC-3 specifically.
* The fixed 6144-byte AC-3 burst size, across two bitrates and two PCM
  sample rates.
* The default byte-swap of payload words, and that `-spdif_flags be`
  output is **not** read back by `ffmpeg -f spdif` (checked directly).
* Demuxer vs. muxer asymmetry: no `Common extensions:`/`Mime type:` on the
  demuxer side at all, both present on the muxer side; differing long
  names between the two directions.
* Full remux round trip, byte-identical, against a real 4-burst capture.
* That `-f s337m` refuses AC-3/MPEG/DTS/E-AC-3 content in this reference
  build, verbatim error text included.

**Not measured, and known to be absent, not merely approximate**:

* MPEG-1 layer 2/3, DTS, E-AC-3 burst framing (`Pd` unit, burst size) —
  refused with `Error::Unsupported`, not guessed at.
* `lfeon`/6-channel AC-3 reporting.
* SMPTE 337M's 20/24-bit "professional" word-packing mode.
* `s337m`'s own probe/auto-detection behaviour when it *does* support a
  data type — no successful reference run exists in this environment to
  measure that against.
