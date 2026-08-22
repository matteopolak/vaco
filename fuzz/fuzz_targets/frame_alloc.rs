//! `vaco-frame` allocation and cropping on arbitrary geometry.
//!
//! `(format, width, height)` comes straight out of a sequence header in real
//! life, so every size below is attacker-controlled. Either a valid frame comes
//! back or an error does; a panic, an overflow or a plane shorter than its own
//! rows is a finding.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_frame::{Crop, Frame, PlaneMut};
use vaco_limits::{Budget, Limits};
use vaco_pixfmt::PixFmt;
use vaco_pool::ALIGN;

#[derive(Arbitrary, Debug)]
struct Input {
    format: u16,
    width: u32,
    height: u32,
    crop: (u32, u32, u32, u32),
    bands: u8,
    strict: bool,
}

fuzz_target!(|input: Input| {
    let all = PixFmt::all();
    let format = all[input.format as usize % all.len()];
    let limits = if input.strict {
        Limits::strict()
    } else {
        Limits::tiny()
    };
    let mut budget = Budget::new(limits);

    let Ok(mut frame) = Frame::alloc_video(&mut budget, format, input.width, input.height) else {
        return;
    };

    assert_eq!(frame.plane_count(), format.plane_count());
    for i in 0..frame.plane_count() {
        let plane = frame.plane(i).expect("plane in range");
        assert_eq!(plane.as_slice().as_ptr().addr() % ALIGN, 0);
        assert!(plane.row_bytes() <= plane.stride());
        // The buffer really does hold every row it claims.
        assert!(plane.as_slice().len() >= plane.rows().saturating_mul(plane.stride()));
        assert!(plane.row(plane.rows()).is_none());
        assert_eq!(plane.rows_iter().count(), plane.rows());
    }

    // Cropping is metadata: it must never move a byte, and an invalid rectangle
    // must be rejected rather than producing a nonsense visible size.
    let addrs: Vec<usize> = (0..frame.plane_count())
        .map(|i| frame.plane(i).expect("plane").as_slice().as_ptr().addr())
        .collect();
    let crop = Crop {
        top: input.crop.0,
        bottom: input.crop.1,
        left: input.crop.2,
        right: input.crop.3,
    };
    if frame.set_crop(crop).is_ok() {
        if let Some((vw, vh)) = frame.cropped_dimensions() {
            assert!(vw > 0 && vh > 0);
            assert!(vw <= input.width && vh <= input.height);
        }
    }
    for (i, addr) in addrs.iter().enumerate() {
        assert_eq!(
            frame.plane(i).expect("plane").as_slice().as_ptr().addr(),
            *addr,
            "cropping moved bytes"
        );
    }

    // Banding must partition every plane exactly, for any split count.
    for i in 0..frame.plane_count() {
        let plane = frame.plane_mut(i).expect("plane in range");
        let rows = plane.rows();
        let bands = plane.split_bands(input.bands as usize);
        assert!(!bands.is_empty());
        assert_eq!(bands.iter().map(PlaneMut::rows).sum::<usize>(), rows);
    }
});
