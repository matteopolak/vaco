# RIST EAP SHA256-SRP6a Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. This repository explicitly requires work in the shared main checkout for this task; do not create a worktree or dispatch reviewer/test subagents.

**Goal:** Add bounded, mutually authenticated Annex D EAP SHA256-SRP6a sessions to `vaco-protocol-rist`, carried in cleartext GRE Protocol Type `0x888E`.

**Architecture:** A typed EAPOL codec and GRE adapter feed separate sans-I/O client/server state machines. A private `crypto-bigint` adapter performs constant-time fixed-width SRP arithmetic for the one allowlisted 2048-bit group, while injected entropy, zeroized secrets, cached retransmissions, and explicit data gating make security-sensitive state visible in the API.

**Tech Stack:** Rust 2024, `crypto-bigint` 0.7.5, `vaco-hash` SHA-256, `vaco-crypto` AES-CTR for optional passphrase framing, `vaco-limits`, `proptest`.

---

### Task 1: Dependency and wire-codec contract

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/io/vaco-protocol-rist/Cargo.toml`
- Create: `crates/io/vaco-protocol-rist/src/eap.rs`
- Modify: `crates/io/vaco-protocol-rist/src/lib.rs`

- [ ] **Step 1: Confirm the shared owners are clear**

Run `git status --short -- Cargo.toml Cargo.lock crates/io/vaco-protocol-rist` and check active agents. Do not edit `Cargo.toml` or `Cargo.lock` while another owner is using them.

- [ ] **Step 2: Add failing literal-layout tests**

Create `eap.rs` with tests that expect these exact encodings:

```rust
assert_eq!(EapolPacket::Start.serialize().unwrap(), [3, 1, 0, 0]);
assert_eq!(
    EapolPacket::Eap(EapPacket::success(0x42)).serialize().unwrap(),
    [3, 0, 0, 4, 3, 0x42, 0, 4],
);
assert_eq!(
    EapolPacket::Eap(EapPacket::identity_response(7, b"rist".to_vec()))
        .serialize().unwrap(),
    [3, 0, 0, 9, 2, 7, 0, 9, 1, b'r', b'i', b's', b't'],
);
```

Add failures for truncated nested lengths, reserved EAPOL types/codes, a salt shorter than four bytes, padded `A`/`B`, and packets over `AuthenticationLimits::default().max_packet_bytes`.

- [ ] **Step 3: Obtain the build slot and verify RED**

Run with the granted slot:

```sh
CARGO_INCREMENTAL=0 RUSTC_WRAPPER= cargo test -p vaco-protocol-rist eap::tests --target-dir /private/tmp/vaco-target-rist-srp -j2
```

Expected: compile failure because the packet types and methods do not exist.

- [ ] **Step 4: Adopt the dependency and implement the codec**

Add this reviewed workspace dependency:

```toml
crypto-bigint = { version = "0.7.5", default-features = false, features = ["subtle", "zeroize"] }
```

Add `crypto-bigint.workspace = true` and `vaco-hash = { path = "../../core/vaco-hash" }` to the RIST crate. In a non-wasm target-specific dependency table, enable `crypto-bigint`'s `getrandom` feature; keep `SystemSecretSource` behind the same target condition so the wasm library continues to compile with injected entropy. Implement `AuthenticationLimits`, `EapolPacket`, `EapPacket`, and typed `EapMessage` variants with checked `u16` lengths and allocation only after the configured cap is checked.

- [ ] **Step 5: Verify GREEN**

Run the focused test command again. Expected: all `eap::tests` pass.

### Task 2: Constant-time SRP core and official vector

**Files:**
- Create: `crates/io/vaco-protocol-rist/src/srp.rs`
- Modify: `crates/io/vaco-protocol-rist/src/lib.rs`

- [ ] **Step 1: Add the complete Annex D.9 vector test**

Parse the document's fixed `N`, `s`, `a`, and `b` hex values and assert exact canonical hex for `x`, `v`, `A`, `k`, `B`, `u`, client `S`, server `S`, `K`, `M1`, and `M2`. Add default-group tests rejecting explicit/custom groups and public values `0`, `N`, `N+1`, a leading-zero encoding, and 257 bytes.

- [ ] **Step 2: Verify RED**

Run `cargo test -p vaco-protocol-rist srp::tests --target-dir /private/tmp/vaco-target-rist-srp -j2` with the required environment. Expected: compile failure because SRP types are absent.

- [ ] **Step 3: Implement the hidden arithmetic adapter**

Use `U2048`, constant/runtime Montgomery forms, and only non-`_vartime` operations. Add `VerifierRecord`, `SessionKey`, `SecretSource`, `SystemSecretSource`, zeroizing private state, canonical integer conversion, SHA-256 through `vaco_hash::sha2`, uniform rejection sampling with a 128-attempt ceiling, and constant-time proof comparison. Keep the D.9 512-bit group constructor under `#[cfg(test)]`.

- [ ] **Step 4: Verify the official vector and independent oracle**

Run the focused test. Then use Python's standard `pow(..., ..., N)` and `hashlib.sha256` on the document's values and compare every intermediate to both the PDF and Rust test constants. Record the comparison in the protocol doc rather than storing a generated oracle.

### Task 3: GRE framing and data gate

**Files:**
- Modify: `crates/io/vaco-protocol-rist/src/gre.rs`
- Create: `crates/io/vaco-protocol-rist/tests/eap_gre.rs`

- [ ] **Step 1: Add failing GRE tests**

Assert an authentication packet serializes with GRE Protocol Type `0x888E`, round-trips with a sequence number, rejects any other Protocol Type as an authentication frame, and leaves the EAPOL bytes cleartext rather than passing them through PSK encryption.

- [ ] **Step 2: Verify RED**

Run the integration test alone. Expected: compile failure because `PROTOCOL_TYPE_EAPOL` and the frame helpers do not exist.

- [ ] **Step 3: Implement the minimal GRE adapter**

Add `PROTOCOL_TYPE_EAPOL`, `AuthenticationFrame::parse`, and `serialize`. Reuse `GreHeader`; do not duplicate its parser and do not add socket ownership.

- [ ] **Step 4: Verify GREEN**

Run the focused integration test. Expected: all GRE authentication framing tests pass.

### Task 4: Client/server authentication state machines

**Files:**
- Create: `crates/io/vaco-protocol-rist/src/auth.rs`
- Modify: `crates/io/vaco-protocol-rist/src/lib.rs`
- Create: `crates/io/vaco-protocol-rist/tests/eap_srp_session.rs`

- [ ] **Step 1: Add failing successful-exchange test**

Create a fixed `SecretSource`, in-memory `VerifierStore`, one client, and one server. Drive Start, Identity `n`, Challenge `n+1`, Client Key, Server Key `n+2`, Client Validator, Server Validator `n+3`, and Success as real GRE datagrams. Assert both sides are authenticated, both data gates are open, both session keys equal the Annex formula, and no authentication GRE payload was encrypted.

- [ ] **Step 2: Verify RED**

Run only `eap_srp_session`. Expected: compile failure because session APIs do not exist.

- [ ] **Step 3: Implement the successful state path**

Add `AuthenticationConfig`, `UnknownIdentityPolicy`, `VerifierStore`, `ClientSession`, `ServerSession`, `AuthenticationAction`, and `AuthenticationFailure`. Implement the four-identifier exchange, constant-time M1/M2 verification, session-key access, and data gate.

- [ ] **Step 4: Verify GREEN**

Run the focused integration test. Expected: successful full exchange.

- [ ] **Step 5: Add failure-path tests before behavior**

Add tests for wrong password causing server Failure and disconnect; privacy-mode unknown identity receiving fake Challenge/Server Key then Failure; fail-fast unknown identity; invalid `A`/`B`; wrong `M2` causing client Failure; Logoff clearing key/gate; and any pre-authentication non-`0x888E` packet being discarded.

- [ ] **Step 6: Run RED, implement failures, then run GREEN**

Each negative test must fail for its intended missing transition before implementation. Implement one transition at a time and rerun the named test after each change.

### Task 5: UDP loss semantics and re-authentication

**Files:**
- Modify: `crates/io/vaco-protocol-rist/src/auth.rs`
- Modify: `crates/io/vaco-protocol-rist/tests/eap_srp_session.rs`

- [ ] **Step 1: Add failing sequencing tests**

Assert exact request bytes remain stable across three retries; `a`/`b` are not resampled; duplicate server requests cause the client to resend the original response; duplicate/mismatched server responses are discarded; `n+2` before `n+1` is discarded; identifier windows wrap correctly; server exhaustion resets to waiting; and client timeout emits a fresh Start.

- [ ] **Step 2: Implement cached retry state**

Store the last request/response bytes and absolute next deadline. `on_tick(now_ms)` must be deterministic, use checked/saturating time arithmetic, and never sleep. Preserve secrets across retransmission and clear them on exhaustion.

- [ ] **Step 3: Add failing re-authentication tests**

Assert initiating before 60,000 ms is refused, successful re-authentication replaces `K` while data remains allowed, and failed re-authentication closes the gate and clears both old and new keys.

- [ ] **Step 4: Implement and verify re-authentication**

Reuse the initial exchange state rather than duplicating it. Run all session tests and the full crate test suite.

### Task 6: Property tests, lint, and documentation

**Files:**
- Modify: `crates/io/vaco-protocol-rist/src/eap.rs`
- Modify: `crates/io/vaco-protocol-rist/src/lib.rs`
- Modify: `crates/io/vaco-protocol-rist/Cargo.toml`
- Modify: `docs/io/vaco-protocol-rist.md`
- Modify after shared owners clear and regenerate only: `docs/README.md`
- Modify: `docs/dependencies.md`

- [ ] **Step 1: Add property tests**

Generate bounded typed messages and assert serialize/parse agreement. Generate arbitrary byte slices up to 8 KiB and assert parsing never panics and either rejects or returns a packet whose declared nested lengths are self-consistent.

- [ ] **Step 2: Update developer documentation**

Document the feature, flow, default-only group policy, APIs, configuration, retry/state limits, entropy and zeroization behavior, dependency decision, D.9/Python evidence, optional passphrase-framing boundary, no-peer interoperability limit, and unchanged patent posture. Remove stale statements that Annex D is unimplemented.

- [ ] **Step 3: Run scoped verification**

Run, with `CARGO_INCREMENTAL=0 RUSTC_WRAPPER=` and the private target:

```sh
cargo fmt -p vaco-protocol-rist -- --check
cargo test -p vaco-protocol-rist -j2
cargo clippy -p vaco-protocol-rist --all-targets -- -D warnings
cargo run -p xtask -- layer-check
cargo run -p xtask -- dep-gate
cargo run -p xtask -- owner-gate
cargo run -p xtask -- provenance-check
cargo run -p xtask -- comment-check
```

Regenerate `docs/README.md` only when its other active owners are clear, then confirm the diff adds/updates only the RIST row.

- [ ] **Step 4: Inspect the release dependency graph**

Run `cargo tree -p vaco-protocol-rist -e normal` and verify `crypto-bigint` is reachable only from the RIST crate, SHA-256 remains reachable through `vaco-hash`, and no unexpected FFI/native build dependency was introduced.

### Task 7: Commit, publish evidence, and close the issue

**Files:** all files above, scoped exactly.

- [ ] **Step 1: Review owned diffs and measurements**

Run `git diff --check`, read every owned hunk, and distinguish independent Annex D.9 evidence from self-consistency tests and unmeasured peer interoperability.

- [ ] **Step 2: Commit through a private index**

Create a scoped `commit-tree` commit with the exact trailers:

```text
feat(protocol-rist): add Annex D SRP authentication

Vaco-Provenance: spec
Vaco-Spec-Ref: vsf-tr-06-2-2022 Annex D
Vaco-Clean-Room: yes
```

Use compare-and-swap `git update-ref`, verify ancestry, verify the commit is non-empty, and inspect every committed path/content. Never stage or commit another agent's changes.

- [ ] **Step 3: Push and report measured evidence**

Push `main`, post the full successful/failure/vector/gate evidence on issue #657, explicitly state that no external RIST peer was available, and close only if every acceptance item is reachable and verified.

- [ ] **Step 4: Clean private artifacts**

Remove `/private/tmp/vaco-target-rist-srp` after verification and confirm all owned paths are clean.
