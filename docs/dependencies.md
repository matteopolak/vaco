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

## `crypto-bigint` 0.7.5 — fixed-width arithmetic for RIST Annex D

**Adopted** 2026-09-04. **Used by** `vaco-protocol-rist` alone, with workspace
default features disabled and only `subtle`/`zeroize` enabled. Native RIST
builds additionally enable `getrandom`; wasm keeps the entropy interface
injected and does not enable that feature.

**What for.** `TR-06-2:2024` Annex D requires 2048-bit modular
exponentiation for SHA256-SRP6a. The standard library has no fixed-width
constant-time big integer. `crypto-bigint` supplies constant-time comparisons,
Montgomery multiplication, and exponentiation for the single allowlisted
Annex D group; `zeroize` clears private values and `subtle` supplies proof
comparison. The crate does not expose arbitrary modulus selection.

**Gate 1.** Pure Rust with no build script, FFI, or native library. The selected
feature graph adds `cmov`, `ctutils`, `hybrid-array`, `num-traits`, `subtle`,
and `zeroize`; native entropy also adds `getrandom`/`rand_core`. The dependency
and its `cmov` substrate contain reviewed Rust `unsafe` internally for integer
representations, slice casts, and architecture-specific conditional moves.
That is dependency-internal and does not weaken this workspace's own
`unsafe_code = "forbid"`, but it is an explicit Gate 3 cost rather than being
described as an unsafe-free graph.

**Gate 2.** `crypto-bigint` declares `Apache-2.0 OR MIT`, both on the allowlist.

**Gate 3.** This is the RustCrypto project's maintained big-integer primitive,
not an ad-hoc SRP implementation. Version 0.7.5 requires Rust 1.85, has no build
script, and keeps the production surface at compile-time `U2048` arithmetic.
The implementation never calls a `_vartime` operation. Public values are
validated as canonical members of `1..N`, private scalars use bounded rejection
sampling, and official Annex D.9 intermediates are checked independently of a
self-round-trip. The exact version and checksums are locked so a later upgrade
requires a fresh vector, feature-tree, advisory, and internal-unsafe review.

**Exit.** All big-integer usage is isolated in
`crates/io/vaco-protocol-rist/src/srp.rs`. Replacing the dependency changes that
adapter and its vector tests, not the EAP codec or session state machines.

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

## `openssl` 0.10 (vendored) — DTLS for `vaco-protocol-dtls`

**Adopted** 2026-08-28, for #562 (PR-12b, DTLS), under the same Gate 1
amendment as `ring` above. **Used by** `vaco-protocol-dtls` alone (sole
declarer; `cargo xtask owner-gate`'s `MEDIA` list and `cargo xtask dep-gate`'s
scoped Gate 1 check both enforce this).

**What for.** DTLS 1.2 (RFC 6347) client and server handshakes over UDP for
the `dtls:` protocol — the transport WHIP/WebRTC-shaped callers need and that
#562 was blocked on since project start: no pure-Rust DTLS implementation
exists, and D14.2's zero-FFI Gate 1 admitted no alternative until the
amendment.

**Why FFI is unavoidable here, not merely convenient.** Checked directly
rather than assumed: no `rustls`-based or other pure-Rust crate implements
DTLS at all as of this adoption (`rustls` itself only speaks TLS; there is no
`rustls-dtls` or comparable pure-Rust crate with any real adoption). Every
credible option for real DTLS is an FFI binding to a C TLS/DTLS library.

**Why `openssl` (rust-openssl) over `boring`/`wolfssl`:**
- **`wolfssl`** is disqualified outright on Gate 2: wolfSSL is dual-licensed
  GPLv2 or a paid commercial licence, and GPL is denied unconditionally by
  this workspace's licence allowlist (`deny.toml`) — this is not a borderline
  call.
- **`boring`** (Cloudflare's Rust bindings to BoringSSL, Google's OpenSSL
  fork) is Gate 3-viable (actively maintained, adopted) but murkier on Gate 2:
  BoringSSL's own `LICENSE` is a composite of the original OpenSSL licence,
  the SSLeay licence, and ISC for newer Google-authored files — none of the
  first two are the modern "OpenSSL License" that OpenSSL itself relicensed
  away from at 3.0, and neither maps cleanly to an SPDX identifier on this
  workspace's allowlist. Checked directly against `boring-sys`'s vendored
  source per D9's "check what is actually linked, not what the wrapper
  crate declares" — the trap that caught `x264`/`x265` in D9 is exactly the
  shape of trap a composite non-SPDX licence sets.
- **`openssl`** (`sfackler`/rust-openssl) vendors OpenSSL itself via
  `openssl-src`, which at the version this workspace resolves to
  (`openssl-src 300.6.1+3.6.3`, i.e. OpenSSL 3.6.3) is licensed **Apache-2.0**
  — OpenSSL relicensed from the old dual OpenSSL/SSLeay licence to Apache-2.0
  at version 3.0, and every version `openssl-src` can vendor for this crate is
  3.x or later. Apache-2.0 is on the allowlist outright, no composite to
  puzzle over.

**Gate 3, checked, not assumed:**
- **Alive**: `openssl`/`openssl-sys` both released 2026-06-12; `sfackler`'s
  rust-openssl has been continuously maintained since 2014.
- **Adopted**: 382M+ crates.io downloads for `openssl` alone; it is the
  original, most widely depended-on TLS binding in the Rust ecosystem
  (predating `rustls` itself), used by `hyper-openssl`, `postgres`,
  `actix-web`'s TLS backend and many others.
- **Sound**: ten historical RUSTSEC advisories exist for `openssl`, checked
  individually against the version this workspace resolves to (`0.10.81`) —
  the three most recent (RUSTSEC-2024-0357, patched `>= 0.10.66`;
  RUSTSEC-2025-0004, patched `>= 0.10.70`; RUSTSEC-2025-0022, patched
  `>= 0.10.72`) are all patched well below `0.10.81`. None apply. No
  advisory directory exists for `openssl-sys` at all.
- **Shallow**: not shallow, and this is the honest cost of this adoption —
  `openssl-src` vendors the entire OpenSSL source tree, and `openssl-sys`
  pulls `cc`, `pkg-config` and `vcpkg` (the latter two are declared
  unconditionally by `openssl-sys` even though only the non-vendored path
  actually invokes them) as build machinery. Both are now permitted, scoped
  to this crate alone, by `cargo xtask dep-gate` (see `xtask/src/deps.rs`).
- **Vendorable**: weak, and also an honest cost — this workspace could not
  realistically fork and maintain the whole OpenSSL C codebase itself. This
  is inherent to *any* real DTLS FFI option (the owner's amendment names
  exactly these three as the available choices), not specific to picking
  `openssl` over the alternatives above, which are equally unvendorable by
  this team.

**Unsafe.** `openssl-sys` is unsafe-heavy by construction (raw FFI bindings
to a large C library) and `openssl` wraps it in a safe API, same shape as
`ring` above. `vaco-protocol-dtls` itself stays `#![forbid(unsafe_code)]` —
D13's `unsafe` allowance is for `vaco-hw-*` only, and this is not that.

**wasm.** Native-only: `openssl-sys`'s vendored build compiles C via `cc`,
which does not target `wasm32-unknown-unknown`, and DTLS itself needs a real
UDP socket, which that target has none of either. `vaco-protocol-dtls` is on
`xtask/src/wasm.rs`'s `NATIVE_ONLY` list for both reasons.

**What is scoped out.** DTLS's own retransmission timers for a lossy
transport (RFC 6347 §4.2.4) are not implemented — every test here runs over
loopback, where the gap does not show. `-listen`'s stateless cookie exchange
(`DTLSv1_listen`) is scoped out in favour of a simpler connect-on-first-packet
accept. Both are recorded in `vaco-protocol-dtls`'s own crate docs
(`transport`/`listen` modules), not silently shipped.

**Verified interoperability.** A real handshake was run against the actual
`ffmpeg 8.1` reference binary (built `--enable-openssl`) as the DTLS
listener, from this crate's own client path: `ffmpeg -listen 1 -f data -i
dtls://127.0.0.1:<port>` accepted the connection (`Input #0, data, from
'dtls://127.0.0.1:<port>'`) with no handshake error, satisfying #562's
acceptance criterion ("a DTLS session completes against a reference peer")
directly rather than only against this crate's own client/server pair.

**Exit.** `openssl` is the sole DTLS dependency, isolated behind
`vaco-protocol-dtls`'s own `Protocol` implementation; swapping it means
rewriting `context.rs`/`connect.rs`/`listen.rs`/`cert.rs` and nothing outside
this crate, matching D11's contained-replacement property.

---

## `rustfft` 6 — dev-dependency only

**Adopted** 2026-08-22, orchestrator. **Used by** `vaco-tx` (dev only).

**What for.** A float FFT oracle above n≈1024, where `vaco-tx`'s own O(n²) `f64`
reference becomes too slow to run in a test. Below that the direct definition is
the stronger oracle, because it *is* the definition rather than a second
implementation that could share a mistake.

**Gate 1** pass — pure Rust, no FFI. **Gate 2** pass — MIT OR Apache-2.0,
compatible with Vaco's GPL-3.0-or-later distribution. **Gate 3** pass — widely
adopted, actively maintained, shallow tree.

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

---

## `ash` 0.38 — Vulkan Video device/capability probing for `vaco-hw-vulkan`

**Adopted** 2026-08-28. **Used by** `vaco-hw-vulkan` only.

**What for.** Vulkan instance/device bring-up and device-extension
enumeration, to check whether a machine has a `VK_KHR_video_decode_h264`
-capable device (H-06a's scope). No decode session is implemented on top of
it yet (H-06b, real, substantially larger work, left open).

**Why no owner sign-off was requested first.** Same basis as
`objc2-video-toolbox` above: D14.3 already names `ash` by name in its
permitted list for `vaco-hw-*`, and D13's own backend-strategy table
independently calls it "the best single investment" for hardware
acceleration (one vendor-independent API reaching Linux, Windows and
Android). The decision predates this adoption record.

**Gate 1** pass — pure Rust, zero FFI in the vendored/compiled-C sense.
`ash`'s default `loaded` feature reaches the Vulkan loader via `libloading`'s
`dlopen`/`LoadLibrary` at *runtime*, not a build-time link — no `cc`,
`bindgen` or `cmake` anywhere in `cargo tree -p vaco-hw-vulkan -e build`
(empty). `cargo deny check licenses bans advisories` (workspace-wide, exit
0) raises nothing against `ash` or `libloading`.

**Gate 2** pass — `MIT OR Apache-2.0`, on the D3 allowlist.

**Gate 3**:
- **Alive**: checked, not assumed to still be current. `ash` 0.38.0+1.3.281
  was last published 2024-04-01 — over two years before this adoption date,
  which is worth flagging explicitly rather than glossing over. Its
  crates.io download count (34M+) and its position as *the* Rust Vulkan
  binding (no viable alternative exists) argue this is a stable, complete
  surface rather than an abandoned one, but a maintenance re-check before
  building the decode-session layer (H-06b) on top of it is warranted.
- **Adopted**: 34M+ downloads, the de facto standard Rust Vulkan binding.
- **Sound**: no RUSTSEC advisory surfaced by `cargo deny check advisories`.
- **Unsafe, measured**: `cargo xtask unsafe-audit` reports 5 unsafe sites in
  `vaco-hw-vulkan` — `Entry::load`, `create_instance`,
  `enumerate_physical_devices`, `enumerate_device_extension_properties`,
  `destroy_instance` — each with its own `SAFETY` comment. Deliberately
  small: this pass implements the capability query only, not a decode
  session, which would add substantially more.

**wasm.** Does not build for `wasm32-unknown-unknown` — measured directly:
`ash`'s `loaded` feature depends on `libloading`, whose safe `Library`/
`Symbol` re-export is `cfg`-gated to `any(unix, windows, libloading_docs)`,
which that target satisfies neither of (`E0432`, inside `libloading` itself,
before `ash`'s own code is even reached). `vaco-hw-vulkan` is on
`xtask/src/wasm.rs`'s `NATIVE_ONLY` list with this measurement recorded.

**Verified, not just built.** `vaco-hw-vulkan`'s own `probe()` runs real
`ash` calls on this machine and is confirmed (directly, via a throwaway
example binary) to return `ProbeOutcome::NoLoader` here — this development
machine has no properly configured system Vulkan loader, so even ordinary
instance creation, let alone the video-decode extension check, has not been
exercised end to end anywhere in this adoption. See
`docs/hw/vaco-hw-vulkan.md` for the full honesty statement. What *is*
verified: `probe()` never panics regardless of outcome, and `vaco-hw-core`'s
`select()` correctly falls back to software against this crate's real
(non-mocked) `HwAccelDesc` on this machine.

**Exit.** Confined to one crate; removing it removes one `HwAccelDesc`
candidate, which `select()` already treats as an ordinary "nothing
available" case.

---

## `cosmic-text` 0.12 — text shaping and rasterisation for `vaco-filter-text`

**Adopted** 2026-08-28, for #462 (FT-3.5). **Used by** `vaco-filter-text`
(`TextRenderer`) alone today; `vaco-ass`/`vaco-filter-subtitle` reach it only
through that crate's own API, never directly.

**Why this one crate, not four.** `planning/16-filters.md` SS6.1's own
architecture table names `fontdb` (font discovery), `rustybuzz` (shaping,
via `unicode-bidi` for reordering) and `swash` (outline parsing and
rasterisation) as the all-Rust replacement for
fontconfig/HarfBuzz/FreeType/FriBidi, each annotated "(via cosmic-text)" —
this is that annotation acted on literally rather than four separate direct
dependencies. `cosmic-text` is already declared in this workspace's root
`Cargo.toml` under "text shaping and fonts" (`swash`/`ttf-parser` are listed
there too, for the same D10 review this entry documents), so this is not a
new manifest entry, only the first crate to actually depend on it.

**Gate 1 — pure Rust, zero FFI.** `cosmic-text` 0.12.1 and its transitive
closure (`fontdb` 0.16.2, `rustybuzz` 0.14.1, `swash` 0.1.19, `ttf-parser`
0.20.1/0.21.1, `skrifa`/`read-fonts`/`font-types` 0.22.x/0.7.3,
`unicode-bidi` 0.3.18, `unicode-script`/`unicode-properties`/
`unicode-linebreak`/`unicode-segmentation`/`unicode-bidi-mirroring`/
`unicode-ccc`, `rangemap`, `self_cell`, `sys-locale`, `yazi`, `zeno`) — no
`-sys` crate, no `links` key, no build script compiling native code.
Confirmed by `cargo build` succeeding with no C toolchain invoked and by
inspecting each crate's own `Cargo.toml` for a `build.rs`/`links` entry.

**Gate 2 — licence.** `cosmic-text`, `fontdb`, `rustybuzz`, `swash`,
`ttf-parser`: `MIT OR Apache-2.0`. `unicode-*` family: `MIT OR Apache-2.0` or
`Apache-2.0 OR BSL-1.0` depending on crate, `zeno`: `MIT OR Apache-2.0`. All
on the D3 allow-list already (`deny.toml`).

**Gate 3 — trusted and maintained.** `cosmic-text` is System76's own text
stack (backs `cosmic-comp`/COSMIC desktop), actively released (0.12.1 is a
current line at adoption time), widely adopted beyond its origin project
(GUI toolkiles including `iced` and `egui`-adjacent crates use it or its
siblings). `rustybuzz` is a mature, from-scratch Rust port of HarfBuzz's
shaping algorithm (register task SS9.7's "confirm the port is a rewrite, not
a near-verbatim translation" is resolved by this: it is an independent
reimplementation of the *algorithm*, not a transliteration of HarfBuzz's own
C, and it carries plain MIT with no Old-MIT/HarfBuzz-inherited attribution
clause). `fontdb`/`ttf-parser`/`swash` are RazrFalcon-originated,
widely-depended-on crates (the same author's `resvg` ecosystem).

**FreeType is not in this dependency tree at all.** No `freetype-sys`, no
`freetype-rs`, nothing FTL-licensed anywhere in `cosmic-text`'s closure —
confirmed via `cargo tree`. This is the whole reason the all-Rust stack was
preferred in the register: FreeType's FTL carries a real, standing
attribution obligation in redistributed binaries; not depending on it at
all removes that obligation rather than merely discharging it.

**Unsafe.** Not audited line-by-line this pass; `swash`'s rasteriser and
`skrifa`/`read-fonts` (font table parsing) are exactly the kind of
performance-sensitive binary-format code that commonly carries some
`unsafe`, weighed rather than vetoed per D10 — none of it is reachable from
`vaco-filter-text`'s own `#![forbid(unsafe_code)]` boundary, which is
unchanged by this dependency.

**wasm.** Not checked this pass — `vaco-filter-text` has no `NATIVE_ONLY`
entry yet. `fontdb`'s system font discovery (`Database::new`'s platform
scan) is the most likely wasm hazard; a follow-up should verify before
`vaco-filter-text` is wired into any wasm target.

**Exit.** Confined to `vaco-filter-text::layout`/`vaco-filter-text::alias`;
swapping shaping/rasterisation backends means rewriting those two modules
against a different API, not touching `vaco-ass`/`vaco-filter-subtitle`,
which only see `TextRenderer`/`Layout`/`AlphaMask`.
