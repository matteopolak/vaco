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

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

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

// --- Column bands: the 2-D tile grid a wavefront needs -----------------

fn tiled_spec(width: u32, height: u32, band_w: u32, band_h: u32) -> PictureSpec {
    PictureSpec::new(vec![PlaneSpec::new(width, height).with_bands(band_w, band_h)])
}

/// Every byte of tile `(row_band, col_band)` is filled with a value derived
/// from its own tile position, so any read can check it came from the right
/// tile.
fn fill_tile(writer: &mut vaco_codec_core::PictureWriter, row_band: usize, col_band: usize) -> u8 {
    let value = ((row_band * 16 + col_band) & 0xFF) as u8;
    let mut tile = writer.tile_mut(0, row_band, col_band).unwrap();
    for r in 0..tile.rows() {
        tile.row_mut(r).unwrap().fill(value);
    }
    value
}

#[test]
fn column_bands_publish_independently_of_each_other() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, r) = ProgressPicture::allocate(&tiled_spec(48, 32, 16, 16), 0, &mut budget).unwrap();
    assert_eq!(w.row_bands(0), 2);
    assert_eq!(w.col_bands(0), 3);

    assert_eq!(r.ready_cols(0, 0), 0);
    assert!(r.try_tile(0, 0, 0).is_none());

    let v00 = fill_tile(&mut w, 0, 0);
    w.publish_tile(0, 0, 0).unwrap();
    assert_eq!(r.ready_cols(0, 0), 1, "column 0 of row 0 is done");
    assert_eq!(r.ready_cols(0, 1), 0, "row 1 is untouched");
    let t00 = r.try_tile(0, 0, 0).unwrap();
    assert_eq!(t00.data[0], v00);
    assert!(
        r.try_tile(0, 0, 1).is_none(),
        "column 1 of row 0 has not published yet"
    );
    // Publishing one column must not move the row-level watermark: that
    // still means "every column of these rows is done", and column 1 isn't.
    assert_eq!(r.ready_rows(0), 0);

    let v01 = fill_tile(&mut w, 0, 1);
    w.publish_tile(0, 0, 1).unwrap();
    let v02 = fill_tile(&mut w, 0, 2);
    w.publish_tile(0, 0, 2).unwrap();
    assert_eq!(r.ready_cols(0, 0), 3, "every column of row 0 is done");
    assert_eq!(r.ready_rows(0), 16, "row 0 is now fully done across its whole width");
    assert_eq!(r.try_tile(0, 0, 1).unwrap().data[0], v01);
    assert_eq!(r.try_tile(0, 0, 2).unwrap().data[0], v02);

    w.finish().unwrap();
}

#[test]
fn publish_tile_out_of_order_in_a_row_is_refused() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, _r) = ProgressPicture::allocate(&tiled_spec(48, 16, 16, 16), 0, &mut budget).unwrap();
    fill_tile(&mut w, 0, 1);
    assert!(
        w.publish_tile(0, 0, 1).is_err(),
        "column 1 must not publish before column 0"
    );
}

#[test]
fn a_tile_may_not_be_written_after_publication() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, _r) = ProgressPicture::allocate(&tiled_spec(32, 16, 16, 16), 0, &mut budget).unwrap();
    fill_tile(&mut w, 0, 0);
    w.publish_tile(0, 0, 0).unwrap();
    assert!(w.tile_mut(0, 0, 0).is_err());
}

#[test]
fn publish_through_refuses_a_column_banded_plane_and_vice_versa() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut tiled_w, _r) = ProgressPicture::allocate(&tiled_spec(32, 16, 16, 16), 0, &mut budget).unwrap();
    assert!(tiled_w.publish_through(0, 0).is_err());

    let mut budget = Budget::new(Limits::permissive());
    let (mut row_w, _r) = ProgressPicture::allocate(&spec(32, 32, 16, 4), 0, &mut budget).unwrap();
    assert!(row_w.publish_tile(0, 0, 0).is_err());
}

#[test]
fn wait_tile_refuses_a_row_banded_plane() {
    let mut budget = Budget::new(Limits::permissive());
    let (_w, r) = ProgressPicture::allocate(&spec(32, 32, 16, 4), 0, &mut budget).unwrap();
    assert!(r.wait_tile(0, 0, 0).is_err());
}

#[test]
fn plane_view_block_refuses_a_column_banded_plane() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, r) = ProgressPicture::allocate(&tiled_spec(32, 16, 16, 16), 0, &mut budget).unwrap();
    fill_tile(&mut w, 0, 0);
    w.publish_tile(0, 0, 0).unwrap();
    fill_tile(&mut w, 0, 1);
    w.publish_tile(0, 0, 1).unwrap();
    let view = r.finished(0).unwrap();
    let mut scratch = BlockScratch::new(&mut budget, 16, 16).unwrap();
    assert!(
        view.block(0, 0, 8, 8, &mut scratch).is_err(),
        "a row split across independent tile allocations has no single contiguous slice to hand back"
    );
}

#[test]
fn tile_of_maps_pixel_positions_to_the_tile_that_owns_them() {
    let mut budget = Budget::new(Limits::permissive());
    let (w, r) = ProgressPicture::allocate(&tiled_spec(48, 32, 16, 16), 0, &mut budget).unwrap();
    assert_eq!(w.tile_of(0, 0, 0), Some((0, 0)));
    assert_eq!(w.tile_of(0, 15, 15), Some((0, 0)));
    assert_eq!(w.tile_of(0, 16, 15), Some((0, 1)));
    assert_eq!(w.tile_of(0, 47, 31), Some((1, 2)));
    assert_eq!(r.tile_of(0, 32, 16), Some((1, 2)));
}

#[test]
fn chroma_sized_tiles_use_their_own_geometry_independent_of_luma() {
    let mut budget = Budget::new(Limits::permissive());
    let spec = PictureSpec::new(vec![
        PlaneSpec::new(64, 64).with_bands(32, 32),
        PlaneSpec::new(32, 32).with_bands(16, 16),
    ]);
    let (mut w, r) = ProgressPicture::allocate(&spec, 0, &mut budget).unwrap();
    assert_eq!((w.row_bands(0), w.col_bands(0)), (2, 2), "luma: 64/32 each way");
    assert_eq!((w.row_bands(1), w.col_bands(1)), (2, 2), "chroma: 32/16 each way");

    for rb in 0..2 {
        for cb in 0..2 {
            let mut t = w.tile_mut(0, rb, cb).unwrap();
            for row in 0..t.rows() {
                t.row_mut(row).unwrap().fill(1);
            }
            w.publish_tile(0, rb, cb).unwrap();

            let mut t = w.tile_mut(1, rb, cb).unwrap();
            for row in 0..t.rows() {
                t.row_mut(row).unwrap().fill(2);
            }
            w.publish_tile(1, rb, cb).unwrap();
        }
    }
    let luma = r.wait_tile(0, 1, 1).unwrap();
    assert_eq!(luma.stride, 32, "luma's own tile width");
    let chroma = r.wait_tile(1, 1, 1).unwrap();
    assert_eq!(chroma.stride, 16, "chroma's own, independently-sized tile width");
}

#[test]
fn a_reader_blocks_until_the_specific_tile_it_asked_for_publishes() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, r) = ProgressPicture::allocate(&tiled_spec(48, 16, 16, 16), 0, &mut budget).unwrap();
    let reached = Arc::new(AtomicBool::new(false));
    let reader = {
        let r = r.clone();
        let reached = Arc::clone(&reached);
        std::thread::spawn(move || {
            let block = r.wait_tile(0, 0, 2).unwrap();
            reached.store(true, Ordering::Release);
            block.data[0]
        })
    };
    fill_tile(&mut w, 0, 0);
    w.publish_tile(0, 0, 0).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(
        !reached.load(Ordering::Acquire),
        "column 2 has not published yet; the waiter must still be blocked"
    );
    fill_tile(&mut w, 0, 1);
    w.publish_tile(0, 0, 1).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(
        !reached.load(Ordering::Acquire),
        "column 1 publishing must not wake a waiter for column 2"
    );
    let v2 = fill_tile(&mut w, 0, 2);
    w.publish_tile(0, 0, 2).unwrap();
    assert_eq!(reader.join().unwrap(), v2);
    assert!(reached.load(Ordering::Acquire));
}

#[test]
fn a_dropped_writer_wakes_tile_waiters_with_an_error() {
    let mut budget = Budget::new(Limits::permissive());
    let (w, r) = ProgressPicture::allocate(&tiled_spec(48, 16, 16, 16), 0, &mut budget).unwrap();
    let reader = {
        let r = r.clone();
        std::thread::spawn(move || r.wait_tile(0, 0, 2).map(|_| ()))
    };
    drop(w);
    assert!(reader.join().unwrap().is_err());
}

/// The point of column bands, proven directly: row 1's worker publishes its
/// own first tile *before* row 0 — the tile it actually depends on — has
/// finished its whole width, the way a full-width row band would force. A
/// row-banded plane cannot even express this schedule (row 1 would have
/// nothing to wait on shorter than "all of row 0"); this test is the
/// difference `PlaneSpec::with_bands` exists to make possible.
#[test]
fn a_later_row_starts_before_an_earlier_row_finishes_its_whole_width() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, r) = ProgressPicture::allocate(&tiled_spec(64, 32, 16, 16), 0, &mut budget).unwrap();
    assert_eq!(w.col_bands(0), 4, "four CTU-shaped columns per row");

    // Publish row 0's tiles one at a time from this thread, and hand row 1's
    // worker a `PictureRef` to read from as it goes -- exactly the
    // `wait_tile_for` shape a real wavefront worker uses on its neighbour.
    let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let row0_cols_published = Arc::new(AtomicU32::new(0));

    let row1 = {
        let r = r.clone();
        let order = Arc::clone(&order);
        let row0_cols_published = Arc::clone(&row0_cols_published);
        std::thread::spawn(move || {
            // Row 1's own tile 0 only needs row 0's tile 0 (directly above);
            // its tile 1 needs row 0's tile 1 (above-right of tile 0) too --
            // the two-tile lag this module's doc describes.
            r.wait_tile_for(1, 0, 0, 0).unwrap();
            order.lock().unwrap().push("row1 saw row0 col0");
            assert!(
                row0_cols_published.load(Ordering::Acquire) < 4,
                "row 1 must be able to proceed while row 0 still has columns left"
            );

            let mut w1_tile = Vec::new();
            for c in 0..4usize {
                r.wait_tile_for(1, 0, 0, c).unwrap();
                w1_tile.push(c);
            }
            order.lock().unwrap().push("row1 finished reading row0");
            w1_tile
        })
    };

    for c in 0..4usize {
        fill_tile(&mut w, 0, c);
        w.publish_tile(0, 0, c).unwrap();
        row0_cols_published.fetch_add(1, Ordering::Release);
        if c == 0 {
            // Give row 1's worker a chance to observe column 0 and proceed
            // before row 0 publishes anything else -- if it were blocked on
            // the whole row, it could not have logged its first message yet.
            std::thread::sleep(std::time::Duration::from_millis(20));
            assert!(
                order.lock().unwrap().contains(&"row1 saw row0 col0"),
                "row 1 should already be past its own first wait by now"
            );
        }
    }

    assert_eq!(row1.join().unwrap(), vec![0, 1, 2, 3]);
    assert_eq!(
        order.lock().unwrap().as_slice(),
        &["row1 saw row0 col0", "row1 finished reading row0"]
    );
}

// --- `band_ref`/`tile_ref`: reading a still-staged band without `&mut` ---

#[test]
fn band_ref_reads_back_a_still_staged_row_band() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, _r) = ProgressPicture::allocate(&spec(32, 48, 16, 4), 0, &mut budget).unwrap();
    // The staged buffer exists (zero-filled) from allocation on -- `band_ref`
    // is a plain read of whatever is there, not a "has anyone written here"
    // signal (that is what `PictureRef::ready_rows`/`try_rows` are for, and
    // they only ever answer for *published* bands).
    assert_eq!(w.band_ref(0, 0).unwrap().data[0], 0, "unwritten, but present and zero");
    fill(&mut w, 0);
    let seen = w.band_ref(0, 0).expect("band 0 is staged, not yet published");
    assert_eq!(seen.data[0], 0, "row 0's own fill value");
    assert_eq!(seen.data[seen.stride], 1, "row 1's own fill value");
    // Reading it back does not consume or otherwise disturb it: the normal
    // exclusive path still works afterward.
    w.publish_through(0, 0).unwrap();
    assert!(
        w.band_ref(0, 0).is_none(),
        "band_ref only ever sees the staged copy, not the published one"
    );
}

#[test]
fn tile_ref_reads_back_a_still_staged_tile() {
    let mut budget = Budget::new(Limits::permissive());
    let (mut w, _r) = ProgressPicture::allocate(&tiled_spec(48, 16, 16, 16), 0, &mut budget).unwrap();
    assert_eq!(w.tile_ref(0, 0, 1).unwrap().data[0], 0, "unwritten, but present and zero");
    let v = fill_tile(&mut w, 0, 1);
    let seen = w.tile_ref(0, 0, 1).expect("tile (0, 1) is staged, not yet published");
    assert_eq!(seen.data[0], v);
    w.publish_tile(0, 0, 0).unwrap();
    w.publish_tile(0, 0, 1).unwrap();
    assert!(w.tile_ref(0, 0, 1).is_none(), "published now, not staged");
}

