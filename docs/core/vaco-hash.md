# vaco-hash

## What it is

The single owner of `crc`, `md-5`, `sha1` and `sha2` — every checksum and
digest Vaco computes goes through here (**D11**: one third-party media crate,
one Vaco crate that reaches it).

Two components need the same primitives: `vaco-probe`'s `-show_data_hash` and
`vaco-mux-hash`'s eight checksum muxers. Before this crate they each declared
all four dependencies and each defined their own fifteen-name algorithm enum —
one spelled `HashAlg`, the other `HashAlgo`, with the same names and the same
labels. `cargo xtask owner-gate` caught the dependency half; `dup-check` would
have caught the rest.

That duplication was worse than it looks. In both places **the checksum is the
printed output**, so two implementations that disagree by a seed or a byte order
are a byte-level divergence from the reference *by definition*. And one of the
two consumers is `framemd5`, which the differential harness uses as one of its
own oracles (D6). An oracle with a private copy of the algorithm is not an
oracle.

## How it works

`HashAlgo` names all fifteen algorithms the reference's `-hash` accepts.
`implemented()` says which ten this build can compute; the other five —
`murmur3` and the four RIPEMD widths — have no pre-declared pure-Rust crate, and
adding one is a D10 decision.

They are still *named*, deliberately. A caller that has to reject a name needs
to distinguish "not a hash" from "a hash this build cannot do", and the
reference's own rejection message lists all fifteen. Omitting them would make
`-show_data_hash RIPEMD160` print an ordinary block with the field silently
missing — indistinguishable from success, which a differential harness scores as
a pass.

Two entry points:

- `digest_hex(data)` / `labelled_digest(data)` — one shot.
- `running()` → `RunningHash` — incremental, for muxers folding packet after
  packet. A property test asserts the two agree for every implemented algorithm.

### The two things measurement established

Both are recorded at length in `src/lib.rs`, and both are the kind of fact that
would silently corrupt every downstream comparison:

- **`framecrc` is not CRC-32.** It is Adler-32, seeded `(a=0, b=0)` rather than
  RFC 1950's `(a=1, b=0)`. The whole-file `crc` muxer is Adler-32 too, with the
  standard seed. Real CRC-32 appears only when asked for by name, `-hash crc32`.
  See `ADLER32_FRAME_SEED` and `ADLER32_STANDARD_SEED`.
- **SHA-1's accepted spelling is `sha160`.** `-hash sha1` is refused.

## How to change it

Adding an algorithm is a D10 dependency decision first and a variant here
second. Add the name to `NAMES` **in the reference's own order** — that order is
observable, because it is the order printed when a name is rejected — then the
variant, then arms in `label`, `hex_len`, `digest_hex` and `RunningHash`.

`HashAlgo` is deliberately not `#[non_exhaustive]`: a `match` that silently
accepted a new variant without a new hasher arm would produce a wrong digest
rather than a compile error.

## Configuration

None. No features, no options.

## Dependencies

`vaco-core` for the error type; `crc`, `md-5`, `sha1`, `sha2` — all pure Rust,
no FFI, no build scripts (D10 Gate 1). Adler-32 is nine lines here rather than a
fifth dependency.

`sha2` is also re-exported (`pub use sha2;`, added 2026-08-28) so
`vaco-crypto` (layer 0) can build `Hmac<Sha256>` for PBKDF2-HMAC-SHA256
without declaring a second direct `sha2` dependency — this crate stays
`sha2`'s one D11 owner; `vaco-crypto` composes on the concrete type rather
than re-declaring it. See `docs/core/vaco-crypto.md`.

`sha1` is re-exported the same way (`pub use sha1;`, added 2026-08-28
alongside `vaco-protocol-srtp`, #551) so `vaco-crypto` can build
`Hmac<Sha1>` for SRTP's RFC 3711 §4.2 authentication tag without a second
direct `sha1` dependency.
