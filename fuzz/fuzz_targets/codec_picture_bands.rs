//! Banded picture publication and block reads.
//!
//! Geometry comes from a bitstream in real use, so every combination of
//! dimensions, band height and guard depth has to either work or fail cleanly.
//! Reads are checked against the value written into each row, so a mis-indexed
//! band shows up as wrong data rather than as a crash that never happens in
//! safe Rust.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::picture::{BlockScratch, PictureSpec, PlaneSpec, ProgressPicture};
use vaco_limits::{Budget, Limits};

#[derive(Debug, arbitrary::Arbitrary)]
struct Input {
    width: u8,
    height: u8,
    band_h: u8,
    guard: u8,
    reads: Vec<(i8, i8, u8, u8)>,
}

fuzz_target!(|input: Input| {
    if input.reads.len() > 128 {
        return;
    }
    let width = u32::from(input.width) + 1;
    let height = u32::from(input.height) + 1;
    let spec = PictureSpec::new(vec![PlaneSpec::new(width, height)])
        .with_band_height(u32::from(input.band_h))
        .with_guard(u32::from(input.guard));

    let mut budget = Budget::new(Limits::permissive());
    let Ok((mut writer, reader)) = ProgressPicture::allocate(&spec, 1, &mut budget) else {
        return;
    };

    let bands = writer.band_count(0);
    for k in 0..bands {
        let Ok(mut band) = writer.band_mut(0, k) else {
            return;
        };
        let first = band.first_row();
        for r in 0..band.rows() {
            let value = ((first + r) % 251) as u8;
            if let Some(row) = band.row_mut(r) {
                row.fill(value);
            }
        }
        if writer.publish_through(0, k).is_err() {
            return;
        }
    }

    let Ok(view) = reader.finished(0) else {
        return;
    };
    assert_eq!(view.rows(), height);
    for y in 0..height {
        let row = view.row(y).expect("published row must be readable");
        assert_eq!(row[0], (y % 251) as u8, "row {y} read back wrong");
    }

    let Ok(mut scratch) = BlockScratch::new(&mut budget, 256, 256) else {
        return;
    };
    for &(x, y, w, h) in &input.reads {
        let w = u32::from(w) % 64;
        let h = u32::from(h) % 64;
        let Ok(block) = view.block(i32::from(x), i32::from(y), w, h, &mut scratch) else {
            continue;
        };
        if w == 0 || h == 0 {
            continue;
        }
        // Whichever path served it, row j of the block is picture row
        // clamp(y + j), whose every byte is that row's value.
        for j in 0..h {
            let gy = (i32::from(y) + j as i32).clamp(0, height as i32 - 1) as u32;
            let want = (gy % 251) as u8;
            let at = (j as usize) * block.stride;
            assert_eq!(block.data[at], want, "block row {j} came from the wrong place");
        }
    }
    let _ = writer.finish();
});
