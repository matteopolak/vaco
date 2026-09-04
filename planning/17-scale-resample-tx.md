# 17 — Signal Processing: `vaco-scale`, `vaco-resample`, `vaco-tx`

Plan of record for the three layer-3 DSP crates. Conforms to `planning/00-decisions.md`
(D2 no-unsafe, D3 licences, D6 differential testing, D7 clean-room, D8 toolchain) and to
`planning/10-architecture.md` §7 (performance architecture, KernelSet dispatch).

Sources used: `planning/research/01-libavutil-swr-sws.md` §§8, 10, 11 (the option/feature
inventories), `planning/research/08-performance-simd.md` (SIMD shape and ranking),
`planning/research/09-dependency-licence-register.md` (dependency verdicts).

**Clean-room provenance.** No FFmpeg source was consulted for this document. Every algorithm
below is specified from published signal-processing literature and from public standards; each
part carries a reference list naming exactly what an implementer works from. The only FFmpeg-derived
inputs are *interface facts* — option names, enum names, documented semantics — which D7 explicitly
permits.

---

## 0. Why these three, and why first

### 0.1 The reward argument

Research §08 §6 ranks the ~15 hottest real-world code paths. Of the eight areas that portable SIMD
can reach cleanly, four live in these three crates:

| §08 rank | Path | Crate | SIMD verdict from §08 |
|---:|---|---|---|
| 9 | swscale colour-space / pixel-format conversion | `vaco-scale` | "near-ideal for `std::simd`, likely one of the best 'start here' areas" |
| 10 | swscale bicubic/bilinear rescaling | `vaco-scale` | workable; the win is coefficient-table layout, not ISA choice |
| 11 | audio resampling + rematrixing | `vaco-resample` | "comparatively low-effort, high-value" |
| 13 | FFT/MDCT | `vaco-tx` | butterflies vectorise; permute/twiddle stages are the risk |

Architecture §7.2 ranks the same areas 1, 2, 3 and 5. That is not a coincidence — it is the same
finding stated twice. These crates are where the project's SIMD story is either proved or disproved.

### 0.2 The risk argument

D2 makes a bet: that safe `std::simd` plus autovectorisation plus PGO can reach parity with
~190k lines of hand-written assembly. That bet is currently unevidenced. These three crates are the
cheapest place to collect evidence, because:

- They are **leaf** crates. `vaco-scale` and `vaco-resample` depend only on layers 0–1;
  `vaco-tx` depends only on `vaco-core` and `vaco-simd`. No codec, no container, no scheduler is
  needed to build or benchmark them.
- They are **individually benchmarkable against the reference binary today** — `ffmpeg -vf scale`,
  `ffmpeg -af aresample` are direct, isolated, single-threaded comparisons (§08 §5 scenario 3 and 4
  explicitly recommend these as fair comparison points).
- Their reference implementations upstream carry a *moderate* amount of assembly (swscale ~16.3k
  lines, swresample ~3.0k, av_tx ~3.2k) versus H.264's 39.6k — so parity is a realistic target,
  not a moonshot.

If portable SIMD reaches parity on colour conversion and resampling, D2 holds and the codec work can
proceed on that assumption. If it does not, we learn it in month three rather than month thirty, and
we escalate per D2's escalation clause with real numbers.

### 0.3 The dependency argument

`vaco-tx` is a hard prerequisite for essentially every audio codec (AAC, AC-3, Vorbis, Opus, MP3,
DTS, ATRAC, MLP-adjacent). `vaco-scale` and `vaco-resample` are hard prerequisites for the `vaco`
binary doing anything other than stream copy, and for `vaco-play`. Nothing downstream of milestone
v0.1 can start without them.

### 0.4 Sequencing

```
v0.1 (probe)  ──▶ M2: DSP foundation ──▶ M3: first codecs ──▶ M4: filters/CLI
                  ├── vaco-simd (substrate)          [prereq, ~3 pw]
                  ├── vaco-checkasm (harness)        [prereq, ~2 pw]
                  ├── vaco-tx          ──────────────▶ needed by every audio codec
                  ├── vaco-resample    ──────────────▶ needed by CLI + play
                  └── vaco-scale       ──────────────▶ needed by CLI + play + filters
```

All three run **in parallel** after the `vaco-simd` / `vaco-checkasm` substrate lands. They share no
code with each other, only with layers 0–1.

### 0.5 Shared conventions across all three crates

These are stated once here and assumed throughout.

**Crate attributes.** Every one of the three carries:

```rust
#![forbid(unsafe_code)]
#![cfg_attr(feature = "simd", feature(portable_simd))]
#![warn(missing_docs, clippy::undocumented_unsafe_blocks)]
```

`portable_simd` is behind a feature only so that a `--no-default-features` build can be compiled by
a stable toolchain for triage; the shipping build always enables it.

**Kernel dispatch (architecture §7.3).** Each crate defines one or more `KernelSet` structs of plain
safe `fn` pointers, resolved **once** at plan/context construction:

```rust
// vaco-simd
pub struct Tier(u16);              // ordered ISA tier, comparable
pub fn detected_tier() -> Tier;    // cached process-wide, overridable by VACO_CPU_TIER for testing
pub fn tier_from_env_override() -> Option<Tier>;

/// A kernel table: one struct per DSP area, populated by a cascade.
pub trait KernelTable: Sized + Copy {
    fn scalar() -> Self;
    fn for_tier(t: Tier) -> Self;  // starts from scalar(), overwrites upward
}
```

Three independent gates, mirroring research §08 §1(d) — build-time "is this lane width compiled at
all" (`cfg(feature = "wide-512")`), build-time "which architecture module is linked"
(`cfg(target_arch)`), runtime "which tier does this CPU have". We reproduce all three rather than
collapsing them, because collapsing them costs the ability to test lower tiers on high-end hardware.

**Kernel generic-over-width pattern.** Kernels are written once, generic over `LaneCount`, and
instantiated for the widths we care about:

```rust
fn filter_h_u16<const N: usize>(/* … */)
where
    LaneCount<N>: SupportedLaneCount;

// instantiated:
const KERNELS_128: ScaleKernels = ScaleKernels::instantiate::<8>();   // 8×u16
const KERNELS_256: ScaleKernels = ScaleKernels::instantiate::<16>();
const KERNELS_512: ScaleKernels = ScaleKernels::instantiate::<32>();
```

**Scalar reference is mandatory.** Per architecture §7.3, every kernel has a scalar reference and a
`vaco-checkasm` differential test against it. A kernel without both is not merged. This is
non-negotiable and is the mechanism that makes the whole SIMD effort reviewable.

**Reproducibility classes.** Used by all three crates; referenced as "Class A/B/C" below.

| Class | Guarantee | How |
|---|---|---|
| **A — Bit-exact, always** | Identical output bytes across architecture, lane width, thread count, and build profile. | Integer-only arithmetic with a defined rounding rule per operation; no reassociation; no FMA; every output element is a pure function of its input window, never of chunking. |
| **B — Bit-exact under `bitexact`** | Identical output when the `bitexact` flag is set; otherwise Class C. | Float path pinned to a canonical evaluation order, FMA contraction disabled (`#[allow]`-free: we write the multiply and add as separate `Simd` ops and never call `mul_add`), accumulation order fixed to a documented tree shape independent of lane count. |
| **C — ULP-bounded** | Not bit-exact; bounded relative error, documented per operation. | Default float path. FMA permitted, accumulation order may vary with lane width. Bounds asserted in tests. |

Thread-count independence is **mandatory in every class**, with exactly one documented exception
(error-diffusion dither in `vaco-scale`, §A.9.3).

**Option surface.** All three crates expose their options through `vaco-opts`, which per architecture
§3 provides a derive macro producing typed, introspectable, string-parsable option sets. The exact
shape used throughout:

```rust
#[derive(Options, Clone, Debug)]
#[opts(name = "swr")]
pub struct ResampleOptions {
    #[opt(name = "isr", alias = "in_sample_rate", help = "input sample rate",
          default = 0, min = 0, max = i32::MAX)]
    pub in_sample_rate: i32,
    // …
}
```

`alias` carries the long form; `unit` groups named constants; `runtime` marks options settable after
init. CLI compatibility (D1) means **every name and alias in the research inventory is preserved
verbatim**, including the ones we consider poorly named.

### 0.6 Build-or-buy under D10 and D11

D10 replaced "write everything" with three gates plus a judgement call. D11 then decided *how* any
adopted crate is wired in. Both apply directly to these three crates, so the position is stated once
here and applied per-crate in §A.14, §B.13 and §C.10.

**The gates, restated as the questions we actually ask:**

1. *Pure Rust, zero FFI?* — a hard filter, no argument available.
2. *Licence per D3?* — a hard filter.
3. *Trusted and maintained?* — alive, adopted, RUSTSEC-clean, shallow tree, unsafe measured with
   `cargo-geiger`, plausibly forkable.
4. *Then the judgement call:* does the crate's model fit ours, does it cover the capability surface
   we actually need, and do we need control over its SIMD and allocation behaviour?

**Where the honest answer lands for these three.** All three of the candidate crates named in the
brief — `rustfft`/`realfft`, `rubato`, `yuv`/`dcv-color-primitives` — clear gates 1–3 comfortably.
None of them fails on licence, purity or maintenance. Every one of them is decided on question 4,
and the recurring reason is the same:

> **Our requirement surface is the reference tool's option surface, not the mathematical operation.**

A resampling crate implements *resampling*. `vaco-resample` has to implement `filter_size`,
`phase_shift`, `linear_interp`, `exact_rational`, `cutoff`, `kaiser_beta`, three filter types, ten
dither methods including a noise-shaping bank, arbitrary channel rematrixing with five mix-level
controls, matrix-encoded surround, raw channel mapping, and soft/hard timestamp compensation — and it
has to do all of it with output that a differential harness can compare against `ffmpeg -af aresample`.
The mathematical core is perhaps 15% of the crate. That ratio is what decides these questions, not the
quality of the candidate.

**The two places this reasoning does *not* apply**, and where adoption is genuinely on the table:

- `rustfft` for `vaco-tx`'s **float** paths, because a complex FFT genuinely is the whole job for
  that subset, and the option surface above it (`AVTXType`, the flags) is thin. Assessed properly in
  §C.10.
- Specific leaf kernels in `vaco-scale` — a fixed `yuv420p → rgb24` 8-bit conversion is a
  self-contained function with no option surface at all. Assessed in §A.14.

**D11's boundary, applied.** If any of these crates is adopted, it is reachable from exactly one Vaco
crate, that crate exposes only our types, and CI's single-occurrence check enforces it. Concretely:
`rustfft` would appear in `crates/dsp/vaco-tx/Cargo.toml` and nowhere else in the workspace; no codec
crate would ever name it, see its `Fft` trait, or hold its `Complex<f32>`. Backend selection uses
D11's mutually-exclusive feature pattern:

```toml
[features]
default          = ["backend-native"]
backend-native   = []
backend-external = ["dep:rustfft"]
```

Both backends satisfy the same test suite, which is what makes the three-way comparison (ours vs
wrapped vs reference binary) available as a debugging tool rather than a research project.

**D11's fidelity grades applied to signal processing.** D11 defines Exact / Equivalent / Divergent /
Unmeasured for codecs. These three crates need the same treatment, and it is more delicate here than
for a codec, because a codec has a normative bitstream to conform to and a scaler does not — the
reference tool's output *is* the specification. §A.15, §B.14 and §C.11 give the per-crate assessment,
which the brief correctly identifies as one of the more valuable things this document can contain.

---

# PART A — `vaco-scale`

## A.1 Architectural stance

Research §01 §12 states the case directly: upstream is mid-migration from monolithic per-format-pair
kernels to a composable ops graph, and for a clean-room reimplementation the ops-graph vocabulary is
"arguably a better target architecture to emulate … since it already factors scaling into orthogonal,
individually testable primitives". Architecture §10 makes this binding: per-format-pair scaling
kernels are explicitly rejected.

So `vaco-scale` is an ops graph from line one. There is no legacy backend, no `swscale_unscaled.c`
equivalent, and no per-format-pair kernel table. The consequences we accept:

- **Cost:** a naive ops graph is slower than a fused per-format-pair kernel, because each op
  round-trips through memory. The optimiser and the chain compiler (§A.6) exist to recover that,
  and they are on the critical path — not a later optimisation.
- **Benefit:** *n* input formats × *m* output formats becomes *n* + *m* pieces of code instead of
  *n*×*m*. With ~268 pixel formats that is the difference between a tractable and an intractable
  project. It also makes each primitive independently checkasm-testable, which is what lets us
  trust the SIMD.

The `SWS_BACKEND_*` selection surface from the inventory maps to our backend enum (§A.12), with
`legacy`/`stable` accepted and mapped onto our single backend rather than erroring — CLI
compatibility without implementation compatibility.

## A.2 The value model

Every op consumes and produces a **block**: a fixed number of pixels, deinterleaved into up to four
component planes, in a single element type.

```rust
/// Element type flowing between ops. Chosen per-stage by the precision pass (§A.5.8).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Elem { U8, U16, U32, F32 }

/// Number of pixels a kernel processes per call. Fixed at chain-compile time from the
/// selected lane width; always a multiple of the widest lane count.
pub const BLOCK: usize = 64;

/// The register-resident working set. Four component "channels"; unused channels are
/// tracked by `CompMask` and never touched.
pub struct Block<E> {
    pub c: [[E; BLOCK]; 4],
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct CompMask(u8);   // bit i set == component i is live
```

Four channels is the ceiling because no pixel format in the target set carries more than four
components (R,G,B,A / Y,U,V,A / X,Y,Z / gray+alpha / Bayer single). Palette is handled as a
read-side expansion (§A.4.3), not as a fifth component.

Rationale for structure-of-arrays (`[[E; BLOCK]; 4]` rather than `[[E; 4]; BLOCK]`): every arithmetic
op becomes lanewise with zero shuffles. The only ops that touch the interleaving are `Read`, `Write`,
`Pack`, `Unpack` and `Swizzle`, and those are exactly where shuffles belong. This is the same lever
that §C.6 applies to the FFT.

## A.3 The op vocabulary

The primitive set is a direct Rust rendering of the inventory's `SwsOpType` vocabulary, which we
adopt as a *design vocabulary* (an interface fact, per D7) and implement independently.

```rust
/// A single primitive operation. Ops are pure functions of their input block plus their
/// own immutable parameters — with the sole exception of `Dither(ErrorDiffusion)`,
/// which is stateful across a row (§A.9.3).
#[derive(Clone, Debug)]
pub enum Op {
    /// Gather raw pixels from source planes into a block. Always the first op of a chain.
    Read(ReadOp),
    /// Scatter a block to destination planes. Always the last op of a chain.
    Write(WriteOp),
    /// Byte-swap each element in place (endianness normalisation).
    SwapBytes,
    /// Reorder / duplicate / drop component channels.
    Swizzle(Swizzle),
    /// Split bit-packed components out of a wider element (e.g. RGB565 u16 → 3×u16).
    Unpack(BitPacking),
    /// Merge components into a bit-packed wider element.
    Pack(BitPacking),
    /// Per-component left shift.
    LShift([u8; 4]),
    /// Per-component right shift (logical; rounding is a separate Scale/Convert decision).
    RShift([u8; 4]),
    /// Overwrite selected components with a constant (e.g. force A=opaque, force UV=neutral).
    Clear([Option<f64>; 4]),
    /// Element type conversion, with a declared value-range policy.
    Convert { to: Elem, expand: Expand },
    /// Per-component lower clamp.
    Min(Px),
    /// Per-component upper clamp.
    Max(Px),
    /// Multiply every live component by one scalar.
    Scale(f64),
    /// Generalised affine transform: out = M · in + b, over up to 4 components.
    Linear(Box<Linear>),
    /// Quantisation noise shaping on the way down in bit depth.
    Dither(DitherOp),
    /// Horizontal resampling filter.
    FilterH(Arc<FilterBank>),
    /// Vertical resampling filter.
    FilterV(Arc<FilterBank>),
    /// 3D lookup table with tetrahedral interpolation (gamut/tone mapping).
    Lut3D(Arc<Lut3D>),
}

/// An op plus the element type and live-component set it operates on. The graph carries
/// these, not bare `Op`s, because the same op means different things at different widths.
#[derive(Clone, Debug)]
pub struct OpNode {
    pub op: Op,
    pub elem: Elem,
    pub comps: CompMask,
    /// Interval bounds the optimiser proved for each live component at this point.
    /// Used for clamp elimination and for precision selection.
    pub range: [Interval; 4],
}
```

Supporting types:

```rust
/// Component permutation with optional constant injection. `src[i] == None` means
/// "component i of the output is not produced by this swizzle" (used with Clear).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Swizzle { pub src: [Option<u8>; 4] }

impl Swizzle {
    pub const IDENTITY: Swizzle = Swizzle { src: [Some(0), Some(1), Some(2), Some(3)] };
    pub fn is_identity(&self, comps: CompMask) -> bool { /* … */ }
    /// Composition: `self` applied after `first`.
    pub fn after(&self, first: &Swizzle) -> Swizzle { /* … */ }
}

/// Bit layout of packed components within one storage element.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct BitPacking {
    pub container: Elem,        // U8 / U16 / U32
    /// (shift, width) per component, LSB-relative in host order after SwapBytes.
    pub fields: [(u8, u8); 4],
    pub nfields: u8,
}

/// How a Convert treats the numeric range.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Expand {
    /// Reinterpret the integer value unchanged (u8 5 → u16 5). Used inside integer chains.
    Raw,
    /// Rescale full-scale to full-scale (u8 255 → u16 65535, u8 255 → f32 1.0).
    /// Integer widening uses bit replication, not a shift, so 255→65535 exactly.
    FullScale,
    /// Normalise by 2^(bits) exactly (u8 128 → f32 0.5). Used entering the colour path.
    Normalised,
}

/// Affine transform. Row i of `m` produces output component i.
/// Column 4 is the constant term. Stored f64 at plan time, lowered at compile time.
#[derive(Clone, Debug)]
pub struct Linear {
    pub m: [[f64; 5]; 4],
    /// Structural classification, computed once, used to pick a specialised kernel.
    pub shape: LinearShape,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum LinearShape {
    /// Only the diagonal and the constant column are non-trivial: per-component a*x+b.
    Diagonal,
    /// 3×3 over components 0..3, component 3 passes through. The colour-matrix case.
    Matrix3,
    /// 3×4 (3×3 plus constants), component 3 passes through. Colour matrix + range offset.
    Affine3,
    /// Anything else.
    Full,
}
```

`Px` is a four-component constant in the current element type:

```rust
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Px { pub c: [f64; 4] }
```

Storing clamp bounds as `f64` at plan time and lowering them to the chain's element type at compile
time keeps the graph representation independent of the precision decision, which is what lets the
precision pass (§A.5.8) run *after* graph construction.

### A.3.1 Read and Write

`Read` and `Write` are where all layout knowledge lives. Everything else is layout-agnostic.

```rust
#[derive(Clone, Debug)]
pub struct ReadOp {
    /// One entry per source plane this op touches.
    pub planes: [Option<PlaneRead>; 4],
    /// Element type produced.
    pub elem: Elem,
    /// Components produced, in plane/field order (a later Swizzle puts them in canonical order).
    pub comps: CompMask,
}

#[derive(Copy, Clone, Debug)]
pub struct PlaneRead {
    pub plane: u8,
    /// Bytes between consecutive pixels within a row. 1/2/3/4/6/8 for packed layouts,
    /// == element size for planar.
    pub pixel_stride: u8,
    /// How many components this plane contributes (1 for planar Y, 2 for NV12 UV,
    /// 3 for RGB24, 4 for RGBA).
    pub ncomp: u8,
    /// Horizontal and vertical log2 subsampling of this plane relative to component 0.
    pub log2_sub: (u8, u8),
}
```

`WriteOp` is the mirror image. The important property: `ReadOp` is derived mechanically from the
`vaco-pixfmt` descriptor, so a new pixel format costs a table row and zero code — which is the whole
point of the ops-graph architecture.

### A.3.2 Filter banks

```rust
/// A resampling filter: for each destination position, an offset into the source and
/// `taps` coefficients. Layout is interleaved for SIMD (see §A.7.5).
pub struct FilterBank {
    pub dst_len: usize,
    pub src_len: usize,
    pub taps: usize,
    /// Source index of the first tap for each destination position. `dst_len` entries.
    pub offsets: Vec<i32>,
    /// Coefficients. Layout depends on `repr`.
    pub coeffs: Coeffs,
    /// Fixed-point shift for the integer representation (coefficients sum to 1<<shift).
    pub shift: u8,
    pub repr: CoeffRepr,
    /// Provenance: what generated this bank. Kept for `-v debug` dumps and for tests.
    pub spec: FilterSpec,
}

pub enum Coeffs {
    /// `i16` coefficients, blocked: coeffs[(d/W)*taps*W + t*W + (d%W)] for lane width W.
    I16 { data: Vec<i16>, lane_width: usize },
    F32 { data: Vec<f32>, lane_width: usize },
}

pub enum CoeffRepr { Fixed14, Float32 }
```

## A.4 Graph construction

### A.4.1 The request

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct ImageSpec {
    pub format: PixelFormat,
    pub width: u32,
    pub height: u32,
    pub color: ColorSpec,
}

/// Everything from `vaco-color` that affects conversion. All fields may be `Unspecified`;
/// §A.8.1 defines the defaulting rules.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ColorSpec {
    pub primaries: ColorPrimaries,
    pub transfer: ColorTransfer,
    pub matrix: ColorMatrix,
    pub range: ColorRange,
    pub chroma_loc: ChromaLocation,
    pub alpha_mode: AlphaMode,
    /// HDR metadata, when present, feeds tone mapping (§A.8.6).
    pub mastering: Option<MasteringDisplay>,
    pub content_light: Option<ContentLightLevel>,
}
```

A conversion request is `(src: ImageSpec, dst: ImageSpec, opts: ScaleOptions)`.

### A.4.2 Construction is a lowering pipeline, not a search

Graph construction is deterministic and non-searching. It emits a canonical op sequence, then hands
it to the optimiser. Never the reverse: the builder does not try to be clever, because a clever
builder and a clever optimiser interact unpredictably and neither can be tested in isolation.

The canonical sequence, in order:

```
  READ                     src planes → block, native element type
  SWAP_BYTES               if src format endianness ≠ host
  UNPACK                   if src is bit-packed within an element
  SWIZZLE                  → canonical component order (see below)
  CLEAR                    inject missing components (e.g. A=opaque)
  SHIFT / CONVERT          → the working element type chosen by the precision pass
  ── input value domain ──────────────────────────────────────────────
  LINEAR                   range expansion: limited → full
  LINEAR                   matrix: YCbCr → R'G'B'   (skipped if matrices match)
  ── gamma domain ────────────────────────────────────────────────────
  (transfer)               R'G'B' → linear RGB      (only if gamma-correct scaling,
                                                     transfer conversion or tone mapping
                                                     is required — §A.8.4)
  LINEAR                   primaries: RGB_src → RGB_dst  (only if primaries differ)
  LUT_3D                   gamut/tone mapping        (only for non-trivial intents)
  (transfer)               linear RGB → R'G'B'_dst
  ── back to coded domain ────────────────────────────────────────────
  FILTER_H                 horizontal resample       (skipped if widths match and no shift)
  FILTER_V                 vertical resample         (skipped if heights match and no shift)
  LINEAR                   matrix: R'G'B' → YCbCr_dst
  LINEAR                   range compression: full → limited
  MIN / MAX                clamp to the destination's legal range
  DITHER                   if the destination has fewer bits than the working precision
  CONVERT                  → destination element type
  SWIZZLE                  canonical order → destination component order
  PACK                     if dst is bit-packed
  SWAP_BYTES               if dst format endianness ≠ host
  WRITE                    block → dst planes
```

Two structural notes:

1. **Canonical component order** is `(0,1,2,3) = (Y|R, Cb|G, Cr|B, A)`. Every format is swizzled into
   it on read and out of it on write. This is what collapses the format matrix: the middle of the
   graph never knows whether it started as `bgr24` or `gbrp`.
2. **Filter placement is inside the gamma domain when gamma-correct scaling is on and outside it
   when it is off.** Placing `FILTER_H`/`FILTER_V` after the transfer-to-linear step is the entire
   meaning of the `gamma` option (§A.8.4). The builder therefore emits the filters at one of two
   positions depending on that flag. This is the only branch in the canonical sequence that changes
   op *order* rather than op *presence*.

### A.4.3 Special sources

- **Palette (`pal8`)**: `Read` produces the 8-bit index; a `Lut3D`-shaped 1-D LUT op (a degenerate
  `Lut3D` with `dim = (256,1,1)`) expands to BGRA. We special-case the 1-D degenerate form in the
  optimiser to a table-lookup kernel. The palette lives in frame side data and is captured into the
  graph at `frame_start` time, which means a palette change invalidates the compiled chain — handled
  by hashing the palette into the chain cache key.
- **Bayer**: `Read` produces one component plus a `(x&1, y&1)` phase; demosaicing is emitted as a
  small fixed `FilterH`/`FilterV` pair with phase-dependent coefficient banks. Bilinear demosaic is
  the only mode; anything better belongs in a filter, not in the scaler.
- **XYZ (`xyz12le` etc.)**: transfer is DCI gamma 2.6 with the 48/52.37 luminance scaling
  (SMPTE ST 428-1); the primaries step is a plain 3×3 from XYZ, so no special casing is needed
  beyond the transfer function.
- **`Unspecified` sizes / cropping**: not handled here. Cropping is a frame-level operation applied
  by the caller before `scale`; the scaler sees only whole planes.

### A.4.4 Chroma siting

Chroma location produces a sub-pixel *phase shift* on the chroma filter banks, not a separate op.
For a plane with horizontal subsampling factor `s = 1 << log2_sub.0`, the source sample position of
chroma sample `j` in luma units is:

```
  x_luma(j) = (j + 0.5) * s - 0.5 + h_offset(src_chroma_loc, s)
```

where `h_offset` is 0 for left-sited chroma (H.273 `Left`, the MPEG-2/H.264 default) and
`(s - 1) / 2` for centre-sited (`Center`, the MPEG-1/JPEG default). Vertical siting is analogous
using `TopLeft`/`Top`/`Bottom`/`BottomLeft`. The destination phase is computed the same way, and the
difference `x_luma_src - x_luma_dst` is folded into the filter-centre computation of §A.7.2 as an
additive offset. Explicit `src_h_chr_pos` / `dst_h_chr_pos` options override the enum-derived value,
expressed in 1/256 luma-grid units exactly as the inventory specifies.

**Consequence:** even a same-size, same-format conversion may need chroma filters if the siting
differs. The optimiser's no-op detection must not fire on size equality alone.

## A.5 The optimiser

`ops_optimize(graph: &mut Graph)` runs the following passes to a fixed point (typically 2–3
iterations; the loop is capped at 8 with a debug assertion, since a non-terminating optimiser is a
hang and hangs are what fuzzing looks for).

Each pass is a separate function with its own unit tests, and each pass is individually disableable
via a debug option (`-v debug` plus `sws_opt_disable=fuse_linear`) — because "the optimiser broke
it" is a bug class that needs bisecting.

### A.5.1 No-op elimination
Removes: identity `Swizzle`; `LShift`/`RShift` of all zeros; `Scale(1.0)`; `Convert` to the current
element type with `Expand::Raw`; `Clear` of components not in `CompMask`; `Linear` whose matrix is
the identity with zero constants; `FilterH`/`FilterV` whose bank is a unit impulse at offset 0.

### A.5.2 Dead-component elimination
Backward liveness analysis. Start from `Write`'s `CompMask` and propagate backwards through each op's
component-dependency relation (`Linear` makes output *i* depend on every *j* with `m[i][j] != 0`;
`Swizzle` maps directly; elementwise ops are the identity relation). Then delete every op that only
writes dead components, and narrow every op's `CompMask`.

This is the pass that makes `yuv420p → gray` fast: the chroma planes are never read, and the read op
loses two of its `PlaneRead` entries. It is also what makes `rgb24 → rgba` cheap in the other
direction (the alpha channel's producer is a `Clear`, which survives; nothing upstream feeds it).

### A.5.3 Linear fusion
Adjacent `Linear` nodes with the same element type fuse by matrix multiplication in `f64`:
`fuse(A, B)` for `B ∘ A` computes `M = M_B · M_A` and `b = M_B · b_A + b_B` over the augmented 4×5
representation. `Scale(k)` participates as a diagonal `Linear`. `Convert` with
`Expand::{FullScale, Normalised}` also participates, because both are affine.

This is the single highest-value pass. The canonical sequence emits up to six consecutive `Linear`
nodes (range expand, YCbCr→RGB, primaries, RGB→YCbCr, range compress, plus scale-in-convert); fusion
collapses them to one `Affine3`. That is the difference between six passes over the block and one.

`LinearShape` is recomputed after each fusion so the dispatcher picks the narrowest kernel.

### A.5.4 Shift/scale unification
In a float chain, `LShift(n)` ≡ `Scale(2^n)` and `RShift(n)` ≡ `Scale(2^-n)` — rewriting them as
`Scale` lets them participate in linear fusion. In an integer chain the rewrite goes the other way:
a `Scale` by an exact power of two becomes a shift. The pass rewrites toward whichever form the
current element type prefers, then re-runs fusion.

Note the asymmetry: `RShift` in integer truncates, `Scale(2^-n)` in integer must round. The pass only
performs the integer rewrite when the graph's rounding policy for that node is truncation, which the
builder records explicitly. Getting this wrong is a silent off-by-one-LSB bug across the whole image,
so the rewrite carries a `debug_assert` comparing against the unfused path in test builds.

### A.5.5 Clamp elimination via interval propagation
Every op has a transfer function on component intervals:

```rust
impl Op {
    /// Propagate value-range bounds forward. Conservative (may over-approximate);
    /// must never under-approximate, or clamp elimination becomes unsound.
    fn propagate(&self, input: [Interval; 4]) -> [Interval; 4];
}
```

`Min`/`Max` nodes whose bound is already implied by the incoming interval are deleted. Nodes that are
*partially* implied are narrowed (a two-sided clamp becomes one-sided). This matters because the
canonical sequence emits a defensive clamp before every quantisation, and most of them are provably
redundant — e.g. a `bt709` → `bt709` limited→limited path with no scaling cannot leave `[16,235]`.

Soundness is the risk. The pass is tested by a property test: for a randomly generated graph and
randomly generated inputs, running with clamps and without clamps must agree, for every graph the
pass claims is clamp-free.

### A.5.6 Swizzle fusion and sinking
Adjacent swizzles compose. A swizzle may sink through any op whose behaviour is component-symmetric
(`Convert`, `SwapBytes`, `Scale`, `Min`/`Max` with equal per-component bounds, `FilterH`/`FilterV`)
and may be permuted into a neighbouring `Linear` by permuting the matrix rows/columns — which
deletes it outright. Sinking swizzles toward the ends of the chain is what lets the middle be
format-agnostic without paying for it.

### A.5.7 Pack/unpack cancellation and memcpy detection
`Unpack(p)` immediately followed by `Pack(p)` with the same `BitPacking` cancels. After all other
passes, if the graph has reduced to exactly `Read` + `Write` with identical plane layouts, strides
and sizes, the whole pass is replaced by a plane-copy pass (`ops_memcpy` equivalent). If it has
reduced to nothing at all — same format, same size, same colour spec — `is_noop()` returns true and
`scale_frame` degrades to an `Arc` clone of the source planes, per architecture §7.4's zero-copy rule.

### A.5.8 The precision pass

This is the pass that decides integer versus float, and it is the one with the most performance
leverage.

**Inputs:** the source and destination bit depths, the set of ops present, and the `precision`
option.

**Decision procedure** (first matching rule wins):

1. If `precision` is set explicitly (`u8`/`u16`/`u32`/`f32`), use it. Tests pin this.
2. If the graph contains a transfer-function evaluation, a `Lut3D`, or a primaries conversion →
   **F32**. These are transcendental or high-dynamic-range; fixed point is not worth the analysis.
3. If `max(src_depth, dst_depth) ≤ 8` and every `Linear` is `Diagonal` or `Affine3` → **U8 blocks
   with U16 arithmetic**. Coefficients are 14-bit fixed point; products accumulate in `i32`.
4. If `max(src_depth, dst_depth) ≤ 14` → **U16 blocks with I32 arithmetic**, 14-bit coefficients.
5. If `max(src_depth, dst_depth) ≤ 16` and no `FilterH`/`FilterV` with more than 8 taps →
   **U16 blocks with I32 arithmetic**, coefficient shift reduced to `min(14, 30 - depth - log2(taps))`
   to keep the accumulator inside `i32`.
6. Otherwise → **F32**.

**Coefficient shift derivation.** For a filter with `T` taps, input depth `d`, and coefficient shift
`s`, the worst-case accumulator magnitude is `T · (2^d - 1) · 2^s`. Requiring that to fit in `i32`
gives `s ≤ 31 - d - ceil(log2(T))`. We take `s = min(14, 31 - d - ceil(log2 T))` and fail the plan
with a diagnostic if that drops below 8, falling back to F32. The bound is asserted in a test that
enumerates every (depth, taps) pair we support.

**Float sub-precision.** When the F32 path is chosen, the transfer functions are still evaluated in
`f32`; `f64` appears only at plan time (coefficient generation, matrix derivation) and never in a
per-pixel loop. Research §08 gives no evidence that `f64` pixel math is ever needed, and it halves
throughput.

### A.5.9 Pass fusion (the chain compiler's input)
The final pass groups the op sequence into **passes** — maximal runs of ops that can execute on one
block without an intervening buffer round-trip. `FilterV` forces a pass boundary (it needs multiple
source *rows*, so it cannot be block-local). `FilterH` does not, if the horizontal filter is applied
during the read. Everything else is block-local.

Typical result for `yuv420p → rgb24` scaled: two passes (read+unpack+matrix+filterH into a row
buffer; filterV+clamp+dither+pack+write). For `yuv420p → nv12` unscaled: one pass.

## A.6 Kernel dispatch and execution

### A.6.1 The compiled chain

```rust
/// A run of ops compiled into a callable sequence. One per pass.
pub struct CompiledChain {
    entries: Vec<ChainEntry>,
    /// Total scratch bytes the chain needs per worker.
    scratch: usize,
}

struct ChainEntry {
    kernel: KernelFn,
    params: OpParams,       // flattened, cache-friendly, no Box/Arc deref in the loop
}

/// Every kernel has this signature. `state` is the register-resident block plus scratch.
type KernelFn = fn(&mut ChainState<'_>, &OpParams);
```

### A.6.2 Greedy longest-run matching

The naive chain — one `KernelFn` per `Op` — costs one indirect call per op per block. With
`BLOCK = 64` and a 12-op chain that is one call per 5.3 pixels, which is real overhead.

The dispatcher therefore does greedy longest-run matching against a **fused kernel table**:

```rust
/// Fused kernels, keyed by a small op-pattern signature. Populated per ISA tier.
pub struct ScaleKernels {
    /// Per-op fallbacks — always complete, so any chain can always be compiled.
    pub per_op: [Option<KernelFn>; OP_KIND_COUNT],
    /// Fused supernodes, tried longest-first.
    pub fused: &'static [(Pattern, KernelFn)],
}

/// A pattern is a short op-kind sequence with element-type and shape constraints.
pub struct Pattern {
    pub kinds: &'static [OpKind],
    pub elem: Elem,
    pub constraint: fn(&[OpNode]) -> bool,
}
```

Patterns we ship in the first cut, chosen because they cover the measured-hot conversions
(§08 §5 scenario 3 names `yuv420p→rgb24`, `yuv420p→nv12`, and bicubic up/downscale):

| Pattern | Covers |
|---|---|
| `Read · Swizzle · Convert` | planar 8→16-bit widening read |
| `Read · Unpack · Swizzle` | RGB565/RGB555/packed-10-bit read |
| `Linear(Affine3) · Min · Max` | fused colour matrix with clamp |
| `Convert · Linear(Affine3) · Min · Max · Convert` | the whole colour stage |
| `FilterH · Linear(Affine3)` | horizontal scale fused with matrix |
| `Dither · Convert · Swizzle · Pack · Write` | the whole output stage |
| `Linear(Diagonal) · Min · Max · Convert · Write` | range conversion output |

Adding a fused kernel is purely additive: it never changes semantics, only speed, because the
per-op path remains and is what the differential test compares against. That is the property that
makes this table safe to grow opportunistically based on profiles.

### A.6.3 KernelSet resolution

Per architecture §7.3, resolution happens once, at graph-compile time:

```rust
impl KernelTable for ScaleKernels {
    fn scalar() -> Self { SCALAR_KERNELS }
    fn for_tier(t: Tier) -> Self {
        let mut k = Self::scalar();
        #[cfg(target_arch = "x86_64")] {
            if t >= Tier::SSE2   { k.merge(&x86::KERNELS_128); }
            if t >= Tier::AVX2   { k.merge(&x86::KERNELS_256); }
            if t >= Tier::AVX512 { k.merge(&x86::KERNELS_512); }
        }
        #[cfg(target_arch = "aarch64")] {
            if t >= Tier::NEON   { k.merge(&aarch64::KERNELS_128); }
            if t >= Tier::SVE256 { k.merge(&aarch64::KERNELS_256); }
        }
        k
    }
}
```

`x86::KERNELS_256` and `aarch64::KERNELS_256` are both instantiations of the *same* generic kernel
source at `N = 256/element_bits`. The per-arch modules exist to hold the tier gating and any
arch-specific pattern table, not to hold arch-specific kernel bodies. If we ever find we need a
genuinely arch-specific body, that is a signal worth escalating, not a routine occurrence.

`merge` overwrites only the slots the higher tier actually provides — "highest satisfied tier wins,
expressed as sequential overwrites", which is exactly the model research §08 §1(c) describes and the
model architecture §7.3 adopts.

### A.6.4 Execution

```rust
struct Pass {
    chain: CompiledChain,
    /// Rows of input this pass needs to produce one row of output, and their offsets.
    row_window: RowWindow,
    /// Intermediate buffer descriptor (None for the final pass, which writes to dst).
    buffer: Option<BufferSpec>,
}

pub struct Graph {
    passes: Vec<Pass>,
    kernels: ScaleKernels,
    src: ImageSpec,
    dst: ImageSpec,
    /// Set when the whole graph is a plane copy or a no-op.
    fast_path: Option<FastPath>,
}
```

Execution walks passes in order; within a pass it walks destination rows, and within a row it walks
`BLOCK`-sized column groups. Intermediate row buffers are ring buffers sized to the vertical filter's
support so a `FilterV` pass consumes rows as they are produced — the classic slice pipeline, which is
also what makes the streaming slice API (§A.11.2) fall out for free.

## A.7 Scaling algorithms as coefficient generators

Every algorithm is a continuous kernel `k(x)` plus a support radius. There are no tables in the
source. A coefficient table is an authorial choice under D7; a kernel formula is mathematics.

### A.7.1 The kernel set

Let `x` be the distance in source-pixel units from the filter centre.

| Name | `k(x)` | Support | Params |
|---|---|---|---|
| `point` | `1` for `|x| < 0.5`, else 0 | 0.5 | — |
| `bilinear` | `1 - |x|` for `|x| < 1`, else 0 | 1 | — |
| `fast_bilinear` | as `bilinear`, but evaluated by integer DDA (§A.7.6) | 1 | — |
| `bicubic` | Mitchell–Netravali, below | 2 | `param0 = B`, `param1 = C` |
| `bicublin` | `bicubic` for luma, `bilinear` for chroma | — | as bicubic |
| `area` | `1` for `|x| ≤ r/2` where `r = max(1, src/dst)`, else 0, normalised | `r/2` | — |
| `gaussian` | `exp(-x² / (2σ²))` | `3σ` | `param0 = σ` (default 1.0) |
| `sinc` | `sinc(x)` unwindowed | `param0` (default 16) | `param0` = radius |
| `lanczos` | `sinc(x) · sinc(x/a)` for `|x| < a` | `a` | `param0 = a` (default 3) |
| `spline` | natural cubic spline, below | 2/3/4 | `param0` selects 16/36/64 |

with `sinc(x) = sin(πx)/(πx)`, `sinc(0) = 1`.

**Mitchell–Netravali bicubic** (Mitchell & Netravali, SIGGRAPH 1988, "Reconstruction Filters in
Computer Graphics"):

```
          ⎧ ( (12 - 9B - 6C)|x|³ + (-18 + 12B + 6C)|x|² + (6 - 2B) ) / 6            , |x| < 1
  k(x) =  ⎨ ( (-B - 6C)|x|³ + (6B + 30C)|x|² + (-12B - 48C)|x| + (8B + 24C) ) / 6   , 1 ≤ |x| < 2
          ⎩ 0                                                                        , otherwise
```

Named points on the `(B, C)` plane: Catmull–Rom `(0, 0.5)` — our default, interpolating and sharp;
Mitchell `(1/3, 1/3)` — the paper's recommended compromise; cubic B-spline `(1, 0)` — smooth,
approximating. The `scaler = bicubic` selection in the new-style option surface is documented in the
inventory as "2-tap cubic B-spline", so `scaler=bicubic` maps to `(1, 0)` while the legacy
`sws_flags=bicubic` maps to Catmull–Rom. That divergence is inherited from the interface contract; we
preserve it and document it rather than harmonising it.

**Natural cubic spline (`spline16` / `spline36` / `spline64`).** These are the interpolation kernels
obtained by fitting a natural cubic spline (second derivative zero at the boundary) through
`2m` equally spaced samples and reading off the resulting weight as a function of `x`. The derivation
is standard and is given in the Panorama Tools / Avisynth resampling literature; we implement the
derivation at plan time rather than embedding the resulting polynomial coefficients:

1. Build the tridiagonal system for a natural cubic spline through `2m` unit-spaced knots.
2. Solve it symbolically for the cardinal basis (input `δ_j`), yielding a piecewise cubic on each of
   the `2m` intervals.
3. Evaluate at the required `x` to get the tap weight.

Because step 2 is done once per plan for the small `m ∈ {2, 3, 4}` we support, the cost is
negligible and we never hold a transcribed table. `param0` selects `m` (default 2 = spline16, which
is what the bare `spline` name maps to).

**Area.** The area kernel is the only one whose support depends on the scale ratio. It is a box of
width `r = max(1, src_len / dst_len)` in source-pixel units, which makes downscaling an exact area
average (each destination pixel is the mean of the source pixels its footprint covers, weighted by
partial coverage at the ends). At `r ≤ 1` it degenerates to nearest-neighbour, which is the correct
behaviour for upscaling with an area filter and matches what callers expect.

### A.7.2 Bank generation

```rust
pub struct FilterSpec {
    pub kernel: Kernel,
    pub src_len: usize,
    pub dst_len: usize,
    /// Sub-pixel phase offset in source-pixel units (chroma siting, §A.4.4).
    pub phase: f64,
    /// Coefficient representation and fixed-point shift.
    pub repr: CoeffRepr,
    pub shift: u8,
    pub lane_width: usize,
}

pub fn build_bank(spec: &FilterSpec) -> FilterBank;
```

The generator, for destination index `d ∈ [0, dst_len)`:

```
  ratio   = src_len / dst_len
  centre  = (d + 0.5) * ratio - 0.5 + phase
  xscale  = min(1, 1 / ratio)          // stretch the kernel when downscaling
  radius  = kernel.support() / xscale
  first   = ceil(centre - radius)
  taps    = floor(centre + radius) - first + 1
  w[j]    = kernel.eval((first + j - centre) * xscale)      for j in 0..taps
```

`xscale < 1` when downscaling stretches the kernel so it low-pass filters at the *destination*
Nyquist rather than the source Nyquist — without this, downscaling aliases, and it is the single most
common way to get resampling visibly wrong.

Then, in order:

1. **Edge clamping.** Taps landing outside `[0, src_len)` are folded onto the nearest valid source
   index by accumulating their weight into the boundary tap. This is edge replication, and it is
   chosen over zero-padding (which darkens edges) and reflection (which mirrors detail). Alternative
   edge modes are not exposed; a caller wanting reflection uses a pad filter.
2. **Uniform tap count.** `taps` is taken as the maximum over all `d`, and shorter banks are
   zero-padded with their offsets adjusted so every destination position reads the same number of
   source samples. Uniform tap count is what makes the inner loop a fixed trip count, which is what
   architecture §7.1 says autovectorisation requires.
3. **Normalisation.** `w[j] /= Σ w` so DC gain is exactly 1. Without this, area and gaussian kernels
   change image brightness.
4. **Quantisation** (fixed-point representations only). `q[j] = round(w[j] * (1 << shift))`, then the
   residual `(1 << shift) - Σ q` is added to the single tap with the largest `|w[j]|`. This
   guarantees `Σ q == 1 << shift` exactly, which is what makes the integer path Class A: a constant
   input produces exactly that constant on output, with no drift.

### A.7.3 Filter-size caps

Support radius times `1/xscale` grows linearly with the downscale factor. A 16× downscale with
`lanczos:3` wants 96 taps. We cap `taps` at 64 and, above the cap, switch to a two-stage decimation:
an integer-factor `area` pre-decimation followed by the requested kernel at the residual ratio. This
is exact for power-of-two factors and near-exact otherwise, and it turns an O(scale) inner loop into
O(1). The cap is an option (`max_taps`, default 64) so the behaviour is measurable rather than
folklore.

### A.7.4 Which filter is chosen for what

`scaler` selects the luma algorithm; `scaler_sub` selects the algorithm applied to subsampled
(chroma) planes. `scaler = auto` resolves to `bicubic` (Catmull–Rom) for both, which is the quality
level most callers expect. `bicublin` is the legacy spelling of `scaler=bicubic, scaler_sub=bilinear`
and is normalised to it during option parsing, so the graph builder never sees `bicublin`.

### A.7.5 Coefficient layout for SIMD

The naive layout `coeffs[d * taps + t]` forces a gather: consecutive destination pixels read
coefficients that are `taps` apart. The blocked layout avoids it entirely:

```
  coeffs[(d / W) * taps * W + t * W + (d % W)]
```

for lane width `W`. Processing `W` destination pixels at once, tap `t`'s coefficients for all `W`
pixels are a single contiguous vector load. Research §08 §6 item 10 names this precisely: "avoiding
true hardware gather via precomputed-coefficient-table tricks matters more than instruction-set
choice here."

The source samples still need gathering, because `offsets[d]` differs per destination pixel. Two
mitigations, chosen per-bank at build time:

- **Uniform-stride detection.** When `src_len` and `dst_len` share a small ratio, `offsets` is an
  arithmetic progression and the gather becomes a strided load, which we express as `W` scalar loads
  that LLVM recognises. For the very common integer ratios (1:1, 2:1, 3:1, 4:1, 1:2) we emit a
  dedicated kernel.
- **Row-window transpose.** For arbitrary ratios with `taps ≤ 8`, we transpose an
  `8 × BLOCK` window of the source row once and then read it lanewise. The transpose costs
  `log2(8)` shuffle rounds and amortises over all taps.

`lane_width` is baked into the bank at build time, which means the bank depends on the resolved
kernel tier. That is fine — banks are built inside graph compilation, after tier resolution — but it
does mean a bank is not portable across tiers, and the type system should reflect that. It does:
`FilterBank` is only constructible via `build_bank`, which takes `lane_width` in its spec.

### A.7.6 Fast bilinear

`fast_bilinear` is the one algorithm that is not a coefficient bank. It is a single-pass incremental
scaler: a 16.16 fixed-point DDA walks the source position, and each output pixel is
`(s0 * (256 - f) + s1 * f) >> 8` with `f` the top 8 bits of the fraction. No table, no offsets array,
no per-plan setup. It exists for the case where setup cost dominates (tiny images, one-shot
conversions) and for callers who explicitly ask for speed over quality.

It is Class A but is *not* bit-identical to `bilinear`, because it uses 8-bit rather than 14-bit
weights. That divergence is intentional and documented; the differential harness allowlists it
against `bilinear` while still requiring exact match against the reference binary's
`fast_bilinear`.

## A.8 Colour management

`vaco-color` (layer 1) owns the enums and the chromaticity data; `vaco-scale` owns the conversion
logic that turns them into `Linear` and transfer ops. Everything here is per ITU-T H.273 /
ISO-IEC 23091-2 code points, which is exactly what D7 §"freely used" contemplates: spec-dictated
constants derived independently from the spec text.

### A.8.1 Defaulting `Unspecified`

Real files are full of `Unspecified`. The resolution rules, applied before graph construction:

| Field | Rule when `Unspecified` |
|---|---|
| `matrix` | For RGB formats: `Rgb`. For YCbCr: `Bt709` if `height ≥ 720`, else `Bt470bg` (i.e. BT.601-625) if `height > 576` is false and the frame rate suggests PAL, else `Smpte170m`. Where the heuristic is ambiguous we take `Bt709` and log at `debug`. |
| `primaries` | Follow `matrix` (`Bt709` → `Bt709`, `Bt470bg` → `Bt470bg`, `Smpte170m` → `Smpte170m`, `Bt2020Ncl` → `Bt2020`). |
| `transfer` | Follow `primaries`, using the corresponding OETF; `Bt2020` → `Bt709` (the BT.1886 curve), not PQ, because unflagged BT.2020 is overwhelmingly SDR. |
| `range` | `Limited` for YCbCr, `Full` for RGB and for JPEG-derived formats. |
| `chroma_loc` | `Left` for all YCbCr except MPEG-1/JPEG-derived sources, where `Center`. |
| `alpha_mode` | `Straight`. |

These heuristics are observable behaviour and therefore fall under the differential harness: we
record what the reference binary does for a matrix of unflagged inputs and require agreement, with
an explicit reviewed allowlist for the cases where we deliberately differ.

### A.8.2 Range conversion

For bit depth `n`, limited range is `Y ∈ [16·2^(n-8), 235·2^(n-8)]`,
`C ∈ [16·2^(n-8), 240·2^(n-8)]`, full range is `[0, 2^n - 1]`.

Limited → full for luma: `y_full = (y - 16·2^(n-8)) · (2^n - 1) / (219·2^(n-8))`.
Chroma is centred: `c_full = (c - 128·2^(n-8)) · (2^n - 1) / (224·2^(n-8)) + 128·2^(n-8)`.

Both are affine, so both become `Linear(Diagonal)` nodes that fuse into the colour matrix. This is
why range conversion costs nothing when a matrix conversion is already happening, and costs one fused
affine pass when it is not.

### A.8.3 The YCbCr matrix

For matrix coefficients with luma weights `(Kr, Kg, Kb)`, `Kg = 1 - Kr - Kb`, the non-constant-luminance
forward transform on non-linear `R'G'B'` in `[0,1]` is:

```
  Y'  =  Kr·R' + Kg·G' + Kb·B'
  Cb  =  (B' - Y') / (2 · (1 - Kb))
  Cr  =  (R' - Y') / (2 · (1 - Kr))
```

and the inverse is the matrix inverse, computed in `f64` at plan time rather than stored:

```
  R' = Y' + 2(1 - Kr)·Cr
  G' = Y' - 2(Kb(1 - Kb)·Cb + Kr(1 - Kr)·Cr) / Kg
  B' = Y' + 2(1 - Kb)·Cb
```

`(Kr, Kb)` per H.273 §8.3 code point: `Bt709 = (0.2126, 0.0722)`, `Bt470bg`/`Smpte170m` =
`(0.299, 0.114)`, `Smpte240m = (0.212, 0.087)`, `Fcc = (0.30, 0.11)`, `Bt2020Ncl` /
`Bt2020Cl = (0.2627, 0.0593)`.

Special matrices that are **not** of this form and get their own derivations:

- **`Rgb` (identity)** — no matrix op.
- **`YCgCo`** — the lifting-based `Y = (R + 2G + B)/4`, `Cg = (-R + 2G - B)/4`, `Cr_o = (R - B)/2`.
- **`YCgCoRe` / `YCgCoRo`** — the *reversible* integer lifting forms (even/odd bit expansion). These
  are not affine and cannot be expressed as `Linear`; they get a dedicated op-free lowering as a
  short fixed sequence of `Linear` + `RShift` nodes with defined truncation, and they are Class A by
  construction (they must be, since they are lossless).
- **`Bt2020Cl`** — constant luminance. The luma channel is derived from *linear* RGB and the chroma
  from non-linear, so the graph must evaluate the transfer function in the middle of the matrix step.
  Lowered as: transfer→linear, luma dot product, transfer→non-linear on luma, then chroma from the
  non-linear channels. It is the only matrix that forces a transfer evaluation even when transfer
  characteristics match.
- **`ChromaDerivedNcl` / `ChromaDerivedCl`** — `(Kr, Kb)` derived from the *primaries* rather than
  tabulated: with the RGB→XYZ matrix `M` built from the primaries (§A.8.5), `Kr = M[1][0]`,
  `Kg = M[1][1]`, `Kb = M[1][2]` — i.e. the luminance row. Falls out of the primaries code with no
  extra machinery.
- **`Ictcp`** — ITU-R BT.2100 ICtCp: LMS from BT.2020 RGB, PQ or HLG encode per-channel, then the
  ICtCp matrix. Requires a transfer evaluation, so it forces F32.
- **`IptC2`** — the SMPTE ST 2128 IPT-C2 variant; same shape as ICtCp with a different LMS matrix
  and crosstalk step.
- **`Smpte2085`** (Y'D'zD'x) — a fixed 3×3 on non-linear RGB.

### A.8.4 Transfer characteristics

Each H.273 §8.2 code point becomes an EOTF/OETF pair:

```rust
pub trait TransferFn {
    /// Non-linear signal → linear light, both nominally in [0, 1] (PQ maps to [0, 10000] nits/10000).
    fn to_linear(&self, v: f32) -> f32;
    fn from_linear(&self, v: f32) -> f32;
    /// True when to_linear/from_linear are exact inverses to within 1 ULP — enables
    /// round-trip elimination in the optimiser.
    fn is_exact_inverse(&self) -> bool;
}
```

Implemented set: `Bt709`/`Bt601`/`Smpte170m` (the shared 1.099/0.018 piecewise curve),
`Gamma22`, `Gamma28`, `Smpte240m`, `Linear`, `Log` (log100), `LogSqrt` (log316),
`Iec61966_2_4` (xvYCC, defined for negative inputs by odd symmetry), `Bt1361Ecg`,
`Iec61966_2_1` (sRGB, the 1.055/0.0031308 curve), `Bt2020_10`, `Bt2020_12`,
`Smpte2084` (PQ), `Smpte428` (DCI gamma 2.6 with the 52.37/48 scaling), `AribStdB67` (HLG,
including the inverse OOTF with system gamma derived from peak luminance).

**Evaluation strategy.** Per-pixel `powf` is unacceptably slow. Three tiers, chosen at plan time:

1. **Integer input, ≤ 12 bits** → a `2^depth`-entry `f32` LUT built at plan time. Exact, since the
   input domain is finite. This covers the overwhelming majority of real conversions.
2. **Float input, or > 12 bits** → a segmented polynomial approximation: the domain is split at the
   curve's knee, and each segment gets a minimax polynomial fitted at plan time (Remez, or in
   practice a Chebyshev fit refined by a few Remez exchanges) to a target of ≤ 1e-6 absolute error.
   This evaluates as a Horner chain of `Simd<f32, N>` FMAs — fully lanewise, no branches beyond a
   knee select.
3. **`bitexact` mode** → the reference scalar path, with a fixed evaluation order.

The polynomial coefficients are computed at plan time by our own fitting code, not transcribed. That
is both a D7 requirement and, conveniently, the only way to make the error target auditable.

**Round-trip elimination.** When source and destination transfer functions are equal and
`is_exact_inverse()` holds and no operation between them needs linear light, the optimiser deletes
both. This is why `gamma = false` is fast: with gamma-correct scaling off, the transfer pair is only
emitted when the transfer *characteristics differ*, and then it is unavoidable.

**Gamma-correct scaling (`gamma` option).** When on, `FilterH`/`FilterV` are emitted *between* the
to-linear and from-linear steps, so the resampling averages linear light rather than coded values.
This is the physically correct thing to do and produces visibly different (better) results on
high-contrast edges. It is off by default for compatibility, and turning it on forces the F32 path
(precision rule 2, §A.5.8) — a real cost, honestly stated.

### A.8.5 Primaries conversion

Given primaries chromaticities `(xr,yr), (xg,yg), (xb,yb)` and white point `(xw,yw)`, the RGB→XYZ
matrix is derived per SMPTE RP 177:

```
        ⎡ xr/yr  xg/yg  xb/yb ⎤
  P  =  ⎢   1      1      1   ⎥
        ⎣ zr/yr  zg/yg  zb/yb ⎦          with z = 1 - x - y

  W  =  (xw/yw, 1, zw/yw)ᵀ
  S  =  P⁻¹ · W                          per-primary scaling
  M  =  P · diag(S)                      RGB → XYZ
```

Conversion between two primary sets is `M_dst⁻¹ · A · M_src`, where `A` is the chromatic adaptation
matrix when the white points differ. We use the **Bradford** transform for `A` (von Kries adaptation
in the Bradford cone space), which is the standard choice in colour management and is what makes
D65↔DCI-P3-white conversions look right. All of this is `f64` plan-time arithmetic producing one
3×3 that becomes a `Linear(Matrix3)`.

Chromaticity data per H.273 §8.1 code point (`Bt709`, `Bt470m`, `Bt470bg`, `Smpte170m`, `Smpte240m`,
`Film`, `Bt2020`, `Smpte428` (XYZ), `Smpte431` (DCI-P3, DCI white), `Smpte432` (Display P3, D65),
`Ebu3213`) lives in `vaco-color` as spec-derived constants.

**Primaries conversion happens in linear light**, so it forces the transfer pair to be emitted. This
is why `sws_scale` between BT.709 and BT.2020 content is dramatically more expensive than a
same-primaries conversion, and it is not something we can optimise away — it is physics.

### A.8.6 Tone mapping and the four intents

`intent` selects the rendering intent when the source gamut/dynamic range exceeds the destination's.
Following the ICC model that the option names come from:

| Intent | Behaviour |
|---|---|
| `absolute_colorimetric` | No white-point adaptation, no tone curve. Colours inside the destination gamut are reproduced exactly; out-of-gamut colours are clipped to the gamut boundary along the shortest path in the working space. |
| `relative_colorimetric` (default) | White-point adapted (Bradford, §A.8.5), then as absolute. In-gamut colours exact; out-of-gamut clipped. |
| `perceptual` | Smooth compression: a BT.2390 EETF luminance roll-off plus a soft chroma compression toward the gamut boundary. Nothing clips; everything shifts slightly. Correct for photographic/video content. |
| `saturation` | Preserves relative saturation: the source gamut boundary maps to the destination gamut boundary, with hue preserved and lightness scaled. Correct for graphics and charts, wrong for photographs. |

**Luminance tone mapping (used by `perceptual`, and by any intent when peak luminance decreases).**
We implement the ITU-R BT.2390 EETF: a Hermite spline in PQ-encoded luminance with a knee point
`kS = 1.5·maxLum - 0.5`, mapping source peak to destination peak while leaving everything below the
knee untouched. Source peak comes from `MasteringDisplay` metadata when present, else from
`ContentLightLevel`, else from the transfer function's nominal peak (10000 nits for PQ, 1000 for HLG
at nominal system gamma). Destination peak comes from the destination's mastering metadata or a
default of 100 nits for SDR.

**Gamut mapping.** Out-of-gamut handling operates in ICtCp, which is close enough to perceptually
uniform that "shortest path to the gamut boundary" means something sensible and hue is preserved by
holding the Ct/Cp angle constant. For `perceptual` and `saturation` the mapping is a smooth
compression of the chroma magnitude; for the colorimetric intents it is a hard clip at the boundary.

**Implementation as a 3D LUT.** All of this is expensive per-pixel and smooth in the input, which is
exactly the case a 3D LUT is for. At plan time we evaluate the full chain (transfer → primaries →
tone map → gamut map → inverse transfer) on a `N × N × N` grid (default `N = 33`, option
`lut3d_size`, range 9–65) and emit a single `Lut3D` op with **tetrahedral interpolation**.
Tetrahedral rather than trilinear because trilinear does not preserve the neutral axis: a grey input
must produce a grey output, and trilinear interpolation of a non-linear function does not guarantee
that, while tetrahedral (which decomposes the cube into six tetrahedra all sharing the black-white
diagonal) does exactly.

The LUT build is `N³` evaluations of a moderately expensive function — 36k evaluations at `N = 33`,
a few milliseconds. It is cached in the graph and keyed on the full colour spec pair, so a video
stream pays it once.

When `intent` is colorimetric, the transfer characteristics match, and the primaries match, no
`Lut3D` is emitted at all — the common case costs nothing.

## A.9 Dithering

Dither is emitted whenever the working precision exceeds the destination depth. The methods from the
inventory, all as `DitherOp`:

```rust
#[derive(Clone, Debug)]
pub enum DitherOp {
    None,
    Bayer { order: u8 },                   // matrix is 2^order square
    ErrorDiffusion,
    ArithAdd,                              // "a_dither"
    ArithXor,                              // "x_dither"
}
```

`auto` is resolved at plan time, not represented in the op: **`None`** when the depth drop is zero;
**`Bayer { order: 3 }`** (8×8) when dropping ≤ 4 bits, which is the common 16→8 and 10→8 case;
**`ErrorDiffusion`** when dropping more than 4 bits, where ordered dither's pattern becomes visible.

### A.9.1 Bayer

The threshold matrix is generated recursively, never tabulated:

```
  M₁ = [0]
  M_{2n} = [ 4·M_n + 0    4·M_n + 2 ]
           [ 4·M_n + 3    4·M_n + 1 ]
```

The dither value added before truncation is
`t(x, y) = ((M[y mod n][x mod n] + 0.5) / n² - 0.5) · q`, with `q` the quantisation step
`2^(working_bits - dst_bits)`. Class A: `t` is a pure function of `(x, y)` and the plane, so it is
identical regardless of slicing, threading, or lane width. The matrix is generated at plan time into
a small `[i16; 64]`; at `BLOCK` granularity the `x mod n` pattern is a repeating vector, so the SIMD
kernel loads one pre-broadcast row vector per row and adds it — essentially free.

### A.9.2 Arithmetic dither

Both `a_dither` and `x_dither` are stateless hashes of position, which is what makes them the fastest
methods and the only ones that are trivially parallel:

```
  a_dither: t(x, y, c) = (( (x + c·17) * 0x2545F491 ^ (y * 0x9E3779B1) ) >> 20) & (q - 1)
  x_dither: t(x, y, c) =  ( (x ^ (y * 0x45D9F3B)) * 0x27220A95 >> 20 ) & (q - 1)
```

The exact hash constants are ours (chosen for good low-bit avalanche and verified with a
chi-squared uniformity test over a 4096×4096 grid), not transcribed. This means our arithmetic dither
patterns will not be bit-identical to the reference binary's. That is an explicit, reviewed entry on
the D6 divergence allowlist: for `sws_dither ∈ {a_dither, x_dither}` the harness compares against a
PSNR floor and a noise-spectrum test rather than byte-exactly. Both are Class A.

### A.9.3 Error diffusion

Floyd–Steinberg, with the standard `7/16, 3/16, 5/16, 1/16` kernel distributing the quantisation
residual to the right and the three below-adjacent pixels, and serpentine row ordering (alternating
left-to-right and right-to-left) to break up the directional artefacts plain raster order produces.

This is **the one stateful op** and **the one exception to thread-count independence**. The
dependency chain is: pixel `(x, y)` depends on `(x-1, y)`, and row `y` depends on row `y-1`. That
serialises both axes.

Our resolution:

- Error diffusion forces the pass containing it to run **single-threaded over rows**, regardless of
  the `threads` option. Slice threading still applies to *other* passes in the graph.
- Consequently error diffusion **is** thread-count independent and Class A in our implementation. We
  pay throughput for that guarantee. The alternative — per-band error state reset, which is what a
  naively parallel implementation does — produces visible band seams and non-reproducible output, and
  we reject it.
- We log at `verbose` when error diffusion has forced serialisation, so the cost is discoverable
  rather than mysterious.

## A.10 Alpha

Two orthogonal concerns.

**Alpha mode** (`AlphaMode::{Premultiplied, Straight}`, from `vaco-color`). Converting straight →
premultiplied is `c' = c · a`; premultiplied → straight is `c' = c / a` with `a = 0` producing `0`
(the only sane choice, since the colour is genuinely undefined there). Both are per-pixel, and
premultiply is a `Linear`-shaped op only in the degenerate sense — it is a *product of two
components*, which the `Linear` vocabulary cannot express. So it gets its own lowering: a dedicated
`Swizzle` + multiply pattern recognised by the chain compiler. Rather than adding a `Mul` op to the
vocabulary (which would open the door to a general expression IR we do not want), premultiply is
represented as a `Lut3D` in the degenerate 2-D case for float paths and as a fused kernel pattern for
integer paths.

**Important correctness point:** resampling must happen in *premultiplied* space. Filtering straight
alpha independently of colour produces halos around transparent edges. So when alpha is present and
a filter is emitted, the builder inserts premultiply before the filter and unpremultiply after — and
the optimiser cancels the pair when source and destination are both premultiplied.

**Alpha blend** (`alphablend` option), applied when the destination format has no alpha channel and
the source does:

| Mode | Behaviour |
|---|---|
| `none` | Discard alpha. `Swizzle` drops the channel; dead-component elimination then removes its whole producer chain. Fast and usually wrong. |
| `uniform` | Composite over a solid background: `c' = c·a + bg·(1-a)`. `bg` defaults to black; option `alphablend_color` sets it. |
| `checkerboard` | Composite over an 8×8 checkerboard of two greys (0.4 and 0.6 in linear light). Position-dependent `bg`, so a `(x, y)`-derived constant, same machinery as bayer dither. |

Both blend modes are affine in `(c, a)` given `bg`, so once premultiply has been applied they reduce
to `c' = c_premul + bg·(1 - a)` — a `Linear` with a position-varying constant term, which the
`Linear` op supports by allowing column 4 to be a small per-`x mod 8` table.

## A.11 Public API

### A.11.1 Construction and the frame API

```rust
pub struct Scaler {
    graph: Graph,
    /// Cached compiled graphs keyed on (src spec, dst spec, opts hash, palette hash).
    cache: GraphCache,
    pool: BufferPool,
    threads: ThreadConfig,
}

pub struct ScalerBuilder { /* … */ }

impl Scaler {
    pub fn builder() -> ScalerBuilder;

    /// Build for a fixed conversion. The common case.
    pub fn new(src: &ImageSpec, dst: &ImageSpec, opts: &ScaleOptions) -> Result<Self>;

    /// Whole-frame conversion. Reconfigures automatically if the frames' specs differ
    /// from the current graph (equivalent to sws_scale_frame + the cached-context idiom).
    pub fn scale_frame(&mut self, src: &Frame, dst: &mut Frame) -> Result<()>;

    /// True when the configured conversion would copy planes unchanged.
    pub fn is_noop(&self) -> bool;

    /// Introspection: the compiled op sequence, for `-v debug` and for tests.
    pub fn explain(&self) -> GraphDump;
}
```

Capability queries, mirroring the inventory's `sws_test_*` family:

```rust
pub fn supports_input(fmt: PixelFormat) -> bool;
pub fn supports_output(fmt: PixelFormat) -> bool;
pub fn supports_color(spec: &ColorSpec) -> bool;
pub fn supports_conversion(src: &ImageSpec, dst: &ImageSpec) -> bool;
```

These are real predicates, evaluated by attempting graph construction against a stub and reporting
whether it succeeded, rather than a hand-maintained table that can drift from reality.

### A.11.2 The slice API

```rust
/// A streaming conversion session. Borrows the scaler for its lifetime.
pub struct SliceSession<'a> { /* … */ }

impl Scaler {
    /// Begin a streaming conversion between two frames.
    pub fn frame_start<'a>(&'a mut self, src: &'a Frame, dst: &'a mut Frame)
        -> Result<SliceSession<'a>>;
}

impl<'a> SliceSession<'a> {
    /// Provide `height` source rows starting at `y`. Rows must arrive in order.
    pub fn send_slice(&mut self, y: u32, height: u32) -> Result<()>;
    /// Request `height` destination rows starting at `y`. Returns the number produced,
    /// which may be less than requested if more input is needed.
    pub fn receive_slice(&mut self, y: u32, height: u32) -> Result<u32>;
    /// Rows of input required before `dst_y` can be produced.
    pub fn input_needed(&self, dst_y: u32) -> Range<u32>;
    pub fn finish(self) -> Result<()>;
}
```

Plus the raw plane form, for callers that do not have a `Frame` (the filter graph's in-place paths,
and `vaco-play`'s renderer):

```rust
pub fn scale_planes(
    &mut self,
    src: PlanesRef<'_>, src_stride: &[usize], src_y: u32, src_h: u32,
    dst: PlanesMut<'_>, dst_stride: &[usize],
) -> Result<u32>;
```

The `SliceSession` lifetime tying both frames is deliberate: it makes "mutate the source mid-slice"
a compile error, which is a real class of bug in the C API this replaces.

### A.11.3 Slice threading

Architecture §6 axis 3 (data parallelism, safe by disjoint mutable slice splitting).

```rust
pub enum ThreadConfig { Auto, Fixed(usize), Serial }
```

`Auto` resolves from `std::thread::available_parallelism()` clamped to
`min(available, dst_height / MIN_BAND_ROWS)` with `MIN_BAND_ROWS = 16` — a band smaller than the
vertical filter's support is pure overhead. Per D8/architecture §6, we do **not** inherit a 16-thread
constant; the clamp is derived from the work available, and the crossover is measured.

Mechanics:

1. The destination is split into `n` row bands via `chunks_mut` on each plane. Disjointness is proved
   by the type system, so no unsafe is needed.
2. Each band computes its required source row range from the vertical filter's support:
   `src_rows(band) = [offsets[first_dst] , offsets[last_dst] + taps)`. Bands overlap on input, which
   is fine — the source is shared immutably.
3. Each band gets its own intermediate buffers from the pool and its own dither state (irrelevant for
   the stateless methods; error diffusion has already forced `Serial`).
4. `rayon::scope` (or a `std::thread::scope`, since the work is a simple fork-join) runs them.

**Reproducibility:** because every destination row is a pure function of a source row window, band
boundaries cannot affect output. This is asserted by a test that runs every conversion in the
benchmark matrix at 1, 2, 3, 5 and 8 threads and requires byte-identical output. Thread count is not
an excuse for divergence.

Note that `threads` is a `SwsContext` option in the inventory and we keep it, but the filter-graph
integration will usually set `Serial` and parallelise at the filter level instead, to avoid nested
thread pools.

## A.12 Option surface

Complete mapping of the inventory's `SwsContext` option table to `vaco-opts`. Every name and alias is
preserved.

```rust
#[derive(Options, Clone, Debug)]
#[opts(name = "sws")]
pub struct ScaleOptions {
    // ── legacy algorithm bitmask ────────────────────────────────────────────
    #[opt(name = "sws_flags", unit = "sws_flags", default = "bicubic",
          help = "legacy scaler algorithm and modifier flags")]
    pub flags: SwsFlags,

    // ── new-style scaler selection (preferred) ──────────────────────────────
    #[opt(name = "scaler", unit = "sws_scaler", default = "auto",
          help = "luma scaling algorithm")]
    pub scaler: Scaler2,
    #[opt(name = "scaler_sub", unit = "sws_scaler", default = "auto",
          help = "chroma (subsampled plane) scaling algorithm")]
    pub scaler_sub: Scaler2,
    #[opt(name = "param0", default = f64::NAN, help = "scaler parameter 0")]
    pub param0: f64,
    #[opt(name = "param1", default = f64::NAN, help = "scaler parameter 1")]
    pub param1: f64,

    // ── geometry and format ─────────────────────────────────────────────────
    #[opt(name = "srcw", default = 0, min = 0)] pub src_w: i32,
    #[opt(name = "srch", default = 0, min = 0)] pub src_h: i32,
    #[opt(name = "dstw", default = 0, min = 0)] pub dst_w: i32,
    #[opt(name = "dsth", default = 0, min = 0)] pub dst_h: i32,
    #[opt(name = "src_format")] pub src_format: PixelFormat,
    #[opt(name = "dst_format")] pub dst_format: PixelFormat,
    #[opt(name = "src_range", default = false)] pub src_range_full: bool,
    #[opt(name = "dst_range", default = false)] pub dst_range_full: bool,

    // ── colour ──────────────────────────────────────────────────────────────
    #[opt(name = "gamma", default = false, help = "gamma-correct scaling")]
    pub gamma: bool,
    #[opt(name = "intent", unit = "intent", default = "relative_colorimetric")]
    pub intent: Intent,
    #[opt(name = "src_v_chr_pos", default = -513, min = -513, max = 1024)]
    pub src_v_chr_pos: i32,
    #[opt(name = "src_h_chr_pos", default = -513, min = -513, max = 1024)]
    pub src_h_chr_pos: i32,
    #[opt(name = "dst_v_chr_pos", default = -513, min = -513, max = 1024)]
    pub dst_v_chr_pos: i32,
    #[opt(name = "dst_h_chr_pos", default = -513, min = -513, max = 1024)]
    pub dst_h_chr_pos: i32,

    // ── output conditioning ─────────────────────────────────────────────────
    #[opt(name = "sws_dither", unit = "sws_dither", default = "auto")]
    pub dither: Dither,
    #[opt(name = "alphablend", unit = "alphablend", default = "none")]
    pub alphablend: AlphaBlend,

    // ── execution ───────────────────────────────────────────────────────────
    #[opt(name = "threads", unit = "threads", default = 0, min = 0, max = 1024,
          help = "worker threads (0 = auto)")]
    pub threads: i32,
    #[opt(name = "sws_backends", unit = "sws_backend", default = "all")]
    pub backends: BackendMask,

    // ── vaco extensions (not in the reference; documented as such) ──────────
    #[opt(name = "precision", unit = "sws_precision", default = "auto",
          help = "force the internal working precision (vaco extension)")]
    pub precision: Precision,
    #[opt(name = "max_taps", default = 64, min = 2, max = 256,
          help = "cap on filter tap count before two-stage decimation (vaco extension)")]
    pub max_taps: i32,
    #[opt(name = "lut3d_size", default = 33, min = 9, max = 65,
          help = "gamut/tone-mapping LUT grid size (vaco extension)")]
    pub lut3d_size: i32,
    #[opt(name = "bitexact", default = false,
          help = "force reproducible float evaluation order (vaco extension)")]
    pub bitexact: bool,
}
```

Enum surfaces, named exactly as the inventory lists them so `-h` output and `key=value` parsing match:

```rust
#[derive(Options, Copy, Clone, Debug)] #[opts(unit = "sws_scaler")]
pub enum Scaler2 { Auto, Bilinear, Bicubic, Point, Area, Gaussian, Sinc, Lanczos, Spline }

#[derive(Options, Copy, Clone, Debug)] #[opts(unit = "sws_dither")]
pub enum Dither { None, Auto, Bayer, Ed, ADither, XDither }

#[derive(Options, Copy, Clone, Debug)] #[opts(unit = "alphablend")]
pub enum AlphaBlend { None, Uniform, Checkerboard }

#[derive(Options, Copy, Clone, Debug)] #[opts(unit = "intent")]
pub enum Intent { Perceptual, RelativeColorimetric, Saturation, AbsoluteColorimetric }

bitflags! {
    pub struct SwsFlags: u32 {
        const FAST_BILINEAR = 1 << 0;  const BILINEAR = 1 << 1;
        const BICUBIC       = 1 << 2;  const X        = 1 << 3;
        const POINT         = 1 << 4;  const AREA     = 1 << 5;
        const BICUBLIN      = 1 << 6;  const GAUSS    = 1 << 7;
        const SINC          = 1 << 8;  const LANCZOS  = 1 << 9;
        const SPLINE        = 1 << 10;
        const PRINT_INFO    = 1 << 12; const FULL_CHR_H_INT = 1 << 13;
        const FULL_CHR_H_INP= 1 << 14; const ACCURATE_RND   = 1 << 18;
        const BITEXACT      = 1 << 19; const ERROR_DIFFUSION= 1 << 23;
        const DIRECT_BGR    = 1 << 24; const UNSTABLE = 1 << 25; const STRICT = 1 << 26;
    }
}
```

**Legacy-flag normalisation.** `sws_flags` and `scaler`/`scaler_sub` are two spellings of the same
thing. Parsing resolves them once, before graph construction: if `scaler` is `Auto` and `sws_flags`
names an algorithm, the flag wins; otherwise `scaler` wins. `BICUBLIN` expands to
`scaler=bicubic, scaler_sub=bilinear`. `X` (the "experimental" scaler) maps to `gaussian` with a
deprecation warning. `DIRECT_BGR` is accepted and ignored (it is documented as a no-op retained for
ABI, and we have no ABI). `ACCURATE_RND` maps to our default rounding, which is always the accurate
one — we do not ship an inaccurate fast rounding mode, so the flag is accepted and ignored, and that
is a divergence worth recording. `BITEXACT` maps to `bitexact = true`.

`sws_backends` is accepted and largely ignored: we have one backend. `MEMCPY` disables the
memcpy fast path (useful for testing), `LEGACY`/`STABLE`/`C`/`X86`/`AARCH64` are accepted as no-ops,
`SPIRV` is rejected with a clear "not implemented" diagnostic rather than silently ignored, since a
caller asking for GPU conversion needs to know they did not get it.

## A.13 Reproducibility guarantees

Summarising §0.5's classes as applied here:

| Path | Class | Notes |
|---|---|---|
| Integer chains (precision rules 3–5): format conversion, range conversion, YCbCr matrices, integer resampling, bayer/arithmetic dither | **A** | Bit-exact across arch, lane width, thread count, band split. The default for the large majority of real conversions. |
| Float chains with `bitexact` | **B** | Canonical evaluation order, no FMA, fixed accumulation tree, transfer functions on the scalar reference path. |
| Float chains by default | **C** | ≤ 2 ULP relative on linear ops; ≤ 1e-6 absolute on transfer-function evaluation; ≤ 1 LSB of the destination depth end-to-end, which is the bound the tests actually assert since it is the one users can observe. |
| Error diffusion | **A** (forced serial) | See §A.9.3. |
| `a_dither` / `x_dither` | **A**, but not reference-identical | Documented D6 allowlist entry; compared by PSNR floor and noise spectrum. |

**Enforcement.** Class A is enforced by a test that runs each conversion under every combination of
`{scalar, 128, 256, 512}` × `{1, 2, 3, 5, 8}` threads and requires byte equality. Class B is enforced
the same way with `bitexact = true`. Class C is enforced by an error-bound assertion against an `f64`
reference model computed by an independent straightforward implementation (not the SIMD path with
wider types — a genuinely separate implementation, so the test can catch a wrong formula and not just
a wrong rounding).

## A.14 Build-or-buy assessment (D10)

Two candidates were named: `yuv` (yuvutils-rs) and `dcv-color-primitives` (AWS). Both are relevant
prior art and both are worth reading for *approach*, which is not a clean-room concern since neither
is FFmpeg-derived.

### A.14.1 `yuv` (yuvutils-rs)

| Gate | Finding |
|---|---|
| 1. Pure Rust, zero FFI | **Pass.** No `-sys`, no build script compiling C. |
| 2. Licence | **Pass.** BSD-3-Clause OR Apache-2.0 — both on D3's allowlist. |
| 3. Trusted/maintained | **Pass, with a note.** Actively developed, growing adoption, no RUSTSEC advisory, shallow tree. `cargo-geiger` will report substantial `unsafe`: the crate uses `core::arch` intrinsics for its NEON/AVX paths, which is exactly the D10 "unsafe tension" case and is a real input, not a disqualifier. |
| 4. Model fit | **Fail, decisively.** |

The model-fit failure is structural, not a quality judgement:

- **It is a per-format-pair library.** Its API is a set of functions like
  `yuv420_to_rgb(planes, strides, …)` — precisely the architecture that `planning/10-architecture.md`
  §10 rejects and that §A.1 of this plan replaces. Adopting it would mean maintaining both
  architectures, with the ops graph handling everything the crate does not cover and the crate
  handling a shrinking privileged subset. That is the worst of both.
- **Its format coverage is a small fraction of ours.** It covers the common 8/10/12-bit YUV↔RGB
  conversions well. The `vaco-pixfmt` surface is ~268 formats including Bayer, palette, XYZ,
  half-float, packed 10/12-bit in exotic bit layouts, and every endianness variant. The long tail is
  most of the work, and the crate does not address it.
- **No scaling at all.** Resampling, filter banks, chroma siting — none of it. That is §A.7 in full.
- **No colour management.** No transfer functions, no primaries conversion, no tone mapping, no
  intents. That is §A.8 in full, and it is the part with the most specification surface.
- **Rounding is its own.** Its conversions were written to be correct, not to be byte-identical to
  `ffmpeg`. Since byte-identical output on integer conversions is a stated goal (§A.15), we would be
  fighting a dependency's rounding decisions from outside — the exact scenario D11's preamble
  describes.

**Verdict: do not adopt.** Read it for kernel-shape ideas (its coefficient blocking and its
saturating-pack sequences are instructive), record that reading in the PR provenance trailer, and
write our own.

### A.14.2 `dcv-color-primitives`

| Gate | Finding |
|---|---|
| 1. Pure Rust, zero FFI | **Pass.** |
| 2. Licence | **Pass.** MIT-0, the most permissive entry on D3's list — no attribution required. |
| 3. Trusted/maintained | **Marginal.** AWS-authored with a real track record, but development is intermittent and the crate is narrow. Uses `core::arch` intrinsics, so the same unsafe note applies. |
| 4. Model fit | **Fail.** |

Narrower than `yuv` in every dimension: a handful of conversions (chiefly BGRA↔I420/NV12) aimed at
a specific video-pipeline use case, no scaling, no colour management, no bit-depth range. Everything
said above applies more strongly.

**Verdict: do not adopt.**

### A.14.3 Is there a defensible partial adoption?

The coordinator's framing is right that partial adoption is a legitimate outcome, so it deserves a
straight answer rather than a reflexive no. The place it could work is a **leaf kernel**: a fixed,
option-free conversion such as 8-bit `yuv420p → rgb24` at BT.709 limited range, which is a
self-contained function with no configuration surface.

We still decline, for one concrete reason: in the ops-graph architecture that conversion **is not a
leaf**. It is `Read · Swizzle · Convert · Linear(Affine3) · Min · Max · Convert · Pack · Write`, and
§A.6.2's fused-pattern mechanism already compiles it into a single kernel. Substituting an external
function for one privileged path would mean that path bypasses the optimiser, the precision pass, the
interval analysis and the checkasm differential harness — losing exactly the properties the
architecture exists to provide, in exchange for saving one kernel out of a few dozen.

**What we do take from these crates instead:** they are permissively licensed, clean-room-safe prior
art demonstrating that pure-Rust colour conversion reaches competitive throughput. That is a useful
data point for the D2 bet (§0.2), and their published benchmark numbers are a legitimate secondary
target alongside the reference binary.

### A.14.4 Dependencies `vaco-scale` does take

`rayon` (slice parallelism, §A.11.3), `bitflags` (`SwsFlags`), `thiserror` (error taxonomy), plus
workspace-internal `vaco-core`, `vaco-simd`, `vaco-opts`, `vaco-pixfmt`, `vaco-color`, `vaco-frame`,
`vaco-pool`. Dev-dependencies: `proptest`, `criterion`. Nothing media-specific, so D11's
single-occurrence rule has nothing to enforce here — a fact worth stating explicitly so the CI check's
empty result for this crate is understood as correct rather than as a gap.

## A.15 Fidelity assessment — what "identical to ffmpeg" means here

This is the most delicate of the three crates, because scaling and colour conversion have no
normative output. A codec has a bitstream specification that says what the decoded samples must be.
A scaler has nothing: the reference tool's output *is* the definition of correct, and that output is
the accumulation of a long series of implementation-defined choices — coefficient quantisation,
intermediate precision, rounding direction, the order of range and matrix application, chroma phase
conventions, dither patterns.

So the question "can we be byte-identical" decomposes into "can we reverse-engineer each of those
choices from black-box observation", which D7 permits and D6's harness is built for. The answer
differs sharply by conversion class.

### A.15.1 Per-class verdict

| Class | Byte-identical achievable? | Confidence | What has to be matched |
|---|---|---|---|
| **1. Pure layout change** — plane reorder, packing, endianness swap, planar↔packed at the same depth (`rgb24→bgr24`, `yuv420p→nv12`, `rgba→bgra`) | **Yes, certainly** | Very high | Nothing numeric. These are permutations; any correct implementation agrees. |
| **2. Bit-depth change by shift** — `yuv420p10le→yuv420p16le` and back, no dither | **Yes** | High | Only whether widening replicates bits (`255→65535`) or shifts (`255→65280`). One observation pins it; §A.3's `Expand` already models both. |
| **3. Integer colour matrix + range, no resize** — `yuv420p→rgb24`, `yuv420p→yuv420p` range change | **Yes, with effort** | Medium-high | Coefficient quantisation shift, accumulator width, rounding direction, and whether range and matrix are applied fused or separately. Four unknowns, each determined by a handful of probe conversions. Our `Linear` fusion (§A.5.3) can be forced to match whichever grouping the reference uses. |
| **4. Integer resampling, no colour change** — `bilinear`/`bicubic`/`lanczos` scale within one format | **Yes, with significant effort** | Medium | Everything in class 3 *plus* the coefficient generator: filter centre convention, `xscale` handling, edge clamping, tap-count selection, normalisation, and the residual-distribution rule (§A.7.2 step 4). Each is individually observable by scaling a delta image and reading off the impulse response — an unusually direct probe, which is why the confidence is medium rather than low. |
| **5. Integer resampling + colour change** | **Yes, conditionally** | Medium-low | Classes 3 and 4 compose, *plus* the op ordering: whether the reference filters before or after the matrix. A different order gives different rounding. Determined by probe, then pinned in our builder. |
| **6. Chroma subsampling change** — `yuv444p→yuv420p` and back | **Yes** | Medium | Class 4 plus the siting convention and the default `*_chr_pos` values (§A.4.4). Siting bugs are visually obvious and numerically small, so they are easy to detect and easy to get subtly wrong. |
| **7. Ordered / arithmetic dither** | **Bayer yes; `a_dither`/`x_dither` no** | High (bayer), N/A (arith) | Bayer's recursive construction is canonical, so agreement is likely once the threshold scaling is pinned. The arithmetic methods depend on a specific hash whose constants are authorial choice — §A.9.2 declares ours different by construction. |
| **8. Error-diffusion dither** | **Probably, but fragile** | Low-medium | Floyd–Steinberg's kernel is canonical, but row ordering (serpentine or not), error accumulator precision, and slice-boundary handling are all free choices, and any mismatch propagates across the whole image rather than staying local. A single wrong pixel diverges everything to its right and below. |
| **9. Gamma-correct scaling (`gamma=1`)** | **No** | High confidence in the negative | Requires matching transfer-function evaluation bit-for-bit, which means matching the reference's LUT sizes, interpolation and float rounding. Tolerance-based. |
| **10. Primaries conversion, HDR tone mapping, any intent other than clip** | **No** | High confidence in the negative | Float throughout, transcendental functions, a 3D LUT whose grid size and interpolation are free parameters, and a tone curve with several defensible parameterisations. Tolerance-based, and the tolerance is not tight. |
| **11. `fast_bilinear`** | **Yes** | Medium | A small integer DDA with few free parameters (fraction width, rounding). Easy to pin, easy to verify. |

### A.15.2 What this means operationally

**Classes 1–6, 7-bayer and 11 are targeted at grade Exact.** Together they are the large majority of
real-world conversions — every stream-copy-adjacent format change, every SDR transcode, every
playback-path conversion. Committing to byte-exactness here is realistic and is what makes the D6
harness a sharp instrument rather than a fuzzy one. Each of the free parameters listed above becomes
a **pinned constant with a recorded probe**: a small committed file naming the observation that
determined it, the date, and the reference binary version. When a future reference version changes a
rounding rule, the harness fails and the probe file tells us exactly which assumption broke.

**Classes 9–10 are grade Equivalent, with declared tolerances:**

| Comparison | Tolerance | Rationale |
|---|---|---|
| Gamma-correct scaling, SDR | max abs diff ≤ 1 LSB at the output depth; ≥ 60 dB PSNR | 1 LSB is the smallest bound that survives a legitimate difference in transfer-function LUT precision. Anything tighter would be asserting we matched an implementation detail we deliberately did not. |
| Primaries conversion, SDR | max abs diff ≤ 2 LSB; ≥ 55 dB PSNR | Two matrix multiplications plus a transfer round trip; 2 LSB is the accumulation of 1-LSB-class error at each. |
| HDR tone mapping, any intent | ≥ 40 dB PSNR **and** ΔE₀₀ ≤ 1.0 over a reference colour-patch set | PSNR alone is the wrong metric for tone mapping — a small hue shift can be perceptually large and numerically small, or vice versa. ΔE₀₀ (CIEDE2000) is the defensible perceptual bound, and 1.0 is the conventional just-noticeable-difference threshold. |
| `a_dither` / `x_dither` | ≥ 45 dB PSNR against the undithered ideal, plus a noise-spectrum test (flat to within 3 dB across the band) | We are not matching the pattern; we are asserting our dither is *as good*. The spectrum test is what makes that claim real rather than rhetorical. |

**Class 8 (error diffusion) is the one to watch.** It is the only class where we expect Exact but
would not be surprised by Divergent, and where a divergence is total rather than partial. It is
sequenced early in A13 (test infrastructure) precisely so we find out early. If it lands Divergent,
the fallback is to declare it Equivalent under the same PSNR-plus-spectrum bound as the arithmetic
methods, which is defensible because error-diffusion dither is a *rendering* choice, not a
reconstruction of a defined signal.

### A.15.3 The honest summary

Byte-identical output is achievable for the integer paths and is not achievable for the float paths.
That split is not a limitation we could engineer away with more effort — it is inherent, because the
float paths involve transcendental evaluation and free parameters (LUT grid sizes, polynomial fits,
tone-curve parameterisation) where matching would mean reverse-engineering a specific numerical
implementation rather than a defined transformation. Chasing bit-exactness there would trade real
quality and real speed for a metric that no user perceives.

What we commit to instead: **byte-exact where the operation is exactly defined, tightly-bounded and
independently-justified where it is not, and never a tolerance chosen to make a failing test pass.**
Every tolerance in the table above has a reason attached, and a tolerance change is a reviewed diff.

## A.16 Test strategy

**Unit.** Per-op kernel tests against hand-computed expected values, including the degenerate cases
(zero-width filters, single-tap banks, `CompMask` with one live component).

**Property (proptest).**
- Round-trip: `fmt → rgb24 → fmt` is within a bounded error for every format pair; exact for formats
  where the round trip is information-preserving.
- Identity: `scale(x, same spec) == x` for every format.
- Constant preservation: a constant-valued image scales to the same constant, for every algorithm and
  every scale ratio. This is the test that catches normalisation bugs, and it is the single most
  valuable property test in the crate.
- Separability: `scale(w,h) == scale(w,h0) ∘ scale(w0,h)` within the float bound.
- Optimiser soundness: for a randomly generated graph and random inputs, optimised and unoptimised
  execution agree bit-for-bit in integer paths and within bounds in float paths. Run per-pass, with
  each pass individually toggled, so a failure names the guilty pass.
- Interval soundness: for a random graph, every observed value lies inside the interval the
  propagation pass claimed.
- Slice invariance: `scale_frame(x) == concat(scale over random slice partition)`.

**Differential against the reference binary (D6).** The harness runs
`ffmpeg -f rawvideo -pix_fmt SRC -s WxH -i in.raw -vf "scale=W2:H2:flags=ALG:..." -f rawvideo -pix_fmt DST -` and
compares to ours. Matrix:

- Formats: every format pair in a curated set of ~120 pairs covering all families (planar/packed YUV
  at 8/10/12/16 bits, all subsamplings, all RGB packings, gray, palette, XYZ, Bayer, alpha and
  non-alpha), plus a randomly sampled 500-pair subset per CI run drawn from the full matrix with a
  seed derived from the commit, so coverage of the long tail accumulates over time.
- Sizes: identity, 2× up, 2× down, 3:2, 1.01× (worst case for phase), 16× down (tap cap path),
  1×1 and 1×N degenerate.
- Algorithms: all ten, with default and non-default `param0`/`param1`.
- Colour: every `(matrix, primaries, transfer, range)` combination that appears in real content, plus
  a randomised sample of the rest.

Expected result: **byte-exact** for integer paths; PSNR floor plus max-absolute-difference bound for
float paths, with the bound recorded per conversion class in a reviewed file so a regression shows as
a bound change in the diff.

**Fuzzing (D6).** Targets: (1) `ScaleOptions` string parsing; (2) graph construction over arbitrary
`(PixelFormat, ColorSpec, w, h)` pairs via `arbitrary`, asserting either a clean plan or a clean
error and never a panic or an unbounded allocation; (3) the optimiser, fed randomly generated graphs,
asserting termination and semantic preservation; (4) execution with adversarial strides (negative,
huge, overlapping) and adversarial slice sequences.

**checkasm-equivalent.** Every kernel, every tier, randomised and edge-case inputs (all-zero,
all-max, alternating, single-bit), compared against the scalar reference.

## A.17 Benchmarks

Per research §08 §5 scenario 3 (single-threaded, which is the fair comparison since the reference's
scaling is rarely threaded), reporting MB/s and cycles/pixel:

| # | Scenario | Why |
|---:|---|---|
| 1 | `yuv420p → rgb24`, 1920×1080, no resize | The canonical playback conversion. §08 names it. |
| 2 | `yuv420p → nv12`, 1920×1080, no resize | The canonical hardware-encode feed. Should hit the near-memcpy path. |
| 3 | `yuv420p → yuv420p`, 1920×1080 → 1280×720, bicubic | The canonical transcode downscale. |
| 4 | `yuv420p → yuv420p`, 1280×720 → 1920×1080, lanczos | Upscale, wide taps. |
| 5 | `yuv420p10le → yuv420p`, 1920×1080, dither=auto | Bit-depth reduction with dither. |
| 6 | `rgb24 → yuv420p`, 1920×1080 | Encode-side conversion, chroma downsampling. |
| 7 | `yuv420p10le (bt2020/pq) → yuv420p (bt709/bt709)`, 3840×2160, intent=perceptual | The full HDR tone-mapping path — the most expensive thing the crate does. |
| 8 | `yuv420p → yuv420p`, 3840×2160 → 640×360, bicubic | Extreme downscale, exercises the tap cap. |
| 9 | Same as 3, with `gamma=1` | Isolates the cost of gamma-correct scaling. |
| 10 | Same as 3, at 1/2/4/8/16 threads | Slice-threading scaling curve. |
| 11 | Plan/compile time for a cold `Scaler::new` on scenarios 1–8 | Setup cost matters for short CLI invocations (§08 §5 scenario 7). |

Each runs under `vaco-checkasm --bench` for per-kernel cycles and under criterion for end-to-end, with
CI regression tracking per D8. Frequency scaling and turbo disabled, threads pinned, medians with
variance reported — research §08 §5 item 9.

**Target:** parity (±10%) with the reference binary on scenarios 1–6 by the end of the crate's
implementation window; scenarios 7–8 are allowed to be slower initially since the reference's
tone-mapping path is itself new and not heavily optimised.

## A.18 Effort and work breakdown

| Workstream | Person-weeks | Depends on | Parallel? |
|---|---:|---|---|
| A1. Op vocabulary, `Graph`, `Block`, scalar per-op kernels | 4 | `vaco-pixfmt`, `vaco-color` | — |
| A2. Graph construction (format decomposition, canonical sequence) | 3 | A1 | — |
| A3. Optimiser passes (§A.5.1–A.5.9) | 4 | A1, A2 | yes, with A4/A5 |
| A4. Filter coefficient generators (all ten kernels + banks) | 3 | A1 | yes |
| A5. Colour: matrices, transfer functions, primaries, range | 4 | `vaco-color`, A1 | yes |
| A6. Tone mapping, intents, 3D LUT, tetrahedral interpolation | 3 | A5 | after A5 |
| A7. Dither + alpha | 2 | A1 | yes |
| A8. Chain compiler + dispatch + fused-kernel patterns | 3 | A1, A3 | after A3 |
| A9. SIMD kernels (per-op, all tiers) | 6 | A1, A8, `vaco-simd` | partially parallel |
| A10. SIMD kernels (fused patterns, profile-driven) | 3 | A9 | after A9 |
| A11. Frame + slice APIs, slice threading, buffer pooling | 2 | A2, A8 | after A2 |
| A12. Option surface, parsing, legacy-flag normalisation | 1.5 | `vaco-opts` | yes |
| A13. Test infrastructure: property tests, differential harness integration, fuzz targets | 4 | A2 | overlaps everything |
| A14. Fidelity probes: determine and pin each free parameter in §A.15.1 classes 2–8, with recorded provenance | 3 | A13 | after A13 starts |
| A15. Benchmarks + CI regression tracking | 1.5 | A11 | after A11 |
| A16. Documentation (`docs/scale/*.md` per the repo standard) | 1.5 | all | overlaps |
| **Total** | **48.5** | | |

A14 is new relative to a naive plan and is not optional: it is the work that converts "we implemented
scaling" into "we implemented *this* scaling", and it is the difference between the differential
harness being a gate and being a source of permanently-yellow tests. It is listed separately because
it is easy to under-budget — each pinned parameter is cheap individually, and there are roughly
twenty of them.

**Critical path:** A1 → A2 → A3 → A8 → A9 → A10, which is 23 pw of strictly serial work. With four
engineers the calendar estimate is **~14–16 weeks**; with two, **~26 weeks**. A13 must start with A2,
not after A10 — a differential harness written at the end finds bugs when they are expensive.

**Parallelisation plan (4 engineers):**
- **E1 (core):** A1 → A2 → A3 → A8. The spine. Most senior person.
- **E2 (colour):** A5 → A6 → A7. Largely independent; needs only A1's types.
- **E3 (SIMD):** `vaco-simd` substrate → A4 → A9 → A10 → A15.
- **E4 (surface + quality):** A12 → A13 → A14 → A11 → A16.

The hand-off points that need explicit coordination are A1's type definitions (day one, all four
engineers depend on them — worth a design review before any code) and A8's `Pattern` mechanism, which
is E1's design but E3's consumer.

## A.19 References an implementer works from

- ITU-T H.273 / ISO-IEC 23091-2, *Coding-independent code points for video signal type identification*
  — §8.1 primaries, §8.2 transfer characteristics, §8.3 matrix coefficients, chroma location.
- ITU-R BT.709-6, BT.601-7, BT.2020-2, BT.2100-2 (PQ, HLG, ICtCp), BT.1886 (EOTF for SDR displays).
- ITU-R BT.2390 (EETF tone mapping), BT.2408 (production practice for HDR).
- SMPTE ST 2084 (PQ), ST 428-1 (D-Cinema XYZ), RP 177 (derivation of RGB↔XYZ matrices),
  ST 2128 (IPT-C2).
- IEC 61966-2-1 (sRGB), 61966-2-4 (xvYCC).
- Mitchell & Netravali, "Reconstruction Filters in Computer Graphics", SIGGRAPH 1988.
- Duchon, "Lanczos Filtering in One and Two Dimensions", J. Appl. Meteorology 1979.
- Keys, "Cubic Convolution Interpolation for Digital Image Processing", IEEE ASSP 1981.
- Unser, Aldroubi & Eden, "B-Spline Signal Processing", IEEE Trans. Signal Processing 1993.
- Floyd & Steinberg, "An Adaptive Algorithm for Spatial Greyscale", Proc. SID 1976.
- Bayer, "An optimum method for two-level rendition of continuous-tone pictures", IEEE ICC 1973.
- Ulichney, *Digital Halftoning*, MIT Press 1987 — for the ordered/error-diffusion comparison and the
  serpentine-ordering rationale.
- Lindbloom's published chromatic-adaptation matrices (Bradford/von Kries), and Luo & Hunt's work on
  gamut mapping, for §A.8.5–A.8.6.
- Kang, *Computational Color Technology*, SPIE Press 2006 — tetrahedral interpolation.

---

# PART B — `vaco-resample`

## B.1 Architectural stance: three independent stages

Research §01 §12 identifies the seam: libswresample "cleanly separates three independently portable
stages: format conversion, channel rematrixing, and resampling". Architecture §3 makes that binding.

We take the separation further than the reference does, because in Rust it costs nothing:

```rust
/// Each stage is an independent, independently-testable, independently-usable component.
pub trait Stage {
    type In;
    type Out;
    fn process(&mut self, input: &AudioBuf<Self::In>, out: &mut AudioBuf<Self::Out>) -> Result<usize>;
    /// Samples currently held internally (filter state, partial frames).
    fn delay(&self) -> usize;
    fn reset(&mut self);
}

pub struct FormatConvert { /* … */ }   // §B.3
pub struct Rematrix { /* … */ }        // §B.4
pub struct RateConvert { /* … */ }     // §B.5
pub struct Dither { /* … */ }          // §B.6
```

Each is a public type with a public constructor. A caller who only wants channel downmixing gets
`Rematrix` without dragging in a resampler; a codec that needs `s16 → f32` conversion uses
`FormatConvert` directly. The reference exposes only the fused `SwrContext`; exposing the parts is
strictly better and costs one extra layer of composition.

`Resampler` is that composition:

```rust
pub struct Resampler {
    input:  FormatConvert,     // in_fmt      → internal_fmt
    matrix: Option<Rematrix>,  // in_layout   → out_layout
    rate:   Option<RateConvert>,
    dither: Option<Dither>,
    output: FormatConvert,     // internal_fmt → out_fmt
    comp:   Compensation,      // §B.7
    // …
}
```

### B.1.1 Stage ordering, and why it is not obvious

The ordering `convert → rematrix → resample → dither → convert` is chosen, not inherited. The two
live questions:

**Rematrix before or after rate conversion?** Before. Rematrixing to fewer channels first means the
resampler — by far the more expensive stage — runs on fewer channels. Downmixing 5.1→stereo before
resampling is a 3× saving on the expensive stage. When rematrixing *increases* channel count
(upmix), the same argument reverses, so the builder compares `in_channels` and `out_channels` and
places the rematrix stage on whichever side has fewer channels. This is a genuine optimisation the
staged architecture makes trivial and a fused implementation makes awkward.

**What is the internal format?** Options, in precedence order: (1) the explicit
`internal_sample_fmt`/`tsf` option; (2) `f32` when either endpoint is float, or when rematrixing
needs headroom that fixed point cannot supply; (3) `s16` when both endpoints are `≤ 16` bits and the
rematrix matrix is a pure permutation or has ≤ 2 taps per output; (4) `s32` when either endpoint is
`s32`/`s64`; (5) `f32` otherwise. Rule 3 is the one that matters for throughput on the common
`s16 → s16` path, and it is also the one that makes that path Class A.

## B.2 The buffer model

```rust
/// A planar-or-interleaved audio buffer view. Both layouts are first-class; the
/// crate never internally forces one, because forcing one is a copy.
pub enum AudioBuf<'a, T> {
    Planar { planes: &'a [&'a [T]], len: usize },
    Interleaved { data: &'a [T], channels: usize },
}

pub enum AudioBufMut<'a, T> {
    Planar { planes: &'a mut [&'a mut [T]], len: usize },
    Interleaved { data: &'a mut [T], channels: usize },
}
```

Internally every stage works **planar**, always. Interleaving is a layout concern handled at the
`FormatConvert` boundaries and nowhere else — the same discipline §A.2 applies to pixels, and for the
same reason: planar makes every kernel lanewise with no shuffles.

## B.3 Sample-format conversion

### B.3.1 The matrix, factorised

Six element types (`u8`, `s16`, `s32`, `s64`, `f32`, `f64`) × two layouts = twelve `SampleFormat`
values, so 144 ordered pairs. Implementing 144 kernels is what the reference does and what we
explicitly will not do. The factorisation:

```
  conversion(src_fmt → dst_fmt)  =  deinterleave? ∘ element_convert ∘ interleave?
```

which is 30 non-identity element conversions plus one deinterleave and one interleave per channel
count. In practice we implement:

- **30 element converters** (6×6 minus the diagonal), each a `chunks_exact`-driven lanewise map.
- **Deinterleave / interleave** specialised for `C ∈ {1, 2, 4, 6, 8}` and generic otherwise. Channel
  counts 1 and 2 cover the overwhelming majority; 6 and 8 cover surround.
- **Fused converters** for the highest-traffic pairs (`s16 ↔ f32`, `s16 ↔ s16p`, `f32p → s16`),
  discovered by profile and added exactly as §A.6.2's fused patterns are — additively, with the
  factorised path retained as the differential reference.

### B.3.2 The numeric definitions

These are the crate's contract and must be stated exactly, because every one of them is a rounding
decision that shows up in differential output.

Let `n` be the integer width in bits (`u8` = 8, `s16` = 16, `s32` = 32, `s64` = 64). `u8` is the odd
one out: unsigned with a 128 bias, for historical PCM reasons.

**Integer ↔ integer.** Pure shifts, never multiplies:

```
  narrow (n → m, n > m):  round-half-up  →  (x + (1 << (n-m-1))) >> (n-m), saturating
  widen  (n → m, n < m):  x << (m-n)
  u8 → s16:               ((x as i16) - 128) << 8
  s16 → u8:               (((x + 128) >> 8) + 128) clamped to [0, 255]
```

Round-half-up on narrowing rather than truncation: truncation introduces a DC offset of half an LSB,
which is audible as a click at buffer boundaries in some pathological content and is simply wrong.
The `+ (1 << (n-m-1))` must saturate, because `s32::MAX + rounding` overflows.

**Integer → float.** Scale by the negative power of two, which is exact:

```
  s16 → f32:  x as f32 * (1.0 / 32768.0)          // f32::from(x) is exact; the multiply is exact
  s32 → f32:  x as f32 * (1.0 / 2147483648.0)     // the i32→f32 conversion itself rounds
  s32 → f64:  x as f64 * (1.0 / 2147483648.0)     // exact
  u8  → f32:  (x as f32 - 128.0) * (1.0 / 128.0)
```

Dividing by `2^(n-1)` rather than `2^(n-1) - 1` means full-scale negative maps to exactly `-1.0` and
full-scale positive maps to `32767/32768`, slightly under `+1.0`. That asymmetry is inherent to
two's complement and every audio system lives with it; the alternative (scaling by `2^(n-1) - 1`)
makes the round trip inexact, which is worse.

**Float → integer.** The rounding mode here is the single most consequential decision in the stage:

```
  f32 → s16:  clamp(round_half_away_from_zero(x * 32768.0), -32768, 32767)
```

`round_half_away_from_zero` — not Rust's default `as` cast (truncation toward zero) and not
round-half-to-even. Truncation is wrong for the same DC-offset reason as above. Half-to-even versus
half-away is a genuine coin flip mathematically, and the choice is therefore **determined by probe
against the reference binary** (§B.14.1) and pinned. `f32::round()` implements half-away-from-zero
and vectorises to a single instruction on every target we care about, so if the probe says
half-away we pay nothing; if it says half-to-even we use `round_ties_even()`, which also vectorises.

Clamping is mandatory and must come after rounding, since `x` may legitimately exceed `±1.0` — float
audio is not normalised by definition, and a codec that overshoots must clip, not wrap.

### B.3.3 SIMD shape

Every element converter is a pure lanewise map with a fixed trip count — the easiest possible SIMD
target and the shape research §08 says portable SIMD handles outright.

```rust
fn s16_to_f32<const N: usize>(src: &[i16], dst: &mut [f32])
where LaneCount<N>: SupportedLaneCount
{
    const SCALE: f32 = 1.0 / 32768.0;
    let (chunks_s, tail_s) = src.as_chunks::<N>();
    let (chunks_d, tail_d) = dst.as_chunks_mut::<N>();
    for (s, d) in chunks_s.iter().zip(chunks_d) {
        let v: Simd<i16, N> = Simd::from_array(*s);
        *d = (v.cast::<f32>() * Simd::splat(SCALE)).to_array();
    }
    for (s, d) in tail_s.iter().zip(tail_d) { *d = *s as f32 * SCALE; }
}
```

Three notes on this shape, which recurs throughout the crate:

1. `as_chunks` gives LLVM a provably-sized inner loop with no bounds checks in the body — the
   idiom architecture §7.1 names.
2. The tail is scalar and uses the *same expression*, so the SIMD and scalar paths cannot disagree.
   This is not an optimisation detail; it is what makes the kernel Class A by construction.
3. `cast::<f32>()` on `Simd<i16, N>` is a widening convert; on x86 it lowers to
   `pmovsxwd` + `cvtdq2ps`, on AArch64 to `sshll` + `scvtf`. No intrinsics, no unsafe.

Deinterleave for `C = 2` is a `Simd::deinterleave` pair; for `C = 6`/`8` it is a small register
transpose. The generic path is a strided scalar loop, which is acceptable because non-power-of-two
channel counts above 2 are rare and are dominated by the resampler anyway.

## B.4 Channel rematrixing

### B.4.1 Matrix construction

```rust
pub struct MixLevels {
    /// Linear gain applied to the centre channel when folding it into L/R. Default 1/√2 (−3 dB).
    pub center: f32,
    /// Linear gain applied to surround channels when folding them forward. Default 1/√2.
    pub surround: f32,
    /// Linear gain applied to LFE when folding it in. Default 0.0 — LFE is discarded unless asked for.
    pub lfe: f32,
    /// Overall output scale. Negative means "auto-normalise" (§B.4.4).
    pub rematrix_volume: f32,
    /// Clipping ceiling used by auto-normalisation.
    pub rematrix_maxval: f32,
}

pub fn build_matrix(
    inp: &ChannelLayout,
    out: &ChannelLayout,
    levels: &MixLevels,
    encoding: MatrixEncoding,
) -> Result<MixMatrix>;

/// `m[out_index][in_index]`, dense, f64 at build time.
pub struct MixMatrix {
    pub rows: usize,
    pub cols: usize,
    pub m: Vec<f64>,
    pub shape: MatrixShape,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum MatrixShape {
    /// Every output takes exactly one input with gain 1.0 — a channel permutation.
    Permutation,
    /// Every output takes at most one input, with arbitrary gain.
    Scaled,
    /// Every output takes at most two inputs. Covers most downmixes.
    Sparse2,
    Dense,
}
```

Construction proceeds in four phases, and the order matters:

**Phase 1 — direct copies.** Every output channel identifier also present in the input gets
`m[o][i] = 1.0`. After this phase, a layout-preserving conversion is complete and the shape is
`Permutation`, which is the fast path we care most about.

**Phase 2 — upmix rules**, for output channels with no input counterpart:
- Mono in, anything out: `FrontCenter` feeds `FrontLeft` and `FrontRight`. The gain is a pinned
  constant (1.0 or 1/√2 — an energy-preserving upmix uses 1/√2, a loudness-preserving one uses 1.0);
  §B.14.1 determines it by probe.
- Stereo in, centre out: `FrontCenter = (L + R) · 0.5` only when no `matrix_encoding` is active;
  with an encoding active, §B.4.3's decode governs.
- Surround channels are **not** synthesised from stereo by default. Inventing surround content is a
  creative decision, not a conversion, and belongs in a filter.
- Anything still unfed is silent.

**Phase 3 — downmix rules**, for input channels with no output counterpart, folding each into its
nearest surviving neighbours. Per ITU-R BS.775 and the ATSC A/52 downmix equations, which are the
published sources here:

```
  FrontCenter        → L += center·C,        R += center·C
  BackLeft/SideLeft  → L += surround·Ls
  BackRight/SideRight→ R += surround·Rs
  BackCenter         → L += surround/√2·Cs,  R += surround/√2·Cs
  FrontLeftOfCenter  → L += 1.0·Lc          (or center· if folding to C)
  LowFrequency       → L += lfe·LFE,         R += lfe·LFE     (lfe defaults to 0)
  Top* / Bottom*     → fold onto the corresponding non-height channel with gain 1/√2
```

with `center` and `surround` defaulting to `1/√2 ≈ 0.7071` (−3 dB), which is BS.775's
power-preserving fold for uncorrelated sources.

**Phase 4 — normalisation** (§B.4.4).

`build_matrix` is exposed publicly (it is the `swr_build_matrix2` equivalent) because callers
legitimately want to inspect or modify the computed matrix before use.

### B.4.2 User-supplied matrices and raw mapping

```rust
impl Resampler {
    /// Replace the computed matrix. `matrix[out][in]`, row-major.
    pub fn set_matrix(&mut self, matrix: &[f64], stride: usize) -> Result<()>;
    /// Bypass matrixing entirely: output channel `i` is input channel `map[i]`.
    /// `None` produces silence. This is not a matrix — it is a plane permutation,
    /// and it skips the mixing stage completely.
    pub fn set_channel_mapping(&mut self, map: &[Option<usize>]) -> Result<()>;
}
```

`set_channel_mapping` deliberately bypasses `Rematrix` rather than building a permutation matrix,
because the caller asking for a raw map is asking for *no processing*, and going through the mixer
would apply normalisation and clipping they did not ask for.

### B.4.3 Matrix-encoded surround

`MatrixEncoding ∈ { None, Dolby, DolbyProLogicII, DolbyProLogicIIx, DolbyProLogicIIz, DolbyEx,
DolbyHeadphone }` from the `AVMatrixEncoding` surface. We implement the **encode** direction
(surround → matrixed stereo), which is what `matrix_encoding` selects on a downmix; the decode
direction is a filter's job, not a resampler's.

**Dolby Surround (`dolby`)** — the classic Lt/Rt fold, with the surround channels summed, phase-
inverted on one side, and band-limited:

```
  S  = surround · (Ls + Rs) / √2
  Lt = L + center·C − S
  Rt = R + center·C + S
```

**Dolby Pro Logic II (`dplii`)** — the published DPLII encode uses distinct coefficients per surround
channel so the decoder can recover separate left and right surround:

```
  Lt = L + (1/√2)·C − 0.8165·Ls − 0.5774·Rs
  Rt = R + (1/√2)·C + 0.5774·Ls + 0.8165·Rs
```

`0.8165 = √(2/3)` and `0.5774 = 1/√3`; these are the DPLII matrix constants as published in the
Dolby encoder specification and reproduced in the ATSC/DVD-Audio literature. They are spec-dictated
constants derived from the specification text, which is exactly what D7 permits.

`dplii_x` and `dplii_z` extend the same construction to back-surround and height channels
respectively; `dolby_ex` adds the matrixed back-centre. `dolby_headphone` is a binaural HRTF
convolution, which is genuinely a filter and not a matrix — we accept the option name and return a
clear "not implemented in the resampler; use the `headphone` filter" diagnostic rather than silently
falling back to plain Dolby, since silently producing a different result is the worse failure.

**Important:** matrix encoding is only meaningful when the output layout is exactly stereo and the
input has surround content. The builder validates this and warns otherwise.

### B.4.4 Normalisation and clipping

```
  if rematrix_volume >= 0:
      m *= rematrix_volume
  else:                                    # auto
      peak = max over output rows of (Σ |m[o][i]|)
      if peak > rematrix_maxval:
          m *= rematrix_maxval / peak
```

The row absolute sum is the worst-case output magnitude for a full-scale input, so scaling by
`maxval / peak` guarantees no clipping for any input. `rematrix_maxval` defaults to `1.0` for
integer output formats (where clipping is destructive) and to `+∞` for float output formats (where
it is not, and where reducing level is the more destructive choice). That default split is a
deliberate divergence from a single global default, and it is the behaviour callers actually want.

### B.4.5 Application and SIMD shape

Dispatch by `MatrixShape`:

| Shape | Kernel |
|---|---|
| `Permutation` | No arithmetic. Planar: swap the plane pointers, zero copies. Interleaved: a shuffle. |
| `Scaled` | One lanewise multiply per output channel. |
| `Sparse2` | Two multiply-adds per output channel — a `mul_add` chain over `Simd<f32, N>`. Covers 5.1→stereo, 7.1→5.1, and most real downmixes. |
| `Dense` | Full `rows × cols` multiply-accumulate, output-stationary: accumulate one output plane across all input planes so the accumulator stays in registers. |

All four are the "small fixed matrix multiply-add across channel counts" shape research §08 §6 item
11 describes as sufficient for portable SIMD. The integer path uses 15-bit fixed-point coefficients
with `i32` accumulation for `s16` (worst case `C · 2^15 · 2^15`, so `C ≤ 2` is safe in `i32` and
anything wider accumulates in `i64`); the float path is direct.

## B.5 Rate conversion

### B.5.1 The polyphase structure

Let `in_rate` and `out_rate` be the sample rates, `g = gcd(in_rate, out_rate)`, and
`p = out_rate/g`, `q = in_rate/g` the reduced ratio.

A polyphase resampler computes output sample `k` as a windowed-sinc-weighted sum of input samples
around position `k · q/p`:

```
  pos(k)     = k · in_rate / out_rate
  index(k)   = floor(pos(k))
  frac(k)    = pos(k) − index(k)                     ∈ [0, 1)
  phase(k)   = round(frac(k) · P)                    ∈ [0, P)   where P = 1 << phase_shift
  y[k]       = Σ_{j=0}^{T-1}  h[phase(k)][j] · x[index(k) − T/2 + 1 + j]
```

`P` phases × `T` taps is the coefficient bank: `P·T` coefficients, computed once at init.
`phase_shift` defaults to 10 (1024 phases); `filter_size` defaults to 32 taps. That is 32768
coefficients — 128 KB in `f32`, which fits comfortably in L2 and is walked linearly per output
sample.

**Phase quantisation error.** With `P` phases the timing error is at most `1/(2P)` input samples,
which for `P = 1024` is −66 dB of sidelobe-equivalent error — inaudible for most content but visible
in a spectrum analyser. Two mitigations, both from the option surface:

- **`linear_interp`**: interpolate between phase `⌊frac·P⌋` and `⌊frac·P⌋+1` by the residual
  fraction. Doubles the MAC count but reduces the error to `O(1/P²)`. The right choice when quality
  matters more than speed.
- **`exact_rational`**: when `p ≤ MAX_EXACT_PHASES` (we use 4096), build exactly `p` phases instead
  of `2^phase_shift`. Then `phase(k) = (k · q) mod p` is exact integer arithmetic and there is **no
  phase error at all**, and no float accumulation drift over long streams. For the overwhelmingly
  common rates this triggers: 44100→48000 has `p = 160, q = 147`; 48000→44100 has `p = 147`. Both
  are tiny. This is the default and it is the right default.

`exact_rational` also changes the position advance from float accumulation to integer:

```rust
struct Advance { index: i64, num: u32, den: u32 }   // position = index + num/den
impl Advance {
    fn step(&mut self, q: u32, p: u32) {            // advance by q/p input samples
        self.num += q % p;  self.index += (q / p) as i64;
        if self.num >= p { self.num -= p; self.index += 1; }
    }
}
```

which cannot drift over any stream length. Float position accumulation loses a sample after roughly
`2^24` outputs at `f32` — about five minutes at 48 kHz — which is a real, reported class of bug in
naive resamplers, and integer advance eliminates it categorically.

### B.5.2 Filter design

The coefficient at phase `φ ∈ [0, P)`, tap `j ∈ [0, T)`:

```
  centre = T/2 − 1
  t      = (j − centre − φ/P) · min(1, out_rate/in_rate)
  h[φ][j] = window(j, φ) · fc · sinc(fc · t)
```

with `sinc(x) = sin(πx)/(πx)`, `sinc(0) = 1`, and `fc` the normalised cutoff.

The `min(1, out_rate/in_rate)` factor is the same anti-aliasing stretch as §A.7.2: when downsampling,
the filter must cut off at the *output* Nyquist, not the input's, or it aliases. Getting this wrong
produces a resampler that sounds fine on upsampling and terrible on downsampling, which is a common
failure mode.

**Cutoff.** `cutoff` defaults to `0.97` for Blackman–Nuttall and to a beta-dependent value for
Kaiser. The Kaiser default follows from the window's own transition width: for a Kaiser window of
length `T` with parameter `β`, the normalised transition width is approximately
`Δf ≈ (A − 8) / (2.285 · π · T)` where `A ≈ 8.7 + 0.1102·(β − 8.7)·... ` — in practice we invert
Kaiser's standard design formulas to place the stopband edge at Nyquist:

```
  A  = 2.285 · (T − 1) · Δω + 8            (Kaiser's stopband attenuation estimate)
  β  = 0.1102·(A − 8.7)                       for A > 50
     = 0.5842·(A − 21)^0.4 + 0.07886·(A − 21) for 21 ≤ A ≤ 50
     = 0                                      for A < 21
```

Given the user's `β` and `T`, solving the first equation for `Δω` gives the transition width, and
`fc = 1 − Δω` places the stopband edge exactly at Nyquist. That derivation is Kaiser's, published in
every DSP textbook, and it means our default cutoff is *derived* rather than tabulated.

**Windows.**

*Blackman–Nuttall* (Nuttall 1981, the 4-term minimum-sidelobe window, −98 dB peak sidelobe):

```
  w(n) = a0 − a1·cos(2πn/(N−1)) + a2·cos(4πn/(N−1)) − a3·cos(6πn/(N−1))
  a0 = 0.3635819,  a1 = 0.4891775,  a2 = 0.1365995,  a3 = 0.0106411
```

*Kaiser* (Kaiser 1974), the near-optimal prolate-spheroidal approximation:

```
  w(n) = I₀( β · √(1 − (2n/(N−1) − 1)²) ) / I₀(β)
```

with `I₀` the zeroth-order modified Bessel function of the first kind, evaluated by its series
`I₀(x) = Σ_{k≥0} ((x/2)^k / k!)²` summed until the term falls below `1e-17` relative — which happens
in under 30 terms for `β ≤ 16`, and this is init-time code so the cost is irrelevant. `kaiser_beta`
ranges 2–16 with default 9; `β = 9` gives roughly −70 dB stopband, which is transparent for 16-bit
audio.

*Cubic* is not a windowed sinc. It is a 4-tap piecewise-cubic interpolator (Catmull–Rom, the same
`(B,C) = (0, 0.5)` kernel as §A.7.1) evaluated per phase. It has poor stopband rejection and is
included because it is cheap and because the option exists. `filter_size` is forced to 4 when
`filter_type = cubic`, with a warning if the user asked for something else.

**Normalisation.** Each phase's taps are normalised to sum to exactly 1 (in fixed point, to
`1 << shift`, with the residual added to the largest tap — the same rule as §A.7.2 step 4). Per-phase
normalisation, not global: without it, different phases have slightly different DC gains and the
output acquires a periodic amplitude modulation at the phase-cycle rate, which is audible as a faint
buzz. This is a classic resampler bug and per-phase normalisation is the categorical fix.

### B.5.3 Coefficient layout and SIMD

Unlike §A.7.5, the audio case needs **no blocking trick**: for a single output sample, the `T` taps
of one phase are contiguous, and the `T` input samples are contiguous. Both are unit-stride vector
loads. The inner loop is a plain dot product:

```rust
fn convolve<const N: usize>(x: &[f32], h: &[f32]) -> f32
where LaneCount<N>: SupportedLaneCount
{
    let mut acc = Simd::<f32, N>::splat(0.0);
    let (xc, xt) = x.as_chunks::<N>();
    let (hc, ht) = h.as_chunks::<N>();
    for (a, b) in xc.iter().zip(hc) {
        acc += Simd::from_array(*a) * Simd::from_array(*b);   // no mul_add: see Class B
    }
    let mut s = acc.reduce_sum();
    for (a, b) in xt.iter().zip(ht) { s += a * b; }
    s
}
```

This is the single hottest loop in the crate. Three refinements over the naive form:

1. **Multiple accumulators.** One `Simd` accumulator serialises on the FMA latency chain (4–5
   cycles). Four independent accumulators summed at the end saturate the FMA units. With `T = 32`
   and `N = 8`, that is exactly four vectors — the loop unrolls completely and there is no loop at
   all, just 4 multiplies and 4 adds plus a reduction.
2. **Reduction order is fixed.** `reduce_sum` on a `Simd` is a tree reduction whose order depends on
   `N`. For Class B (bitexact) we replace it with an explicit fixed-order scalar sum of the lanes.
   For Class C we let it be, and accept the last-bit variance.
3. **Channel batching.** For `C` channels at the same rate, the same coefficients apply to all of
   them. Processing channels in the outer loop and reusing the loaded coefficient vectors amortises
   the coefficient loads across `C` convolutions — a real win at 6 and 8 channels.

The integer path (`s16` internal) accumulates in `i64`: worst case `T · 2^15 · 2^15 = 32 · 2^30 =
2^35`, which overflows `i32`. Using `i32` with a reduced coefficient shift is possible but costs
stopband rejection, so `i64` accumulation it is; on 64-bit targets `Simd<i64, N>` widening
multiply-accumulate is well supported and the throughput cost is acceptable given that the `s16`
path exists for compatibility rather than for speed.

### B.5.4 Delay, latency and priming

The filter's group delay is `centre = T/2 − 1` input samples. Three consequences the API must
handle:

- `delay()` reports the current internal delay in output samples, converted by the rate ratio. This
  is the `swr_get_delay` equivalent and callers need it for A/V sync.
- At stream start the filter needs `centre` samples of history that do not exist. We prepend
  `centre` zeros. The alternative — replicating the first sample — avoids the initial fade-in but
  introduces a DC step, which is worse. Zero-priming means the first `centre` output samples are
  attenuated, and callers who care use `first_pts` to account for it.
- At stream end, draining requires `centre` samples of lookahead that do not exist. We append
  `centre` zeros on flush.

`get_out_samples(in_samples)` returns the exact upper bound on output samples for a given input
count, so callers can size buffers without guessing:

```
  out = ((in + delay_in_samples) · out_rate + in_rate − 1) / in_rate
```

## B.6 Dithering

Applied when quantising the internal format down to a narrower integer output — the `s16` output
case, essentially always.

```rust
pub enum DitherMethod {
    None,
    Rectangular,
    Triangular,
    TriangularHighpass,
    NoiseShaping(NsKind),
}

pub enum NsKind {
    Lipshitz, FWeighted, ModifiedEWeighted, ImprovedEWeighted,
    Shibata, LowShibata, HighShibata,
}
```

**Rectangular**: add one uniform random variable in `[−q/2, q/2)` before truncation, `q` the
quantisation step. Decorrelates the quantisation error from the signal but leaves the noise
amplitude modulated by the signal.

**Triangular (TPDF)**: add the *sum of two* independent uniforms, giving a triangular density over
`[−q, q)`. This is the standard choice and the correct default: TPDF is the lowest-order dither that
makes both the mean and the variance of the quantisation error independent of the signal (Lipshitz,
Wannamaker & Vanderkooy, JAES 1992). Costs 4.8 dB of noise floor versus rectangular, and is worth it.

**Triangular highpass**: TPDF with the noise spectrally shaped away from the ear's sensitive midband
by a first-order highpass on the dither sequence — `d[n] = u[n] − u[n−1]` with `u` uniform, which is
TPDF-distributed and highpass by construction. Same variance, less audible.

**Noise shaping** is error feedback: the quantisation error of previous samples is filtered and
subtracted from the current input, pushing the noise into frequency bands where the ear is less
sensitive.

```
  e[n]  = quantise(x[n] + Σ_{k=1}^{K} c[k]·e[n−k]) − (x[n] + Σ c[k]·e[n−k])
  y[n]  = quantise(x[n] + Σ c[k]·e[n−k])
```

The `c[k]` define the shaping curve, and here we hit a **clean-room and licence problem worth stating
plainly**: the seven named shaping curves in the option surface come from two sources. The
Lipshitz / F-weighted / E-weighted family is published in Lipshitz, Vanderkooy & Wannamaker,
"Minimally Audible Noise Shaping", JAES 1991 — a paper we may work from freely. The Shibata curves
originate in SSRC, whose licensing is not on D3's allowlist, and their coefficient sets are the
author's own expression, not spec-dictated constants.

**Our decision:** we implement the Lipshitz-family curves from the published paper's coefficients,
and we **generate the Shibata-named curves ourselves** by weighted least-squares fitting of a
minimum-phase error-feedback filter to an inverse absolute-threshold-of-hearing weighting curve
(ISO 226 equal-loudness contours), at the three aggressiveness levels the names imply. The option
names are preserved for CLI compatibility; the curves are ours and will not be bit-identical.

This is a **declared D6 divergence** and is graded Equivalent, not Exact, with the tolerance being a
psychoacoustic one: the shaped noise floor must be at or below the reference's under the same
weighting curve, verified by a spectrum test. That is a defensible criterion — it says our dither is
at least as good — and it is the only honest one available.

**Determinism.** The dither PRNG is seeded from a fixed constant plus the sample position, not from
`SystemTime` or a global generator. So the same input produces the same output on every run and every
machine, and buffer chunking does not change the result. This makes dither Class A despite being
"random", and it is what allows the differential harness to compare dithered output at all. A
`dither_seed` option (vaco extension) lets a caller vary it deliberately.

`dither_scale` multiplies the dither amplitude; `output_sample_bits` overrides the assumed target
depth, which matters when the output format is `s32` but the real destination is a 24-bit device.

## B.7 Timestamp compensation

This is the stage that makes a resampler usable in a live pipeline, and it is where the reference's
option surface is least self-explanatory.

The problem: a caller feeds audio and also knows what the timestamps say. Over time the two drift —
because the source clock and the sink clock differ, because packets were dropped, or because a
container's timestamps are simply inconsistent. The resampler is the natural place to absorb that
drift, since it is already resampling.

```rust
pub struct Compensation {
    /// Below this drift (seconds), do nothing. Default 0.1.
    pub min_comp: f32,
    /// Above this drift (seconds), pad or trim immediately. Default 0.1.
    pub min_hard_comp: f32,
    /// Seconds over which a soft correction is spread. Default 1.0.
    pub comp_duration: f32,
    /// Maximum fractional rate change for soft compensation. Default 0.0 (disabled).
    pub max_soft_comp: f32,
    /// Single-parameter shorthand. Default 0.0 (disabled).
    pub async_samples: f32,
    /// Assumed first output PTS, in output samples. Default UNSET.
    pub first_pts: i64,
}
```

**Hard compensation.** When `|drift| > min_hard_comp`, the resampler inserts silence (drift
positive: output is behind) or drops samples (drift negative). It is immediate, it is audible as a
click, and it is the correct response to a large drift because the alternative is losing sync.

**Soft compensation.** When `min_comp < |drift| ≤ min_hard_comp` and `max_soft_comp > 0`, the
resampler *changes its ratio slightly* for `comp_duration` seconds, absorbing the drift by
stretching or squeezing. The rate change is clamped to `max_soft_comp` (a fraction, e.g. `0.01` for
1%). Inaudible if `max_soft_comp` is small, and it is what a well-behaved live pipeline uses.

Soft compensation interacts with `exact_rational` (§B.5.1): a modified ratio is generally not a nice
rational, so during a soft correction we fall back to the `2^phase_shift` phase bank and the integer
advance is replaced by a fixed-point one with a compensating increment. The plan therefore always
builds *both* banks when `max_soft_comp > 0` — the memory cost is one extra bank and it avoids a
mid-stream reallocation, which in a live pipeline is exactly when you cannot afford one.

**`async`** is the single-parameter shorthand the option surface documents: `async = N` sets
`min_hard_comp` to a small value and enables soft compensation limited to `N` samples per second of
stretch. `async = 1` is the widely-used idiom meaning "fix the stream start by padding or trimming,
then leave it alone" — with `N = 1` the soft limit is so small that only the initial hard
compensation does anything.

**Manual control**, for callers driving compensation themselves:

```rust
impl Resampler {
    /// Apply `sample_delta` samples of correction spread over `compensation_distance` output samples.
    pub fn set_compensation(&mut self, sample_delta: i32, compensation_distance: i32) -> Result<()>;
    /// Next output PTS given the next input PTS, accounting for internal delay.
    pub fn next_pts(&self, input_pts: i64) -> i64;
    pub fn drop_output(&mut self, count: usize) -> Result<()>;
    pub fn inject_silence(&mut self, count: usize) -> Result<()>;
}
```

## B.8 Public API

```rust
pub struct Resampler { /* … */ }

impl Resampler {
    pub fn builder() -> ResamplerBuilder;

    pub fn new(input: &AudioSpec, output: &AudioSpec, opts: &ResampleOptions) -> Result<Self>;

    /// Core conversion. Returns samples written per channel. `input: None` drains.
    pub fn convert(
        &mut self,
        input: Option<AudioBuf<'_, u8>>,
        output: AudioBufMut<'_, u8>,
    ) -> Result<usize>;

    /// Frame-level convenience; reconfigures from the frames' own specs if needed.
    pub fn convert_frame(&mut self, input: Option<&Frame>, output: &mut Frame) -> Result<()>;

    /// Configure an output frame's spec to match this resampler.
    pub fn config_frame(&self, frame: &mut Frame) -> Result<()>;

    /// Internal delay, in units of `rate` (pass the output rate for output samples).
    pub fn delay(&self, rate: i64) -> i64;
    /// Upper bound on output samples for `in_samples` of input.
    pub fn out_samples(&self, in_samples: usize) -> usize;

    pub fn is_initialized(&self) -> bool;
    pub fn reset(&mut self);
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioSpec {
    pub sample_rate: i32,
    pub format: SampleFormat,
    pub layout: ChannelLayout,
}
```

`convert` takes `AudioBuf<'_, u8>` — byte buffers with the format carried in the spec — because the
element type is a runtime value. Typed accessors (`convert_f32`, `convert_i16`) wrap it for the
common cases and check the format once, which keeps the ergonomic path type-safe without forcing
generic explosion on the core.

## B.9 Threading

Rate conversion is per-channel independent, so channels parallelise trivially — but at 2 channels
and typical buffer sizes the work per call is a few microseconds and thread dispatch dominates.
Our position: **`vaco-resample` is single-threaded internally**. Parallelism comes from the pipeline
(architecture §6 axis 1), where an audio filter chain runs as its own task. We expose no `threads`
option because the reference does not either, and adding one would invite a configuration that is
slower than the default.

The exception worth revisiting with measurements: 8+ channel content at high sample rates with large
buffers, where per-channel `rayon` parallelism might pay. Deferred until a benchmark says so.

## B.10 Option surface

Complete mapping of the inventory's `SwrContext` option table. Every name and alias preserved.

```rust
#[derive(Options, Clone, Debug)]
#[opts(name = "swr")]
pub struct ResampleOptions {
    // ── endpoints ───────────────────────────────────────────────────────────
    #[opt(name = "isr", alias = "in_sample_rate",  default = 0, min = 0)] pub in_sample_rate: i32,
    #[opt(name = "osr", alias = "out_sample_rate", default = 0, min = 0)] pub out_sample_rate: i32,
    #[opt(name = "isf", alias = "in_sample_fmt")]        pub in_sample_fmt: SampleFormat,
    #[opt(name = "osf", alias = "out_sample_fmt")]       pub out_sample_fmt: SampleFormat,
    #[opt(name = "tsf", alias = "internal_sample_fmt", default = "none",
          help = "internal working sample format")]      pub internal_sample_fmt: SampleFormat,
    #[opt(name = "ichl", alias = "in_chlayout")]         pub in_chlayout: ChannelLayout,
    #[opt(name = "ochl", alias = "out_chlayout")]        pub out_chlayout: ChannelLayout,
    #[opt(name = "uchl", alias = "used_chlayout")]       pub used_chlayout: ChannelLayout,

    // ── mixing ──────────────────────────────────────────────────────────────
    #[opt(name = "clev", alias = "center_mix_level",   default = 0.70711, min = -32.0, max = 32.0)]
    pub center_mix_level: f32,
    #[opt(name = "slev", alias = "surround_mix_level", default = 0.70711, min = -32.0, max = 32.0)]
    pub surround_mix_level: f32,
    #[opt(name = "lfe_mix_level", default = 0.0, min = -32.0, max = 32.0)]
    pub lfe_mix_level: f32,
    #[opt(name = "rmvol", alias = "rematrix_volume", default = 1.0, min = -1.0, max = 1000.0)]
    pub rematrix_volume: f32,
    #[opt(name = "rematrix_maxval", default = 0.0, min = 0.0, max = 1000.0,
          help = "clipping ceiling for rematrixed samples (0 = format-dependent default)")]
    pub rematrix_maxval: f32,
    #[opt(name = "matrix_encoding", unit = "matrix_encoding", default = "none")]
    pub matrix_encoding: MatrixEncoding,

    // ── engine ──────────────────────────────────────────────────────────────
    #[opt(name = "flags", alias = "swr_flags", unit = "flags", default = 0)]
    pub flags: SwrFlags,                                   // named const: `res` = force resampling
    #[opt(name = "resampler", unit = "resampler", default = "swr")]
    pub resampler: Engine,                                 // named consts: swr, soxr
    #[opt(name = "filter_size", default = 32, min = 0, max = 65536)]
    pub filter_size: i32,
    #[opt(name = "phase_shift", default = 10, min = 0, max = 24)]
    pub phase_shift: i32,
    #[opt(name = "linear_interp", default = false)]        pub linear_interp: bool,
    #[opt(name = "exact_rational", default = true)]        pub exact_rational: bool,
    #[opt(name = "cutoff", alias = "resample_cutoff", default = 0.0, min = 0.0, max = 1.0,
          help = "filter cutoff ratio (0 = derive from filter type and parameters)")]
    pub cutoff: f64,
    #[opt(name = "filter_type", unit = "filter_type", default = "kaiser")]
    pub filter_type: FilterType,                           // cubic, blackman_nuttall, kaiser
    #[opt(name = "kaiser_beta", default = 9.0, min = 2.0, max = 16.0)]
    pub kaiser_beta: f64,
    #[opt(name = "precision", default = 20.0, min = 15.0, max = 33.0,
          help = "soxr resampling precision in bits (accepted; see §B.13.3)")]
    pub precision: f64,
    #[opt(name = "cheby", default = false,
          help = "soxr Chebyshev passband (accepted; see §B.13.3)")]
    pub cheby: bool,

    // ── dither ──────────────────────────────────────────────────────────────
    #[opt(name = "dither_method", unit = "dither_method", default = "none")]
    pub dither_method: DitherMethod,
    #[opt(name = "dither_scale", default = 1.0, min = 0.0, max = f64::MAX)]
    pub dither_scale: f64,
    #[opt(name = "output_sample_bits", default = 0, min = 0, max = 64)]
    pub output_sample_bits: i32,

    // ── timestamp compensation ──────────────────────────────────────────────
    #[opt(name = "min_comp",      default = f32::MAX, min = 0.0)] pub min_comp: f32,
    #[opt(name = "min_hard_comp", default = 0.1,      min = 0.0)] pub min_hard_comp: f32,
    #[opt(name = "comp_duration", default = 1.0,      min = 0.0)] pub comp_duration: f32,
    #[opt(name = "max_soft_comp", default = 0.0)]                 pub max_soft_comp: f32,
    #[opt(name = "async", default = 0.0, help = "simplified timestamp matching")]
    pub async_samples: f32,
    #[opt(name = "first_pts", default = i64::MIN, help = "assumed first PTS, in samples")]
    pub first_pts: i64,

    // ── vaco extensions ─────────────────────────────────────────────────────
    #[opt(name = "dither_seed", default = 0, help = "dither PRNG seed (vaco extension)")]
    pub dither_seed: u64,
    #[opt(name = "bitexact", default = false, help = "force reproducible float evaluation (vaco extension)")]
    pub bitexact: bool,
}
```

Enum surfaces, named as the inventory lists them:

```rust
#[derive(Options, Copy, Clone, Debug)] #[opts(unit = "resampler")]
pub enum Engine { Swr, Soxr }

#[derive(Options, Copy, Clone, Debug)] #[opts(unit = "filter_type")]
pub enum FilterType { Cubic, BlackmanNuttall, Kaiser }

#[derive(Options, Copy, Clone, Debug)] #[opts(unit = "dither_method")]
pub enum DitherMethod {
    None, Rectangular, Triangular, TriangularHighpass,
    NsLipshitz, NsFWeighted, NsModifiedEWeighted, NsImprovedEWeighted,
    NsShibata, NsLowShibata, NsHighShibata,
}

#[derive(Options, Copy, Clone, Debug)] #[opts(unit = "matrix_encoding")]
pub enum MatrixEncoding { None, Dolby, Dplii, DpliiX, DpliiZ, DolbyEx, DolbyHeadphone }
```

**Divergences from the reference defaults, each deliberate:**

- `exact_rational` defaults to `true`. It is strictly better (no phase error, no drift) and costs
  nothing for the rate pairs anyone actually uses (§B.5.1). If the probe shows the reference defaults
  it off and this changes output, we match the reference's default and document ours as an opt-in —
  fidelity outranks our preference.
- `rematrix_maxval` default `0.0` means "format-dependent" (§B.4.4) rather than a single constant.
- `cutoff` default `0.0` means "derive from the filter design" (§B.5.2) rather than a magic constant,
  so changing `kaiser_beta` automatically moves the cutoff to stay optimal.

## B.11 Reproducibility

| Path | Class | Notes |
|---|---|---|
| Integer sample-format conversion (all 30 element converters, all layouts) | **A** | Shifts and saturating adds only. |
| `Permutation` / `Scaled` / integer `Sparse2` rematrix | **A** | Fixed-point, defined rounding. |
| Integer rate conversion (`s16` internal, `i64` accumulate) | **A** | Fixed coefficient bank, fixed accumulate order. |
| Float rate conversion with `bitexact` | **B** | Explicit lane-order reduction, no `mul_add`, single accumulator. |
| Float rate conversion, default | **C** | Multiple accumulators, FMA permitted. Bound: ≤ 2 ULP per output sample relative to an `f64` reference convolution. |
| Float → integer output quantisation | **A** | Rounding mode pinned (§B.3.2); clamping defined. |
| Dither, all methods | **A** | Position-seeded PRNG (§B.6); no wall-clock, no global state. |
| Soft/hard compensation | **A** | Integer sample counts and a deterministic ratio schedule. |

Buffer-chunking independence is asserted for every path: feeding the same stream in chunks of 1, 7,
1024 and 65536 samples must produce byte-identical output. This catches state-management bugs in the
filter history and in the compensation accumulator, which are the two places this crate is most
likely to be wrong, and it is the single highest-value test in the crate.

## B.12 Test strategy

**Unit.** Each of the 30 element converters against hand-computed values at the boundary points
(`0`, `±1`, `MIN`, `MAX`, `MIN+1`, `MAX−1`, and the values that straddle each rounding boundary).
Each window function against its published closed form at `n = 0`, `n = N/2`, `n = N−1`.

**Property (proptest).**
- **Round-trip**: `s16 → f32 → s16` is the identity for every `i16`. `f32 → s16 → f32` is within
  `q/2`. These two catch essentially every scaling bug.
- **Chunk invariance**: as §B.11 — the highest-value test in the crate.
- **DC gain**: a constant input at any rate ratio produces the same constant (within the dither
  floor) on output. This is what per-phase normalisation (§B.5.2) exists to guarantee, and the
  property test is how we know it holds for all phases rather than the ones we happened to look at.
- **Rate-ratio identity**: `resample(x, r → r)` is the identity when `flags` does not force
  resampling.
- **Matrix identity**: rematrixing a layout to itself is the identity, for all 40 named layouts.
- **Matrix energy**: for any layout pair and default mix levels, no output row's absolute sum
  exceeds `rematrix_maxval` after normalisation.
- **Delay consistency**: `delay()` before and after a conversion agrees with the actual sample
  count produced, for random input sizes.
- **Compensation convergence**: with a synthetic drifting clock, the drift returns below `min_comp`
  within `comp_duration` and stays there.

**Signal-quality tests** — the ones that distinguish a resampler that works from one that is
correct:
- **THD+N sweep**: a full-scale sine at 40 frequencies from 20 Hz to just below Nyquist, resampled
  between each pair in `{8k, 11.025k, 16k, 22.05k, 32k, 44.1k, 48k, 88.2k, 96k, 192k}`, measuring
  total harmonic distortion plus noise. Assert ≤ −90 dB for Kaiser at default settings.
- **Stopband rejection**: an impulse resampled, FFT'd, and checked against the designed stopband
  attenuation for each `(filter_type, filter_size, kaiser_beta)` combination. Catches window and
  cutoff bugs that THD does not.
- **Passband ripple**: ≤ 0.01 dB across the passband for Kaiser at `β = 9`.
- **Aliasing on downsample**: 96k → 44.1k of a sweep that crosses the output Nyquist; assert no
  energy folds back above −90 dB. This is the test that catches a missing `min(1, ratio)` factor.
- **Dither noise floor**: for each dither method, the noise spectrum of a dithered −90 dBFS sine at
  16-bit, checked against the expected shaped curve.

**Differential against the reference binary (D6).** `ffmpeg -f s16le -ar R1 -ac C1 -i in.pcm
-af "aresample=R2:..." -f s16le -` versus ours. Matrix: all rate pairs above; all format pairs; all
layout pairs from a curated set of 30 plus a random sample; all three filter types at default and
non-default `filter_size`/`phase_shift`/`cutoff`/`kaiser_beta`; all dither methods; compensation
scenarios driven by a synthetic drifting source.

**Fuzzing.** Option-string parsing; `Resampler::new` over arbitrary `(rate, format, layout)` triples
including absurd rates (1 Hz, 2^31−1 Hz) and 64-channel custom layouts; `convert` driven with
adversarial chunk sequences including zero-length and single-sample calls interleaved with drains.

## B.13 Build-or-buy assessment (D10)

### B.13.1 `rubato`

| Gate | Finding |
|---|---|
| 1. Pure Rust, zero FFI | **Pass.** |
| 2. Licence | **Pass.** MIT. |
| 3. Trusted/maintained | **Pass.** Actively maintained, widely used in the Rust audio ecosystem, no RUSTSEC advisory. Modest `unsafe` in its own code; its optional FFT resampler pulls `realfft` → `rustfft`, which is heavily unsafe and would deepen the tree considerably. |
| 4. Model fit | **Fail on coverage; partial fit on the core.** |

`rubato` implements the mathematical core well: `SincFixedIn`/`SincFixedOut` are a competent
polyphase windowed-sinc resampler with selectable interpolation, and its FFT resampler is a good
implementation of a different technique. If the requirement were "resample audio", adopting it would
be right.

The requirement is the option surface. Mapping our §B.10 table onto `rubato`:

| Requirement | `rubato` |
|---|---|
| Polyphase windowed sinc | ✅ covered |
| `filter_size`, `cutoff` | ✅ covered (different parameterisation, mappable) |
| `phase_shift` (explicit phase count), `exact_rational` | ❌ not exposed; its oversampling factor is a different control and there is no exact-rational integer-advance mode |
| `filter_type = {cubic, blackman_nuttall, kaiser}` | ⚠️ partial — window selection differs; Blackman–Nuttall not offered as such |
| `linear_interp` as distinct from higher-order | ⚠️ its interpolation options do not map one-to-one |
| Sample-format conversion (30 converters, planar/interleaved) | ❌ out of scope — it is `f32`/`f64` only |
| Channel rematrixing, mix levels, matrix encoding, user matrices, raw mapping | ❌ entirely out of scope |
| Ten dither methods incl. noise-shaping bank | ❌ entirely out of scope |
| Timestamp compensation (soft/hard/async/first_pts) | ❌ entirely out of scope |
| Integer (`s16`/`s32`) internal paths | ❌ float only |
| Bit-exact chunk invariance as a contract | ❌ not a stated guarantee |

Roughly **one of the three stages, partially**. Format conversion, rematrixing, dithering and
compensation — which together are the majority of the crate's code and nearly all of its option
surface — are untouched.

The partial-adoption question is therefore sharper here than in §A.14: could we use `rubato` for
`RateConvert` and write the rest? The staged architecture (§B.1) makes that mechanically easy —
`RateConvert` is one `Stage` implementation behind a trait, and D11's backend feature pattern would
wire it cleanly. We still decline, for three reasons:

1. **The option surface is the requirement, and it lands almost entirely on this stage.**
   `filter_size`, `phase_shift`, `linear_interp`, `exact_rational`, `cutoff`, `filter_type`,
   `kaiser_beta` are seven of the resampler's options and every one of them is a `RateConvert`
   control. We would be wrapping a crate and then reaching around it for most of what callers set.
2. **Soft compensation requires mid-stream ratio changes** (§B.7) with continuity of filter state.
   That is an unusual capability, it is not in `rubato`'s model, and retrofitting it from outside is
   not possible — it is inherently internal.
3. **Integer paths and bit-exactness.** The `s16` internal path is Class A in our design and does not
   exist in `rubato`. We would have two rate converters regardless.

**Verdict: implement our own.** Not because `rubato` is deficient — it is good at what it does — but
because what it does is the 15% of this crate that was never the hard part.

### B.13.2 Other candidates

- **`dasp` / `dasp_interpolate`** (MIT OR Apache-2.0, pure Rust): a sample-type and interpolation
  trait framework rather than a resampler. Its `Converter` is generic over an interpolator, and its
  `Sample` conversion traits overlap §B.3. Gate-clean, but the abstraction is generic-heavy and its
  conversion rounding is its own; adopting it would trade our pinned numeric contract (§B.3.2) for
  its. **Do not adopt**; it is worth reading for its sample-trait design.
- **`fon`, `audrey`, `samplerate`-style crates**: either FFI (Gate 1 failure — `samplerate` binds
  libsamplerate) or too small to clear Gate 3.
- **`libsoxr`**: LGPL-2.1 and C. Fails Gate 1 *and* Gate 2. See §B.13.3.

### B.13.3 The `soxr` engine option — a specific problem

The option surface includes `resampler = soxr`, plus `precision` and `cheby` which only mean anything
to that engine. libsoxr is LGPL-2.1 (Gate 2 failure) and C (Gate 1 failure), so it is doubly excluded
and no wrapper resolves it.

Three possible behaviours, and the choice matters for CLI compatibility:

1. **Error out.** Honest but breaks scripts that pass `-resampler soxr` without caring.
2. **Silently alias to our native engine.** Convenient and dishonest — the user asked for a specific
   engine and got another, with different output.
3. **Accept, warn, and alias.** Our choice: `resampler = soxr` logs at `warning` level
   ("libsoxr is not available in this build; using the native resampler") and proceeds with the
   native engine. `precision` and `cheby` are accepted and ignored, with a `verbose`-level note.

This is the general pattern for every option we cannot honour: accept the name, do the sensible
thing, and say so at a log level the user will see. Silent divergence is the one option we never
take. The behaviour is recorded in `docs/resample/compatibility.md` and is a declared D6 divergence
for any test that passes `-resampler soxr`.

### B.13.4 Dependencies `vaco-resample` does take

`thiserror`, plus workspace-internal `vaco-core`, `vaco-simd`, `vaco-opts`, `vaco-sampfmt`,
`vaco-chlayout`, `vaco-frame`. Dev-dependencies: `proptest`, `criterion`, and `rustfft` **as a
dev-dependency only** for the FFT used by the THD/stopband/aliasing analysis in §B.12 — a test
oracle, never shipped, and permitted because D11's boundary rule concerns crates reachable from
shipped code. No `rayon` (§B.9). Nothing media-specific in the shipped graph.

## B.14 Fidelity assessment — what "identical to ffmpeg" means here

Resampling is a middle case between scaling and transforms: more of it is exactly defined than in
`vaco-scale`, but the parts that are not are harder to pin because they compound over a stream rather
than staying local to a pixel.

### B.14.1 Per-class verdict

| Class | Byte-identical achievable? | Confidence | What has to be matched |
|---|---|---|---|
| **1. Sample-format conversion, integer ↔ integer** | **Yes, certainly** | Very high | Shift direction and narrowing rounding. Two probes. |
| **2. Sample-format conversion, integer → float** | **Yes** | Very high | The scale divisor (`2^(n−1)` vs `2^(n−1)−1`). One probe. |
| **3. Sample-format conversion, float → integer** | **Yes** | High | The rounding mode (§B.3.2) and clamp placement. One probe each, and the answer is stable — it is a single line of behaviour, not an accumulation. |
| **4. Planar ↔ interleaved** | **Yes, certainly** | Very high | Pure permutation. |
| **5. Rematrix, permutation only** | **Yes, certainly** | Very high | Nothing numeric — it is a channel reorder. Only the *layout matching rules* need to agree (which input channel maps to which output), and those are enumerable and testable exhaustively over the 40 named layouts. |
| **6. Rematrix with gains, integer path** | **Yes, with effort** | Medium-high | The default mix-level constants, the upmix gain (§B.4.2), the coefficient quantisation shift, the accumulator width, the rounding, and the normalisation rule. Six pinned parameters, all individually probeable by rematrixing unit impulses on each input channel and reading off the resulting output gains — a direct measurement of the matrix itself. |
| **7. Rematrix, float path** | **Realistically yes; formally Equivalent** | Medium | Same six parameters, but the arithmetic is float. With ≤ 2 taps per output (the common case) there is no reassociation freedom, so identical coefficients give identical results. `Dense` matrices at 8+ channels have accumulation-order freedom and land at ≤ 1 ULP. |
| **8. Rate conversion, integer path (`s16` internal)** | **Yes, conditionally** | Medium-low | Everything above, plus the entire filter design: window formula, cutoff derivation, tap-position convention, per-phase normalisation, residual distribution, coefficient quantisation shift, and accumulator width. That is ~8 more parameters, and unlike the scaling case they are **not independently observable** — an impulse response measures their *product*, not each one. |
| **9. Rate conversion, float path** | **No, and we should not try** | High confidence in the negative | Class 8's parameters plus float accumulation order over 32 taps. |
| **10. Dither, all methods** | **No** | High confidence in the negative | Requires matching a specific PRNG and, for noise shaping, specific filter coefficients we have deliberately generated ourselves (§B.6). |
| **11. Timestamp compensation** | **Yes for the sample counts; the audio depends on class 8/9** | Medium | *How many* samples are inserted or dropped, and *when*, is integer logic and fully matchable. What those samples contain inherits the rate-conversion class. |

### B.14.2 The rate-conversion problem, stated precisely

Classes 8 and 9 are the crux, and they deserve more than a table row.

For scaling (§A.15.1 class 4) we noted that the coefficient generator's parameters are individually
observable, because scaling a delta image reads off the impulse response directly and the parameters
separate. For audio resampling they do not separate. An impulse response at a given phase is
`h[φ][·]`, which is `window × sinc × cutoff × normalisation` collapsed into one vector of numbers.
Recovering four factors from their product requires either assuming three of them or fitting, and
fitting to 14-bit-quantised coefficients is under-determined.

What we can do, and will:

- **Measure the impulse response at every phase** for a reference conversion and compare against
  ours coefficient-by-coefficient. If they match, class 8 is Exact and we are done — the parameters
  agreed even though we could not observe them individually.
- If they do not match, **diff the residual's structure**: a constant scale factor implicates
  normalisation; a smooth envelope difference implicates the window; a difference concentrated at the
  edges implicates the cutoff; a ±1 pattern implicates quantisation or residual distribution. This
  makes the search directed rather than blind, and each hypothesis is one rebuild to test.
- **Budget three person-weeks for this** and treat it as genuinely uncertain. It is the single
  largest schedule risk in the crate.

**If class 8 lands Divergent**, the fallback is well-defined and not embarrassing: grade it
Equivalent with a tolerance of **≥ 100 dB SNR against the reference output** (i.e. the difference
signal is 100 dB below the signal), plus the independent signal-quality assertions of §B.12
(THD+N ≤ −90 dB, stopband ≥ designed attenuation, no aliasing above −90 dB). That combination says
something stronger than byte-equality in one respect: it says our resampler is *good*, not merely
*the same*. A byte-identical resampler inherits the reference's design choices including any that are
suboptimal; a 100 dB-equivalent one that independently passes the quality suite is demonstrably
correct on its own terms.

We would still prefer Exact, because Exact makes the differential harness a sharp gate. But Equivalent
here is defensible in a way that Equivalent on, say, an integer format conversion would not be.

### B.14.3 Declared tolerances

| Comparison | Tolerance | Rationale |
|---|---|---|
| Classes 1–5 | Byte-identical | No free parameters after the probes. Anything less is a bug. |
| Class 6 (integer rematrix) | Byte-identical | Target. Fallback: max abs diff ≤ 1 LSB, which for a two-tap fixed-point mix is the entire possible disagreement. |
| Class 7 (float rematrix) | ≤ 1 ULP per sample | Accumulation order is the only freedom. |
| Class 8 (integer rate conversion) | Byte-identical target; fallback ≥ 100 dB SNR + the §B.12 quality suite | See §B.14.2. |
| Class 9 (float rate conversion) | ≥ 120 dB SNR | Float `f32` has ~144 dB of dynamic range; 120 dB leaves headroom for legitimate accumulation-order differences over 32 taps while catching any real error. |
| Class 10 (dither) | Noise floor at or below the reference's under ITU-R BS.1770 weighting, plus a shaped-spectrum test | We are asserting our dither is at least as good, which is the only claim available once the coefficients differ. |
| Class 11 (compensation) | Exact sample counts and exact insertion points | Integer logic; a mismatch here is a real bug and there is no tolerance to hide it in. |

### B.14.4 The honest summary

Format conversion and rematrixing should be byte-identical, and if they are not, we have a bug worth
fixing. Rate conversion is a genuine open question whose answer we will not know until we build the
probe, and we should plan for either outcome rather than assuming the good one. Dither will not be
identical and should not be — the noise-shaping curves are ours by necessity (§B.6), and matching
someone else's dither pattern has no user-visible value.

The one thing we will not do is widen a tolerance to make a failing test pass. Each number in the
table above has a derivation, and changing one is a reviewed diff with a reason attached.

## B.15 Benchmarks

Per research §08 §5 scenario 4, reporting samples/second per channel and cycles/sample:

| # | Scenario | Why |
|---:|---|---|
| 1 | `44.1k → 48k`, stereo, `s16 → s16`, kaiser default | The canonical rate conversion. Exercises `exact_rational`. |
| 2 | `48k → 44.1k`, stereo, `f32 → f32` | The downsample direction; exercises the anti-alias stretch. |
| 3 | `44.1k → 48k`, stereo, `f32p → s16`, dither=triangular | Full pipeline: convert, resample, dither, convert. |
| 4 | `48k → 48k`, `s16 → f32p`, no resampling | Isolates format conversion. Should be memory-bandwidth-bound. |
| 5 | 5.1 → stereo, 48k, `f32p → f32p`, no resampling | Isolates rematrixing (`Sparse2`). |
| 6 | 7.1 → 5.1, 48k, `f32p → f32p` | `Dense`-ish rematrix at higher channel count. |
| 7 | `44.1k → 48k`, 5.1, `f32p → s16`, ns_shibata | Worst realistic case: all four stages, 6 channels, noise shaping. |
| 8 | `96k → 48k`, stereo, `f32 → f32`, `filter_size=256` | Long filter; measures the convolution kernel in isolation. |
| 9 | Same as 1, with `linear_interp=1` | Isolates the cost of phase interpolation. |
| 10 | `Resampler::new` cold setup for scenarios 1–8 | Coefficient-bank generation is `P·T` transcendental evaluations; matters for short CLI runs. |
| 11 | Chunk-size sweep on scenario 1: 64, 256, 1024, 4096, 65536 samples | Per-call overhead versus throughput; informs the pipeline's buffer sizing. |

**Target:** parity (±10%) with the reference on scenarios 1–6 by the end of the implementation
window. Scenario 4 should *exceed* the reference, since format conversion is pure lanewise work and
the reference's own SIMD here is comparatively small (§08 §2b ranks swresample format conversion at
~3.0k lines of asm total across all architectures).

## B.16 Effort and work breakdown

| Workstream | Person-weeks | Depends on | Parallel? |
|---|---:|---|---|
| B1. Buffer model, `Stage` trait, `Resampler` composition, builder | 1.5 | `vaco-sampfmt`, `vaco-chlayout` | — |
| B2. Sample-format conversion: 30 element converters, interleave/deinterleave, scalar reference | 2 | B1 | yes |
| B3. Format-conversion SIMD, all tiers | 2 | B2, `vaco-simd` | after B2 |
| B4. Rematrix: matrix construction, layout mapping rules, normalisation | 2.5 | B1 | yes |
| B5. Rematrix: matrix encodings (Dolby, DPLII/x/z, Ex) | 1 | B4 | after B4 |
| B6. Rematrix kernels + SIMD, all shapes | 1.5 | B4, B3 | after B4 |
| B7. Filter design: windows, cutoff derivation, bank generation, normalisation | 2.5 | B1 | yes |
| B8. Polyphase engine: phase advance, exact-rational, linear interp, delay/priming/drain | 3 | B7 | after B7 |
| B9. Convolution kernels + SIMD, integer and float paths | 2.5 | B8, `vaco-simd` | after B8 |
| B10. Dither: all methods, PRNG, noise-shaping curve generation and validation | 2.5 | B1 | yes |
| B11. Timestamp compensation: soft, hard, async, first_pts, manual API | 2 | B8 | after B8 |
| B12. Option surface, parsing, `soxr` compatibility handling | 1 | `vaco-opts` | yes |
| B13. Test infrastructure: property tests, signal-quality suite, differential harness, fuzz | 3.5 | B1 | overlaps everything |
| B14. Fidelity probes: pin classes 1–7 and 11; investigate class 8 | 3 | B13 | after B13 starts |
| B15. Benchmarks + CI regression tracking | 1 | B9 | after B9 |
| B16. Documentation (`docs/resample/*.md`) | 1 | all | overlaps |
| **Total** | **32.5** | | |

**Critical path:** B1 → B7 → B8 → B9, which is 9.5 pw serial, plus B14's class-8 investigation which
cannot start until B9 produces coefficients to compare. With three engineers the calendar estimate is
**~12–14 weeks**; with two, **~18 weeks**.

**Parallelisation plan (3 engineers):**
- **E1 (rate):** B1 → B7 → B8 → B9 → B15. The spine and the schedule risk.
- **E2 (channels + formats):** B2 → B4 → B5 → B6 → B3.
- **E3 (quality + surface):** B10 → B12 → B13 → B14 → B16.

B14's class-8 investigation is the item most likely to overrun. It is scheduled with slack and has a
defined fallback (§B.14.2), so an overrun degrades the fidelity grade rather than blocking the crate.

## B.17 References an implementer works from

- Crochiere & Rabiner, *Multirate Digital Signal Processing*, Prentice-Hall 1983 — the standard
  reference for polyphase decomposition and the efficiency argument for it.
- Smith, J. O., *Digital Audio Resampling* (CCRMA, online) and *Physical Audio Signal Processing* —
  windowed-sinc interpolation, the bandlimited-interpolation formulation used in §B.5.1.
- Kaiser, J. F., "Nonrecursive digital filter design using the I₀-sinh window function",
  Proc. IEEE ISCAS 1974 — the window and the design formulas in §B.5.2.
- Nuttall, A. H., "Some Windows with Very Good Sidelobe Behavior", IEEE ASSP 1981 — the
  Blackman–Nuttall coefficients.
- Oppenheim & Schafer, *Discrete-Time Signal Processing*, 3rd ed. — filter design, group delay,
  the transition-width relations.
- Lipshitz, Wannamaker & Vanderkooy, "Quantization and Dither: A Theoretical Survey", JAES 1992 —
  why TPDF is the correct default.
- Lipshitz, Vanderkooy & Wannamaker, "Minimally Audible Noise Shaping", JAES 1991 — the
  noise-shaping filter family and its published coefficients.
- Wannamaker, "Psychoacoustically Optimal Noise Shaping", JAES 1992 — the weighting curves used for
  our own curve generation (§B.6).
- ISO 226:2003, *Normal equal-loudness-level contours* — the weighting target for curve fitting.
- ITU-R BS.775-3, *Multichannel stereophonic sound system with and without accompanying picture* —
  the downmix equations and the −3 dB fold convention.
- ITU-R BS.1770-4 — loudness weighting, used for the dither noise-floor comparison.
- ATSC A/52 (AC-3) Annex on downmixing — the Lt/Rt and Lo/Ro downmix definitions.
- Dolby Pro Logic II encoder specification and the ATSC/DVD-Audio literature reproducing its matrix
  constants — §B.4.3.

---

# PART C — `vaco-tx`

## C.1 Scope

One crate providing every transform the codec layer needs, across three precisions. The surface, from
the inventory's `AVTXType` table:

| Transform | Meaning | Precisions |
|---|---|---|
| `Fft` | Complex-to-complex DFT, forward and inverse | f32, f64, i32 |
| `Mdct` | Modified DCT; forward (N→N/2) and inverse (N/2→N, half or full output) | f32, f64, i32 |
| `Rdft` | Real-to-complex and complex-to-real DFT | f32, f64, i32 |
| `Dct` | DCT-II forward / DCT-III inverse | f32, f64, i32 |
| `DctI` | DCT-I, self-inverse up to scaling | f32, f64, i32 |
| `DstI` | DST-I, self-inverse up to scaling | f32, f64, i32 |

Flags, from the same table:

| Flag | Meaning |
|---|---|
| `INPLACE` | Output may alias input. Restricts algorithm choice for some sizes. |
| `UNALIGNED` | Caller's buffers carry no alignment guarantee. Disables the aligned load fast paths. |
| `FULL_IMDCT` | The inverse MDCT emits all `N` output samples rather than the `N/2` unique ones. |
| `REAL_TO_REAL` | Real input, real output — used by codecs that need only the real part. |
| `REAL_TO_IMAGINARY` | Real input, imaginary output. |

### C.1.1 Why this is on the critical path

Every transform-coded audio codec depends on it: AAC, AC-3/E-AC-3, MP3, Vorbis, Opus (CELT), DTS,
ATRAC, and the MPEG-family audio decoders. None of those can start before `vaco-tx` produces correct
output at the sizes they need. It is also the crate with the widest *size* requirements, which is
what makes the factorisation work in §C.3 non-optional.

Sizes real codecs demand (this list drives the algorithm choice, so it is worth being concrete):

| Codec | MDCT sizes (N) | Underlying complex FFT length (N/4) | Factorisation |
|---|---|---|---|
| AAC LC | 2048, 256 | 512, 64 | 2^k |
| AAC LD/ELD | 1024, 960, 512, 480 | 256, 240, 128, 120 | 2^k and 2^k·3·5 |
| AC-3 | 512, 256 | 128, 64 | 2^k |
| MP3 (hybrid) | 36, 12 | — (direct small transforms) | 2²·3², 2²·3 |
| Vorbis | 64 … 8192 | 16 … 2048 | 2^k |
| Opus / CELT | 480, 240, 120, 960 | 120, 60, 30, 240 | 2^k·3·5 |
| DTS | 512, 256, 64 | 128, 64, 16 | 2^k |
| ATRAC1/3 | 512, 256, 64 | 128, 64, 16 | 2^k |

**The conclusion that shapes the whole design:** power-of-two alone is insufficient. Opus and
AAC-LD/ELD need radix 3 and radix 5. MP3's hybrid synthesis needs radix 3. A power-of-two-only FFT
would leave those codecs to implement their own transforms, which defeats the purpose of a shared
crate.

## C.2 Public API — plan/execute split

Setup cost (twiddle-factor tables, permutation indices, algorithm selection) is paid once; execution
is a hot-path call.

```rust
/// Sample types the transforms are generic over.
pub trait TxSample: Copy + Send + Sync + 'static {
    /// The type of the scale parameter (f32/f64 for float, i32 fixed-point Q31 for integer).
    type Scale: Copy;
}
impl TxSample for f32 { type Scale = f32; }
impl TxSample for f64 { type Scale = f64; }
impl TxSample for i32 { type Scale = i32; }   // Q31 fixed point

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TxKind { Fft, Mdct, Rdft, Dct, DctI, DstI }

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Direction { Forward, Inverse }

bitflags! {
    pub struct TxFlags: u32 {
        const INPLACE           = 1 << 0;
        const UNALIGNED         = 1 << 1;
        const FULL_IMDCT        = 1 << 2;
        const REAL_TO_REAL      = 1 << 3;
        const REAL_TO_IMAGINARY = 1 << 4;
    }
}

/// Immutable, shareable setup: twiddles, permutation tables, the selected algorithm.
/// `Send + Sync`, cheap to `Arc`, built once per (kind, direction, length, scale, flags).
pub struct Plan<T: TxSample> { /* … */ }

impl<T: TxSample> Plan<T> {
    pub fn new(
        kind: TxKind,
        dir: Direction,
        len: usize,
        scale: T::Scale,
        flags: TxFlags,
    ) -> Result<Arc<Self>>;

    pub fn len(&self) -> usize;
    /// Elements the input buffer must hold.
    pub fn input_len(&self) -> usize;
    /// Elements the output buffer must hold.
    pub fn output_len(&self) -> usize;
    /// Scratch elements an executor needs. Zero for most plans.
    pub fn scratch_len(&self) -> usize;
    /// Introspection: the chosen decomposition, for tests and `-v debug`.
    pub fn describe(&self) -> PlanDescription;
}

/// A plan bound to its own scratch. `Send`, not `Sync` — one per thread.
/// This is what a decoder holds.
pub struct Tx<T: TxSample> {
    plan: Arc<Plan<T>>,
    scratch: Vec<T>,
}

impl<T: TxSample> Tx<T> {
    pub fn new(plan: Arc<Plan<T>>) -> Self;

    /// Out-of-place execution. `input` and `output` must not alias.
    pub fn execute(&mut self, output: &mut [T], input: &[T]);

    /// In-place execution. Requires the plan to carry `TxFlags::INPLACE`.
    pub fn execute_inplace(&mut self, buf: &mut [T]);
}
```

Three deliberate API decisions:

1. **`Plan` is `Sync`, `Tx` is not.** Twiddle tables are read-only and should be shared across
   threads; scratch is mutable and must not be. Splitting them is what lets a frame-threaded decoder
   build one plan and hand each worker its own `Tx` — a common pattern that a single fused type makes
   awkward.
2. **No `Complex<T>` type in the API.** Complex data is passed as interleaved `[re, im, re, im, …]`
   slices of `T`. This avoids inventing a public complex type, avoids a foreign one (D11), and
   matches how every caller already stores its data. The *internal* representation is split-complex
   (§C.6), and the conversion happens at the boundary where it is cheap.
3. **`execute` takes `&mut self`** even though the transform is mathematically pure, because scratch
   is mutated. That is honest about the aliasing and lets us avoid interior mutability.

Convenience constructors keep the common cases readable:

```rust
impl Plan<f32> {
    pub fn fft(len: usize, inverse: bool) -> Result<Arc<Self>>;
    pub fn mdct(len: usize, inverse: bool, scale: f32) -> Result<Arc<Self>>;
    pub fn rdft(len: usize, inverse: bool) -> Result<Arc<Self>>;
}
```

## C.3 Algorithm selection and factorisation

### C.3.1 The decomposition

`Plan::new` factors `len` and builds a decomposition tree. The rules, applied in order:

1. **Power of two** → **conjugate-pair split-radix**, iterative, with precomputed twiddles.
   Split-radix has the lowest known operation count for power-of-two DFTs among the classical
   algorithms; the conjugate-pair variant (Johnson & Frigo, "A Modified Split-Radix FFT with Fewer
   Arithmetic Operations", IEEE Trans. Signal Processing 2007) reduces it further and — more
   importantly for us — has a more regular memory access pattern than the classical form, which
   matters more than the operation count once the kernel is vectorised.
2. **Composite with all factors in the kernel set `{2, 3, 4, 5, 7, 8}`** → **mixed-radix
   Cooley–Tukey**, with hardcoded straight-line butterflies for each radix. Radix 4 and 8 are
   included as distinct kernels rather than being composed from radix 2, because the larger radix
   reduces both the number of passes and the twiddle multiplications.
3. **Composite with coprime factors** → **Good–Thomas (prime-factor algorithm)** for the coprime
   split, which eliminates the twiddle multiplications between those stages entirely, then recurse.
   For `N = 120 = 8 · 3 · 5` — an Opus size — this is a genuine win.
4. **Prime `p` not in the kernel set, with `p` small enough that `2(p−1)−1` is a convenient
   length** → **Rader's algorithm**: the length-`p` DFT becomes a length-`(p−1)` cyclic convolution,
   computed by a power-of-two FFT.
5. **Anything else** → **Bluestein's chirp-z**: `N` becomes a convolution of length `M ≥ 2N−1`,
   `M` chosen as the next power of two. Universal, works for every `N`, costs roughly 3× a
   same-length power-of-two FFT plus the padding.

Rule 5 is the safety net that makes `Plan::new` total: **there is no length we cannot transform.**
That property is worth the Bluestein implementation on its own, because "the codec needs 1381 points"
is the kind of requirement that appears once, late, and blocks everything.

```rust
pub enum Decomposition {
    SplitRadix { log2n: u32 },
    MixedRadix { radices: Vec<u8>, sub: Vec<Decomposition> },
    PrimeFactor { factors: Vec<usize>, sub: Vec<Decomposition> },
    Rader { p: usize, inner: Box<Decomposition> },
    Bluestein { m: usize, inner: Box<Decomposition> },
    Small { n: usize },        // fully unrolled straight-line kernel, n in {2,3,4,5,7,8}
}
```

`describe()` returns this tree, which makes "why is this size slow?" answerable by printing it rather
than by profiling.

### C.3.2 Cache behaviour for large transforms

Above roughly `L1/2` bytes of working set (`N ≳ 4096` for `f32` complex), a straightforward iterative
FFT thrashes: the late stages have small strides and good locality, the early stages have large
strides and terrible locality. The standard fix, which we adopt, is the **four-step / six-step
algorithm**: factor `N = N₁ · N₂` with both near `√N`, treat the data as an `N₁ × N₂` matrix, and do
column FFTs, a twiddle multiply, a transpose, and row FFTs. Each sub-FFT then fits in cache and the
transpose is a blocked operation with controlled access patterns.

This matters for Vorbis (up to `N = 8192`, so a 2048-point complex FFT) and for any future use in
video (large convolutions). It is a second-phase optimisation, not first-cut work, and it is listed
separately in the effort table so it can be deferred visibly rather than forgotten.

## C.4 Derived transforms

All five non-FFT transforms reduce to a complex FFT plus pre/post processing. This is standard and is
what makes one crate cover the whole surface.

### C.4.1 MDCT

The MDCT of `N` inputs producing `N/2` outputs:

```
  X[k] = Σ_{n=0}^{N−1} x[n] · cos( (π/(N/2)) · (n + 1/2 + N/4) · (k + 1/2) ),   k ∈ [0, N/2)
```

Computed via an `N/4`-point complex FFT:

1. **Pre-rotation**: fold the `N` real inputs into `N/4` complex values, applying the
   `exp(−iπ(n + 1/8)/(N/4))` rotation. The fold uses the MDCT's built-in symmetry, which is why the
   input length collapses by 4 rather than 2.
2. **`N/4`-point complex FFT.**
3. **Post-rotation**: apply the matching twiddle and unpack to the `N/2` real outputs.

The IMDCT is the same steps in reverse with conjugated twiddles. `FULL_IMDCT` emits all `N` samples
by applying the MDCT's antisymmetry (`x[N−1−n] = −x[n]` on the appropriate halves) rather than
computing anything extra — it is a fill, not a transform.

The pre/post rotations are `N/4` complex multiplies each, so an MDCT costs roughly an `N/4` FFT plus
`N/2` complex multiplies. That factor-of-four reduction is the entire reason MDCT-based codecs are
practical, and it is worth stating because a naive implementation that does an `N/2` FFT is 2× slower
and easy to write by accident.

**Windowing is the caller's job.** The MDCT's window (sine, KBD, Vorbis power-complementary) is
codec-specific and belongs in `vaco-codec-dsp-sinewin`, not here. `vaco-tx` transforms; it does not
window. This keeps the crate free of codec knowledge, per architecture §1.5.

### C.4.2 RDFT

An `N`-point real-input DFT via an `N/2`-point complex FFT: pack the real input as `N/2` complex
values (even samples real, odd samples imaginary), transform, then apply the split step

```
  X[k]      = (Z[k] + conj(Z[N/2−k]))/2 + (−i/2)·exp(−2πik/N)·(Z[k] − conj(Z[N/2−k]))
```

recovering the `N/2 + 1` unique complex outputs. The inverse packs and unpacks in the reverse order.
`REAL_TO_REAL` and `REAL_TO_IMAGINARY` skip the corresponding half of the unpack, saving the
multiplies for the discarded part rather than computing and throwing away.

### C.4.3 DCT-II and DCT-III

```
  DCT-II:   X[k] = Σ_{n} x[n] · cos( π(2n+1)k / (2N) )
  DCT-III:  X[k] = x[0]/2 + Σ_{n≥1} x[n] · cos( π(2k+1)n / (2N) )
```

DCT-III is the inverse of DCT-II up to scaling, which is why the inventory's single `Dct` type covers
both via `Direction`. Computed via an `N/2`-point complex FFT with an even/odd input permutation
(`x[0], x[2], …, x[N−1], …, x[3], x[1]`) and a post-twiddle — the standard Makhoul decomposition
(Makhoul, "A Fast Cosine Transform in One and Two Dimensions", IEEE ASSP 1980).

### C.4.4 DCT-I and DST-I

```
  DCT-I:  X[k] = (x[0] + (−1)^k·x[N−1])/2 + Σ_{n=1}^{N−2} x[n]·cos(πnk/(N−1))
  DST-I:  X[k] = Σ_{n=1}^{N−1} x[n]·sin(πnk/N)
```

Both are computed by symmetric extension into a real DFT: DCT-I of length `N` uses an RDFT of length
`2(N−1)` on the evenly-extended sequence; DST-I of length `N−1` uses an RDFT of length `2N` on the
oddly-extended sequence. Both are self-inverse up to a scale factor, so `Direction` only changes the
scaling.

These two are the least-used transforms in the set (a handful of codecs and filters), so they get the
straightforward extension-based implementation and no specialised optimisation. Stating that
explicitly avoids someone spending three weeks on a DST-I nobody calls.

## C.5 Fixed-point (i32) transforms

This is the capability D10 names as likely to make the crate ours, and it deserves precision.

### C.5.1 Why it exists

Several codecs specify **fixed-point** decoding for conformance: the fixed-point AAC decoder
(ISO/IEC 14496-3 defines a fixed-point profile whose output is bit-exactly specified), AC-3's
fixed-point mode, and MP3's integer decoder. For these, "close enough" is a conformance failure, not
a quality trade-off. The conformance bitstreams come with expected output samples and the decoder
must reproduce them exactly.

The transform is where that requirement bites hardest, because it is the deepest arithmetic chain in
the decoder — every rounding decision compounds through `log₂ N` stages.

### C.5.2 The specification

The `i32` transform is not merely an implementation; it is a **specification** with a defined
arithmetic contract. Every operation is pinned:

**Representation.** Samples are Q31 fixed point (one sign bit, 31 fractional bits, nominal range
`[−1, 1)`). Twiddle factors are Q31.

**Complex multiply.** For `(a + bi)(c + di)` with all values Q31:

```
  re = round_shift(a as i64 * c as i64 − b as i64 * d as i64, 31)
  im = round_shift(a as i64 * d as i64 + b as i64 * c as i64, 31)

  fn round_shift(x: i64, s: u32) -> i32 {
      let r = (x + (1i64 << (s - 1))) >> s;
      r.clamp(i32::MIN as i64, i32::MAX as i64) as i32
  }
```

Round-half-up, then saturate. The `i64` intermediate is mandatory — the product of two Q31 values
needs 62 bits and there is no correct way to do it in 32.

**Butterfly scaling.** Each radix-2 stage can grow the magnitude by up to `√2`, so a `log₂N`-stage
FFT can grow by `√N`. Three standard policies exist; we pick one and pin it:

- *Unscaled* — no shifting, caller guarantees headroom. Fastest, overflows easily.
- *Scaled every stage* — shift right by 1 each stage. Never overflows, loses `log₂N` bits of
  precision.
- *Block floating point* — track a per-block exponent and shift only when needed. Best precision,
  most complex, and its output depends on the data.

**We use scale-every-stage**, with the shift folded into the `round_shift` above (`s = 32` rather
than `31` for the stages that scale). Rationale: it is the only one of the three whose output is a
pure function of the input with no data-dependent branching, which is what makes bit-exactness
testable and what makes the SIMD variants provably identical to the scalar reference. Block floating
point would give better SNR but introduces a data-dependent shift, and a data-dependent shift is a
place where a SIMD variant can diverge.

The total output scale is therefore `1/N` for a scale-every-stage FFT, and the `scale` parameter
compensates: it is applied as a Q31 multiply on input or output depending on direction.

**Every one of these choices is documented in the crate's docs and asserted by golden vectors**
(§C.8). Changing any of them is a breaking change to a codec's conformance, so they are versioned
alongside the crate.

### C.5.3 Precision expectations

Scale-every-stage costs roughly `log₂N / 2` bits of SNR. For `N = 512` (AC-3), that is ~4.5 bits,
leaving ~26 effective bits — comfortably above the 16- or 24-bit output the codecs produce. The
precision is measured, not assumed: §C.8's SNR test asserts a floor per size.

## C.6 SIMD strategy

Research §08 §6 item 13 is the finding to answer directly:

> "classic butterfly network: complex multiply-add + permute/transpose per stage. `std::simd` handles
> straight-line stages; real-world FFT libraries universally hand-tune the permute/twiddle stages, so
> expect intrinsics (or vetted external crates) to matter here more than most other areas."

That is correct about the *conventional* FFT data layout, and it is the reason `vaco-tx` is the
riskiest of the three crates for the D2 bet. Our answer has three parts.

### C.6.1 Split-complex internal layout — the main lever

Interleaved complex (`[re, im, re, im, …]`) makes a complex multiply a shuffle-heavy operation: the
real and imaginary parts of each product need lanes rearranged, which is exactly the permute problem
the research names.

**Split-complex** (separate `re[]` and `im[]` arrays) makes it pure lanewise arithmetic:

```rust
// (a + bi)(c + di), fully lanewise, zero shuffles.
fn cmul<const N: usize>(
    ar: Simd<f32, N>, ai: Simd<f32, N>,
    br: Simd<f32, N>, bi: Simd<f32, N>,
) -> (Simd<f32, N>, Simd<f32, N>)
where LaneCount<N>: SupportedLaneCount
{
    (ar * br - ai * bi, ar * bi + ai * br)
}
```

Four multiplies, two adds, no permutes, at any lane width, on any architecture. Conversion between
the caller's interleaved buffers and our split-complex internal form happens **once on input and once
on output** — an `O(N)` deinterleave against an `O(N log N)` transform, so under 5% overhead at
`N = 256` and less above.

This single decision removes most of the permute problem the research warns about. It is the same
lever §A.2 and §B.2 apply, and it is why all three crates make the same layout choice.

### C.6.2 Vectorise across sub-transforms, not within butterflies

The second lever. A radix-2 butterfly on two complex numbers is 4 lanes of work — hopeless for a
16-lane vector. But an FFT stage performs `N/2` *independent* butterflies. Processing `N` of them in
parallel lanes gives perfect vector utilisation with unit-stride access, at every stage where the
stride permits it.

Concretely: for the mixed-radix decomposition, the outer loop over sub-transforms becomes the vector
loop. For the split-radix power-of-two path, the early stages (large stride) are naturally
contiguous in the sub-transform index, and only the final `log₂(lane_width)` stages have strides
smaller than the vector width.

### C.6.3 The last stages, and the honest admission

The final `log₂(N_lanes)` stages — 3 stages at 8 lanes, 4 at 16 — have strides below the vector
width, and those genuinely need in-register permutes. Three options, in the order we try them:

1. **Absorb them into the output permutation.** The bit-reversal/digit-reversal permutation is
   already a pass we perform. Folding the last stages' data movement into that pass's index table
   means the permutes become part of a scatter we were doing anyway, executed with precomputed
   indices rather than in-register shuffles.
2. **Radix-8 or radix-16 final kernels.** A single unrolled straight-line kernel for the last three
   or four stages combined, written once and hoisted entirely into registers, so the permutes are
   compile-time-known lane rearrangements that `simd_swizzle!` expresses directly. `simd_swizzle!` is
   safe, const-indexed, and lowers to the architecture's native shuffle — this is portable SIMD doing
   exactly what the research says needs intrinsics.
3. **Accept the loss and measure it.** If the last stages cost 20% and they are 15% of the work, the
   net is 3%. That may be fine.

**The commitment:** we measure this specifically, at `N ∈ {64, 256, 1024, 4096}`, against the
reference binary and against `rustfft` (as a dev-dependency benchmark oracle, §C.10.3), and we report
the number rather than assuming it. If portable SIMD loses badly here and nowhere else, that is
precisely the kind of specific, evidenced finding D2's escalation clause exists for — and `vaco-tx`
is the cheapest place in the project to discover it.

### C.6.4 Twiddle tables

Twiddles are precomputed in `f64` at plan time and stored in the execution precision, in
**split-complex, stage-major, pre-permuted order** — the order the kernel walks them, so the twiddle
access is a linear scan with no indexing arithmetic in the inner loop. Table size is `O(N)` per
transform; at `N = 4096` in `f32` that is 32 KB, which fits L1 alongside the data for the sizes that
matter.

For the `i32` path the twiddles are computed in `f64` and rounded to Q31 with round-half-up — one
more pinned rounding decision, and one that must match across every plan for the same length, so it
is asserted by a golden table.

## C.7 Reproducibility

| Path | Class | Notes |
|---|---|---|
| `i32` transforms, all kinds and sizes | **A, mandatory** | Every operation pinned (§C.5.2). Bit-identical across architecture, lane width and build profile. This is a conformance requirement, not a nicety. |
| `f32`/`f64` with `bitexact` | **B** | Fixed operation order, no FMA, fixed butterfly ordering, scalar twiddle rounding. |
| `f32`/`f64` default | **C** | FMA permitted, lane width may reorder independent operations. Bound: relative RMS error ≤ `2^-20` for `f32` and `2^-48` for `f64` against an `f64`/`f128`-equivalent reference, at every supported size. |

The `i32` guarantee is stronger than anywhere else in the project, and it is enforced differently: not
by a property test but by **golden vectors** committed to the repository — for each `(kind, direction,
len, flags)` combination a codec actually uses, a fixed input and its expected output, generated by
the scalar reference and reviewed on introduction. Every SIMD variant must reproduce them exactly.
If a golden vector changes, that is a deliberate, reviewed, codec-affecting decision.

## C.8 Test strategy

**Unit.** Each small-radix kernel against a directly-evaluated DFT. Each pre/post-rotation against
its closed form.

**Property (proptest).**
- **Round-trip**: `inverse(forward(x)) ≈ x · N` (or `≈ x` with the appropriate scale) for every
  supported `len`, every kind, every precision. Bounds per §C.7.
- **Linearity**: `T(ax + by) = aT(x) + bT(y)` within the error bound. Catches whole classes of
  indexing bug that round-trip alone misses, because a broken permutation that is its own inverse
  round-trips fine.
- **Parseval**: `Σ|x[n]|² = (1/N)·Σ|X[k]|²`. An energy check that catches scaling errors the
  round-trip cancels out.
- **Known transforms**: DC input → single non-zero bin; unit impulse → flat magnitude; a pure
  sinusoid at bin `k` → energy concentrated at bin `k`. These are the tests that fail loudly when the
  twiddle sign convention is wrong, which is the single most common FFT bug.
- **Shift theorem**: a circular shift in time is a linear phase ramp in frequency.
- **Decomposition agreement**: for lengths reachable by more than one rule in §C.3.1 (e.g. `N = 64`
  via split-radix and via mixed-radix 8×8), force each and require agreement within bounds. This
  tests the decomposition selector itself, which is otherwise untested code.

**Direct-DFT oracle.** For `N ≤ 512`, compare against a naive `O(N²)` DFT computed in `f64`. Slow but
unambiguous, and it is the ground truth that makes the property tests meaningful.

**Golden vectors** for `i32`, per §C.7.

**Differential against the reference binary.** `vaco-tx` has no CLI surface, so it cannot be compared
directly — a real difference from the other two crates. Two indirect routes, and both matter:
1. **Through codecs.** Once a codec using `vaco-tx` exists, its decoded output is compared frame-by-
   frame against the reference. A transform error shows up immediately and unmistakably as broken
   audio. D11's observation applies directly here: transform output feeding codec conformance tests
   makes fidelity measurable, just not in isolation.
2. **Against published conformance suites.** The AAC and AC-3 conformance bitstreams have
   specified output. A fixed-point transform bug fails them exactly.

**Fuzzing.** `Plan::new` over arbitrary `(kind, dir, len, flags)` — every length from 1 to 65536 plus
random large primes — asserting either a valid plan or a clean error, never a panic, never an
unbounded allocation, never a hang. The Bluestein path is the one to watch here, since its internal
length is `2N−1` rounded up and a caller asking for `len = 2^30` must be rejected rather than
attempted.

## C.9 Benchmarks

Reporting nanoseconds per transform and cycles per point, single-threaded:

| # | Scenario | Why |
|---:|---|---|
| 1 | `f32` complex FFT, `N = 64, 128, 256, 512, 1024, 4096` | The core. Compared against the reference and against `rustfft`. |
| 2 | `f32` MDCT/IMDCT, `N = 2048, 256` | AAC LC's transform, the single most-executed transform in real workloads. |
| 3 | `f32` MDCT/IMDCT, `N = 512, 256` | AC-3. |
| 4 | `f32` complex FFT, `N = 120, 240, 480, 960` | Opus/CELT — the mixed-radix 2·3·5 path. |
| 5 | `f32` RDFT, `N = 512, 2048` | |
| 6 | `f32` DCT-II, `N = 32, 512` | |
| 7 | `i32` FFT and MDCT at the sizes in 1–3 | The fixed-point path; expected slower than float, and we want to know by how much. |
| 8 | `f64` FFT, `N = 1024` | Used by some analysis filters. |
| 9 | `Plan::new` cold setup at each size in 1–7 | Twiddle generation is `O(N)` transcendental evaluations; a codec creating plans per stream cares. |
| 10 | Scenario 1 with `bitexact` | The cost of Class B. |
| 11 | Scenario 1 at `N = 8192, 32768` | The cache-blocking threshold; tells us when §C.3.2 becomes necessary. |

**Target:** within 20% of the reference on scenarios 2–4 (the ones real codecs execute), and within
30% of `rustfft` on scenario 1. The looser target than the other two crates is deliberate and
honest — this is the area research §08 flags as hardest for portable SIMD, and setting a target we
believe rather than one we would like is the point of having targets.

## C.10 Build-or-buy assessment (D10)

This is the one place among the three crates where adoption is genuinely arguable, so it gets a full
treatment rather than a verdict.

### C.10.1 The candidates against the gates

| | `rustfft` | `realfft` |
|---|---|---|
| 1. Pure Rust, zero FFI | **Pass** | **Pass** |
| 2. Licence | **Pass** — MIT OR Apache-2.0 | **Pass** — MIT OR Apache-2.0 |
| 3a. Alive | **Pass** — actively maintained | **Pass** |
| 3b. Adopted | **Pass** — the de-facto Rust FFT, very high download counts | **Pass** |
| 3c. Sound | **Pass** — no open RUSTSEC advisory | **Pass** |
| 3d. Shallow | **Pass** — small tree (`num-complex`, `num-traits`, `primal-check`, `strength_reduce`, `transpose`) | **Pass** — depends on `rustfft` |
| 3e. Unsafe-light | **Fail on the metric, by design.** Heavy `unsafe` throughout: `core::arch` intrinsics for SSE/AVX/NEON, unchecked indexing in inner loops. `cargo-geiger` will report a large count. | Inherits `rustfft`'s |
| 3f. Vendorable | **Pass** — we could fork it | **Pass** |

So both clear the hard gates and land on D10's explicit "unsafe tension" clause: *measure it, prefer
the lower-unsafe option when otherwise comparable, treat heavy unsafe on a hot path as a deliberate
trade-off to be argued — not an automatic disqualification.*

### C.10.2 The judgement call

**What `rustfft` covers:** complex-to-complex FFT, any length, any direction, `f32` and `f64`, with
mature algorithm selection (it implements essentially the same decomposition rules as §C.3.1,
including Rader and Bluestein) and well-optimised SIMD. It is a genuinely good implementation of
exactly the thing §C.3 describes.

**What it does not cover:**

| Requirement | `rustfft` | `realfft` |
|---|---|---|
| Complex FFT, f32/f64 | ✅ | via `rustfft` |
| Real-to-complex DFT (RDFT) | ❌ | ✅ |
| MDCT / IMDCT, `FULL_IMDCT` | ❌ | ❌ |
| DCT-II/III | ❌ (`rustdct`, a sibling crate, provides it) | ❌ |
| DCT-I, DST-I | ❌ (`rustdct` partially) | ❌ |
| **i32 fixed point, bit-exact** | ❌ | ❌ |
| `REAL_TO_REAL` / `REAL_TO_IMAGINARY` | ❌ | ❌ |
| Split-complex internal layout | ❌ (interleaved `Complex<T>`) | ❌ |
| Bit-exactness contract across builds | ❌ not offered | ❌ |

The decisive facts, in order of weight:

1. **Fixed point is absent and cannot be added from outside.** `rustfft` is generic over
   `FftNum: Float`, which structurally excludes integer arithmetic — this is not a missing feature,
   it is a type-level exclusion. The i32 paths are ours regardless of what we decide about float.
   D10 names this case explicitly as the concrete example likely to make `vaco-tx` ours.
2. **Half the transform surface is missing.** MDCT is the transform audio codecs actually use, and it
   is absent from both crates. `rustdct` covers DCT-II/III but not MDCT, and adding it would mean
   depending on two crates and still writing the MDCT.
3. **The layout mismatch is a real cost, not a formality.** §C.6.1 makes split-complex the central
   SIMD decision. `rustfft` works in interleaved `Complex<T>`. Wrapping it means converting layouts
   at every call — which is fine for the FFT itself but means our MDCT pre/post-rotation, written in
   split-complex, converts twice per transform for no benefit.
4. **We would maintain two FFTs anyway** — theirs for float, ours for fixed point — with two sets of
   size-support rules, two failure modes, and a golden-vector suite that only covers one.

### C.10.3 The decision

**`vaco-tx` implements everything, including the float FFT. `rustfft` is adopted as a
dev-dependency benchmark and correctness oracle only.**

The reasoning, stated as a trade rather than a dismissal:

- Using `rustfft` for float would save perhaps **4–5 person-weeks** — the split-radix and mixed-radix
  kernels, which are the best-understood part of the crate.
- It would cost: a second FFT implementation to maintain for fixed point anyway; a layout conversion
  on every MDCT; a divided golden-vector story; a substantial `unsafe` surface on one of the
  hottest paths in the project, weakening exactly the end-to-end safety claim D2 exists to make; and
  it would remove the crate that is our best early evidence about whether portable SIMD can handle
  butterfly networks (§C.6.3) — the very question we most need answered early.

That last point converts what looks like a cost into a benefit. `vaco-tx` is where the D2 bet is
hardest to win. Outsourcing it would make the project *look* fine while leaving the hard question
unasked until a codec needed it.

**The oracle role is real and worth keeping.** `rustfft` as a dev-dependency gives us: an independent
correctness reference at every size (far better than our naive DFT above `N = 512`), and a
performance target from a mature optimised implementation. Neither ships. D11's boundary rule
concerns crates reachable from shipped code, so a dev-dependency raises no boundary question — but we
record it in `docs/dependencies.md` anyway, because "why is this in the lockfile" deserves an answer.

**If the §C.6.3 measurement goes badly** — portable SIMD losing by more than ~40% on the butterfly
stages with no remedy — the fallback is D11's backend feature exactly as the coordinator framed it:

```toml
[features]
default          = ["backend-native"]
backend-native   = []
backend-external = ["dep:rustfft"]      # float paths only; i32 stays native either way
```

`rustfft` would then be reachable from `crates/dsp/vaco-tx/Cargo.toml` and nowhere else in the
workspace; no codec crate would name it, see its `Fft` trait, or hold a `Complex<f32>`. The public
API in §C.2 does not change, the tests do not change, and the golden vectors continue to gate the
fixed-point paths. That is the whole point of the boundary: **this decision is reversible, and it is
reversible in one crate.** Which is exactly why it is safe to start native.

### C.10.4 Dependencies `vaco-tx` does take

`thiserror`, `bitflags`, plus workspace-internal `vaco-core` and `vaco-simd`. Dev-dependencies:
`proptest`, `criterion`, `rustfft` (oracle, §C.10.3). Notably **no `num-complex`** — §C.2's API
avoids a public complex type entirely, and the internal split-complex representation needs none.

## C.11 Fidelity assessment — what "identical to ffmpeg" means here

`vaco-tx` differs from the other two crates in a way that makes this section shorter and sharper:
**it has no CLI surface and no direct output.** Nobody runs `ffmpeg -tx`. So "identical to ffmpeg"
is not directly measurable, and the question decomposes differently.

### C.11.1 Per-class verdict

| Class | Byte-identical achievable? | Confidence | Notes |
|---|---|---|---|
| **1. `i32` fixed-point transforms** | **Yes — and it is mandatory, but the target is the *specification*, not the reference binary** | High | See §C.11.2. This is the crucial distinction in this section. |
| **2. `f32`/`f64` transforms, in isolation** | **No, and it does not matter** | High | Different butterfly orderings give different last bits. No consumer of the crate can observe this directly. |
| **3. Float transforms as seen through a codec's decoded output** | **Usually yes, in practice** | Medium | See §C.11.3. |
| **4. Plan-time behaviour (which sizes are supported, what errors are returned)** | **Not applicable** | — | Internal. Our support set is a superset (Bluestein makes it total). |

### C.11.2 Fixed point: match the specification, not the implementation

For the `i32` paths the reference binary is **not** the authority — the codec specification is. The
fixed-point AAC profile in ISO/IEC 14496-3 and the AC-3 fixed-point mode define the expected decoder
output, and conformance bitstreams ship with reference output samples. Our target is those samples.

This is a materially better position than the other two crates are in, because:

- The requirement is written down, publicly, in a normative document. There is no reverse
  engineering.
- Conformance suites exist and are the industry's own acceptance test.
- If our output matches the conformance vectors and the reference binary's output differs, **the
  reference binary is wrong**, and we should say so rather than match it.

**Grade target: Exact against conformance vectors.** Anything less blocks the affected codec from
shipping, because a fixed-point decoder that is not bit-exact is not a fixed-point decoder.

The corollary is that §C.5.2's arithmetic contract is not ours to choose freely — the rounding and
scaling policy must be whatever produces the specified output. §C.5.2 states our starting choice
(round-half-up, scale-every-stage) and the honest position is that **it may need to change** once
conformance vectors are run. That is scheduled work (C10 in §C.12), not a surprise.

### C.11.3 Float transforms seen through codecs

A float codec's decoded output is compared against the reference frame-by-frame. A last-bit
difference in the MDCT propagates into the decoded samples — so does byte-identical output survive?

**Usually yes, and the reason is quantisation.** Most float audio decoders produce `s16` or `s32`
output, and the transform's last-bit error is ~2^-20 relative while the output quantisation step is
2^-15 or 2^-31. At `s16` output, a 2^-20 error is 32× below the LSB and rounds away completely except
for inputs sitting within 2^-20 of a rounding boundary — which, over a long file, happens
occasionally. Empirically this means "byte-identical for the overwhelming majority of frames, with
rare single-LSB differences".

**Our position, per codec:**

- If a codec's differential test is byte-exact with our transform: grade **Exact**, and note that the
  margin is thin.
- If it shows rare single-LSB differences traceable to the transform: grade **Equivalent** with the
  tolerance "≤ 1 LSB on ≤ 0.01% of samples, zero differences ≥ 2 LSB", which is a bound tight enough
  that a real bug cannot hide behind it.
- If a codec specifies float output directly (e.g. decoding to `f32p`), the tolerance is
  ≥ 120 dB SNR, matching §B.14.3's float bound.

**Tightening lever if needed:** the `bitexact` flag (Class B) forces a canonical evaluation order.
If a specific codec's conformance requires byte-exactness through a float transform, that codec's
decoder sets `bitexact` and pays the throughput. Having that lever available at the transform level
is a design property worth keeping, and it is why Class B exists in this crate at all.

### C.11.4 The honest summary

Fixed point must be exact, is measured against a normative specification rather than against another
implementation, and is the crate's hardest and least negotiable requirement. Float need not be exact
in isolation, is not directly observable, and shows up only as thin-margin agreement through codec
output. The uncomfortable case is a codec whose conformance turns out to need byte-exact float
transform output — we have the `bitexact` lever for it, but we should expect to discover at least one
such case and budget for the investigation.

## C.12 Effort and work breakdown

| Workstream | Person-weeks | Depends on | Parallel? |
|---|---:|---|---|
| C1. API, `Plan`/`Tx` split, decomposition types, error taxonomy | 1 | `vaco-core` | — |
| C2. Small-radix kernels (2,3,4,5,7,8), scalar, all precisions | 1.5 | C1 | yes |
| C3. Split-radix power-of-two FFT, scalar reference | 1.5 | C2 | after C2 |
| C4. Mixed-radix + prime-factor composition and the decomposition selector | 2 | C2, C3 | after C3 |
| C5. Rader + Bluestein fallbacks | 1.5 | C4 | after C4 |
| C6. Derived transforms: MDCT/IMDCT (+`FULL_IMDCT`), RDFT (+R2R/R2I), DCT-II/III, DCT-I, DST-I | 3 | C3 | after C3 |
| C7. Split-complex layout, twiddle generation and ordering, boundary conversion | 1.5 | C3 | after C3 |
| C8. SIMD kernels: butterflies, cmul, all tiers, all widths | 4 | C7, `vaco-simd` | after C7 |
| C9. SIMD: last-stage strategy (§C.6.3) — measure, then implement the winning option | 2 | C8 | after C8 |
| C10. `i32` fixed-point paths: arithmetic contract, all kinds, golden vectors, conformance validation | 4 | C3, C6 | partially parallel |
| C11. Test infrastructure: property tests, direct-DFT oracle, `rustfft` oracle, fuzz targets | 2.5 | C1 | overlaps everything |
| C12. Benchmarks + CI regression tracking, incl. the §C.6.3 measurement | 1.5 | C8 | after C8 |
| C13. Cache-blocked large transforms (§C.3.2) | 2 | C8 | **deferrable** |
| C14. Documentation (`docs/tx/*.md`), incl. the arithmetic contract | 1 | all | overlaps |
| **Total** | **29** | | |
| **Total excluding deferrable C13** | **27** | | |

**Critical path:** C1 → C2 → C3 → C7 → C8 → C9, which is 11.5 pw serial. With three engineers the
calendar estimate is **~10–12 weeks**; with two, **~15 weeks**.

**Parallelisation plan (3 engineers):**
- **E1 (core FFT):** C1 → C2 → C3 → C4 → C5.
- **E2 (SIMD):** C7 → C8 → C9 → C12. Starts once C3's scalar reference exists.
- **E3 (derived + fixed point):** C6 → C10 → C14, with C11 threaded through from the start.

**C10 is the schedule risk**, for the reason §C.11.2 gives: the arithmetic contract may need revision
once conformance vectors are available, and conformance vectors may not be available until a codec
exists. Mitigation: build the golden-vector machinery early with *our* chosen contract, so that a
later contract change is a table regeneration and a review rather than a rewrite.

**C13 is explicitly deferrable** and should be deferred unless scenario 11 in §C.9 shows a problem at
the sizes real codecs use. Vorbis at `N = 8192` is the only realistic trigger.

## C.13 References an implementer works from

- Cooley & Tukey, "An Algorithm for the Machine Calculation of Complex Fourier Series",
  Math. Comp. 1965.
- Duhamel & Hollmann, "Split-radix FFT algorithm", Electronics Letters 1984.
- Johnson & Frigo, "A Modified Split-Radix FFT with Fewer Arithmetic Operations", IEEE TSP 2007 —
  the conjugate-pair variant used in §C.3.1 rule 1.
- Frigo & Johnson, "The Design and Implementation of FFTW3", Proc. IEEE 2005 — the planner model,
  the four-step/six-step cache algorithm of §C.3.2, and the codelet-generation approach that informs
  our small-radix kernels.
- Good, "The interaction algorithm and practical Fourier analysis", JRSS 1958; Thomas 1963 — the
  prime-factor algorithm.
- Rader, "Discrete Fourier transforms when the number of data samples is prime", Proc. IEEE 1968.
- Bluestein, "A linear filtering approach to the computation of discrete Fourier transform",
  IEEE Trans. Audio and Electroacoustics 1970.
- Winograd, "On computing the discrete Fourier transform", Math. Comp. 1978 — the small-radix
  minimal-multiplication kernels.
- Makhoul, "A Fast Cosine Transform in One and Two Dimensions", IEEE ASSP 1980 — the DCT-II/III
  reduction of §C.4.3.
- Princen & Bradley, "Analysis/synthesis filter bank design based on time domain aliasing
  cancellation", IEEE ASSP 1986; Princen, Johnson & Bradley 1987 — the MDCT and TDAC.
- Duhamel, Mahieux & Petit, "A fast algorithm for the implementation of filter banks based on
  time domain aliasing cancellation", ICASSP 1991 — the N/4-point FFT reduction of §C.4.1.
- Bosi & Goldberg, *Introduction to Digital Audio Coding and Standards*, Springer 2003 — MDCT
  windowing and the codec context.
- ISO/IEC 14496-3 (AAC, including the fixed-point profile) and ATSC A/52 — the normative fixed-point
  requirements of §C.5.1 and §C.11.2.
- Oppenheim & Schafer, *Discrete-Time Signal Processing*, 3rd ed. — DCT/DST definitions and the
  symmetric-extension reductions of §C.4.4.

---

# PART D — Cross-cutting

## D.1 Totals and sequencing

### D.1.1 Effort summary

| Crate | Person-weeks | Critical path (pw) | Calendar @ 3–4 eng |
|---|---:|---:|---|
| Prerequisite: `vaco-simd` substrate | 3 | 3 | 1.5 weeks (1 eng) |
| Prerequisite: `vaco-checkasm` harness | 2 | 2 | 1 week (1 eng) |
| `vaco-tx` (excl. deferrable C13) | 27 | 11.5 | 10–12 weeks @ 3 |
| `vaco-resample` | 32.5 | 9.5 | 12–14 weeks @ 3 |
| `vaco-scale` | 48.5 | 23 | 14–16 weeks @ 4 |
| **Total** | **113** | | |

113 person-weeks. Taken at face value with ten engineers running all three crates concurrently after
the substrate lands, the calendar is **~18 weeks** — the `vaco-scale` critical path plus the
substrate, since scale is both the largest and the longest-pole crate.

More realistic staffing (5–6 engineers) gives **~28–32 weeks** for all three, with `vaco-tx`
completing first (it is smallest and its critical path is shortest) and unblocking the audio codecs
while scale is still in progress.

**A caution on these numbers.** They assume the differential harness (D6) exists and works, and they
include the fidelity-probe work (A14, B14) that a naive plan omits. They do **not** include: the
`vaco-pixfmt` / `vaco-color` / `vaco-sampfmt` / `vaco-chlayout` layer-1 crates these depend on;
integration into the filter graph or the CLI; or hardware backends. They are also *implementation*
estimates and assume the design in this document is accepted rather than relitigated.

### D.1.2 Recommended sequence

```
Phase 0  (2.5 wk, 2 eng)   vaco-simd substrate  +  vaco-checkasm harness
                            │  Everything below depends on both. Do not start
                            │  DSP work before the differential kernel harness
                            │  exists — retrofitting it is how SIMD projects rot.
                            ▼
Phase 1  (parallel)  ┌─── vaco-tx        ──▶ unblocks ALL transform audio codecs
                     ├─── vaco-resample  ──▶ unblocks CLI audio, vaco-play audio
                     └─── vaco-scale     ──▶ unblocks CLI video, vaco-play video,
                                             the scale/format filters
```

**`vaco-tx` first among equals.** Not because it is most valuable in isolation — it is the least
directly user-visible of the three — but because the largest number of downstream crates block on it.
Every transform-coded audio codec is gated behind it, and audio codecs are the natural next milestone
after v0.1 (they are smaller than video codecs, several are GREEN under D9, and they exercise the
whole pipeline end-to-end). If staffing forces a choice, `vaco-tx` starts first.

**`vaco-scale` starts second and finishes last.** It has the longest critical path, so starting it
late makes it the schedule. Start it as early as staffing allows even if it progresses slowly.

**`vaco-resample` is the best first assignment for an engineer new to the project.** It is
self-contained, its mathematics is the most classical, its correctness criteria are the most
measurable (§B.12's signal-quality suite gives immediate, unambiguous feedback), and its option
surface teaches the `vaco-opts` idiom that every subsequent crate uses.

### D.1.3 What each crate proves

Each phase-1 crate answers a distinct question that the rest of the project depends on. Sequencing
them early is worth as much for the answers as for the code.

| Crate | The question it answers | Why it matters beyond itself |
|---|---|---|
| `vaco-scale` | Can safe portable SIMD match hand-written asm on regular, gather-free, pack-heavy integer kernels? | This is the *best case* for D2. If it fails here, D2 fails everywhere and we escalate immediately with numbers. |
| `vaco-tx` | Can safe portable SIMD handle butterfly networks and permute-heavy stages? | This is the *worst case* among the tractable areas (§08 §6 item 13). It brackets the answer from the other side. |
| `vaco-resample` | Can we reproduce the reference's numeric behaviour closely enough for a byte-exact differential harness to be a merge gate? | The answer determines whether D6 is a sharp tool or a permanently-yellow test suite, for the whole project. |

Getting all three answers within the first six months of implementation is worth more than the code
itself.

## D.2 Shared infrastructure these three define

Three pieces of infrastructure are built here and used everywhere afterwards. They should be designed
as shared infrastructure from the start, not extracted later.

**`vaco-simd`'s `KernelTable` model** (§0.5). These three crates are its first users and will
determine its shape. Every codec DSP crate (`vaco-codec-dsp-*`) uses the same model. Getting the
tier-cascade ergonomics right here saves the same argument thirty times.

**`vaco-checkasm`.** Architecture §3 names it as our clean-room equivalent of a tool whose upstream
implementation cannot be consulted under the clean-room rule. Its requirements are set by these crates: randomised
and edge-case input generation per kernel signature, differential comparison against a scalar
reference, cycle-accurate benchmarking with a nop baseline subtracted, and CLI filtering by
kernel-name pattern. Building it in phase 0 rather than alongside the first crate is the single
highest-leverage sequencing decision in this plan.

**The fidelity-probe pattern** (§A.15.1, §B.14.1). "Determine an implementation-defined constant by
black-box observation, record the probe and the observation, pin the constant" is a workflow every
codec will need. These three crates should produce a reusable harness and a documented convention —
a `probes/` directory with one file per pinned parameter, each naming the observation, the reference
binary version, the date and the reviewer — rather than three ad-hoc solutions.

## D.3 Risk register

| # | Risk | Likelihood | Impact | Mitigation |
|---:|---|---|---|---|
| 1 | Portable SIMD loses badly on FFT butterfly/permute stages (§C.6.3) | Medium | Medium | Measured explicitly and early (C9, C12). Fallback is D11's `backend-external` feature with `rustfft` for float paths only — one crate changes, the API does not. |
| 2 | Rate-conversion coefficient generation cannot be matched to the reference (§B.14.2) | Medium-high | Low-medium | Defined fallback: grade Equivalent at ≥ 100 dB SNR plus the independent §B.12 quality suite. Budgeted at 3 pw (B14) and scheduled with slack. |
| 3 | The ops graph is materially slower than fused per-format-pair kernels | Medium | High | The optimiser (§A.5) and the fused-pattern chain compiler (§A.6.2) exist precisely for this and are on the critical path, not deferred. Benchmark scenarios 1–3 are the early-warning signal; if they miss badly at the end of A9, A10 expands. |
| 4 | Error-diffusion dither diverges from the reference and cannot be reconciled (§A.15.1 class 8) | Medium | Low | Fallback to grade Equivalent under the same PSNR-plus-spectrum bound as the arithmetic dithers. Detected early because it is a self-contained probe. |
| 5 | Fixed-point transform arithmetic contract needs revision after conformance testing (§C.11.2) | Medium | Medium | Golden-vector machinery built early (C10) so a contract change is a table regeneration, not a rewrite. |
| 6 | A codec turns out to need byte-exact *float* transform output for conformance (§C.11.4) | Low-medium | Medium | The `bitexact` flag (Class B) exists at the transform level for exactly this. Cost is throughput on that codec only. |
| 7 | HDR tone mapping produces visibly different results from the reference despite being within tolerance | Medium | Low | ΔE₀₀ ≤ 1.0 on a colour-patch set (§A.15.2) is a perceptual bound, not just a numeric one — it is specifically chosen to catch "within PSNR but looks wrong". |
| 8 | Effort estimates are optimistic because the option surface is under-appreciated | Medium-high | Medium | The estimates already carry explicit line items for option surfaces (A12, B12) and fidelity probes (A14, B14), which are the two things most commonly omitted. Treat a 25% overrun as the planning case. |
| 9 | The three crates diverge in conventions (buffer models, kernel dispatch, error handling) because they are built in parallel | Medium | Medium | §0.5 states the shared conventions explicitly and all three follow them. A single cross-crate design review at the end of phase 0, before parallel work starts, is cheap insurance. |
| 10 | `vaco-scale`'s optimiser has a soundness bug that produces subtly wrong output in a rare graph shape | Medium | High | The optimiser-soundness property test (§A.16) compares optimised against unoptimised execution on randomly generated graphs, per-pass, with each pass individually toggleable. This is the highest-value test in the crate and is listed first in A13. |

## D.4 Documentation deliverables

Per the repository standard (`CLAUDE.md`, architecture §9), each crate lands with its docs in the
same change. Planned files:

```
docs/scale/README.md              — what it is, the ops-graph model, when to reach for it
docs/scale/ops-graph.md           — the op vocabulary, graph construction, the IR
docs/scale/optimiser.md           — the passes, what each does, how to add one, how to debug one
docs/scale/kernels.md             — dispatch, KernelSet, adding a fused pattern
docs/scale/filters.md             — the kernel formulas, bank generation, coefficient layout
docs/scale/colour.md              — H.273 mapping, matrices, transfer functions, primaries
docs/scale/tone-mapping.md        — intents, BT.2390 EETF, the 3D LUT
docs/scale/dither-alpha.md
docs/scale/fidelity.md            — §A.15's table, kept current, with the probe index
docs/resample/README.md
docs/resample/format-convert.md   — the numeric contract of §B.3.2, which callers rely on
docs/resample/rematrix.md         — matrix construction, mix levels, encodings
docs/resample/rate-convert.md     — polyphase structure, filter design, exact-rational
docs/resample/dither.md           — methods, the noise-shaping curve derivation and why it is ours
docs/resample/compensation.md     — soft/hard/async, and how they interact
docs/resample/compatibility.md    — accepted-but-unhonoured options (soxr), declared divergences
docs/resample/fidelity.md
docs/tx/README.md
docs/tx/algorithms.md             — decomposition rules, factorisation, why each algorithm
docs/tx/fixed-point.md            — **the arithmetic contract**; normative for codec conformance
docs/tx/simd.md                   — split-complex, cross-transform vectorisation, the last stages
docs/tx/fidelity.md
docs/dependencies.md              — the D10 adoption record (incl. dev-only oracles)
```

`docs/tx/fixed-point.md` deserves special note: it is not documentation of an implementation, it is a
**specification that codecs depend on**. It is versioned with the crate and changing it is a
codec-affecting decision.

## D.5 Open questions

Recorded rather than resolved, each with the point at which it must be answered.

1. **Does the reference apply range conversion before or after the colour matrix?** Determines
   whether our fused affine matches bit-for-bit (§A.15.1 class 3). *Answer by probe, during A14.*
2. **Round-half-away or round-half-to-even for float→integer sample conversion?** (§B.3.2)
   *Answer by probe, early in B14 — it is one probe and it gates class 3.*
3. **What is the mono→stereo upmix gain?** 1.0 or 1/√2 (§B.4.2). *Answer by probe, during B14.*
4. **Is `exact_rational` on or off by default in the reference, and does it change output?**
   Determines whether our preferred default (§B.10) is compatible. *Answer by probe, during B14.*
5. **Can the rate-conversion coefficient bank be matched at all?** (§B.14.2) The largest open
   question in this plan. *Answer during B14; both outcomes have defined consequences.*
6. **How much does portable SIMD lose on the FFT's last stages?** (§C.6.3) *Answer during C9/C12.*
7. **Does the `i32` arithmetic contract in §C.5.2 produce conformance-correct output?**
   *Answer when the first fixed-point codec runs conformance vectors — which may be after `vaco-tx`
   is otherwise complete. Plan for the contract to be revisited.*
8. **Is the ops graph fast enough without fused kernels for the long-tail formats?** We will ship
   fused patterns for the hot conversions; the tail runs the per-op path. *Answer from benchmark
   scenario coverage during A15; if the tail is unacceptably slow, the pattern table grows.*
9. **Should `vaco-scale` expose a GPU backend seam now or later?** The inventory shows the reference
   has a SPIR-V backend over the same ops graph. Our graph is backend-agnostic in principle. We do
   not build one, but the `Pass`/`CompiledChain` seam is where one would attach. *Revisit after
   `vaco-play` exists and has a `wgpu` context we could reuse.*
10. **Is per-channel threading ever worth it in `vaco-resample`?** (§B.9) *Answer from benchmark
    scenario 7 at 8 channels; deferred until then.*

## D.6 What this plan deliberately rejects

| Rejected | Why |
|---|---|
| Per-format-pair scaling kernels | Architecture §10 and §A.1. `n`×`m` instead of `n`+`m`, and it is the architecture upstream is migrating away from. |
| A fused `SwrContext`-equivalent monolith | §B.1. The three stages are independently useful and independently testable; exposing them costs one composition layer. |
| Adopting `rubato`, `yuv`, or `dcv-color-primitives` | §A.14, §B.13. All clear D10's hard gates; all fail on model fit, because our requirement surface is an option surface, not a mathematical operation. |
| Adopting `rustfft` for the shipped float paths | §C.10.3. Saves 4–5 pw, costs a second FFT for fixed point anyway, a layout conversion per MDCT, a large `unsafe` surface on a hot path, and the early answer to the hardest D2 question. Kept as a dev-only oracle, with a defined fallback route via D11's backend feature. |
| Block-floating-point fixed-point FFT | §C.5.2. Better SNR, but data-dependent shifts make bit-exactness across SIMD variants unprovable. |
| Parallel error-diffusion dither with per-band state | §A.9.3. Produces band seams and thread-count-dependent output. We pay throughput for reproducibility. |
| `f64` pixel arithmetic | §A.5.8. No evidence it is ever needed; halves throughput. `f64` is plan-time only. |
| Interleaved-complex internal FFT layout | §C.6.1. Split-complex removes most of the permute problem for free. |
| A `threads` option on `vaco-resample` | §B.9. It would invite configurations slower than the default. |
| Silently aliasing unimplementable options | §B.13.3. Accept the name, do the sensible thing, and log it. Silent divergence is the one failure mode we never choose. |
| Widening a tolerance to make a failing test pass | §A.15.3, §B.14.4. Every tolerance in this document has a derivation attached; changing one is a reviewed diff. |

---

## Corrections from implementation (vaco-tx, 2026-08-22)

**§C.3.1 and §C.5.2 are incompatible, and the conflict is real.** Split-radix
decomposes N into one N/2 and two N/4 sub-blocks *at different depths*, so
§C.5.2's fixed-point contract — "divide by the radix at every stage" — has no
meaning under it. Only one could survive. The arithmetic contract was kept,
because codec conformance depends on it, and Stockham radix-8/4/2 replaces
split-radix. Cost is single-digit percent of arithmetic on pure powers of two.
**Split-radix is not implemented, and reinstating it means restating the
fixed-point contract first.**

**The "last log₂(lanes) stages" are the *first* stages.** In a Stockham flow the
stride starts at 1 and multiplies by the radix, so with largest-radix-first
exactly **one** stage falls below the vector width, and it is at the beginning.
The plan describes the opposite end.

**A counterintuitive measurement worth carrying to every other SIMD crate.**
Preferring radix 4 over radix 8 *improves* the SIMD-versus-scalar ratio (1.82x
against 1.40x at n=1024) while making the transform **slower** in absolute terms
(3.54 µs against 2.92 µs). **The vector/scalar ratio is a misleading optimisation
target on its own** — it improves when the scalar path gets worse. Optimise
absolute time; use the ratio only to find where the vector path is not working.

Measured on Apple M5, NEON, 4-lane f32, same plan run twice through a scalar hook
so the ratio isolates kernels: 1.22x at n=64 rising to 1.64x at n=4096. The
un-vectorised first stage is *not* the main limiter — it is one stage of four at
n=4096, worth at most 1.25x. The rest is radix-8 register pressure (16 live
vectors, half the NEON file) and the O(n) boundary interleave. x86 unmeasured.

**`Tx` is `Sync`.** The plan specifies Send-not-Sync; `execute` takes `&mut self`,
so `&Tx` grants nothing and an artificial `!Sync` is pure cost to callers.

**The i32 path has no SIMD, deliberately** — reproducibility becomes structural
rather than something a test has to keep proving. Costs 2.4–3.2x against f32.

**No `bitexact` flag.** The f32 SIMD and scalar paths already agree bit for bit
(one butterfly source, no FMA) and a test asserts exact equality — but that is
deliberately kept out of the *contract*, so the guarantee can be relaxed later
without a semver break.

### On the estimate

Plan 17 costed 27 pw excluding the deferrable C13. Actual: one agent session for
everything except split-radix and cache blocking. **Plan 17 looks substantially
over-costed, and the reason is structural rather than luck.** One `Lane` trait
means the scalar reference, the fixed-point specification and the SIMD kernel are
*the same source* monomorphised three times — which collapses C2, C3, C8 and C10's
kernel work into one body and removes C9's scalar-versus-SIMD divergence risk
entirely, rather than testing for it.

Note the contrast with plan 15, whose 3.4x under-cost of this same crate was real.
Both errors came from estimating work packages independently rather than asking
what a single well-chosen abstraction collapses.

### Fixed-point results
Q31, round-half-up, saturate, divide by radix each stage, so forward produces
`DFT(x)/n` exactly. Measured SNR 152.5 dB at n=64 falling to 138.9 dB at n=2048 —
about 23 effective bits at AAC LC's transform size. 27 golden vectors pin the
codec-relevant combinations.

Rader and Bluestein normalise through a convolution and lose roughly
`log₂(M²/n)` bits, which is why `DIRECT_MAX_FIXED` is 4096: a direct O(n²) DFT
rounds once per output and is the accurate fixed-point choice. No shipping codec
reaches Bluestein in fixed point.

## Amendment — §A.7.1 and §A.4.4 corrected against the binary (2026-08-22)

`vaco-scale` measured three claims in this plan and found all three wrong.

**§A.7.1: the default bicubic is not Catmull–Rom.** The plan says (B, C) =
(0, 0.5). Measured, it is Mitchell–Netravali (0, **0.6**) — recovered by scaling
an impulse 8× into 16-bit and least-squares fitting a cubic to each piece, which
lands on 0.6 within 0.002. Pinned in a test. The two kernels differ by 20% in
the negative lobe, so this is visible ringing, not a rounding detail.

**§A.4.4: chroma siting does not shift phase by default.** The plan implies it
does. The reference applies the plain box mapping; confirmed by impulse
symmetry.

**The canonical sequence is missing a step, and it dominates fidelity.** The
reference does **not** interpolate chroma on the way to R'G'B' — it *replicates*.
§A's sequence has no notion of this. Without it, `yuv420p→rgb24` diverges by
184/255; with it, by 1/255. Any implementation following the plan as written
would have been Divergent on the single most common conversion in the project
and would have had no idea why.

### The colour arithmetic is recoverable, and cheaply

The plans assumed reproducing the reference's fixed-point arithmetic would be
hard. It took about an hour: **13-bit fixed point with `+1<<12` rounding out to
R'G'B', 15-bit with `+(1<<14)+(1<<8)` out to Y'CbCr**, both fitted uniquely and
reproducing every probe sample exactly. Worth remembering the next time a plan
assumes bit-exactness against the reference is out of reach — for the arithmetic
itself, it usually is not. What actually costs the fidelity is a *missing stage*
like chroma replication above, not the rounding.

### Two performance results that contradicted expectation

Recorded because plan 12's PF-0.1 and PF-0.2 keep proving the same point:

- **Clamping intermediates between the horizontal and vertical passes made
  fidelity worse**, not better. Reverted; numbers in the crate docs.
- **Special-casing a one-tap filter bank as a gather was worth 2.2×** on
  `yuv420p→rgb24`. Unexpected, because a one-tap bank looks like
  multiply-by-one — but chroma *replication* makes the degenerate case the
  common one, so the "special case" is the hot path.

### Honest performance position

6–9× off the reference, and structural rather than a missing intrinsic: the
generic path materialises `i32` planes and makes up to four passes where the
reference fuses. 1080p `yuv420p→rgb24` is 6.1 ms against 0.67 ms. The SIMD affine
row measures 2.73× over scalar at 1920 samples and threading scales 3.02× at 8
workers, so the substrate is working — the loss is in the pipeline shape. `fast.rs`
is the seam for closing it, and closing it is a scheduled piece of work, not a
mystery.

## Amendment — §B corrected against the binary (2026-08-22)

`vaco-resample` measured five claims in §B and found all five wrong. Each was
written from the algorithm as it *should* be, not from the binary.

1. **§B.3.2 integer narrowing** says round-half-up. The reference does an
   arithmetic shift: `s32 -32768 → -1`.
2. **§B.3.2 float→int rounding** says half-away-from-zero. Measured, `f32→s16`
   is half-**up** and everything else is ties-to-even.
3. **§B.5.4 zero-priming.** The reference does not zero-prime; it **mirrors** —
   whole-sample at the head, half-sample at the tail. A DC input produces flat
   DC with no fade-in, which zero-priming cannot do.
4. **§B.5.2 per-phase normalisation.** The reference scales the whole bank by
   `1/Σh[0]`. Per-phase normalisation would change every coefficient.
5. **§B.5.2 cutoff.** Derived from Kaiser this comes out 0.63; the measured
   default is **0.97**, applied as `min(1, ratio·cutoff)` — with the `min`
   outside the product, so **upsampling applies no cutoff at all**.

### §B.14.1 was too pessimistic, and this is the useful part

§B.14.1 answered "can we match the reference's resampler bit-exactly?" with
*"No, and we should not try"*, and budgeted 3 person-weeks of class-8
investigation. Measured:

| path | grade |
|---|---|
| integer↔integer, integer→float, packed↔planar | **Exact** |
| rematrix, 23 layout pairs, float output | **Exact**, 23/23 bit-identical f64 |
| rate conversion, upsampling | **Equivalent, 304–307 dB** |
| rate conversion, downsampling | **Equivalent, 101–113 dB** |
| output sample count | **Exact**, `ceil(in·p/q)` |

The filter design was *recoverable exactly* rather than merely fittable, and the
budgeted investigation was not needed. Together with `vaco-scale` recovering the
reference's fixed-point colour arithmetic in about an hour against plans that
assumed it would be hard, the lesson is the same both times:

> **A plan predicting that reproducing the reference is infeasible is a
> hypothesis, and a cheap one to test.** Spend an hour probing before budgeting
> weeks — or before conceding the divergence.

### Two divergences kept, both D17.1-shaped

`f32→s16` ties in a trailing partial block: the reference's rounding is **not a
function of the sample value alone** — half-up in whole 16-sample blocks,
ties-to-even in the trailing `len % 16`. Reproducing it would make output depend
on how the caller chunked its input, which is exactly the kind of contract
D17.1 says not to adopt. Also `22.2→stereo` and `hexadecagonal→stereo`, where no
single rule reproduces the reference's own inconsistent folding.
