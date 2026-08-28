//! Shared fuzz-target scaffolding (plan 13 §2.3, §2.5.4; work packages X-03
//! and QA-05): [`guard::Guard`] plus a re-export of `vaco-limits`'s
//! `ProgressGuard`, structured `arbitrary` input types ([`inputs`]), and
//! corpus/regression replay ([`replay`]).
//!
//! # Why one crate for these three things
//!
//! All three exist to stop every fuzz target from re-deriving the same
//! handful of ideas: how to fail fast on a hang instead of waiting for
//! libFuzzer's own timeout, how to generate inputs that reach interesting
//! code instead of being rejected in the first microsecond, and how to keep
//! a fixed bug fixed. `fuzz/fuzz_targets/*.rs` files depend on this crate the
//! same way they depend on `vaco-bitstream` or any other shared crate — a
//! `//! fuzz-crate: vaco-fuzz-support` header plus `cargo xtask gen-fuzz`.
//!
//! This crate is **not** itself part of the excluded `fuzz/` workspace: it
//! is an ordinary `crates/tool/*` member, forbidding `unsafe` and gated by
//! every normal workspace lint, specifically so it can be unit-tested with
//! plain `cargo test -p vaco-fuzz-support` rather than only ever exercised
//! inside a `cargo fuzz` run.
//!
//! # What is a seam, not a finished integration
//!
//! [`replay::replay_dir`]/[`replay::replay_dir_or_panic`] are the *mechanism*
//! plan 13 §2.5.4 point 5 asks for. Wiring one `#[test]` per existing fuzz
//! target that calls the target's own decode/parse body against
//! `fuzz/seeds/<target>/` is a separate, ~190-target integration this crate
//! does not attempt — see `replay.rs`'s module docs for why, and for exactly
//! what is and is not implied by this module existing.

#![forbid(unsafe_code)]

pub mod guard;
pub mod inputs;
pub mod replay;

pub use guard::Guard;
pub use inputs::{BoundedBytes, Dim, FuzzPacket};
pub use replay::{ReplayFailure, replay_dir, replay_dir_or_panic};
/// The stepping-contract guard (plan 13 §2.2.4a). Re-exported from
/// `vaco-limits`, not redefined here — see `guard.rs`'s module docs for why
/// there is exactly one implementation of this in the whole workspace.
pub use vaco_limits::ProgressGuard;
