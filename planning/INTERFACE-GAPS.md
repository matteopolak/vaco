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

## 12. `BsfProvider::open` carries no per-instance option string

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

## 13. `vaco_frame::FrameSideData` has no console-log-only output channel

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

## 14. `vaco_frame::FrameSideData` has no motion-vector variant

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

## 15. `vaco-filter-color`'s `sample` module cannot address float pixel formats

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


## 17. No decoder in this workspace can produce a subtitle: `FrameData` has no `Subtitle` variant, and `Decoder` is fixed to return `Frame`

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

## 18. Nothing in this workspace populates `FrameSideData::ClosedCaptions`

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

### Shape

Additive: an SEI/user-data extraction step in the relevant video parser(s)
that appends the raw `cc_data` triplet bytes (the same three-byte-per-triplet
shape ffmpeg's own `a53_caption` side data carries, and the shape
`vaco-codec-subtitle-cc`'s decode API already takes as input, precisely so
this gap does not block *implementing* the decoder, only *reaching* it end to
end) into `FrameSideData::ClosedCaptions`. Not attempted here: it is a change
to the H.264/HEVC/MPEG-2 parser crates, all outside this scope.

**Blocks:** end-to-end CEA-608/708 decode from a real broadcast file. Not
blocking `vaco-codec-subtitle-cc`'s own correctness, which is verified
directly against hand-built (and, where obtained, extracted) `cc_data`
bytes.
