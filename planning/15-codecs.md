# 15 — Codec Plan

> **CORRECTED BY D15.** This document classifies a large part of the inventory as impossible to
> clean-room. **That was a legal error.** 17 U.S.C. §102(b) excludes procedures, processes and methods
> from copyright "regardless of the form in which it is described", and format-dictated tables are the
> paradigm case for the merger doctrine — a Huffman table a decoder *must* contain is a fact about the
> format, not authorial expression. Reverse-engineered formats are therefore **legally implementable**
> from a functional specification.
>
> What actually constrains them is **cost and demand**: a specification-extraction pass (0.5–3 pw per
> format) must precede implementation, and most of these formats have very few users. Re-read every
> "cannot be done cleanly" below as **"requires spec extraction first, prioritise on demand"**.
>
> The genuinely blocked set is small: trained model weights (`nnedi`) and hand-tuned perceptual tables
> that the format does not dictate. See D15 for the full analysis and the process.


How Vaco implements codecs: the core traits, the shared DSP decomposition, the tiering of the
whole inventory, per-codec implementation plans, the H.264/HEVC problem, parsers and bitstream
filters, a parallelisable work breakdown, and hardware acceleration.

**Binding inputs.** `planning/00-decisions.md` (D2 no-unsafe, D3 licensing, D4 patent posture,
D5 v0.1 scope, D6 differential testing, D7 clean-room, **D9 legal amendments, D10 dependency
policy, D11 external codecs behind our API**), `planning/10-architecture.md` (layers, crate naming,
threading axes, kernel dispatch), `research/02-libavcodec.md` (inventory and the
shared-infrastructure dependency map), `research/07-legal-patents-licensing.md` (per-codec patent
verdicts, §1.7 clean-room tiers), `research/09-dependency-licence-register.md` (licence findings
still authoritative; its OPT-IN verdicts are superseded by D10 Gate 1).

**Clean-room reminder, restated because this document is what implementers will read.**
Nobody working on a codec opens FFmpeg's source. Every codec below cites the public specification
an implementer works from. Where the research marks a format "RE — no public spec", that format
**cannot be implemented spec-first at all**; §3.5 says what we do instead. Every PR carries a
`Vaco-Provenance:` trailer naming the spec document and section.

---

## 0. Summary of the position

| Question | Answer |
|---|---|
| How much codec work is there? | ~330–360 person-weeks for the entire Tier-1 set plus shared infrastructure. ~1,100 pw for everything we would ever plausibly implement. |
| What is the biggest single item? | AV1 decode, ~70 pw. Then VVC decode (~110 pw, T3, never shipped) and H.264 decode (~60 pw, T3). |
| What ships by default? | ~97 decoder entries and ~77 encoder entries from ~24 codec crates. Zero patent-encumbered software codecs. |
| What is the hardest engineering constraint? | Frame-level threading without `unsafe`. §1.7 solves it with guard-padded band publication; it costs a `PlaneView` indirection in every motion-compensation kernel. This is the single most consequential design decision in this document. |
| What is the hardest legal constraint? | H.264/HEVC/AAC. All three are unavoidable in real files and none of them may appear as a software codec in a binary we publish. §5 lays out the options; the answer is hardware delegation plus in-tree-but-never-shipped software behind `patent-encumbered-*`. |
| What cannot be done at all? | ~300 decoders (half the upstream inventory) exist only because FFmpeg's source is their specification. §3.5. |
| Do we write every codec ourselves? | No. D10 admits pure-Rust, permissively-licensed, maintained crates; D11 puts every one of them behind a `vaco-codec-*` crate exposing only our traits over our types. §4A does the per-codec assessment. **The crates cover the image periphery and a few audio decoders. Every video codec of consequence, every encoder of consequence, and all of the shared DSP is still ours to write.** |
| How much does buying save? | ~46 pw of first-pass implementation, at the cost of ~12 pw of wrapping and measurement, and with a standing obligation to replace anything the differential harness grades **Divergent**. Net saving in the first year: ~34 pw of ~383 pw — about 9%. It buys schedule, not scope. |

---

# 1. `vaco-codec-core`

Layer 4, `#![forbid(unsafe_code)]`. Depends only on layers 0–1 (`vaco-core`, `vaco-frame`,
`vaco-packet`, `vaco-pixfmt`, `vaco-sampfmt`, `vaco-chlayout`, `vaco-color`, `vaco-opts`,
`vaco-pool`). It knows about no specific codec — it defines the seams and nothing else.

## 1.1 Identity, properties, parameters

`CodecId` is **generated** from a declarative `codecs.toml`, the same technique `vaco-pixfmt` uses,
so name/long-name/media-type/property metadata cannot drift from the enum.

```rust
/// One variant per codec *format*. Not per implementation: `av1`, `libdav1d` and
/// `av1_videotoolbox` are three `DecoderDesc`s sharing one `CodecId::Av1`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum CodecId { H264, Hevc, Vvc, Av1, Vp8, Vp9, Opus, Vorbis, Flac, Alac, /* … */ }

impl CodecId {
    pub const fn name(self) -> &'static str;            // "av1"  — CLI-stable
    pub const fn long_name(self) -> &'static str;       // "Alliance for Open Media AV1"
    pub const fn media_type(self) -> MediaType;
    pub const fn properties(self) -> CodecProperties;   // INTRA_ONLY|LOSSY|LOSSLESS|REORDER|FIELDS|…
    pub const fn profiles(self) -> &'static [ProfileDesc];
    pub fn from_name(s: &str) -> Option<Self>;
}

/// FourCC / container-specific tag. Kept separate from `CodecId` because the mapping is
/// many-to-one and container-specific; the tables live in `vaco-format-riff` / `-isom`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct CodecTag(pub [u8; 4]);
```

`CodecParameters` is the codec-agnostic stream description that travels in containers, is emitted
by `vaco-probe`, and initialises a decoder. It is a plain value: `Clone + PartialEq + Debug`, no
interior mutability, no allocation beyond `Arc<[u8]>` for extradata.

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct CodecParameters {
    pub codec: CodecId,
    pub tag: Option<CodecTag>,
    pub extra: ExtraData,
    pub bit_rate: Option<u64>,
    pub bits_per_coded_sample: Option<u8>,
    pub profile: Option<Profile>,
    pub level: Option<Level>,
    pub kind: ParametersKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParametersKind {
    Video(VideoParameters),
    Audio(AudioParameters),
    Subtitle(SubtitleParameters),
    Data,
}

/// Extradata carries its *flavour*, because "the same bytes" mean different things per
/// container. A decoder that receives `Avcc` knows it must not expect start codes; a BSF
/// that produces `AnnexB` records the change. FFmpeg leaves this implicit and it is a
/// perennial source of bugs.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtraData { pub bytes: Arc<[u8]>, pub flavour: ExtraFlavour }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExtraFlavour {
    None, Raw, Avcc, Hvcc, Vvcc, Av1C, VpCodecConfig, EsdsAsc, OpusHead, VorbisComment, FlacStreamInfo,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct VideoParameters {
    pub width: u32,
    pub height: u32,
    pub coded_width: u32,
    pub coded_height: u32,
    pub sample_aspect_ratio: Rational,
    pub pix_fmt: Option<PixelFormat>,
    pub field_order: FieldOrder,
    pub color: ColorInfo,          // vaco-color: primaries/trc/matrix/range/chroma_loc/alpha
    pub frame_rate: Option<Rational>,
    pub delay: u8,                 // max reorder depth (`has_b_frames` equivalent)
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct AudioParameters {
    pub sample_rate: u32,
    pub sample_fmt: Option<SampleFormat>,
    pub ch_layout: ChannelLayout,
    pub frame_size: Option<u32>,
    pub block_align: Option<u32>,
    pub initial_padding: u32,      // Opus pre-skip, AAC encoder delay
    pub trailing_padding: u32,
    pub seek_preroll: u32,
}
```

## 1.2 Profiles and levels

Profiles are a codec-scoped integer plus a name table; levels are a codec-scoped integer plus a
*constraint* table, because levels are what `-level` validation, DPB sizing and hardware capability
matching all need.

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Profile { pub codec: CodecId, pub raw: i32 }

impl Profile {
    pub fn name(self) -> Option<&'static str>;
    pub fn from_name(codec: CodecId, s: &str) -> Option<Self>;
    /// e.g. AV1 Professional ⊇ High ⊇ Main. Used for decoder capability matching.
    pub fn subsumes(self, other: Profile) -> bool;
}

/// Raw, codec-specific encoding: H.264 level ×10, HEVC general_level_idc ×30,
/// AV1 seq_level_idx, VP9 level ×10. Never normalise — round-tripping matters.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Level { pub codec: CodecId, pub raw: i32 }

#[derive(Clone, Copy, Debug)]
pub struct LevelConstraints {
    pub max_luma_picture_size: u64,
    pub max_luma_sample_rate: u64,
    pub max_bitrate_kbps: u32,
    pub max_dpb_frames: u16,
    pub max_h_size: u32,
    pub max_v_size: u32,
    pub max_tiles: u16,
    pub max_tile_cols: u16,
}

impl Level {
    pub fn constraints(self, profile: Profile) -> Option<&'static LevelConstraints>;
    pub fn name(self) -> Option<&'static str>;              // "5.1", "6.0"
    /// The smallest level satisfying a given coded configuration — what an encoder uses
    /// when the user says `-level auto`.
    pub fn smallest_for(codec: CodecId, profile: Profile, cfg: &LevelQuery) -> Option<Level>;
}
```

The tables are per-codec (`vaco-codec-av1` supplies AV1's, from AV1 spec Annex A) and registered
through the descriptor, not centralised — otherwise `vaco-codec-core` would have to know every
codec, violating architecture principle 5.

## 1.3 Capability flags

Deliberately smaller than FFmpeg's 23. We drop the ones that describe FFmpeg's internal plumbing
(`DR1`, `DRAW_HORIZ_BAND`, `ENCODER_RECONF`, `OTHER_THREADS`) and add one FFmpeg does not have.

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Caps(u32);

impl Caps {
    pub const DELAY: Caps                = Caps(1 << 0);  // must be drained at EOF
    pub const SUBFRAMES: Caps            = Caps(1 << 1);  // one packet may yield >1 frame
    pub const SMALL_LAST_FRAME: Caps     = Caps(1 << 2);
    pub const FRAME_THREADS: Caps        = Caps(1 << 3);
    pub const SLICE_THREADS: Caps        = Caps(1 << 4);
    pub const PARAM_CHANGE: Caps         = Caps(1 << 5);  // tolerates mid-stream reconfiguration
    pub const CHANNEL_CONF: Caps         = Caps(1 << 6);  // derives layout itself, distrust container
    pub const VARIABLE_FRAME_SIZE: Caps  = Caps(1 << 7);
    pub const AVOID_PROBING: Caps        = Caps(1 << 8);
    pub const HARDWARE: Caps             = Caps(1 << 9);
    pub const HYBRID: Caps               = Caps(1 << 10); // hw with internal sw fallback
    pub const EXPERIMENTAL: Caps         = Caps(1 << 11); // needs -strict experimental
    pub const ENCODER_FLUSH: Caps        = Caps(1 << 12);
    pub const ENCODER_RECON_FRAME: Caps  = Caps(1 << 13);
    pub const ENCODER_COPY_OPAQUE: Caps  = Caps(1 << 14);
    /// **Ours, not FFmpeg's.** Set on every implementation the legal register marks AMBER
    /// or RED. CI asserts that no descriptor with this bit is reachable in the default
    /// build (D4: "assert on the compiled feature list, not on intent" — this is the
    /// runtime half of that assertion).
    pub const PATENT_ENCUMBERED: Caps    = Caps(1 << 15);
}
```

## 1.4 Descriptors and the registry seam

```rust
pub struct DecoderDesc {
    pub name: &'static str,               // "av1", "av1_videotoolbox"
    pub long_name: &'static str,
    pub codec: CodecId,
    pub caps: Caps,
    pub options: &'static OptionSchema,   // vaco-opts — powers `-h decoder=av1`
    pub supported: &'static Supported,    // pix/sample formats, rates, layouts, profiles
    pub hw_configs: &'static [HwConfig],  // §8
    pub open: fn(&CodecParameters, &Options, &DecoderSetup) -> Result<Box<dyn Decoder>>,
}
```

The descriptor is a `const` value; opening is a function pointer. `-h decoder=av1` and
`vaco -decoders` never construct anything. Registration is the explicit generated module described
in architecture §Layer 6.

## 1.5 The send/receive model

One state machine, used identically by decoders, encoders and bitstream filters. Learning it once
covers all three.

```rust
pub trait Decoder: Send {
    /// Feed a packet. `Err(SendError::Again)` means the internal output queue is full —
    /// the caller must `receive_frame` before retrying with the *same* packet.
    fn send_packet(&mut self, pkt: &Packet) -> Result<(), SendError>;

    /// Begin draining. After this, `send_packet` returns `SendError::Eof`.
    fn send_eof(&mut self) -> Result<(), SendError>;

    fn receive_frame(&mut self) -> Result<Frame, ReceiveError>;

    /// Discard all buffered state and return to `Feeding`. Used on seek.
    fn flush(&mut self);

    /// Parameters as the decoder now understands them; may change after the first frame
    /// or after a `PARAM_CHANGE` event.
    fn parameters(&self) -> &CodecParameters;

    fn threading(&self) -> Threading { Threading::None }
}

#[derive(Debug)]
pub enum SendError { Again, Eof, Invalid(Error) }

#[derive(Debug)]
pub enum ReceiveError { Again, Eof, Decode(Error) }
```

```text
                 send_packet ─┐        ┌─ receive_frame → Frame
                              ▼        │
   Open ──open()──►  Feeding ─────────┴──►  Feeding
                       │  send_eof
                       ▼
                    Draining ──receive_frame*──► Drained ──receive_frame──► Err(Eof)
                       │                            │
                       └──────── flush() ───────────┴──► Feeding
```

Rules, normative:

1. `Again` from `send_packet` is **backpressure**, and it is what `vaco-sched` uses to size its
   bounded channels. It never means "error".
2. `Again` from `receive_frame` in `Feeding` means "send more". In `Draining` it never occurs.
3. A decoder without `Caps::DELAY` is guaranteed never to buffer: exactly one `receive_frame`
   succeeds per `send_packet` that produced output.
4. `receive_frame` returning `Decode(e)` is **not** terminal unless `e.is_fatal()`. Concealment
   policy (`-err_detect`) decides whether a corrupt frame is emitted with
   `FrameFlags::CORRUPT` or suppressed.
5. `flush()` is infallible and total. It must leave the decoder in exactly the state a fresh
   `open()` would, minus reparsing extradata. Fuzz target: `flush()` at every point in a stream.
6. **Determinism is a contract.** Output must be bit-identical for any legal thread count.
   CI runs every conformance suite at `threads ∈ {1, 2, 3, 8, 17}` and diffs.

The encoder mirror:

```rust
pub trait Encoder: Send {
    fn send_frame(&mut self, frame: &Frame) -> Result<(), SendError>;
    fn send_eof(&mut self) -> Result<(), SendError>;
    fn receive_packet(&mut self) -> Result<Packet, ReceiveError>;
    /// Only when `Caps::ENCODER_RECON_FRAME` and the caller asked for reconstruction.
    fn receive_recon_frame(&mut self) -> Result<Frame, ReceiveError> { Err(ReceiveError::Eof) }
    fn flush(&mut self);
    fn parameters(&self) -> &CodecParameters;
}
```

## 1.6 The parser trait

A parser turns a byte stream into complete access units and fills in what the container did not
say. It **never decodes**. This distinction is load-bearing legally: parsing an H.264 SPS or an
AAC `AudioSpecificConfig` implements no decoder, so the header-parsing crates ship in the default
build while the decoders do not (§5, §6).

```rust
pub trait Parser: Send {
    /// Consume from `input`; return how much was consumed and, if an access unit was
    /// completed, a borrow of it. The borrow may alias `input` (zero-copy when the unit
    /// happens to be contiguous) or the parser's internal reassembly buffer.
    fn parse<'s>(&'s mut self, input: &'s [u8], ts: Timestamps) -> Result<ParseOutput<'s>>;

    /// End of stream: emit any partially-buffered final unit.
    fn flush<'s>(&'s mut self) -> Option<ParsedUnit<'s>>;

    /// Header-only inspection. Fills width/height/profile/level/sample rate/layout from
    /// in-band headers. This is the entire v0.1 (`vaco-probe`) contract.
    fn update_parameters(&self, params: &mut CodecParameters) -> Result<()>;

    /// Some parsers rewrite the packet (e.g. Annex-B reassembly). Reported so the
    /// scheduler knows whether it may pass the original buffer through.
    fn rewrites(&self) -> bool { false }
}

pub struct ParseOutput<'a> { pub consumed: usize, pub unit: Option<ParsedUnit<'a>> }

pub struct ParsedUnit<'a> {
    pub data: &'a [u8],
    pub pts: Option<i64>,
    pub dts: Option<i64>,
    pub duration: Option<u64>,
    pub key: bool,
    pub pict_type: Option<PictureType>,
    pub field: FieldOrder,
    pub repeat_pict: u32,
    pub output_delay: u8,
}
```

## 1.7 The bitstream-filter trait

Same shape as `Decoder`, deliberately. A BSF is packets-in/packets-out with the identical
`Again`/`Eof` state machine.

```rust
pub trait BitstreamFilter: Send {
    fn send_packet(&mut self, pkt: Packet) -> Result<(), SendError>;
    fn send_eof(&mut self) -> Result<(), SendError>;
    fn receive_packet(&mut self) -> Result<Packet, ReceiveError>;
    fn flush(&mut self);
    /// BSFs may rewrite extradata (`mp4toannexb`, `extract_extradata`); the muxer or
    /// decoder downstream must see the *output* parameters, not the input's.
    fn output_parameters(&self) -> &CodecParameters;
}

pub struct BsfDesc {
    pub name: &'static str,
    pub codecs: &'static [CodecId],       // empty = codec-agnostic (null, noise, setts)
    pub options: &'static OptionSchema,
    pub open: fn(&CodecParameters, &Options) -> Result<Box<dyn BitstreamFilter>>,
}
```

A BSF *chain* (`vaco-bsf-core::BsfChain`) implements `BitstreamFilter` itself, so
`h264_mp4toannexb,dump_extradata` is one object to the caller.

## 1.8 The threading contract

Three declarations, matching architecture §6's three axes. Pipeline parallelism is
`vaco-sched`'s business and needs nothing from the codec.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Threading {
    None,
    /// A single frame's slices/tiles/wavefronts decode concurrently.
    Slice { max_jobs: usize },
    /// N frames decode concurrently; `delay` is the extra output latency in frames.
    Frame { max_frames: usize, delay: usize },
    Both { max_frames: usize, max_jobs: usize, delay: usize },
}
```

### 1.8.1 Frame threading: mutable state never crosses a thread

FFmpeg propagates decoder state between per-thread contexts with
`update_thread_context`/`init_thread_copy`. That is a mechanism for sharing mutable state safely,
and it is exactly what we do not want. Our model instead **splits the decoder in two**:

- A **sequential header stage** that owns *all* mutable decoder state. It parses parameter sets
  and picture headers, maintains the DPB and reference lists, allocates the output picture, and
  emits a self-contained, `Send + 'static` **task**.
- A **stateless frame task** that owns its bitstream bytes, holds `Arc` snapshots of every
  parameter set it needs, holds `PictureRef`s to its references, holds the sole `PictureWriter`
  for its own output, and touches nothing else.

```rust
pub trait FrameThreadedDecoder: Decoder {
    type Task: FrameTask;

    /// Runs on the caller's thread, strictly in decode order. This is the only place
    /// mutable decoder state exists.
    fn split(&mut self, pkt: &Packet) -> Result<SplitOutcome<Self::Task>>;
}

pub enum SplitOutcome<T> {
    Task(T),
    /// Header-only packet (parameter sets, metadata OBUs) — nothing to schedule.
    NoOutput,
    /// Resolution/format change; the runner drains outstanding tasks before continuing.
    Reconfigure(Box<CodecParameters>),
}

pub trait FrameTask: Send + 'static {
    fn run(self: Box<Self>, ctx: &TaskCtx<'_>) -> Result<Frame>;
}
```

`TaskCtx` carries the thread pool handle (for nested slice threading), the buffer pool, the
selected `KernelSet`, and the cancellation token. It is `Sync`; it contains no per-frame state.

Because the task holds only `Arc`s and owned data, it is `Send` by construction and the compiler
proves the absence of data races. There is no `update_thread_context` equivalent because there is
nothing to update. The cost is that the header stage is serial — which is fine: it is <2% of decode
time for every codec we care about, and it is also where the DPB semantics live, which is exactly
the part you want single-threaded and easy to reason about.

### 1.8.2 "Frame N+1 may proceed once frame N has produced row R" — in safe Rust

This is the hard problem. FFmpeg solves it with one contiguous picture buffer, a raw pointer, and
an atomic row counter (`threadprogress.c`); readers race ahead of writers into the same
allocation. We cannot write that, and the ordinary borrow rules cannot express "this allocation is
`&mut` above row R and `&` below it, and R moves over time".

**The solution: ownership transfer at band granularity, published through `OnceLock`.**

A picture plane is allocated as a sequence of *bands*. Each band is a `GUARD`-padded row block —
`band_h + 2 * GUARD` rows of the plane's stride, where `GUARD` is the codec's maximum
inter-prediction filter reach (8 rows covers H.264, HEVC, VP9 and AV1). The writer owns each band
exclusively while filling it, then moves it into a `OnceLock`, which is where it becomes shared and
immutable. `OnceLock::set` is a release store; `OnceLock::get` is an acquire load. No locks, no
`unsafe`, and the type system enforces that nobody can observe a partially-written band.

```rust
// vaco-frame
pub struct ProgressPlane {
    bands:  Box<[OnceLock<PooledBand>]>,   // published, immutable once set
    ready:  AtomicU32,                     // rows guaranteed readable (monotonic)
    state:  Mutex<PlaneWaitState>,         // { rows: u32, failed: Option<Error> }
    wake:   Condvar,
    band_h: u32,
    guard:  u32,
    stride: usize,
}

/// Held by exactly one frame task. Not `Sync`, not `Clone`.
pub struct PictureWriter { planes: Vec<PlaneWriter>, picture: Arc<ProgressPicture> }

impl PictureWriter {
    /// Exclusive `&mut [u8]` for band `k` of `plane`. Panics if `k` was already published.
    pub fn band_mut(&mut self, plane: usize, k: usize) -> BandMut<'_>;

    /// Publish every band through `k`: fill each band's top guard rows from the tail of
    /// its predecessor, move the band into its `OnceLock`, then advance `ready` to
    /// `(k + 1) * band_h - guard` and wake waiters.
    ///
    /// The `- guard` is the publication lag: a band's *bottom* guard rows come from the
    /// band below, so band `k` only becomes fully readable once band `k+1` has started.
    pub fn publish_through(&mut self, plane: usize, k: usize);

    /// For slice/tile threading: hand out disjoint band ranges to concurrent jobs.
    pub fn split_bands_mut(&mut self, plane: usize, ranges: &[Range<usize>])
        -> Vec<BandRangeMut<'_>>;
}

/// `Drop` is the deadlock guard: a writer dropped before the plane is complete marks the
/// picture failed and wakes every waiter with an error. A panicking task therefore
/// unblocks its readers instead of hanging the pipeline.
impl Drop for PictureWriter { /* mark failed if incomplete, notify_all */ }

/// Cheap to clone, `Send + Sync`. This is what a frame task holds for each reference.
#[derive(Clone)]
pub struct PictureRef(Arc<ProgressPicture>);

impl PictureRef {
    /// Block until rows `..=y` of `plane` are readable. Returns `Err` if the producing
    /// task failed. Fast path is one relaxed atomic load and no syscall.
    pub fn wait_rows(&self, plane: usize, y: u32) -> Result<PlaneView<'_>>;
    pub fn try_rows(&self, plane: usize, y: u32) -> Option<PlaneView<'_>>;
    /// The whole picture, once complete — the non-threaded and post-decode path.
    pub fn finished(&self) -> Result<PlaneView<'_>>;
}
```

A frame task's motion-compensation loop therefore reads:

```rust
let need = mv_bottom_row(block, mv) + GUARD as i32;
let src  = reference.wait_rows(plane, need.clamp(0, height - 1) as u32)?;
let blk  = src.block(x0, y0, bw, bh + TAPS, &mut scratch);
kernels.mc_8tap_hv(blk.data, blk.stride, dst.data, dst.stride, frac_x, frac_y, bw, bh);
```

and after each CTU/macroblock row the producer calls
`writer.publish_through(plane, row_band)`.

### 1.8.3 What this costs, stated honestly

Kernels cannot take a bare `(&[u8], stride)` from a picture that is still being written, because
the plane is not one allocation. They take a `PlaneView`, and get a contiguous borrow out of it:

```rust
pub struct PlaneView<'a> { bands: &'a [PooledBand], band_h: u32, guard: u32, stride: usize, h: u32 }

pub struct BlockRef<'a> { pub data: &'a [u8], pub stride: usize }

impl<'a> PlaneView<'a> {
    /// Fast path: the requested `w × h` region lies inside one guard-padded band, so we
    /// return a contiguous borrow with the band's natural stride — identical cost to
    /// FFmpeg's `(ptr, stride)`.
    ///
    /// Cold path: the region straddles a band seam, or falls outside the picture. Copy it
    /// into `scratch` and borrow that. This is the *same* cold path that already exists
    /// for out-of-picture motion vectors (architecture §7.4, "pad the common case,
    /// emulate the rare case out-of-line"), so it costs us one extra condition, not a new
    /// mechanism.
    pub fn block(&self, x: i32, y: i32, w: u32, h: u32, scratch: &'a mut BlockScratch)
        -> BlockRef<'a>;
}
```

Straddle rate at `band_h = 256`, `GUARD = 8`: ~3% for 8×8 and 16×16 blocks (the overwhelming
majority), ~45% for AV1's rare 128-tall blocks. The cold path is a ≤135-row `copy_from_slice` of
≤128 bytes each. We budget **<1.5% of decode time** for this and we will measure it in
`vaco-checkasm` before AV1 lands, not after.

Three escape hatches exist and are documented, in this order of preference:

1. **`band_h = height`** (one band, one allocation) whenever frame threading is off or the codec
   is intra-only. `PlaneView` then always takes the fast path — the non-threaded case pays nothing.
2. **Slice/tile threading instead of frame threading** for codecs where it is at least as good.
   AV1 (tiles), HEVC (tiles + WPP) and VP9 (tile columns) are all in this category; frame threading
   matters most for H.264 and VP8, which have neither.
3. If measurement shows a specific kernel provably cannot reach parity, **escalate as a decision**
   per D2 — do not reach for `unsafe`.

### 1.8.4 Deadlock freedom

Three properties, each mechanically checkable:

1. **Acyclicity.** A task waits only on pictures that precede it in decode order, and the header
   stage emits tasks in decode order. The wait graph is therefore a DAG. Enforced by a debug
   assertion in `wait_rows` comparing a monotonic `decode_index` on both pictures.
2. **Monotonic progress.** `ready` never decreases; `publish_through` is the only writer, and it is
   called from the single task owning the `PictureWriter`.
3. **Liveness under failure.** `PictureWriter::drop` marks incomplete pictures failed and wakes all
   waiters. Every `wait_rows` therefore terminates: either progress arrives or the picture fails.
   A CI watchdog fails any conformance run whose thread pool is idle-blocked for >5 s.

### 1.8.5 Slice, tile and wavefront threading

```rust
pub trait SliceThreadedDecoder: Decoder {
    type Job: Send;
    /// Partition the current picture into independently-decodable jobs, each holding a
    /// disjoint `BandRangeMut` of the output.
    fn slice_jobs<'a>(&'a mut self, w: &'a mut PictureWriter) -> Result<Vec<Self::Job>>;
    /// Immutable `&self`: all per-job mutable state lives in `Job`.
    fn run_slice(&self, job: Self::Job) -> Result<()>;
}
```

Driven by `std::thread::scope` (or a rayon scope) over `split_bands_mut`. Safety is
`split_at_mut`-style disjointness — nothing exotic. Wavefront parallel processing (HEVC WPP,
VP9/AV1 within a tile) reuses the *same* `ProgressPlane` primitive at CTU-column granularity: a
row's job waits on the row above having published `x + 2` CTUs.

---

# 2. The shared-DSP crate decomposition

Research §1.11's dependency map is the input. The governing rule, applied ruthlessly:

> **A shared-DSP crate exists only when two or more codec crates depend on it.** Everything else
> lives in the codec crate that uses it. AV1's CDEF and loop restoration are AV1-only; they stay
> in `vaco-codec-av1`.

Every crate below: `#![forbid(unsafe_code)]`, a scalar reference implementation for every kernel,
a `std::simd` implementation selected once into a `KernelSet` of plain safe `fn` pointers at
construction (architecture §7.3), a `vaco-checkasm` differential test, and a criterion bench.
A kernel without a scalar reference and a differential test does not merge.

SIMD priority is on architecture §7.2's scale, 1 = do first.

| Crate | Contents | Depended on by | API shape | SIMD |
|---|---|---|---|---|
| `vaco-codec-vlc` | Multi-level LUT builder for canonical and non-canonical Huffman/VLC codes; sub-table generation, length-limited construction, incremental decode. Upstream's `vlc.c` role. | MJPEG, mpegvideo family, DV, Vorbis, HuffYUV/FFVHuff, UtVideo, Indeo, WMA, Bink, ~40 more | `VlcTable::build(&[(len, sym)]) -> VlcTable`; `reader.read_vlc(&table) -> Option<u16>` | 7 — scalar, but table layout + PGO matter enormously |
| `vaco-codec-golomb` | Exp-Golomb `ue/se/te/me`, unary, Golomb–Rice (fixed and adaptive `k`), Elias-gamma. Both read and write. | H.264, HEVC, VVC (dec + parse + SEI), FFV1, FLAC, Dirac, RV30/40/60, CAVS, Dolby Vision RPU, EVC, JPEG-LS | `reader.ue()`, `reader.se()`, `reader.rice(k)`, batched `rice_block(k, &mut [i32])` | 6 — the batched Rice path is where FLAC and FFV1 live |
| `vaco-codec-cabac` | The binary arithmetic coding **engine** only: range/offset state, `decode_decision`, `decode_bypass`, `decode_bypass_n`, `decode_terminate`, and the encoder mirror. Context-model *tables* and init are per-codec. | H.264, HEVC, VVC | `Cabac::new(&[u8]) -> Cabac`; `cabac.decision(&mut ctx) -> bool`; `cabac.bypass_n(n) -> u32` | 8 — **not vectorizable at all.** Upstream has no CABAC asm either. Wins come from table layout, branch layout, and PGO (architecture §7.2 #8). This is where we can genuinely beat C. |
| `vaco-codec-msac` | AV1/VP9's multi-symbol adaptive arithmetic decoder and VP8's boolean decoder. Separate crate from CABAC: different algorithm, disjoint dependents. | AV1, VP9, VP8 | `Msac::symbol(&mut cdf) -> usize`, `bool_(&mut p)`, `literal(n)`, `golomb()` | 8 — same reasoning as CABAC. The `cdf` update is a short vector op and *is* worth SIMD. |
| `vaco-codec-cbs-core` | The coded-bitstream-syntax framework: unit list, read/write round-trip, RBSP emulation-prevention insert/remove, fragment/reassembly, `trace_headers` hooks. | all `vaco-cbs-*`, all `vaco-bsf-*`, parsers | `trait CbsCodec { type Unit; fn read_unit(&mut RbspReader) -> Result<Unit>; fn write_unit(&Unit, &mut RbspWriter) }` | n/a |
| `vaco-cbs-h2645` | H.264 / HEVC / VVC syntax structures: VPS/SPS/PPS/APS, slice headers, SEI messages. Read **and** write, because the metadata BSFs must round-trip. | h264/hevc/vvc parsers + decoders, `h264_metadata`, `hevc_metadata`, `vvc_metadata`, `*_mp4toannexb`, `filter_units`, `extract_extradata`, `dovi_rpu`, `trace_headers` | typed structs + `Cbs` impls | n/a |
| `vaco-cbs-av1` | OBU syntax: sequence header, frame header, tile group, metadata OBUs, Annex-B/low-overhead framing. | av1 parser + decoder, `av1_metadata`, `av1_frame_merge/split`, `filter_units` | as above | n/a |
| `vaco-cbs-vp9`, `vaco-cbs-jpeg` | VP9 uncompressed header + superframe index; JPEG marker segments. | `vp9_metadata`, `vp9_superframe*`, `mjpeg2jpeg`, jpeg parser | as above | n/a |
| `vaco-codec-dsp-idct` | 8×8 IDCT/FDCT (integer, AAN, simple, "spec-exact" variants and the permutation model), 4×4 variants, `blockdsp` (clear/fill blocks) and `pixblockdsp` (get_pixels/diff_pixels). | mpegvideo family (H.261/H.263/MPEG-1/2/4/MSMPEG4/WMV1-2/FLV1/RV10-20), MJPEG, DNxHD, ProRes, SpeedHQ, DV, AMV | `fn idct_add(dst: &mut BlockMut, coeffs: &mut [i16; 64])`, `fn idct_put(...)`, `IdctPermutation` | 5 — butterflies vectorize well; the transpose stage is where we must measure |
| `vaco-codec-dsp-mc` | **Generic separable subpel FIR**, parameterised by tap count and coefficient table via const generics, plus half-pel bilinear, chroma bilinear/4-tap, average/bi-prediction blending, and the out-of-picture edge emulator. Codecs supply their own spec coefficients. | H.264, HEVC, VVC, VP8, VP9, AV1, RV30/40, SVQ3, mpegvideo family, VC-1 | `fn conv_hv<const TAPS: usize>(src: BlockRef, coef_h: &[i16; TAPS], coef_v: &[i16; TAPS], dst: &mut BlockMut, w: u32, h: u32)` | **4 — the single largest asm area upstream, and the biggest portable-SIMD win available to us.** Do this early. |
| `vaco-codec-dsp-deblock` | Edge-oriented loop-filter primitives: the boundary-strength-driven 4/6/8/14-tap luma and chroma filters, transpose helpers for vertical edges, and the masked-select machinery all of them need. Codec-specific decision logic stays in the codec crate; only the *filters* are shared. | H.264, HEVC, VP8, VP9, AV1, VC-1 | `fn filter_edge_luma(view: &mut EdgeMut, params: &EdgeParams)`, one entry per (width, direction) | **7 — hardest portable-SIMD target.** Branchy per-edge decisions become masked lane select. Expect to lose ground here first; budget explicit measurement. |
| `vaco-codec-dsp-intrapred` | DC/H/V/plane/Paeth/smooth predictors, directional predictors with the shared angle→delta machinery, edge filtering/upsampling, and the neighbour-availability model. | H.264, HEVC, VP8, VP9, AV1, RV30/40, SVQ3 | `fn pred_dir(dst: &mut BlockMut, above: &[u8], left: &[u8], angle: i32, flags: PredFlags)` | 6 — mixed; DC/H/V are trivial, directional needs shuffles |
| `vaco-codec-dsp-mecmp` | Block comparison metrics for **encoders**: SAD, SATD (Hadamard), SSE, NSSE, bit-exact DCT-domain costs, sub-block variants 4×4…64×64, and the `MeCmp` selection enum behind `-cmp`. | mpegvideo encoder family, SVQ1 encoder, and our future VP8/VP9/AV1 encoders | `fn sad(a: BlockRef, b: BlockRef, w: u32, h: u32) -> u32` and friends, dispatched through `MeCmpSet` | 3 — pure horizontal reduction, near-ideal portable SIMD, high value per effort |
| `vaco-codec-dsp-me` | The motion **search** (diamond/hex/EPZS/full/UMH patterns), predictor sets, sub-pel refinement, and the rate-distortion lambda plumbing. Built on `mecmp`. | same as `mecmp` | `trait MotionSearch { fn search(&self, ctx: &MeCtx) -> MotionVector }` | 4 (inherits `mecmp`) |
| `vaco-codec-dsp-ratecontrol` | 1-pass/2-pass rate control: complexity estimation, the ratecontrol expression evaluator (via `vaco-expr`), VBV/HRD model, QP→bits curves, adaptive quantisation. | every native video encoder | `trait RateControl { fn qp_for(&mut self, ctx: &FrameCtx) -> f32; fn commit(&mut self, actual_bits: u64) }` | n/a |
| `vaco-codec-dsp-lpc` | Autocorrelation, Levinson–Durbin, Cholesky, coefficient quantisation, order selection, and the **prediction restore/apply** kernels (order 1–32, i32/i64 accumulate, arbitrary shift). | FLAC, TAK, TrueHD/MLP, ALS, Shorten, WavPack, Bonk, OSQ, Monkey's Audio (T5) | `fn compute_autocorr(x: &[f64], order: usize, out: &mut [f64])`, `fn lpc_restore_i32(coefs: &[i32], shift: u32, buf: &mut [i32], order: usize)` | 5 — the restore recursion is serial across samples, so SIMD goes **across taps with a horizontal reduce**; effective for order ≥ 8, which is the common FLAC case |
| `vaco-codec-dsp-sinewin` | Window generation and application: sine, KBD (Kaiser–Bessel-derived), Vorbis power, and the overlap-add/fold primitives every MDCT codec shares. Windows are computed at init from spec formulae, never tabled from someone else's source. | AAC, AC-3/E-AC-3, Vorbis, Opus/CELT, WMA family, ATRAC family, MLP encoder, TwinVQ, MPEG audio (17 dependents upstream) | `Window::sine(n) -> Window`, `fn overlap_add(prev: &[f32], cur: &[f32], win: &Window, out: &mut [f32])` | 3 — trivially vectorizable, high value, tiny crate |
| `vaco-codec-dsp-fmtconvert` | The audio output stage: f32↔s16/s32 with scale and clip, planar↔interleaved, int→float with per-channel gain, and the "decode in f32, emit in the container's format" bridge. | every audio decoder and encoder | `fn f32_to_s16(src: &[f32], dst: &mut [i16], scale: f32)`, `fn interleave(planes: &[&[f32]], out: &mut [f32])` | 3 — saturating pack, ideal portable SIMD |
| `vaco-codec-dsp-dwt` | Discrete wavelet transforms: 5/3 and 9/7 reversible/irreversible lifting, the Dirac/VC-2 filter family. | JPEG 2000, Dirac/VC-2, Snow (if ever) | `fn idwt_2d(plane: &mut PlaneMut, kind: WaveletKind, levels: u8)` | 6 |
| `vaco-codec-mpegvideo` | The genuinely-shared MPEG-family decoder core: picture/macroblock structures, motion-vector prediction, run-level tables engine, the DC/AC prediction model, the shared slice loop, plus the encoder half (`mpegvideo_enc` role) built on `dsp-me`/`-mecmp`/`-ratecontrol`. | H.261, H.263/+/i, MPEG-1, MPEG-2, MPEG-4 Part 2, MSMPEG4 v1–3, WMV1/2, FLV1, RV10/20, IPU, SpeedHQ | a `MpegDecoder` struct generic over a `Flavour` trait supplying the per-codec header parse, VLC tables and quirks | inherits `idct` + `mc` |

Existing crates this plan leans on rather than duplicating: `vaco-bitstream` (layer 0 — bit
reader/writer with the checked-tail/unchecked-body split), `vaco-tx` (layer 3 — FFT/MDCT/RDFT/DCT,
shared by every transform audio codec), `vaco-simd` (layer 0 — feature detection and the
kernel-selection model), `vaco-scale` and `vaco-resample` (layer 3).

**SIMD sequencing.** `dsp-fmtconvert` and `dsp-sinewin` first (days of work, immediate wins across
every audio codec). Then `dsp-mecmp` and `dsp-mc` (largest video win). Then `dsp-idct` and
`vaco-tx`. `dsp-deblock` last, with an explicit measurement gate before we commit to the design.
`cabac`/`msac` get no SIMD at all — they get PGO, table-layout work, and a dedicated benchmark
suite.

---

# 3. Codec tiering

Method: the upstream inventory is ~605 decoder and ~271 encoder registrations. Of the encoders,
~85 are hardware or external-library wrappers (§8 covers those; they are not software codecs we
write). Tiers below partition the *software* implementations. Counts are registration entries
(`pcm_s16le` and `pcm_s16be` count as two), with the crate count in brackets — that is the number
that matters for parallel work.

| Tier | Meaning | Decoders | Encoders | Crates | Effort |
|---|---|---|---|---|---|
| **T1** | Default build, royalty-free, high value | ~97 | ~77 | 24 | ~249 pw |
| **T2** | Default build, decode-only, expired or low-risk patents | ~60 | ~12 | 18 | ~200 pw |
| **T3** | In-tree behind `patent-encumbered-*`, never shipped | ~20 | ~12 | 8 | ~350 pw |
| **T4** | Long tail, low priority, non-FFmpeg documentation exists | ~125 | ~25 | 10 | ~250 pw |
| **T5** | Cannot be done cleanly — no public spec | ~300 | ~60 | — | see §3.5 |
| | **Total** | **~602** | **~186** | **60** | |

## 3.1 T1 — default build, royalty-free, high value

**Rationale.** Every entry is 🟢 GREEN or 🟡 AMBER-with-a-good-story in the legal register §2.3,
carries a real royalty-free grant or has no patent story at all, and is something users actually
encounter. This is the set that makes Vaco a usable tool on the modern open web: WebM/MP4 with
AV1/VP9/Opus, FLAC and ALAC archives, PNG/JPEG/WebP images, PCM/ADPCM in every container, and FFV1
for preservation. If we shipped only T1 we would already be useful.

| Group | Entries | Crate |
|---|---|---|
| AV1 decode | 1 | `vaco-codec-av1` |
| VP9 decode + encode | 2 | `vaco-codec-vp9` |
| VP8 decode + encode | 2 | `vaco-codec-vp8` |
| FFV1 decode + encode | 2 | `vaco-codec-ffv1` |
| Opus decode + encode | 2 | `vaco-codec-opus` |
| Vorbis decode + encode | 2 | `vaco-codec-vorbis` |
| FLAC decode + encode | 2 | `vaco-codec-flac` |
| ALAC decode + encode | 2 | `vaco-codec-alac` |
| PCM | 38 dec / 20 enc | `vaco-codec-pcm` |
| ADPCM (standardised: G.722, G.726/le, MS, SWF, IMA-WAV, IMA-QT) | 7 / 7 | `vaco-codec-adpcm` |
| PNG + APNG | 2 / 2 | `vaco-codec-png` |
| JPEG / MJPEG / MJPEG-B | 3 / 2 | `vaco-codec-jpeg` |
| WebP (lossy + lossless + animated) | 2 / 2 | `vaco-codec-webp` |
| GIF | 1 / 1 | `vaco-codec-gif` |
| BMP, PCX, TGA, SGI, XWD, XBM | 6 / 6 | `vaco-codec-image-simple` |
| PNM family (pbm/pgm/pgmyuv/ppm/pam/pfm/phm) | 7 / 7 | `vaco-codec-pnm` |
| QOI | 1 / 1 | `vaco-codec-qoi` |
| TIFF | 1 / 1 | `vaco-codec-tiff` |
| OpenEXR | 1 / 1 | `vaco-codec-exr` |
| Raw/uncompressed (rawvideo, v210, v210x, y41p, r10k, r210, avui, bitpacked, wrapped_avframe) | 9 / 9 | `vaco-codec-rawvideo` |
| Text subtitles (ass, ssa, srt, subrip, webvtt, movtext, text; + ttml encode) | 7 / 8 | `vaco-codec-subtitle-text` |
| Null (vnull, anull) | 0 / 2 | `vaco-codec-null` |

**"Container-adjacent essentials"** — the things without which a container is useless even when no
decoding happens: `wrapped_avframe`, `rawvideo`, the PCM set, the text-subtitle set, and — crucially —
the **header parsers** for H.264/HEVC/AV1/AAC/Opus (§6). Parsing an SPS is not decoding; those
parsers ship in the default build and are what D5's v0.1 milestone is made of.

## 3.2 T2 — default build, decode-only, expired or low-risk patents

**Rationale.** Legal register §2.3 marks these 🟢 GREEN (expired) or 🟡 AMBER-decode-only, and §2.4
establishes that decode-only is materially lower risk even where the licence text does not
distinguish. Everything here is spec-first implementable. Encoders are included only where the
legal verdict is GREEN for encode *and* the encoder is cheap because it shares the mpegvideo core.

| Group | Spec | Enc? | Note |
|---|---|---|---|
| MPEG-1 video, MPEG-2 video | ISO/IEC 11172-2, 13818-2 \| ITU-T H.262 | yes | Last US patent expired 2018-02-13 |
| MPEG-4 Part 2 (ASP/SP) | ISO/IEC 14496-2 | yes | Verify the Brazilian tail before selling into BR |
| H.261, H.263, H.263+ | ITU-T H.261, H.263 | yes | Long expired |
| MSMPEG4 v1/2/3, WMV1/2, FLV1, RV10/20 | — | v2/v3/wmv1/wmv2/flv only | **T5-flavoured**: these are mpegvideo-family variants with no published spec. Included only because the mpegvideo core does 90% of the work; the deltas need the §1.7-T2 clean-room protocol. |
| VC-1 / WMV3 | SMPTE 421M-2006 | no | Decode only; pool posture aged out but unverified |
| Theora | Theora Specification (Xiph, 2011-03-16) | no | RF by design |
| Dirac / VC-2 | SMPTE ST 2042-1 | yes | RF |
| MP1/MP2/MP3 (+ mp3adu, mp3on4) | ISO/IEC 11172-3, 13818-3 | MP2, MP3 | MP3 programme terminated 2017-04-23; GREEN enc+dec |
| AC-3 | ATSC A/52 | yes | Last patent expired 2017-03-20. **Never use the "Dolby Digital" mark.** |
| E-AC-3 | ATSC A/52 Annex E | no | 🟡 ships **only after** the 2026-01-30 expiry is independently verified (legal Q3) |
| MPEG-4 ALS | ISO/IEC 14496-3 Annex | no | Lossless |
| JPEG 2000 | ISO/IEC 15444-1 | yes | Needs `dsp-dwt` |
| JPEG-LS | ITU-T T.87 \| ISO/IEC 14495-1 | yes | |
| JPEG XL | ISO/IEC 18181-1/-2 | later | Google RF grant; large but valuable |
| APV | SMPTE ST 2118 | later | |
| DV | SMPTE ST 314M / 370M | yes | |
| DNxHD / VC-3 decode | SMPTE RDD 36 / ST 2019 | **no** | 🟡 Avid licence reportedly required for commercial use — decode only |
| ProRes decode | Apple ProRes (partially published) + SMPTE RDD 36-adjacent | **no** | 🟡 Apple's objection is encoder-focused; **encode is RED** |
| G.711, G.722, G.726, G.729, G.723.1 | ITU-T G.711/G.722/G.726/G.729/G.723.1 | G.711, G.722 | G.729 royalty-free since 2017-01-01 |
| AMR-NB decode | 3GPP TS 26.090 | no | 🟡 |
| Speex decode | Speex Manual (Xiph) | no | Superseded by Opus |
| SBC | Bluetooth SIG A2DP / SBC | yes | |
| DVB subtitles, DVD subtitles, PGS, CEA-608/708, Teletext | ETSI EN 300 743, ETSI EN 300 706, CTA-708 | dvbsub, dvdsub | PGS and VOBSUB are RE — clean-room |
| Comfort noise | RFC 3389 | yes | |
| dfpwm, QOA | Open informal specs | yes | Trivial |

## 3.3 T3 — in-tree behind `patent-encumbered-*`, never shipped

**Rationale.** D4 and legal register §5.2/§5.3. These are implemented in-tree so the code is
reviewed, tested, fuzzed and CI-built, but the feature is never in `default`, never in `full-rf`,
and CI asserts its absence from every published binary (both by compiled-feature list and by the
`Caps::PATENT_ENCUMBERED` runtime assertion of §1.3). Feature names say exactly what they are.

| Feature | Contents | Legal verdict | Effort |
|---|---|---|---|
| `patent-encumbered-h264-decode` | H.264 decoder (Baseline→High 4:4:4 Predictive) | 🟡 AMBER — Via LA pool active to ~2027–28, "unit" counts decoders | 60 pw |
| `patent-encumbered-h264-encode` | **Not planned.** See §5. | 🟡 | — |
| `patent-encumbered-hevc-decode` | HEVC decoder (Main/Main10/Main12/RExt) | 🔴 RED — multi-pool, injunction-seeking holders | 55 pw |
| `patent-encumbered-hevc-encode` | **Not planned.** See §5. | 🔴 | — |
| `patent-encumbered-vvc-decode` | VVC decoder | 🔴 RED | 110 pw |
| `patent-encumbered-aac-decode` | AAC-LC, HE-AAC (SBR), HE-AACv2 (PS), LATM/LOAS | 🔴 RED — Via LA AAC pool active; royalty attaches to decoder units | 30 pw |
| `patent-encumbered-aac-encode` | AAC-LC encoder | 🔴 RED | 25 pw |
| `patent-encumbered-ac3-encode` | AC-3 / E-AC-3 encoders | 🟢/🟡 patents, but encoder-side trademark and certification exposure | 12 pw |
| `patent-encumbered-dts-decode` | DTS core (+ extensions if ever) | 🔴 RED | 40 pw |
| `patent-encumbered-avs` | AVS2 / AVS3 decode | 🟡 unverifiable Chinese pool posture | 40 pw |

Note that **remuxing does not need any of this**. The AAC pool charges on encoder/decoder units,
explicitly not on bitstreams, so `vaco -i in.mkv -c copy out.mp4` with an AAC track is fine in the
default build, and so is `vaco-probe` reading the `AudioSpecificConfig`. Only decoding is gated.
Likewise HEIF: parse the container, refuse the HEVC payload.

## 3.4 T4 — long tail, low priority

**Rationale.** Legacy, game and FMV formats where a **non-FFmpeg** description exists: community
reverse-engineering documentation written independently of FFmpeg's source, vendor SDK
documentation, published academic papers, or — the underused one — **expired patents, which are
published specifications by construction**. Cinepak, Duck TrueMotion, Indeo and several ADPCM
variants all have granted patents describing the algorithm in normative detail.

These are perfect "good first codec" work packages: small, self-contained, one crate each, no
shared-DSP dependencies, and each one is a complete deliverable for a single contributor.
Grouped into ~10 crates (`vaco-codec-legacy-game-video`, `-legacy-game-audio`,
`-legacy-screen`, `-legacy-dpcm`, `-legacy-adpcm`, `-legacy-realmedia`, `-legacy-ms`,
`-legacy-apple`, `-legacy-image`, `-legacy-misc`) rather than 125 crates.

**Policy for T4:** every T4 codec needs its provenance trailer to name a *specific, archivable*
document, and the gatekeeper (legal §1.6.2) checks the document is not a paraphrase of FFmpeg's
source. Where no such document survives, the codec is T5, not T4.

## 3.5 T5 — cannot be done cleanly

**This is roughly half the upstream decoder inventory: ~300 decoders and ~60 encoders.**

The formats where FFmpeg's source *is* the specification. Research §2.4–2.6, §2.10–2.11 mark them
"RE". They include things people genuinely want — Monkey's Audio, TrueHD/MLP, WavPack, TTA, DTS
extensions, ATRAC, WMA Pro/Lossless/Voice, Bink, Smacker, RealVideo 3/4/6, ProRes RAW, CineForm
internals, HAP, most screen-capture codecs, and ~200 game/FMV formats.

**These cannot be implemented spec-first, because there is no spec.** Saying otherwise would be
dishonest about the clean-room policy. Four responses, applied by triage:

1. **Omit (the default, ~250 formats).** Do not implement. `vaco` reports the stream, names the
   codec, and says "no decoder — see `docs/why-some-codecs-are-not-included.md`". This is not a
   failure; it is the honest consequence of D7. FFmpeg accumulated these over 25 years of people
   scratching personal itches, and the marginal user value of `bethsoftvid` is approximately zero.
2. **Two-team clean room (legal §1.7 T2), for the ~15 that matter.** Candidate list, in order of
   user value: TrueHD/MLP decode, WavPack decode, Monkey's Audio decode, TTA decode, DTS core
   decode (ETSI TS 102 114 covers the core — partially spec-available), ProRes decode (partially
   published), Bink + Smacker (huge installed base of game media), RealVideo 3/4, ATRAC3/3+,
   WMA v1/v2, QuickTime RLE/Animation, MS Video 1, Cinepak, HuffYUV/FFVHuff, UtVideo.
   Protocol: a **dirty reader** who may read FFmpeg produces a behavioural specification document
   in `planning/specs/`; a **gatekeeper** reviews it for expression leakage; a **clean implementer**
   who has never read FFmpeg for that module writes the code. Budget **2.5×** the normal effort.
   Total for the 15: ~120 pw at 2.5× = **~300 pw**. This is a large, slow, low-priority programme
   and it should not start before v1.0.
3. **Independent reverse engineering from sample files.** Legitimate and clean (it produces a
   *new* spec document from observation, not from someone's source), but slower still. Reserved
   for formats with active demand and a good sample corpus. In practice this collapses into (2).
4. **Out-of-process delegation.** For anything the user already has a tool for, `vaco` can exec a
   user-installed binary — the same pattern legal §4.4.2 recommends for x264. No GPL, no patents,
   no clean-room issue in our tree. Worth building the mechanism once (`vaco-codec-exec`,
   ~4 pw) because it also solves x264/x265 and gives users an escape hatch for everything in T5.

**Recommendation: publish the specification documents we produce.** Every T2-clean-room spec doc
is a public good the multimedia community has wanted for two decades, it strengthens our
clean-room evidence trail by making the process auditable, and it is excellent marketing.

---

# 4. Per-codec implementation plans — the T1 set

Common to all: `#![forbid(unsafe_code)]`; a fuzz target from the day the crate lands (D6); a
`Vaco-Provenance` trailer per PR; a scalar reference for every kernel; differential testing against
the reference binary as a black box; bit-identical output at every thread count.

Effort estimates are **person-weeks of a competent engineer working from the spec**, including
tests, fuzzing, docs and conformance, excluding SIMD optimisation beyond a first pass (SIMD is
tracked as separate work packages in §7). They assume no prior familiarity with the codec.

## 4.1 AV1 — decode — **70 pw** — the largest single T1 item

**Governing specification.** *AV1 Bitstream & Decoding Process Specification, Version 1.0.0 with
Errata 1* (Alliance for Open Media, 2019-01-08). Supporting: *AV1 Codec ISO Media File Format
Binding v1.2.0* (for `av1C`), the Annex B length-delimited framing in §5.2, levels in Annex A,
film grain synthesis in §7.18.3.

**Patent posture.** 🟡 AMBER-with-a-good-story. AOMedia Patent License 1.0 gives a real RF grant
from ~50 members, but *Dolby v. Snap* (D. Del. 1:26-cv-00317, filed 2026-03-23) shows non-members
can assert. We ship it. **We must reproduce the AOM licence with our distribution** — that is a
condition of the grant, and it is action item #8 in the legal register.

**Stages.**

| # | Stage | Content | pw |
|---|---|---|---|
| 1 | OBU layer | OBU parsing, temporal units, Annex-B vs low-overhead framing, sequence header, `av1C` extradata, operating points, temporal/spatial layer selection | 5 |
| 2 | Frame header | Frame type, reference frame management (the 8-slot ref buffer, `ref_frame_idx`, order hints), frame size/render size/superres, tile info, quantiser params, segmentation, loop filter params, CDEF params, LR params, `frame_refs_short_signaling` | 8 |
| 3 | Symbol decoder | The multi-symbol arithmetic decoder, CDF initialisation tables, forward/backward CDF update, `disable_cdf_update`, per-tile CDF save/restore. → `vaco-codec-msac` | 4 |
| 4 | Tile/superblock loop | Partition tree (4×4…128×128, T/H/V splits), mode info, skip/segment, delta-Q/delta-LF | 5 |
| 5 | Intra | DC/V/H/Paeth/smooth×3, directional with edge filter + upsample, filter-intra, chroma-from-luma, palette, intrabc | 8 |
| 6 | Inter | MV prediction stack (the `ref_mv_stack` construction is the fiddliest part of AV1), 8-tap subpel with 4 filter types, warped motion (local + global), OBMC, compound modes (wedge, diff-weighted, distance-weighted, inter-intra), masked blending | 12 |
| 7 | Transforms | DCT/ADST/flipADST/identity/WHT, 4…64 point, 16 tx sizes × 16 tx types, quantiser and dequant tables, lossless mode | 8 |
| 8 | Post-filters | Deblocking, CDEF (direction search + primary/secondary taps), superres (upscaling), loop restoration (Wiener + self-guided) | 8 |
| 9 | Film grain | AR coefficient noise synthesis, chroma scaling LUTs, blending, `clip_to_restricted_range` | 4 |
| 10 | Threading + integration | Tile threading, frame threading, DPB, `show_existing_frame`, `Decoder` impl, error concealment | 5 |
| 11 | Conformance + fuzz | Argon streams bring-up, failure triage, fuzz corpus | 3 |

**Conformance suite.** **AOM Argon Streams AV1** — the definitive suite, tens of thousands of
targeted streams with expected MD5s, covering syntax corners no real encoder produces. Plus
`libaom` test vectors (`av1-test-vectors`) for real-world sanity. Gate: 100% Argon pass for the
profiles we claim, before AV1 leaves experimental.

**Hot kernels and SIMD shape.**

| Kernel | Share | SIMD shape | Priority |
|---|---|---|---|
| Inter prediction (8-tap separable, `conv_hv`) | 20–30% | Widening multiply-add over `i16xN`, saturating pack. `vaco-codec-dsp-mc`. Textbook portable SIMD. | 1 |
| Inverse transforms | 15–20% | Butterflies over `i32x8`; the transpose stage needs lane shuffles and is where we may lose ground. | 2 |
| Loop restoration + CDEF | 10–15% | Wiener is separable FIR (easy). Self-guided needs integral images (prefix sums — vectorizable with a log-step scan). CDEF is masked select over 8 directions. | 3 |
| Symbol decode (msac) | 10–20% | **None.** Scalar quality + table layout + PGO. | — |
| Intra prediction | 5–10% | Mixed; directional needs shuffles. | 4 |
| Film grain | 3–8% | Blending vectorizes; AR generation is serial. | 5 |

**Threading.** Tiles first — AV1 tiles are fully independent (CDF restore at tile start), so tile
threading is embarrassingly parallel and needs no progress primitive. Frame threading second, for
low-tile-count streams, using §1.8's `PictureRef`/`PictureWriter`. Post-filters are row-parallel
within a frame. Expect near-linear scaling to 8 threads on tiled content.

**DSP dependencies.** `vaco-codec-msac`, `-dsp-mc`, `-dsp-intrapred`, `-dsp-deblock`,
`vaco-cbs-av1`, `vaco-bitstream`, `vaco-simd`. CDEF, LR, superres and film grain stay in
`vaco-codec-av1` (AV1-only — the §2 rule).

**Sequencing note.** This is the one T1 item that cannot be done by one person in a reasonable
time. Split it as eleven work packages (§7 C-30…C-40) with a stable internal API defined up front:
stages 1–4 are one contributor's critical path, stages 5, 6, 7, 8, 9 are five independent
contributors working against the stage-4 output structures.

**AV1 encode is not in T1's critical path.** `rav1e` (BSD-2, pure Rust) clears D10 Gates 1 and 2
and is the strongest build-or-buy candidate in the whole inventory — see §4A.2, which recommends
starting with it behind `vaco-codec-av1`'s `backend-external` feature. A from-scratch competitive
AV1 encoder is 80–120 pw and belongs after v1.0. **rav1e's source is subject to the same
clean-room discipline as FFmpeg's**: we may depend on the crate, but nobody writing our native AV1
encoder may read it. Implement from the AOM specification.

## 4.2 VP9 — decode + encode — **26 pw decode / 22 pw encode**

**Governing specification.** *VP9 Bitstream & Decoding Process Specification, Version 0.6*
(Google, 2016-03-31). Supporting: *VP9 Bitstream Superframe and Uncompressed Header* (Google,
2016) for superframe indices, RFC 9628 for RTP payload, the WebM `VP9 Codec ISO Media File Format
Binding` for `vpcC`.

**Patent posture.** 🟡 AMBER — Google RF grant, Sisvel disputes sufficiency. Lower practical risk
than AV1 (less deployed, less worth suing over). Ship.

**Stages.** (1) Uncompressed header + superframe index + `vpcC` — 2 pw. (2) Bool decoder and
probability model, forward updates and backward adaptation — 3 pw. (3) Tile/superblock loop,
partition tree, mode info — 3 pw. (4) Intra prediction (10 modes) — 3 pw. (5) Inter: MV
prediction, 8-tap subpel with 3 filter types, compound prediction — 5 pw. (6) Transforms:
DCT/ADST 4/8/16/32, WHT lossless — 4 pw. (7) Loop filter (4 filter widths, level/sharpness
derivation) — 3 pw. (8) Profiles 1–3: 4:2:2/4:4:0/4:4:4 and 10/12-bit — 2 pw. (9) Threading,
integration, conformance — 1 pw.

**Conformance.** `vp9-test-vectors` from the WebM project (`vp90-2-*.webm` with per-frame MD5s),
which covers all four profiles and the resize/superframe corners. Gate: 100%.

**Hot kernels.** 8-tap MC (`dsp-mc`, shares the machinery with AV1 — different coefficients only);
iDCT/iADST 4–32 (`dsp-idct`-adjacent but VP9-specific transforms live in the codec crate);
loop filter (`dsp-deblock`); bool decoder (`vaco-codec-msac`, scalar); probability adaptation
(a short vectorizable pass over ~2000 counters at frame end).

**Threading.** Tile columns are independent → tile threading. VP9 also supports frame-parallel
mode signalled in the uncompressed header (`frame_parallel_decoding_mode`), which disables backward
adaptation and makes frame threading trivial; when it is off, frame threading needs the §1.8
progress primitive. Loop filter is row-parallel.

**Encode** (22 pw) reuses `dsp-me`, `dsp-mecmp` and `dsp-ratecontrol`. Target: correct and
reasonable, not competitive with libvpx. Do it after the decoder ships and after `dsp-me` exists.

**DSP dependencies.** `vaco-codec-msac`, `-dsp-mc`, `-dsp-intrapred`, `-dsp-deblock`,
`vaco-cbs-vp9` (for the metadata BSF), plus `-dsp-me`/`-mecmp`/`-ratecontrol` for encode.

## 4.3 VP8 — decode + encode — **10 pw / 12 pw**

**Governing specification.** RFC 6386, *VP8 Data Format and Decoding Guide* (November 2011).
RFC 7741 for RTP payload.

**Clean-room note, specific to this codec.** RFC 6386's normative content includes embedded
reference source code. That code is licensed under the WebM BSD licence with Google's patent
grant, and it is *not* FFmpeg — reading it is lawful and carries no FFmpeg contamination. But
copying it would create a BSD attribution obligation and would sit awkwardly with our
"implement from prose" discipline. **Policy: implement from the RFC's prose and pseudocode
sections; treat the embedded C as a tie-breaker for ambiguity only; never transcribe it.** Record
this in the provenance trailer.

**Patent posture.** 🟢 GREEN — the MPEG LA pool effort was abandoned in March 2013 after Google
cross-licensed all 11 holders, plus Google's RF grant.

**Stages.** Frame header and segmentation — 1 pw. Bool decoder — shared with VP9, ~0. Macroblock
mode/MV decode — 2 pw. Intra (16×16, 8×8 chroma, 4×4 B_PRED) — 1.5 pw. Inter (6-tap subpel,
bicubic/bilinear filters, split MVs) — 2 pw. Transforms (4×4 DCT + WHT) — 1 pw. Loop filter
(normal + simple) — 1.5 pw. Golden/altref buffer management, integration, conformance — 1 pw.

**Conformance.** `vp8-test-vectors` (`vp80-00-comprehensive-001…017`, plus the intra/inter/segment
targeted sets) with per-frame MD5s. Gate: 100%.

**Threading.** VP8 has no tiles and no slices, so **frame threading is the only intra-decoder
parallelism available** — this makes VP8 the best small test case for the §1.8 machinery, and it
should be the *first* codec that exercises `PictureRef`/`PictureWriter`. Additionally, VP8's loop
filter can be run one macroblock row behind reconstruction on a second thread.

**DSP dependencies.** `vaco-codec-msac`, `-dsp-mc`, `-dsp-intrapred`, `-dsp-deblock`.

## 4.4 Opus — decode + encode — **16 pw / 20 pw**

**Governing specification.** RFC 6716 (September 2012), *Definition of the Opus Audio Codec*,
**as updated by RFC 8251** (October 2017, the normative corrections — implementing 6716 without
8251 will fail the current test vectors). Supporting: RFC 7845 (Ogg encapsulation and `OpusHead`),
RFC 7587 (RTP payload), RFC 8486 (channel mapping families 2/3, ambisonics), and the multistream
API description in RFC 7845 §5.

**Patent posture.** 🟢 GREEN — the single best audio choice available. RF by design, with
royalty-free IPR disclosures from Xiph, Broadcom and Microsoft. No pool exists.

**Stages — decode.**

| # | Stage | pw |
|---|---|---|
| 1 | Range decoder (the entropy coder shared by SILK and CELT), packet framing (TOC byte, codes 0–3, self-delimiting framing), padding | 2 |
| 2 | CELT: band structure, PVQ decode + spreading, coarse/fine energy, anti-collapse, transient handling, MDCT sizes 120/240/480/960 | 5 |
| 3 | SILK: LSF/LPC decode and stabilisation, LTP, excitation (pulses + LSBs + signs), gains, NLSF interpolation, stereo prediction, PLC | 5 |
| 4 | Hybrid mode: mode switching, redundancy frames, delay compensation, the SILK↔CELT crossover at 8 kHz | 2 |
| 5 | Multistream/surround (mapping families 0/1/2/3), pre-skip, gain, `OpusHead`/`OpusTags` | 1 |
| 6 | Integration, PLC/FEC (`opus_decode` with `fec=1`), conformance, fuzz | 1 |

**Conformance.** The official Opus test vectors (`testvector01`…`testvector12`, updated for
RFC 8251) compared with `opus_compare`'s weighted-spectral metric — Opus is deliberately **not**
bit-exact-mandated, so the gate is `opus_compare` quality score ≥ the reference threshold, not
byte equality. **We must write our own `opus_compare` equivalent from RFC 6716 §6's description**
rather than using libopus's tool, and validate it by checking that a deliberately-degraded decode
fails. Additionally: the RFC 8251 vectors, and differential testing against the reference binary
per D6.

**Hot kernels.** MDCT/IMDCT at 4 sizes (`vaco-tx` — the largest single cost in CELT); PVQ decode
(integer combinatorics, partially vectorizable); LPC synthesis filter (order ≤ 16, **serial
recursion** — SIMD across taps with horizontal reduce, or unroll by 4 with the standard
dependency-breaking trick); LTP filter (5-tap, serial); comb filter/postfilter; range decoder
(scalar, ~10% of decode time, PGO territory); resampling for non-48k output (`vaco-resample`).

**Threading.** **None internally.** Opus frames are 2.5–60 ms and decode in microseconds; the
parallelism is at the pipeline level (`vaco-sched`). Declaring `Threading::None` here is correct,
not a gap. Multistream surround *could* decode streams concurrently but the frames are too small
to pay for it.

**DSP dependencies.** `vaco-tx` (MDCT), `-dsp-sinewin` (CELT's window overlap), `-dsp-lpc`
(SILK's LPC/LSF machinery), `-dsp-fmtconvert`, `vaco-resample`.

**Encode (20 pw)** is genuinely harder than decode: the SILK noise-shaping quantiser, pitch
analysis, the CELT band-energy allocation and PVQ search, and the mode/bandwidth decision logic
are all quality-critical heuristics that the RFC describes but does not mandate. Target: within
0.5 dB of the reference at the same bitrate, verified by an `opus_compare`-style metric on a
listening corpus. Defer until after the decoder ships and after `vaco-tx` is optimised.
**There is no fallback here.** libopus bindings are FFI and fail D10 Gate 1; no pure-Rust Opus
encoder of production quality exists (§4A.2). Until we write it, Vaco has no Opus encoder —
which is a real gap, because Opus is the one audio encoder every user will want. This makes
Opus encode the highest-priority audio encoder in the plan.

## 4.5 FLAC — decode + encode — **6 pw / 6 pw**

**Governing specification.** RFC 9639, *Free Lossless Audio Codec (FLAC)* (December 2024). This is
a genuinely complete, modern specification — one of the easiest codecs in the entire project to
implement correctly. Supporting: the Ogg-FLAC mapping in RFC 9639 §10.2, and the `fLaC` metadata
block layout for the Matroska/MP4 `CodecPrivate`.

**Patent posture.** 🟢 GREEN — Xiph, RF by design.

**Stages — decode.** (1) `STREAMINFO` and the metadata block chain (`SEEKTABLE`, `VORBIS_COMMENT`,
`PICTURE`, `CUESHEET`, `APPLICATION`) — 1 pw. (2) Frame header parse, CRC-8/CRC-16 verification,
variable blocksize, the sample-rate/bit-depth escape codes — 1 pw. (3) Subframe decode:
`CONSTANT`, `VERBATIM`, `FIXED` (orders 0–4), `LPC` (orders 1–32, arbitrary precision and shift) —
1.5 pw. (4) Residual: Rice partitioning (methods 0 and 1), escape codes, `partition_order` —
1 pw. (5) Stereo decorrelation (independent/left-side/right-side/mid-side), 32-bit-per-sample
support, wasted-bits — 0.5 pw. (6) Seeking (seektable + binary search fallback), the raw-FLAC
frame resync parser, integration, conformance — 1 pw.

**Conformance.** The IETF CELLAR **`flac-test-files`** repository — `subset/` (64 files exercising
every legal subset configuration), `uncommon/` (non-subset streams, 32-bit, extreme blocksizes),
and `faulty/` (must be rejected cleanly, not panic — this is a fuzz-adjacent gate). Plus xiph's
historical test suite and differential testing. Gate: byte-exact PCM on every `subset/` and
`uncommon/` file; clean `Err` on every `faulty/` file.

**Hot kernels.** LPC restore (`dsp-lpc::lpc_restore_i32`) is 50–70% of decode time — the recursion
`s[i] = r[i] + (Σ c[j]·s[i-j]) >> shift` is serial across `i`, so vectorisation goes **across the
tap dimension with a horizontal reduce**, which pays from order ≈ 8 upward and is the common case.
Rice decoding is the other 20–30%: bit-serial, but a multi-symbol path that extracts several
quotient/remainder pairs from one 64-bit window is a large win and is where our `vaco-bitstream`
unchecked-body/checked-tail split earns its keep. Stereo decorrelation and the int→output
conversion are trivially vectorizable (`dsp-fmtconvert`).

**Threading.** FLAC frames are fully independent, so **frame threading is trivially safe here** —
no `PictureRef` machinery needed, just N decoders over N frames with an output reorder queue. The
frame parser (finding frame boundaries by CRC in a raw stream) is the only serial part. Worth
doing: FLAC decode is fast enough to be I/O bound, but multi-threaded verification of a large
archive is a real use case.

**Encode (6 pw).** Autocorrelation → Levinson–Durbin → coefficient quantisation → order and
partition-order search, all in `dsp-lpc`. Add `-compression_level 0..12` mapping and exhaustive
stereo-mode search. Verification is trivially strong: encode then decode then compare to the
original, bit-exactly, over the whole corpus. Also verify our output decodes correctly in the
reference binary (D6).

**DSP dependencies.** `vaco-codec-golomb` (Rice), `-dsp-lpc`, `-dsp-fmtconvert`, `vaco-bitstream`.

## 4.6 AAC — decode — **30 pw** — **T3, `patent-encumbered-aac-decode`, never shipped**

Included in this section because the prompt asks for it and because it carries real engineering
weight (~21.6k lines upstream, the third-largest codec family in the inventory). **But it is not
T1.** The legal register marks AAC 🔴 RED: the Via LA AAC pool is active in 2026, and its royalty
attaches to encoder **and decoder** units — explicitly not to bitstreams. So:

- **In the default build:** AAC header parsing only (`AudioSpecificConfig`, ADTS, LATM/LOAS), which
  is what `vaco-probe` and the MP4/TS demuxers need, and remuxing, which the pool does not charge
  for. No decoder.
- **Behind `patent-encumbered-aac-decode`:** the full decoder, in-tree, CI-built, never shipped.
- **For users who need AAC playback in a shipped binary:** the system codec (AudioToolbox on
  macOS/iOS, Media Foundation on Windows, MediaCodec on Android) via §8. The OS vendor is licensed.

**Governing specification.** ISO/IEC 14496-3:2019, *Coding of audio-visual objects — Part 3:
Audio*, Subpart 4 (General Audio Coding). SBR is §4.6.18, Parametric Stereo is §8.6.4 and Annex
8.B. ADTS framing is ISO/IEC 13818-7. LATM/LOAS is 14496-3 §1.7. Levels/profiles per 14496-3
§1.6.2. **These are paid ISO documents** — budget ~CHF 400 for the parts, and note that the
conformance streams (ISO/IEC 14496-26) are a separate purchase.

**Stages.** (1) `AudioSpecificConfig`, GASpecificConfig, ADTS and LATM/LOAS framing, program
config elements, channel configuration — 3 pw. (2) Raw data block: SCE/CPE/CCE/LFE/DSE/FIL element
loop, ICS info, window sequences — 3 pw. (3) Scalefactor and spectral Huffman decode (11
codebooks, 2- and 4-tuple), inverse quantisation (`x^(4/3)`) — 4 pw. (4) Tools: M/S stereo,
intensity stereo, PNS, TNS (LPC filtering along frequency), LTP — 4 pw. (5) Filterbank: IMDCT
1024/128 (and 960/120 for the low-delay variants), window shapes (sine and KBD), block switching,
overlap-add — 3 pw. (6) SBR: the 64-band complex QMF analysis/synthesis banks, HF generation by
LPC-based patching, envelope and noise-floor adjustment, the `sbr_extension` payload — 6 pw.
(7) Parametric Stereo: the hybrid filterbank, IID/ICC/IPD/OPD parameters, decorrelation allpass
chain, stereo reconstruction — 4 pw. (8) LATM/LOAS variant decoder, error concealment, encoder
delay/`initial_padding` handling, integration, conformance — 3 pw.

**Conformance.** ISO/IEC 14496-26 conformance bitstreams for AAC-LC, HE-AAC and HE-AACv2 — the
authoritative set, purchased. AAC is not bit-exact-mandated in the same way as video: the standard
defines an RMS-error threshold against the reference decoder output. Our gate is the 14496-26
threshold plus differential testing against the reference binary. Cheap partial substitute for
early development: the ETSI/3GPP test vectors and the public HE-AAC conformance samples.

**Hot kernels.** SBR's QMF analysis/synthesis is the single largest cost in HE-AAC (roughly 40–50%
of decode time) — a 640-tap prototype filter plus a 64-point complex DCT/DST, both extremely
vectorisable and both `vaco-tx` customers. IMDCT is the largest cost in plain AAC-LC. Inverse
quantisation `x^(4/3)` is a table lookup plus fixup and vectorises well. Huffman/scalefactor decode
is scalar (`vaco-codec-vlc`). TNS is a short serial LPC filter. Windowing/overlap-add is
`dsp-sinewin`.

**Threading.** None internally — AAC frames are 1024 samples and independent apart from the
overlap-add tail. Pipeline parallelism only.

**DSP dependencies.** `vaco-tx` (IMDCT + SBR's DCT), `-dsp-sinewin`, `-dsp-fmtconvert`,
`vaco-codec-vlc`, `vaco-codec-golomb` (no), `vaco-bitstream`.

## 4.7 Vorbis — decode + encode — **8 pw / 12 pw**

**Specification.** *Vorbis I Specification* (Xiph.Org, 2020-07-04 revision). Ogg mapping in the
same document §A; RFC 5215 for RTP.

**Stages.** Header packets (identification, comment, setup) and the codebook decoder — 3 pw.
Floor 0 (LSP) and Floor 1 (piecewise-linear) — 2 pw. Residue types 0/1/2, channel coupling
(square polar mapping) — 2 pw. IMDCT + window + overlap-add, blocksize switching — 1 pw.

**Conformance.** No official bit-exact suite exists (Vorbis is not bit-exact-mandated). Gate:
differential testing against the reference binary at a tight tolerance, plus round-trip through
our encoder, plus the Xiph test samples. 🟢 GREEN patents.

**Hot kernels.** IMDCT (`vaco-tx`), floor curve synthesis, residue codebook decode
(`vaco-codec-vlc`), `dsp-fmtconvert`. **Threading:** none internally.

## 4.8 ALAC — decode + encode — **3 pw / 3 pw**

**Specification source — read this carefully.** There is no ISO/ITU spec. Apple open-sourced the
ALAC reference implementation under **Apache-2.0 in October 2011**, together with
`ALACMagicCookieDescription.txt` documenting the codec-private structure. That reference is
**not FFmpeg** and is permissively licensed, so reading it creates no clean-room problem — but
deriving code from it creates an Apache-2.0 attribution and NOTICE obligation that would travel
into our binary. **Policy: a spec writer produces a behavioural description from the Apache-2.0
reference and the magic-cookie document; the implementer works from that description.** Same
two-step as T5, but cheap, because the source we are allowed to read is small and clear.
Provenance trailer must name both documents.

🟢 GREEN — Apache-2.0 §3 gives an express patent grant from Apple.

**Stages.** Magic cookie parse; frame header; the bit-shift/uncompressed paths; adaptive FIR
prediction (the "Rice + adaptive predictor" pair); Rice/Golomb residual decode; stereo
decorrelation with the mixing coefficients; 16/20/24/32-bit paths.
**Conformance:** round-trip plus differential. **Hot kernels:** the adaptive predictor (serial
recursion with coefficient update — hard to vectorise, unroll and PGO), Rice decode
(`vaco-codec-golomb`). **Threading:** frame-independent, same as FLAC.

## 4.9 PCM — **3 pw** — and ADPCM — **5 pw**

**PCM specifications.** ITU-T G.711 (11/1988) for A-law and µ-law; everything else is byte layout
defined by the container (RIFF WAVE `WAVEFORMATEX`, QuickTime `stsd`/`sowt`, Blu-ray/DVD LPCM in
their respective system specs, AES3/SMPTE 302M for `pcm_s24daud`). 🟢 GREEN.

Implementation is one **table-driven** crate, not 38 hand-written decoders: a `PcmFormat` descriptor
(width, signedness, endianness, planar, float) drives generic conversion through
`vaco-codec-dsp-fmtconvert`. A generated table maps each `CodecId::Pcm*` to a descriptor. This is
where our "components are data, not code paths" principle pays off most visibly — FFmpeg has
~38 registration entries and a macro forest; we have one 300-line crate and a table.
**Hot kernel:** byte-swap + widen + scale, pure `dsp-fmtconvert`, SIMD priority 3.

**ADPCM specifications (standardised subset only).** ITU-T G.722 (09/2012), ITU-T G.726 (12/1990),
the IMA *Recommended Practices for Enhancing Digital Audio Compatibility* (1992) for IMA-ADPCM,
the RIFF WAVE registry entry for MS-ADPCM, the *SWF File Format Specification v19* (Adobe) for
`adpcm_swf`, and the *QuickTime File Format Specification* for `ima4`. All 🟢 GREEN.
The ~30 game-specific ADPCM variants are T4/T5.
Shared `AdpcmState` machinery with per-flavour step tables from each spec.

## 4.10 PNG / APNG — **6 pw**

**Specification.** *PNG Specification (Third Edition)*, W3C Recommendation — which now
incorporates **APNG**, `cICP`/`mDCV`/`cLLI` HDR chunks, and the EXIF chunk. Also ISO/IEC 15948:2004
and RFC 2083 for the historical baseline; RFC 1950/1951 for zlib/deflate. 🟢 GREEN.

**Stages.** Chunk layer and CRC; IHDR/PLTE/tRNS/gAMA/cHRM/sRGB/iCCP/cICP; the five filter types and
the Paeth predictor; Adam7 interlacing; bit depths 1/2/4/8/16 across all five colour types;
zlib via `flate2`+`miniz_oxide` (pure Rust, MIT/Zlib — dependency register: USE); APNG `acTL`/`fcTL`/
`fdAT` with the four dispose and two blend operations. Encoder: filter-type selection heuristics,
palette generation, compression level mapping.

**Conformance.** **PngSuite** (Willem van Schaik) — 170+ files including every legal bit
depth/colour type combination and a corrupt-file set that must be rejected without panicking.
Plus the APNG test suite. Gate: 100% correct decode, 100% clean rejection of the corrupt set.

**Hot kernels.** Unfiltering (Paeth and Average have a **serial per-pixel dependency** along the
row — this is the classic hard case; vectorise across the *bytes-per-pixel* dimension, which gives
3–8 lanes and is what everyone does), and deflate (delegated). SIMD priority 4.
**Threading:** none for a single image; APNG frames are sequential by definition.

## 4.11 JPEG / MJPEG — **10 pw**

**Specification.** ITU-T T.81 | ISO/IEC 10918-1 (baseline + extended + progressive + arithmetic).
JFIF: ITU-T T.871 | ISO/IEC 10918-5. Adobe's APP14 colour-transform marker is documented in
*Adobe Technical Note #5116*. MJPEG-A/B framing: the QuickTime File Format Specification.
Restart markers and the `DRI` semantics are T.81 §B.2.4.4. 🟢 GREEN — Forgent's campaign collapsed
in 2007 and the patent expired in October 2006.

**Stages.** Marker parsing and the segment model; Huffman table construction (`vaco-codec-vlc`);
baseline sequential decode; progressive decode (spectral selection + successive approximation —
the fiddliest part); 8-bit and 12-bit precision; the four common chroma subsamplings plus arbitrary
sampling factors; restart-marker resynchronisation; arithmetic coding (T.81 Annex D — rarely
needed, do it last or never); the encoder with quality→quant-table mapping and optimised Huffman
tables.

**Conformance.** ITU-T T.83 conformance data (paid) is authoritative. Practical gate: the
libjpeg-turbo test corpus, PngSuite-style adversarial files, and D6 differential testing at
bit-exact IDCT settings. Note that JPEG's IDCT is **not** bit-exact-mandated (T.81 Annex A.3.3
gives an accuracy requirement, not an algorithm), so differential testing needs a tolerance or a
forced "spec-exact IDCT" mode on both sides — build that mode.

**Hot kernels.** Huffman decode (scalar, multi-symbol table — `vaco-codec-vlc`), IDCT
(`dsp-idct`), upsampling and colour conversion (`vaco-scale`). SIMD priority 2 for the IDCT and
colour path.
**Threading.** Restart intervals make MJPEG **slice-threadable**; whole-image MJPEG streams are
frame-threadable trivially (each frame is independent). Do restart-interval slice threading — it
is the cheapest real win in the image codecs.

## 4.12 WebP — **8 pw**

**Specification.** *WebP Container Specification* and *WebP Lossless Bitstream Specification*
(Google, both published and stable). Lossy WebP is VP8 intra-only, i.e. RFC 6386 — so
`vaco-codec-webp` **depends on `vaco-codec-vp8`** rather than duplicating it, which is why VP8
must land first. 🟢 GREEN.

**Stages.** RIFF container and the `VP8 `/`VP8L`/`VP8X`/`ALPH`/`ANIM`/`ANMF`/`ICCP`/`EXIF`/`XMP`
chunks; lossy path (delegate to VP8 keyframe decode) plus the alpha chunk with its own filtering
and optional lossless compression; the lossless format's five transforms (predictor, colour,
subtract-green, colour-indexing) and its prefix-code/LZ77/colour-cache entropy stage; animation
with dispose/blend. Encoder: lossless only initially (the lossy encoder is the VP8 encoder).

**Conformance.** libwebp's test data plus differential testing. **Hot kernels:** the lossless
predictors (13 modes, per-pixel serial along the row like PNG), the colour cache lookup, and the
inherited VP8 kernels. **Threading:** none for a single image.

## 4.13 FFV1 — decode + encode — **14 pw**

**Specification.** RFC 9043, *FFV1 Video Coding Format Versions 0, 1, and 3* (August 2021). For
version 4, `draft-ietf-cellar-ffv1-v4` (track it; do not implement until it stabilises).

**Special clean-room note.** FFV1 originated inside FFmpeg. RFC 9043 is a proper independent
normative specification and is the *only* thing implementers may use. This is called out in the
legal register (§5.1‡) and it must be in the provenance trailer for every FFV1 PR. 🟢 GREEN.

**Stages.** Container/frame header, `configuration_record` (v3), slice structure and the slice
header with its CRC; the range coder (v1/v3) and the Golomb-Rice coder (v0/v1 `coder_type 0`);
context modelling with the quantisation tables and the state-transition table; median predictor;
plane/slice loop for RGB (with the JPEG2000-RCT reversible colour transform) and YCbCr at
8/9/10/12/14/16 bits; `slice_crc` verification; error concealment.

**Conformance.** The IETF CELLAR FFV1 test suite (`ffv1-tests`) plus round-trip encode/decode over
a broad corpus, plus differential testing against the reference binary. Gate: byte-exact.

**Hot kernels.** The range coder (scalar, hot) and the median-predictor + context-quantisation
loop (per-pixel serial, difficult to vectorise — this is FFV1's fundamental shape). The RCT and the
plane interleave are vectorizable. Realistically FFV1 is entropy-bound and our wins come from PGO
and table layout, per architecture §7.2 #8.

**Threading.** FFV1 v3 **slices are independent by design** — this is the codec's whole point for
archival multithreaded encode/decode. Slice threading gives near-linear scaling and needs no
progress primitive. Frame threading on top for the many-small-frames case.

**DSP dependencies.** `vaco-codec-golomb` (Rice), `vaco-bitstream`. No shared range coder (FFV1's
differs from CABAC and msac; it lives in the codec crate).

## 4.14 The image long tail (GIF, BMP, TIFF, PNM, TGA, QOI, EXR) — **16 pw combined**

| Codec | Specification | pw |
|---|---|---|
| GIF | *GIF89a Specification* (CompuServe, 1990); LZW patent expired 2004 | 3 |
| BMP / PCX / TGA / SGI / XWD / XBM | Microsoft `BITMAPINFOHEADER` documentation; ZSoft PCX Technical Reference; *Truevision TGA File Format Specification v2.0*; SGI `IMAGE` format spec; X11 XWD/XBM headers | 4 |
| PNM | *Netpbm format specifications* (pbm/pgm/ppm/pam/pfm man pages) | 1 |
| QOI | *The Quite OK Image Format Specification* (Dominic Szablewski, one page) | 0.5 |
| TIFF | *TIFF Revision 6.0* (Adobe, 1992) + TIFF Technical Notes; the codec set (PackBits, LZW, Deflate, CCITT G3/G4 per ITU-T T.4/T.6, JPEG-in-TIFF) is most of the work | 5 |
| OpenEXR | *OpenEXR File Layout* and *Technical Introduction* (ASWF); PIZ/ZIP/RLE/PXR24/B44/DWA compression | 3 |

All 🟢 GREEN. All are single-contributor work packages with no shared-DSP dependencies beyond
`vaco-scale` for pixel-format output — ideal onboarding tasks.

## 4.15 T1 effort roll-up

| Item | pw |
|---|---|
| `vaco-codec-core` (traits, threading primitives, descriptors, registry seam) | 6 |
| Shared DSP + entropy + CBS crates (§2, excluding `mpegvideo` which is T2) | 69 |
| AV1 decode | 70 |
| VP9 decode + encode | 48 |
| VP8 decode + encode | 22 |
| FFV1 decode + encode | 14 |
| Opus decode + encode | 36 |
| Vorbis decode + encode | 20 |
| FLAC decode + encode | 12 |
| ALAC decode + encode | 6 |
| PCM + ADPCM (standardised) | 8 |
| PNG/APNG | 6 |
| JPEG/MJPEG | 10 |
| WebP | 8 |
| Image long tail | 16 |
| Raw/uncompressed video | 4 |
| Text subtitles | 6 |
| v0.1 parsers (§6) | 10 |
| v0.5 bitstream filters (§6) | 12 |
| **Total** | **~383 pw ≈ 7.4 person-years** |

Deferring the encoders that have credible permissive alternatives (VP9 encode, Vorbis encode,
Opus encode) removes ~54 pw and gets the T1 decode surface to **~330 pw**.

> **§4.15's totals are the "write everything ourselves" number.** §4A adjusts them for D10/D11
> build-or-buy. Read the two together.

---

# 4A. Build or buy — which backend do we start with

D10 admits external crates through three gates (pure Rust / permissive licence / trusted and
maintained). D11 says every one of them lives behind exactly one `vaco-codec-*` crate exposing only
our traits over our types, carries a measured fidelity grade, and can be swapped by rewriting that
one crate's internals.

**Framing, per D11: this section chooses a starting backend, not a permanent one.** The architecture
is identical either way — `vaco-codec-flac` implements `Decoder` over `Frame` whether its internals
are `claxon` or ours. What changes is which work package we run first and what we measure.

## 4A.1 What "Exact" can even mean — a taxonomy that has to come first

The fidelity grades in D11 are only meaningful against a codec class. Grading a lossy encoder
"Divergent" for not being byte-identical would be a category error.

| Class | Examples | Best achievable grade | Comparison mode |
|---|---|---|---|
| **Lossless decode** | FLAC, ALAC, PNG, FFV1, WebP-lossless, GIF, TIFF, QOI, PCM | **Exact** — required | Byte-identical decoded output. Anything else is a bug in one of the two implementations. |
| **Bit-exact-specified lossy decode** | AV1, VP8, VP9, H.264, HEVC, VVC, AAC-fixed | **Exact** — required | Byte-identical decoded output. The spec defines the reconstruction to the bit; a mismatch is a conformance failure. |
| **Tolerance-specified lossy decode** | JPEG (T.81 A.3.3 gives an accuracy bound, not an algorithm), MP3, AAC float, Vorbis, Opus, AC-3 | **Equivalent** | Within the spec's stated accuracy bound, plus a documented tolerance. Byte-exactness is achievable only by matching the reference's *choice* of transform, which is not required and not always desirable. |
| **Lossless encode** | FLAC, PNG, FFV1, ALAC | **Exact** possible but *not* guaranteed | The decoded result is bit-identical by definition; the *compressed bytes* differ whenever the encoder makes different (equally legal) choices. Compare decoded output byte-exactly; compare compressed size against a regression baseline. |
| **Lossy encode** | AV1, VP8/VP9, Opus, Vorbis, AAC, MP3, MPEG-2 | **Never Exact, by construction** | Encoders are not specified — only decoders are. `rav1e` will never produce libaom's bitstream, and our own encoder will never produce either. The harness must switch to **quality-based** comparison: decode both outputs, compare PSNR/SSIM/VMAF (video) or an `opus_compare`-style spectral metric (audio) at matched bitrate, against a committed regression baseline. |

**This must be built into `vaco-conformance` before the first encoder lands**, otherwise every lossy
encoder will be permanently "Unmeasured" and therefore unshippable under D11. It is work package
X-04 in §7 and it is on the critical path.

## 4A.2 The assessment

Gate columns: **G1** pure Rust / no FFI, **G2** licence, **G3** trusted+maintained.
"Fit" is D10's judgement call: can it produce our `Frame` without a copy, can we drive it
incrementally, does it cover what we need.

### Video

| Codec | Candidate crate | G1 | G2 | G3 | Fit | Start with | Predicted grade | What would drive it Divergent |
|---|---|---|---|---|---|---|---|---|
| AV1 **decode** | — | — | — | — | — | **Native** | Exact (required) | No candidate. `rav1d` is a machine translation of dav1d: pervasive `unsafe`, not independently maintained, not plausibly forkable → fails G3 decisively. |
| AV1 **encode** | `rav1e` | ⚠ | ✅ BSD-2 | ✅ | ⚠ | **External** | **Equivalent by definition — never Exact** | ⚠ **G1 caveat, and it is load-bearing:** rav1e's *default* features build hand-written x86/aarch64 assembly through a `nasm`/`cc` build script. That is compiling native code, which fails D10 Gate 1. We must take it with `default-features = false` (no `asm`), which costs **2–4× encode speed**. It also uses `unsafe` heavily even without asm — D10's stated tension, in its sharpest form. Adopt with that written into the adoption record. |
| VP8 / VP9 **decode** | — | — | — | — | — | **Native** | Exact (required) | Nothing pure-Rust and production-grade exists. |
| VP8 / VP9 **encode** | — | — | — | — | — | **Native** | Never Exact | Same. |
| FFV1 | — | — | — | — | — | **Native** | Exact (required) | Nothing exists. RFC 9043 is clear and the codec is ours to write. |
| H.264 / HEVC / VVC | — | — | — | — | — | **Native, T3** | Exact (required) | Nothing pure-Rust exists, and openh264 is FFI (G1) with no patent cover for source builds anyway. §5. |
| JPEG XL **decode** | `jxl-oxide` | ✅ | ✅ MIT/Apache | ✅ actively maintained, real adoption, notably low unsafe | ✅ | **External** | Equivalent (VarDCT is float; Modular lossless should be Exact) | Colour-management differences (`cICP`/ICC handling), progressive/partial decode semantics, animation. **This is the single largest saving in the plan: ~40 pw avoided.** |
| PNG | `png` (image-rs) | ✅ | ✅ MIT/Apache | ✅ very widely adopted, active, shallow tree | ✅ decodes into a caller-supplied buffer, so no extra copy into `Frame` | **External** (decode **and** encode) | Decode **Exact**; encode **Equivalent** (deflate stream differs) | Decode: 16-bit endianness, Adam7 corners, APNG dispose/blend, how `gAMA`/`cHRM`/`sRGB`/`cICP`/`iCCP` map onto our `ColorInfo`. Encode: byte comparison is the wrong test — compare decoded pixels exactly and compressed size against a baseline. |
| JPEG / MJPEG **decode** | `zune-jpeg` | ✅ | ✅ MIT/Apache/Zlib | ✅ maintained, fast, adopted; uses unsafe for SIMD | ⚠ still images only | **External for still JPEG, native for MJPEG** | Equivalent | ⚠ **Highest Divergent risk in the set.** No MJPEG-A/B framing, no 12-bit, no arithmetic coding, no lossless JPEG, no CMYK/YCCK Adobe-transform parity guarantee, and no "spec-exact IDCT" mode — which we need on *both* sides to make differential testing meaningful at all. Because MJPEG is a video codec we must have regardless, **the native JPEG implementation is scheduled inside year one** and `zune-jpeg` is a bridge, not a destination. |
| JPEG **encode** | `jpeg-encoder` | ✅ | ✅ MIT | ⚠ smaller, single-maintainer | ⚠ | **Native** | Never Exact | Quantisation-table and Huffman-optimisation choices differ; and we need MJPEG encode with container-specific framing anyway. |
| WebP | `image-webp` | ✅ | ✅ MIT/Apache | ✅ image-rs org, active | ✅ | **External initially, native lossy once our VP8 lands** | Exact (both paths are integer-exact) | Animation compositing (`ANMF` dispose/blend), `ALPH` filtering modes, `VP8X` canvas semantics. Once `vaco-codec-vp8` exists, routing lossy WebP through it removes the duplicate VP8 and is strictly better. |
| GIF | `gif` (image-rs) | ✅ | ✅ MIT/Apache | ✅ | ✅ | **External** (decode + encode) | Exact | Frame-compositing policy: FFmpeg's decoder composites onto a canvas and applies disposal itself. If `gif` hands us raw sub-frames, the *pipeline* must composite identically — that is our bug to fix, not the crate's. |
| TIFF | `tiff` (image-rs) | ✅ | ⚠ **MIT only**, not dual — allowed by D3 but note the asymmetry | ✅ | ⚠ **incomplete** | **External, with native gap-filling scheduled** | Exact for what it covers | Coverage, not correctness: CCITT G3/G4 (ITU-T T.4/T.6), JPEG-in-TIFF, some tiled and planar configurations, BigTIFF. A file it rejects is not "Divergent", it is missing — but it is missing to the user all the same. |
| OpenEXR | `exr` crate | ✅ | ✅ BSD-3 | ✅ maintained, well documented | ✅ | **External** | Exact for ZIP/RLE/PIZ; Equivalent for lossy DWA | Missing compression methods (B44/B44A/DWAA/DWAB coverage), deep-image and multi-part files, tile ordering. |
| AVIF | `ravif` (encode) | ✅ (drags rav1e) | ✅ BSD-3 | ✅ | ✅ | **Container native, encode via the AV1 backend, decode via our AV1 decoder** | Never Exact (encode) | AVIF is AV1 in a HEIF-derived container. The container is ours; the payload follows AV1's story exactly. |
| QOI, PNM, BMP, PCX, TGA, SGI, XWD, XBM | (`image` has them) | ✅ | ✅ | ✅ | ❌ coupled to `image`'s own model | **Native** | Exact | Each is 0.5–1 pw. Taking a dependency to save a day, and then owning the D11 boundary crate plus the adoption record for it, is a net loss. |

### Audio

| Codec | Candidate crate | G1 | G2 | G3 | Fit | Start with | Predicted grade | What would drive it Divergent |
|---|---|---|---|---|---|---|---|---|
| FLAC **decode** | `claxon` | ✅ | ✅ Apache-2.0 | ⚠ **alive** is marginal — low recent activity — but small, exceptionally well documented, plainly forkable, and safe Rust | ✅ drivable incrementally via its frame reader | **External** | **Exact** (lossless — anything else is a bug) | 32-bit-per-sample streams, the `uncommon/` non-subset configurations, and behaviour on the `faulty/` corpus (must be a clean `Err`, never a panic — a panic from a dependency is a D11 defect we own). Also: no seektable-driven seek API may mean we drive seeking ourselves. |
| FLAC **encode** | — | — | — | — | — | **Native** | Decoded output Exact; compressed bytes Equivalent | `claxon` has no encoder. This is the textbook "a codec need not be all-or-nothing" case: `vaco-codec-flac` wraps for decode and implements for encode, behind one API. |
| ALAC **decode** | `alac` (ebarnard) | ✅ | ✅ MIT/Apache | ⚠ tiny, low adoption, quiet | ✅ | **Native** (3 pw) — but adopt the crate as a **dev-dependency differential oracle** | Exact | Cheaper to write than to own the adoption record for. Using a second independent implementation as a *test* oracle is high value and carries none of D11's shipping obligations. Apply this pattern wherever a small crate exists and we build anyway. |
| Vorbis **decode** | `lewton` | ✅ | ✅ MIT/Apache | ⚠ quiet | ❌ **capability gap: no Floor 0** | **Native** | Equivalent | A decoder that rejects legal streams cannot be our only backend. Keep `lewton` as a dev-dependency oracle for Floor-1 content. |
| Vorbis **encode** | — | — | — | — | — | **Native** | Never Exact | Nothing exists. |
| Opus **decode/encode** | `opus-decoder`, `opus_rs` | ✅ | ⚠ verify | ❌ young, 0.x, negligible adoption — fails G3 | — | **Native** | Equivalent (RFC 6716 is not bit-exact-mandated) | The strongest argument for native in the whole document: Opus is our best audio codec legally, it is the one every user wants, and **no adoptable implementation exists in either direction**. |
| MP3 **decode** | `puremp3` | ✅ | ✅ MIT/CC0 | ❌ 0.1.x, incomplete, unmaintained | ❌ | **Native** (T2) | Equivalent | Note the useful oddity from the licence register: **minimp3's C source is CC0**, i.e. public domain, so it may be *read* freely without any clean-room concern. It is FFI so it can never be a dependency, but as a specification aid for MP3 it is legitimate and worth using. |
| AAC, AC-3, DTS | — | — | — | — | — | **Native, T3** | Equivalent | Nothing exists, and all three are gated on patents anyway. |
| PCM, ADPCM | — | — | — | — | — | **Native** | Exact | Table-driven, trivial, and the semantics are container-specific in ways no general crate models. |

### Infrastructure (not codecs, but they gate codecs)

| Job | Candidate | Verdict |
|---|---|---|
| deflate/zlib (PNG, FFV1-adjacent, Matroska, TIFF) | `miniz_oxide` (+ `flate2` **pinned to `rust_backend`** — its default feature set can select a C backend, which would fail Gate 1) | **Adopt.** Widely used, pure Rust, shallow. |
| FFT / MDCT / DCT (`vaco-tx`) | `rustfft`, `realfft` | **Build ours.** D10 names this case explicitly: they offer no bit-exact `i32` fixed-point path, which AAC-fixed, AC-3-fixed and several conformance modes require. Both also use `unsafe` heavily for SIMD. |
| Data parallelism | `rayon` | **Adopt** for slice/tile threading and filter data parallelism. |
| Matroska/MP4 parsing | `matroska` crate; `mp4parse` | `matroska` is metadata-oriented, not a demuxer we can drive; `mp4parse` is MPL (Gate 2). **Build ours** — that is the container plan's problem, not this document's. |

## 4A.3 Two rules that fall out of D11 and should be written into CI

1. **Depend on format crates, never on the `image` umbrella.** `image` re-exports a dozen format
   crates behind its own `DynamicImage` model. Depending on it would (a) put several media crates
   into one `Cargo.toml`, which is exactly what D11's single-owner CI check forbids, (b) force a
   copy through a foreign image type on every decode, and (c) deepen the tree considerably. Take
   `png`, `gif`, `tiff`, `image-webp`, `jxl-oxide`, `exr` individually, each owned by exactly one
   `vaco-codec-*` crate.
2. **Dev-dependency oracles are encouraged and are not subject to the single-owner rule.** A second
   independent implementation used only in tests is one of the cheapest correctness tools available
   (it triangulates: ours vs. the crate vs. the reference binary — D11's three-way comparison,
   for free). CI distinguishes `[dependencies]` from `[dev-dependencies]` when enforcing the rule.

## 4A.4 The honest summary

The surviving crate set is genuinely capable, and it is nowhere near complete.

**What buying actually covers:** JPEG XL decode, PNG, GIF, TIFF, EXR, WebP, still-JPEG decode, FLAC
decode, and an AV1 encoder that has to run with its assembly disabled. That is the image periphery
plus two audio decoders plus one encoder.

**What buying does not cover, at all:** every video decoder that matters (AV1, VP8, VP9, and — behind
feature gates — H.264, HEVC, VVC), every video encoder except AV1, every audio codec of consequence
(Opus, Vorbis, AAC, AC-3, MP3, ALAC, PCM, ADPCM), FFV1, all of the shared DSP in §2, `vaco-tx`, all
parsers, all bitstream filters, and all of `vaco-codec-core`. **The core is ours.**

**Adjusted effort.** Buying removes ~46 pw of first-pass implementation (JPEG XL 40, PNG 6, GIF 3,
EXR 3, TIFF 5, WebP 8, still-JPEG 5, FLAC decode 3, AV1 encode 90 — of which JPEG XL and AV1 encode
were never in the T1 total anyway, so the T1 saving is ~33 pw). It adds ~12 pw of D11 boundary
crates, backend features, adoption records and fidelity measurement, plus a standing replacement
obligation for anything graded Divergent. **Net first-year saving against §4.15's 383 pw: ~34 pw,
about 9%.** The number is small because the expensive things are the ones nobody has written.

**Sequencing rule, per D11: schedule native implementations by expected grade.** Anything predicted
Divergent, or Equivalent-with-known-gaps, gets its native replacement scheduled early:

| Order | Native implementation | Why it cannot wait |
|---|---|---|
| 1 | JPEG / MJPEG | `zune-jpeg` has no MJPEG framing, no 12-bit, no spec-exact IDCT mode. MJPEG is a video codec we need in year one. |
| 2 | TIFF gap-filling (CCITT G3/G4, JPEG-in-TIFF) | Coverage holes are user-visible failures. |
| 3 | WebP lossy → route through our VP8 | Removes a duplicate VP8 implementation the moment ours exists. |
| 4 | FLAC encode | No candidate exists; and it completes the crate that already wraps for decode. |
| 5 | PNG encode | Only if the size-regression baseline shows `png`'s output materially worse than the reference; decode stays external either way. |
| 6 | AV1 encode | Only when rav1e-without-asm's speed becomes the binding constraint. Correctness is not the issue; throughput is. |

---

# 5. The H.264 / HEVC question

## 5.1 The situation, stated precisely

Four facts, from the research, that together define the problem:

1. **FFmpeg has no native software encoder for either codec.** Every `h264_*` and `hevc_*` encoder
   registration is a hardware API wrapper or a wrapper around x264/x265. There is no reference
   implementation to work from even if we wanted one (research §4.6).
2. **x264 and x265 are GPL-2.0-or-later.** Denied outright by D3, and their Rust bindings declare
   MIT while statically linking GPL — the exact metadata lie D9 warns about.
3. **openh264 is no longer an option at all.** It is a C++ library, so D10 Gate 1 excludes its
   bindings on FFI grounds before the patent question even arises. And the patent question is fatal
   anyway: **Cisco's royalty payment covers Cisco's own precompiled binaries, not source builds.**
   Building openh264 from source gets you BSD-2 and zero patent cover. The Firefox-style
   "download Cisco's binary at runtime" pattern would technically work, but it means shipping a
   product that fetches and executes an unverifiable third-party binary — a supply-chain posture we
   should not adopt, and one that is FFI at the moment of use regardless.
4. **The legal verdicts differ between the two codecs and this matters.** H.264 is 🟡 AMBER: the Via
   LA pool is active with a tail to roughly 2027–28, there is a real 100,000-unit/year free tier,
   and there is one pool to deal with. HEVC is 🔴 RED and unmitigable: Access Advance now
   administers the ex-Via LA pool as well as its own, consolidation is incomplete, Sisvel is
   separate, and unpooled holders (Dolby's GE-heritage portfolio) are actively seeking injunctions.
   Paying one pool does not stop another suing. **No structure fixes HEVC.**

And one asymmetry that runs through everything below: **decoders are tractable and encoders are
not.** A decoder implements exactly what the specification mandates — 800 pages of unambiguous
normative pseudocode, a strong *scènes à faire* position on the copyright side, and a conformance
suite that tells you objectively when you are done. An encoder implements none of that: rate
control, mode decision, motion search and psychovisual tuning are unspecified engineering, they are
where the newer patents cluster (research §2.4.2), and there is no objective finish line.

## 5.2 The options, with costs

### Option A — Decoders from the ITU-T specs; no software encoder, ever

Implement H.264 and HEVC decoders in-tree behind `patent-encumbered-h264-decode` and
`patent-encumbered-hevc-decode`. Never in `default`, never in `full-rf`, never in a published
binary. CI asserts absence both by compiled-feature list and by the `Caps::PATENT_ENCUMBERED`
runtime check (§1.3).

- **Specifications.** ITU-T H.264 (latest edition) | ISO/IEC 14496-10 — Annexes A (profiles/levels),
  B (byte stream), D (SEI), E (VUI), G (SVC) and H (MVC) as needed. ITU-T H.265 | ISO/IEC 23008-2 —
  Annexes A, B, D, E, and the RExt/SCC profile annexes.
- **Conformance.** ITU-T/ISO JVT conformance bitstream sets for H.264 and JCT-VC's for HEVC. Both
  are publicly downloadable, comprehensive, and come with expected reconstructions. This is one of
  the best-supported conformance situations of any codec.
- **Cost.** H.264 ~60 pw (research: ~19.8k LOC upstream). HEVC ~55 pw (~13.7k LOC, but a denser
  spec — CTUs, tiles, WPP, SAO, the RPS model). VVC ~110 pw (~22.9k LOC, the largest single item in
  the entire inventory).
- **Value even though we never ship it.** Three concrete uses: it validates the parse-half we *do*
  ship for hardware acceleration (§8); it gives developers in licensed contexts and in jurisdictions
  without software patents a working build; and it is the reference against which the hardware paths
  are differentially tested.

### Option B — Write encoders from scratch and accept the exposure

- **Cost.** A *correct* H.264 encoder (Baseline/Main, CAVLC+CABAC, sensible rate control) is ~40 pw.
  A *competitive* one — the thing users actually mean when they say "H.264 encode" — is 150–250 pw
  and will still lose to x264, which has had fifteen years of psychovisual tuning by people who do
  nothing else. HEVC is worse: x265 is roughly 50% larger again, and 200–300 pw is optimistic.
- **Patent exposure.** Maximal and unmitigated. Encoders are where the money and the enforcement
  are; encoder patents are newer than bitstream-syntax patents so the tail outlives the decoder's;
  and for HEVC there is no single counterparty who could sell us peace even if we wanted to buy it.
- **Timing.** AVC's essential patents largely lapse around 2027–28. An encoder that takes three
  years to become competitive arrives exactly as the codec stops mattering commercially, in a market
  that by then cares about AV1 and AV2.
- **Verdict: no.** This is 200–500 person-weeks spent to produce something worse than a free
  alternative, in a legal position we cannot defend, on a schedule that lands after the window
  closes.

### Option C — Hardware and system-codec encode only

`vaco-hw-videotoolbox`, `-vaapi`, `-d3d11`/Media Foundation, `-nvenc`, `-amf`, MediaCodec. The user's
silicon already carries the licence; our binary contains no software implementation of either codec.
D9 upgrades this from "nice to have" to **strategic**, and it is the reason §8 is not an appendix.

- **Cost.** `vaco-hw-core` ~8 pw, then ~6 pw per backend for decode + ~5 pw for encode.
- **Limits, stated honestly.** Coverage is platform-dependent; quality is fixed by the vendor and
  is generally below x264 at the same bitrate; headless Linux servers and CI machines frequently
  have no usable device; and counsel should confirm that facilitating a licensed hardware encoder
  creates no inducement exposure under 35 U.S.C. § 271(b)/(c) — that is the legal register's
  open question Q5.

### Option D — `exec` the user's own installed x264 / x265 binary

A `vaco-codec-exec` backend that spawns a user-installed encoder, pipes raw frames in and reads the
elementary stream out, mapping our option surface onto its CLI.

- **Cost.** ~4 pw for the generic mechanism (process management, pipe plumbing, timeout and error
  handling, `-y4m`/rawvideo framing, progress parsing), plus ~2 pw per tool for argument mapping.
- **Why it works.** The process boundary solves both problems at once, which is precisely why the
  legal register recommends it: no GPL code enters our tree or our binary, and the patent posture is
  the user's, arising from software they chose to install. It is also not FFI — no linking, no
  shared address space, no D10 Gate 1 issue.
- **Bonus.** The same mechanism is the escape hatch for everything in T5 (§3.5) and for any codec a
  user has a tool for and we do not.

## 5.3 Recommendation

**Adopt A + C + D. Reject B unconditionally.**

1. **Ship the parsing, never the decoding.** `vaco-parse-h2645`, `vaco-cbs-h2645` and the
   `h264_*`/`hevc_*` bitstream filters go in the **default build**. Parsing an SPS is not decoding:
   it implements no codec, it is what `vaco-probe` and the MP4/TS demuxers need, and it is what
   remuxing needs — and the pools charge on codec units, not on bitstreams. This makes
   `vaco -i in.mp4 -c copy out.mkv` on an H.264/AAC file work perfectly in a binary that contains
   no H.264 decoder and no AAC decoder.
2. **Make hardware acceleration a first-class, early deliverable, not a v2 feature.** It is the only
   way our users decode and encode H.264/HEVC from a binary we publish. Sequence `vaco-hw-core` plus
   VideoToolbox and VA-API alongside the T1 codec work, not after it.
3. **Implement the H.264 and HEVC decoders in-tree behind `patent-encumbered-*-decode`**, after the
   T1 set, in that order. 115 pw combined. They are never shipped, but they are built, tested,
   fuzzed and conformance-gated in CI, and they are what validates the hardware parse-half.
4. **Defer VVC entirely** until after v1.0. 110 pw for a codec that is 🔴 RED, has negligible
   deployment, and whose hardware support is thin. If it is ever built, it is built by someone who
   specifically wants it.
5. **Never write a software H.264, HEVC or VVC encoder.** Put that budget into AV1 and VP9 encoders
   instead, where the output is royalty-free, shippable, and improving in relevance rather than
   declining.
6. **Build `vaco-codec-exec` early** — 4 pw for a mechanism that answers "why can't vaco encode
   H.264?" with "install x264 and it will", and simultaneously gives T5 a story.

The uncomfortable part, stated plainly so nobody is surprised later: **`vaco` will not encode H.264
or HEVC in software, ever.** For a tool positioning itself as an ffmpeg replacement that is the most
visible functional gap in the entire project, and no amount of engineering removes it — it is a
legal constraint, not a technical one. The mitigations (hardware, system codecs, `exec`) cover most
real users most of the time, and `docs/why-some-codecs-are-not-included.md` has to explain the rest
honestly. The legal register is right that this document is the most valuable thing the project can
write, and it is right that FFmpeg has never written it well.

---

# 6. Parsers and bitstream filters

## 6.1 Crate decomposition

The `Parser` and `BitstreamFilter` traits live in `vaco-codec-core` (§1.6, §1.7). Implementations
are grouped by **syntax family**, because that is what actually shares code — a per-parser crate
would be 66 crates sharing three tables.

| Crate | Contents | Depends on |
|---|---|---|
| `vaco-parse-h2645` | H.264, HEVC, VVC: Annex-B start-code scanning, NAL framing, AVCC/HVCC/VVCC length-prefix framing, parameter-set tracking, picture-boundary detection, POC computation, SEI extraction, field/frame and reorder-depth reporting | `vaco-cbs-h2645`, `vaco-bitstream` |
| `vaco-parse-av1` | OBU and temporal-unit framing, Annex-B vs low-overhead detection, sequence-header extraction, `show_frame` and `show_existing_frame` handling | `vaco-cbs-av1` |
| `vaco-parse-vpx` | VP8, VP9 (incl. superframe index), VP3/Theora | `vaco-cbs-vp9` |
| `vaco-parse-mpegaudio` | MP1/2/3 frame sync, AAC ADTS, AAC LATM/LOAS, AC-3/E-AC-3 sync, DTS sync — all of them "find the frame boundary and read the header" | `vaco-bitstream` |
| `vaco-parse-mpegvideo` | MPEG-1/2 video, MPEG-4 Part 2, H.261, H.263, VC-1, CAVS, DV | `vaco-bitstream` |
| `vaco-parse-audio-misc` | FLAC (CRC-based resync), Opus, Vorbis, MLP/TrueHD, TAK, SBC, ALAC, AMR, G.7xx | |
| `vaco-parse-image` | PNG, JPEG/MJPEG, GIF, BMP, WebP, PNM, DPX, QOI, HDR, XBM, XWD, JPEG 2000, JPEG XL — framing for `image2` and piped elementary streams | |
| `vaco-parse-legacy` | The T4/T5 long tail | |

| Crate | Contents |
|---|---|
| `vaco-bsf-core` | The `BitstreamFilter` trait's runtime: the chain type, option parsing, the `null` filter, and the packet-queue plumbing |
| `vaco-bsf-h2645` | `h264_mp4toannexb`, `hevc_mp4toannexb`, `vvc_mp4toannexb`, `h264_metadata`, `hevc_metadata`, `vvc_metadata`, `h264_redundant_pps`, `dovi_rpu`, `dovi_split` |
| `vaco-bsf-av1` | `av1_metadata`, `av1_frame_split`, `av1_frame_merge` |
| `vaco-bsf-vpx` | `vp9_metadata`, `vp9_superframe`, `vp9_superframe_split`, `vp9_raw_reorder` |
| `vaco-bsf-audio` | `aac_adtstoasc`, `eac3_core`, `dca_core`, `truehd_core`, `opus_metadata`, `pcm_rechunk`, `mp3_header_*` |
| `vaco-bsf-generic` | `null`, `noise`, `chomp`, `setts`, `dts2pts`, `extract_extradata`, `dump_extradata`, `remove_extradata`, `filter_units`, `trace_headers`, `showinfo` — the CBS-driven ones are generic across families |
| `vaco-bsf-subtitle` | `mov2textsub`, `text2movsub`, `pgs_frame_merge`, `eia608_to_smpte436m`, `smpte436m_to_eia608` |
| `vaco-bsf-legacy` | `mjpeg2jpeg`, `mjpega_dump_header`, `imx_dump_header`, `media100_to_mjpegb`, `mpeg4_unpack_bframes`, `mpeg2_metadata`, `hapqa_extract`, `dv_error_marker` |

## 6.2 What v0.1 needs — and why it is legal to ship

D5's milestone is `ffprobe` on MP4/MOV, Matroska/WebM and MPEG-TS, parsing H.264/HEVC/AV1/AAC/Opus
stream headers, with byte-identical writer output. That requires **parsing only, and no decoding at
all** — which is exactly the distinction that lets the encumbered formats ship in the default build.

| v0.1 component | Why | pw |
|---|---|---|
| `vaco-codec-core` — `CodecParameters`, `CodecId`, `Profile`/`Level`, the `Parser` trait, descriptors | Everything else hangs off it | 4 |
| `vaco-cbs-core` + `vaco-cbs-h2645` (read path only) | SPS/PPS/VPS and SEI syntax; RBSP emulation-prevention removal | 6 |
| `vaco-cbs-av1` (read path only) | Sequence header OBU, `av1C` | 2 |
| `vaco-parse-h2645` (header subset) | H.264/HEVC profile, level, resolution, SAR, field order, colour info, reorder depth | 3 |
| `vaco-parse-av1` (header subset) | AV1 profile, level, tier, resolution, colour info, `still_picture` | 1 |
| `vaco-parse-mpegaudio` (ASC/ADTS/LATM subset) | AAC object type, sample rate, channel configuration, SBR/PS signalling | 2 |
| `vaco-parse-audio-misc` (Opus/Vorbis/FLAC header subset) | `OpusHead` pre-skip and gain, Vorbis identification header, `STREAMINFO` | 1 |
| Profile/level name tables for the five codecs | `-show_streams` prints them | 1 |
| **v0.1 total** | | **20 pw** |

Three legal notes that belong in the code, not just here:

- **Parsing is not decoding.** These crates implement no reconstruction process, produce no samples,
  and are not "a decoder" under any pool's definition of a unit. They ship in the default build.
  Each carries a crate-level doc comment saying so and naming D4 and D9.
- **They do not get `Caps::PATENT_ENCUMBERED`**, because they are not codec implementations.
- **The provenance trailer still applies**: ITU-T H.264 §7.3.2 for the SPS, ISO/IEC 14496-3 §1.6.2
  for the ASC, and so on.

## 6.3 What comes later

- **v0.5 (remux)**: `vaco-bsf-core`, `vaco-bsf-generic` (`extract_extradata`, `dump_extradata`,
  `remove_extradata`, `filter_units`, `setts`, `null`, `noise`), `vaco-bsf-h2645`
  (`*_mp4toannexb` — mandatory for MP4↔TS↔MKV round-tripping), `vaco-bsf-audio`
  (`aac_adtstoasc` — mandatory for TS→MP4), `vaco-bsf-av1` and `vaco-bsf-vpx` (superframe and
  temporal-unit handling, mandatory for WebM↔MP4). **~12 pw.** Without these, remuxing the formats
  people actually have simply does not work.
- **v0.9 (full CLI)**: `*_metadata` filters, `trace_headers`, `dts2pts`, `pcm_rechunk`, the subtitle
  BSFs. These need the CBS **write** path, which roughly doubles `vaco-cbs-h2645`'s cost — budget
  a further 6 pw there.
- **Post-v1.0**: `vaco-bsf-legacy`, `dovi_*`, the LCEVC and EVC filters.

---

# 7. Parallelisable work breakdown

Every package below is independently assignable: it has a named owner-sized scope, an explicit
dependency list, a definition of done, and it does not require its owner to coordinate with anyone
outside its dependencies. This is the mechanism that lets dozens of contributors work at once.

**Definition of done, uniformly:** implementation + unit tests + a `cargo-fuzz` target + a scalar
reference and `vaco-checkasm` differential test for every kernel + criterion bench + the `docs/`
page required by the repository standard + conformance pass at the stated gate + provenance trailer
naming the specification + (for wrapped codecs) a fidelity grade recorded in `docs/codec-status.md`.

## 7.1 Wave 0 — foundations (blocks nearly everything; ~5 people, ~6 weeks)

| ID | Package | Deps | pw | Notes |
|---|---|---|---|---|
| F-01 | `vaco-codec-core`: `CodecId` codegen, `CodecParameters`, `Profile`/`Level`, `Caps`, descriptors | — | 3 | Blocks everything. Do first, review hard, then freeze the surface. |
| F-02 | `vaco-codec-core`: `Decoder`/`Encoder`/`Parser`/`BitstreamFilter` traits and the send/receive state machine | F-01 | 2 | Includes the state-machine conformance test every implementation must pass. |
| F-03 | `ProgressPicture` / `PictureWriter` / `PictureRef` / `PlaneView` (§1.8) | F-01 | 4 | **The highest-risk design item in the document.** Land it with a synthetic benchmark measuring band-straddle cost before any codec depends on it. |
| F-04 | Registry codegen + feature-model wiring + the `PATENT_ENCUMBERED` CI assertion | F-01 | 2 | |
| X-01 | `vaco-conformance`: differential harness core (byte, framecrc, framemd5, structured-metadata diff) | F-02 | 4 | D6. |
| X-02 | `vaco-checkasm`: kernel differential tester + cycle benchmarking | — | 3 | Clean-room equivalent of checkasm, whose FFmpeg implementation is GPL. |
| X-03 | Fuzz scaffolding: shared `arbitrary` generators, corpus fetch/minimise, CI wiring | — | 2 | |
| X-04 | **Quality-based comparison modes** (PSNR/SSIM/VMAF video, spectral metric audio) for §4A.1 | X-01 | 3 | **On the critical path**: without it every lossy encoder is permanently "Unmeasured" and therefore unshippable under D11. |
| X-05 | `vaco-corpus`: conformance-suite fetching (Argon, vp9/vp8 vectors, flac-test-files, PngSuite, JVT/JCT-VC) | X-03 | 2 | |
| X-06 | D11 CI checks: single-owner rule for third-party media crates; `cargo-geiger` report; adoption records | F-04 | 1 | |

## 7.2 Wave 1 — shared DSP and entropy (mostly parallel; ~10 people)

| ID | Package | Deps | pw | Parallel? |
|---|---|---|---|---|
| D-01 | `vaco-codec-vlc` | F-01 | 3 | yes |
| D-02 | `vaco-codec-golomb` (Exp-Golomb + Rice, read + write) | F-01 | 2 | yes |
| D-03 | `vaco-codec-cabac` (engine only) | F-01 | 4 | yes |
| D-04 | `vaco-codec-msac` (AV1/VP9 multi-symbol + VP8 bool) | F-01 | 3 | yes |
| D-05 | `vaco-codec-dsp-fmtconvert` | F-01, X-02 | 2 | yes — **do early, every audio codec needs it** |
| D-06 | `vaco-codec-dsp-sinewin` | F-01, X-02 | 1 | yes |
| D-07 | `vaco-codec-dsp-lpc` | F-01, X-02 | 3 | yes |
| D-08 | `vaco-codec-dsp-mc` (generic separable FIR, const-generic taps) | F-01, F-03, X-02 | 8 | yes — **largest SIMD payoff; start early** |
| D-09 | `vaco-codec-dsp-intrapred` | F-01, X-02 | 6 | yes |
| D-10 | `vaco-codec-dsp-deblock` | F-01, X-02 | 6 | yes — gate the design on a measurement spike first |
| D-11 | `vaco-codec-dsp-idct` (+ blockdsp/pixblockdsp) | F-01, X-02 | 5 | yes |
| D-12 | `vaco-codec-dsp-mecmp` | F-01, X-02 | 4 | yes |
| D-13 | `vaco-codec-dsp-me` (search patterns) | D-12 | 5 | after D-12 |
| D-14 | `vaco-codec-dsp-ratecontrol` | F-01, `vaco-expr` | 5 | yes |
| D-15 | `vaco-codec-dsp-dwt` | F-01, X-02 | 4 | yes (T2 only) |
| D-16 | `vaco-tx` — FFT/MDCT/RDFT/DCT incl. **bit-exact i32 fixed-point** (D10 says build, not buy) | F-01, X-02 | 8 | yes — **blocks every transform audio codec** |
| D-17 | `vaco-cbs-core` | F-01 | 3 | yes |
| D-18 | `vaco-cbs-h2645` read path | D-17 | 6 | yes |
| D-19 | `vaco-cbs-h2645` write path | D-18 | 6 | after D-18 (v0.9) |
| D-20 | `vaco-cbs-av1` | D-17 | 4 | yes |
| D-21 | `vaco-cbs-vp9` + `vaco-cbs-jpeg` | D-17 | 4 | yes |
| D-22 | `vaco-codec-mpegvideo` core (T2) | D-11, D-01, F-03 | 14 | yes |

## 7.3 Wave 1b — v0.1 parsers (parallel with Wave 1; ~4 people, delivers D5)

| ID | Package | Deps | pw |
|---|---|---|---|
| P-01 | `vaco-parse-h2645` header subset | D-18 | 3 |
| P-02 | `vaco-parse-av1` header subset | D-20 | 1 |
| P-03 | `vaco-parse-mpegaudio` (ADTS/LATM/ASC + MP1/2/3 + AC-3 sync) | F-02 | 3 |
| P-04 | `vaco-parse-audio-misc` (Opus/Vorbis/FLAC/ALAC headers) | F-02 | 2 |
| P-05 | Profile/level name + constraint tables for H.264, HEVC, AV1, VP9, AAC | F-01 | 2 |
| P-06 | `vaco-parse-vpx` | D-21 | 2 |
| P-07 | `vaco-parse-mpegvideo` | F-02 | 3 |
| P-08 | `vaco-parse-image` | F-02 | 3 |

## 7.4 Wave 2 — T1 codecs (the bulk; heavily parallel)

AV1 is deliberately split into eleven packages against a stage-4 interface frozen up front, so five
people can work on it simultaneously after one person clears the first four packages.

| ID | Package | Deps | pw |
|---|---|---|---|
| C-01 | `vaco-codec-pcm` (table-driven, all 38+20 entries) | D-05 | 3 |
| C-02 | `vaco-codec-adpcm` (standardised subset) | D-05 | 5 |
| C-03 | `vaco-codec-rawvideo` (rawvideo, v210, r10k/r210, y41p, avui, bitpacked, wrapped_avframe) | F-01 | 4 |
| C-04 | `vaco-codec-subtitle-text` (ass/ssa/srt/webvtt/movtext/text/ttml) | F-01 | 6 |
| C-05 | `vaco-codec-flac` **decode via `claxon`** + D11 boundary + fidelity measurement | F-02, X-01 | 3 |
| C-06 | `vaco-codec-flac` **native encode** (`backend-native` for decode follows later if graded Divergent) | D-02, D-07 | 6 |
| C-07 | `vaco-codec-alac` (native decode+encode; `alac` crate as dev-dependency oracle) | D-02, D-07 | 6 |
| C-08 | `vaco-codec-png` **wrapping `png`** + D11 boundary + APNG + colour-metadata mapping | F-02, X-01 | 3 |
| C-09 | `vaco-codec-gif` **wrapping `gif`** + compositing parity | F-02, X-01 | 2 |
| C-10 | `vaco-codec-tiff` **wrapping `tiff`** + coverage audit | F-02, X-01 | 2 |
| C-11 | `vaco-codec-exr` **wrapping `exr`** | F-02, X-01 | 2 |
| C-12 | `vaco-codec-jpegxl` **wrapping `jxl-oxide`** | F-02, X-01 | 3 |
| C-13 | `vaco-codec-image-simple` (BMP/PCX/TGA/SGI/XWD/XBM) + `vaco-codec-pnm` + `vaco-codec-qoi`, all native | F-01 | 6 |
| C-14 | `vaco-codec-jpeg` **wrapping `zune-jpeg`** for still decode | F-02, X-01 | 2 |
| C-15 | `vaco-codec-jpeg` **native**: baseline + progressive + 12-bit + MJPEG-A/B + spec-exact IDCT mode + encoder | D-01, D-11 | 10 |
| C-16 | `vaco-codec-vp8` decode | D-04, D-08, D-09, D-10, F-03 | 10 |
| C-17 | `vaco-codec-vp8` encode | C-16, D-13, D-14 | 12 |
| C-18 | `vaco-codec-webp` **wrapping `image-webp`** | F-02, X-01 | 2 |
| C-19 | `vaco-codec-webp` native lossless + route lossy through C-16 | C-16, C-18 | 5 |
| C-20 | `vaco-codec-vorbis` decode (native; Floor 0 **and** Floor 1) | D-16, D-06, D-01 | 8 |
| C-21 | `vaco-codec-vorbis` encode | C-20 | 12 |
| C-22 | `vaco-codec-opus`: range decoder + packet framing | F-02 | 2 |
| C-23 | `vaco-codec-opus`: CELT decode | C-22, D-16, D-06 | 5 |
| C-24 | `vaco-codec-opus`: SILK decode | C-22, D-07 | 5 |
| C-25 | `vaco-codec-opus`: hybrid, multistream, PLC/FEC, integration | C-23, C-24 | 4 |
| C-26 | `vaco-codec-opus`: encoder (SILK NSQ, CELT allocation + PVQ search, mode decision) | C-25, X-04 | 20 |
| C-27 | `vaco-codec-ffv1` decode | D-02 | 8 |
| C-28 | `vaco-codec-ffv1` encode | C-27 | 6 |
| C-29 | `vaco-codec-vp9`: headers, superframes, bool decoder, probability model | D-04, D-21 | 5 |
| C-30 | `vaco-codec-vp9`: intra + transforms | C-29, D-09 | 7 |
| C-31 | `vaco-codec-vp9`: inter + MV prediction | C-29, D-08 | 5 |
| C-32 | `vaco-codec-vp9`: loop filter + profiles 1–3 + threading + conformance | C-30, C-31, D-10, F-03 | 9 |
| C-33 | `vaco-codec-vp9` encode | C-32, D-13, D-14, X-04 | 22 |
| C-34 | `vaco-codec-av1`: OBU layer, sequence header, `av1C`, Annex-B | D-20 | 5 |
| C-35 | `vaco-codec-av1`: frame header, reference management, tile info | C-34 | 8 |
| C-36 | `vaco-codec-av1`: symbol decoder + CDF machinery | C-34, D-04 | 4 |
| C-37 | `vaco-codec-av1`: tile/superblock loop, partition tree, mode info | C-35, C-36 | 5 |
| C-38 | `vaco-codec-av1`: intra prediction (incl. CFL, palette, intrabc) | C-37, D-09 | 8 |
| C-39 | `vaco-codec-av1`: inter prediction (MV stack, warp, OBMC, compound) | C-37, D-08 | 12 |
| C-40 | `vaco-codec-av1`: transforms | C-37 | 8 |
| C-41 | `vaco-codec-av1`: deblocking, CDEF, superres, loop restoration | C-37, D-10 | 8 |
| C-42 | `vaco-codec-av1`: film grain | C-41 | 4 |
| C-43 | `vaco-codec-av1`: tile + frame threading, DPB, integration | C-38..C-42, F-03 | 5 |
| C-44 | `vaco-codec-av1`: Argon conformance bring-up and triage | C-43, X-05 | 3 |
| C-45 | `vaco-codec-av1` **encode wrapping `rav1e`** (`default-features=false`, no asm) + D11 boundary + quality baselines | C-34, X-04 | 4 |
| C-46 | `vaco-codec-exec` (spawn a user-installed encoder; solves x264/x265 and the T5 escape hatch) | F-02 | 4 |
| C-47 | `vaco-codec-null` (`vnull`, `anull`) | F-02 | 0.5 |

## 7.5 Wave 3 — bitstream filters, T2 codecs, hardware

| ID | Package | Deps | pw |
|---|---|---|---|
| B-01 | `vaco-bsf-core` + `vaco-bsf-generic` | F-02, D-17 | 5 |
| B-02 | `vaco-bsf-h2645` (`*_mp4toannexb` first) | D-18 | 4 |
| B-03 | `vaco-bsf-av1` + `vaco-bsf-vpx` | D-20, D-21 | 3 |
| B-04 | `vaco-bsf-audio` (`aac_adtstoasc` first) | P-03 | 3 |
| B-05 | `*_metadata` filters (needs the CBS write path) | D-19 | 4 |
| B-06 | `vaco-bsf-subtitle` + `vaco-bsf-legacy` | B-01 | 4 |
| T2-01 | MPEG-1/2 video decode+encode | D-22 | 12 |
| T2-02 | MPEG-4 Part 2, H.263, H.261 | D-22 | 12 |
| T2-03 | MP1/MP2/MP3 decode (+ MP2/MP3 encode) | D-16, D-01 | 14 |
| T2-04 | AC-3 decode (E-AC-3 decode gated on the expiry verification) | D-16, D-06 | 12 |
| T2-05 | Theora decode | D-11, D-01 | 8 |
| T2-06 | DV decode+encode | D-11, D-01 | 8 |
| T2-07 | JPEG 2000 decode+encode | D-15 | 16 |
| T2-08 | JPEG-LS decode+encode | D-02 | 5 |
| T2-09 | ProRes decode / DNxHD decode (decode-only per legal) | D-11 | 14 |
| T2-10 | VC-1 / WMV3 decode | D-22, D-08, D-10 | 16 |
| T2-11 | Dirac / VC-2 | D-15 | 12 |
| T2-12 | G.711/722/726/729 + SBC + comfort noise + dfpwm + QOA | D-05 | 8 |
| T2-13 | Bitmap and text subtitle decoders (DVB, DVD, PGS, CEA-608/708, Teletext) | F-02 | 14 |
| T2-14 | APV, JPEG XS (assess crates first) | F-02 | 10 |
| H-01 | `vaco-hw-core`: device/frames contexts, `HwAccel` trait, `Frame` hw storage, selection and fallback | F-02 | 8 |
| H-02 | `vaco-hw-videotoolbox` (decode + encode) | H-01 | 11 |
| H-03 | `vaco-hw-vaapi` (decode + encode) | H-01 | 11 |
| H-04 | `vaco-hw-d3d11` + Media Foundation | H-01 | 11 |
| H-05 | `vaco-hw-nvdec` / `-nvenc` | H-01 | 11 |
| H-06 | `vaco-hw-vulkan` | H-01 | 12 |
| H-07 | Hardware conformance matrix + fallback tests in CI | H-02..H-06 | 4 |

## 7.6 Wave 4 — T3 (in-tree, never shipped) and beyond

| ID | Package | Deps | pw |
|---|---|---|---|
| T3-01 | H.264 decode, `patent-encumbered-h264-decode` | D-03, D-08, D-09, D-10, D-18, F-03 | 60 |
| T3-02 | HEVC decode, `patent-encumbered-hevc-decode` | D-03, D-08, D-09, D-10, D-18, F-03 | 55 |
| T3-03 | AAC-LC/HE/HEv2 decode, `patent-encumbered-aac-decode` | D-16, D-06, D-01 | 30 |
| T3-04 | AAC-LC encode | T3-03, X-04 | 25 |
| T3-05 | AC-3 / E-AC-3 encode | T2-04, X-04 | 12 |
| T3-06 | DTS core decode | D-16 | 40 |
| T3-07 | VVC decode — **post-v1.0, only if someone specifically wants it** | T3-02 | 110 |
| T5-01 | The two-team clean-room programme for the ~15 high-value spec-less formats (§3.5) | — | ~300 |
| T4-* | The documented long tail, grouped into ~10 crates | varies | ~250 |

## 7.7 What this means for staffing

| Contributors | Wave 0–1b | Wave 2 (T1) | Wave 3 | Realistic v1.0 |
|---|---|---|---|---|
| 3 | 10 months | 20 months | 14 months | ~3.7 years |
| 8 | 4 months | 8 months | 6 months | ~1.5 years |
| 20 | 3 months (Wave 0 does not parallelise below ~5) | 4 months | 3 months | ~10 months |

Wave 0 is the hard floor: F-01, F-02 and F-03 cannot be usefully parallelised beyond about five
people, and everything else waits on them. **Getting `vaco-codec-core` right is worth more than any
other six weeks in the project.**

---

# 8. Hardware acceleration

D9 promotes this from "nice to have" to **strategic**: hardware delegation is how our users get
H.264 and HEVC from a binary that contains no software implementation of either. It is also the
only place in the workspace where `unsafe` is permitted (D2), so its containment matters.

D10 Gate 1 does not apply here and it is worth saying why explicitly: the gate forbids **FFI to
third-party libraries that implement functionality we could implement ourselves**. VideoToolbox,
VA-API, D3D11, NVDEC and Vulkan are **platform silicon interfaces** — the OS *is* the boundary,
there is no pure-Rust alternative by construction, and D2's allowlist already anticipated exactly
these crates. **SUPERSEDED BY D13.** This paragraph originally read: *"They stay behind non-default
`hw-<backend>` features and out of the published default build until enabled per platform."* That is
now wrong on both counts.

D13 establishes that `hw-<backend>` features are **enabled by default on every platform that supports
them**. Containing `unsafe` was the stated reason for excluding them, and it is not a good reason —
unsafe is acceptable where it is the only way to do something, and talking to video hardware is
exactly that case. Excluding them was also quietly undermining the codec strategy this very section
argues for: if hardware delegation is how users get H.264 and HEVC at all, shipping it disabled means
shipping nothing.

D13 also refines Gate 1 more precisely than the paragraph above does. The distinction is not
"functionality we could implement ourselves" but **vendored third-party C** (banned) versus **an OS or
driver API reached through a pure-Rust binding crate** (permitted: `ash`, `objc2-*`, `windows`,
`wgpu` — none of which compile foreign code, vendor a foreign library, or launder a licence).

Backend priority per D13: **Vulkan Video via `ash`** is the primary investment — one vendor-independent
API covering Linux, Windows and Android — with **VideoToolbox via `objc2-video-toolbox`** required for
Apple, since MoltenVK does not implement Vulkan Video. VA-API, D3D12 and NVDEC are added only if
measurement shows Vulkan Video's coverage is insufficient; that measurement is an open item, and the
33 pw it gates should not be spent before it is taken.

## 8.1 Two integration shapes, and why the first one is architecturally free for us

**Shape 1 — the hwaccel hook (bitstream-level).** Our *safe* parser reads the bitstream and produces
the picture-parameter structures; the hardware backend uploads them plus the slice data and gets a
surface back. This requires the decoder to be split into a "parse and derive parameters" half and a
"reconstruct pixels" half.

**We already have that split, for free.** §1.8.1's frame-threading design puts every piece of
mutable decoder state — parameter sets, DPB, reference lists, picture headers — in the sequential
header stage, and everything else in a stateless task. The hwaccel hook is simply *a different
implementation of the task*.

The payoff is exact and it is the whole H.264/HEVC strategy in one sentence: **ship the header
stage, gate the reconstruction stage.** Crate split:

- `vaco-codec-h264-params` — SPS/PPS/slice-header parsing, POC, reference-list construction, DPB
  management. Pure parsing and derivation, no pixels. **Default build.**
- `vaco-codec-h264-decode` — the reconstruction half. **`patent-encumbered-h264-decode` only.**

A published `vaco` binary therefore contains everything needed to drive a hardware H.264 decoder,
and contains no software H.264 decoder. Same for HEVC and VVC.

```rust
// vaco-hw-core
pub trait HwAccel: Send {
    /// Negotiated once: the surface format and pool the decoder must allocate into.
    fn frames_config(&self, params: &CodecParameters) -> Result<HwFramesConfig>;

    fn start_frame(&mut self, pic: &HwPictureParams, out: HwSurface) -> Result<()>;
    fn decode_slice(&mut self, slice: &HwSliceParams, data: &[u8]) -> Result<()>;
    fn end_frame(&mut self) -> Result<Frame>;

    /// Called on flush/seek; must not fail.
    fn abort_frame(&mut self);
}

/// Codec-specific parameter payloads. Each backend translates these into its own API
/// structs; the codec crate never mentions VA-API or DXVA types.
pub enum HwPictureParams<'a> {
    H264(&'a H264PictureParams),
    Hevc(&'a HevcPictureParams),
    Av1(&'a Av1PictureParams),
    Vp9(&'a Vp9PictureParams),
    /* … */
}
```

**Shape 2 — the full hardware codec.** VideoToolbox sessions, MediaCodec, NVDEC/NVENC, QSV: the
backend implements `Decoder`/`Encoder` directly and we hand it whole packets. It carries
`Caps::HARDWARE` (or `Caps::HYBRID` where it has an internal software fallback), and it registers a
`DecoderDesc` like anything else — the registry does not special-case it.

## 8.2 Frames and zero copy

```rust
// vaco-frame gains one storage variant
pub enum PlaneStorage {
    Host(Arc<PooledBuffer>),
    Banded(Arc<ProgressPicture>),      // §1.8
    Hw(HwSurfaceRef),                  // opaque, backend-owned, refcounted
}
```

An `Hw` frame carries no host memory. It flows through the graph untouched until either a filter
needs pixels (auto-inserted `hwdownload`), or a hardware encoder consumes it directly (the
decode→encode transcode path stays entirely on the GPU), or the `vaco-play` renderer imports it as a
texture. `hwupload`/`hwdownload` are explicit filters, and the negotiation layer inserts them
automatically exactly as it inserts format conversions.

## 8.3 Selection and fallback

```rust
pub struct HwSelection {
    pub mode: HwMode,                  // Auto | Named(&str) | None
    pub device: Option<String>,
    pub output_format: Option<PixelFormat>,
    pub fallback: HwFallback,
}

pub enum HwFallback {
    /// Fall back to software on init failure, or on decode failure before any frame has
    /// been output. Default.
    OnInit,
    /// Additionally allow mid-stream fallback, but only at a keyframe boundary.
    Keyframe,
    /// Never fall back; fail loudly. What CI and batch pipelines want.
    Never,
}
```

The algorithm, in order:

1. **Candidate enumeration.** The registry is queried with `(CodecId, Profile, Level, PixelFormat,
   width, height, bit_depth)`. Each `HwConfig` declares what its backend supports; entries that
   cannot satisfy the query are dropped without opening a device.
2. **Platform ordering.** macOS: VideoToolbox → software. Windows: D3D11VA → Media Foundation →
   NVDEC → software. Linux: VA-API → NVDEC → Vulkan → software. Overridable with `-hwaccel`.
3. **Probe.** Open the device and query capabilities for real. Vendors misreport; the ordering
   above is a hypothesis, the probe is the test. `Caps::AVOID_PROBING` keeps expensive hardware
   decoders out of format probing.
4. **Format negotiation.** The decoder offers its output formats and the pipeline picks:
   ```rust
   fn choose_format(&mut self, offered: &[PixelFormat]) -> PixelFormat;
   ```
   Choosing a hardware pixel format commits to the hardware path; choosing a host format triggers
   an implicit download.
5. **Fallback.** On failure, the packet stream is replayed from the last keyframe into the software
   decoder if one is enabled. **If the software decoder is behind a `patent-encumbered-*` feature
   that is not compiled in — which is the normal case for H.264 and HEVC in a published binary —
   there is no fallback, and the error message must say exactly that**, naming the codec, the
   hardware that was tried and why it failed, and pointing at
   `docs/why-some-codecs-are-not-included.md`. A generic "decoder not found" here would be the
   worst user-facing failure in the product.

## 8.4 Containing `unsafe`

Per D2, `vaco-hw-*` are the only codec-layer crates permitted `unsafe`, and each needs a
justification in its crate-level docs plus a CI-enforced exception entry. On top of that:

- Every `unsafe` block carries a `// SAFETY:` comment naming the invariant and how it is upheld.
- The safe/unsafe boundary is **one module per crate** (`ffi.rs`), and nothing outside it is
  `unsafe`. The public API of every `vaco-hw-*` crate is 100% safe.
- Device and session handles are RAII wrappers with `Drop`; surfaces are refcounted through `Arc`
  and returned to the backend's pool on drop. Neither is `Sync` unless the platform API documents
  thread safety; where it does not, the handle is wrapped in a `Mutex`.
- CI runs the hardware paths under ASan/TSan where the platform allows, and requires a passing
  conformance run against the software decoder's output (which is why keeping T3-01/T3-02 in-tree
  matters even though we never ship them).
- `cargo-geiger` reports unsafe counts per release; a rise is a reviewed change, not a silent one.

## 8.5 Scope

Decode-side coverage worth having: H.264, HEVC, VP9, AV1 on all backends; MPEG-2 and VC-1 where
free; VVC on VA-API only. Encode-side: H.264, HEVC, AV1 via VideoToolbox / VA-API / NVENC / AMF /
Media Foundation / MediaCodec. Explicitly **not** in scope: QSV (an FFI-heavy vendor SDK with poor
lifecycle guarantees), RKMPP and OpenHarmony (negligible reach), and the AudioToolbox audio
decoders — except that the last one is the recommended AAC playback route on Apple platforms and
should be revisited if user demand justifies the exception.

---

# 9. Sequencing, risks and the numbers

## 9.1 Milestones

| Milestone | Contents | Cumulative pw |
|---|---|---|
| **v0.1** — `vaco-probe` (D5) | F-01, F-02, F-04, X-01, X-03, D-17, D-18(read), D-20, P-01…P-05 | ~35 |
| **v0.3** — first decodes | F-03, X-02, X-04, D-02, D-04, D-05, D-06, D-07, D-08, D-09, D-10, D-16, C-01, C-03, C-05, C-08, C-09, C-13, C-16, C-20, C-22…C-25, C-27 | ~150 |
| **v0.5** — remux + the image set | B-01…B-04, C-02, C-04, C-10, C-11, C-12, C-14, C-18, C-46, C-47 | ~200 |
| **v0.7** — the AV1/VP9 tentpoles | C-29…C-32, C-34…C-45, H-01, H-02, H-03 | ~330 |
| **v0.9** — encoders + T2 | C-06, C-07, C-15, C-17, C-19, C-21, C-26, C-28, C-33, D-19, B-05, B-06, T2-01…T2-06, H-04…H-07 | ~490 |
| **v1.0** — T2 complete, T3 in-tree | T2-07…T2-14, T3-01, T3-02, T3-03 | ~700 |
| **post-1.0** | T3-04…T3-07, T4, T5-01 | ~1,300 |

## 9.2 Where the effort is enormous, and what that means for sequencing

Say it plainly, because the numbers drive the plan:

- **VVC is ~23,000 lines upstream and ~110 pw for us.** It is 🔴 RED, has negligible deployment,
  thin hardware support, and no user is going to choose Vaco because of it. **Do not build it.**
  It sits in the plan only so nobody is surprised by its absence.
- **H.264 is ~20,000 lines and ~60 pw**, for something we can never ship in a published binary.
  It is worth building anyway — it validates the hwaccel parse-half and it serves licensed users —
  but it comes *after* the entire T1 set, not before, and it must never be allowed to become the
  project's centre of gravity just because it is the codec everyone knows.
- **AAC is ~22,000 lines and ~55 pw across decode and encode**, also unshippable. It is the most
  painful exclusion in the project because AAC is unavoidable in MP4. The mitigation that actually
  matters is not the decoder: it is that **remuxing AAC needs no decoder at all**, which we get
  from ~2 pw of `AudioSpecificConfig` parsing.
- **AV1 at ~70 pw is the largest thing we will actually ship.** It is also the item most likely to
  slip, because it is the only T1 codec that cannot be done by one person. Split it early
  (C-34…C-44), freeze the internal interfaces before parallelising, and accept that stages 5–9 will
  each need their own conformance triage.
- **T5 at ~300 pw for fifteen formats** is the worst effort-to-value ratio in the document. It is
  post-1.0 and it is optional.

The sequencing consequence: **do all of T1 before any of T3.** The temptation will be to start with
H.264 because it is the famous one and because "ffmpeg replacement" implies it. Resist. A tool that
decodes AV1, VP9, VP8, Opus, FLAC, PNG and JPEG perfectly and delegates H.264 to hardware is useful.
A tool with a half-finished H.264 decoder it is not allowed to ship is not.

## 9.3 Top risks

| Risk | Impact | Mitigation |
|---|---|---|
| **§1.8's banded picture model is too slow** for motion compensation | Frame threading becomes unusable; AV1/VP8 decode falls behind | Measure in F-03 with a synthetic benchmark **before** any codec depends on it. Escape hatches in §1.8.3, in order. If all three fail, escalate as a D2 decision — do not reach for `unsafe`. |
| **Portable SIMD cannot reach parity on deblocking** (architecture §7.2 #8) | 10–20% slower decode on H.264-family codecs | D-10 starts with a measurement spike, not an implementation. Masked-lane select is the technique; if it does not land, deblocking becomes a documented performance gap, not a correctness one. |
| **A wrapped codec grades Divergent** | It leaves the default build; a native implementation is unscheduled work | §4A.4 already ranks the likely candidates (JPEG first, TIFF second). Measure every wrapped codec in the same sprint it is adopted, never later. |
| **`rav1e` without assembly is too slow to be a credible AV1 encoder** | Our only AV1 encoder is 2–4× off the pace | Known at adoption time; write it into the adoption record. The fix is our own encoder (80–120 pw) and it is post-1.0. |
| **AV1's legal position deteriorates** (*Dolby v. Snap*) | The default build's flagship codec becomes AMBER-verging-RED | Track the case; D9 already requires it. The feature-flag machinery means AV1 can be moved behind an opt-in without an architectural change. |
| **E-AC-3's expiry claim is wrong** | We ship an encumbered decoder | Do not ship it until counsel confirms. It is already gated in §3.2. |
| **Paid specifications and conformance suites** (ISO 14496-3, ISO 14496-26, ITU-T T.83) | Blocks AAC and JPEG conformance | Budget it — a few thousand euro total, trivial against 380 person-weeks. Buy the AAC parts before T3-03 starts. |
| **Contributors who have read FFmpeg** | Clean-room contamination | The module-scoped contamination rule (legal §1.6.1) plus the provenance trailer plus the CI similarity scan. An ex-FFmpeg reader can work on everything except the module they read. |

## 9.4 The one-paragraph version

Build `vaco-codec-core` and the threading primitives first and get them right; they are six weeks
that determine everything after. Build the shared DSP crates in parallel with the v0.1 parsers, so
`vaco-probe` ships early and the kernels exist before the codecs need them. Buy the image periphery
and FLAC decode through D11 boundary crates, measure them immediately, and schedule native
replacements by grade. Write AV1, VP9, VP8, Opus, Vorbis, FFV1, ALAC, PCM, ADPCM and MJPEG
ourselves, because nobody else has. Delegate H.264 and HEVC to hardware, ship their parsers but
never their decoders, and never write their encoders. Keep the encumbered decoders in-tree behind
honestly-named features so the code is reviewed and the hardware paths are testable. Put the
encoder budget into AV1 and Opus. And write down, for every codec we do not support, exactly why —
because that document is the one FFmpeg never wrote, and it is the clearest signal that this project
means what it says.
