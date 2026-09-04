# RIST EAP SHA256-SRP6a Design

## What it is

`vaco-protocol-rist` will implement the optional PSK authentication method from
VSF TR-06-2:2022 Annex D. It is a sans-I/O, mutually authenticated
SHA256-SRP6a exchange carried as cleartext EAPOL inside GRE Protocol Type
`0x888E`; it produces an independent 32-byte session key `K` and gates media
until authentication succeeds.

This closes issue #657. It does not add a `rist:` socket protocol, register a
component, implement TLS-SRP, or add support for arbitrary SRP groups.

## Dependency decision

The implementation adds `crypto-bigint` 0.7.5 with default features disabled
and the `getrandom`, `subtle`, and `zeroize` features enabled. The crate is pure
Rust, MIT OR Apache-2.0, requires Rust 1.85 (the workspace requires 1.89), is
actively maintained by RustCrypto, and provides fixed-width Montgomery
arithmetic whose normal APIs are constant-time. Its published security notes
state that operations are constant-time unless their name ends in `_vartime`;
this implementation will not call any `_vartime` API. The upstream project also
records an NCC Group audit, while warning that the implementation has evolved
since that audit.

`num-bigint` is rejected because its variable-width representation and modular
exponentiation do not meet the issue's constant-time requirement. OpenSSL's
big-number API is rejected because it adds FFI, native build machinery, and an
unsafe boundary for a task a suitable pure-Rust crate covers. Hand-written
2048-bit arithmetic is rejected because a subtle arithmetic or timing error
would fail silently.

SHA-256 remains behind the existing `vaco-hash` ownership boundary. No second
direct `sha2` dependency is added. The `crypto-bigint` `getrandom` feature backs
a fallible native OS entropy source. The public RIST API exposes Vaco-owned
types only, including an injectable `SecretSource`, so deterministic tests do
not leak dependency types into the API. Passwords, private exponents, shared
secrets, and session keys are zeroized when dropped. Validator comparisons use
constant-time equality.

## Group policy and arithmetic

Production accepts only Annex D.3.4.3's default group: generator `g = 2` and
the specified 2048-bit safe prime `N`. A Challenge selects it by omitting both
the generator and modulus. Any explicit generator or modulus receives a named
unsupported-group failure. This avoids attacker-selected modulus validation,
an extra primality dependency, and a CPU denial-of-service surface.

All values are held in fixed-width `U2048` values and computed in Montgomery
form. Public values use canonical network-byte-order encoding with no leading
zero padding. `A` and `B` must be in `1..N`; zero modulo `N`, values at least
`N`, padded encodings, and encodings longer than 256 bytes are rejected.
Hashes concatenate the canonical byte representation Annex D defines. Private
`a` and `b` are sampled uniformly from `1..N` by rejection sampling over 2048
random bits, with a 128-attempt bound so a failing or malicious entropy source
cannot loop forever.

The informative Annex D.9 numeric vector uses the weakest permitted 512-bit
group. Tests may construct that exact group through a private test-only helper
which exercises the same fixed-width arithmetic and canonical encoding. It is
not exposed to production callers.

## Components and APIs

`src/eap.rs` owns the bounded EAPOL and EAP wire codec. Its typed messages are
EAPOL Start/Logoff; Identity Request/Response; Nak; SRP Challenge, Client Key,
Server Key, Client Validator, and Server Validator; Success/Failure; and the
optional Passphrase Request/Response framing. Parsing validates nested EAPOL
payload and EAP lengths, ignores bytes beyond the EAP length as required, and
never allocates more than the configured packet cap.

`src/srp.rs` is the only module which names `crypto-bigint`. It exports:

- `VerifierRecord::from_password(identity, password, salt)` and a fallible
  salt-generating constructor;
- `SessionKey`, which exposes a borrowed 32-byte value but is not implicitly
  clonable;
- `SecretSource` and `SystemSecretSource`;
- the staged client/server SRP calculations needed by the state machine.

The big-integer types remain private. A server stores salt and verifier, never
the cleartext password.

`src/auth.rs` owns the client and server state machines. `VerifierStore`
returns an owned `VerifierRecord` for an identity. `ClientSession` and
`ServerSession` accept configuration at construction and expose `start`,
`on_gre_packet`, `on_tick`, `is_authenticated`, `allows_data`, and
`session_key`. State transitions return one of four actions: ignore the input,
send a GRE packet, send a GRE packet and report authentication, or send an
optional Failure and disconnect. No API owns a socket, sleeps, or reads a
clock.

`src/gre.rs` adds `PROTOCOL_TYPE_EAPOL = 0x888E` and authentication frame
helpers. The helpers preserve caller-supplied GRE sequence information but
require Protocol Type `0x888E`; authentication payloads are never passed
through PSK encryption. Before initial authentication, a session discards GRE
packets with any other Protocol Type. A successful re-authentication keeps the
data gate open; a failed re-authentication closes it.

## Exchange and retry flow

The successful sequence is:

1. The client may send EAPOL-Start; the server may begin when the tunnel is
   established without waiting for it.
2. The server sends Identity Request with identifier `n`; the client returns
   Identity Response with `n`.
3. The server sends Challenge with `n+1`; the client samples `a` once and
   returns `A` with `n+1`.
4. The server samples `b` once and sends `B` with `n+2`; the client returns
   `M1` with `n+2`.
5. The server validates `M1` and sends `M2` with `n+3`; the client validates it
   and sends Success with `n+3`.

Identifiers wrap from `0xFF` to `0x00`. Each exchange owns four consecutive
values; a server session advances its next starting value by four so successive
exchanges do not overlap. The client accepts only the current `n..n+3` window,
discards a later request until its predecessor arrived, and resends its cached
response when an earlier request is duplicated. The server discards duplicate
or non-matching responses.

Each server request is cached byte-for-byte. `on_tick` retransmits that exact
request, identifier, and public key; it never resamples `a` or `b`. The default
timeout is 3,000 ms and the retry count is three, Annex D.6's recommended
minimum. Exhaustion clears transient authentication state and waits for a new
client contact. Client timeout restarts with EAPOL-Start. Re-authentication
cannot be initiated less than 60 seconds after the preceding exchange.

For an unknown identity, the default privacy-preserving policy continues with
a random fake salt and verifier, then returns Failure when validator checking
finishes. A configurable fail-fast policy implements Annex D.4's other allowed
behavior. Wrong `M1`, invalid `A`/`B`, and wrong `M2` produce the mandated
Failure packet before disconnecting where the specification requires one.

## Limits and configuration

`AuthenticationConfig` defaults to a 4,096-byte EAPOL packet cap, 1,024-byte
identity and password caps, a 3,000 ms timeout, three retries, privacy-preserving
unknown-identity handling, and no request to use `K` as the PSK passphrase.
Server names and salts are bounded by their one-byte wire lengths; salts must
be 4..=255 bytes. The parser performs checked length arithmetic and rejects
truncation before allocation.

One state-machine object represents one peer. The caller owns the global map
and therefore the maximum number of simultaneous peers; the API documentation
will state that deployment-level session limits must be enforced there. This
keeps global policy out of a sans-I/O per-peer primitive.

The optional Passphrase Request/Response packets are encoded and decoded. This
change exposes `K` for a caller that implements the existing section 7.4
rotation policy, but does not add that policy to the authentication state
machine.

## Error handling

Malformed lengths, reserved code/type/subtype values, non-canonical public
keys, invalid state transitions, entropy failure, unsupported groups, timeout
exhaustion, unknown identity under fail-fast policy, and proof mismatch have
distinct authentication failure reasons. Peer-controlled malformed packets do
not panic. Reserved EAPOL types and reserved EAP codes are silently discarded
as Annex D requires; unsupported Request types/subtypes produce Nak.

Terminal authentication failures zeroize transient secrets. Logoff returns the
session to unauthenticated state and closes the data gate. An error never
silently opens the gate or leaves a failed re-authentication's old key active.

## Verification

Development follows test-first red/green cycles. The independent evidence is:

- Annex D.9's published `x`, `v`, `A`, `k`, `B`, `u`, client/server `S`, `K`,
  `M1`, and `M2` values from its fixed salt and fixed private `a`/`b`;
- a one-time Python standard-library big-integer/SHA-256 reproduction of the
  same vector, compared with both the document and Rust results;
- literal wire-layout tests derived from Figures 20 through 33;
- a complete default-2048 GRE exchange in both success and failure paths;
- negative tests for wrong password, unknown identity, invalid/padded `A` and
  `B`, wrong `M2`, identifier mismatch, out-of-order requests, duplicates,
  stable retransmission, retry exhaustion, timeout restart, data gating, and
  the 60-second re-authentication floor;
- property tests asserting parse/serialize agreement only after the independent
  vector and literal-layout checks establish the interpretation.

There is no RIST reference peer on this machine, so cross-implementation
network interoperability remains unmeasured and will be reported as such.

## Documentation and provenance

`docs/io/vaco-protocol-rist.md`, the crate-level Rust documentation, and the
generated documentation index will describe the new authentication flow,
configuration, limits, dependency, and remaining interop boundary.

The implementation is derived only from the already-declared official source
`vsf-tr-06-2-2022`, specifically Annex D.1-D.6 and D.9. Commits touching the
protocol crate carry:

```text
Vaco-Provenance: spec
Vaco-Spec-Ref: vsf-tr-06-2-2022 Annex D
Vaco-Clean-Room: yes
```

No forbidden implementation source is consulted. The existing patent posture
is unchanged: the crate remains unregistered, and any future registration must
be encumbered and default-off.
