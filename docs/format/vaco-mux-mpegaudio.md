# `vaco-mux-mpegaudio`

Layer 4. The `mp3` muxer, registered under the reference's own name `mp3`
(long name `MP3 (MPEG audio layer 3)`).

---

## What it is

Almost a byte-for-byte pass-through of the packet stream — every packet
handed to `write_packet` is already a complete, self-delimited MPEG audio
frame — plus two things the reference always does on top of a plain copy:

1. An empty `ID3v2.4` header at the start of the file.
2. A synthesized Xing/Info + LAME header frame, immediately after it,
   describing the stream that follows.

---

## How it works

### The reference writes a placeholder header even under `-c copy`

Measured directly: `ffmpeg -c copy -fflags +bitexact -f mp3` on a CBR
source with **no** original Xing tag still produces an output exactly one
frame longer than the source, starting with a freshly built Xing/Info+LAME
frame. This is not conditional on the source having had one — every mp3
this muxer writes gets one. `write_header` builds it from the stream's
sample rate and channel count (both known at `add_stream` time, so this
does not need to wait for the first packet); `write_trailer` patches the
frame/byte counts and the gapless delay/padding once the real totals are
known, which needs a seekable sink.

### `"Xing"` vs `"Info"` is decided by bit rate, not byte length

A real CBR frame still alternates length by one byte through the padding
bit to average out a fractional bits-per-frame — byte length is the wrong
signal for "is this CBR". Measured against two real files (a true VBR
source and a true CBR one): the reference calls the tag `"Info"` exactly
when every frame's own header declares the same bit-rate index, `"Xing"`
otherwise. `write_packet` parses each packet's header for its bit-rate
index and tracks whether it has stayed constant.

### The placeholder frame's own bit rate

Measured against a VBR source: the placeholder frame's declared bit rate
was **not** any real frame's bit rate, but the smallest standard bit-rate
index whose frame length is large enough to hold the full Xing+LAME
payload (`choose_bitrate_index`'s fallback branch). Measured against a CBR
source: the placeholder used the stream's own declared bit rate instead,
which already exceeded that minimum. Both are reproduced; the exact
decision rule for cases in between (a real bit rate below the minimum) is
inferred, not separately measured.

### Gapless delay/padding are inverted back from packet side data

The muxer receives already-decoder-adjusted `SkipSamples` side data (see
`vaco-demux-mpegaudio`'s doc for the `+529`/`-529` decoder-delay
adjustment) and inverts it before writing the LAME extension, so a
demux→mux round trip reproduces the *original* encoder-stated delay and
padding, not the decoder-adjusted ones.

---

## Fidelity: what was measured, and what was not

| Aspect | Status |
|---|---|
| Output file length | Exact match against the reference on both a CBR and a VBR source |
| Placeholder frame's header (version/layer/sample rate/channel mode) | Exact match |
| `"Xing"` vs `"Info"` selection | Exact match on both measured cases |
| Frame count / byte count fields | Exact match |
| Gapless delay/padding fields | Exact match |
| Xing TOC (the 100-byte seek table) | **Not computed** — written as zero. The reference computes a real byte-percentage table; left as `TECH-DEBT` |
| Quality field, `MusicCRC`, `InfoTagCRC` | **Not computed** — written as zero. The reference's exact CRC-16 variant was not identified from the one measured sample; see `TECH-DEBT.md` |
| Encoder-id string | Fixed to `"Lavf"` + zero padding, matching what the reference itself writes on copy |

Net effect: a copy round trip is byte-identical to the reference everywhere
except the 100-byte TOC and the four CRC/quality bytes inside the
synthesized Xing/LAME frame — everything a decoder or a gapless player
actually reads (frame data, frame/byte counts, delay/padding) matches
exactly.

## How to change it

- TOC synthesis needs the muxer to record a cumulative byte offset per
  packet (it already tracks a running total) and compute the standard
  100-entry percentage table at `write_trailer`.
- The CRC fields need the reference's exact CRC-16 parameters, which one
  sample was not enough to pin down; a second real LAME-tagged file with a
  different payload would let the parameters be solved for algebraically.

## Configuration

None — no muxer-private options.

## Dependencies

`vaco-format-mpegaudio` (frame header encode/decode, `version_for_sample_rate`),
`vaco-format-core`, `vaco-io`, `vaco-packet`, `vaco-codec-core`.
