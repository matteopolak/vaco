# `vaco-mux-mpegts`

Layer 4. The MPEG-TS muxer: `-f mpegts` (ISO/IEC 13818-1). FM-25, issue #576.

---

## What it is

Writes an MPEG-2 Transport Stream: 188-byte packets carrying a PAT, a PMT, a
DVB SDT, and one PES-packetised elementary stream per declared track, with
PCR insertion and per-PID continuity counters. Optionally wraps every packet
in a four-byte Blu-ray M2TS timestamp prefix.

One registration, `mpegts` — measured (`ffmpeg -muxers | grep -i ts`), the
reference has exactly one MPEG-TS muxer; M2TS is a *mode* of it, not a second
format name, and this crate follows suit.

This crate owns the muxer. The PAT/PMT/SDT section *format* — what bytes a
table actually contains — is owned by `vaco-format-mpegts-tables`, alongside
that crate's existing PSI readers, so there is one definition of "what a PAT
looks like" for both the demuxer and this muxer to agree on (D19).

---

## How it works

### Layout

| Module | Contents |
|---|---|
| `tsw` | the 188-byte transport packet: header, adaptation field, PCR encoding, per-PID continuity counters, M2TS wrapping |
| `pes` | PES header encoding, including the 33-bit PTS/DTS marker-bit field |
| `options` | `-mpegts_*` flags and every option's measured default |
| `mux` | `MpegTsMuxer`, the `vaco_format_core::Muxer` implementation |

### The pipeline, per packet

1. `Muxer::add_stream` assigns a PID (sequential from `-mpegts_start_pid`,
   default `0x0100`) and resolves a `stream_type` via
   `vaco_format_mpegts_tables::stream_type::for_codec`. The first video
   stream (or, failing that, the first stream declared) becomes the PCR PID,
   decided in `Muxer::init` once every stream is known.
2. `write_header` emits a PAT, a PMT and an SDT immediately.
3. `write_packet`:
   - Repeats the PAT/PMT and, separately, the SDT if the configured period
     has elapsed, `-mpegts_flags resend_headers` is set, or (for PAT/PMT only)
     `pat_pmt_at_frames` is set and this is a video keyframe.
   - Rewrites the payload to Annex B if the stream declared a length-prefixed
     framing (`CodecParameters::video::nal_length_size`), via
     `vaco_format_nalu::convert::length_prefixed_to_annexb` — a transport
     stream has no out-of-band configuration record, so H.264/HEVC/VVC must
     carry NAL units start-code-framed in the payload itself.
   - Builds a PES header (`crate::pes`) with PTS-only or PTS+DTS timestamps,
     and `PES_packet_length` `0` ("unbounded") for video by default —
     measured: `-omit_video_pes_length` defaults to `true`.
   - Attaches a PCR to the PCR PID's packet when the configured period has
     elapsed, sets `random_access_indicator` on a video keyframe (unless
     `omit_rai`), and marks the stream's first packet discontinuous under
     `initial_discontinuity`.
   - Hands the finished PES bytes to `tsw::TsWriter::write_pes`, which slices
     them into 188-byte cells and advances the PID's continuity counter —
     **only on packets that carry payload**, which every packet this writer
     builds does; see `tsw`'s module docs for why the distinction is kept
     explicit anyway.

### The two padding conventions, and why there are two

A short final packet of a run has to reach 188 bytes somehow, and PES and PSI
pad differently:

* **PES** payload is elementary-stream bytes. Appending `0xFF` after the last
  of them would corrupt the stream, so the pad lives in a stuffed adaptation
  field instead (`TsWriter::write_pes`).
* **PSI** sections are followed by `0xFF` as ordinary stuffing-to-end-of-packet
  — `vaco_format_mpegts_tables::section::SectionAssembler`'s own doc says so —
  so a short PAT/PMT/SDT packet is padded with trailing `0xFF` **payload**
  bytes and never grows an adaptation field just to reach 188
  (`TsWriter::write_section`).

Conflating the two was the first bug this crate's own round-trip test found:
an early version always attached a stuffed adaptation field, which produces a
file that *parses* but does not match what a real PSI PID looks like.

### The 33-bit timestamp

`crate::pes::encode_timestamp` writes the interleaved-marker-bit layout
Table 2-21 defines. It is checked three ways, per the brief's own emphasis
that this field is the single most error-prone part of the format:

1. A property test asserts every value in `0..2^33` round-trips through
   **`vaco_demux_mpegts::pes::decode_timestamp`** directly — an independently
   written parser, not a second transcription of this crate's own encoder.
2. A property test builds a complete PES header with random PTS/DTS/length
   and parses it back with `vaco_demux_mpegts::pes::PesHeader::parse`.
3. `tests/roundtrip.rs` mux → demux's *whole file* and asserts the packets
   that come back carry the PTS that went in.

### PCR

Attached to the PCR PID's own next packet — never a dedicated null-payload
"PCR packet" — with `extension` fixed at `0`: this muxer does not track
sub-90-kHz-tick timing, and `extension = 0` is a fully spec-legal PCR. The
retransmission interval defaults to `tsw::DEFAULT_PCR_PERIOD_MS` (100 ms, the
specification's own §2.7.2 ceiling) rather than a reconstruction of the
reference's own scheduler — see *What is measured vs. decided* below.

### M2TS

Every 188-byte packet is prefixed with a 4-byte arrival time stamp: two
reserved bits (`0`) plus a 30-bit, 27 MHz counter. This crate's counter is
derived from bytes written so far at a configurable (or nominal, 20 Mbit/s)
rate, which is monotonically increasing and Blu-ray-legal but **not** proven
byte-identical to the reference — see below.

---

## How to change it

* **A new codec** needs a `stream_type::for_codec` arm in
  `vaco-format-mpegts-tables` (not here — see that crate's docs) and, if the
  container needs bitstream reframing the way H.264/HEVC/VVC do, a case in
  `MpegTsMuxer::maybe_convert`.
* **A new `-mpegts_flags` bit**: add it to `options::MpegTsFlags`, wire its
  effect into `MpegTsMuxer::write_packet` or `add_stream`, and note in this
  file whether it changed observable output or is accepted-but-inert (like
  `latm`'s payload framing today).
* **Changing PCR/PAT/PMT/SDT scheduling**: `MpegTsMuxer` tracks
  `clock_90k`/`last_pat_clock`/`last_sdt_clock`/`last_pcr_clock` in the
  stream's own 90 kHz domain, not wall-clock time — deliberately, since
  `vaco-time` exists for wall-clock reads this crate has no reason to make
  (there is nothing here that needs the *host* clock; every timestamp comes
  from the media itself, which is also what keeps this code wasm-safe with no
  special-casing).
* **Gotcha**: `TsWriter::write_payload`'s two pad styles (see above) are not
  interchangeable. If a new payload kind is added, decide up front which one
  applies — elementary-stream bytes always want `write_pes`'s adaptation-field
  padding; anything that is itself a PSI-style section wants
  `write_section`'s trailing-byte padding.
* **Gotcha**: `payload_unit_start_indicator` is set on the first packet of
  *every* `write_payload` call, regardless of whether a `pointer_field` is
  present. An earlier version of this code tied PUSI to "has a pointer field",
  which is a PSI-only concept — the result was PES packets that never set
  PUSI at all, and the sibling demuxer read zero packets back. `tests/
  roundtrip.rs` is what caught it; see *What was measured vs. assumed* below.

---

## Configuration

Everything under `options::MpegTsMuxOptions` and `options::MpegTsFlags`.
Names and defaults are measured against `ffmpeg -h muxer=mpegts` (ffmpeg 8.1,
`LC_ALL=C`) rather than recalled — this is the exact transcript:

```
Muxer mpegts [MPEG-TS (MPEG-2 Transport Stream)]:
    Common extensions: ts,m2t,m2ts,mts.
    Mime type: video/MP2T.
    Default video codec: mpeg2video.
    Default audio codec: mp2.
MPEGTS muxer AVOptions:
  -mpegts_transport_stream_id <int>   (default 1)
  -mpegts_original_network_id <int>   (default 65281)
  -mpegts_service_id <int>            (default 1)
  -mpegts_service_type <int>          (default digital_tv)
  -mpegts_pmt_start_pid <int>         (default 4096)
  -mpegts_start_pid <int>             (default 256)
  -mpegts_m2ts_mode <boolean>         (default auto)
  -muxrate <int>                      (default 1 — meaning "unset", not 1 bit/s)
  -pes_payload_size <int>             (default 2930)
  -mpegts_flags <flags>               (default 0)
     resend_headers / latm / pat_pmt_at_frames / system_b /
     initial_discontinuity / nit / omit_rai
  -mpegts_copyts <boolean>            (default auto)
  -tables_version <int>               (default 0)
  -omit_video_pes_length <boolean>    (default true)
  -pcr_period <int>                   (default -1, "auto")
  -pat_period <duration>              (default 0.1 = 100 ms)
  -sdt_period <duration>              (default 0.5 = 500 ms)
  -nit_period <duration>              (default 0.5 = 500 ms)
```

`omit_rai` is not in this issue's original brief; it was found only by
running the probe above, which is exactly the point of measuring rather than
recalling — the brief's own flag list was a close but incomplete guess.

Everything in that table is a field of `MpegTsMuxOptions` or a
`MpegTsFlags` bit **except**: `mpegts_copyts` (out of scope — this trait's
timestamp offsetting happens upstream, in `vaco_format_core::interleave`, not
in a per-container option) and `nit`/`nit_period` (accepted as a flag/field
for round-tripping a caller's option string, but this muxer never writes a
NIT regardless — see *Deferred* below).

`-mpegts_flags` maps onto `options::MpegTsFlags`, `-mpegts_service_type` onto
`options::ServiceType`. None of this is routed through
`vaco_format_core::FormatOptions` — that type is the options every container
shares, and these are MPEG-TS-specific in the same way `vaco-mux-mp4`'s
`movflags` are MP4-specific. `MpegTsMuxer::with_options` is the entry point a
caller who needs anything beyond the registry's plain-defaults `MUXER.open`
uses; `MUXER.open` always builds `MpegTsMuxOptions::default()` with M2TS
disabled, because `MuxerDesc::open`'s signature is frozen at
`fn(Box<dyn MediaSink>) -> Result<Box<dyn Muxer>>` and has no options channel.

---

## Dependencies

`vaco-core`, `vaco-limits`, `vaco-io`, `vaco-packet`, `vaco-codec-core`,
`vaco-format-core`, `vaco-format-mpegts-tables` (the PSI table writers and
`stream_type::for_codec`), `vaco-format-nalu` (Annex-B ↔ length-prefixed
conversion — a *format* crate, not `vaco-parse-h264`; D14.1 forbids the
latter edge from a `vaco-format-*`/`vaco-mux-*` crate). `bitflags`.

Dev-only: `proptest`, and `vaco-demux-mpegts` — used **only** by
`crate::pes`'s timestamp round-trip tests and `tests/roundtrip.rs`'s
whole-file mux-then-demux check. The production dependency graph does not
include the demuxer; this is the same pattern `vaco-mux-mp4` uses with
`vaco-demux-mp4`.

---

## What is measured vs. decided

The wire-level pieces are checked directly against an independent parser
(the sibling demuxer) and are therefore the *least* likely part of this
crate to be silently wrong: the transport packet header, the adaptation
field and PCR encoding, PAT/PMT/SDT section syntax, and the PES 33-bit
timestamp field.

The **scheduling policy** is this crate's own reasonable reading of the
specification's bounds, not a byte-for-byte reproduction of the reference's
internal scheduler — which is not observable without reading its source
(D7), and this crate does not:

* **PCR interval.** Measured with `ffmpeg -f lavfi -i testsrc=rate=25:d=2 -c:v
  mpeg2video -f mpegts`: PCR arrives every 80 ms at 25 fps and every 100 ms at
  10 fps — not a flat period, and not simply "once per frame" either. This
  crate uses a flat 100 ms ceiling (the specification's own §2.7.2 bound)
  instead of reconstructing whatever frame-timing-aware heuristic produces
  those two numbers.
* **M2TS arrival timestamps.** Measured with `-mpegts_m2ts_mode 1`: the
  first few ATS values are **not monotonic in file order** — the SDT
  packet's ATS was larger than the PAT's, even though the SDT is the earlier
  packet in the file. That is evidence the reference computes ATS from an
  internal multiplexing order (packets are decided before they are placed),
  which this crate does not reconstruct. This crate's ATS is instead derived
  from cumulative bytes written at a configurable (or nominal) bit rate:
  monotonic and Blu-ray-legal, not a claim of byte-identity.
* **AC-3/E-AC-3/DTS `stream_type` assignment.** `system_b` chooses between
  ATSC's own values (`0x81`/`0x87`/`0x8A`) and DVB's `0x06` + registration
  descriptor convention. Both directions are verified to round-trip through
  `vaco-format-mpegts-tables::resolve`; which one the reference picks for
  *unset* `system_b` under every possible codec combination was not
  independently re-derived beyond the flag's own documented meaning.

## Deferred / not implemented

Named here rather than silently absent, per the honesty bar this project
holds closing an issue to:

* **`-mpegts_flags nit`**: accepted as a flag, has no effect. This muxer
  never writes a Network Information Table.
* **`-mpegts_flags latm`**: accepted; changes the PMT's AAC `stream_type`
  (`0x11` instead of `0x0F`) but does **not** rewrite the payload into
  LATM/LOAS framing — that needs an `AudioSpecificConfig`-aware bitstream
  rewrite this crate does not have a source for.
* **AAC ADTS wrapping.** MPEG-TS carries AAC as ADTS frames; this muxer
  writes whatever bytes `Packet::payload()` already contains and does not
  synthesize ADTS headers for a raw/LATM source. A caller must supply
  ADTS-framed AAC.
* **`-muxrate` (CBR padding).** The option is accepted and stored, but this
  muxer never inserts null (`0x1FFF`) stuffing packets to hold a constant
  output rate; VBR output only. `tsw::TsWriter::next_cc`'s
  "only advances on a payload-carrying packet" rule is already correct for
  the day null-packet stuffing is added — it just has no caller yet.
- **PES packet splitting via `-pes_payload_size`.** One access unit is
  always one PES packet (further sliced into 188-byte cells by `tsw`); this
  crate does not split one AU across several smaller PES packets the way the
  reference's `-pes_payload_size` implies it can for non-video streams.
* **Parameter-set injection from `extradata`.** When a stream's SPS/PPS live
  only in `CodecParameters::extradata` (common for an MP4-sourced stream) and
  are not repeated in-band by the encoder, this muxer does not splice them in
  front of each keyframe. Only the *framing* of whatever bytes `write_packet`
  receives is corrected (length-prefixed → Annex B); parameter-set placement
  is unchanged. A stream whose encoder already repeats SPS/PPS in-band (the
  common case for a live Annex-B source) is unaffected.
* **Multi-section PAT/PMT/SDT.** `vaco-format-mpegts-tables::write_pat`/
  `write_pmt`/`write_sdt` return `None` rather than spanning a table across
  more than one section; not reachable in practice for the single- or
  few-program streams this muxer produces, but a PMT with an implausibly
  large descriptor set would fail to mux rather than being split.

---

## Testing

* Unit tests in every module (`pes`, `tsw`, `options`, `mux`, `lib`), and
  `proptest` on both round-trippable properties: the 33-bit timestamp
  (against the sibling demuxer's parser) and "any payload length reassembles
  to the original bytes" (`tsw`'s TS-packet splitter, against the sibling
  crate's own `TsPacket::parse`).
* `tests/roundtrip.rs`: mux with this crate, demux with `vaco-demux-mpegts`,
  and check the streams, payloads and timestamps that come back match what
  went in — including one run in M2TS mode. This is the test that found the
  PUSI bug above; a unit test on `tsw` alone could not have, because the bug
  was in how `mux.rs` and `tsw.rs`'s contracts disagreed about what a PES
  packet's start looks like.
* Fuzz target `mpegts_mux_packet` (D6): `MpegTsMuxer::write_packet` over an
  arbitrary payload, with `nal_length_size` toggled by an input byte so the
  Annex-B conversion path (parsing attacker-controlled length prefixes) is
  exercised on every run. Asserts output is always a whole number of
  188-byte cells and that output growth is boundedly proportional to input —
  guarding the one place this muxer could, in principle, amplify a small
  input into a large one. 30-second run: `exit=0`, `execs=#1655520` (a second
  independent 30-second run: `execs=#1688960`, also clean).
  `find fuzz/artifacts -type f` is empty.
