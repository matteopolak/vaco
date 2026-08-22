# `vaco-pool`

## What it is

Aligned, refcounted, copy-on-write byte buffers and the free lists that recycle
them. Two public types carry the whole crate:

- **`Buffer`** — the storage that *both* `vaco-frame`'s planes and
  `vaco-packet`'s payloads are built on. 64-byte aligned, cheap to clone, copies
  only when a shared holder writes, and returns to its pool on last drop.
- **`BufferPool`** — a bounded free list of same-sized buffers.

It matters because a 1080p frame is ~3 MB and a 60 fps pipeline that allocates
one per frame spends its profile in the allocator. It also matters because until
this crate had a public constructor, **no crate in the project could build a
`Packet` or a `Frame` at all**.

## How it works

### 64-byte alignment in safe Rust — the non-obvious part

Rust's global allocator only guarantees the alignment of the element type, and
`u8` has alignment 1. There are three ways to raise it and two of them are
closed to us: a custom `Allocator` is unstable, and a `#[repr(align(64))]`
element type would need `unsafe` (or `bytemuck::Pod`, whose derive feature we do
not have) to be viewed as `[u8]`. So we **over-allocate and sub-slice**:

```text
  raw:    |...|xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx|.......|
           ^   ^                                       ^
           |   offset (0..=63)          len bytes      raw.len() = len + 63
           base address, alignment 1

  offset = (-base) mod 64  ==  base.wrapping_neg() & 63
```

`AlignedBuf::new(len)` allocates `len + ALIGN - 1` bytes and starts the logical
buffer at the first 64-byte boundary inside them. Three facts make it sound with
no `unsafe`:

1. `<*const T>::addr()` is a **safe** operation (strict provenance, stable since
   1.84). We read the address only to compute an index; we never rebuild a
   pointer from it.
2. The `Vec` is never grown, shrunk or reallocated after construction —
   `AlignedBuf` exposes no method that could — so the address measured at
   construction is the address it keeps for life. Moving an `AlignedBuf` moves
   the `Vec` header, not the heap block.
3. `offset + len <= raw.len()` holds by construction because `offset <= 63`, so
   the `.get(offset..offset + len)` calls can never take their fallback branch.

Cost: 63 wasted bytes per allocation — 0.002% of a 1080p plane, and the pool
recycles the whole thing. Zero-length buffers still take the aligned path, so
the invariant is unconditional rather than "true except for the empty case".

### Copy-on-write

`Buffer` is `Arc<BufferInner>`. `Buffer::make_mut` is `Arc::make_mut`, which is
literally the "unique ⇒ in place, shared ⇒ clone" contract. `is_unique()`
exposes the predicate; `make_writable()` forces the copy at a point of the
caller's choosing.

One subtlety is load-bearing: `Arc::make_mut` also clones when a `Weak` exists,
so **no `Weak<BufferInner>` is ever handed out**. The only `Weak` in the design
points from a buffer at its *pool*, never the reverse.

`Clone for BufferInner` deliberately re-acquires from the same pool, so a
steady-state copy-on-write is also allocation-free. When the pool is dead or at
its cap the copy falls out of the pool rather than failing, because `Clone`
cannot return an error.

### Recycling

There is no `release` method anywhere in the project, so a buffer cannot be
returned twice or forgotten. `Drop for BufferInner` pushes the storage back onto
the free list, and `Arc` runs that `Drop` exactly when the strong count reaches
zero. The back-reference is a `Weak<PoolInner>`, so a long-lived frame never
keeps a dead pool alive — a buffer whose pool has gone is simply freed normally.

The pool is `parking_lot::Mutex<PoolState>`, not a lock-free stack: acquire and
release happen order 10³/s, not 10⁹/s, and an uncontended mutex is ~20 ns. The
allocation on a miss happens *outside* the lock so a 3 MB zeroing memset does
not serialise every other thread's free-list pop.

### Zeroing policy

Buffers are zeroed on first allocation and **not** re-zeroed on recycle. A
recycled pixel buffer may hold a previous frame's data; that is fine within one
process and is what every media library does, and paying for a 3 MB memset per
frame is precisely the cost this crate exists to remove. `get_zeroed()` exists
for the callers who need a clean slate, and `vaco-packet` re-zeroes only its
64-byte padding tail, which is the one region whose contents are load-bearing.

### The bitstream padding contract

`BITSTREAM_PADDING = 64` must equal `vaco_bitstream::Padded::PAD`, because
`Padded` is a typestate asserting that a reader may load eight bytes at any
position up to the logical end. `lib.rs` carries a `const _: () = assert!(...)`
so a mismatch is a compile error rather than a runtime surprise;
`vaco-bitstream`'s author asked for that assertion and could not write it in
their own crate. `Buffer::padded(logical_len)` hands back a `Padded` when the
buffer really carries the zeros.

## How to change it

- **The alignment scheme lives in `src/aligned.rs` and nowhere else.** If you
  change how `AlignedBuf` stores bytes, re-read the three soundness facts above:
  the "never reallocate the `Vec`" one is the easy one to break by adding a
  `push` or a `resize`.
- **`AlignedBuf` is deliberately private.** Keeping it crate-local is what makes
  the invariant checkable by reading one small module. Expose `Buffer` methods
  instead.
- **Adding a `Weak<BufferInner>` anywhere would silently break copy-on-write**:
  `Arc::make_mut` would start copying on every write. Do not.
- **Do not add a `release`/`recycle` public method.** `Drop` is what makes double
  return unrepresentable.
- Size classes: `BufferPool` is one class by design. Multi-class caching lives in
  `vaco-frame`'s `FramePool`, which owns one `BufferPool` per plane and throws
  them all away on a geometry change.
- If a benchmark ever shows mutex contention, `crossbeam` clears the D10 gates
  and is the right answer — but bring the benchmark.

## Configuration

| Item | Meaning | Default |
|---|---|---|
| `ALIGN` | Alignment of every buffer | `64` (compile-time constant) |
| `BITSTREAM_PADDING` | Zero bytes past a padded payload | `64`, asserted `== Padded::PAD` |
| `PoolConfig::max_live_bytes` | Bytes a pool may be responsible for | 1 GiB |
| `PoolConfig::max_live_buffers` | Buffers in flight | 4096 |
| `PoolConfig::max_retained_buffers` | Buffers kept on the free list | 32 |

The bounds are a **correctness property, not a tuning knob**: `FFmpeg`'s pool is
unbounded, which turns a resolution-switching stream into a memory leak, and D6
names unbounded allocation as a fuzz finding. `BufferPool::get` returns
`Error::LimitExceeded` rather than exceeding them.

Standalone `Buffer` allocation goes through a `vaco_limits::Budget` — a
positional parameter, per that crate's rules, because `clippy.toml` bans
`Vec::with_capacity` project-wide precisely so every input-sized allocation
lands on `Budget::alloc`. Pool allocation is bounded by `PoolConfig` instead,
since a `Budget` is `&mut`-single-owner and a pool is shared across threads.

## Dependencies

`vaco-core` (the `Error` taxonomy), `vaco-limits` (`Budget`), `vaco-bitstream`
(`Padded`, and the constant assertion), `parking_lot`. Dev: `proptest`.

`bytes::Bytes` was the one real external candidate and clears all three D10
gates; it fails on model. `Bytes` gives cheap *slicing* of immutable buffers,
whereas we need `make_mut`-style copy-on-write with pool return on last drop,
which `BytesMut`'s split/unsplit model does not express. `std`'s `Arc` does
exactly what we need in about forty lines.

Fuzz target: `fuzz/fuzz_targets/pool_acquire.rs` — arbitrary acquire/drop/share/
write schedules with fuzzer-chosen sizes and caps, asserting alignment, the
padding invariant and that the accounting never passes its bounds.
