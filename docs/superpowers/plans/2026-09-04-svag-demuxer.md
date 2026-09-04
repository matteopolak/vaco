# SVAG Demuxer Implementation Plan

> **For agentic workers:** Execute inline in the current shared checkout. The
> repository forbids a worktree for this task and requires private-index/CAS
> commits. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a reachable Konami PS2 SVAG demuxer whose stream fields and packet stream match `ffprobe` 9.0.1.

**Architecture:** A bespoke `SvagDemuxer` parses the fixed little-endian header and reads interleaved packets to physical EOF. Registry generation remains the sole dispatch/listing source, and a synthetic fixture pins the measured black-box behavior.

**Tech Stack:** Rust, `vaco-format-core`, `vaco-io`, `vaco-packet`, generated Vaco registry, `ffprobe` 9.0.1.

---

### Task 1: Pin SVAG behavior with red tests

**Files:**

- Create: `crates/format/vaco-format-misc-audio/tests/svag.rs`
- Create: `crates/format/vaco-format-misc-audio/tests/fixtures/svag.svag`

- [ ] **Step 1: Write a synthetic-file helper and failing integration tests**

Build `VAGm` plus four little-endian `u32` fields and deterministic payload
bytes. Test `sample_rate = 44_100`, `channels = 2`, `interleave = 16`,
`data_size = 320`, ten 32-byte packets, `duration_ts = 280`, packet positions
starting at byte 20, and 28-tick timestamp increments. Add a second case with
`interleave = 32` expecting five 64-byte packets and 56-tick increments. Add a
short-tail case expecting a final `CORRUPT` packet with no timestamps.

- [ ] **Step 2: Run the focused test and verify RED**

Run only after `uptime` reports load below 8:

```sh
CARGO_INCREMENTAL=0 cargo test -p vaco-format-misc-audio --test svag \
  --target-dir /private/tmp/vaco-target-tracker-slice
```

Expected: compilation failure because `vaco_format_misc_audio::svag` does not
exist.

### Task 2: Implement the minimal demuxer

**Files:**

- Create: `crates/format/vaco-format-misc-audio/src/svag.rs`
- Modify: `crates/format/vaco-format-misc-audio/src/lib.rs`

- [ ] **Step 1: Parse and validate the fixed header**

Export `probe`, `DEMUXER`, and `SvagDemuxer`. Read `VAGm`, `data_size`,
`sample_rate`, `channels`, and `interleave`; reject zero or non-block-aligned
geometry and checked-multiplication overflow. Create one audio stream at time
base `1 / sample_rate`, leaving codec identity unset like `vag`.

- [ ] **Step 2: Emit reference-shaped packets**

Read `channels * interleave` bytes through physical EOF. Full packets receive
PTS/DTS and `DurationTicks(28 * interleave / 16)`; a short packet retains unset
timestamps and receives `KEY | CORRUPT`. Derive stream duration from declared
size in 16-byte-per-channel blocks, not from physical EOF.

- [ ] **Step 3: Run the focused test and verify GREEN**

Run the Task 1 command. Expected: all SVAG integration tests pass.

### Task 3: Register and differentially verify SVAG

**Files:**

- Modify: `crates/format/vaco-format-misc-audio/vaco-component.toml`
- Modify: `crates/registry/vaco-registry/Cargo.toml` (generated)
- Modify: `crates/registry/vaco-registry/src/generated.rs` (generated)
- Modify: `crates/format/vaco-format-misc-audio/tests/differential.rs`
- Modify: `crates/format/vaco-format-misc-audio/tests/properties.rs`
- Modify: `provenance/sources.toml`

- [ ] **Step 1: Add the component row after the descriptor exists**

Add the `svag` demuxer row with extension `svag`, then run:

```sh
CARGO_INCREMENTAL=0 cargo xtask gen-registry \
  --target-dir /private/tmp/vaco-target-tracker-slice
```

- [ ] **Step 2: Add differential and arbitrary-input coverage**

Add the fixture row expecting 44.1 kHz stereo, 6,349 microseconds, and ten
32-byte packets. Include `svag::DEMUXER` in the property-test descriptor list.
Register the forum layout note and the `ffprobe` 9.0.1 field/packet sweeps in
`provenance/sources.toml`.

- [ ] **Step 3: Verify the generated registry path**

Run focused crate tests and registry tests, then use the real probe CLI on the
fixture. Compare stream count, sample rate, channel count, duration ticks,
packet count, sizes, positions, PTS, and durations with `ffprobe`.

### Task 4: Document, verify, and land

**Files:**

- Modify: `docs/format/vaco-format-misc-audio.md`
- Modify: `docs/README.md` (generated only if its generator changes output)

- [ ] **Step 1: Update the feature documentation**

Move `svag` out of the deferred list; document the header, packet geometry,
declared-size/physical-EOF split, short-tail behavior, source, configuration,
dependencies, and exact measured fixture row.

- [ ] **Step 2: Run verification below the load gate**

Run `cargo fmt -p vaco-format-misc-audio`, focused tests, registry generation
check, provenance check, layer check, docs check where practical, and the real
CLI comparison, always with `CARGO_INCREMENTAL=0` and the required private
target directory.

- [ ] **Step 3: Commit with clean-room trailers via private index/CAS**

Use a conventional `feat(format-misc-audio): ...` message plus:

```text
Vaco-Provenance: blackbox
Vaco-Spec-Ref: vaco-format-misc-audio-svag-fixtures-probe
Vaco-Clean-Room: yes
```

Push, comment measured evidence on issue #620, leave it open because other
named containers remain, and delete `/private/tmp/vaco-target-tracker-slice`.
