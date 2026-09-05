# Release engineering

## What it is

Everything needed to cut a `vaco`/`vaco-probe` binary release: reproducible
builds, the SBOM, the third-party attribution file, checksums, and the
signing/notarization runbook. Closes QA-10 (#182), the last child of epic
#9.

For the crates.io distribution (`vaco`, with installed `vvmpeg` and `vvprobe`),
see [Crates.io publishing](release/crates-io-publishing.md). That path has a
separate read-only package-content gate and is intentionally held until the
repository's open issues are cleared.

### CI compiler cache

The GitHub Actions build matrix keeps sccache enabled on every platform, but
pins the Windows leg to sccache `v0.16.0`. sccache `v0.17.0` expands Rust
argument files before spawning `rustc`; the resulting command can exceed
Windows' process command-line limit and fail with `failed to spawn` before any
test runs. This exact failure is tracked upstream in
[`mozilla/sccache#2787`](https://github.com/mozilla/sccache/issues/2787), and
the issue's follow-up reports that `v0.16.0` fixes it while retaining the
wrapper. Linux and macOS remain on `v0.17.0`; the test and clippy commands are
unchanged. Update the Windows pin when an upstream fix for the argfile path is
available.

## How it works

### Attribution (the actual legal obligation)

MIT, BSD, ISC, Apache and FTL all require attribution in a redistributed
binary. That duty has two independent sources, and missing either one is a
real compliance gap, not a polish item:

1. **Linked Cargo dependencies** — every crate compiled into `vaco`/
   `vaco-probe`. Covered by [`cargo-about`](https://github.com/EmbarkStudios/cargo-about),
   config in `about.toml` (kept in sync with `deny.toml`'s `[licenses]`
   allow list — see that file's own comments for why `CDLA-Permissive-2.0`
   is on both).
2. **Permissively-licensed reference implementations a crate was
   translated from, but never linked as a dependency.** `libopus` (via RFC
   6716 Appendix A, BSD-3-Clause) and Apple's ALAC reference (Apache-2.0)
   are the two live cases today, in `vaco-codec-opus` and `vaco-codec-alac`
   respectively — see AGENT-CONSTRAINTS.md's "clean-room rule is about
   FFmpeg, not about every reference implementation" for why reading them
   was allowed, and `provenance/third-party-notices.toml` for the
   attribution record itself. `cargo-about` cannot see these; nothing about
   a `Cargo.lock` scan would ever surface them.

`scripts/gen_third_party_notices.py` builds `THIRD_PARTY_LICENSES.html`
from both sources and also runs a coverage scan: it greps every
`provenance/*.toml` for a permissive-licence keyword (`Apache License`,
`BSD`, `MIT License`, `ISC License`, `zlib license`) and fails
(`--check`) if the nearest `[[source]]` id isn't cross-referenced from
`third-party-notices.toml`. This is what catches the next one — dav1d
(BSD) is the likely next case once AV1 decode reads it as a Tier-A
reference, and libvpx/HM/JM/libjxl are named as pre-cleared Tier-A sources
in `planning/research/07-legal-patents-licensing.md` §1.6.1 if any of them
are ever read the same way.

Run `just licence-report` to regenerate `THIRD_PARTY_LICENSES.html`, or
`just licence-report-check` (wired into `just ci`) to just run the
coverage/resolution check without needing the file on disk to be current.

**What this does NOT cover yet**: a `reuse lint`-style per-file SPDX-header
audit (`planning/research/07-legal-patents-licensing.md` §4.3 names this as
a fourth, independent CI job). Not built here — flag as a separate task if
wanted; it is orthogonal to the attribution *file* this issue is about.

### Licence and advisory gates (D3 / D10 Gates 2–3)

`just licence` runs `cargo deny check` (licenses, bans, sources,
advisories — all four, as of this issue; `advisories` used to be left out
of both this recipe and the separate, still-unwired `audit` recipe, so
RUSTSEC vulnerabilities were never actually gated). Running it for the
first time found real, previously-unnoticed gaps in every category — see
`deny.toml`'s own comments for what was found, what was fixed, and what is
recorded as an assessed, intentional `ignore` versus what is genuinely
blocked pending a cross-crate fix that is out of this issue's scope (two
spawned follow-up tasks: a `publish = false` sweep across every crate
manifest, and a `quick-xml` 0.41 bump blocked on three renamed call sites).

**Resolved:** the `quick-xml` bump landed at **0.42.0** and its two
temporary `ignore` entries (RUSTSEC-2026-0194, -0195) are gone. `cargo
audit` now reports **zero** vulnerabilities; `cargo deny check advisories`
still fails, on `ttf-parser` (RUSTSEC-2026-0192) and `rustybuzz`
(RUSTSEC-2026-0206) — both `unmaintained`, both reached only through
`cosmic-text`, and neither has an accepted `ignore` or an in-place upgrade.
Closing those means moving text shaping off `cosmic-text`.

The headline finding: `rustls-rustcrypto` (our only D10/D14.2-compliant TLS
crypto provider — `ring`/`aws-lc-rs` are banned for compiling C/assembly)
has had no release since 2024-04-24 and now carries five unpatched RUSTSEC
advisories in its own pinned dependencies. Each was individually checked
for reachability against `vaco-protocol-tls`'s actual code before being
recorded as accepted residual risk; none is a rubber-stamped ignore. This
is a real "trusted and maintained" gap under D10 and worth the owner's
attention independent of this issue — a crypto-provider swap or a vendored
fork are the two ways out, and neither is this agent's call to make.

**Resolved 2026-08-28**: the owner made the call this section flagged as
outstanding — a Gate 1 amendment (`planning/00-decisions.md`) permitting FFI
for TLS specifically. `vaco-protocol-tls` swapped to `ring`; all five
`rustls-webpki`/`rsa` advisories above cleared (`cargo deny check advisories`
now reports them as "not encountered" — the vulnerable crate versions are
simply out of the graph), and their `deny.toml` ignore entries were removed.
See `docs/dependencies.md`'s `ring` entry for the replacement's own Gate 2/3
record. One advisory from the original finding (`RUSTSEC-2024-0436`, `paste`)
remains ignored for an unrelated reason — it is also pulled in by
`vaco-codec-exr`'s `exr`/`pulp` dependency, independent of the TLS stack.

### SBOM

`just sbom` writes SPDX 2.3 and CycloneDX 1.5 JSON for both `vaco-cli` and
`vaco-probe` to `dist/sbom/`, via
[`cargo-sbom`](https://github.com/psastras/rsdoctor-cargo-sbom) (`cargo
install cargo-sbom --locked`). Chosen over `cargo-cyclonedx` — measured,
not assumed: `cargo-cyclonedx` writes a `<crate>.cdx.json` file directly
into *every* crate directory it touches (tested against this workspace: it
scattered six files across `xtask/`, `crates/app/vaco-cli/`,
`crates/app/vaco-probe/` and three `crates/tool/` crates in one run), which
is unusable in a shared tree where those directories belong to other
agents. `cargo-sbom` reads `cargo metadata` and writes to stdout only.
Trade-off worth knowing: `cargo-sbom`'s last release was 2025-06-17, over a
year stale against `cargo-about`/`cargo-deny`/`cargo-cyclonedx`'s more
recent releases — acceptable for a tool this narrow in scope (it does not
compile anything, does not touch `Cargo.lock`, and both output formats were
verified against the real dependency graph while building this), but worth
re-checking if it ever stops working against a newer `cargo metadata`
schema.

### Reproducible builds

`just verify-reproducible [--profile release|dist] [packages...]`
(default: `--profile release`, `vaco-cli vaco-probe`) builds twice, from
two separate `--target-dir`s so neither build can reuse the other's
objects, and compares the results byte-for-byte. **This found a real,
currently-unresolved difference** — the check has teeth, this is not a
"ran once, looked clean" report:

- **`--profile release`** (`lto = "fat"`, `codegen-units = 1`,
  `strip = "symbols"`): **reproduces bit-for-bit**, measured on this
  machine (macOS/aarch64, 2026-08-28), including the Mach-O `LC_UUID` load
  command, which is the most common source of a spurious mismatch on this
  platform (most linkers regenerate it per link; this toolchain evidently
  derives it deterministically from content).
- **`--profile dist`** — what `scripts/package-release.sh` actually
  ships, because it keeps `debug = "line-tables-only"` so a crash report
  is symbolisable — **does NOT reproduce**. Two independent builds of
  `vaco` differed by ~19,888 bytes, and the difference is not confined to
  a trailing metadata section: `otool -l`'s `__TEXT` segment `vmsize` and
  `filesize` themselves differ between the two builds (0x848000 vs
  0x84c000), meaning actual generated code differs, not just an embedded
  UUID or timestamp. Every embedded absolute path was checked and is
  identical between the two builds (both built from the same checkout),
  which rules out the usual "different `--target-dir` leaked into a
  string" explanation. Root cause **not isolated** — the leading
  hypothesis is LLVM/rustc codegen-unit or symbol-ordering
  nondeterminism that debug-info emission makes visible and that full
  stripping (as `release` does) happens to discard, but this is a
  hypothesis, not a measurement, and is exactly the kind of claim D17
  would want probed further before being written down as fact.

**This is the actual release-blocking finding of this issue.** Shipping
`--profile dist` today means the owner cannot yet make a two-independent-
builders reproducibility claim for what actually gets published, only for
a build config that is not published. Fixing it is out of this pass's
remaining scope; flagged as a follow-up. Options worth trying first: bisect
which crate's codegen changes when debug info is added (a `cargo build
-Z build-std` style unit-by-unit diff, or simply re-running with
`debug = false` to confirm `release`'s reproducibility is really about
stripping and not something else profile.dist changes); or accept
`strip = "symbols"` for the shipped profile too and ship a separate
`.dSYM`/split-debuginfo artifact for crash symbolication instead of
inline line tables.

Neither profile has been measured on Linux (ELF build-id) or Windows yet —
the script's diagnostic pass covers both, but only macOS/aarch64 has
actually been run.

`SOURCE_DATE_EPOCH` is set and passed through but nothing in this tree
currently reads it (no `build.rs` exists at all, checked directly); it is
there for the day a dependency does.

### Signing and notarization — infrastructure only

**This agent does not have, and must never request, the credentials this
needs.** What follows is the pipeline and runbook the owner runs; nothing
here executes a signing or notarization step.

#### Sigstore/cosign keyless signing — the one step that needs no credential at all

`planning/13-correctness.md` §7.2 names this the *default* path, and it is
worth calling out separately from the platform-specific steps below: it
needs **no certificate, no key custody, and no CI secret whatsoever**. It
works by minting a short-lived certificate off the CI job's own OIDC token
(GitHub Actions issues one automatically) and recording the signature in
the public Sigstore transparency log (Rekor), so "who signed this" is "the
GitHub Actions workflow run at this URL", not a name.

```sh
# In GitHub Actions, with `permissions: id-token: write` on the job --
# no secrets. block needed.
cosign sign-blob --yes --output-signature vaco.sig --output-certificate vaco.pem dist/<version>/<triple>/vaco
cosign verify-blob --certificate vaco.pem --signature vaco.sig \
    --certificate-identity-regexp 'https://github.com/<owner>/vaco/.*' \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    dist/<version>/<triple>/vaco
```

This is the one part of this section that could run in CI today, entirely
unattended, with no owner action beyond adding the `id-token: write`
permission to the release workflow. It does not replace the platform
notarization below (Gatekeeper and SmartScreen do not consult Rekor), but
it is the cheapest possible "prove this artifact came from our CI and
was not tampered with after" guarantee and should ship first.

#### Platform-specific signing

#### macOS: Developer ID signing + notarization

What the owner supplies, and where:

| Secret | How it reaches the build | Notes |
|---|---|---|
| Developer ID Application certificate (`.p12`) | CI secret, base64-encoded, imported into a temporary keychain at build time | From an active Apple Developer Program membership |
| Certificate import password | CI secret | Only needed transiently to unlock the `.p12` |
| Apple ID or App Store Connect API key (`.p8`) + Key ID + Issuer ID | CI secrets | For `notarytool`; an API key is preferred over an Apple ID + app-specific password (no 2FA prompt in CI) |
| Developer Team ID | CI secret or plain config (not sensitive) | Needed to select the right identity out of the keychain |

Pipeline (run manually or wire into CI once the secrets above are
configured as repository/environment secrets — nothing here assumes a
specific CI provider):

```sh
# 1. Import the certificate into a throwaway keychain (never the login keychain).
security create-keychain -p "$TEMP_KEYCHAIN_PASSWORD" build.keychain
security import "$CERT_P12_PATH" -k build.keychain -P "$CERT_P12_PASSWORD" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple: -s -k "$TEMP_KEYCHAIN_PASSWORD" build.keychain
security list-keychains -d user -s build.keychain

# 2. Sign the binary. --options runtime enables the Hardened Runtime,
#    required for notarization.
codesign --force --options runtime --timestamp \
    --sign "Developer ID Application: <Owner Name> ($TEAM_ID)" \
    dist/<version>/<triple>/vaco
codesign --verify --verbose dist/<version>/<triple>/vaco

# 3. Notarize. Requires the binary inside a zip/dmg for submission.
ditto -c -k --keepParent dist/<version>/<triple>/vaco vaco.zip
xcrun notarytool submit vaco.zip \
    --key "$APP_STORE_CONNECT_API_KEY_PATH" \
    --key-id "$APP_STORE_CONNECT_KEY_ID" \
    --issuer "$APP_STORE_CONNECT_ISSUER_ID" \
    --wait

# 4. Staple (lets Gatekeeper verify offline). Only applies to bundles/dmgs,
#    not a bare executable -- if shipping a raw binary rather than a .app
#    or .dmg, notarization still applies via `notarytool`'s ticket, but
#    `stapler` has nothing to attach it to; distribute the notarization
#    receipt alongside instead, or wrap the binary in a signed .dmg first.
xcrun stapler staple vaco.dmg   # only if distributing a .dmg
```

Cleanup: `security delete-keychain build.keychain` at the end of the job
regardless of success or failure.

#### Windows: Authenticode signing

| Secret | How it reaches the build | Notes |
|---|---|---|
| Code-signing certificate (`.pfx`) | CI secret, base64-encoded | EV certificate strongly preferred — avoids SmartScreen reputation delay for a new publisher |
| Certificate password | CI secret | |
| Timestamp server URL | Plain config, not sensitive | e.g. `http://timestamp.digicert.com`; a timestamp keeps the signature valid after the cert expires |

```sh
signtool sign /f cert.pfx /p "$CERT_PASSWORD" /fd sha256 /tr "$TIMESTAMP_URL" /td sha256 dist\<version>\<triple>\vaco.exe
signtool verify /pa dist\<version>\<triple>\vaco.exe
```

#### Linux: detached signature

No OS-level gatekeeper equivalent; the norm is a detached GPG signature
plus the checksum file `scripts/package-release.sh` already writes.

| Secret | How it reaches the build | Notes |
|---|---|---|
| GPG private signing key | CI secret (armored, base64-encoded) | A release-signing subkey, not the owner's primary key |
| Key passphrase | CI secret | |

```sh
gpg --batch --import release-signing-key.asc
gpg --batch --yes --local-user "$SIGNING_KEY_ID" --detach-sign --armor \
    -o dist/<version>/<triple>/SHA256SUMS.asc \
    dist/<version>/<triple>/SHA256SUMS
```

#### What to verify before publishing

- `codesign --verify` / `signtool verify` / `gpg --verify` all pass on a
  clean machine that never had the signing keys.
- `xcrun notarytool submit ... --wait` reports `Accepted`, not
  `Invalid`.
- The reproducible-build check (`just verify-reproducible`) passed on the
  exact commit being released, and the SBOM/attribution file were
  generated from that same commit.

## How to change it

- New attribution source (a new Tier-A reference implementation gets
  read): add a `[[notice]]` to `provenance/third-party-notices.toml`
  cross-referencing the `provenance/` source id, fetch the actual licence
  text from upstream (don't recall it from memory — D17), then re-run
  `just licence-report-check`.
- New dependency: `deny.toml` and `about.toml` need the same licence on
  both allow lists if it introduces a licence not already accepted — keep
  them in sync, the script assumes they are.
- Changing what gets packaged: `scripts/package-release.sh` is the one
  place that lists the shipped binaries (`vaco`, `vaco-probe`) and the
  `dist` build profile.
- Adding a platform to the signing runbook above: follow the existing
  table-plus-script-block shape per OS; do not add a step that could
  execute with a real secret from this repository — this file is a runbook
  for a human/CI job with its own secret store, not something this agent
  or any automated agent should run end-to-end itself.

## Configuration

- `about.toml` — cargo-about's accepted-licence list (dependency half).
- `deny.toml` — `cargo-deny`'s licence allow-list, dependency bans, and
  advisory ignores (with justification comments for every entry).
- `provenance/third-party-notices.toml` — Tier-A reference-implementation
  attribution records (non-dependency half).
- `.gitignore`'s `/dist` entry — release artifacts are never committed.

## Dependencies

- [`cargo-about`](https://crates.io/crates/cargo-about) (`cargo install cargo-about --locked --features cli`) — build-time only, dependency-licence scan and report generation.
- [`cargo-deny`](https://crates.io/crates/cargo-deny) (`cargo install cargo-deny --locked`) — build-time only, licence/bans/sources/advisories gate.
- [`cargo-sbom`](https://crates.io/crates/cargo-sbom) (`cargo install cargo-sbom --locked`) — build-time only, SPDX/CycloneDX SBOM generation.
- Python 3 (system Python is enough — `scripts/gen_third_party_notices.py`
  deliberately avoids needing `pip install` anything, matching
  `scripts/unblock-manifests.py`'s precedent).

None of the above are Cargo workspace dependencies — they never appear in
`Cargo.lock` or a shipped binary, so D10's "pure Rust, zero FFI" bar applies
to them only as "a build-time tool is judged less harshly than a linked
dependency" (AGENT-CONSTRAINTS.md), which is the standard this section
holds them to.
