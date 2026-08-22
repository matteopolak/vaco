# `vaco-packet`

## What it is

The compressed-data counterpart to `Frame`: a refcounted byte payload plus
timing, stream index, byte position, flags and packet side data. It is what a
demuxer produces, what a bitstream filter rewrites, and what a decoder consumes.

It shares its storage type — `vaco_pool::Buffer` — with `vaco-frame`'s planes,
so the ownership model is one design rather than two.

## How it works

### Padding is not optional

Every constructor here allocates `len + BITSTREAM_PADDING` (64) bytes and keeps
the tail zero, so `payload_padded()` is free and gives every parser in the
project `vaco-bitstream`'s unchecked-body fast path at no per-call cost (plan 11
F9). `vaco-bitstream`'s own benchmarks put that at a measured **11.7%** on the
per-unit workload — 512 NALs of about 27 bytes each — which is exactly the shape
of header parsing, the D5 v0.1 milestone. On long buffers the gap is zero, which
is why the original "1-3% on everything" estimate in plan 11 §8.3 was measuring
the wrong axis.

`payload_mut()` deliberately exposes only `..len`, so a bitstream filter cannot
dirty the zeros `payload_padded()` promises. `truncate()` re-zeroes the gap it
opens, for the same reason. `alloc_pooled()` re-zeroes only the 64-byte tail of a
recycled buffer rather than the whole thing — the padding is the one region
whose contents are load-bearing, and a full memset would give back the win the
pool exists to provide.

`payload_padded()` returns `Option` rather than asserting, so a caller who
replaced the public `data` field with an unpadded buffer degrades to
`BitReader::new` instead of getting a wrong answer.

### Copy-on-write and pooling

Cloning a packet is a refcount bump: demuxer → bitstream filter → decoder all
pass the same `Buffer`, and only a filter that rewrites the payload triggers a
copy. `is_writable()` / `make_writable()` expose and force that decision.
Pooled payloads return to their pool when the last clone drops; the mechanism
lives in `vaco-pool` and needs nothing here.

### Timestamps

`rescale_ts(from, to, rounding)` moves `pts` and `dts` together. Rescaling them
at separate call sites with possibly different rounding is how a stream drifts,
and making it one operation removes the whole bug class.

`duration` is **not** rescaled, because `vaco_core::Duration` is microseconds
rather than a tick count in the packet's own base — it is already
base-independent. That is a deviation from plan 11 §14.1, which assumed a tick
count plus a `time_base` field on the packet; the frozen struct has neither, and
the microsecond form is the better model.

## How to change it

- **`sub_packet` copies, and that is currently unavoidable.** Zero-copy
  splitting — MPEG-TS PES, Matroska laced blocks, ADTS framing — needs two
  things the frozen struct does not have: a byte `offset` field, and an answer
  for the padding invariant, since only the *last* sub-slice of a payload has 64
  zero bytes after it. The likely design is `offset: usize` plus a
  `payload_padded()` that returns `None` for interior slices, letting those
  callers take the unpadded reader. Both changes want to happen before demuxers
  start splitting packets in anger.
- **Do not widen `payload_mut()` to cover the padding.** The invariant is the
  fast path's precondition.
- **Side data is a typed enum.** `PacketSideData` is `#[non_exhaustive]`; add
  variants with a matching `PacketSideDataKind` as the containers that carry them
  arrive. Bulk payloads should be `Buffer` or `Arc<[u8]>` so cloning stays cheap.
- The `opaque: Option<Arc<dyn Any + Send + Sync>>` scheduler correlation token
  from plan 11 §14.1 is **not** in the frozen struct and has not been added. If
  `vaco-sched` needs it, that is a coordinated change, not a local one.

## Configuration

No environment variables and no feature flags.

| Knob | Where | Effect |
|---|---|---|
| `vaco_pool::BITSTREAM_PADDING` | compile-time | Zero bytes past every payload (64) |
| `Budget` / `Limits` | `alloc`, `from_slice`, `sub_packet` | Caps every input-sized allocation |
| `BufferPool::new_padded(len)` | `alloc_pooled` | Size class including the padding, so it does not fragment the free lists |

## Dependencies

`vaco-core` (timestamps, rationals, errors), `vaco-pool` (`Buffer`,
`BufferPool`, `BITSTREAM_PADDING`), `vaco-bitstream` (`Padded`), `vaco-limits`
(`Budget`), `smallvec`, `bitflags`. Dev: `proptest`. Same `bytes` assessment as
`vaco-pool`: it clears the D10 gates and fails on model.

No direct differential oracle exists — `Packet` has no CLI surface — but
`ffprobe -show_packets` prints an exact per-packet comparison of every field this
struct holds, which makes it very well covered from v0.1 onward via the demuxer
tests.

Fuzz target: `fuzz/fuzz_targets/packet_from_slice.rs` — arbitrary payloads,
sub-packet ranges, truncation points and pool classes, asserting the padding
invariant survives all of them, that a padded reader and an unpadded reader agree
bit for bit including on overrun, and that a sub-packet never aliases its parent.
