//! [`RowPublish`]: the cross-thread-safe row-band publish primitive Stage 2b
//! needs, built and tested here in isolation before anything in this crate's
//! production decode path is wired onto it.
//!
//! `docs/codec/hevc-wavefront-threading.md`'s "Stage 2b's concrete
//! prerequisites" section (and its own correction right after) found that
//! `EdgeMarks`/`CuGrid`/`SaoParamsGrid`'s existing row-banded shape --
//! `{ current: Band, published: Vec<Band>, current_band: usize }`, correct
//! only because Stage 1 runs one worker -- is not `Sync`-safe once more than
//! one row is genuinely being decoded at a time: two workers each wanting to
//! own the "current" row in the current design would data-race on the same
//! field. The fix is not a smaller version of `Vec<Band>`; it needs the same
//! split `ReconPlane` already has between a worker's own in-progress state
//! and `vaco_codec_core::picture`'s published tiles, at row rather than
//! per-CTU-tile granularity.
//!
//! [`RowPublish<T>`] is that split's read side: a fixed-size grid of
//! [`OnceLock<T>`] slots, one per row band, shared (`Arc` this module does
//! not itself impose -- callers choose their own sharing shape) across
//! however many row workers exist. A worker owns its own row's `T` privately
//! (an ordinary, unsynchronised value -- `EdgeBand`/`CuGridBand`/
//! `Vec<CtuSao>` today, unchanged) while decoding it, then calls
//! [`RowPublish::publish`] exactly once, handing the finished value over.
//! Every other worker reads it back through [`RowPublish::get`], which is
//! `None` until published and never blocks -- Stage 2's own wait/dispatch
//! logic (not yet written) is what turns "not yet published" into "wait", or
//! into a hard error if a bound meant to make that impossible was wrong; this
//! primitive only ever answers "is it there right now."
//!
//! `OnceLock<T>` is `Sync` whenever `T: Send + Sync`, and every `Band` type
//! this will hold (`Vec<bool>`, `Vec<u8>`, `Vec<CtuSao>`, ...) already is —
//! so `RowPublish<T>: Sync` for every `T` these three structures need, with
//! no `unsafe` anywhere in this module.
//!
//! Not yet wired into `EdgeMarks`/`CuGrid`/`SaoParamsGrid`, `Ctx`, `ctu.rs`
//! or `decoder.rs` — this lands as its own additive, separately-tested piece
//! first, per this item's own established practice (`vaco-codec-core` commit
//! `0af678e` landed the per-CTU-tile primitive `ReconPlane` needed the same
//! way, one commit before `1ba192d` wired it in).

#![allow(dead_code, reason = "landed ahead of the EdgeMarks/CuGrid/SaoParamsGrid wiring and the Stage 2 dispatch that will call it; see the module doc")]

use std::sync::OnceLock;

use vaco_core::{Error, Result};

/// A fixed-size, one-shot-per-slot publish board: `n` row bands, each
/// writable exactly once and freely, repeatedly readable by any number of
/// threads afterward with no further synchronisation.
#[derive(Debug, Clone)]
pub(crate) struct RowPublish<T> {
    slots: Vec<OnceLock<T>>,
}

impl<T> RowPublish<T> {
    /// `n` empty slots, one per row band a picture has — sized once at
    /// construction, matching `EdgeMarks`/`CuGrid`/`SaoParamsGrid`'s own
    /// `n_bands`, never grown or shrunk afterward.
    #[must_use]
    pub(crate) fn new(n: usize) -> Self {
        Self { slots: (0..n).map(|_| OnceLock::new()).collect() }
    }

    /// Row bands this board has slots for.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    /// Publish `value` for `row`, exactly once. A second publish of the same
    /// row (a bug in whatever drives the row loop, not a condition either
    /// caller-visible error variant this crate uses elsewhere describes) or
    /// an out-of-range `row` both refuse rather than silently overwrite or
    /// drop `value` — the same "wrong is a loud failure, not a quiet one"
    /// rule this document's own Stage 2 section states for a read past what
    /// was waited for.
    ///
    /// # Errors
    /// [`vaco_core::Error`] if `row` is out of range or already published.
    pub(crate) fn publish(&self, row: usize, value: T) -> Result<()> {
        let slot = self
            .slots
            .get(row)
            .ok_or(Error::InvalidData("vaco-codec-hevc: row publish index out of range"))?;
        slot.set(value)
            .map_err(|_dropped| Error::InvalidData("vaco-codec-hevc: row already published"))
    }

    /// `row`'s published value, or `None` if `row` is out of range or not
    /// published yet. Never blocks — a caller that must wait (Stage 2's own
    /// dispatch logic, not yet written) polls this or pairs it with its own
    /// wake mechanism; this primitive is deliberately just the shared,
    /// read-many/write-once storage, not a wait strategy.
    #[must_use]
    pub(crate) fn get(&self, row: usize) -> Option<&T> {
        self.slots.get(row).and_then(OnceLock::get)
    }

    /// Whether every slot has been published — the condition `finish`'s own
    /// eventual caller checks before treating a picture's row-banded data as
    /// safe to read as a whole (deblocking/SAO's post-walk passes today).
    #[must_use]
    pub(crate) fn all_published(&self) -> bool {
        self.slots.iter().all(|s| s.get().is_some())
    }

    /// Every published row's value, in row order, skipping any slot not yet
    /// published — the summation shape `SaoParamsGrid::budget_bytes` needs
    /// over whatever has actually been charged so far (a plain `Vec`'s own
    /// `iter()` never had gaps to skip; `RowPublish`'s can, mid-decode).
    pub(crate) fn iter(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().filter_map(OnceLock::get)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, reason = "test code over fixed scenarios")]
mod tests {
    use super::RowPublish;

    #[test]
    fn a_fresh_board_has_no_published_rows() {
        let board: RowPublish<u32> = RowPublish::new(4);
        assert_eq!(board.len(), 4);
        for row in 0..4 {
            assert_eq!(board.get(row), None);
        }
        assert!(!board.all_published());
    }

    #[test]
    fn publish_then_get_round_trips_the_value() {
        let board = RowPublish::new(3);
        board.publish(1, vec![1u8, 2, 3]).expect("first publish of row 1 succeeds");
        assert_eq!(board.get(0), None);
        assert_eq!(board.get(1), Some(&vec![1u8, 2, 3]));
        assert_eq!(board.get(2), None);
        assert!(!board.all_published());
    }

    #[test]
    fn publishing_every_row_makes_all_published_true() {
        let board = RowPublish::new(2);
        board.publish(0, "a").expect("row 0 publishes");
        assert!(!board.all_published());
        board.publish(1, "b").expect("row 1 publishes");
        assert!(board.all_published());
    }

    #[test]
    fn publishing_out_of_range_is_refused() {
        let board = RowPublish::new(2);
        assert!(board.publish(2, 0u8).is_err());
        assert!(board.publish(usize::MAX, 0u8).is_err());
    }

    #[test]
    fn publishing_the_same_row_twice_is_refused_and_the_first_value_survives() {
        let board = RowPublish::new(1);
        board.publish(0, 10u32).expect("first publish succeeds");
        assert!(board.publish(0, 20u32).is_err());
        // The refused second publish must not have clobbered the first --
        // this is the whole safety property `OnceLock::set` gives for free,
        // pinned here as a test rather than only argued in the doc comment.
        assert_eq!(board.get(0), Some(&10u32));
    }

    #[test]
    fn rows_may_publish_out_of_order() {
        // Real WPP dispatch will not necessarily finish rows in ascending
        // order under contention (a later row's worker can finish before an
        // earlier one if it got scheduled first and had less work) -- a
        // board that only worked for in-order publish would be the wrong
        // primitive for this job.
        let board = RowPublish::new(3);
        board.publish(2, "third").expect("row 2 publishes first");
        board.publish(0, "first").expect("row 0 publishes second");
        assert_eq!(board.get(0), Some(&"first"));
        assert_eq!(board.get(1), None);
        assert_eq!(board.get(2), Some(&"third"));
        board.publish(1, "second").expect("row 1 publishes last");
        assert!(board.all_published());
    }

    #[test]
    fn concurrent_publish_and_read_across_real_threads_is_race_free_and_sees_committed_values() {
        // The property this whole primitive exists for: N writer threads
        // each publish one row while, concurrently, reader threads spin
        // polling every row's `get`. No `unsafe` appears anywhere in this
        // module, so the compiler already rules out undefined behaviour;
        // what this test pins is the *functional* contract -- a reader that
        // ever observes `Some(v)` for a row must see the exact value that
        // row's writer published, never a torn or default one, regardless
        // of how the two threads interleave.
        const ROWS: usize = 32;
        let board: RowPublish<usize> = RowPublish::new(ROWS);
        std::thread::scope(|scope| {
            for row in 0..ROWS {
                let board = &board;
                scope.spawn(move || {
                    // A little deliberately non-uniform work per row so rows
                    // do not all publish in lockstep -- closer to real CTU
                    // rows, which take different amounts of time.
                    let busy = row % 5;
                    let mut acc = 0usize;
                    for i in 0..busy * 1000 {
                        acc = acc.wrapping_add(i);
                    }
                    std::hint::black_box(acc);
                    board.publish(row, row * row).unwrap_or_else(|e| unreachable!("row {row} publish failed: {e}"));
                });
            }
            for _ in 0..4 {
                let board = &board;
                scope.spawn(move || {
                    let mut seen = [false; ROWS];
                    while !seen.iter().all(|&s| s) {
                        for (row, seen_row) in seen.iter_mut().enumerate() {
                            if let Some(&v) = board.get(row) {
                                assert_eq!(v, row * row, "row {row} published a torn or wrong value");
                                *seen_row = true;
                            }
                        }
                        std::thread::yield_now();
                    }
                });
            }
        });
        assert!(board.all_published());
        for row in 0..ROWS {
            assert_eq!(board.get(row), Some(&(row * row)));
        }
    }

    #[test]
    fn iter_yields_only_published_rows_in_order_skipping_gaps() {
        let board = RowPublish::new(5);
        board.publish(3, 30u32).expect("row 3 publishes");
        board.publish(1, 10u32).expect("row 1 publishes");
        // Rows 0, 2 and 4 are never published -- `iter` must skip them
        // rather than yielding a default, since `SaoParamsGrid::
        // budget_bytes`'s own sum must reflect exactly what has been
        // charged so far, mid-decode, not a full picture's worth.
        let seen: Vec<u32> = board.iter().copied().collect();
        assert_eq!(seen, vec![10, 30]);
    }

    #[test]
    fn cloning_preserves_published_rows_and_gaps() {
        // `EdgeMarks`/`CuGrid`/`SaoParamsGrid` all derive `Clone` (the
        // deblock-lag probe's own test machinery, `planning/E2E-GAPS.md`
        // ss34, needs two independent instances from one decode) -- pinning
        // that `RowPublish` itself clones correctly, rather than
        // discovering it the first time one of those three is wired onto
        // it and its own `#[derive(Clone)]` silently stops compiling or
        // silently resets.
        let board = RowPublish::new(3);
        board.publish(0, 1u32).expect("row 0 publishes");
        board.publish(2, 3u32).expect("row 2 publishes");
        let cloned = board.clone();
        assert_eq!(cloned.get(0), Some(&1));
        assert_eq!(cloned.get(1), None);
        assert_eq!(cloned.get(2), Some(&3));
        // And the clone is independent -- publishing into the original
        // after cloning must not appear in the clone.
        board.publish(1, 2u32).expect("row 1 publishes after the clone was taken");
        assert_eq!(board.get(1), Some(&2));
        assert_eq!(cloned.get(1), None);
    }

    #[test]
    fn row_publish_is_send_and_sync_for_the_band_types_this_item_needs() {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RowPublish<Vec<bool>>>(); // EdgeMarks's own EdgeBand fields
        assert_send_sync::<RowPublish<Vec<u8>>>(); // CuGrid's own depth/mode-shaped bands
        assert_send_sync::<RowPublish<Vec<i32>>>(); // CuGrid's own mv-shaped bands
    }
}
