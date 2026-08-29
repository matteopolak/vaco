# Dependency adoption record

Every direct dependency is a reviewed decision (D10), not a `cargo add`. One entry
each, with the assessment, the date, and who signed off.

## Gates
1. **Pure Rust, zero FFI** — no `-sys`, no bindings, no vendored C, no build script
   compiling native code, for anything that is part of "the ffmpeg pieces": codecs,
   containers, muxers, bitstream filters, signal processing, the filter graph, the
   CLI. **Amended 2026-08-28 (owner, `planning/00-decisions.md` "Gate 1
   amendment")**: FFI is now permitted for peripheral subsystems carrying no media
   semantics — transport security (TLS/DTLS) named explicitly. Enforced
   structurally by `cargo xtask dep-gate`, which reads the resolved **build
   graph** rather than `Cargo.lock` (the lock lists optional deps whether or not
   they are enabled) and — since the amendment — checks that each permitted FFI
   dependency is reachable *only* through the specific crate named for it
   (`xtask/src/deps.rs`'s `Banned::permitted_via`), not "FFI is fine anywhere
   now". `deny.toml`'s `[bans]` still hard-denies `cmake`/`bindgen` unconditionally
   (neither permitted provider below needs them) and every codec/container FFI
   target (`dav1d`, `libaom`, `libvpx`, `x264`, `x265`, `openh264`, `libopus`,
   `boring-sys`, `wolfssl-sys`) outright; it no longer tries to express the
   TLS/DTLS scoping itself, because cargo-deny's ban list cannot say "except
   through these two crates" the way `dep-gate` can.
2. **Licence** — the D3 allowlist.
3. **Trusted and maintained** — alive, adopted, no RUSTSEC advisory, shallow tree,
   forkable, unsafe measured.

---

## `ring` 0.17 — TLS crypto provider for `vaco-protocol-tls`

**Adopted** 2026-08-28, in response to the Gate 1 amendment. **Replaces**
`rustls-rustcrypto` (D14.2's original pure-Rust choice). **Used by**
`vaco-protocol-tls` (declares it, via `rustls`'s own `ring` Cargo feature —
see `crates/io/vaco-protocol-tls/src/crypto.rs`) and, by Cargo feature
unification on the one shared `rustls` package, `vaco-protocol-http`'s `ureq`
dependency (it does not declare `ring` itself; removing `vaco-protocol-tls`'s
feature flag removes `ring` from the build graph entirely — see
`xtask/src/deps.rs`'s comment on the `ring` row for the full mechanics).

**What for.** The actual cryptographic primitives (AES-GCM, ChaCha20-Poly1305,
ECDSA/RSA signature verification, key exchange) behind `rustls`'s TLS/HTTPS
handshake and record layer.

**Why this swap happened.** `rustls-rustcrypto` was pinned at `0.0.2-alpha`
(published 2024-04-24, no release since) and hard-required dependency versions
(`rustls-webpki ^0.102`, `rsa 0.9`) carrying RUSTSEC advisories that could not
be patched without a new release of it — see `deny.toml`'s advisories comment
for the QA-10 finding this traces to. Failing Gate 3's "alive"/"sound"
criteria outright, with `ring`/`aws-lc-rs` (the two providers that would
normally fix this) previously banned outright by Gate 1, was exactly the
situation the owner's Gate 1 amendment was written to resolve.

**Why `ring` over `aws-lc-rs`** (rustls's own current default, and the other
option the amendment named): both pass Gate 2 (`ring`: "Apache-2.0 AND ISC";
`aws-lc-rs`: "ISC AND (Apache-2.0 OR ISC)" — every constituent licence is on
the D3 allowlist) and both are actively maintained (`aws-lc-rs` releases
roughly monthly, most recently 2026-08-07). The deciding factor is Gate 3's
**shallow** and **vendorable** criteria: `aws-lc-sys` (checked directly against
its own `Cargo.toml`, per D9's "check what is actually linked, not what the
wrapper declares") requires `cc`, `cmake` and `pkg-config` as non-optional
build-dependencies plus `bindgen` for uncommon targets — a materially larger
build-machinery surface to reason about and to keep exempted from the rest of
Gate 1. `ring`'s own build-dependencies are `cc` alone. Fewer moving parts to
audit, and a smaller footprint if this workspace ever needs to vendor/fork it.

**Gate 3, checked, not assumed:**
- **Alive**: `ring`'s last crates.io release was 2025-03-11 (>12 months ago at
  adoption time), which would fail the letter of "a release... within ~12
  months" — but its GitHub repository shows commits as recent as 2026-07-23
  (`briansmith/ring`, not archived), which the same criterion also accepts
  ("...or a substantive commit"). Checked via the GitHub API directly rather
  than assumed from the stale crates.io date alone.
- **Adopted**: 699M+ crates.io downloads; the long-standing default `rustls`
  crypto provider before `aws-lc-rs` became the newer one, maintained by
  Brian Smith with the rustls team providing security co-maintenance.
- **Sound**: `ring` 0.17.14 (the version this workspace resolves to) has zero
  open RUSTSEC advisories. Three exist in the advisory database for the
  `ring` crate overall, checked individually: RUSTSEC-2025-0007
  (informational "unmaintained", withdrawn the same month after the rustls
  team took over co-maintenance), RUSTSEC-2025-0009 (patched in >= 0.17.12,
  and 0.17.14 postdates that), RUSTSEC-2025-0010 (only affects < 0.17, this
  workspace is on 0.17.14). None apply.
- **Shallow**: `cfg-if`, `getrandom`, `libc`/`windows-sys`, `untrusted`, plus
  the `cc` build-dependency — a small, well-known set.
- **Vendorable**: yes in practice — it is exactly the crate several other
  major Rust TLS stacks already treat as forkable-if-abandoned, and its own
  scope (hand-written C/assembly crypto primitives, no external C library
  vendored wholesale) is far more auditable than a full OpenSSL/BoringSSL fork
  would be.

**Unsafe.** `ring` is `unsafe`-heavy internally (it is a crypto primitives
library implemented substantially in C and assembly, wrapped in Rust). D10
says to weigh, not veto: this workspace's own `#![forbid(unsafe_code)]` covers
only `vaco-protocol-tls`'s own code, unchanged by this dependency, exactly as
it was unchanged by `rustls-rustcrypto`'s own internal `unsafe` use before it.

**wasm.** Does not build for `wasm32-unknown-unknown` — re-measured directly
(a throwaway crate depending on `ring` alone), same wall as
`rustls-rustcrypto` before it: `getrandom` hits its own hard `compile_error!`
without wasm's `js` feature before `ring` gets anywhere near its own
C/assembly. `vaco-protocol-tls` was already `NATIVE_ONLY` for this reason and
remains so (`xtask/src/wasm.rs`).

**Exit.** Swapping providers again means editing `vaco-protocol-tls`'s
`Cargo.toml` (the `rustls` feature flag) and `crypto.rs` (the one function
that constructs the provider) — D11's whole point.

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

---

## `objc2-video-toolbox` 0.3 + `objc2`/`block2`/`objc2-core-media`/`objc2-core-video`/`objc2-core-foundation` — VideoToolbox binding for `vaco-hw-videotoolbox`

**Adopted** 2026-08-28. **Used by** `vaco-hw-videotoolbox` only, behind a
`[target.'cfg(target_os = "macos")']` dependency table (not a Cargo feature),
so no other platform's build graph is affected at all.

**What for.** Reaching `VTDecompressionSession` (and the `CMFormatDescription`/
`CMBlockBuffer`/`CMSampleBuffer`/`CVPixelBuffer` types a decode call is built
from) without hand-writing Objective-C message-send and Core Foundation
retain/release bookkeeping by hand — H-01/H-02's hardware-acceleration
framework needs at least one real backend to prove the framework against, and
VideoToolbox is the only one this development machine (macOS) can exercise
end to end.

**Why no owner sign-off was requested first, unlike a normal D10 adoption.**
`planning/00-decisions.md` D14.3 already names this exact crate family, by
name, inside its permitted list: *"in `vaco-hw-*` and `vaco-filter-gpu` only,
pure-Rust bindings to OS/driver media and graphics APIs (`ash`, `objc2-*`,
`windows`, `wgpu`)."* D13's own backend-strategy table independently
recommends `objc2-video-toolbox` by name for exactly this reason (MoltenVK
does not implement Vulkan Video, so VideoToolbox is the only path to Apple's
media engine at all). The decision was already made; this entry is the
adoption record Gate 3 still requires, not a request for one.

**Gate 1** pass — pure Rust, zero FFI in the Gate-1 sense (no vendored or
compiled foreign C/C++). `objc2`'s own `build.rs` only emits
`cargo:rustc-cfg` target-triple checks; checked directly (not assumed) that
no crate in this dependency subtree pulls in `cc`/`bindgen`/`cmake`/`pkg-config`
(`cargo tree -p vaco-hw-videotoolbox -e build` — empty). `cargo deny check
licenses bans advisories` (workspace-wide, exit 0) raises nothing against any
`objc2*`/`block2`/`dispatch2` package.

**Gate 2** pass — every crate in this family is `Zlib OR Apache-2.0 OR MIT`
or plain `MIT` (checked per-crate via the crates.io API, 2026-08-28): all on
the D3 allowlist.

**Gate 3**, checked per crate:
- **Alive**: `objc2-video-toolbox`/`objc2-core-media`/`objc2-core-video`/
  `objc2-core-foundation` all last published 2025-10-04 (v0.3.2); the parent
  `objc2` crate 2026-02-26 (v0.6.4) — all well inside 12 months at adoption.
- **Adopted**: `objc2` itself has 100M+ crates.io downloads and is the de
  facto standard pure-Rust Objective-C interop crate (`github.com/madsmtm/objc2`).
  `objc2-video-toolbox` specifically is smaller (~46k downloads, ~34k recent)
  as a narrow sub-crate of that project, not a standalone adoption risk.
- **Sound**: no RUSTSEC advisory surfaced by `cargo deny check advisories`
  for any package in this family.
- **Shallow / forkable**: single upstream repository (`madsmtm/objc2`) for
  the whole family; no build-time C toolchain dependency to fork around.
- **Unsafe, measured**: `cargo xtask unsafe-audit` reports 19 unsafe sites in
  `vaco-hw-videotoolbox` (the consuming crate) — every FFI call across the
  VideoToolbox/CoreMedia/CoreVideo boundary, plus three `unsafe impl
  Send`/`Sync` — each with its own `SAFETY` comment. `vaco-hw-core` itself
  (the framework these traits live on) has zero.

**wasm.** `vaco-hw-videotoolbox` (and therefore this whole family) does not
enter a `wasm32-unknown-unknown` build graph at all — confirmed directly via
`cargo check -p vaco-hw-videotoolbox --target wasm32-unknown-unknown`
(clean) — because the dependency table itself is `cfg`-gated to
`target_os = "macos"`, not merely feature-gated. No `xtask/src/wasm.rs`
`NATIVE_ONLY` entry is needed.

**Verified, not just built.** `vaco-hw-videotoolbox`'s own test suite decodes
a real ffmpeg-produced H.264 keyframe through a real `VTDecompressionSession`
on this machine and checks the resulting frame's dimensions, pixel format
and pixel content (`tests/videotoolbox_decode.rs`) — this is not a stub.

**Exit.** Confined to one crate (`vaco-hw-videotoolbox`); removing it removes
one `HwAccelDesc` from the hardware candidate list, which `vaco-hw-core`'s
own `select()` already treats as an ordinary, non-fatal "nothing available"
case.
