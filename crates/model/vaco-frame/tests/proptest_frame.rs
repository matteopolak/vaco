//! Property tests over arbitrary geometry: allocation, plane isolation and
//! cropping arithmetic.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a panic is the assertion mechanism"
)]

use proptest::prelude::*;
use vaco_frame::{Crop, Frame, FrameData, PlaneMut};
use vaco_limits::{Budget, Limits};
use vaco_pixfmt::PixFmt;
use vaco_pool::ALIGN;

/// A spread of formats covering every subsampling and both packed and planar.
const FORMATS: [PixFmt; 8] = [
    PixFmt::Yuv420p,
    PixFmt::Yuv422p,
    PixFmt::Yuv444p,
    PixFmt::Yuv410p,
    PixFmt::Nv12,
    PixFmt::Rgb24,
    PixFmt::Rgba,
    PixFmt::Gray8,
];

fn format() -> impl Strategy<Value = PixFmt> {
    (0usize..FORMATS.len()).prop_map(|i| FORMATS[i])
}

proptest! {
    /// Whatever the geometry, every plane is 64-byte aligned, long enough for
    /// its rows, and strided to a multiple of ALIGN.
    #[test]
    fn allocation_is_well_formed(fmt in format(), w in 1u32..600, h in 1u32..600) {
        let mut b = Budget::new(Limits::permissive());
        let frame = Frame::alloc_video(&mut b, fmt, w, h).unwrap();
        prop_assert_eq!(frame.plane_count(), fmt.plane_count());
        for i in 0..frame.plane_count() {
            let plane = frame.plane(i).unwrap();
            prop_assert_eq!(plane.as_slice().as_ptr().addr() % ALIGN, 0);
            prop_assert_eq!(plane.stride() % ALIGN, 0);
            prop_assert!(plane.row_bytes() <= plane.stride());
            prop_assert_eq!(plane.rows(), fmt.plane_height(h, i as u8) as usize);
            prop_assert!(plane.as_slice().len() >= plane.rows() * plane.stride());
            prop_assert_eq!(plane.rows_iter().count(), plane.rows());
        }
    }

    /// Copy-on-write never aliases across planes: mutate one plane of one
    /// clone, and every plane of the other clone is byte-identical to what it
    /// started as.
    #[test]
    fn plane_cow_never_aliases(fmt in format(), w in 1u32..200, h in 1u32..200, fill in any::<u8>()) {
        let mut b = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_video(&mut b, fmt, w, h).unwrap();
        // Seed with a recognisable pattern before sharing.
        for i in 0..frame.plane_count() {
            frame.plane_mut(i).unwrap().fill(0x5A);
        }
        let original = frame.clone();

        let target = (w as usize) % frame.plane_count();
        frame.plane_mut(target).unwrap().fill(fill);

        for i in 0..original.plane_count() {
            let view = original.plane(i).unwrap();
            for y in 0..view.rows() {
                prop_assert!(view.row(y).unwrap().iter().all(|&x| x == 0x5A),
                    "plane {} row {} was disturbed", i, y);
            }
        }

        // Untouched planes are still shared; the written one is not.
        let FrameData::Video { planes: a, .. } = &frame.data else { unreachable!() };
        let FrameData::Video { planes: c, .. } = &original.data else { unreachable!() };
        for i in 0..a.len() {
            prop_assert_eq!(a[i].data.ptr_eq(&c[i].data), i != target);
        }
    }

    /// Bands partition a plane exactly: no row is covered twice and none is
    /// missed, for any split count.
    #[test]
    fn bands_partition_exactly(fmt in format(), w in 1u32..200, h in 1u32..200, n in 0usize..17) {
        let mut b = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_video(&mut b, fmt, w, h).unwrap();
        for i in 0..frame.plane_count() {
            let plane = frame.plane_mut(i).unwrap();
            let rows = plane.rows();
            let bands = plane.split_bands(n);
            prop_assert!(!bands.is_empty());
            prop_assert_eq!(bands.iter().map(PlaneMut::rows).sum::<usize>(), rows);
        }
    }

    /// A crop the validator accepts always leaves a non-empty visible picture
    /// on a chroma sample boundary, and never moves a byte.
    #[test]
    fn accepted_crops_are_sane(
        fmt in format(),
        w in 2u32..200, h in 2u32..200,
        crop in (0u32..64, 0u32..64, 0u32..64, 0u32..64),
    ) {
        let mut b = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_video(&mut b, fmt, w, h).unwrap();
        let before: Vec<usize> = (0..frame.plane_count())
            .map(|i| frame.plane(i).unwrap().as_slice().as_ptr().addr())
            .collect();

        let c = Crop { top: crop.0, bottom: crop.1, left: crop.2, right: crop.3 };
        if frame.set_crop(c).is_ok() {
            let (vw, vh) = frame.cropped_dimensions().unwrap_or((w, h));
            prop_assert!(vw > 0 && vh > 0);
            prop_assert!(vw <= w && vh <= h);
            let (log2_w, log2_h) = fmt.log2_chroma();
            prop_assert_eq!(vw % (1 << log2_w), w % (1 << log2_w));
            prop_assert_eq!(vh % (1 << log2_h), h % (1 << log2_h));
        }
        // Accepted or rejected, the pixels did not move.
        for (i, addr) in before.iter().enumerate() {
            prop_assert_eq!(frame.plane(i).unwrap().as_slice().as_ptr().addr(), *addr);
        }
        prop_assert_eq!(frame.dimensions(), Some((w, h)));
    }

    /// Row access is total: in range it yields exactly `row_bytes`, out of range
    /// it yields `None`. It never panics and never returns a neighbour's row.
    #[test]
    fn row_access_is_total(fmt in format(), w in 1u32..200, h in 1u32..200, y in 0usize..500) {
        let mut b = Budget::new(Limits::permissive());
        let frame = Frame::alloc_video(&mut b, fmt, w, h).unwrap();
        for i in 0..frame.plane_count() {
            let plane = frame.plane(i).unwrap();
            match plane.row(y) {
                Some(row) => {
                    prop_assert!(y < plane.rows());
                    prop_assert_eq!(row.len(), plane.row_bytes());
                }
                None => prop_assert!(y >= plane.rows()),
            }
        }
    }
}
