# Dependency adoption record

Every direct dependency is a reviewed decision (D10), not a `cargo add`. One entry
each, with the assessment, the date, and who signed off.

## Gates
1. **Pure Rust, zero FFI** — no `-sys`, no bindings, no vendored C, no build script
   compiling native code. Enforced structurally by `deny.toml`'s bans and
   `cargo xtask dep-gate`, which reads the resolved **build graph** rather than
   `Cargo.lock` (the lock lists optional deps whether or not they are enabled).
2. **Licence** — the D3 allowlist.
3. **Trusted and maintained** — alive, adopted, no RUSTSEC advisory, shallow tree,
   forkable, unsafe measured.

---

## `rustfft` 6 — dev-dependency only

**Adopted** 2026-08-22, orchestrator. **Used by** `vaco-tx` (dev only).

**What for.** A float FFT oracle above n≈1024, where `vaco-tx`'s own O(n²) `f64`
reference becomes too slow to run in a test. Below that the direct definition is
the stronger oracle, because it *is* the definition rather than a second
implementation that could share a mistake.

**Gate 1** pass — pure Rust, no FFI. **Gate 2** pass — MIT OR Apache-2.0, matching
our own. **Gate 3** pass — widely adopted, actively maintained, shallow tree.

**Unsafe.** It uses `unsafe` internally for SIMD. D10 says to weigh that rather
than veto it, and here it is moot: a dev-dependency never enters a shipped
artifact, so the `forbid(unsafe_code)` guarantee about our binaries is untouched.

**Why not as an implementation.** Plan 17 assessed and rejected it for that role
on model fit: we need bit-exact i32 fixed-point transforms for codec conformance
and `rustfft` provides no fixed-point path at all. That objection is about
implementing with it, not testing against it.

**Exit.** Deleting it costs the large-n oracle only; the direct definitions and the
golden vectors remain.
