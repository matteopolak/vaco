# Frozen-interface gaps found by the container wave

Interfaces are frozen (plan 19 §8): an agent that finds a signature wrong
reports it and does not change it. Five agents have now reported the same
handful of gaps independently, which is the signal that they are real rather
than one agent's misreading.

**These are wave-boundary changes and belong to the orchestrator.** They cannot
land while agents are writing against the current signatures, so they are queued
here with what each one blocks.

## 1. `Muxer` has no metadata channel — CLOSED 2026-08-23

`Muxer::set_metadata(&mut self, &metadata::MuxMetadata) -> Result<()>`, a
defaulted trait method (no-op default), plus `metadata::MuxMetadata` (tags,
chapters — reusing `Chapter` verbatim — attachments, per-stream tags) and
`MuxBuilder::with_metadata`/`MuxBuilder::open` calling it once, after `init`
and stream time bases, before the header (M30). No implementor needed an edit;
`cargo check --workspace` confirms it. `vaco-mux-matroska`, `vaco-mux-mp4` and
`vaco-mux-stream`'s `ffmetadata` still need to *override* `set_metadata` to
actually write anything — that is a later wave's work, this one only opens the
channel. See `docs/format/vaco-format-core.md`'s "2026-08-23 wave" section for
the full design and reasoning.

Original report, kept for the *why*:

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

## 4. `DemuxerDesc::open` has no `Limits` or options parameter — SUBSTITUTED, not closed, 2026-08-23

The signature change this entry proposes turned out **not to be additive**:
`open` is a bare `fn` pointer and every one of the ~90 registered demuxers
already supplies its own free function coercing to today's exact two-argument
signature — a function item only coerces to a function-pointer type with a
matching parameter list, so widening it means editing every one of those
functions, not just the descriptor literals naming them. That is the edit this
wave forbids.

Substituted with `Demuxer::reconfigure(&mut self, limits: &Limits, opts:
&FormatOptions) -> Result<()>`, a defaulted trait method callable *after*
`open` returns rather than *during* the call. `Discovery::run` now calls it
before reading anything, so wrapping a demuxer in `Discovery` reaches it; a
fuzz target driving `open` directly can call it too. No implementor needed an
edit. It does **not** fix a demuxer's own hardcoded budget for work `open`
itself does before any `Demuxer` value exists to call this on —
`vacoraw::VacoRawDemuxer::open`'s `Budget::new(Limits::permissive())` is
exactly that case and is unreached by this substitute. Closing that fully still
needs the signature change, in a wave that edits every implementor at once. See
`docs/format/vaco-format-core.md`.

Original report, kept for the *why*:

A demuxer cannot see the allocation budget it is supposed to respect, nor the
`-analyzeduration`/`-probesize`-class options that change how it opens. Today
each demuxer either ignores limits or invents its own defaults, which is the
D19 failure mode: the same concept defined once per crate.

**Blocks:** FW-11 (#537, the 40 generic options), and quietly weakens every
fuzz target — a demuxer that cannot see a budget cannot be fuzzed against one.

## 5. `MuxerDesc::open` carries no options — SUBSTITUTED, not closed, 2026-08-23

Same shape and same finding as gap 4: `MuxerDesc::open` is likewise a bare `fn`
pointer ~90 crates already implement at a fixed one-argument signature, so
widening it is not additive either.

Substituted with `Muxer::set_option(&mut self, name: &str, value: &str) ->
Result<()>`, a defaulted trait method (default: `Err`, "no such option" — an
unrecognised name is refused, not silently dropped, matching `NoBsfs`'s
philosophy). Mirrors `vaco_opts::OptionsExt::set_str`'s name/value-string
contract on purpose, so a caller that already knows how to drive an
`#[derive(Options)]` struct needs no second convention to reach a muxer through
the registry. `MuxBuilder::with_private_options` queues pairs;
`MuxBuilder::open` applies them through `set_option` before `init` runs (M29),
early enough that `-movflags` can still change what `init` decides. No
implementor needed an edit. `vaco-mux-mp4` reaching `MovMuxer::with_options`
through this is still that crate's own work in a later wave. See
`docs/format/vaco-format-core.md`.

Original report, kept for the *why*:

The registry constructs a muxer through `MuxerDesc::open`, whose signature has
no room for `-movflags`, `-fflags` or any other per-muxer option. `vaco-mux-mp4`
works around it with a `MovMuxer::with_options` constructor the registry path
cannot reach, which means every fragmented-MP4 option is unreachable from the
CLI even though it is implemented.

This is the muxer-side twin of gap 4. Both are the same shape — a descriptor
that constructs without seeing the options that change how construction should
go — and they should be fixed together.

**Blocks:** every `-movflags` from the CLI, and the CLI half of FM-22 (#573).

## 6. `MuxerDesc` has no `flags` field — SUBSTITUTED, not closed, 2026-08-23

The proposed fix — add `flags: FormatFlags` to `MuxerDesc`, matching
`DemuxerDesc` — is **not additive on this workspace's pinned toolchain**.
Verified two ways: every one of the ~90 registered `MuxerDesc` constants lists
every current field with no `..base` update syntax, so Rust's struct-literal
exhaustiveness rule requires any new field to be named at every one of those
call sites regardless of its type; and default field values (RFC 3681), which
would remove that requirement, were checked directly against `rustc 1.97.1`
(the pinned version) and remain `error[E0658]`, gated behind an unstable
feature this project does not and should not enable.

Substituted with `MuxerDesc::probe_flags(&self) -> FormatFlags`, a method that
does exactly what `exec::open_output` already did by hand — construct against
a throwaway `vacoraw::MemorySink`, read `.flags()` — except written once here.
No implementor needed an edit. It does not remove `exec::open_output`'s double
construction of a real, non-`NOFILE` output; it removes the *duplicated probing
logic*. The field itself needs a wave that edits every `MuxerDesc` literal at
once — the same wave `DemuxerDesc.flags` must have needed, before any
implementor existed to edit. See `docs/format/vaco-format-core.md`.

Original report, kept for the *why*:

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
