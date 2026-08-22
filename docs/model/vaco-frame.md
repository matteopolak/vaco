# `vaco-frame`

## What it is

Decoded media: video pictures and audio sample blocks, with plane storage,
strides, colour signalling, timing, cropping and side data. This is where the
"zero copy" claim in architecture §7.4 is either true or not: the refcount and
copy-on-write model that `FFmpeg` builds by hand from `AVBufferRef` is expressed
here with `Arc` and `Arc::make_mut`, without a line of `unsafe`.

## How it works

### One `Arc` per plane (plan 11 F11)

A `Frame` is metadata plus a list of `Plane`s, and **each plane owns its own
`vaco_pool::Buffer`** — not one allocation per frame with planes at computed
offsets, which is `FFmpeg`'s layout. That single decision buys three things:

1. Two threads can hold `&mut` to different planes with the borrow checker
   proving disjointness. Splitting one `&mut [u8]` across planes would require
   the whole frame to be uniquely owned at that moment, which is exactly what
   cannot be guaranteed for a shared reference picture.
2. A filter that rewrites chroma and passes luma through shares the luma `Arc`
   and copies nothing.
3. Copy-on-write granularity is the plane, not the picture.

The cost is one extra allocation per plane, which the pool amortises to a
free-list pop, and inter-plane locality, which matters only for whole-frame
`memcpy`.

`Frame::planes_mut()` is the concurrency answer, and it needs no runtime
mechanism at all:

```rust
let mut planes = frame.planes_mut();
let (luma, chroma) = planes.split_at_mut(1);
std::thread::scope(|s| {
    s.spawn(|| luma[0].fill(16));
    s.spawn(|| { let _ = chroma[0].row(0); });
});
```

`iter_mut` yields one `&mut Plane` per element with distinct provenance, and
each plane's `Buffer::make_mut` acts on a *different* `Arc`. The three ways this
could go wrong are all closed: planes never share an allocation; a clone of the
whole frame makes each written plane copy privately; and "reader and writer want
the same plane" is not expressible.

Within one plane, `PlaneMut::split_bands(n)` yields disjoint row bands over
`chunks_mut`, which is how slice-threaded filtering parallelises. Same story:
structural, not checked.

### Plane views, and why nothing takes a bare slice

All plane access goes through `PlaneRef` / `PlaneMut`, which carry
`stride`, `rows` and `row_bytes` alongside the bytes. This is a review-blocking
rule rather than a style preference (plan 11 §13.6). Safe Rust cannot express
`FFmpeg`-style frame threading — a buffer simultaneously `&mut` above row R and
`&` below it — so v1 makes frame-threaded decoders wait for a whole reference
picture. The forward-compat move, if we later adopt banded planes, is that
`PlaneRef` grows a banded representation and an `await_rows(n)` method and every
kernel that took a `PlaneRef` keeps compiling. Handing out raw slices from
`Frame` would foreclose that.

`row(y)` returns `Option`, never a panic: an off-by-one in a kernel produces a
visible `None` rather than a neighbouring row.

### Allocation

`Frame::alloc_video` asks `vaco_pixfmt::PixFmt::plane_layout(w, h, ALIGN)` for
strides and sizes, so **every stride is a multiple of 64 and therefore every row
of every plane is 64-byte aligned**, not just row zero. Dimensions are checked
against `Budget::check_frame` before a byte is touched, and hardware pixel
formats are refused outright — their planes live on a device.

`Frame::alloc_audio` allocates one buffer per channel for planar formats and
exactly one for interleaved. `Frame::video_from_planes` wraps planes filled
elsewhere and validates that each is long enough for its declared geometry.

### Cropping is metadata

`Crop` rides in side data and `cropped_dimensions()` reads it. No plane is
touched, no byte moves — a coded 1920x1088 HEVC picture presents as 1920x1080 by
carrying `bottom: 8`. `set_crop` validates against chroma subsampling: a 4:2:0
picture cannot be cropped by an odd number of pixels because there is no half
chroma sample to start at, and rejecting that here is what stops a
bitstream-supplied rectangle from producing a misaligned chroma plane.

### Pooling

`FramePool` owns one `BufferPool` per plane, keyed by `(format, width, height)`
for video or `(format, channels, samples)` for audio. When the geometry changes
it **throws the whole cached set away**, because keeping the old classes is
exactly how a pool becomes a leak on a resolution-switching stream. The
steady-state test asserts that `allocations` stops rising while `hits` climbs —
proving the pooling claim rather than asserting it.

## How to change it

- **Do not add a method that returns `&[u8]` or `&mut [u8]` from a `Frame`.**
  See the plane-views section; this is the one rule in the crate that is worth
  blocking a review over. (The `Plane.data` field is public because the interface
  freeze made it so; treat it as legacy surface, not as an example.)
- **Adding fields to `Plane` is a breaking change for struct-literal
  construction.** The frozen shape is `{ data, stride }`; geometry is derived
  from the frame's format instead. If zero-copy `apply_cropping` or zero-copy
  `Packet::slice` ever land, both need a byte `offset` on `Plane` and on
  `Packet` — that is the one deferred change that touches other crates, and it
  gets cheaper the sooner it happens.
- **Side data is a typed enum, not a blob.** Add variants as the codecs that
  produce them arrive, and give each one a `FrameSideDataKind`. `FrameSideData`
  is `#[non_exhaustive]`, so adding is not a breaking change outside the crate.
  Bulk payloads should be `Arc<[u8]>` so cloning a frame carrying an ICC profile
  stays a refcount bump.
- **`FramePool` caches exactly one geometry.** If a real workload turns out to
  alternate between two resolutions, widen it to a small LRU rather than removing
  the eviction.
- Adding a new pixel format needs nothing here: geometry comes from
  `PixFmt::plane_layout`.

## Configuration

No environment variables and no feature flags. The knobs are parameters:

| Knob | Where | Effect |
|---|---|---|
| `vaco_pool::ALIGN` | compile-time | Stride rounding and buffer alignment (64) |
| `Budget` / `Limits` | `alloc_video`, `alloc_audio` | `max_dimension`, `max_frame_bytes`, `max_alloc_*` |
| `PoolConfig` | `FramePool::new` | Per-plane live and retention bounds |

`Limits::strict()` is the library default and `Limits::permissive()` the CLI
one; `Limits::tiny()` is what the fuzz targets use.

## Dependencies

`vaco-core` (timestamps, rationals, errors), `vaco-pixfmt` (plane geometry),
`vaco-sampfmt`, `vaco-chlayout`, `vaco-color`, `vaco-pool` (`Buffer`, `ALIGN`,
`PoolConfig`), `vaco-limits` (`Budget`), `smallvec`, `bitflags`, `parking_lot`.
Dev: `proptest`. No external runtime dependencies beyond those.

Fuzz target: `fuzz/fuzz_targets/frame_alloc.rs` — arbitrary
`(format, width, height, crop, band count)`, asserting that either a valid frame
or an error comes back, that every plane is aligned and long enough for its own
rows, that cropping never moves a byte, and that bands partition a plane exactly.
