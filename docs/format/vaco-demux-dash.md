# `vaco-demux-dash`

Layer 4. FM-45. MPEG-DASH demuxer, MPD parsed with `quick-xml`.

## What it is

Parses an MPD, picks a `Representation`, enumerates its segments under
whichever of the three DASH addressing modes it uses, and reads them one at
a time through a nested fMP4 (or MPEG-TS) demuxer.

## How it works

`quick-xml` produces events; [`tree::parse`] turns those into one generic,
bounded [`tree::Node`] tree (namespace prefixes stripped) in a single pass,
and every element-specific parser in [`mpd`] walks that tree afterwards —
far easier to get right under time pressure than a hand-rolled streaming
state machine over `quick-xml`'s push events, at the cost of holding the
(already bounded) document in memory once. [`mpd::interpret`] turns the tree
into `Period` > `AdaptationSet` > `Representation`, folding
`SegmentTemplate`/`SegmentList`/`SegmentBase` inheritance (a
`Representation`'s own addressing overrides its `AdaptationSet`'s).
[`segments::enumerate`] turns one representation's addressing into an
ordered `DashSegment` list — the point where the three XML shapes stop being
different and become one list, the same shape `vaco-demux-hls` already uses.

### `SegmentTimeline`'s `@r`, the fiddly part named in the brief

Parsing `<S t d r>` into `vaco_format_adaptive::TimelineEntry` is this
crate's job; **expanding** the run-length encoding — including `r="-1"`
("until the next `<S>`'s `@t`, or until the period ends") — is
`vaco_format_adaptive::timeline::expand`'s, and its own proptest is what
actually exercises the arithmetic. This crate supplies `period_end` from
whichever of `Period/@duration` or `MPD/@mediaPresentationDuration` is
stated; a dynamic (live) MPD with neither enumerates that entry to zero
segments rather than guessing.

### Continuity is free here, unlike HLS

`vaco-demux-hls` re-times packets across `#EXT-X-DISCONTINUITY` because a
raw MPEG-TS segment's own clock is not guaranteed continuous with its
predecessor. A correctly-authored fMP4/CMAF segment states its own absolute
`tfdt` base decode time, so a representation's segments are already
continuous without this crate doing anything — [`DashDemuxer::read_packet`]
does not rebase timestamps at all.

## What this crate does not do

- **Only the MPD's first `Period` is read.** See the crate's top-level doc
  comment for why (it reduces to the same "shift a nested demuxer's native
  timestamps onto one continuous outer timeline" problem `vaco-demux-hls`
  already solves for discontinuities, not attempted here this wave).
- **`SegmentBase`'s `sidx` is not parsed** — reported as one whole-file
  segment rather than one segment per index entry. Still a correct read.
- **A dynamic MPD with no `SegmentTimeline` and no period duration
  enumerates to zero segments** — live-edge computation needs the wall
  clock, out of scope per plan 13 §1b.
- **`ContentProtection` is detected and reported, never decrypted** — see
  `mpd::ContentProtectionInfo::unsupported_error`, which fails
  `read_packet` the moment a protected representation's first segment would
  be opened.

## How to change it

`segments::enumerate` is the seam every addressing-mode change should go
through; the three `enumerate_*` helpers behind it are independent and can
be extended without touching each other.

If you touch `tree::parse`'s event loop, note the entity gotcha:

`quick-xml` reports a `&...;` reference as its own `Event::GeneralRef`
rather than folding it into the surrounding `Text`, so a `match` that
handles only `Text` silently drops every entity *and* splits the text run
around it. That is why the loop carries a `GeneralRef` arm and why the
reader's own `trim_text` is off: trimming per event would eat the
whitespace on either side of an entity, so each element's text is trimmed
once, when the element closes.

## Configuration

`DashOptions { max_bandwidth: Option<u64> }`.

## Dependencies

`quick-xml` (the only MPD dependency; all interpretation is this crate's
own), `vaco-format-adaptive`, `vaco-protocol-core` (never a concrete
protocol crate), `vaco-format-core`, `vaco-io`, `vaco-limits`,
`vaco-packet`, `vaco-codec-core`. Dev-only: `vaco-demux-mp4`/`vaco-mux-mp4`
and `vaco-protocol-file`.
