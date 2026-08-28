#!/usr/bin/env bash
# One release build: binaries, checksums, SBOM, attribution file (QA-10, #182).
#
# Deliberately stops BEFORE signing/notarization. Those need the owner's own
# credentials (a Developer ID certificate, an Apple ID app-specific password
# or API key, a Windows Authenticode certificate) which this script must
# never handle, request, or store -- see docs/release-engineering.md for the
# runbook that picks up from here.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)"
TARGET_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
OUT="dist/${VERSION}/${TARGET_TRIPLE}"
mkdir -p "$OUT"

echo "== building vaco-cli, vaco-probe ($TARGET_TRIPLE, profile: dist) =="
# The `dist` profile (root Cargo.toml) is release + line-tables-only debug
# info, so a crash report is still symbolisable without shipping full debug
# sections.
cargo build --profile dist --locked -p vaco-cli -p vaco-probe --target-dir "$OUT/build"

for bin in vaco vaco-probe; do
    src="$OUT/build/dist/$bin"
    if [ ! -f "$src" ]; then
        src="$OUT/build/dist/$bin.exe"
    fi
    if [ ! -f "$src" ]; then
        echo "expected binary not found for $bin under $OUT/build/dist" >&2
        exit 1
    fi
    cp "$src" "$OUT/"
done
rm -rf "$OUT/build"

echo "== checksums =="
( cd "$OUT" && shasum -a 256 vaco vaco-probe 2>/dev/null > SHA256SUMS || \
  sha256sum vaco vaco-probe > SHA256SUMS )
cat "$OUT/SHA256SUMS"

echo "== SBOM =="
mkdir -p "$OUT/sbom"
for pkg in vaco-cli vaco-probe; do
    for fmt in spdx_json_2_3 cyclone_dx_json_1_5; do
        if command -v cargo-sbom >/dev/null 2>&1 || cargo sbom --help >/dev/null 2>&1; then
            cargo sbom --cargo-package "$pkg" --output-format "$fmt" \
                > "$OUT/sbom/${pkg}.${fmt}.json"
        else
            echo "cargo-sbom not installed (cargo install cargo-sbom --locked) -- skipping SBOM" >&2
        fi
    done
done

echo "== attribution =="
if cargo about --help >/dev/null 2>&1; then
    python3 scripts/gen_third_party_notices.py -o "$OUT/THIRD_PARTY_LICENSES.html"
else
    echo "cargo-about not installed (cargo install cargo-about --locked --features cli) -- skipping attribution file" >&2
fi

echo "== done: $OUT =="
ls -la "$OUT"
echo
echo "NOT signed, NOT notarized. See docs/release-engineering.md for that step --"
echo "it needs credentials this script must never touch."
