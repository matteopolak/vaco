# vaco-filter-key

Keying/masking video filters: `premultiply`, `unpremultiply`,
`maskedmerge`.

**Scope note**: `planning/16-filters.md` §4.2's `vaco-filter-key` row lists
20 filters (`chromakey`, `chromahold`, `colorkey`, `colorhold`, `hsvkey`,
`hsvhold`, `lumakey`, `backgroundkey`, `despill`, `premultiply_dynamic`,
`maskedclamp`, `maskedmax`, `maskedmin`, `maskedthreshold`, `maskfun`,
`threshold`, `hysteresis`, `floodfill`, plus the three implemented here).
Only the three are built — carried over from a prior mis-scoped crate
(GitHub issue #476's `vaco-filter-component`; see that issue for the
correction). The other 17 filters are a separate, not-yet-scheduled unit
of work.

## What it is

Alpha-compositing primitives: multiply/divide colour by alpha
(`premultiply`/`unpremultiply`) and a three-input linear blend
(`maskedmerge`).

## How it works

### `maskedmerge`: measured formula

`out = base + (overlay - base) * mask / maxval`, per selected plane
(`planes` bitmask, default all). Confirmed against the reference with a
concrete probe: base `0x64` (100), overlay `0xc8` (200), mask `0x80`
(128) produced `0x96` (150) — exactly `100 + 100*128/255`. Implemented as
a direct three-input `vaco_filter_core::Filter` (lockstep consumption of
one frame per pad), not through `vaco-filter-framesync`: the reference's
own `-h filter=maskedmerge` exposes no `eof_action`/`shortest` surface,
unlike the framesync-shaped filters elsewhere in this project.

### `premultiply`/`unpremultiply`: what is measured and what is not

Measured: the reference genuinely instantiates an internal `framesync`
(confirmed via `-loglevel verbose`, which prints "Sync level N" lines)
even though no `eof_action`/`shortest`/`ts_sync_mode` option is exposed —
so this crate uses `vaco_filter_framesync::Synced` with
`FrameSyncOpts::default()`, matching the framework's precedent for that
shape. Also measured: a main input with **no alpha channel is an exact
pixel no-op**, for every alpha value tried on the second input.

**Not conclusively pinned down**: the doc string says "PreMultiply first
stream with first plane of second stream", but a `yuva420p` main input's
output bytes were ambiguous between "premultiplied by its own alpha" and
"premultiplied by the second stream's channel". This crate implements the
conservative, testable reading — multiply by the **main input's own**
alpha channel when it has one — and states this as a documented
simplification in `premultiply.rs`'s own doc, not a confirmed match.
`inplace=1`'s single-input shape (measured to exist) is parsed but not
wired to a dynamic pad count.

## How to change it

- A new keying filter from the row above: for a 2-or-3-input filter with
  no framesync options exposed by the reference, follow `maskedmerge.rs`
  (a raw `Filter` impl, lockstep pad consumption); for one that does
  (or that the reference's `-loglevel verbose` shows using framesync
  internally, per `premultiply.rs`'s note), follow `premultiply.rs`
  (`vaco_filter_framesync::FrameSyncFilter`).
- Register in `vaco-component.toml`, run `cargo xtask gen-registry`, and
  add the name to `registry.rs`.

## Configuration

No crate-level configuration; each filter's options are its own
`vaco_opts::Options` struct.

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph`, `vaco-filter-framesync` (for `premultiply`/
`unpremultiply`).
