# `vaco-limits`

Layer 0. Allocation budgets, fuel counters and progress guards for code that
reads untrusted input.

## What it is

Vaco is `#![forbid(unsafe_code)]`, so the classic media-parser bug — memory
corruption from an attacker-controlled length — is not reachable. Three other
classes are (plan 13 §2.2), and this crate addresses two of them *structurally*,
by making the safe thing the only thing the type system permits:

| Bug class | Mechanism |
|---|---|
| Unbounded allocation | `Budget` — a required constructor parameter, with two-phase reservation |
| Non-termination | `Budget::consume_fuel` and `ProgressGuard` |
| Panics | not this crate — `clippy::unwrap_used` / `panic` / `indexing_slicing` are `deny` workspace-wide |

## How it works

### Policy and meter are separate types

`Limits` is immutable policy — caps only, no counters. It is `Clone + Send +
Sync` and can be shared across a whole pipeline.

`Budget` is the per-instance meter — counters only, every mutation behind `&mut
self`. No atomics, no interior mutability, one owner.

That split is what makes consumption **deterministic**. A shared counter behind
an `AtomicU64` would give a different exhaustion point depending on thread
interleaving, and a fuzz finding that does not replay is not a finding. With
`&mut self` the same input always exhausts at the same byte.

```rust
use vaco_limits::{Budget, Limits};

struct BoxParser { budget: Budget }

impl BoxParser {
    // The budget is positional. There is no constructor that omits it.
    fn new(limits: Limits) -> Self { Self { budget: Budget::new(limits) } }
}
```

### Two-phase reservation

The defence against declared-length amplification — "the header says 4 GiB, the
file is 16 bytes".

```rust
let reservation = budget.reserve(declared_len)?;  // phase 1: checked, not spent
// ... validate the header, decide whether to proceed ...
let buf = reservation.alloc::<u8>(actually_available)?;   // phase 2: spent
```

`Reservation` borrows the `Budget` mutably and releases its hold in `Drop`, so
the reject branch cannot leak budget: there is no release call to forget because
there is no release call. `Reservation::alloc` re-checks that the requested
element count fits inside what phase one approved — phase two quietly allocating
more than phase one checked would defeat the whole point.

Reserved bytes count against `max_alloc_total` while held, so a parser cannot
reserve its way past the cap by never committing.

### Incremental growth

`IncrementalVec` is for "declared size, unknown truth". It never allocates the
declared size; capacity doubles from a 32-element floor as data actually arrives,
each growth is charged, and delivery beyond the declared size is refused. A
16-byte file therefore cannot cause a gigabyte allocation however large its
length fields are. `IncrementalVec::charged` reports what was actually spent so
the caller can `Budget::release` it.

### Fuel

`Budget::consume_fuel(n)` is charged by any loop whose trip count is a function
of input data. It is a counter, not a clock: exhaustion is reproducible,
minimises cleanly, and regresses as an ordinary unit test. `Limits::deadline` and
`Budget::check_deadline` exist as the wall-clock fallback and are deliberately
*not* the primary mechanism, because an `Instant` comparison gives a different
answer on every machine.

### Progress guard

`ProgressGuard` enforces the contract every stepping API in Vaco carries: a call
to `read_packet` / `receive_frame` / `activate` either advances the input,
produces output, or says it is done. `tick(progressed)` counts consecutive
stalls and returns `LimitError::NoProgress` at 64. `tick_position(pos)` is the
stronger form — it derives progress from the cursor instead of trusting the
component's own claim, which is what catches a demuxer returning packets without
consuming bytes.

It returns an error rather than panicking, because `clippy::panic` is denied and
because a scheduler wants to shut a component down, not abort the process. A
fuzz wrapper is free to escalate the error into a panic.

## How to change it

- **Adding a cap.** Add the field to `Limits`, give it a value in all three
  presets, and add a `check_*` helper on `Budget`. `Limits` is
  `#[non_exhaustive]`, so this is not a breaking change and no caller can build
  one with the new field left at zero.
- **Adding an allocation shape.** Put it on `Budget` or `Reservation`, never on
  the caller. If a caller needs `Vec::with_capacity`, the answer is a new method
  here — `clippy.toml` denies the raw call project-wide and names `Budget::alloc`
  as the replacement, and that entry must keep pointing at something real.
- **Gotcha: releasing.** Nothing releases automatically except a dropped
  `Reservation`. A component that frees a buffer and keeps parsing must call
  `Budget::release`, or its budget ratchets down over a long file. The reason
  `Drop` cannot do it is that a `Vec<u8>` has no back-reference to the budget,
  and giving it one would mean an `Arc` and a shared counter — which would cost
  the determinism the whole design is built on.
- **Gotcha: `peak()` never falls.** It is a high-water mark, which is what makes
  it useful in a test ("this parser never went near its cap on valid input").
- **Gotcha: fuel is sticky.** Once exhausted, even `consume_fuel(0)` fails.
  `refuel()` at a packet or frame boundary is the intended reset.

## Configuration

| Preset | Total | Single | Dimension | Fuel | Intended use |
|---|---|---|---|---|---|
| `Limits::permissive()` | 1 GiB | 512 MiB | 65 536 | 2^32 | CLI default; sized for the largest plausible legitimate input |
| `Limits::strict()` | 64 MiB | 16 MiB | 8192 | 2^26 | library/embedder default, and `Default` |
| `Limits::tiny()` | 64 KiB | 16 KiB | 256 | 2^16 | `limit_*` fuzz targets |

Individual caps: `with_alloc_total`, `with_alloc_single`, `with_fuel`,
`with_deadline`. `Default` is `strict` deliberately — a default is what gets used
when nobody thought about it, and a library on untrusted input should be
conservative unless told otherwise.

The CLI will expose `-limits permissive|strict|custom:…` (plan 13 §2.2.2 rule 6);
that mapping lives in `vaco-cli-core`, not here.

## Dependencies

- `vaco-core` — for the shared `Error` taxonomy. `LimitError` converts into
  `vaco_core::Error::LimitExceeded`.
- `thiserror` — the error `Display` impls.
- Dev: `proptest`.

No external runtime dependencies beyond `thiserror`.

## Testing

- `crates/core/vaco-limits/tests/budget.rs` — unit tests for every cap, both
  reservation phases, fuel stickiness, the progress contract, and the error
  mapping.
- `crates/core/vaco-limits/tests/proptest_budget.rs` — the cap holds under any
  operation sequence; incremental growth never charges more than 2× delivery plus
  the floor; fuel accounting is exact and monotone.
- `fuzz/fuzz_targets/limit_budget.rs` — arbitrary operation sequences under
  `tiny` and `strict`. `LimitError` is success; a panic, an overflow, a breached
  cap or a drifted counter is a finding.
