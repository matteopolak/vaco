# Filter Benchmark Tracking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add registry-complete filter benchmark measurement and reproducible CI result tracking.

**Architecture:** A new layer-10 `vaco-bench` tool discovers filters from `vaco-registry`, measures default instantiation, emits identity-safe JSONL, and compares only like-for-like baselines. Dedicated CI workflows run an advisory PR smoke and persist scheduled results on `bench-results`.

**Tech Stack:** Rust standard library, Vaco filter registry, GitHub Actions, workspace `divan` benchmarks.

---

### Task 1: Result model and registry coverage

**Files:**
- Create: `crates/tool/vaco-bench/Cargo.toml`
- Create: `crates/tool/vaco-bench/src/lib.rs`
- Create: `crates/tool/vaco-bench/benches/filters.rs`
- Create: `crates/tool/vaco-bench/tests/filter_suite.rs`

- [x] Write tests asserting one successful row for every name returned by `vaco_registry::filters()` and no duplicate benchmark identities.
- [x] Run the focused test and confirm it fails because the suite API does not exist.
- [x] Implement registry-derived Divan cases, deterministic warmup, batched timing, and median/MAD/min/p95 summaries.
- [x] Run the focused test and confirm it passes.

### Task 2: JSONL and comparison semantics

**Files:**
- Modify: `crates/tool/vaco-bench/src/lib.rs`
- Modify: `crates/tool/vaco-bench/tests/filter_suite.rs`

- [x] Write tests for JSONL round-trip, exact fingerprint matching, absent baselines, and explicit matched-row threshold failures.
- [x] Run the focused tests and confirm the new assertions fail.
- [x] Implement JSONL I/O and comparison without adding a serialization dependency.
- [x] Run the focused tests and confirm they pass.

### Task 3: CLI, workflows, and documentation

**Files:**
- Create: `crates/tool/vaco-bench/src/main.rs`
- Create: `crates/tool/vaco-bench/tests/cli.rs`
- Create: `.github/workflows/filter-benchmarks.yml`
- Modify: `Justfile`
- Create: `docs/tool/vaco-bench.md`
- Modify: `docs/README.md` through `cargo xtask gen-docs-index`

- [x] Add CLI integration coverage for JSON output and incomparable baselines.
- [x] Implement `list` and `filter` commands with explicit sampling, output, baseline, and threshold options.
- [x] Add advisory pull-request smoke plus scheduled/manual tracked measurement storage.
- [x] Document scope, commands, comparison identity, storage, configuration, and dependencies.
- [x] Run focused tests, release smoke at `-j2`, Clippy, formatting, policy checks, and generated-doc checks in a private target directory.
