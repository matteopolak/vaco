# vaco-filter-denoise

T4 video denoise filters (FT-4.6b, GitHub issue #469): `hqdn3d`, `atadenoise`,
`removegrain`, `nlmeans`, `owdenoise`, `dctdnoiz`, `fftdnoiz`,
`vaguedenoiser`. `bm3d` is named in the same reference group but is **not
implemented** here — see [Scope: `bm3d`](#scope-bm3d-not-implemented) below.

## Group membership, checked rather than assumed

The brief handed to this work package listed nine names as "the reference's
denoise family" and explicitly warned not to trust that list in either
direction. Checked directly against the shipped reference
(`ffmpeg -hide_banner -filters`, ffmpeg 8.1, 2026-08-23, recorded in
`provenance/sources.toml`'s `ffmpeg-denoise-filters-probe` entry) rather than
trusted: filtering that output to rows whose description mentions denoising
gives exactly nine filters — `atadenoise`, `bm3d`, `dctdnoiz`, `fftdnoiz`,
`hqdn3d`, `nlmeans`, `owdenoise`, `removegrain`, `vaguedenoiser`. That is the
brief's list **exactly** — nothing to add, nothing to drop. (`afftdn` and
`afwtdn` also match "denois" in their description but are `A->A` audio
filters, not `V->V`/`N->V` video ones, and belong to a different work
package.)

## What it is

One module per filter (`src/<name>.rs`), each exposing `pub const DESC:
FilterDesc` and a crate-private `fn create`, aggregated by
[`registry::DenoiseRegistry`](../../crates/filter/vaco-filter-denoise/src/registry.rs)
— the same shape `vaco-filter-audio-eq` and `vaco-filter-video-geometry` use.
`src/video.rs` is the shared plane-decode/encode helper ([`PlaneBuf`], an
`f32` view of one plane) every filter is built on. `src/wavelet.rs` is the
à trous wavelet transform and coefficient-shrinkage engine shared by
`owdenoise` and `vaguedenoiser`.

## How it works

### Pixel format coverage

Every filter processes the planar `grayN`/`yuvN` family at any bit depth up
to 16, via `video::sample_layout` — exactly one component per plane,
byte-aligned, host-little-endian, none of `BITSTREAM`/`HW_ACCEL`/
`PALETTE`/`FLOAT`/`BAYER`. Semi-planar (`nv12`) and packed (`rgb24`) formats
are out of scope: a filter asked to process one returns
`vaco_core::Error::Unsupported` rather than silently miscomputing (see
`video.rs`'s module doc for the exact test, `sample_layout`).

### Correctness discipline: independent oracles, not byte-identity

None of these eight filters are checked byte-for-byte against the reference.
Recovering the reference's exact algorithm for any of them would mean either
reading FFmpeg's source (closed by D7's clean-room policy) or a black-box
bisection well beyond this work package's time budget. Instead, every filter
implements a **real, independent denoising algorithm** — derived from the
option table's documented semantics and, where a named public algorithm
exists, from that algorithm's own paper (never from FFmpeg's source) — and is
checked against a property the correct algorithm *must* have, not against a
second transcription of the same formula:

| Filter | Independent oracle |
|---|---|
| `hqdn3d` | Flat-field invariant (every neighbour/history diff is `0`, so every blend weight is a no-op); noise-power bound on synthetic noisy planes, both spatial and temporal |
| `atadenoise` | Exact algebraic identity: when every history sample is guaranteed full weight, the weighted average *is* the plain arithmetic mean — checked exactly, not as a statistical tolerance |
| `removegrain` | Every mode's output is provably within `[min(neighbours), max(neighbours)]`; an outlier planted in an otherwise-uniform 3x3 block must clip back to exactly that uniform value, for every mode `1..=24` |
| `nlmeans` | Flat-field invariant (identical patches -> equal weights -> average of identical values); noise-power bound |
| `owdenoise`, `vaguedenoiser` | [`wavelet::Decomposition`]'s own oracle carries through: a constant plane has exactly zero detail at every level (a textbook wavelet fact), so thresholding is a no-op; noise-power bound with a non-trivial threshold |
| `dctdnoiz` | DC-coefficient preservation (a block's mean is untouched by any AC-only thresholding — a property of the DCT basis, not of this file's code) and a flat-block invariant; the DCT/IDCT round-trip is checked against the textbook DCT-II/DCT-III definitions |
| `fftdnoiz` | The hand-written DFT is cross-checked against `rustfft` (an independently-authored crate, dev-dependency only) on random input; flat-block invariant and noise-power bound for the denoising step itself |

### Filter-by-filter notes and documented simplifications

* **`hqdn3d`**: probed directly that the printed `AVOption` default of `0`
  for all four strength options is **not** what the reference actually
  applies — `hqdn3d` with no arguments and `hqdn3d=0:0:0:0` produce
  identical, non-trivial `framecrc` output, both different from no filtering
  at all. The reference substitutes its own built-in defaults whenever an
  argument is `0`/omitted. This implementation takes `0` literally (a
  genuine zero strength disables that pass) — recovering the reference's
  exact substituted constants would need either its source or a much larger
  bisection. See `hqdn3d.rs`'s module doc for the full `framecrc` evidence.
* **`atadenoise`**: uses a **trailing** window (frame `N`'s output averages
  the last `min(s, N+1)` input frames including itself) rather than the
  reference's **centred** window, trading frame-alignment fidelity for zero
  added latency and no special-cased stream edges. The `a` (parallel/serial)
  algorithm-variant option is parsed but not distinguished.
* **`removegrain`**: modes `1..=7` are a rank-order clip to
  `[sorted[mode-1], sorted[8-mode]]` of the 8 neighbours — inspired by, but
  not a transcription of, AviSynth's `RemoveGrain` per-mode formulas (several
  of which weight by distance to the centre, which this does not model).
  Modes `8..=24` are real `RemoveGrain` mode numbers this crate has not
  transcribed; `create` rejects them with a named error rather than
  silently running mode `7`'s clip (a real accepted-value substitution
  until this pass — see `removegrain.rs`'s own doc).
* **`dctdnoiz`**/**`fftdnoiz`**: non-overlapping block tiling, no
  overlap-add (`overlap` is parsed, has no effect — visible as mild
  blockiness at high `sigma`, harmless to the stated oracles).
  `dctdnoiz`'s `expr` and `fftdnoiz`'s `window`/`prev`/`next` are parsed and
  have no effect (rectangular window, spatial-only).
* **`owdenoise`**: decomposition depth is a fixed constant (3 levels) since
  the option table exposes no "levels" knob for it (unlike
  `vaguedenoiser`'s `nsteps`); `depth` (`8..=16`) is parsed but unused, since
  this crate already computes in `f32` regardless of the plane's bit depth.
* **`vaguedenoiser`**: `type=universal` scales the textbook VisuShrink
  formula by the user's `threshold` option rather than replacing it (a fixed
  threshold with no noise-dependence at all would make the option
  pointless); `type=bayes` uses BayesShrink per band.

## Scope: `bm3d` not implemented

Left out deliberately, for reasons specific to `bm3d` rather than shared by
the other eight:

* It is `N->V`, not `V->V` — `ref=true` takes a second reference/basic-estimate
  input stream, a pad shape none of `vaco-filter-graph`'s fixed-count pad
  helpers cover, and getting that negotiation right is its own unit of work.
* The algorithm (block matching, a 3D joint transform, collaborative
  hard-thresholding or Wiener filtering, weighted aggregation, run as a
  two-pass `estim=basic` then `estim=final` pipeline per Dabov, Foi,
  Katkovnik & Egiazarian 2007) is substantially larger than the other eight
  filters combined.

Per the brief's own guidance ("if a filter is genuinely out of reach,
implement the rest ... and say clearly which ones you left and why"), `bm3d`
is that filter. See `src/bm3d.rs`'s module doc.

## How to change it

* **Add a filter**: one new module exposing `pub const DESC: FilterDesc` and
  `pub(crate) fn create(&Instantiate) -> Instance`, a `NAMES` entry and match
  arm in `registry.rs`, a `pub mod` line in `lib.rs`, and a `[[component]]`
  row in `vaco-component.toml` (run `cargo xtask gen-registry` after).
* **Extend an existing filter's fidelity**: every module's doc names exactly
  which options are implemented and which are parsed-but-inert. Look there
  first — a "structural gap" note is the map of what is safe to extend
  without re-deriving the whole module.
* **Gotcha**: `PlaneBuf` (in `video.rs`) is the *only* place pixel bytes are
  decoded/encoded. A filter that reads a `Plane` directly instead will not
  get the depth-aware clamping `PlaneBuf::write` provides, and will silently
  misbehave on `>8`-bit formats.

## Configuration

No crate-level configuration; every option is per-filter, parsed from the
filtergraph instantiation string via `vaco_filter_graph::registry::Instantiate`.
See each module's doc comment for its option table (name, range, default) as
probed from `ffmpeg -h filter=<name>`.

## Dependencies

`vaco-core`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph` — all internal. `rustfft` is a **dev-dependency only**
(an independent FFT oracle for `fftdnoiz`'s tests, never linked into the
shipped filter itself, which uses its own hand-written DFT — see
`fftdnoiz.rs`'s module doc).
