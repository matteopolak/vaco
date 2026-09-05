//! `vaco-filter-mm` — the multimedia/plumbing filter family: `concat`,
//! `select`/`aselect`, `segment`/`asegment`, `streamselect`/`astreamselect`,
//! `trim`/`atrim`, `loop`/`aloop`, `reverse`/`areverse`, `cue`/`acue`,
//! `realtime`/`arealtime`, `latency`/`alatency`, `bench`/`abench`,
//! `perms`/`aperms`, `metadata`/`ametadata`, `sidedata`/`asidedata`,
//! `sendcmd`/`asendcmd`, `split`/`asplit`, `setpts`/`asetpts`, `null`/`anull`,
//! `interleave`/`ainterleave`, plus graph-plumbing filters carried over from
//! this crate's previous name: `copy`/`acopy`, `settb`/`asettb`,
//! `nullsrc`/`anullsrc`, `nullsink`/`anullsink`, `color`.
//!
//! `color`/`nullsrc`/`nullsink`/`anullsrc` conceptually belong in
//! `vaco-filter-source`/`vaco-filter-asource`, but those crates don't yet
//! register these names, and deleting working filters with nothing to
//! replace them would regress the CLI — they stay here under the
//! single-writer rule until that migration happens.
//! Not implemented: `avsynctest` (disproportionate to this crate's plumbing
//! scope), `cmdsocket`/`acmdsocket` (need a real listening socket, outside a
//! filter's normal scope), `aeval` (the reference's own docs call it slow
//! and recommend a dedicated filter instead). `sendcmd`/`asendcmd` parse the
//! command grammar, track enter/leave edges and dispatch fired commands through
//! `vaco-filter-core`'s deferred `FilterContext` request channel.
//! Two filters carry noteworthy bugs found while building this crate: see
//! `select`'s module doc for a routing formula that disagreed with the
//! reference, and `streamselect`'s for a fuzzing-found allocation bug.
//! `buffer`/`abuffer`/`buffersink`/`abuffersink` live in `vaco-filter-core`
//! instead of a leaf crate, because they need privileged access to link
//! internals — a buffer source pushes directly into the link queue and a
//! buffer sink holds frames with no downstream.
//! [`vaco_filter_core::Graph::add_source`]/`add_sink`/`send`/`recv`/
//! `close_source`/`source_wants`/`sink_format` already is that API; mapping
//! the DSL spellings onto it is `vaco-filter-graph` or `vaco-cli-core`'s
//! job.
//! Shape: one module per filter (or per closely related pair — `null`+`anull`+
//! `copy`+`acopy` in `passthrough.rs`, the two `setpts` variants together,
//! and so on), each exposing `pub const DESC: FilterDesc` and a
//! crate-private `create`. [`registry::MmRegistry`] dispatches by name.
//! Every per-filter `Options`/`State`/`Mode` type is `pub(crate)`, the
//! simplest response to ~35 filters converging on the same short type
//! names.
#![forbid(unsafe_code)]

pub mod color;
pub mod concat;
pub mod interleave;
pub mod looping;
pub mod metadata;
pub mod misc;
pub mod nullsink;
pub mod nullsrc;
pub mod passthrough;
pub mod reverse;
pub mod segment;
pub mod select;
pub mod sendcmd;
pub mod setpts;
pub mod settb;
pub mod split;
pub mod streamselect;
pub mod trim;

pub mod registry;

pub use registry::MmRegistry;

/// Convert a finite media duration into source frames without floating-point
/// clock arithmetic.
pub(crate) fn frame_budget(duration: vaco_core::Duration, rate: vaco_core::Rational) -> u64 {
    duration
        .to_ticks_rounding(rate.inverse(), vaco_core::Rounding::NearestAwayFromZero)
        .and_then(|frames| u64::try_from(frames.max(0)).ok())
        .unwrap_or(0)
}

/// Convert a finite media duration into source samples without floating-point
/// clock arithmetic.
pub(crate) fn sample_budget(duration: vaco_core::Duration, sample_rate: u32) -> u64 {
    let rate = i32::try_from(sample_rate.max(1)).unwrap_or(1);
    duration
        .to_ticks_rounding(
            vaco_core::Rational::new(1, rate),
            vaco_core::Rounding::NearestAwayFromZero,
        )
        .and_then(|samples| u64::try_from(samples.max(0)).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use vaco_core::{Duration, Rational};

    #[test]
    fn frame_budget_retains_a_large_awkward_clock_duration() {
        let frames = 9_007_199_254_740_993_i64;
        let duration = Duration::from_ticks(frames, Rational::new(1_001, 30_000))
            .unwrap_or(Duration::ZERO);
        assert_eq!(super::frame_budget(duration, Rational::new(30_000, 1_001)), frames as u64);
    }

    #[test]
    fn sample_budget_retains_a_large_awkward_clock_duration() {
        let samples = 9_007_199_254_740_993_i64;
        let duration = Duration::from_ticks(samples, Rational::new(1, 48_000))
            .unwrap_or(Duration::ZERO);
        assert_eq!(super::sample_budget(duration, 48_000), samples as u64);
    }
}
