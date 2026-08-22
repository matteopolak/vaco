# 10 — Workspace and Crate Architecture

The keystone document. Every subsequent plan conforms to the layering, naming and feature model here.
Constraints come from `planning/00-decisions.md`; research backing is in `planning/research/`.

---

## 1. Design principles

1. **Layers are acyclic and strictly downward.** A crate may depend only on crates in lower layers.
   CI enforces this with `cargo-deny bans` plus a layer-check script; a cycle is a build failure.
2. **Safety is structural, not aspirational.** Every crate carries `#![forbid(unsafe_code)]` at the crate
   root except those on the D2 allowlist. The lint is in the source, not just in CI, so it cannot be
   silently dropped.
3. **Components are data, not code paths.** A decoder, muxer, filter or protocol is a value implementing a
   trait, registered in a table. Adding one means adding a crate and one registry line — never editing a
   `match` in the core.
4. **Traits at boundaries, concrete types inside.** Public seams (demuxer, decoder, filter, DSP kernel set)
   are traits. Hot inner loops are monomorphised concrete code — no `dyn` dispatch inside a per-pixel loop.
5. **The core knows nothing about any specific format.** `vaco-format-core` has no idea MP4 exists.
   Everything specific lives in a leaf crate.
6. **Every crate is independently testable and independently fuzzable.** A crate that cannot be exercised
   without spinning up the whole pipeline is mis-factored.
7. **Prefer many small crates over few large ones** — it is the mechanism for parallel work, for compile
   times, and for feature granularity. Roughly 120–160 crates at full scope.

---

## 2. Repository layout

```
vaco/
├── Cargo.toml               # workspace root, [workspace.lints], shared profiles
├── rust-toolchain.toml      # pinned nightly (portable_simd)
├── Justfile                 # single entry point for every dev command
├── deny.toml                # licence + ban policy
├── about.toml               # cargo-about config for THIRD_PARTY.md
├── rustfmt.toml, clippy.toml
├── crates/
│   ├── core/                # layer 0-1
│   ├── io/                  # layer 2
│   ├── format/              # layer 3  (containers)
│   ├── codec/               # layer 4  (codecs)
│   ├── dsp/                 # layer 3-4 (scale, resample, transforms)
│   ├── filter/              # layer 5
│   ├── hw/                  # optional, the unsafe allowlist lives here
│   ├── registry/            # layer 6
│   ├── cli/                 # layer 7
│   └── tools/               # conformance, bench, fuzz harnesses
├── fuzz/                    # cargo-fuzz targets
├── benches/                 # cross-crate end-to-end benchmarks
├── testdata/                # small committed fixtures (large corpora fetched)
├── docs/                    # user- and developer-facing docs (per CLAUDE.md)
└── planning/                # this directory; research + plans
```

---

## 3. Layers

### Layer 0 — Foundation

| Crate | Contents |
|---|---|
| `vaco-core` | `Error`/`Result` taxonomy, `Rational`, timestamps and time bases, rescaling with explicit rounding modes, `MediaType`, logging façade over `tracing`, small shared newtypes. Depends on nothing but `std`. |
| `vaco-simd` | The portable-SIMD substrate: CPU feature detection, the kernel-selection model (§7), lane-width helpers, and safe wrappers for the widening-multiply-add / saturating-pack / shuffle patterns used everywhere. No codec knowledge. |
| `vaco-opts` | The `AVOption` equivalent: a `Options` derive macro producing typed, introspectable, string-parsable option sets with ranges, defaults, units, named constants, and runtime-settable flags. This is what makes `-h filter=x` and `key=value:key=value` parsing work uniformly. |
| `vaco-expr` | The `eval` expression language (research §01 lists the full grammar): constants, the unary math functions, `st`/`ld` slots, `while`/`taylor`/`root`, `if`/`ifnot`, `between`/`clip`, infix operators. User-facing DSL used by filters, `-force_key_frames`, and timeline `enable=`. Pure, no I/O. |
| `vaco-bitstream` | Bit reader/writer, Exp-Golomb, byte readers, start-code scanning. **Safe:** the "over-read past the end into guaranteed zero padding" trick FFmpeg uses is replaced by an explicit checked-tail / unchecked-body split (§7.4). |

### Layer 1 — Media data model

| Crate | Contents |
|---|---|
| `vaco-pixfmt` | The pixel format enum (~268 variants) and its descriptor metadata — plane count, component layout, bit depth, subsampling, endianness, flags. **Generated** from a declarative table, not hand-written, so metadata cannot drift. |
| `vaco-sampfmt` | Sample formats, planar/interleaved, byte widths. |
| `vaco-chlayout` | The channel-layout model: `Unspecified` / `Native(mask)` / `Custom(Vec<Channel>)` / `Ambisonic`, channel identifiers, the named standard layouts, parse and display. |
| `vaco-color` | Colour primaries, transfer characteristics, matrix coefficients, range, chroma location, alpha mode (all per ITU-T H.273), plus primaries/whitepoint chromaticity data and matrix derivation. |
| `vaco-frame` | `Frame` (video and audio), plane storage, strides, `SideData` (the full enumerated set), metadata, cropping. Buffers are `Arc`-shared, copy-on-write via `Arc::make_mut` — the refcount model without a hand-rolled refcount. |
| `vaco-packet` | `Packet`, packet side data, timestamps, flags. |
| `vaco-pool` | Buffer pooling with alignment guarantees (§7.4). Safe: a pool of `Vec<u8>` with an alignment-carrying wrapper. |

### Layer 2 — I/O

| Crate | Contents |
|---|---|
| `vaco-io` | `Reader`/`Writer`/`Seek` abstractions, buffering, seekability probing, byte-range and dynamic buffers, custom callback I/O. The `AVIOContext` role. |
| `vaco-protocol-core` | The `Protocol` trait and URL parsing/dispatch. |
| `vaco-protocol-file`, `-pipe`, `-cache`, `-concat`, `-subfile`, `-data`, `-crypto`, `-md5` | Local protocols. |
| `vaco-protocol-http`, `-tcp`, `-udp`, `-tls`, `-rtmp`, `-rtp`, `-srt?`, `-rist?` | Network protocols, each an optional feature. TLS via `rustls`. SRT is blocked on the MPL exclusion (research §09) — either native or absent. |

> **Amended by D14.1.** `vaco-codec-core` sits **below** `vaco-format-core`, not beside it — demuxers
> need codec parameters and bitstream parsers. Demuxers reach parsers through an injected
> `ParserProvider` trait, so **no format crate ever depends on a codec crate**. Read the layer numbers
> below with that correction applied.

### Layer 3 — Containers and signal processing

| Crate | Contents |
|---|---|
| `vaco-format-core` | `Demuxer`/`Muxer` traits, `Stream`, `Program`, `Chapter`, `StreamGroup`, probing and scoring, the timestamp model (generation, discontinuity handling, wrapping), the seek model (all methods and flags), muxer interleaving, and the generic format-level option set. |
| `vaco-format-riff`, `-isom`, `-mpegts-tables`, `-id3`, `-metadata-conv` | Shared tables and helpers many formats need. Split out precisely because several formats depend on them. |
| `vaco-demux-*` / `vaco-mux-*` | One crate per container family: `mp4`, `matroska`, `mpegts`, `mpegps`, `avi`, `asf`, `flv`, `ogg`, `wav`, `aiff`, `caf`, `mxf`, `hls`, `dash`, `rtp`, `image2`, `raw`, `subtitle`, `concat`, `segment`, `tee`… plus `vaco-demux-legacy-*` grouping the long tail of game/FMV containers. |
| `vaco-scale` | The swscale equivalent, built on the **ops-graph model**, not per-format-pair kernels (research §01 argues this explicitly): read/write, swizzle, pack/unpack, shift, convert, clamp, scale, linear transform, dither, horizontal/vertical filter, 3D LUT — composed into a graph, optimised, then executed by SIMD kernels. |
| `vaco-resample` | The swresample equivalent, cleanly split into three independent stages: sample-format conversion, channel rematrixing, and rate conversion. |
| `vaco-tx` | FFT / MDCT / RDFT / DCT-I–IV / DST-I in f32, f64 and i32, with the flag set (in-place, unaligned, full IMDCT, real-to-real, real-to-imaginary). Shared by nearly every audio codec. |

### Layer 4 — Codecs

| Crate | Contents |
|---|---|
| `vaco-codec-core` | `Decoder`/`Encoder` traits, the send/receive model, draining and flushing semantics, capability flags, `CodecParameters`, profiles and levels, the threading contract, and the parser and bitstream-filter traits. |
| `vaco-codec-dsp-*` | Shared DSP families identified in research §02's dependency map: `idct`, `mc` (motion compensation), `deblock`, `intrapred`, `mecmp`, `lpc`, `sinewin`, `fmtconvert`. Each is independently benchmarkable and checkasm-testable. |
| `vaco-codec-cabac`, `-golomb`, `-cbs` | Entropy-coding and coded-bitstream-syntax layers shared across H.264/HEVC/VVC/AV1/VP9. |
| `vaco-codec-mpegvideo` | The shared MPEG-family core (H.261/H.263/MPEG-1/2/4, MSMPEG4, WMV1/2, FLV1, RV10/20) — one crate, because these genuinely share a decoder core. |
| `vaco-codec-<name>` | One crate per codec or tight codec family: `h264`, `hevc`, `vvc`, `av1`, `vp8`, `vp9`, `aac`, `ac3`, `mp3`, `opus`, `vorbis`, `flac`, `alac`, `pcm`, `adpcm`, `prores`, `dnxhd`, `ffv1`, `png`, `jpeg`, … Decoder and encoder live together where they share tables; split where they do not. |
| `vaco-codec-legacy-*` | The long tail (game/FMV/legacy formats) grouped into a handful of crates rather than a hundred. |
| `vaco-bsf-*` | Bitstream filters, grouped by family. |

### Layer 5 — Filters

| Crate | Contents |
|---|---|
| `vaco-filter-core` | `Filter` trait, pad model, link model, format negotiation, the **activate-style cooperative scheduler** with per-link frame queues, EOF and timestamp propagation, timeline `enable=` support, slice threading, and the runtime command interface. |
| `vaco-filter-graph` | Filtergraph parsing (the full textual DSL including all three escaping levels), link resolution, auto-inserted conversion filters, and graph execution. |
| `vaco-filter-framesync` | Multi-input timestamp alignment — used by ~68 filters, so it is its own crate. |
| `vaco-filter-draw`, `-text` | Drawing primitives and the text stack (cosmic-text based, per research §09). |
| `vaco-filter-<group>` | Filters grouped by category: `scale`, `crop`, `color`, `blur`, `denoise`, `deinterlace`, `overlay`, `analysis`, `source`, `audio-eq`, `audio-dynamics`, `audio-mix`, … |

### Layer 6 — Registry

`vaco-registry` assembles the enabled components into lookup tables. **No linker tricks.** Registration is
explicit, `#[cfg(feature = "...")]`-gated code in one generated module — a build script emits it from a
manifest so adding a component is a one-line manifest change, and the generated file is committed and
reviewable. This keeps `forbid(unsafe_code)` intact (crates like `inventory`/`linkme` rely on unsafe and
link-section tricks, so they are out).

### Layer 7 — Applications

| Crate | Contents |
|---|---|
| `vaco-cli-core` | Shared CLI machinery: the option-parsing engine, stream-specifier grammar, help system, the `-formats`/`-codecs`/`-filters`/… listing commands, log-level handling, and the text-writer framework backing every output format. |
| `vaco-textformat` | The writer framework: default, compact, csv, flat, ini, json, xml — with the section-schema model. Shared by `vaco-probe` output and `vaco`'s graph dumps. |
| `vaco-sched` | The pipeline scheduler: demux → decode → filter → encode → mux as a DAG of components connected by bounded channels, each on its own task. This is the hardest single component (research §05 names it as such). |
| `vaco` | The ffmpeg-equivalent binary. |
| `vaco-probe` | The ffprobe-equivalent binary. |
| `vaco-play` | The ffplay-equivalent binary: winit + wgpu + cpal, no SDL. |

### Hardware acceleration — in the default build (see D13)

`vaco-hw-core` plus `vaco-hw-vulkan-video` (via `ash`), `vaco-hw-videotoolbox` (via `objc2-*`), and
optionally `vaco-hw-d3d12` (via `windows`). These are **enabled by default on the platforms that
support them**, not opt-in: D13 corrects the earlier assumption that containing `unsafe` was a reason
to exclude them.

They are strategically central rather than peripheral. Hardware delegation is how H.264 and HEVC reach
users at all, given we ship no software codec for either — so `vaco-hw-vulkan-video` is a Wave 2/3
item, not a late addition.

Two distinct problems, not to be conflated:
- **GPU compute** (filters, scaling, colour, tone mapping) → `wgpu`, safe, portable.
  `vaco-filter-gpu` keeps `#![forbid(unsafe_code)]`.
- **Fixed-function video decode/encode** → vendor and OS APIs only. `wgpu` does not expose these
  (gfx-rs/wgpu#2330 is still an open discussion), so these crates carry `unsafe`, kept tiny and
  mechanical, every block documented with a `SAFETY:` invariant, and verified primarily by
  differential testing against our own software decoder plus sanitizers in a nightly CI job.

### Tools

`vaco-conformance` (the differential harness), `vaco-checkasm` (kernel verification and cycle benchmarking —
our clean-room equivalent of checkasm, whose FFmpeg implementation is GPL and therefore unusable),
`vaco-corpus` (fixture fetching and minimisation).

---

## 4. Feature model

FFmpeg's configure expresses a dependency DAG with **strong** (`select`), **weak** (`suggest`) and
**conflict** edges (research §06 §2.1). Cargo features express only strong, additive edges. The mapping:

- **strong edge** → `feature = ["dep:vaco-codec-h264"]`. Direct.
- **weak edge** → not expressible; becomes an explicit opt-in feature. We document it rather than fake it.
- **conflict edge** → a `compile_error!` guard in the crate that owns the conflict.

Feature tiers on the `vaco` binary crate:

| Tier | Meaning |
|---|---|
| `default` | The distributable set: royalty-free codecs, all containers/protocols, all permissively-implementable filters. |
| `full-rf` | Everything royalty-free, including rarely-used long-tail formats. |
| `codec-<name>` | One codec. Every codec has one. |
| `format-<name>`, `filter-<group>`, `protocol-<name>` | Same granularity elsewhere. |
| `patent-encumbered-<name>` | Named exactly so, per D4. Never in `default`, never in a published binary. CI asserts absence. |
| `hw-<backend>` | Hardware acceleration; enables `unsafe` in the corresponding `vaco-hw-*` crate only. |
| `ffi-<lib>` | Optional bindings to an external C library. Never default. |

CI builds and tests: `--no-default-features`, `default`, `full-rf`, and a matrix of single-feature builds to
catch feature-gating mistakes (a crate that only compiles when some unrelated feature happens to be on).

---

## 5. Component traits (shape, not final signatures)

```rust
// vaco-format-core
pub trait Demuxer {
    fn probe(input: &ProbeData) -> ProbeScore where Self: Sized;
    fn open(io: Box<dyn ReadSeek>, opts: &Options) -> Result<Self> where Self: Sized;
    fn streams(&self) -> &[Stream];
    fn read_packet(&mut self) -> Result<Option<Packet>>;
    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()>;
}

// vaco-codec-core — the send/receive model, which handles N:M frame/packet
// relationships that a simple decode(packet) -> frame signature cannot.
pub trait Decoder {
    fn send_packet(&mut self, pkt: Option<&Packet>) -> Result<()>;   // None = drain
    fn receive_frame(&mut self) -> Result<Option<Frame>>;
    fn flush(&mut self);
}

// vaco-filter-core — cooperative scheduling: do one bounded step, report status.
pub trait Filter {
    fn activate(&mut self, ctx: &mut FilterCtx) -> Result<Activity>;
}
pub enum Activity { Progressed, NeedInput, Blocked, Eof }
```

Each trait has a companion descriptor (`DemuxerDesc`, `DecoderDesc`, …) carrying name, long name, media
type, capabilities and the option schema. The registry stores descriptors; instances are constructed on
demand. This is what lets `-h decoder=h264` work without instantiating anything.

---

## 6. Threading

Three independent axes, matching the problem rather than FFmpeg's implementation:

1. **Pipeline parallelism** (`vaco-sched`): every component runs as its own task connected by bounded
   channels. Backpressure is the channel. This is the main source of parallelism for real workloads and it
   is where `-shortest`, sync queues and flush ordering are handled.
2. **Frame parallelism** (decoders): pipelined frame decoding with explicit cross-frame progress
   signalling for inter-frame dependencies. Expressed as a safe progress primitive (an `AtomicU32` row
   counter plus a condvar/notify), not shared mutable frame state.
3. **Data parallelism** (filters, slices, scaling): `rayon`-style scoped parallel iteration over slices or
   planes. Safe by construction via disjoint mutable slice splitting (`split_at_mut`, `chunks_mut`).

We do **not** inherit FFmpeg's 16-thread auto ceiling as a constant; we measure and set it from data.

---

## 7. Performance architecture

This section governs; the detailed plan elaborates it.

### 7.1 No unsafe, no intrinsics, no asm

FFmpeg's speed comes from ~190k lines of hand-written assembly (research §08). We are forbidden that road,
so we must win differently:

- **`std::simd` (portable SIMD)** is safe and covers the dominant kernel shapes: widening multiply-add,
  saturating pack, lane select/blend, shuffle within known patterns, horizontal reduction. Research §08
  identifies these as sufficient for colour conversion, resampling, separable filters, transform arithmetic
  and deinterlacing — which together are a large share of real CPU time.
- **Autovectorization** handles the rest when the code is written for it: no aliasing ambiguity (Rust gives
  us this for free), fixed trip counts, slice-based access with bounds checks hoisted by iterator use, and
  `chunks_exact` to give LLVM a provably-aligned, provably-sized inner loop.
- **PGO and BOLT.** FFmpeg uses neither and only opt-in LTO. This is genuine headroom, and it is
  disproportionately effective on exactly the code SIMD cannot reach: the branchy scalar entropy decoders.

### 7.2 Where the effort goes

Ranked from research §08's assembly-volume analysis, adjusted for what portable SIMD can reach:

| Priority | Area | Why |
|---|---|---|
| 1 | Colour conversion / pixel format conversion | Highest reward per unit effort; regular, gather-free, saturating-pack-heavy; near-ideal for `std::simd`. |
| 2 | Scaling (horizontal/vertical filters) | Very common; use precomputed coefficient tables to avoid hardware gather, as FFmpeg does. |
| 3 | Audio resampling and rematrixing | Simple FIR convolution and small matrix multiply; low effort, high value. |
| 4 | H.264/HEVC motion compensation | The single largest asm area upstream; separable FIR filters, tractable in portable SIMD. |
| 5 | Transforms (IDCT, MDCT/FFT) | Butterfly arithmetic vectorizes well; the permute/transpose stages are where we will lose ground and must measure. |
| 6 | Deinterlace / blur / denoise filters | Upstream's filter SIMD largely predates AVX-512; we may **exceed** it by targeting wider vectors. |
| 7 | Loop filters / deblocking | Branchy per-edge decisions; needs masked-lane select. Hardest portable-SIMD target. |
| 8 | CABAC / entropy decode | **Not vectorizable at all** — upstream has no CABAC asm either. Wins come from scalar code quality, table layout, branch prediction, and PGO. This is where we can genuinely beat C. |

### 7.3 Kernel dispatch

FFmpeg uses a struct of function pointers, populated once by an arch-specific init that cascades through
ISA tiers. Our equivalent, staying safe:

- Kernels are generic over a SIMD lane width and instantiated for the widths we care about.
- Selection happens **once**, at component construction, into a `KernelSet` struct of `fn` pointers
  (ordinary safe `fn`, not `unsafe extern`). One indirect call per kernel invocation, amortised over a whole
  frame or slice — the same cost model as upstream.
- A scalar reference implementation always exists for every kernel and is what `vaco-checkasm` differentially
  tests every SIMD variant against, over randomised and edge-case inputs. A kernel without a scalar
  reference and a differential test is not merged.

### 7.4 Memory

- **Alignment:** buffers are allocated to 64-byte alignment unconditionally. FFmpeg picks alignment from
  build-time SIMD width; we take the maximum and stop thinking about it.
- **Padding:** FFmpeg's guaranteed-zero over-read padding exists to let SIMD bitstream readers skip bounds
  checks. We cannot rely on over-reading. Instead every bitstream reader splits into an unchecked body (where
  a single up-front length check proves the remaining reads in range) and a checked tail. This is the
  standard safe-Rust idiom and costs one comparison per block rather than one per read.
- **Picture edges:** we keep the "pad the common case, emulate the rare case out-of-line" split — motion
  compensation kernels assume padded, edge-free input; out-of-frame vectors go to a separate cold path.
- **Pooling:** `vaco-pool` recycles frame buffers so steady-state decode does not allocate.
- **Zero copy:** frames are `Arc`-shared; a filter that does not modify a plane passes the `Arc` through.

---

## 8. Toolchain and developer experience

- **`rust-toolchain.toml`** pins a specific nightly. `portable_simd` is the reason; the pin makes it
  reproducible. Re-pin deliberately on a schedule, never drift.
- **Cranelift for dev builds.** `[profile.dev] codegen-backend = "cranelift"` — much faster compiles.
  Release stays on LLVM.
- **Release profile:** `lto = "fat"`, `codegen-units = 1`, `panic = "abort"` for binaries, then PGO, then
  evaluate BOLT per target.
- **`Justfile` is the only interface a developer needs.** Targets: `build`, `test`, `lint`, `fmt`, `bench`,
  `bench-compare`, `fuzz <target>`, `fuzz-all`, `conformance`, `conformance-update`, `corpus-fetch`,
  `licence-check`, `licence-report`, `layer-check`, `pgo-build`, `coverage`, `docs`, `ci` (everything CI runs).

---

## 9. Documentation

Per the repository standard, `docs/` carries a doc per module or feature, each covering: what it is, how it
works, how to change it, configuration, and dependencies. `docs/README.md` is the index. A new crate lands
with its doc in the same change — CI checks that every `crates/*/` directory has a corresponding `docs/`
entry.

---

## 10. What this architecture deliberately rejects

| Rejected | Why |
|---|---|
| C ABI compatibility | D1. It would force C struct layouts and ownership semantics through the whole design. |
| `inventory`/`linkme` auto-registration | Requires unsafe and link-section tricks. Explicit generated registration is safe and reviewable. |
| A single monolithic `vaco-codec` crate | Kills parallel work, compile times and feature granularity. |
| Per-format-pair scaling kernels | Upstream is migrating away from this to an ops graph; we start where they are going. |
| `dyn Trait` in inner loops | Vtable indirection per pixel. Traits at seams, monomorphisation inside. |
| Inheriting FFmpeg's option names blindly | We match CLI names (interface compatibility) but not internal field names. |
| Any MPL-2.0 dependency | D3. Costs us Symphonia and mp4parse; we write our own. |
| **Any external crate supplying a codec, container, resampler, scaler or transform** | **D10.** We own the media stack end to end. A tool that delegates its codecs to other people's implementations is a wrapper, not a replacement — and the safety and licence guarantees would only hold as far as the FFI boundary. |


---

## 11. Amendment — D10 dependency policy

Decision D10 was revised after this document was written. The layering is unchanged; what fills it is
governed by three gates: **pure Rust with zero FFI**, **permissive licence per D3**, and **trusted and
maintained**. Prefer a crate that clears all three over writing our own; write our own where no crate
clears them, where the crate's model does not fit ours, or where we need a capability it lacks.

Architectural consequences:

1. **The `ffi-<lib>` feature tier defined in §4 is deleted.** There are no external C bindings in any
   configuration. `hw-<backend>` survives — hardware acceleration is platform silicon reached through
   an OS API, not a third-party codec, and the legal register raises its strategic priority since
   delegating H.264/HEVC to already-licensed hardware means our binary ships no software codec for
   either. Those crates remain the only place `unsafe` is permitted.
2. **`vaco-tx` is very likely ours regardless of the policy**, because several audio codecs need
   bit-exact i32 fixed-point transforms for conformance and `rustfft`/`realfft` do not provide them.
   It is on the critical path either way.
3. **Image codecs may be dependencies** if the candidate crates clear the gates — `png`, `zune-jpeg`,
   `gif`, `tiff`, `image-webp`, `jxl-oxide` are all plausible. Assess each rather than assuming.
4. **A codec need not be all-or-nothing.** Depending on a crate for the part it does well while
   implementing the rest ourselves is a legitimate outcome, particularly where a crate decodes but
   does not encode.
5. **The unsafe guarantee is about our code, not the process.** `forbid(unsafe_code)` is per-crate and
   cannot reach dependencies; several strong pure-Rust media crates use unsafe internally for SIMD.
   Measure with `cargo-geiger`, prefer the lower-unsafe option between comparables, and treat heavy
   unsafe on a hot path as a trade-off to be argued in the adoption record. Where the guarantee needs
   to hold end to end, that is an argument for writing that component ourselves.
6. **Every adoption is a reviewed decision** recorded in `docs/dependencies.md`, not a `cargo add`.


---

## 12. Amendment — D11, the adapter boundary

Every external codec/container/signal-processing crate is reachable from **exactly one** Vaco crate,
which exposes only our traits over our types. No foreign type crosses that boundary — not in a
signature, an error variant, a re-export, or a trait bound.

Consequences for the layering:

1. **Naming is uniform.** `vaco-codec-flac` is `vaco-codec-flac` whether it wraps `claxon` or
   implements FLAC natively. Callers cannot tell, and the distinction never appears in the crate
   graph above that crate.
2. **A new CI check joins the layer-acyclicity check**: every third-party media crate must appear in
   exactly one `Cargo.toml` under `crates/`. A second occurrence fails the build. This is what makes
   the boundary real rather than a convention people drift away from.
3. **Backend features** (`backend-external` / `backend-native`) let both implementations coexist
   behind the same crate and the same tests, making replacement incremental and reversible.
4. **The differential harness gains a three-way mode**: our implementation, the wrapped
   implementation, and the reference binary. When two agree and one differs, that localises the bug
   immediately — which is worth considerably more than a two-way comparison.
5. **`docs/codec-status.md` carries a fidelity grade per codec** (Exact / Equivalent / Divergent /
   Unmeasured). Unmeasured and Divergent codecs cannot ship in the default build. Adoption is
   evidence-based, not reputational.

This is why the trait definitions in §5 matter more than usual: they are the seam along which the
whole dependency question stays reversible.
