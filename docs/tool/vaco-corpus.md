# `vaco-corpus` — content-addressed conformance/fuzz corpus

## What it is

Fetching, local storage, mutation and minimisation for external media test
assets, addressed by content hash. It backs `vaco-conformance`'s `corpus://`
media sources — the JVT (H.264) and JCT-VC (HEVC) conformance suites are its
first real consumers (QA-04/QA-09, #180/#181, #427/#442) — and, separately,
`fuzz/`'s corpus-minimisation tooling.

## How it works

```text
vaco-media.lock ──▶ MediaLock::parse ──▶ [LockEntry] ──▶ fetch::fetch_asset ──▶ Store
                                                                │        │
                                                    NetworkPolicy      zip::extract, when
                                                    gates a miss       entry.member is set
```

- **`lock`** (`vaco-media.lock`, embedded at compile time via
  `embedded_catalogue()`) is the one file naming every asset this project
  knows about: a stable `name`, the `suite` it belongs to,
  `url`/`sha256`/`size`, a free-text `license`, `source` explaining how those
  facts were established, `targets` (which fuzz/conformance consumers use
  it), and `member` — the path *inside* the fetched asset a caller actually
  wants, when the asset is an archive rather than a directly-usable file.
- **`store`** is a local, content-addressed object store: every blob lives
  at `objects/sha256/<prefix>/<hash>`, written via a temp-file-then-rename so
  a reader never observes a partial write. `Store::default_root` honours
  `VACO_CORPUS_CACHE`, else `$HOME/.cache/vaco/corpus`.
- **`fetch`** is the only thing that touches the network, and only when told
  to: `NetworkPolicy::from_env` is `Allowed` iff `VACO_CORPUS_NETWORK=1`,
  `CacheOnly` otherwise — a cache hit never checks the policy at all.
  `fetch::fetch` returns the raw bytes of whatever `url` names, verified
  against `sha256` before being adopted into the store
  (`Store::put_verified` — a corpus is a security boundary, so a hash
  mismatch is fatal, not a warning). `fetch::fetch_asset` is `fetch` plus,
  when `entry.member` is set, `zip::extract`: this is the entry point a
  conformance case actually wants.
- **`zip`** is a from-scratch ZIP central-directory reader and RFC 1951
  (DEFLATE) decompressor, written because the JVT/JCT-VC conformance
  archives are ZIPs bundling the bitstream this project wants alongside a
  decoder trace log and/or a reference YUV it has no use for. See that
  module's own doc for why this is not simply a `miniz_oxide` dependency:
  D11 gives every third-party media crate exactly one owner, and
  `vaco-demux-matroska` already has it — a second `Cargo.toml` listing it
  fails `cargo xtask owner-gate`. Verified byte-for-byte against real
  `unzip` output on all 65 real conformance archives this project registers
  (stored (method 0) and deflated (method 8) members, no ZIP64/encryption/
  multi-disk — a JVT/JCT-VC archive uses neither).
- **`mutate`** is format-agnostic byte-level mutation/minimisation for
  building and shrinking fuzz corpora (unrelated to the conformance path
  above; see `docs/tool/fuzz-corpus-minimisation.md` for the tool that
  drives it).

## How to change it

- **Adding a corpus asset**: append an `[[entry]]` to `vaco-media.lock` by
  hand (this file is meant to be hand-edited — `MediaLock::parse`/`render`
  just round-trip it) with a real `url`+`sha256`+`size`, probed live, and a
  `suite` name a `vaco-conformance` `suites.toml` row can join against. Set
  `member` when the asset is an archive. A suite with no fetchable entries
  yet is not an error — see `suites::ResolvedSuite::is_empty` on the
  `vaco-conformance` side — but say so in this file's own header, the way
  the `argon` row does, rather than omitting the suite silently.
- **Adding a field to `LockEntry`**: touch `parse` *and* `render` in the
  same commit (`lock.rs`'s own module doc says why: an asymmetric change is
  a lock file that silently drops the field on the next rewrite).
- **`zip`**: only what a JVT/JCT-VC conformance ZIP actually contains —
  method 0/8, no ZIP64. A member using anything else is a named `ZipError`,
  never a silent wrong answer. If a future corpus needs ZIP64 or encryption,
  extend `zip.rs`'s central/local header parsing rather than reaching for a
  dependency — the whole reason this module exists is to keep this crate
  the sole owner of "unpack an archive" the same way it is already the sole
  owner of "fetch a URL".

## Configuration

| Variable | Effect |
|---|---|
| `VACO_CORPUS_NETWORK` | `1` allows a fetch to reach the network on a cache miss; anything else (including unset) is cache-only. |
| `VACO_CORPUS_CACHE` | Overrides the object store's root directory. |

## Dependencies

`vaco-hash` (SHA-256, not BLAKE3 — see `store.rs`'s own doc for why),
`vaco-protocol-core`/`vaco-protocol-http` (the only way this crate reaches
`ureq`+`rustls`, per D11), `vaco-limits` (bounds every allocation `zip.rs`
makes from untrusted archive/stream data — declared sizes are *reserved*
before being trusted, and the DEFLATE decompression loop is fuel-limited).
Nothing else; `miniz_oxide` was deliberately not added a second time (see
`zip.rs`'s own doc).
