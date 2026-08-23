# Frozen-interface gaps found by the container wave

Interfaces are frozen (plan 19 §8): an agent that finds a signature wrong
reports it and does not change it. Five agents have now reported the same
handful of gaps independently, which is the signal that they are real rather
than one agent's misreading.

**These are wave-boundary changes and belong to the orchestrator.** They cannot
land while agents are writing against the current signatures, so they are queued
here with what each one blocks.

## 1. `Muxer` has no metadata channel

Reported by: `vaco-mux-matroska`, and implied by `vaco-mux-mp4`'s brief.

`Tags`, `Chapters`, `Attachments` and `SeekHead` in Matroska, and
`udta`/`meta`/`ilst` plus chapter tracks in MP4, all need per-file and
per-stream metadata at header-write time. `MuxBuilder::add_stream` carries
`CodecParameters`; there is nowhere to put a title, a language, a chapter list
or an attachment.

**Blocks:** FM-23 (#574, MP4 metadata write), the deferred half of FM-25 (#575),
`ffmetadata` in FM-33 (#590) — which is *entirely* metadata — and every
`-metadata` CLI option in CL-16 (#207).

## 2. `Muxer` is single-sink

Reported by: `vaco-mux-matroska` (`webm_chunk`), and structural for
`vaco-mux-image2`, `segment`/`stream_segment` and `tee`.

A muxer writes to one `IoWriter` for its lifetime. Four registered components
need to open a new output partway through (numbered segments, one file per
image, several sinks at once). `webm_chunk` currently exposes a
`chunk_boundaries()` accessor as a workaround, which is honest but is not the
feature.

**Blocks:** FM-35b (#593), the segment family in FM-33 (#590), `tee`, and
`-f segment` from the CLI.

## 3. `Muxer::write_packet` takes packets, and some muxers want frames

Reported by: `vaco-mux-hash`.

`uncodedframecrc` hashes *decoded frames* and prints their geometry — width,
height and pixel format for video; sample format, layout and sample count for
audio. There is no path for that through a packet-shaped trait, so the muxer is
unregistered and #572 stays open on it alone.

**Blocks:** the last registration of FM-20 (#572).

## 4. `DemuxerDesc::open` has no `Limits` or options parameter

Reported independently by two agents in the container wave.

A demuxer cannot see the allocation budget it is supposed to respect, nor the
`-analyzeduration`/`-probesize`-class options that change how it opens. Today
each demuxer either ignores limits or invents its own defaults, which is the
D19 failure mode: the same concept defined once per crate.

**Blocks:** FW-11 (#537, the 40 generic options), and quietly weakens every
fuzz target — a demuxer that cannot see a budget cannot be fuzzed against one.

## 5. `MuxerDesc::open` carries no options

Reported by: `vaco-mux-mp4`, and `vaco-mux-avi` already had it.

The registry constructs a muxer through `MuxerDesc::open`, whose signature has
no room for `-movflags`, `-fflags` or any other per-muxer option. `vaco-mux-mp4`
works around it with a `MovMuxer::with_options` constructor the registry path
cannot reach, which means every fragmented-MP4 option is unreachable from the
CLI even though it is implemented.

This is the muxer-side twin of gap 4. Both are the same shape — a descriptor
that constructs without seeing the options that change how construction should
go — and they should be fixed together.

**Blocks:** every `-movflags` from the CLI, and the CLI half of FM-22 (#573).

## 6. `MuxerDesc` has no `flags` field

Reported by: the CLI muxer wiring.

`DemuxerDesc` carries `flags: FormatFlags`; `MuxerDesc` does not. But the CLI
has to know whether a muxer is `NOFILE` **before** it opens an output URL —
`-f null` must not create a file — and the only way to ask is to construct the
muxer and call `.flags()` on it.

So `exec::open_output` builds every muxer **twice**: once against a throwaway
`MemorySink` to read its flags, then again against the real sink. That works and
is documented, but it is a construction with observable side effects being run
for its return value, which is the kind of thing that stops working quietly the
first time a muxer does something in its constructor.

It is also asymmetric for no reason: the same information is a static field on
the demuxer side.

**Blocks:** nothing outright. It buys a double construction per output and an
explanation in two places.

## Sequencing

1, 4, 5 and 6 are additive and can land together behind default-implemented
trait methods and a new struct field, so existing muxers and demuxers keep
compiling. 2 and 3 are shape changes and want their own wave with nothing else
running.

Two more findings from the same wiring pass, recorded here because they are the
same class — an interface that cannot express what a caller needs:

- **`vaco-sched`'s `MuxWork` never runs the M6 bitstream-filter stage.** It
  drives a raw `dyn Muxer` rather than `MuxWriter`, so `BsfChain`/`BsfProvider`
  are skipped and a stream needing conversion (Annex-B H.264 into MP4) is
  written unfiltered instead of converted or refused. Common-case remuxing is
  unaffected; the restructure is a wave-boundary change.
- **`build_work` read `Muxer::stream_time_base` before calling `init()`** — so
  `vaco-mux-mp4`'s movie-timescale derivation and `vaco-mux-mpegts`'s PCR-PID
  assignment never ran at all. Fixed in place, since the ordering was simply
  wrong rather than inexpressible.
