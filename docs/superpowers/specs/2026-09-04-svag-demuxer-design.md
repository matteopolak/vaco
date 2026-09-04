# SVAG Demuxer Design

## Goal

Add a reachable demux-only `svag` container path to
`vaco-format-misc-audio` that matches the stream fields and packet stream
reported by `ffprobe` 9.0.1 for valid Konami PS2 SVAG files.

## Format contract

The input starts with a 20-byte little-endian header: `VAGm`, followed by
`data_size`, `sample_rate`, `channels`, and `interleave`, all `u32`. Audio data
starts at byte 20 and contains PS-ADPCM blocks of 16 bytes per channel. The
reference emits physical input through EOF in packets of
`channels * interleave` bytes. A full packet advances by
`28 * (interleave / 16)` samples. A short final packet is emitted as corrupt
without timestamps or duration.

Stream duration is independent of physical EOF and packet grouping. It is
`floor(data_size / (16 * channels)) * 28` ticks at a `1 / sample_rate` time
base. This intentionally preserves the reference's behavior when declared and
physical sizes disagree.

## Architecture

Implement a small bespoke `SvagDemuxer` in `svag.rs`. Reusing `BlockDemuxer`
would be incorrect because that engine clamps reads to a declared length and
drops partial trailing blocks. Keep codec identity unset until the existing
`AdpcmPsx` interface gap is resolved, matching the current `vag` path.

Register the demuxer through `vaco-component.toml` and regenerate the registry
so listing, probing, extension lookup, and dispatch remain one source of truth.

## Error handling

Reject a missing magic, zero sample rate, zero channels, zero interleave,
interleave values that are not whole 16-byte PS-ADPCM blocks, arithmetic
overflow, and packet sizes above the local allocation bound. Never guess a
layout for malformed geometry.

## Verification

Tests first pin header parsing, one- and two-channel packet geometry, larger
interleave timing, declared-size-versus-EOF behavior, and corrupt short tails.
A checked-in synthetic fixture is compared with `ffprobe` 9.0.1 for sample
rate, channels, duration, packet sizes, positions, timestamps, and durations.
The final verification also opens the fixture through the generated registry
and the real probe CLI.

## Configuration and dependencies

There is no runtime configuration. The implementation uses the crate's
existing `vaco-io`, `vaco-format-core`, `vaco-packet`, `vaco-limits`,
`vaco-chlayout`, and `vaco-codec-core` dependencies.
