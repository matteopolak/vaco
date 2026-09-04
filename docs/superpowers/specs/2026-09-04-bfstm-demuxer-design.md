# BFSTM/BCSTM demuxer design

## Goal

Add the `bfstm` registry path requested by tracker issue #620 without changing
the newly landed BRSTM implementation. The component demuxes the measured
stereo Nintendo DSP-ADPCM subset of Wii U `FSTM` and Nintendo 3DS `CSTM`
containers in either byte order.

## Sources and measured contract

The Custom Mario Kart 8 Wiki's BFSTM page and 3dbrew's BCSTM page independently
describe the 0x40-byte file header, endian marker, sized block references,
`INFO` stream/channel tables, DSP coefficients, `SEEK` histories, and
block-interleaved `DATA`. The BFSTM page also states the per-channel stored-size
formula `(block_count - 1) * block_size + final_block_padded_size`.

Hand-built files derived from those layouts were accepted by the installed
`ffprobe` 9.0.1 binary for `FSTM` and `CSTM`, in big and little endian. The
bounded sweep used stereo DSP-ADPCM blocks of 16, 32, 64, 96, and 256 bytes,
plus full and half-sized final blocks. A two-block 32/16-byte fixture reports:

- one 32 kHz stereo stream, time base `1/32000`, duration 84 samples;
- packet timestamps 0 and 56, durations 56 and 28;
- packet sizes 144 and 112 bytes;
- an 80-byte prefix containing file-endian raw-byte/sample counts, 64 bytes of
  channel coefficients, and one 8-byte interleaved SEEK history entry;
- unpadded channel payload bytes, skipping the physical final-block padding.

The reference names the big-endian codec `adpcm_thp` and the little-endian
codec `adpcm_thp_le`. Vaco has no corresponding `CodecId`, so the demuxer leaves
the codec identity absent rather than mapping it incorrectly.

## Structure

Implement a standalone `bfstm.rs`. A small local byte-order enum selects the
existing `IoContext` big- or little-endian readers and serializes the packet
prefix in the same order as the source file. Opening performs these steps:

1. Validate `FSTM` or `CSTM`, the BOM, the 0x40-byte header, and the three sized
   references for `INFO`, `SEEK`, and `DATA`.
2. Resolve the `INFO` references relative to their documented bases, then read
   stream geometry and the two channel coefficient tables.
3. Validate the `SEEK` table and sample-data reference against the declared
   section ranges and the physical source size.
4. Expose one audio stream with sample rate, stereo layout, time base, duration,
   and frame count.
5. Emit one packet per sample block, seeking past per-channel final padding and
   attaching duration ticks.

All offset arithmetic is checked. Packet allocation uses the crate's existing
permissive `Budget`, consistent with the other standalone demuxers in this
crate.

## Scope and refusals

The component accepts only two-channel DSP-ADPCM with the measured block sizes,
the standard `block_size / 8 * 14` sample relationship, a full or half-sized
last block, equal physical padding to the full block size, four SEEK bytes per
channel per block, and one SEEK interval per full block. PCM, IMA ADPCM, mono,
three-or-more channels, regions, and other block geometry return named
`Unsupported` errors.

Malformed signatures, references, sizes, and truncated chunks return
`InvalidData`. Seeking is not implemented. Probe recognizes both magics and
scores a documented BOM more strongly.

## Verification

Tests cover both magics, both byte orders, packet bytes/timestamps/durations,
final padding removal, malformed offsets, and named scope refusals. The checked
in fixture is the exact big-endian 32/16-byte file measured above. Final checks
compare `vaco-probe` with `ffprobe` on stream structure, packet count, packet
sizes, timestamp/duration ticks, and aggregate sample count, then run the
focused/full crate suite and registry, provenance, layer, and reachability
checks.
