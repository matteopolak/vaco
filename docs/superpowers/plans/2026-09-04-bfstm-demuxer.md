# BFSTM/BCSTM demuxer implementation plan

**Goal:** Register and verify the measured stereo DSP-ADPCM subset of Nintendo
BFSTM/BCSTM without changing BRSTM.

**Architecture:** A standalone demuxer parses the source-defined section and
reference layout, keeps all values in host order, and serializes the synthesized
packet prefix using the container's byte order. Registry generation remains the
single source of truth for listing and dispatch.

**Sources:** `mk8.tockdom.com/wiki/BFSTM` and `3dbrew.org/wiki/BCSTM`, with
packet behavior measured using the installed `ffprobe` 9.0.1 binary.

---

## Task 1: Add failing format tests and fixture

**Files:**

- Create: `crates/format/vaco-format-misc-audio/tests/bfstm.rs`
- Create: `crates/format/vaco-format-misc-audio/tests/fixtures/bfstm.bfstm`
- Modify: `crates/format/vaco-format-misc-audio/tests/differential.rs`
- Modify: `crates/format/vaco-format-misc-audio/tests/properties.rs`

Build a source-derived fixture with distinct coefficients, SEEK histories,
channel payloads, and final padding. Assert both byte orders and magics produce
the oracle-measured stream and packet fields. Add malformed-reference and named
scope-refusal cases.

Run the focused test before implementation and confirm it fails because the
`bfstm` module is absent:

```sh
CARGO_INCREMENTAL=0 cargo test -p vaco-format-misc-audio --test bfstm \
  --target-dir /private/tmp/vaco-target-tracker-slice
```

## Task 2: Implement the demuxer

**Files:**

- Create: `crates/format/vaco-format-misc-audio/src/bfstm.rs`
- Modify: `crates/format/vaco-format-misc-audio/src/lib.rs`

Add probe/open/read-packet paths, checked section/reference resolution, endian
helpers, scope checks, and exact packet synthesis. Run the focused test again;
it must pass.

## Task 3: Register atomically

**Files:**

- Modify: `crates/format/vaco-format-misc-audio/vaco-component.toml`
- Modify (generated): `crates/registry/vaco-registry/Cargo.toml`
- Modify (generated): `crates/registry/vaco-registry/src/generated.rs`
- Modify (generated): `docs/README.md`
- Modify (generated): `docs/format-coverage.md`

Add one `bfstm` component fragment with extensions `bfstm,bcstm`, then run the
registry generator once so listing and dispatch land together:

```sh
CARGO_INCREMENTAL=0 cargo run -p xtask -- gen-registry \
  --target-dir /private/tmp/vaco-target-tracker-slice
```

## Task 4: Record provenance and developer documentation

**Files:**

- Modify: `provenance/sources.toml`
- Modify: `docs/format/vaco-format-misc-audio.md`

Declare the two public byte-layout sources and the black-box fixture sweep.
Replace BFSTM's deferred entry with its measured contract and extension notes.

## Task 5: Verify behavior and repository invariants

Run the full crate tests and scoped lint, followed by registry generation check,
provenance, layer, and reachability checks. Compare the checked-in fixture with
`ffprobe` and the built `vaco-probe`, recording stream count, sample rate,
channels, time base, duration ticks, packet count, packet sizes, packet
timestamps, and packet duration ticks. Treat any structural mismatch as a test
failure.

## Task 6: Commit and report

Create the implementation commit with the private-index/CAS recipe and these
trailers:

```text
Vaco-Provenance: spec
Vaco-Spec-Ref: mk8-bfstm-format File Format; 3dbrew-bcstm-format Header through Reference Types
Vaco-Clean-Room: yes
```

Push `main`, post the measured evidence on #620, leave the issue open while any
listed game-audio format remains, and remove
`/private/tmp/vaco-target-tracker-slice` plus exploratory fixtures.
