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

## 2. `Muxer` is single-sink — SUBSTITUTED for `image2`, not closed, 2026-08-28

Reported by: `vaco-mux-matroska` (`webm_chunk`), and structural for
`vaco-mux-image2`, `segment`/`stream_segment` and `tee`.

A muxer writes to one `IoWriter` for its lifetime. Four registered components
need to open a new output partway through (numbered segments, one file per
image, several sinks at once). `webm_chunk` currently exposes a
`chunk_boundaries()` accessor as a workaround, which is honest but is not the
feature.

Substituted with `Muxer::bind_url(&mut self, url: &str) -> Result<()>`, a
defaulted trait method (default: `Unsupported`) a caller invokes once, right
after `open` returns, with the destination URL a `MuxerDesc::open`-shaped
signature has nowhere to carry (same root cause as gaps 5/9: `open` is a bare
`fn` pointer ~90 registered muxers already implement at a fixed
`Box<dyn MediaSink>` signature). A muxer that opts in typically replaces its
own state outright (`*self = Self::for_pattern(url, ..)?`). No implementor
needed an edit. `impl Muxer for Box<M>` and `vaco-cli`'s `TallyingMuxer` both
forward it explicitly, the same trap gap 9's `add_stream_with` found.

**What this actually closes: `image2` only, verified end to end (#649).**
`vaco-mux-image2`'s registry entry now starts as the old degenerate
one-sink shape and becomes the real `Image2MuxWriter`-backed writer on the
first `bind_url` call; `Muxer::flags()` reports the (previously unused)
`FormatFlags::NEEDNUMBER` so `vaco-cli`'s `open_output` knows to keep a
throwaway-sink-backed instance and bind it rather than ever opening a real
destination for the literal pattern string. `-f image2 -i in_%03d.png -c
copy -f image2 out_%03d.png` now produces `cmp`-identical output to the
input sequence, and `ffmpeg -f rawvideo -pix_fmt rgb24 - | md5` matches on
both sides.

**Still open:** `segment`/`stream_segment`, HLS/DASH's segmenting muxers,
`webm_chunk` and `tee` do not implement `bind_url` and are not registered
as needing it — closing them is unaffected by this substitute except that
the seam now exists for whoever picks them up. `tee` in particular may not
need this method at all: fanning out to several *independently configured*
sinks reads more like a wrapper `Muxer` holding several `Box<dyn Muxer>`
than like one muxer rebinding to one new destination.

See `docs/format/vaco-format-core.md`'s "gaps 2 and 7" section.

**Blocks:** FM-35b (#593) — closed for `image2`; the segment family in
FM-33 (#590), `tee`, and `-f segment` from the CLI remain blocked.

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

## 7. `DemuxerDesc::open` receives exactly one `MediaSource` — SUBSTITUTED for `image2`, not closed, 2026-08-28

Reported by: `vaco-subtitle-bitmap`.

VobSub is **two files**: a `.idx` text index and a `.sub` MPEG-PS stream. The
registered path can only be handed one of them, so it parses the `.idx` and
produces correct timing with empty payloads. The real entry point,
`VobSubDemuxer::open_pair(idx, sub)`, is unreachable through the registry.

`image2` has the same shape from the other direction — a sequence of files
rather than a pair — and solved it by owning the file enumeration itself, which
works because the pattern names them. VobSub cannot: the second file's name is a
convention relative to the first, and the first `MediaSource` has no `path()`
to read its own filename back from, so nothing downstream of the protocol
layer can construct the sidecar name at all.

Substituted with `Demuxer::bind_url(&mut self, url: &str) -> Result<()>`, a
defaulted trait method (default: `Unsupported`) a caller invokes once, right
after `open` returns and before reading anything, with the URL the caller
already resolved to reach that descriptor — the fix needs the *string*, not
a second opened `MediaSource`, so no `MediaSource::path()` accessor and no
change to any caller's I/O layer was needed either. Same root cause as gaps
4/5 — `open` is a bare `fn` pointer ~90 crates already implement at a fixed
signature. No implementor needed an edit; `impl Demuxer for Box<D>` forwards
the new method explicitly, the same trap gap 9 found on the mux side.

**What this actually closes: `image2` only, verified end to end (#649).**
`DEMUXER_IMAGE2.flags` gained `FormatFlags::NEEDNUMBER` (declared since this
crate's foundations wave, never read by anything until now) as the signal
`vaco-cli`'s `input::open` checks *before* attempting to open a `%d` pattern
as a literal file — when set, it hands `open` an empty placeholder and calls
`bind_url` directly instead. `vaco-demux-image2`'s registry entry starts in
the old degenerate one-source shape and becomes the real, already-correct
`Image2Demuxer::open_pattern` on that call. `-f image2 -i img_%03d.png -c
copy -f rawvideo out.bin` now matches `cat img_001.png img_002.png …`
byte-for-byte, where it previously failed outright with "No such file or
directory".

**Still open:** `vaco-subtitle-bitmap`'s `VobSubDemuxer` does not implement
`bind_url` — this substitute makes it reachable (a normally-opened `.idx`
gets a best-effort `bind_url(url)` call from `input::open` today, ignoring
only `Unsupported`) but the sidecar-deriving override itself is unwritten.
MXF OP-Atom, the gap's other named future case, is unaffected either way.

See `docs/format/vaco-format-core.md`'s "gaps 2 and 7" section.

**Blocks:** VobSub payloads — the seam now exists, the override does not;
and any future format whose unit of demuxing is a set rather than a file
(MXF OP-Atom is the other one already in the tree).

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

## 9. `Muxer::add_stream` takes only `CodecParameters` — PARTIALLY CLOSED 2026-08-27

**The `framecrc`-corrupting half (below) is closed; `-disposition`/`-program`
are not** — see *Status* at the end of this entry. Original report kept in
full, since the *why* still explains the shape `StreamSpec` grew into.

Reported by: the same pass. `-disposition` and `-program` parse correctly and
have nowhere to go — a stream's disposition flags and its program membership are
neither codec parameters nor file-level metadata.

Same class as gap 1, and the reason #207 (CL-16) stayed open after tags and
chapters worked.

**Raised in priority 2026-08-27: it also corrupts `framecrc`.** See
`CONFORMANCE-FINDINGS.md` 32. `vaco-mux-hash` prints a `#tb` line and rescales
packet durations, so it needs a definite time base; `add_stream` gives it only
`CodecParameters`, so it recomputes `1/frame_rate`. That is right for freshly
encoded raw media and wrong for stream copy, where the reference keeps the
input's base — `1/12800` for an MP4 and `1/90000` for a TS, against our `1/25`
and `1/50`. Every timestamp column of every `framecrc` line is therefore wrong,
in one of the ten comparison modes the conformance suite runs, while the
checksums themselves are correct.

So this gap is no longer just interface tidiness. The additive shape still
works — a default-implemented `add_stream_with(&mut self, params, spec)` that
forwards to `add_stream`, so none of the ~57 implementors change — but `spec`
now needs to carry the stream's **time base** as well as its disposition and
program membership. Remember that `impl<M: Muxer + ?Sized> Muxer for Box<M>`
must forward the new method too, or a boxed muxer silently takes the default.

### Status

**Time base: closed (issue #634).** `StreamSpec { time_base: Option<Rational> }`
landed exactly as sketched above, with `Muxer::add_stream_with` defaulting to
`add_stream`. `vaco-mux-hash`'s `FrameHashMuxer` is the first (and, as of this
close, only) override — `#tb` now matches the reference byte-for-byte on both
an MP4 and an MPEG-TS fixture, `-bitexact` and not; see
`CONFORMANCE-FINDINGS.md` 32's own *Status* note and
`docs/format/vaco-format-core.md`'s "The 2026-08-27 addition" for the wiring,
including the `Box<dyn Muxer>` **and** `vaco-cli`'s `TallyingMuxer` forwarding
— the trap named above turned out to have a second instance nobody had
written down: any wrapper holding a `Box<dyn Muxer>`, not just the blanket
impl itself, has to forward a new method explicitly or it silently reverts
to the default.

**`-disposition`/`-program`: still open**, deliberately not touched by this
close. `StreamSpec` stays at one field — adding the other two now would be
guessing at a shape nobody has measured a caller for yet (D19), the same
restraint gap 9's own text already argued for. The next agent that wires
`-disposition` or `-program` through a real muxer should extend `StreamSpec`
rather than invent a second channel.

**A `Muxer::set_bitexact` sibling landed alongside this**, same file, same
shape, for an unrelated fact (`vaco-mux-hash`'s `#software` line needed to
know `-bitexact`, and had no channel either) — not itself part of gap 9, but
recorded here because it used the identical "default no-op, wired through
`MuxBuilder::open`, forwarded by `Box<dyn Muxer>` and `TallyingMuxer`" shape,
and a future gap of the same kind should reach for that shape first rather
than re-deriving it.

## 10. `vaco-filter-core` has no adapter for a multi-input or multi-output filter — CLOSED 2026-08-23

Two adapters landed in `vaco-filter-core::adapt`, additive, no change to
`Filter`: **`Paired<F: PairedFilter>`** (N inputs, one output, strict
lockstep — one frame from every input per call, or the whole filter ends
the instant any one input runs dry) and **`Fanout<F: FanoutFilter>`** (one
input, N outputs fixed at construction, one input frame in → exactly N
derived frames out, generalising `vaco-filter-plumbing`'s `split`/`asplit`
from N *clones* to N *different* frames). `cargo test -p vaco-filter-core`
covers both end-to-end against real `Graph`s, including `Paired`
generalised to three inputs and a defensive test that a `Fanout` filter
lying about its own output count surfaces as an error rather than silently
misdirecting a pad.

**`Paired` turned out not to be "`Simple` for two frames, over `framesync`"
— it is a materially different, simpler contract, and that difference is
measured, not assumed.** `ffmpeg -h filter=framepack` and `=mergeplanes`
carry no `eof_action`/`shortest`/`repeatlast`/`ts_sync_mode` section at all,
unlike `alphamerge`'s, which has one verbatim identical to `overlay`'s.
Feeding `framepack` a 10-frame and a 5-frame input at the same rate
produces exactly 5 outputs, not 10 with the shorter input's last frame
repeated (`eof_action=repeat`, the framesync default) — and `framepack`
refuses two inputs with different time bases outright rather than
reconciling them. So `vaco-filter-core` still cannot depend on
`vaco-filter-framesync` (layering: framesync depends on core, not the
reverse, and `layer-check` would refuse it regardless) **and does not need
to**: `Paired` is the honest, separate shape those filters actually have,
not framesync with a dependency problem.

One consequence found only by trying: **`alphamerge` needs neither new
adapter.** Measured, it carries the full framesync option surface, so it
is registered on top of `vaco-filter-framesync`'s existing `Synced` —
exactly what `vaco-filter-video-composite`'s `overlay` already does. Three
data points (`overlay`, `alphamerge`, and `framepack`/`mergeplanes`'s
absence of the surface) now confirm the dividing line is the measured
option surface, not which crate a filter happens to be assigned to.

`Paired` also generalises past exactly two inputs — `mergeplanes`'s input
count is fixed at construction from its own `map<N>s` options (1 to 4) —
rather than adding a third, near-duplicate adapter for "N-in-1-out,
lockstep" once a real filter needed N > 2 (D19).

`overlay` was evaluated for a port onto `Paired`, to see whether the new
adapter could subsume the witness it generalises from, and was **not**
ported: `Paired` cannot express `overlay`'s default `eof_action=repeat` at
all (every existing `overlay` test exercises the default) and has no
timestamp-based event selection for the differing input frame rates
`overlay` routinely runs at. `cargo test -p vaco-filter-video-composite`
was run before and after this wave with zero edits to that crate; its 43
tests pass unmodified. Leaving a working witness alone rather than forcing
a port that would have changed its tested behaviour was the right call,
not a shortfall — see `docs/filter/vaco-filter-core.md`'s "`overlay` was
not ported onto `Paired`" section for the detail.

`framepack`, `mergeplanes`, `alphamerge` and `extractplanes` are now
registered in `vaco-filter-geometry` (18 filters total in that crate). The
"whatever the in-flight `vaco-filter-key` and `vaco-filter-temporal`
crates report" half of the original note below was not followed up in this
pass: at the time, `planning/ASSIGNMENTS.md` showed `vaco-filter-color`,
`vaco-filter-key`, `vaco-filter-lut` and `vaco-filter-temporal` all
`assigned` with no finished date, and each had uncommitted work already on
disk, so they were left untouched per the do-not-race rule rather than
guessed at from outside. See `docs/filter/vaco-filter-core.md` and
`docs/filter/vaco-filter-geometry.md` for the full detail.

Original report, kept for the *why*:

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

## 11. `vaco_frame::Frame` has no per-frame metadata dictionary — CLOSED 2026-08-23

`Frame::metadata() -> &[(String, String)]`, `Frame::set_metadata(key, value)`
and `Frame::metadata_get(key) -> Option<&str>`, backed by a new
`FrameSideData::Metadata(FrameMetadata)` variant (`FrameMetadata` an
insertion-ordered `Vec<(String, String)>` newtype in `vaco-frame`). Additive,
not a new field: `FrameSideData` is already `#[non_exhaustive]`, so the
variant costs nothing anywhere else, and `rg 'side_data:' --type rust` found
only two literal `Frame { .. }` constructions outside `vaco-frame` itself
(`vaco-codec-core`'s and `vaco-sched`'s test/bench scaffolding) — small, but
both sit in crates outside this work's ownership in a six-agent tree, so the
field route would have needed cross-crate coordination this wave didn't have.
The dedicated-field shape is recorded in `docs/model/vaco-frame.md` as the
eventual promotion, once those two sites can move in the same wave as other
`Frame` field changes.

`freezedetect` (`vaco-filter-temporal`) is the wired acceptance case:
`lavfi.freezedetect.freeze_start`/`.freeze_duration`/`.freeze_end`, attached
to the confirming frame and the run-breaking frame respectively, replacing
the `pub(crate)` test-only `Filter::events()` workaround its author had
flagged. Measured against `ffmpeg 8.1`, not guessed, and one real bug found
in the process: `freeze_end`/`freeze_duration` use the timestamp of the frame
that *breaks* the freeze, not the last frozen frame — the two are
indistinguishable at a uniform frame rate, so the original implementation
(before this closed) had it wrong the same way `tblend`'s 256-vs-255 divisor
was wrong, and an irregular-timestamp test is what catches it. Value
formatting is six decimal digits, trailing zeros trimmed, then a bare
trailing `.` trimmed (`0.0` → `"0"`, `1.001000` → `"1.001"`, `1.000001`
unchanged) — see `freezedetect`'s module doc for the full measurement.

On the consuming side, `Frame::metadata()`'s `&[(String, String)]` return
type is deliberately the shape `vaco-probe`'s existing `show::tags` already
renders for `STREAM_TAGS`/`FORMAT_TAGS`/`CHAPTER_TAGS`/`PROGRAM_TAGS`, so no
new renderer was needed — `crates/app/vaco-probe/tests/frame_tags.rs` proves
`show::tags(&mut e, SectionId::FRAME_TAGS, frame.metadata())` reproduces the
reference's `[FRAME_TAGS]`/`"tags"` block byte for byte, including that a
frame with nothing to report opens no section at all.

**What did not ship: `-show_frames` itself is still refused.** Measured while
scoping this, not assumed: `vaco-probe` has zero decoders anywhere in the
workspace (D5) and doesn't even depend on `vaco-filter-core`/
`vaco-filter-graph`/`vaco-sched` (`FRAMES_UNSUPPORTED` in `src/lib.rs`, moved
to v0.2 by D14.4). There is no code path in this workspace today that
produces a `Frame` from real input inside `vaco-probe`, so a live `vaco-probe
-show_frames` run on a `freezedetect` graph is not reachable yet regardless
of this gap — that is a decode/filter-graph wiring gap, not a metadata-model
gap, and belongs to whichever wave adds `-show_frames` for real. This entry
closes what it named: `Frame` has the dictionary, `freezedetect` writes it,
and the renderer reproduces it exactly once handed a `Frame`.

Original report, kept for the *why*:

Reported by the `vaco-filter-color`/`-key`/`-lut` agent on 2026-08-23, which hit
it while trying to place `msad` and `bitplanenoise` and found they had nowhere
to put their answer.

`Frame` carries `side_data: SmallVec<[FrameSideData; 2]>`, and `FrameSideData`
is a closed enum of *typed* variants — display matrix, closed captions,
mastering display, content light level, cropping. There is no string-keyed
dictionary. The reference's equivalent is `AVFrame::metadata`, and a whole
family of filters exists to write into it:

```sh
$ ffprobe -of json -show_frames -f lavfi -i "movie=s.nut,signalstats"
"tags": {
    "lavfi.signalstats.YMIN":  "22",
    "lavfi.signalstats.YAVG":  "59.6797",
    "lavfi.signalstats.YMAX":  "210",
    …
}
```

Measured above. The `lavfi.<filter>.<key>` convention is not decorative — it is
the only output channel those filters have.

### What it blocks

Three whole rows of plan 16 §4.2–4.3, not a filter or two:

* `vaco-filter-analysis` — `psnr`, `ssim`, `ssim360`, `xpsnr`, `vif`,
  `vmafmotion`, `msad`, `identity`, `blackdetect`, `blockdetect`,
  `bitplanenoise`, `entropy`, `siti`, `signalstats`, `scdet`, `bbox`,
  `blackframe`, `cropdetect`, `freezedetect`. Every one of them *is* a
  measurement whose result goes nowhere else.
* `vaco-filter-aanalysis` — the audio half of the same thing.
* `vaco-filter-mm` — `metadata` and `ametadata` exist to **read and filter on**
  these keys, and `select`/`aselect`'s expression language exposes a
  `metadata()` function over them.

And on the consuming side, `vaco-probe`'s `-show_frames` has `FRAME_TAGS`
plumbing for stream/format/chapter/program tags but nothing frame-level to
fill it from.

### Shape

Additive: one more field on `Frame`, or one more `FrameSideData` variant
holding a small ordered map. Ordered, not hashed — the reference prints these
in insertion order and `-show_frames` output is compared byte for byte.

Worth settling **before** `vaco-filter-analysis` is dispatched, because
otherwise its author will do what three agents already did with gap 10:
discover the limitation, write it up, and leave the filters undone.
Twenty-plus filters is too many to rediscover it with.

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
- **Gap 8's own bullet above, updated 2026-08-23 (#349/#350):** a
  `vaco-bsf-*` crate now exists — `vaco-bsf-core` (the `BitstreamFilter`
  driver), `vaco-bsf-generic` (`null`, `extract_extradata`, `noise`,
  `remove_extra`, `setts`, `chomp`, `dump_extra`, `filter_units`,
  `trace_headers`) and `vaco-bsf-h2645` (`h264_mp4toannexb`,
  `hevc_mp4toannexb`) — and `vaco-registry::Bsfs` implements `BsfProvider`
  over all of them. `vaco-mux-avi` and `vaco-mux-mpegts` now implement
  `check_bitstream`, asking for `h264_mp4toannexb`/`hevc_mp4toannexb` under
  the same condition their own inline `maybe_convert` already used; that
  inline conversion stays as the direct-`Muxer` path's behaviour (unchanged,
  still framing-only) and is now also a safe no-op on a payload M6 already
  converted (`starts_with_annexb_start_code`), rather than being deleted —
  measured to diverge from the M6 path (missing SPS/PPS splicing) rather than
  agreeing with it, so deleting it was not this wave's call to make. Still
  open: VVC (no `vvc_mp4toannexb` — `vaco-mux-mpegts`'s VVC support keeps its
  framing-only conversion permanently, not just until a filter lands);
  `h264_redundant_pps` (see `vaco-bsf-h2645`'s own docs for why — a
  CABAC-safe bit-precise rewrite this workspace has no writer layer for); and
  `vaco-mux-mp4`'s fragmented mode, where the init segment's `stsd` is written
  before any packet is seen, so `extract_extradata`'s result arrives too late
  to help it the way it now helps progressive mode's deferred write.

## 12. `BsfProvider::open` carries no per-instance option string — PARTIALLY CLOSED 2026-08-28

Reported by #349/#350 while implementing `vaco-bsf-generic`/`vaco-bsf-h2645`.

`vaco_format_core::mux::BsfProvider::open(&self, name: &str, params:
&CodecParameters) -> Result<Box<dyn BitstreamFilter>>` has exactly two
parameters, neither of which can carry `-bsf:v filtername=opt=value`'s
right-hand side. Several real filters are close to meaningless without one:
`setts` (its whole point is the expression), `noise` (`amount`/`dropamount`),
`filter_units` (`pass_types`/`remove_types`/`discard`), `remove_extra`/
`dump_extra`'s `freq=all`. Every one of them is implemented here as the
reference's bare-name (all-default) behaviour, which for `setts` and
`filter_units` is the identity transform — correct, and also the least
interesting thing either filter does.

This is the same shape as gaps 4/5: a construction-time entry point with
nowhere to put per-instance configuration. Worked around here by not routing
any: `vaco-bsf-generic`'s `noise` in particular defaults to identity rather
than the reference's own nondeterministic bare-name corruption (see that
module's doc comment — there is no reference answer to converge on there
anyway, seeded or not).

### Shape

Likely additive, mirroring gap 5's `Muxer::set_option(&mut self, name: &str,
value: &str) -> Result<()>`: a `BsfDesc`/`BitstreamFilter` could grow the same
method, called after `open` and before the first packet. Not attempted here —
picking the shape is a design decision for whoever wires a real `-bsf:v`
option string through the CLI, which is a separate, larger piece of work
(`FormatOptions`-style parsing) this wave did not touch.

**Blocks:** any CLI spelling of `-bsf:v <name>=<options>` for every filter
`vaco-bsf-generic`/`vaco-bsf-h2645` register; not blocking for the three
containers already wired (M16's `extract_extradata` and the two
`*_mp4toannexb` filters need no options to do their real job).

### Still open, 2026-08-27 (#353/#354)

Confirms the shape above rather than changing it: `h264_metadata`,
`hevc_metadata` (`vaco-bsf-h2645`), `mpeg2_metadata`, `prores_metadata`
(`vaco-bsf-legacy`) all hit this same wall — every option each exposes
defaults to "leave the bitstream alone" (`ffmpeg -h bsf=<name>`), and this
gap means none of those defaults can ever be overridden. Measured directly
against `ffmpeg 8.1` (five adversarial inputs per H.264/HEVC filter; one
`cmp` and one `framemd5` comparison for the MPEG-2/ProRes pair) that the
bare-name behaviour these four are stuck with is also the byte-identical
one, so they were registered anyway rather than left out — same call as
`av1_metadata`/`vp9_metadata`/`opus_metadata` before them.

Not closed by #353/#354's owner: the fix is a change to
`vaco_codec_core::BitstreamFilter` (a defaulted `set_option(&mut self, name:
&str, value: &str) -> Result<()>`, mirroring gap 5's `Muxer::set_option`
substitution the same day this gap was first opened), and `vaco-codec-core`
is outside that owner's single-writer scope. Recorded here again, with the
concrete shape, for whoever next owns that crate — it is now blocking four
more filters than it was when first reported, and `vvc_metadata`'s single
`aud`-only option (which would otherwise be the easiest case yet) could not
even be checked, for want of a VVC sample in that pass's environment rather
than for want of this gap.


### Status, 2026-08-28

**`BitstreamFilter::set_option` landed in `vaco-codec-core`**, exactly the
shape recorded above: `fn set_option(&mut self, name: &str, value: &str)
-> Result<()>`, defaulted to `Err(Error::Option(..))`, mirroring gap 5's
`Muxer::set_option`. `PacketMap::set_option` (`vaco-bsf-core`) forwards to
it with the same default, and `MappedFilter<T>`'s `BitstreamFilter::set_option`
forwards explicitly to `T::set_option` rather than inheriting the trait
default — the wrapper trap this entry's own history already names one
layer up (gap 9's `Box<dyn Muxer>`), caught here before shipping by a test
that drives a real option only through the `BitstreamFilter` face and
checks a deliberately-wrong value never reaches the output.

**One real option is wired**: `h264_metadata`'s `aud` (`pass`/`insert`/
`remove`). Measured against `ffmpeg 8.1`: `insert` unconditionally prepends
a 4-byte-start-coded AUD to every access unit (no "already present" check —
inserting on an already-AUD'd stream produces two), `remove` strips every
AUD unit byte-for-byte, and the inserted `primary_pic_type` is the
per-access-unit ITU-T H.264 Table 7-5 union of the slice kinds actually
coded (`0`/`1`/`2` for I-only/I+P/I+P+B), probed with three GOP structures
and confirmed never a constant across a file. `aud` is a structural
byte-level edit rather than a field inside an existing SPS/VUI, which is
what let it ship without the CBS write path the rest of this gap's option
surface still needs.

**Still open**: the other nineteen `h264_metadata` options, `hevc_metadata`'s
whole surface (its own `aud` was not ported in this pass — HEVC's AUD NAL
header is two bytes, not H.264's one, and porting without checking that
layout would be guessing), and `mpeg2_metadata`/`prores_metadata`/
`vvc_metadata`. Every one of them rewrites a field *inside* an existing
parameter set, which needs a bit-exact CBS write path this workspace still
does not have (see `vaco-bsf-h2645`'s and `vaco-bsf-legacy`'s own docs).
No CLI-side `-bsf:v name=opts` parser exists either — reaching `set_option`
from a graph-syntax string is still a separate, larger piece of work this
pass did not touch, so `aud` is reachable from Rust callers (and this
crate's own fuzz target, extended to drive `insert`/`remove` over
attacker-controlled bytes: 2.1M execs/31s, zero crashes) but not yet from
`vaco-cli`.

## 13. `vaco_frame::FrameSideData` has no console-log-only output channel — CLOSED 2026-08-28

Reported by (confirmed, not newly found) the `vaco-filter-analysis` agent
finishing plan 16 §4.2's row on 2026-08-23, re-checking a claim its own
crate-root doc already made.

`showinfo` measures (`ffprobe -show_frames` through it, `ffmpeg 8.1`) to
write **zero** frame metadata: its entire output is a console `info`-level
log line per frame (`n:… pts:… fmt:… sar:… ...`). This workspace's filter
model has exactly one output channel for a measurement filter — gap 11's
`Frame::metadata()` dictionary — and that dictionary is specifically the
`lavfi.<filter>.<key>` tag convention; there is nowhere to put an
unstructured, human-readable log line, and stuffing one into the metadata
dictionary under a made-up key would not reproduce anything the reference
actually exports (nothing does, since it never touches `AVFrame::metadata`
for this filter).

### Shape

Additive in principle — a `Vec<String>` (or similar) side channel a filter
could push lines into, and something on the `vaco-probe`/CLI side to render
it the way `-loglevel info` does — but this is a genuinely different feature
from gap 11 (frame-scoped structured tags), not a variant of it. Nothing in
this wave needed it enough to design it; recorded so the next agent handed
`showinfo` does not re-measure the same "no metadata at all" finding from
scratch.

**Blocks:** `showinfo` (plan 16 §4.2, `vaco-filter-analysis`'s row) and any
other reference filter whose only documented effect is a log line rather
than a metadata write or a pixel change.


### Status, 2026-08-28

`FrameSideData::Log(Vec<String>)` landed, exactly the shape sketched above:
no keys, no structure, a filter pushes lines through the new
`Frame::push_log_line`, a caller reads them back through `Frame::log_lines`.
`showinfo` (`vaco-filter-analysis`) is the real consumer, reproducing the
reference's own two-line-per-frame format byte for byte against a real
synthetic frame — field widths, the `(a=0, b=0)`-seeded Adler-32 checksums
(confirmed independently here, the same seed already recorded for
`framecrc`/`framehash`), population-not-sample standard deviation, and the
`cl` field being chroma location rather than colour range (measured:
`-vf setrange=` does not move it, `-vf setparams=chroma_location=` does).
See that module's own doc for every field's measurement and the two things
still not reproduced: the once-per-link `config in`/`config out` lines
(a different kind of information than a per-`Frame` write can carry), and
anything above 8 bits per component (no fixture available to measure).

`Frame::log_lines` — the *reader* half — has no production caller yet:
`vaco-probe` still has zero decoders and no live `Frame` pipeline (D5),
the identical wall gap 11's own closing note recorded for `-show_frames`
itself. Wiring a real `-loglevel info` console renderer waits on that,
not on this gap.

## 14. `vaco_frame::FrameSideData` has no motion-vector variant — PARTIALLY CLOSED 2026-08-28 (shape only)

Reported by (confirmed, not newly found) the `vaco-filter-analysis` agent
finishing plan 16 §4.2's row on 2026-08-23.

`codecview`'s `mv`/`mv_type`/`qp`/`block` options all visualise per-block
motion vectors and quantiser values that a decoder would attach to a frame.
`crates/model/vaco-frame/src/lib.rs`'s `FrameSideData` enum has
`DisplayMatrix`, `ClosedCaptions`, `MasteringDisplay`, `ContentLightLevel`,
`Cropping` and `Metadata` (gap 11) — no motion-vector variant, and (per D5)
no decoder in this workspace produces one regardless, since motion vectors
are decoder-internal state that no codec crate currently surfaces at all.
This is a decoder-side gap, not a measurement-formula gap like the rest of
this crate's undone filters: `codecview` cannot be partially implemented
against synthetic `Frame`s the way `signalstats`/`entropy` can, because there
is no representable input to hand it.

### Shape

Additive: one more `FrameSideData` variant (e.g. `MotionVectors(Vec<MotionVector>)`
with a small `{source, w, h, dst_x, dst_y}`-shaped element, matching the
reference's own `AVMotionVector` fields) plus, separately and later, at
least one decoder that actually populates it. Both are out of scope for a
filter crate; recorded so `codecview` is not silently reattempted before the
decoder-side half exists.

**Blocks:** `codecview` (plan 16 §4.2, `vaco-filter-analysis`'s row); nothing
else in this workspace currently reads motion vectors either, so this gap has
exactly one known blocker today.


### Status, 2026-08-28

`FrameSideData::MotionVectors(Vec<MotionVector>)` landed, additive, no
existing match arm changes. `MotionVector`'s fields (`source`, `w`/`h`,
`dst_x`/`dst_y`, `src_x`/`src_y`) are a representative shape matching the
concepts `ffmpeg -h filter=codecview` names for `mv`/`block`, **not** a
transcription of the reference's internal struct — `ffprobe
-export_side_data +mvs` surfaces only a `"side_data_type": "Motion
vectors"` label on a real decode, no field breakdown, so there was nothing
to black-box-measure the exact layout against, and D7 rules out reading
the reference's source to fill the gap.

**Deliberately left without a producer.** No decoder in this workspace
attaches a motion vector to a `Frame` (D5), so `codecview` itself is still
not reachable, and this pass does not add a decoder to make it so — that
is a separate, substantial piece of work, not a filter-crate fix. Recorded
here so the next agent handed `codecview` starts from "the side-data shape
exists, the producer does not" rather than re-measuring the same wall.

## 15. `vaco-filter-color`'s `sample` module cannot address float pixel formats — CLOSED 2026-08-28

Reported by the `vaco-filter-color` agent, 2026-08-23 continuation pass,
asked specifically to establish (not assume) whether this is a real
infrastructure gap before routing any more work around it.

`exposure` and `grayworld` (plan 16 §4.2, `vaco-filter-color`'s row) both
force `gbrpf32le` output — confirmed to be a real, correctly-modelled pixel
format (`crates/model/vaco-pixfmt/src/table.rs`'s `PixFmt::Gbrpf32le`:
three 32-bit-per-component planes, `PLANAR | RGB | FLOAT`, matching the
reference's own `-pix_fmts` row). What is missing is not the format but a
way to *read* it: `vaco_filter_color::sample::read`/`write` are
`u16`-in/`u16`-out by signature, masking a value to `Component::depth` bits
and shifting it into a byte-aligned integer container. For an IEEE-754
`f32` component this is not a lossy downscale of the value — reinterpreting
the raw bits as an integer and truncating to fit `u16` produces an
unrelated number, not an approximation — so there is no way to route a
float sample through this accessor pair and get a meaningful answer,
regardless of which filter calls it. `sample::is_addressable` independently
rejects it twice over: it checks `PixFmtFlags::FLOAT` explicitly, and
separately requires every component's `depth <= 16`, which `gbrpf32le`'s
`depth=32` fails on its own.

This is a genuine infrastructure gap in `vaco-filter-color::sample`, not
friction specific to `exposure`/`grayworld` — a one-off float path built
inside either filter would not be a general capability, and the next
filter in this crate's own row needing float planes would hit the same
wall again.

### Shape

Additive, mirroring `sample`'s existing shape: an `f32`-in/`f32`-out
`read_float`/`write_float` pair (or a small enum-dispatched accessor that
picks integer vs. float encoding from `PixFmtFlags::FLOAT`), gated the same
way `is_addressable` already gates bit depth and packing. Not attempted
here — this crate's `sample` module is shared by every filter it has, so
changing its accessor shape is a design decision affecting all of them, not
a two-filter fix, and picking that shape is left for whoever is dispatched
`exposure`/`grayworld` (or the first float-format filter in any
`vaco-filter-*` crate) next.

**Blocks:** `exposure`, `grayworld` (`vaco-filter-color`'s row); any other
filter in this workspace that needs to read or write a `PixFmtFlags::FLOAT`
format through a `u16`-shaped sample engine would hit the identical wall.


### Status, 2026-08-28

`sample::is_float_addressable`/`read_float`/`write_float` landed, exactly
the shape sketched above: an `f32`-in/`f32`-out pair gated on
`PixFmtFlags::FLOAT` and `depth == 32` (16-bit float — `grayf16le` and
friends — is out of scope: reinterpreting two bytes as an `f32` needs
four, a different layout, not a narrower case of this one). `exposure` is
the real consumer, not just an interface: `out = (v*2^exposure -
black*2^exposure) / abs(1 - black*2^exposure)`, bit-exact against `ffmpeg
8.1` for every integer-`exposure` case tried on real `gbrpf32le` round
trips, including a `black` value that makes the naive `1 - black*scale`
denominator negative — the `abs` and the precomputed-reciprocal evaluation
order are both measured, not decorative (see that module's doc for the
reordering that mattered: a straight division reproduces the reference
only when the denominator is positive).

`grayworld` is **not** shipped by this close: this gap was specifically
the accessor wall, and closing it does not supply `grayworld`'s own
unmeasured LAB-space global-average algorithm, which stays exactly where
it was found — a separate piece of work, now blocked on algorithm
measurement rather than on infrastructure.

## 16. `vaco-demux-raw`'s `BitstreamSpec.parser_codec` cannot distinguish MPEG-1 from MPEG-2 — CLOSED 2026-08-28 (narrowed)

Closed by setting `M4V.parser_codec = Some(CodecId::Mpeg4)` (unconditional —
MPEG-4 part 2 has no MPEG-1/2-style ambiguity) and
`MPEGVIDEO.parser_codec = Some(CodecId::Mpeg2video)`, per the "Shape"
section's second option below: `PARSER_MPEG1` and `PARSER_MPEG2` both
construct the exact same `Mpeg12Parser`, differing only in which `CodecId`
reaches them through `ParserProvider::parser_for`, so either answer reaches
the right parser — this only leaves the *reported* `codec_name` wrong for a
genuine bare MPEG-1 elementary stream, accepted as the narrower, documented
limitation the original report already named as an option, rather than
solved with a per-spec probe or two specs.

Verified against `ffmpeg 8.1` (`-f lavfi -i testsrc -c:v mpeg2video -f
mpeg2video out.m2v` — `mpegvideo` is demux-only in this reference build, so
`mpeg2video` is the muxer name to use): `ffprobe` and `vaco-probe
-show_streams -f mpegvideo` agree on `codec_name=mpeg2video`,
`codec_long_name=MPEG-2 video`, `profile=Main`, `64x64` — `profile=Main`
confirms the real `Mpeg12Parser` ran through the registry's actual
`ParserProvider`, not just the static `codec_id` guess.

Original report, kept for the *why* and for the still-open MPEG-1
disambiguation:

`vaco-parse-mpegvideo` (P-07, #277) ships a real `Mpeg12Parser` for both raw
`CodecId::Mpeg1video` and `CodecId::Mpeg2video`, verified byte-for-byte
against `ffprobe -f mpegvideo -show_packets` on real encodes of both. But
`vaco-demux-raw::bitstream`'s `MPEGVIDEO` and `M4V` `BitstreamSpec` entries
(the raw-elementary-stream demuxers a bare `.m2v`/`.m4v` file opens through)
still carry `parser_codec: None`, so a raw MPEG-1/2/4 file never reaches
either parser today: `BitstreamDemuxer::open_with_limits` only calls
`parsers.parser_for(c)` when `spec.parser_codec` names a `CodecId` at all,
and falls back to `Framing::StartCode3`'s plain span-splitting with
`codec_id` left unset otherwise (`vaco-demux-raw/src/bitstream.rs`,
`compute_spans`/the `Frames::Spans` arm).

Setting `M4V.parser_codec = Some(CodecId::Mpeg4)` is a one-line fix — MPEG-4
part 2 raw streams have no MPEG-1/2-style ambiguity. `MPEGVIDEO` is the real
gap: ffmpeg's own `mpegvideo` raw demuxer covers *both* MPEG-1 and MPEG-2
(they share the identical `00 00 01 xx` start-code space, and `mpeg12.rs`'s
own `Sequence` type already resolves the two the same way — `sequence_
extension()`'s presence). `BitstreamSpec.parser_codec` is a single static
`Option<CodecId>` per spec row, decided once when the row is declared, so
it cannot express "ask a parser after inspecting the first sequence header
and only then know which of two `CodecId`s this is."

### Shape

Either widen `BitstreamSpec.parser_codec` from `Option<CodecId>` to
something that can defer the choice until the first sequence header is
seen (a small enum, or a `fn(&[u8]) -> Option<CodecId>` probe run once the
first bytes are in hand), or accept a `CodecId::Mpeg2video`-only default for
the `mpegvideo` raw demuxer (matching every practical `.m2v` file, since
plain MPEG-1 elementary streams are rare in the wild) and file the MPEG-1
raw case as a known, narrower gap instead — **the option taken above.**

**Blocks:** nothing now for MPEG-2/MPEG-4 raw streams. A genuine bare
MPEG-1 elementary stream still states `codec_name=mpeg2video` instead of
`mpeg1video`, and no fixture exercising that specific case was measured —
the narrower gap the original report anticipated.


## 17. No decoder in this workspace can produce a subtitle: `FrameData` has no `Subtitle` variant, and `Decoder` is fixed to return `Frame` — CLOSED 2026-08-28

Reported by the epic #44 agent (T2-13: DVB/VobSub/PGS/CEA-608/708/Teletext
decode), before writing any of the three new codec crates that epic asks for.

`vaco_codec_core::Decoder::receive_frame(&mut self) -> Result<Frame>`
(`crates/signal/vaco-codec-core/src/lib.rs`) is monomorphic: every decoder the
registry can hold returns exactly a `vaco_frame::Frame`. `Frame::data` is
`FrameData`, a closed enum with exactly two variants, `Video` and `Audio`
(`crates/model/vaco-frame/src/lib.rs`). There is no representable output for a
subtitle decoder anywhere between `Packet` and `Frame` — not a missing
`CodecId` (`DvdSubtitle`, `HdmvPgsSubtitle`, `DvbSubtitle`, `DvbTeletext` and
`Eia608` already exist and are already used by the two subtitle *muxer*
crates, `vaco-subtitle-bitmap` and `vaco-subtitle-text`) but a missing shape
for the thing a subtitle decoder hands back. Confirmed by grepping every
`vaco-component.toml` in the tree: zero components register `media =
"subtitle"` for `kind = "decoder"`, and none can, honestly, today.

This is different from gap 13/14, which are about a *filter's* measurement
channel (`showinfo`'s log line, `codecview`'s motion vectors) — this is
upstream of any filter: a subtitle decoder has nowhere to put its output at
all, so `-c:s <name>` cannot dispatch to a real implementation regardless of
how complete that implementation is.

### Shape

`FrameData` needs a third variant — something like a rectangle list (position,
size, indexed-bitmap-plus-palette or plain text-with-attributes) matching what
the reference calls `AVSubtitleRect`/`AVSubtitle`, since that is the smallest
shape the five formats in T2-13 already converge on independently (DVB, VobSub
and PGS all decode to a palette-index bitmap rectangle; CEA-608/708 and
Teletext both decode to positioned text). `Decoder::receive_frame`'s
`Result<Frame>` return type stays unchanged — the new variant is what carries
subtitle output through the same channel video and audio already use — but
every existing call site that matches on `FrameData::{Video,Audio}`
exhaustively would need a third arm, so this is not a purely additive change
the way gap 11's `Metadata` variant was. Not attempted here: picking the exact
rectangle/text shape is a design decision for whoever owns `vaco-frame`, and
widening `Decoder`'s contract (or the call sites that match on `FrameData`)
is `vaco-codec-core`/`vaco-filter-core`/`vaco-cli` territory, all outside a
subtitle-decoder crate's single-writer scope.

**Blocks:** `-c:s <name>` ever reaching a live decoder for any subtitle
codec — not only the five T2-13 formats but the text ones too
(`SubRip`/`Ass`/`Webvtt`/`Ttml`/... all already have `CodecId` rows and none
have a decoder either, for the same reason). T2-13's three new crates
(`vaco-codec-subtitle-bitmap`, `vaco-codec-subtitle-cc`,
`vaco-codec-subtitle-teletext`) are built as standalone libraries with their
own decode-result types instead of implementing `Decoder`, specifically so
their real parsing/RLE/Hamming/DTVCC work does not sit idle waiting on this —
they are ready to be wired in the moment this gap closes, but none of them
register a `vaco-component.toml` `kind = "decoder"` fragment today, so none
are reachable from the CLI yet.


### Status, 2026-08-28

`FrameData::Subtitle { rects: SmallVec<[SubtitleRect; 2]> }` landed
(`crates/model/vaco-frame/src/subtitle.rs`), the rectangle-list shape this
entry argued for: `SubtitleRect { x, y, w, h, forced, content }` where
`content` is `Bitmap { stride, data: Buffer, palette: Vec<[u8; 4]> }` (DVB/
VobSub/PGS), `Text(String)` (CEA-608/708 and Teletext once decoded, plus
SubRip/WebVTT/TTML natively) or `Ass(String)` (ASS/SSA markup lines) — the
reference's own `AVSubtitleRect` discrimination. The display-time window
is deliberately *not* a field on the variant: `Frame::pts`/`Frame::duration`
already carry it for every variant, and duplicating it here would give a
subtitle frame two independent, possibly-disagreeing clocks. Bitmap pixel
data goes through `Buffer::from_slice`, the same budget-tracked allocation
video planes use, since a decoder's bitmap dimensions come from attacker-
controlled bytes; the palette is a plain `Vec` because its 256-entry cap
falls out of the `u8` index space with nothing extra to enforce.

**`FrameData` stays a closed enum — no `#[non_exhaustive]`.** Considered and
rejected: `FrameSideData` is `#[non_exhaustive]` because its variant set is
genuinely open-ended (one per filter family, generated incrementally).
`FrameData` partitions *what kind of decoded output a `Frame` is*, which is
closed by the model itself — the reference's own `AVMediaType` enumerates
the identical small, stable set for anything that decodes to a frame at
all. Marking it `#[non_exhaustive]` now would force a wildcard arm onto
every match this close just gave an explicit arm instead, trading twelve
honest call sites today for silent pass-through at all of them against a
media type the reference's own evidence says is not coming.

**Every call site re-measured, not taken on report.** The originating
report's own re-check (a same-line `FrameData::Video.*=>` grep) found 10
files; redoing it with a brace-balanced scan across every `match` block
mentioning both `FrameData::Video` and `FrameData::Audio` found **12**:
the same 10 plus `vaco-filter-analysis::video::video_shape` and
`vaco-filter-deinterlace::video::dims`, both multi-line struct patterns
the single-line regex could not see. All 12 got an explicit arm (an arm
that says "not applicable here" beats a wildcard, per this pass's own
brief); `vaco-filter-core::link::Link::accepts` also matches on
`FrameData` but already carries a `_ => false` wildcard that is correct
independent of variant count (no `LinkFormat::Subtitle` exists to need
its own arm), so it was left alone rather than padded with a redundant
one. `vaco-frame` itself needed seven of the twelve (`is_subtitle`,
`pixel_format`, `dimensions`, `planes_slice`/`_mut`, `plane`/`_mut`,
`planes_mut`) — a subtitle frame reports no pixel format, no dimensions
and zero planes, consistently.

**What this does not do**: register a decoder, touch
`vaco-codec-subtitle-bitmap`/`-cc`/`-teletext`, or prove end-to-end
decode — no decoder in this workspace constructs a `FrameData::Subtitle`
yet, so this is a shape the three in-flight T2-13 crates can be wired to,
unit-tested on its own terms (`cargo test -p vaco-frame`), not a claim of
working `-c:s`. `vaco-codec-subtitle-teletext`'s own module doc still
says "`FrameData` has exactly two variants" as of this close — stale now,
left for that crate's own owner to update when it wires in, since this
pass does not touch that crate.

## 18. Nothing in this workspace populates `FrameSideData::ClosedCaptions` — PARTIALLY CLOSED 2026-08-28

Reported by the same agent, scoping CEA-608/708 (`vaco-codec-subtitle-cc`).

The variant exists (`crates/model/vaco-frame/src/sidedata.rs`,
`FrameSideDataKind::ClosedCaptions`, added for gap 11's side-data table) and
`vaco-filter-mm`'s `sidedata` filter already knows how to delete or report it
(`mapped_kind`'s `1 => Some(FrameSideDataKind::ClosedCaptions), // A53_CC`).
But no decoder or parser in this workspace ever constructs one: grepping
`vaco-parse-h264`, `vaco-parse-hevc` and `vaco-parse-mpegvideo` for
`user_data_registered_itu_t_t35`/A/53 caption SEI or user-data extraction
finds nothing. So even once gap 17 above is closed, a CEA-608/708 decoder has
no real `cc_data` to decode from an actual compressed video file in this
tree today — only from bytes constructed by hand or extracted out-of-band.

### What landed

**The extraction half, for all three sources.** Each parser gained an `a53`
module returning the raw `cc_data` triplet bytes — the `cc_count * 3` payload
only, dropping `cc_data()`'s two-byte header and trailing marker, which is
exactly the shape `vaco-codec-subtitle-cc` consumes and the reference's own
`A53_CC` side data carries:

| Crate | Entry point | Mechanism |
|---|---|---|
| `vaco-parse-h264` | `a53::cc_data_from_sei` | SEI type 4, T.35 `0xB5`/`0x0031`, `GA94`, type `0x03` |
| `vaco-parse-hevc` | `a53::cc_data_from_sei` | same prefix and payload type; 2-byte NAL header and prefix/suffix SEI handled upstream |
| `vaco-parse-mpegvideo` | `a53::iter_cc_data`, `a53::find_cc_data` | `user_data_start_code` `0x000001B2`, no T.35 prefix at all |

Every constant was verified against a real broadcast capture
(`transformers_EIA608_H264.ts`) rather than recalled, and each path was
differentially checked against the reference's own `A53_CC` side data —
identical as a multiset for all three codecs (361 H.264 frames, 120 each for
HEVC and MPEG-2 transcodes). No allocation anywhere: `cc_count` is a 5-bit
field, so every return is a borrowed subslice of at most 93 bytes.

### What is still open

**The attachment half.** These are *parsers*: they emit `CodecParameters` and
packet boundaries, not `Frame`s, so there is nothing here to hang a
`FrameSideData::ClosedCaptions` on. Constructing that variant belongs to
whatever produces the `Frame` — a decoder — and this workspace has no H.264,
HEVC or MPEG-2 decoder. The bytes are now available at the exact point such a
decoder would need them; nothing more can be done from the parser side.

### The trap anyone doing the attachment half must know about

**Captions must be consumed in presentation order, and getting it wrong fails
silently.** CEA-608 is a stateful sequential command language, so
concatenating payloads in *decode* order interleaves and destroys the caption
stream. Measured, on the real capture: the same 361 payloads decode to
`" its cities now."` in presentation order and to `"    s  itesciti. now"` in
decode order — **both with zero parity errors**, because every byte pair is
individually valid and only their sequence is wrong. Nothing in the caption
layer signals the mistake.

Attaching each payload to *its own* `Frame` is what makes this correct by
construction, since frames are reordered before output. Accumulating payloads
into a buffer as pictures are parsed is the wrong shape and will look like it
works.

**Blocks:** end-to-end CEA-608/708 decode from a real broadcast file, now only
on the decoder side. Not blocking `vaco-codec-subtitle-cc`'s own correctness,
which is verified directly against hand-built and extracted `cc_data` bytes.

## 19. `Decoder` has no channel for a codec's out-of-band configuration — CLOSED 2026-08-28

Found wiring `vaco-codec-subtitle-bitmap`'s three decoders to the registry —
the first subtitle decoders in this build, and the first consumer of gap 17's
`FrameData::Subtitle`.

`Parser` has `set_extradata` (`crates/signal/vaco-codec-core/src/lib.rs`), and
its own doc calls that "the mechanism that makes a parser useful at all in MP4
and Matroska". `Decoder` has no equivalent: the whole trait surface is
`send_packet`/`receive_frame`/`flush`, and `DecoderDesc::make` takes only
`Limits`. So a decoder reached through `vaco_registry::decoder_for` cannot be
told anything the container knows.

DVD/`VobSub` is the concrete case. An SPU's `SET_COLOR` command does not carry
colours — it carries four 4-bit *indices* into a 16-entry palette that lives
entirely outside the SPU bytes: in the `.idx` sidecar, or in a Matroska
`S_VOBSUB` track's `CodecPrivate`. `vaco_codec_subtitle_bitmap::vobsub::decode_spu`
takes that palette as an explicit parameter and works correctly when a caller
has it (`VobSubDemuxer::open_pair` is such a caller). The *registered* decoder
cannot be one, so `decoder::DVDSUB_DECODER` paints with a documented grey-ramp
fallback: geometry and pixel indices are right, colours are not the disc's.
This is deliberately visible in `decoder::fallback_palette`'s doc rather than
silently wrong.

Not a subtitle-only problem — it is the same shape as any codec whose
configuration is in the container (an `avcC`, an `AudioSpecificConfig`), which
is why it is recorded here rather than in the crate.

### Shape

Additive, mirroring `Parser`: a defaulted `fn set_extradata(&mut self, &[u8])
-> Result<()>` on `Decoder`, forwarded by `Box<dyn Decoder>` and by any
wrapper (`AsDecoder`/`Validated`) the way `Box<dyn Parser>` already forwards
`Parser::set_extradata` — the `Box<dyn Muxer>`-shaped trap gap 9 named. Then a
call site: `vaco-cli`'s transcode leg has the demuxer's `CodecParameters` in
hand at `exec.rs`'s `decoder_desc.build(limits)` and could offer
`params.extradata` there. Both halves are outside this crate's scope, so
neither was attempted.

**Blocks:** correct DVD subtitle colours through any registry-driven path.
Does not block `vobsub` decode itself, which is correct when the palette is
supplied directly.

### Status, 2026-08-28

Landed exactly as sketched above. `Decoder::set_extradata` is a defaulted
`fn(&mut self, &[u8]) -> Result<()>` (`crates/signal/vaco-codec-core/src/lib.rs`),
with the identical default on `SendReceive` so `AsDecoder` has something to
forward to. Every layer between a registered decoder and its `Box<dyn
Decoder>` forwards explicitly: `SendReceive` itself, `AsDecoder`,
`DecoderProtocol`, `Validated`, and a new `impl<D: Decoder + ?Sized> Decoder
for Box<D>` mirroring the existing `Box<dyn Parser>` one. `vaco-cli`'s
`exec.rs` offers `p.extradata` at the `decoder_desc.build(limits)` call site,
discarding a refusal the same way `Parser::set_extradata`'s own callers do.

**The forwarding trap named in this gap's own text bit on the first attempt**,
caught by a dedicated test rather than in review: an `AsDecoder<Validated<T>>`
built without overriding `set_extradata` on `AsDecoder`'s `impl Decoder`
compiles cleanly and silently answers `Ok(())` from the trait default,
never reaching `T`. `vaco-codec-core/tests/protocol.rs`'s
`set_extradata_forwards_through_as_decoder_validated_and_the_box` pins this:
an inner `SendReceive` whose `set_extradata` always errs with a distinctive
message is wrapped exactly as `vaco-codec-subtitle-bitmap`'s three registered
decoders are (`Box::new(AsDecoder(Validated::new(inner)))`), and the test
requires that exact error to surface through the box. Deliberately reverted
the `AsDecoder` forwarding line and re-ran the test to confirm it fails
before restoring it — it does.

**The DVD colour measurement this gap asked for.** `VobSubSubtitleDecoder`
now holds a palette field, initialised to the documented grey ramp and
overwritten by `set_extradata` when the bytes parse as `.idx`-style text with
a `palette:` line (`vaco_subtitle_bitmap::vobsub::idx::parse`, already
written for the demuxer side — reused rather than re-implemented). Matroska's
own subtitle-mapping page (`matroska-subtitles-mapping` in
`provenance/sources.toml`) states a `S_VOBSUB` track's `CodecPrivate` is
exactly the `.idx` file's `size:`/`palette:` lines with the `id:`/
`timestamp:`/comment lines stripped, so that parser is the right one to feed
raw `extradata` bytes into unchanged. Measured on a hand-built 4×2 SPU whose
`SET_COLOR` command indexes palette slot 3: before `set_extradata` is called,
`DVDSUB_DECODER.build(..)` paints that pixel `[51, 51, 51, 255]` (the grey
ramp, `3 * 17`); after offering `b"size: 720x480\npalette: 000000, 0a141e,
ffffff, 010203\n"`, the identical SPU bytes paint `[1, 2, 3, 255]` — slot 3 of
the offered palette. Both the private `make_vobsub` path and the real
`DVDSUB_DECODER.build` (`Box<dyn Decoder>`) path are exercised
(`crates/codec/vaco-codec-subtitle-bitmap/src/decoder.rs`'s
`dvd_subtitle_colours_come_from_extradata_not_the_grey_ramp` and
`the_registered_dvdsub_decoder_forwards_set_extradata_through_the_box`).

## 20. A decoded `Frame` cannot express a display window the codec states in absolute time — CLOSED 2026-08-28 (narrowed — no `Decoder` or `vaco-sched` change needed)

Same origin as gap 19, and a genuinely different problem from it.

`FrameData::Subtitle`'s author recorded (correctly, and this consumer agrees)
that the display window should be `Frame::pts`/`Frame::duration` rather than
new fields, so one frame cannot hold two disagreeing ideas of when it
displays. Those two fields are counted in the stream's time base — which is
what `PipelineSpec::add_decoder` propagates onto the graph edge it creates.

Two of the three bitmap subtitle formats state a display window *in absolute
time* rather than in the container's units:

* DVB's `page_time_out` (EN 300 743 §7.2.2) is a whole number of **seconds**.
* `VobSub`'s SPU `SP_DCSQ_STM` start/stop delays are **90 kHz / 1024 ticks**
  relative to the packet, which this crate already converts to microseconds.

Both are real, useful, and unrepresentable: converting either into
`Frame::duration` requires the stream's time base, and the `Decoder` trait
never receives it (gap 19's surface note applies — `send_packet`/
`receive_frame`/`flush` is all there is). `vaco-codec-subtitle-bitmap`
therefore copies `Frame::pts`/`Frame::duration` straight from the packet,
which is correct and mutually consistent but discards what the codec itself
said about how long the subtitle should stay up. `SubtitleEvent::start`/`end`
still carry it for a direct caller.

Worth distinguishing from gap 19: adding `set_extradata` alone would **not**
fix this. A time base is not extradata — it is a property of the stream the
graph already knows and simply does not pass down.

### Shape

Either a defaulted `fn set_time_base(&mut self, Rational)` on `Decoder`
(cheapest; the graph edge already has the value at
`PipelineSpec::add_decoder`), or letting a decoder set `Frame::time_base`
itself and having the frame consumer rescale — the latter is closer to what
`Frame::time_base` looks like it was meant for, since today every decoder in
this tree leaves it at `Rational::ONE` while stamping `pts` in the stream's
units (`vaco-codec-qoi` does exactly this, so the inconsistency predates
subtitles). Both are `vaco-codec-core`/`vaco-sched` changes, outside this
crate's scope.

**Blocks:** a subtitle renderer knowing how long to leave a DVB or DVD
subtitle on screen, when the container does not state a packet duration.

### Status, 2026-08-28 — the premise above does not hold

This entry's claim that "converting either into `Frame::duration` requires
the stream's time base" was checked against the actual types rather than
taken on report, and it is wrong. `Frame::duration`/`Packet::duration` are
`vaco_core::Duration` — always real microseconds by construction
(`Duration::from_micros`/`as_micros`, and `Timestamp::to_duration`/
`Duration::to_ticks` are the only things in the tree that convert *between*
Duration and a time base, never Duration alone). `SubtitleEvent::start`/`end`
(`crates/codec/vaco-codec-subtitle-bitmap/src/lib.rs`) are already the same
`Duration` type: DVB's `dvb.rs` builds `end` as
`Duration::from_micros(i64::from(page_time_out).saturating_mul(1_000_000))`
and `vobsub.rs`'s `ticks_to_micros` does the 90 kHz/1024 conversion before
`decode_spu` ever returns. So the codec's own display
window arrives at `frame_of_event` in exactly the unit `Frame::duration`
wants, with no time base anywhere in the path — confirmed independently by
grepping every other decoder in the tree (`vaco-codec-vp8`, `vaco-codec-mpeg12`)
for the same `frame.duration = packet.duration` copy with no rescale, which
only makes sense if `Duration` is already time-base-free.

**What landed:** `frame_of_event` now computes `Frame::duration` as
`event.end - event.start` when the codec stated an `end` (DVB and `VobSub`
always do; PGS never does, per `SubtitleEvent`'s own doc, so PGS frames are
unaffected and keep the packet's duration), entirely inside
`vaco-codec-subtitle-bitmap` — no change to `Decoder`, `vaco-sched`, or
`PipelineSpec::add_decoder`. `Frame::pts` is left as the packet's own `pts`
unconditionally, unchanged from before. Confirmed against the existing
`dvb_packet_becomes_a_subtitle_frame_carrying_the_librarys_own_rects` test,
whose fixture states a 5-second `page_time_out`: `frame.duration` now reads
`5_000_000` microseconds (the codec's stated window) rather than the test
packet helper's fixed `2_000_000` — the test's own assertion was updated to
match, since it had been pinning the discarded behaviour this gap exists to
fix, the same "test holding the bug in place" shape `AGENT-CONSTRAINTS.md`
warns about for `codec_tag`.

**What is still open, narrower than the original gap:** `VobSub`'s
`SP_STA_DSP` can state a non-zero `start` (a delayed display, separate from
the packet's own PTS), and shifting `Frame::pts` forward by that amount
*would* need the stream's time base to turn a microsecond delay into ticks —
that part of the original problem is real, just much smaller than "the whole
display window needs a time base". Left as `Frame::pts = packet.pts`
unconditionally; the display *length* this close now reports is correct
regardless, only the display *start* can be off by that delay on the rare
stream that sets one. Not chased further here, since a `Decoder::set_time_base`
built to fix a case this narrow would be solving a problem `vaco-codec-subtitle-bitmap`
does not currently have evidence needs it (D19) — recorded in
`planning/TECH-DEBT.md` instead of speculatively adding the interface surface.

## 21. `vaco-codec-core`'s `CodecId` has no variants for any game-video/game-audio codec — CLOSED 2026-08-28 for `roq`/`flic`/`cdg`/`bink`/`smk`; a tenth id for a different consumer is still open (see below)

Found implementing `vaco-format-misc` (FM-59, issues #623/#624/#625): `ivf`,
`ffmetadata`, `roq`, `flic`, `cdg`, `bink` and `smk`. Of these, `roq` (video
and audio), `flic`, `cdg`, `bink` (video and audio) and `smk` (video and
audio) all need a `CodecId` the enum does not have, and every other
game-video/legacy-video container this package's brief named (`vmd`,
`idcin`, `interplayvideo`/`interplaydpcm`, `cinepak`, `truemotion1`/`2`,
`xan`, and roughly forty more) would need the same. `vaco-format-misc`'s
streams for all six of these formats therefore carry `codec_id: None`, so
`vaco-probe -show_streams` prints `codec_name=unknown` where the reference
prints a real name (`roq`, `roq_dpcm`, `flic`, `cdgraphics`, `binkvideo`,
`binkaudio_dct`/`binkaudio_rdft`, `smackvideo`, `smacker`) — measured
directly, see that crate's own module docs and
`docs/format/vaco-format-misc.md`.

This is the same shape as gap 17/18's `FrameData`/`FrameSideData` variant
additions and the codec-core doc's own precedent ("twelve text-subtitle
codec ids, so seventeen demuxers can name themselves" — commit `9d9655b`,
"56 more CodecId variants, generated by probing" — commit `d68c8fe`): a
format crate discovers the gap because it is the first consumer, but only
`vaco-codec-core` may add the variant, since `CodecId` is a fieldless enum
several call sites (at least `vaco-bsf-generic`'s noise generator) cast to
`u64`, which Rust only allows when every variant is fieldless — a
`CodecId::Ext(&'static ExtCodec)` escape hatch was considered and rejected
for exactly that reason when the image-codec family hit this same wall.

The exact variants needed, by name the reference reports:

| Format | `ffprobe` `codec_name` | Proposed variant |
|---|---|---|
| roq (video) | `roq` | `Roq` |
| roq (audio) | `roq_dpcm` | `RoqDpcm` |
| flic | `flic` | `Flic` |
| cdg | `cdgraphics` | `Cdgraphics` |
| bink (video) | `binkvideo` | `Bink` |
| bink (audio) | `binkaudio_dct`/`binkaudio_rdft` (selected by the container's own audio-algorithm flag, bit 12 of the per-track flags word) | `BinkAudio` |
| smk (video) | `smackvideo` | `Smacker` |
| smk (audio) | `smacker` | `SmackAudio` |

### Shape

Add these eight variants to `vaco-codec-core::CodecId` (name, long name,
media type, `CodecProperties`, probed from `ffmpeg -codecs` the way every
prior batch in that file was). `bink`/`smk`'s chunk/frame-index-table
container framing is now implemented and demuxes structurally
(`crates/format/vaco-format-misc/src/bink.rs`,
`crates/format/vaco-format-misc/src/smk.rs`); only the codec identity is
missing, not the container support. The remaining ~40 names in the
original package are not yet demuxed at all and would need their own
variants when someone gets to them.

**Blocks:** `-show_streams`/`-show_format` byte-identical `codec_name`/
`codec_long_name` for every container in this family; a decoder ever being
registered for any of them (`vaco_registry::decoder_for` matches on
`CodecId`, so a codec cannot be found by identity without one, same as gap
17 noted for subtitle bitmap codecs).

### Status, 2026-08-28 — variants landed, wiring does not

Nine variants added to `vaco-codec-core::CodecId`, not eight: `Roq`,
`RoqDpcm`, `Flic`, `Cdgraphics`, `Bink`, `BinkAudioDct`, `BinkAudioRdft`,
`Smacker`, `SmackAudio`. The proposed shape above collapsed Bink's audio
into one `BinkAudio` variant; measured against `ffmpeg -codecs` and
`-decoders`, the reference has no such unified name — `binkaudio_dct` and
`binkaudio_rdft` are two entirely distinct `codec_name`s with independent
decoder rows, so one variant could not print the right name for whichever
algorithm a track did not use. Split into two, the same reason `AacLatm`
is not folded into `Aac`.

Every name/long-name pair verified two ways: `vaco-codec-core`'s own
`the_codec_table_agrees_with_the_reference` test (`tests/params.rs`), which
diffs the whole `CodecId` table against a live `ffmpeg -hide_banner
-codecs` run, passed with these nine included; and, for `roq`/`roq_dpcm`
specifically, a real file round-tripped through the reference's own
`roqvideo`/`roq_dpcm` encoders and `ffprobe` (the other seven have no
reference encoder to round-trip through, so `-codecs`/`-decoders` is the
only measurement available — the same situation the PCM/leaf-image/RTP
batches were already in). Flags (`CodecProperties::LOSSY`/`LOSSLESS`/
`INTRA_ONLY`) read directly off `-codecs`' own I/L/S columns rather than
guessed — `flic` is lossless where the other three video codecs are lossy,
and every audio row here is intra-only where none of the video rows are.

| `CodecId` | `name()` | `long_name()` | properties | needed by |
|---|---|---|---|---|
| `Roq` | `roq` | `id RoQ video` | LOSSY | `vaco-format-misc`'s `roq` (issue #623) |
| `RoqDpcm` | `roq_dpcm` | `DPCM id RoQ` | LOSSY, INTRA_ONLY | `vaco-format-misc`'s `roq` (issue #623) |
| `Flic` | `flic` | `Autodesk Animator Flic video` | LOSSLESS | `vaco-format-misc`'s `flic` (issues #623, #624) |
| `Cdgraphics` | `cdgraphics` | `CD Graphics video` | LOSSY | `vaco-format-misc`'s `cdg` (issue #625) |
| `Bink` | `binkvideo` | `Bink video` | LOSSY | `vaco-format-misc`'s `bink` (issues #623, #624) |
| `BinkAudioDct` | `binkaudio_dct` | `Bink Audio (DCT)` | LOSSY, INTRA_ONLY | `vaco-format-misc`'s `bink`, when the track's audio-algorithm flag (bit 12) selects DCT |
| `BinkAudioRdft` | `binkaudio_rdft` | `Bink Audio (RDFT)` | LOSSY, INTRA_ONLY | `vaco-format-misc`'s `bink`, when that flag selects RDFT |
| `Smacker` | `smackvideo` | `Smacker video` | LOSSY | `vaco-format-misc`'s `smk` (issues #623, #624) |
| `SmackAudio` | `smackaudio` | `Smacker audio` | LOSSY, INTRA_ONLY | `vaco-format-misc`'s `smk` (issues #623, #624) |

**What did not land, deliberately out of this pass's scope**: nothing in
`vaco-format-misc` was touched — it has a live owner mid-package on `bink`/
`smk` — so every stream in that crate still reports `codec_id: None` today.
The variants exist to be wired, not wired. Gap 21 stays open until a real
`vaco-format-misc` stream actually sets one of these nine and `-show_streams`
prints the reference's `codec_name` for it.

Also found while probing, unrelated to this gap and not fixed here (out of
scope — see `planning/TECH-DEBT.md`): `AdpcmAdx`'s existing table row
(added before this pass) is missing `CodecProperties::INTRA_ONLY`, which
`ffmpeg -codecs`' own `adpcm_adx` row (`DEAIL.`) states it should have.

### A tenth needed variant, found implementing `vaco-format-misc-audio`'s `vag` (2026-08-28)

Different crate, different issue (#620, FM-58's game-audio containers, not
FM-59's game-video group above), same gap: Sony PS2 VAG's codec is
`adpcm_psx` (`ADPCM Playstation` — confirmed via `ffmpeg -codecs`/a real
`ffprobe` run over a hand-built fixture, `Vaco-Spec-Ref
vaco-format-misc-audio-vag-xwma-fixtures-probe`), and `CodecId` has no
variant for it any more than it had one for `Roq`/`Bink`/`Smacker` before
this gap's first pass. `vaco-format-misc-audio`'s `vag.rs` stream carries
`codec_id: None` until it lands, same policy as every other format this
gap has ever named.

| Format | `ffprobe` `codec_name` | Proposed variant |
|---|---|---|
| vag | `adpcm_psx` | `AdpcmPsx` |

`xwma` needed no new variant: its `wFormatTag` maps onto the existing
`Wmav1`/`Wmav2`/`Wmapro`.

### An eleventh needed variant, found implementing `vaco-format-misc-audio`'s `xa` (2026-09-02)

Same gap, same crate, a later pass at #620: Maxis XA's codec is
`adpcm_ea_maxis_xa` (`ADPCM Electronic Arts Maxis CDROM XA` — confirmed via
`ffmpeg -codecs`/a real `ffprobe` run over a hand-built fixture,
`Vaco-Spec-Ref vaco-format-misc-audio-xa-fixtures-probe`), and `CodecId`
has no variant for it. `vaco-format-misc-audio`'s `xa.rs` stream carries
`codec_id: None` until it lands, same policy as `vag` above.

| Format | `ffprobe` `codec_name` | Proposed variant |
|---|---|---|
| xa | `adpcm_ea_maxis_xa` | `AdpcmEaMaxisXa` |

### Status, 2026-08-28 — the original five demuxers wired and measured

`vaco-format-misc` (`roq.rs`, `flic.rs`, `cdg.rs`, `bink.rs`, `smk.rs`) now
sets `CodecParameters::codec_id` on every stream it constructs. Verified
three ways: the crate's own unit tests assert `codec_id` per stream; a
hand-built fixture per format run through the real `vaco-probe` binary
(`-show_streams`) prints the reference's exact `codec_name`/
`codec_long_name`; the identical fixture bytes run through a real `ffprobe`
print the same, byte for byte, for every format except `smk` (the
reference's real `smackvid` decoder refuses to open a framing-only
synthetic fixture with no valid Huffman tree data — the same limitation
`vaco-format-misc`'s own bink/smk landing comment already recorded, worked
around there with `-c copy -f framemd5`; here, `ffprobe`'s own `Input #0`
summary line printed before that failure already showed `smackvideo`
matching).

One correction found only by testing real fixtures rather than trusting the
unit tests: `smk`'s audio `codec_id` is not fixed to `SmackAudio`. An
`AudioRate` entry's `compressed` bit — already read by this crate for
framing — decides whether a track's bytes are Smacker's own compressed
audio or raw PCM; running the crate's own (uncompressed) default fixture
through `ffprobe` printed `pcm_s16le`, not `smackaudio`. Fixed to branch on
`compressed`, selecting `PcmS16le`/`PcmU8` (by the existing bit-depth flag)
when clear.

This closes the gap for the nine ids this pass's own table covers. It does
not close the tenth (`AdpcmPsx`, immediately above) — a different consumer,
found by a different agent, after this pass had already measured and
committed its own nine. Whoever wires `AdpcmPsx` into `vaco-format-misc-audio`'s
`vag.rs` should re-open this kind of entry rather than assume "gap 21 is
CLOSED" covers it too.

## Gap 22 — no cross-node introspection reachable from a filter (`graphmonitor`/`agraphmonitor`, `vaco-filter-scope`) — CLOSED 2026-08-28

`ffmpeg -h filter=graphmonitor`/`agraphmonitor` draw a picture of the
*whole filtergraph's* live state — every link's queue depth, EOF status,
disabled/timeline state, format — driven by `mode`/`flags` bitmasks
(`full`, `compact`, `nozero`, `noeof`, `nodisabled`, `queue`, and more).
Building `vaco-filter-scope` (issue #480, "FT-4.12g") found that this is
not just unimplemented but **not expressible against the current
`vaco-filter-core::FilterContext` surface**, checked directly rather than
assumed: `FilterContext` exposes only the current node's own pads
(`input_link`/`output_link`/`peek_input`/`input_depth`, all keyed through
`self.node: &NodeLinks`, which holds only this node's `LinkId`s). There is
no accessor that reaches another node, enumerates the graph's nodes at
all, or reads a `Link`'s `LinkStats` from outside the two endpoints that
own it.

This is consistent with the architecture's own stated invariant (plan 16
§1.1: "a filter can never reach another filter's private state, only link
state") — it is not an oversight so much as a boundary nobody has needed
to cross yet. Closing this gap means adding a read-only, whole-graph
snapshot accessor (node list, per-link `LinkStats` plus queue
depth/status/disabled-state, in a form that does not let a filter mutate
anything outside its own pads) to `vaco-filter-core`, which is core-crate
work outside `vaco-filter-scope`'s own ownership.

Not fixed here: `graphmonitor`/`agraphmonitor` are left unimplemented in
`vaco-filter-scope`, recorded rather than worked around.

### Status, 2026-08-28 — a read-only NodeInfo/LinkView snapshot, deliberately narrow

Closed by adding two accessors to `FilterContext` in
`crates/filter/vaco-filter-core/src/context.rs`:
`graph_nodes(&self) -> &[NodeInfo]` (each node's `NodeId`, scheduler
label, and `&'static str` filter name) and `graph_links(&self) ->
Vec<LinkView>` (each link's id, `PadRef` src/dst, `MediaType`, queued
frame count, capacity, EOF flag, and its existing `LinkStats`). Both are
read-only snapshots taken at call time — no method lets a filter push to
another node's link, close another node's pad, or reach another node's
`Filter` implementation itself, which is the boundary plan 16 §1.1 draws
("a filter can never reach another filter's private state, only link
state").

Most of the data already existed: `LinkStats`'s own doc comment already
named `graphmonitor` as an intended consumer, and `FilterContext`'s
`links: &mut LinkArena` field was already a reference to the *entire*
graph's link arena, not just the current node's — the gap was in which
methods exposed it, not in missing data. Node labels were the one
genuinely new piece: `Graph` gained a `node_labels: Vec<NodeInfo>` field,
populated incrementally in `push_node` (not rebuilt per `activate()` call,
to avoid an allocation on the scheduler's hot path), and threaded through
to both `FilterContext::new` call sites in `sched.rs`.

Deliberately excluded: per-node scheduler-internal state (`parked_at`,
`self_driven`, `last_run`) and link formats — `graphmonitor`'s `mode`
flags need queue depth, EOF, and disabled-state, none of which require
these. A general graph accessor (arbitrary node reach, mutation of other
nodes' pads) was considered and rejected per D19/the coordinator's own
framing: the narrowest surface that serves the two named consumers, not
a general capability that would let a filter's output depend on
scheduling order.

Verified end-to-end, not just by type-checking: a real 3-node `Graph`
(source → probe filter → sink) with a `FrameFilter` that calls
`ctx.graph_nodes()`/`ctx.graph_links()` on its first frame
(`a_filter_can_read_every_nodes_label_and_every_links_state` in
`crates/filter/vaco-filter-core/tests/graph.rs`) asserts the probe can see
the *other* two nodes' labels and the state of the link feeding it — then
deliberately broken (`graph_nodes`/`graph_links` stubbed to return empty)
to confirm the test fails loudly (`"a filter must be able to name nodes
that are not itself: []"`) before restoring the real implementation.

**Not fixed here**: this closes the framework capability only.
`graphmonitor`/`agraphmonitor` are still not implemented as filters in
`vaco-filter-scope` — wiring them (parsing `mode`/`flags`, rendering the
overlay) is separate work this pass did not do, left for whoever picks
that crate back up.

### Status, 2026-08-28 — wired into real `graphmonitor`/`agraphmonitor` filters, closing this as a solution, not just a capability

The status above closed the framework capability; it did not yet prove a
real consumer used it as built. `vaco-filter-scope` now implements both
`graphmonitor` and `agraphmonitor` (`src/graphmonitor.rs`, one `Filter`
shared by both — the only difference is the input pad's media type),
using `ctx.graph_nodes()`/`ctx.graph_links()` to draw the live filtergraph
as text: one block per node (the monitor's own node included), a header
line then one line per pad naming the peer node and its live counters
(queue depth/capacity, `at_eof`, frame/sample count, peak depth,
backpressure-blocked count).

Proven with a real `Graph`, not just the crate's own pure-function unit
tests: `tests/graphmonitor.rs` builds an actual source → `graphmonitor` →
sink graph (and, separately, an audio source → `agraphmonitor` → video
sink graph), runs it through the real scheduler, and checks the rendered
output actually reflects the other nodes it is wired to — deliberately
broken (`graph_nodes`/`graph_links` stubbed to return nothing inside
`graphmonitor`'s own `filter_frame`) and confirmed to fail before
restoring the real call, the same discipline gap 24's `Dual` adapter
tests used.

One real limit found by building the actual consumer rather than assuming
the accessors were enough: `NodeView`/`LinkView` cannot serve the
reference's `format`/`size`/`rate`/`timebase`/`pts`/`time` fields at all
(link geometry, timing, and timestamp values are outside gap 22's
deliberately narrow snapshot — confirmed a design choice, not an
oversight, matching the coordinator's own "narrowest thing that serves
the two consumers" brief), and cannot cheaply serve the reference's
paired `frame_count_in`/`out`/`delta` shape (`LinkStats` keeps one
post-dequeue counter, not a push/pop pair). Neither blocks shipping
`graphmonitor`/`agraphmonitor` themselves — both filters run and draw a
genuinely useful (if incomplete, and permanently non-framecrc-comparable
per the family's own font ceiling) picture of the graph — so this is
recorded as a known limit of gap 22's scope, not reopened as a new gap.

## Gap 23 — `Stream::r_frame_rate`/`bit_rate` cannot distinguish "no mechanism" from "mechanism declined"

Found investigating a fuzzer-surfaced MP4 mismatch (`planning/CONFORMANCE-FINDINGS.md`
finding 55): a corrupted `stts` (every sample delta zero, `mdhd.duration` intact)
gets `r_frame_rate="1/0"` from the reference — its sentinel for "could not
determine" — while `vaco-probe` computes `"16/1"` from the stream's own valid,
parsed H.264 VUI picture rate.

The mechanism is `crates/app/vaco-probe/src/show.rs`'s `frame_rate()`: when
`Stream::r_frame_rate` is undefined, it falls back to the codec-parsed
(VUI-derived) rate. That fallback is *not* wrong in general — measured
correct on a raw H.264 elementary stream, where the reference also states
the parser's undivided tick rate (`"32/1"`, matching exactly on both sides)
because a raw elementary-stream demuxer has no other source at all. It is
wrong specifically when a container *has* a native per-frame timing
mechanism, ran it, and got an answer of "undefined" on purpose — MP4's own
`stts`-derived estimate in `vaco-demux-mp4` already leaves `r_frame_rate`
undefined for this exact fixture, correctly, while stating `avg_frame_rate`
from the container's total duration (matching the reference on both fields).
`show.rs` cannot tell "no mechanism" from "mechanism ran and declined" apart
from the field's own value — both look like `Rational::UNDEFINED`.

**A same-shaped attempt was tried and reverted.** Gating
`vaco-format-core::discovery`'s packet-mean frame-rate estimate behind "the
container already stated `avg_frame_rate`" (on the theory that a container
which states one field has already had its say about the other) left the
cited MP4 fixture printing `"16/1"` unchanged — proving the estimate was
never the mechanism producing that mismatch — and **regressed ordinary
MPEG-TS video**: `fuzz/seeds/diff/mpegts/h264-aac.ts`'s video stream, which
the reference and our own code already agree on (`r_frame_rate="8/1"`),
started printing `"16/1"` instead, because MPEG-TS video's `avg_frame_rate`
also arrives pre-stated (from the same picture-rate refine pass, ahead of
the mean-delta estimate in packet order) and the change exposed the exact
same `show.rs` fallback bug to a second format. The campaign's aggregate
mismatch count for mpegts did not move either way (`agree=40`/`mismatch=1435`
before and after, in a 1500-iteration `--rng-seed 42` run) — the regression
was invisible to the file-level tally and would have shipped undetected
without a direct fixture comparison against a pristine `HEAD` build. Reverted
in full; `vaco-format-core` is byte-identical to `HEAD`.

**`bit_rate` was checked for the same shape and does not have it** (dispatch
5, same finding 55 investigation). `Stream`-level `bit_rate` for MPEG-TS AAC
audio is missing entirely on our side (`ours="N/A"`) on every tested file,
mutated or not, while the reference always states one — there is no
"declines and states a sentinel" case observed for `bit_rate` the way there
is for `r_frame_rate`'s `1/0`. MP4 is unaffected: its `esds` box states an
explicit per-stream bit_rate, which both sides read directly rather than
estimate, so this is specific to headerless-for-this-purpose formats like
MPEG-TS. Not a fix candidate in this gap — recorded because it was checked
and ruled out, per the same "say what you measured" standard as the rest of
this entry.

**Closing this gap needs a per-stream signal — likely a third state, "this
value was actively determined to be undecidable" — that reaches from
whichever demuxer or estimator has an opinion, through `Discovery<D>`,
to `show.rs`'s display fallback**, distinguishing it from the plain "nothing
has touched this field yet" starting state both currently share as
`Rational::UNDEFINED`/`0/0`. This is wrapper-forwarding-scale work — the
project has already been bitten three times by a new addition not reaching
through a wrapper (`Box<dyn Muxer>`, `MappedFilter`, `AsDecoder`; see finding
55) — and plumbing it through every demuxer that currently relies on
`show.rs`'s blanket codec-parser fallback (at minimum every raw
elementary-stream format, which needs the fallback to keep working) is wider
than one fuzzer-found mismatch justifies fixing alone.

Not fixed here: `vaco-probe` continues to print `"16/1"` for the corrupted-MP4
`r_frame_rate` fixture where the reference prints `"1/0"`, recorded as an
accepted, precisely diagnosed divergence rather than worked around.

## Gap 24 — no adapter for a 2-in/2-out filter with a feedback loop (`feedback`, `vaco-filter-overlay`) — PARTIALLY CLOSED 2026-08-28 (adapter shape added; `feedback` itself still blocked, see gap 25)

`ffmpeg -filters` lists `feedback` as `VV->VV`: two inputs and **two
outputs**. The reference's own use (`[0][fb]feedback[out][fb]`) loops one
output back as the filter's own second input on the next frame — a
genuine cycle in the filtergraph, not just an unusual arity.

Checked every adapter in `vaco-filter-core::adapt` before concluding this,
the same way gap 10's own report checked before it turned out `overlay`
already proved the capability existed: `Simple`/`Blocked` are 1-in-1-out,
`Sourced` is 0-in-1-out, `Fanout` (gap 10) is 1-in-*N*-out, `Paired`
(also gap 10) is *N*-in-**1**-out. None is 2-in-2-out, and none of gap
10's own generalisations (`Paired` past two inputs, `Fanout`'s dynamic
output count) reach this shape by composing them — the missing dimension
is a second *output* pad on a filter that also has more than one input,
which nothing in this tree produces today.

Whether the feedback *loop itself* (a link whose source is downstream of
its own destination) is separately expressible in `vaco-filter-graph`'s
graph construction is a second, unexplored question — this entry is about
the adapter shape only, found first and blocking before the loop
question could even be tested.

`crates/filter/vaco-filter-overlay`'s `feedback` is not implemented for
this reason. Not worked around inside that crate, per the standing rule.


### Status, 2026-08-28 — `Dual`/`DualFilter` adapter added; `feedback` itself still blocked on a cycle, filed as gap 25

Closed for the adapter-shape question specifically: `crates/filter/vaco-filter-core/src/adapt.rs`
gained `DualFilter` (a trait for a fixed-arity multi-in/multi-out filter
body) and `Dual<F>` (the `Filter` adapter), built by combining `Paired`'s
lockstep-input rule with `Fanout`'s all-outputs-have-room backpressure
rule, plus one genuinely new piece neither existing adapter needed: a
pending queue per output pad, since neither `Paired` (one output) nor
`Fanout` (one input) had more than one of both. Like `Paired`,
`DualFilter::input_count`/`output_count` default to two (`feedback`'s own
arity) but may be overridden at construction, generalising to N-in/M-out
the same low-cost way `Paired` already generalises past two inputs — not
a hardcoded-two adapter, just one with a single, two-and-two consumer so
far; D19 argues against speculative new capability, not against reusing a
pattern this crate already had.

Verified with real `Graph`-based tests in `tests/graph.rs`
(`dual_routes_each_output_to_its_own_pad`, using a filter that swaps its
two inputs onto its two outputs specifically to catch cross-pad routing
bugs, and `dual_stops_at_the_first_input_to_run_dry`, mirroring `Paired`'s
own lockstep test), including a deliberate revert: the per-pad routing in
`Dual::activate()` was temporarily hardcoded to always target output pad
0, which made both tests fail with a clear value mismatch, before
restoring the correct per-pad routing and reconfirming both pass. This is
the adapter's forwarding-trap test, in the runtime-failure half; the
compile-failure half does not apply here the way it did for gap 19's
`Box<dyn Muxer>`/`AsDecoder`/`Box<dyn Decoder>` cases, because `DualFilter`
is a wholly new trait implementing the pre-existing `Filter` trait
directly — the same shape as `Paired`/`Fanout` (gap 10), which did not
need such a test either, rather than a defaulted method silently
unforwarded by an existing pervasively-boxed wrapper.

**The adapter is necessary but not sufficient for `feedback`, exactly as
hypothesized before this was checked.** `feedback`'s reference usage
(`[0][fb]feedback[out][fb]`) loops its second output back to its own
second input — a genuine cycle in the filtergraph. `Graph::configure()`
requires `Graph::topological_order()`, which hard-rejects any cycle with
`Error::InvalidData("filtergraph contains a cycle")` before a `Dual`
-shaped node's two inputs and two outputs ever get negotiated. Proven
directly by reading `topological_order()`'s cycle-detection and by a new
end-to-end test, `a_link_back_into_the_same_node_is_rejected_as_a_cycle_at_configure`
(connects a node's own output pad back to its own input pad, asserts
`configure()` returns an `Err` whose message contains "cycle"),
corroborated by a pre-existing, independent test already in the file,
`a_cycle_is_detected`.

This is a second, separate limitation from the one this gap was filed
for — the adapter *shape* gap is closed; the *cyclic graph negotiation*
gap is not, and is filed as gap 25 below rather than folded into this
entry, since fixing gap 25 needs no more `Dual`.

**Not fixed here**: `crates/filter/vaco-filter-overlay`'s `feedback` is
still not implemented — the adapter it needs now exists, but its whole
purpose requires the loop that `Graph::configure()` refuses, so wiring it
up as a non-looping stub would not be `feedback`. Not worked around, per
the standing rule. The overlay family remains 7 of 8, not 8 of 8: the
condition floated for reopening #111's thread ("if `feedback` becomes
implementable") was checked and did not hold.
## Gap 25 — `vaco-filter-core`'s scheduler cannot negotiate or run a cyclic filtergraph

`Graph::configure()` (`crates/filter/vaco-filter-core/src/sched.rs`) calls
`Graph::topological_order()` as part of its negotiation pass, and that
function hard-rejects any cycle with `Error::InvalidData("filtergraph
contains a cycle")`. Found closing gap 24: adding a 2-in/2-out adapter
(`Dual`) was necessary but not sufficient to wire `feedback`, because
`feedback`'s reference usage is not merely 2-in/2-out but self-referential
— one output pad's link terminates back at the same node's own input pad
— and that is a cycle by construction, independent of any adapter shape.

Two separate things would need to give for a cyclic graph to run, neither
explored past this scoping pass:

- **Negotiation.** Format negotiation (`vaco-filter-graph`'s constraint
  propagation, ahead of `configure()`) reasons from sources toward sinks;
  a cycle has no source side to reason from without either breaking the
  cycle at some default/declared format or accepting an under-determined
  first iteration and re-negotiating after frames start flowing. Nothing
  in this tree does the latter today.
- **Scheduling.** The `activate()` driver takes each `Node`'s `filter` out
  of the arena with `.take()` while it runs it (so it can hand out a
  `FilterContext` without aliasing `self`); a self-loop means the same
  node's own output write would need to reach a link whose destination is
  itself, while its own `filter` slot is already taken — not obviously
  safe against the existing borrow structure, and not checked further
  here.

Closing this is substantially more than gap 24 was: it is a scheduling
and negotiation change to the core cooperative scheduler, not a new
adapter. Recorded as its own gap rather than reopening gap 24, since gap
24's own question (is there an adapter shape for 2-in/2-out) is answered
and closed on its own terms.

Not fixed here: `feedback` remains unimplemented in `vaco-filter-overlay`
for this reason, on top of gap 24's now-resolved adapter question.

## Gap 26 — `initial_padding` conflates "publicly declared decoder priming" with "internal pts-accounting offset"

Found closing most of GitHub issue #646 (Ogg/Vorbis `sample_fmt`/
`extradata_size`/`duration`, all fixed in `vaco-demux-ogg`). `start_pts`
would not close the same way.

Vorbis's first packet is genuinely negative in this crate's own pts
accounting — `GranuleMapping::initial_cursor`'s doc comment already names
it "the first packet's own priming" and treats it exactly like Opus's
`pre_skip` for that purpose. The reference agrees the offset is real
(`start_pts=0`, not the raw negative first pts) but, measured directly,
reports `initial_padding=0` for Vorbis — unlike Opus, where `pre_skip` is
both the pts-accounting fix *and* the publicly reported decoder-priming
sample count from the same header field.

`vaco-format-core::discovery`'s `finish()` has exactly one lever for this
correction: `stream.start_time = first_pts.offset(pad_ticks)`, where
`pad_ticks` comes from `stream.params.audio.initial_padding` — the same
field `vaco-probe` prints as `initial_padding`. Two fix attempts were tried
and both rejected by measurement:

- Setting `AudioParameters::initial_padding` to Vorbis's nominal block size
  reaches `discovery.rs`'s existing compensation and gets `start_pts`/
  `start_time`/`format.start_time` all correct — but also makes
  `initial_padding` itself wrong (`1024` where the reference states `0`),
  swapping one mismatch for another rather than closing one.
- Setting `Stream::start_time` directly from within `vaco-demux-ogg`,
  before `Discovery` ever sees the stream, does nothing:
  `Discovery::new` snapshots `inner.streams()` once at construction and
  never re-reads the wrapped demuxer's own state afterward
  (`Demuxer for Discovery<D>::streams()` returns `&self.streams`, Discovery's
  own copy) — a value set on the *inner* `OggDemuxer`'s stream, however
  correct, is invisible to the pipeline `vaco-probe` actually runs. This is
  the same "wrapper does not forward a new addition" shape finding 55
  already names for `Box<dyn Muxer>`, `MappedFilter` and `AsDecoder`,
  encountered here for the first time on `Discovery<D>` itself.

Closing this properly needs `discovery.rs`'s compensation to read a value
distinct from the publicly-reported `initial_padding` — an internal-only
pts offset a demuxer can state without also claiming a decoder must discard
that many samples. That is a new field or a new convention on a type
several formats already depend on (at minimum every consumer of
`AudioParameters`/`Stream::start_time`), which is wider than one format
crate's fix and exactly the caution finding 55 already gives for touching
`vaco-format-core::discovery` on the strength of one format's mismatch.

Not fixed here: `vaco-demux-ogg` still reports `start_pts`/`start_time` as
the raw, uncompensated negative value for Vorbis, unchanged from before
this investigation.

## Gap 27 — `vaco_protocol_core::Protocol` has no directory-creation verb

Found building `vaco-mux-smoothstreaming` (epic #75, issue #617). Real
`ffmpeg -f smoothstreaming` writes each `QualityLevel`'s fragments under its
own `QualityLevels(<bitrate>)/` subdirectory, created fresh alongside the
`Manifest` — measured directly: running it against an empty output
directory produces the subdirectories with no separate mkdir step from the
caller.

`vaco-format-adaptive::WriteAccess` and the `Protocol` trait it wraps
(`crates/io/vaco-protocol-core/src/protocol.rs`) expose `open`/`create`/
`check`/`list_dir`/`delete`/`rename` and nothing that creates a directory.
`vaco-protocol-file`'s own `create` (`crates/io/vaco-protocol-file/src/file.rs`)
opens the target path directly via `OpenOptions::create(true)` with no
parent-directory handling, so `WriteAccess::create` on a path whose parent
does not yet exist fails with the underlying `NotFound` I/O error.

`vaco-mux-dash` and `vaco-mux-hls` never hit this: both name every segment
file flat, in the same directory as the manifest
(`init-stream$RepresentationID$.m4s`, `chunk-stream$RepresentationID$-$Number$.m4s`),
which reads as a deliberate design choice in hindsight rather than proof
this gap does not exist — Smooth Streaming's `QualityLevels(<bitrate>)/`
layout is the first multi-file format in this workspace whose own naming
convention requires a subdirectory.

Not fixed here: `vaco-protocol-file` is owned and closed (`agent:protocols`,
`planning/ASSIGNMENTS.md`), so adding a `create_dir`/`mkdir` verb to
`Protocol` and wiring it through `WriteAccess` is out of scope for a crate
I do not own (D11). `vaco-mux-smoothstreaming`'s own test suite works
around it locally (pre-creating the two `QualityLevels(<bitrate>)/`
directories before exercising the muxer against a `file:` output), and its
module docs name the gap rather than silently relying on the workaround
generalizing. A real caller (`vaco-cli` or an equivalent orchestrator)
driving this muxer against local output needs the same pre-creation step,
generically, for any future subdirectory-shaped multi-file format — the
natural complete fix is a `Protocol::create_dir` default-`Unsupported`
method, implemented for `file:` (and left unsupported for e.g. `http:`,
where a PUT target's directory structure is the server's concern, not the
client's).

**Checked, not assumed, for `vaco-mux-hds` (issue #618)**: this gap does
not apply there. Measured directly with a two-quality-level `ffmpeg -f hds`
reference tree — every file HDS writes (`index.f4m`, each `stream<N>.abst`,
each `stream<N>Seg1-Frag<M>`) sits flat in the manifest's own directory,
the same flat convention `vaco-mux-dash`/`vaco-mux-hls` already use. Smooth
Streaming's `QualityLevels(<bitrate>)/` layout remains, so far, the only
format in this workspace that actually needs `Protocol::create_dir`.

## Gap 28 — no protocol in this workspace owns a socket outright and drives its own clock; SRT needs both, and a proposed seam that needs no `vaco-protocol-core` change

Found scoping `vaco-protocol-srt` (epic PR-10, issue #62, split into #555/
#556/#557). Every protocol registered today is one of two shapes:
request/response over a stream one side opens and the other replies on
(`http`, `ftp`, RTMP's future registration), or a plain duplex byte pipe
handed to the caller once connected (`tcp`, `tls`). `Protocol::open`/
`create` (`crates/io/vaco-protocol-core/src/protocol.rs`) return a
[`vaco_io::MediaSource`]/[`vaco_io::MediaSink`] — one direction, driven
entirely by the caller's own `read`/`write` calls, with no notion of work
happening between them.

SRT does not fit either shape. It is UDP with its own reliability layer:
one socket carries data packets and control packets (handshake, ACK, NAK,
keepalive, shutdown) in both directions at once, and correctness depends
on **wall-clock timers that fire whether or not the caller is calling
`read`/`write`** — periodic ACKs, NAK-triggered retransmission, a latency
window that drops packets that missed their deadline. A `MediaSource`
whose `read` only does work when called cannot service a timer that must
fire during a gap in reads. `vaco-protocol-dial`'s "complete a duplex
round trip, then hand back a stream" shape (built for RTMP-style
handshake-then-stream protocols) does not apply either — SRT never hands
back a stream at all; the packet/timer layer runs for the connection's
entire life.

### The proposed seam: contained inside `vaco-protocol-srt`, not a core change

**`vaco-protocol-wrap`'s `async:` protocol (`src/asyncproto.rs`) already
solves the adjacent problem** — a worker that must do time-driven work the
caller's synchronous `read` calls do not control — for background
read-ahead, and its own module docs record the precedent this follows:
`vaco-sched::driver::run_threaded` spawns `std::thread::spawn` inside a
`#[cfg(not(target_family = "wasm"))]` item, with the *same* public API
working (serially, not ahead) on wasm, exactly what D18 asks for
("parallelism optional at the API level") and exactly what
`xtask/src/time_gate.rs`'s own `FORBIDDEN` table sanctions
(`std::thread::spawn` → "a driver the caller supplies (D18)", not a
finding). `AsyncSource`'s `Threaded` backend — one worker owns `inner`
outright, streams it over a bounded `mpsc` channel, and a second
single-slot command channel carries requests back — is the shape to
extend, not a novel design:

- A worker thread (native-only; see below) owns the raw UDP socket via
  `vaco-protocol-socket`'s existing `socket2`-backed primitives (`udp.rs`
  already preserves datagram boundaries — one `recv` per `read` — which is
  exactly the framing SRT's own packet parser needs) and runs the actual
  engine: handshake, ARQ, congestion control, encryption — everything
  #555/#556 build. It drives its clock through `vaco_time::{Instant,
  sleep}`, never raw `std::time`/`std::thread::sleep` (time-gate).
- The worker talks to a foreground handle over channels: reassembled,
  in-order, decrypted application payload flows out on one channel (a thin
  `MediaSource` impl just pulls from it); application payload to send
  flows in on another (a thin `MediaSink` impl just pushes to it, and the
  worker packetises/encrypts/schedules it). All ACK/NAK/keepalive/timer
  traffic is internal to the worker and invisible to both traits.
- `Protocol::open`/`create`'s existing split already matches SRT's actual
  application-facing shape: a caller reading `srt://` wants a
  `MediaSource`, a caller writing one wants a `MediaSink` — the fact that
  the protocol is bidirectional *underneath* (ACKs flow opposite to data
  regardless of which way the application uses it) is exactly the kind of
  internal-to-the-worker fact this seam is for.
- **Wasm is not actually a new question here**: `wasm32-unknown-unknown`
  has no raw UDP socket at all, the same reason `vaco-protocol-http` is
  already the one `NATIVE_ONLY` entry in `xtask/src/wasm.rs` ("a different
  protocol implementation behind the same `vaco-protocol-core` trait...
  not a hole in the design"). `vaco-protocol-srt` is native-only from the
  socket layer up, so it does not need `asyncproto`'s serial-fallback path
  at all — one `NATIVE_ONLY` entry is the whole story.

This needs no change to `vaco_protocol_core::Protocol`, `vaco_io::
MediaSource`/`MediaSink`, or any trait another protocol depends on — the
entire answer lives inside one new crate, using primitives and a threading
pattern this workspace has already reviewed and sanctioned once. Recorded
as a gap anyway because it is the first UDP/timer-driven protocol in this
tree (RIST, PR-11, is next in line for the identical shape) and the next
person should not have to re-derive that `Protocol::open`/`create` do fit,
just not directly.

### Not load-bearing for #555 itself

#555's own scope — packet framing, the handshake state machine (caller/
listener/rendezvous), encryption negotiation — is sans-io: header
serialisation/deserialisation and state transitions over byte slices, no
socket, no thread, mirroring both `vaco-protocol-rtmp`'s own PR-09a
("a transport-framing library, not yet a `Protocol`... the building
blocks that package will call") and the plan's own note that
`srt-protocol` (the suggested accelerant/oracle, not adopted) is
"sans-io... no I/O ownership, since we drive the socket." The worker-
thread seam above becomes load-bearing once a later child wires this
engine to a live socket and registers `srt:`/`srts:` — not before.


## Gap 29 — `Frame`/`FrameFlags` had no shape for MPEG pulldown's repeat count — CLOSED 2026-08-28

Found independently from two directions, which is what made it actionable
rather than one crate's convenience: `vaco-codec-mpeg12`, decoding
`picture_coding_extension()`'s `repeat_first_field` bit, had nowhere to
put the result; `vaco-filter-deinterlace`'s `repeatfields.rs` had already
documented, from the consuming side, that it needs exactly this signal
(`ffmpeg`'s own `AVFrame::repeat_pict`) and has nothing to read because no
decoder in this workspace could produce it.

**Not a `FrameFlags` bit.** `top_field_first`/`repeat_first_field`
together select how many field periods a picture is held for (H.262
§6.3.10: one, two or three progressive frames, or two or three fields,
depending on `progressive_sequence`/`progressive_frame`) — a count, not a
boolean, and the two existing flags (`INTERLACED`, `TOP_FIELD_FIRST`) do
not extend to one.

**Not a new field on `Frame` either.** Verified before choosing a shape,
not assumed: `Frame`'s fields are deliberately public and unopinionated
(`vaco-frame/src/alloc.rs`'s own module doc — "the fields stay public and
a struct literal still works") and 88 files in this tree write `Frame {
...}` literals directly; `Frame` has no `Default` impl, so none of them
tolerate a new required field without every one being found and edited. A
tree-wide breaking change to a layer-1 model crate while other agents are
actively committing is not a small edit.

**The existing side-data mechanism was already the right seam.**
`FrameSideData` and `FrameSideDataKind` are both `#[non_exhaustive]`
specifically so a new variant is additive — every match outside this
crate already needs a wildcard arm, confirmed by building the whole
workspace (`cargo check --workspace`) both before and after adding the
variant, zero errors either time. Added `FrameSideData::Pulldown(u8)` (the
extra field-period count, `0` meaning no repeat and normally represented
by the entry's absence, not a stored `0`) plus `FrameSideDataKind::
Pulldown`, following the exact `Cropping`/`Crop::set_crop` pattern already
established: `Frame::repeat_pict()`/`Frame::set_repeat_pict()`.

**The value is pre-combined, not a raw bit**, matching what the
consuming side (`repeatfields.rs`, and the reference's own
`AVFrame::repeat_pict`) actually wants: the decoder that has
`progressive_sequence` (sequence-level), `progressive_frame` and
`repeat_first_field`/`top_field_first` (picture-level) all in hand
computes the final field-period count itself, so a filter reads one
already-resolved number instead of re-deriving H.262's three-way
combination rule. `vaco-codec-mpeg12`'s `pulldown_extra_fields` is the
first (and so far only) producer, verified against the primary text
directly, covered by a table-driven unit test for all three combination
cases plus the full `begin_picture` wiring.

### Status, 2026-08-28

Closed for the interface question: `FrameSideData::Pulldown`/
`FrameSideDataKind::Pulldown`/`Frame::repeat_pict`/`Frame::
set_repeat_pict` landed in `vaco-frame` (`crates/model/vaco-frame/src/
lib.rs`, `sidedata.rs`), `vaco-codec-mpeg12` is the first producer, and
`cargo check --workspace` confirms zero breakage across every existing
`Frame`-constructing site. `vaco-filter-deinterlace`'s `repeatfields.rs`
is not wired to read it yet — that filter's own crate would need its own
change to consume `Frame::repeat_pict`, which this entry does not make
(different ownership, and `repeatfields.rs`'s "hard-duplicate a field"
logic is a separate piece of work from the data now being available to
read).

### Addendum — #556 (PR-10b) stays sans-io too, via an explicit `tick()` entry point

Checked before starting #556 (congestion control, ACK/NAK, retransmission,
the latency window): retransmission and periodic ACKs are exactly the
timer-driven property this gap names, so the question was whether #556 is
the child that finally forces the worker-thread seam into existence.

It is not, and does not need to be. The standard sans-io answer for a
timer-driven protocol — used by, among others, QUIC implementations that
deliberately keep their state machine free of any socket or clock — is two
entry points instead of one: `on_packet(data, now)` for network input, and
an explicit `on_tick(now)` a driver calls on its own cadence for
timer-driven work (periodic ACKs, RTO-triggered retransmission, latency-
window drops). Both take time as a plain argument rather than reading a
clock internally, so #556's own ARQ/congestion-control logic stays exactly
as pure and unit-testable as #555's handshake state machines, and the
worker-thread seam this gap already designed is still exactly where
`on_tick` gets called from, on a real interval, once a live socket exists —
unchanged by #556 landing first.
