//! Cross-thread row-band publication.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use vaco_codec_core::picture::{BlockScratch, PictureSpec, PlaneSpec, ProgressPicture};
use vaco_limits::{Budget, Limits};

fn spec(width: u32, height: u32, band_h: u32, guard: u32) -> PictureSpec {
    PictureSpec::new(vec![PlaneSpec::new(width, height)])
        .with_band_height(band_h)
        .with_guard(guard)
}

/// Row `y` is filled with the byte `y as u8`, so any read can check itself.
fn fill(writer: &mut vaco_codec_core::PictureWriter, k: usize) {
    let mut band = writer.band_mut(0, k).unwrap();
    let first = band.first_row();
    for r in 0..band.rows() {
        let value = (first + r) as u8;
        let row = band.row_mut(r).unwrap();
        row.fill(value);
    }
}

#[test]
fn published_rows_read_back_exactly() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, r) = ProgressPicture::allocate(&spec(64, 40, 16, 4), 0, &mut budget).unwrap();
    assert_eq!(w.band_count(0), 3);
    for k in 0..3 {
        fill(&mut w, k);
        w.publish_through(0, k).unwrap();
    }
    let view = r.finished(0).unwrap();
    assert_eq!(view.rows(), 40);
    for y in 0..40u32 {
        assert_eq!(view.row(y).unwrap()[0], y as u8, "row {y}");
    }
    w.finish().unwrap();
}

#[test]
fn ready_rows_advance_only_as_bands_are_published() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, r) = ProgressPicture::allocate(&spec(32, 48, 16, 4), 0, &mut budget).unwrap();
    assert_eq!(r.ready_rows(0), 0);
    assert!(r.try_rows(0, 0).is_none());
    fill(&mut w, 0);
    w.publish_through(0, 0).unwrap();
    assert_eq!(r.ready_rows(0), 16);
    assert!(r.try_rows(0, 15).is_some());
    assert!(r.try_rows(0, 16).is_none());
    fill(&mut w, 1);
    fill(&mut w, 2);
    w.publish_through(0, 2).unwrap();
    assert_eq!(r.ready_rows(0), 48);
    w.finish().unwrap();
}

#[test]
fn a_band_may_not_be_written_after_publication() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, _r) = ProgressPicture::allocate(&spec(16, 32, 16, 4), 0, &mut budget).unwrap();
    fill(&mut w, 0);
    w.publish_through(0, 0).unwrap();
    assert!(w.band_mut(0, 0).is_err());
}

#[test]
fn guard_rows_carry_the_previous_band_so_reads_above_a_seam_stay_contiguous() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, r) = ProgressPicture::allocate(&spec(64, 64, 16, 8), 0, &mut budget).unwrap();
    for k in 0..4 {
        fill(&mut w, k);
        w.publish_through(0, k).unwrap();
    }
    let view = r.finished(0).unwrap();
    let mut scratch = BlockScratch::new(&mut budget, 32, 32).unwrap();
    // Rows 12..20 straddle the seam at row 16: the region starts inside band
    // one's guard rows, so it is served contiguously from band one.
    let block = view.block(0, 12, 8, 8, &mut scratch).unwrap();
    assert_eq!(block.stride, 64, "expected the band's natural stride");
    for j in 0..8u32 {
        let row = &block.data[j as usize * block.stride..];
        assert_eq!(row[0], (12 + j) as u8, "row {}", 12 + j);
    }
}

#[test]
fn a_region_spanning_more_than_the_guard_takes_the_copy_path() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, r) = ProgressPicture::allocate(&spec(64, 64, 16, 4), 0, &mut budget).unwrap();
    for k in 0..4 {
        fill(&mut w, k);
        w.publish_through(0, k).unwrap();
    }
    let view = r.finished(0).unwrap();
    let mut scratch = BlockScratch::new(&mut budget, 16, 32).unwrap();
    // Twenty rows from row 4 cross the seam at 16 by more than the four guard
    // rows, so no single band holds them.
    let block = view.block(0, 4, 8, 20, &mut scratch).unwrap();
    assert_eq!(block.stride, 8, "expected the packed scratch stride");
    for j in 0..20u32 {
        assert_eq!(block.data[j as usize * 8], (4 + j) as u8);
    }
}

#[test]
fn out_of_picture_reads_replicate_the_edge() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, r) = ProgressPicture::allocate(&spec(8, 8, 8, 0), 0, &mut budget).unwrap();
    fill(&mut w, 0);
    w.publish_through(0, 0).unwrap();
    let view = r.finished(0).unwrap();
    let mut scratch = BlockScratch::new(&mut budget, 8, 8).unwrap();
    let block = view.block(-2, -2, 4, 4, &mut scratch).unwrap();
    // Rows above the picture replicate row zero.
    assert_eq!(block.data[0], 0);
    assert_eq!(block.data[4], 0);
    assert_eq!(block.data[8], 0);
    assert_eq!(block.data[12], 1);
}

#[test]
fn a_single_band_picture_is_always_contiguous() {
    let mut budget = Budget::new(Limits::permissive());
    let s = PictureSpec::new(vec![PlaneSpec::new(64, 64)]).single_band();
    let (mut w, r) = ProgressPicture::allocate(&s, 0, &mut budget).unwrap();
    assert_eq!(w.band_count(0), 1);
    fill(&mut w, 0);
    w.publish_through(0, 0).unwrap();
    let view = r.finished(0).unwrap();
    let mut scratch = BlockScratch::new(&mut budget, 64, 64).unwrap();
    for y in (0..56).step_by(8) {
        let block = view.block(0, y, 8, 8, &mut scratch).unwrap();
        assert_eq!(block.stride, 64, "row {y} should not have been copied");
    }
}

#[test]
fn reading_past_the_published_watermark_is_refused() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, r) = ProgressPicture::allocate(&spec(32, 64, 16, 4), 0, &mut budget).unwrap();
    fill(&mut w, 0);
    w.publish_through(0, 0).unwrap();
    let view = r.try_rows(0, 15).unwrap();
    let mut scratch = BlockScratch::new(&mut budget, 32, 32).unwrap();
    assert!(view.block(0, 8, 8, 16, &mut scratch).is_err());
    w.finish().unwrap();
}

#[test]
fn a_reader_blocks_until_the_writer_publishes() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, r) = ProgressPicture::allocate(&spec(32, 64, 16, 4), 0, &mut budget).unwrap();
    let reached = Arc::new(AtomicBool::new(false));
    let reader = {
        let r = r.clone();
        let reached = Arc::clone(&reached);
        std::thread::spawn(move || {
            let view = r.wait_rows(0, 47).unwrap();
            reached.store(true, Ordering::Release);
            assert!(view.rows() >= 48);
            (0..48u32).all(|y| view.row(y).is_some_and(|row| row[0] == y as u8))
        })
    };
    for k in 0..3 {
        fill(&mut w, k);
        assert!(!reached.load(Ordering::Acquire) || k == 2);
        w.publish_through(0, k).unwrap();
    }
    assert!(reader.join().unwrap());
    w.finish().unwrap();
}

#[test]
fn a_dropped_writer_wakes_every_waiter_with_an_error() {
    let mut budget = Budget::new(Limits::permissive());
    let (w, r) = ProgressPicture::allocate(&spec(32, 64, 16, 4), 0, &mut budget).unwrap();
    let reader = {
        let r = r.clone();
        std::thread::spawn(move || r.wait_rows(0, 63).map(|_| ()))
    };
    // The producing task gives up — a panic, a cancellation, an early return.
    drop(w);
    assert!(reader.join().unwrap().is_err());
    assert!(r.failed(0));
}

#[test]
fn slice_jobs_get_disjoint_band_ranges() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, r) = ProgressPicture::allocate(&spec(32, 64, 16, 4), 0, &mut budget).unwrap();
    {
        let mut ranges = w.split_bands_mut(0, &[0..2, 2..4]).unwrap();
        assert_eq!(ranges.len(), 2);
        let (a, b) = ranges.split_at_mut(1);
        std::thread::scope(|s| {
            let ja = &mut a[0];
            let jb = &mut b[0];
            s.spawn(move || {
                for k in 0..2 {
                    let mut band = ja.band_mut(k).unwrap();
                    let first = band.first_row();
                    for rr in 0..band.rows() {
                        band.row_mut(rr).unwrap().fill((first + rr) as u8);
                    }
                }
            });
            s.spawn(move || {
                for k in 2..4 {
                    let mut band = jb.band_mut(k).unwrap();
                    let first = band.first_row();
                    for rr in 0..band.rows() {
                        band.row_mut(rr).unwrap().fill((first + rr) as u8);
                    }
                }
            });
        });
    }
    w.publish_through(0, 3).unwrap();
    let view = r.finished(0).unwrap();
    for y in 0..64u32 {
        assert_eq!(view.row(y).unwrap()[0], y as u8, "row {y}");
    }
}

#[test]
fn overlapping_band_ranges_are_refused() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, _r) = ProgressPicture::allocate(&spec(32, 64, 16, 4), 0, &mut budget).unwrap();
    assert!(w.split_bands_mut(0, &[0..3, 2..4]).is_err());
    assert!(w.split_bands_mut(0, &[2..4, 0..2]).is_err());
    // The plane has four bands, so the second range runs off the end.
    assert!(w.split_bands_mut(0, &[0..1, 3..9]).is_err());
}

#[test]
fn a_picture_larger_than_the_budget_is_refused() {
    let mut budget = Budget::new(Limits::tiny());
    let s = spec(4096, 4096, 256, 8);
    assert!(ProgressPicture::allocate(&s, 0, &mut budget).is_err());
}

/// The guard depth a codec picks is not a margin, it is an exact requirement,
/// and getting it one row short costs correctness nothing and speed a great
/// deal — every straddling read silently falls onto the copy path instead.
///
/// H.264's own worst case is the widest of any codec this guard is sized for:
/// clause 8.4.2.2.1's six-tap luma filter reads a **9-row** region for a 4x4
/// block. A read of `h` rows straddles a seam at `F` exactly when its first row
/// falls in `F - (h - 1) ..= F - 1`, so a guard of `h - 1 = 8` rows is what
/// makes every such read land inside the next band's own allocation — and eight
/// is what `DEFAULT_GUARD` is.
///
/// This walks a 9-row read across every row of a banded plane and asserts that
/// none of them is copied.
#[test]
fn a_nine_row_read_never_straddles_a_band_when_the_guard_is_eight() {
    let mut budget = Budget::new(Limits::permissive());
    let (width, height, band_h, guard) = (64u32, 128u32, 32u32, 8u32);
    let (mut w, r) = ProgressPicture::allocate(&spec(width, height, band_h, guard), 0, &mut budget)
        .unwrap();
    for k in 0..w.band_count(0) {
        fill(&mut w, k);
    }
    w.finish().unwrap();
    let view = r.finished(0).unwrap();
    let mut scratch = BlockScratch::new(&mut budget, 16, 16).unwrap();
    for y in 0..=(height - 9) {
        let block = view.block(0, y as i32, 9, 9, &mut scratch).unwrap();
        assert_eq!(
            block.stride, width as usize,
            "a 9-row read at row {y} was copied rather than borrowed, so the guard is too shallow"
        );
        for j in 0..9u32 {
            let row = &block.data[(j as usize) * block.stride..][..9];
            assert!(
                row.iter().all(|&v| v == (y + j) as u8),
                "row {} of the borrow does not hold picture row {}",
                j,
                y + j
            );
        }
    }
}

/// The same read one row short of the guard *does* get copied, which is what
/// makes the test above a statement about eight specifically rather than about
/// any guard at all.
#[test]
fn a_seven_row_guard_is_one_row_too_few_for_a_nine_row_read() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, r) = ProgressPicture::allocate(&spec(64, 128, 32, 7), 0, &mut budget).unwrap();
    for k in 0..w.band_count(0) {
        fill(&mut w, k);
    }
    w.finish().unwrap();
    let view = r.finished(0).unwrap();
    let mut scratch = BlockScratch::new(&mut budget, 16, 16).unwrap();
    // Rows 24..=32: eight rows above the seam at 32 and one below it.
    let block = view.block(0, 24, 9, 9, &mut scratch).unwrap();
    assert_eq!(block.stride, 9, "with a 7-row guard this read must take the copy path");
}

