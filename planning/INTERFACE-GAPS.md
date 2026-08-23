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

## 7. `DemuxerDesc::open` receives exactly one `MediaSource`

Reported by: `vaco-subtitle-bitmap`.

VobSub is **two files**: a `.idx` text index and a `.sub` MPEG-PS stream. The
registered path can only be handed one of them, so it parses the `.idx` and
produces correct timing with empty payloads. The real entry point,
`VobSubDemuxer::open_pair(idx, sub)`, is unreachable through the registry.

`image2` has the same shape from the other direction — a sequence of files
rather than a pair — and solved it by owning the file enumeration itself, which
works because the pattern names them. VobSub cannot: the second file's name is a
convention, not something the first file states.

**Blocks:** VobSub payloads, and any future format whose unit of demuxing is a
set rather than a file (MXF OP-Atom is the other one already in the tree).

Same root cause as gaps 4 and 5 — `open` is a bare `fn` pointer with a fixed
signature that ~90 crates already implement — so the additive fix is the same
shape: a defaulted trait method that hands over the extra sources after
construction, not a wider `open`.

## 8. `vaco-sched` drives a raw `dyn Muxer` instead of `MuxWriter` — CLOSED 2026-08-23

**One change did close all three faces** — `PipelineSpec` now holds a
`vaco_format_core::mux::MuxBuilder` per output from `add_output`/
`add_output_with` onward, `PipelineSpec::map` calls `MuxBuilder::add_stream`
instead of `Muxer::add_stream` directly, and `PipelineSpec::build` consumes
the builder with one call to `MuxBuilder::open`, handing the resulting
`MuxWriter` to a `node::MuxWork` that shrank to `Option<MuxWriter>` plus the
per-port bookkeeping `write_packet`/`end_stream`/`finish` need. No `#[cfg]`,
no dual code path: `vaco-sched`'s own M8–M11 re-derivation (`header_written`/
`trailer_written` bools, a hand-rolled `InterleaveQueue`/`MuxTimestamps` pair)
is gone, not bypassed.

Two new `PipelineSpec` methods carry what the wrapper needs that the old
`add_output_with(muxer, flags, options)` signature had nowhere to put:
`set_output_metadata(output, metadata)` (M30) and `set_output_bsfs(output,
bsfs)` (M6's `BsfProvider`). `add_output_with` itself lost its `flags`
parameter — `MuxBuilder::new` reads `Muxer::flags()` from the muxer directly,
so a parameter that could only ever legally equal `muxer.flags()` was pure
redundancy.

What each face actually got, now that it has been measured rather than
assumed:

- **Ordering (8a) — fully closed.** `set_metadata` now runs where
  `Muxer::set_metadata`'s own doc comment always said it would: after every
  stream is declared and its time base settled, before the header. The CLI's
  `set_output_metadata` call can happen before or after `PipelineSpec::map`
  now, because `MuxBuilder::open` is what fixes the order the muxer actually
  sees, not the caller's call order at this layer. `vaco-mux-mp4`'s and
  `vaco-mux-matroska`'s lazy per-stream resolution — deferred to
  `write_header` time specifically so it survives either call order — is no
  longer load-bearing for this path, but neither crate needed to change:
  their `set_metadata` overrides just store the metadata and resolve it
  later regardless of when they were called, so they are correct under the
  new ordering by construction, and their `set_metadata_before_add_stream_
  still_resolves_*` regression tests pass unmodified.
- **M15 (`query_codec`) — mechanically closed, practically narrower than the
  writeup below implied.** `PipelineSpec::map` now asks before `Muxer::add_stream`
  runs, closing the exact bypass described. But investigating the six
  known-incompatible pairs this gap's writeup pointed at
  (`planning/CONFORMANCE-FINDINGS.md` finding 19) found that `query_codec`
  was never the mechanism those six needed: `CodecSupport` answers "can this
  container ever hold this codec" (H.264-in-AVI is `Supported`), and the six
  pairs fail on narrower stream-content constraints — a packet with no PTS at
  all, ADTS-framed AAC where the container needs raw `AudioSpecificConfig`
  framing — that live in `vaco-mux-avi`'s and `vaco-mux-mpegts`'s own
  `add_stream`, reachable through the *old* direct `Muxer::add_stream` call
  just as much as the new `MuxBuilder` one. Measured directly: MPEG-TS→AVI
  with ADTS AAC refuses identically before and after this change
  (`unsupported: avi: ADTS-framed AAC has no AVI representation…`, exit 218 —
  the reference's own message is "ADTS is only supported with codec tag
  0x1610", exit 234; same refusal, different wording/code, a separate and
  smaller gap). `query_codec` genuinely closing a real bypass and finding 19's
  six pairs turning on a different mechanism are both true; the original
  writeup below conflated them.
- **M6 (bitstream filters) — the stage runs; nothing feeds it yet.** Every
  packet `MuxWriter::write_packet` handles now passes through
  `check_bitstream`/`BsfChain` before `Muxer::write_packet` sees it, and
  `PipelineSpec::set_output_bsfs` is the seam for a real `BsfProvider`. But no
  caller supplies one: there is no `vaco-bsf-*` crate in this workspace, and
  neither `vaco-mux-avi` nor `vaco-mux-mpegts` — the two the original writeup
  named — implements `check_bitstream` at all, so both still run their own
  inline length-prefix-to-Annex-B conversion and both still get
  `BitstreamAction::Keep` (the trait default) from M6 regardless of which
  path drives them. **Their inline conversions are not dead code from this
  change.** Closing that needs three things done together, deliberately not
  done in this pass so a bisect stays possible if remuxing breaks: a
  bitstream-filter crate, a `BsfProvider` (the mux-side mirror of
  `vaco-registry`'s `ParserProvider`, which does not exist for
  `Kind::BitstreamFilter` yet either — `vaco-codec-core` has no
  `BitstreamFilterDesc`, per `vaco_registry::Kind::has_table`'s own doc
  comment), and those two crates' `check_bitstream` asking for it.

See `docs/app/vaco-sched.md` and `docs/app/vaco-cli.md` (#10) for the
implementation detail and the measurements above.

Original report, kept for the *why* — including why it took three reports to
notice it was one gap:

**This is one gap wearing three faces**, and each was reported separately before
anyone noticed they were the same thing:

- **No metadata ordering.** The CLI calls `set_metadata` before the muxer has
  any streams, because `MuxWork` never goes through `MuxBuilder::open`. MP4 and
  Matroska both had to make per-stream resolution lazy to survive it.
- **No bitstream-filter stage.** `BsfChain`/`BsfProvider` exist on `MuxWriter`
  and never run, so a stream needing Annex-B conversion is written unfiltered.
  `vaco-mux-avi` and `vaco-mux-mpegts` each carry their own conversion inside
  the muxer as a result — two copies of something the framework has a slot for.
- **No codec-compatibility check.** `MuxBuilder::add_stream` calls
  `query_codec`; `PipelineSpec::map` calls `muxer.add_stream()` directly on the
  raw trait object, so the support lists are simply never asked. This one is
  worth stating plainly: **the check exists, is tested, and is bypassed.**

Fixing it is one change — `MuxWork` builds and drives a `MuxWriter` — and it
closes all three. Every workaround above can then be deleted, which is the
argument for doing it rather than adding a fourth.

### 8a. Historical note: `set_metadata` ordering

Reported by: the CLI metadata wiring, immediately after gap 1 was closed to
enable it.

`Muxer::set_metadata` exists and `MuxBuilder::open` calls it at the right point.
But `vaco-sched`'s `MuxWork` drives a raw `dyn Muxer` rather than `MuxWriter`
(the same reason it skips the bitstream-filter stage), so the CLI calls
`set_metadata` **before** the muxer has any streams.

Both MP4 and Matroska had to make per-stream metadata resolution **lazy** —
resolved at `write_header` time by re-reading the stored metadata, rather than
eagerly mutating tracks inside `set_metadata` — because the eager version
silently dropped everything. Both crates carry a regression test for
`set_metadata` before `add_stream`.

Lazy resolution is defensible on its own terms, so this is not urgent. But it is
a constraint every future muxer inherits without being told, and the honest fix
is the same one gap 2 needs: `MuxWork` driving `MuxWriter` instead of a bare
`dyn Muxer`.

**Fixed 2026-08-23** as part of closing gap 8 above — see that entry for what
changed and why the lazy resolution in both crates turned out not to need
touching.

## 9. `Muxer::add_stream` takes only `CodecParameters`

Reported by: the same pass. `-disposition` and `-program` parse correctly and
have nowhere to go — a stream's disposition flags and its program membership are
neither codec parameters nor file-level metadata.

Same class as gap 1, and the reason #207 (CL-16) stayed open after tags and
chapters worked.

## 10. `vaco-filter-core` has no adapter for a multi-input or multi-output filter

Reported by three filter agents independently, on 2026-08-23, each reaching the
same conclusion from a different crate: `framepack`, `mergeplanes`,
`alphamerge`, `maskedmerge`, `tlut2` and friends take two inputs, and
`extractplanes` produces a *dynamic number* of output pads. `adapt.rs` offers
`Simple` (one in, one out), `Sourced` (none in, one out) and `Blocked` (audio,
one in, one out), and nothing else — so each agent recorded the filter as
blocked and moved on.

**It is not blocked, and this entry exists so the fourth agent does not reach
the same wrong conclusion.** The capability is already there: `Filter::activate`
returning `Activity` is the general form, and `vaco-filter-video-composite`'s
`overlay` is a working witness — two inputs, two independent timelines, driven
by `vaco-filter-framesync`, with the `eof_action`/`shortest`/`repeatlast`
surface that goes with it. What is missing is the *convenience*: there is no
`Simple`-shaped adapter that hands a filter two aligned frames and takes one
back, so every multi-input filter re-derives the same `activate` loop.

So this is a different class from gaps 2, 3, 7 and 9. Those are interfaces that
*cannot express* what a caller needs. This one expresses it fine and makes every
author write the same forty lines — which is how the seventh copy ends up
subtly different from the first, the failure mode D19 exists to prevent.

Additive, and worth doing before the next filter wave: a `Paired<F>` adapter
over `framesync` for the two-input case, and a `Fanout<F>` for the
dynamic-output-pad case that `extractplanes` needs. Neither changes `Filter`.

The multi-input filters already declined for this reason, so they can be picked
up in one pass once the adapter exists: `framepack`, `mergeplanes`,
`alphamerge` and `extractplanes` (`vaco-filter-geometry`), and whatever the
in-flight `vaco-filter-key` and `vaco-filter-temporal` crates report.

## Sequencing

1, 4, 5 and 6 are additive and can land together behind default-implemented
trait methods and a new struct field, so existing muxers and demuxers keep
compiling. 2 and 3 are shape changes and want their own wave with nothing else
running.

Two more findings from the same wiring pass, recorded here because they are the
same class — an interface that cannot express what a caller needs:

- **`vaco-sched`'s `MuxWork` never ran the M6 bitstream-filter stage — fixed
  2026-08-23 as part of gap 8.** It drove a raw `dyn Muxer` rather than
  `MuxWriter`, so `BsfChain`/`BsfProvider` were skipped and a stream needing
  conversion was written unfiltered instead of converted or refused. `MuxWork`
  now drives a `MuxWriter`, so the stage runs — but no `BsfProvider` in this
  workspace supplies a real filter yet (there is no `vaco-bsf-*` crate), and
  `vaco-mux-avi`/`vaco-mux-mpegts`, the two containers with an inline
  conversion of their own, do not implement `check_bitstream` to ask M6 for
  one. See gap 8's entry above for what closing that the rest of the way
  needs.
- **`build_work` read `Muxer::stream_time_base` before calling `init()`** — so
  `vaco-mux-mp4`'s movie-timescale derivation and `vaco-mux-mpegts`'s PCR-PID
  assignment never ran at all. Fixed in place, since the ordering was simply
  wrong rather than inexpressible.
