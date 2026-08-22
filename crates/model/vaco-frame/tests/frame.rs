//! Unit tests for frame allocation, plane views, cropping and side data.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a panic is the assertion mechanism"
)]

use vaco_chlayout::ChannelLayout;
use vaco_frame::{Crop, Frame, FrameData, FrameSideData, FrameSideDataKind, PlaneMut};
use vaco_limits::{Budget, Limits};
use vaco_pixfmt::PixFmt;
use vaco_pool::{ALIGN, PoolConfig};
use vaco_sampfmt::SampleFmt;

fn budget() -> Budget {
    Budget::new(Limits::permissive())
}

#[test]
fn video_allocation_matches_the_format_layout() {
    let mut b = budget();
    for format in [
        PixFmt::Yuv420p,
        PixFmt::Yuv422p,
        PixFmt::Yuv444p,
        PixFmt::Nv12,
        PixFmt::Rgb24,
        PixFmt::Gray8,
    ] {
        for (w, h) in [(2u32, 2u32), (17, 13), (64, 64), (1920, 1080)] {
            let frame = Frame::alloc_video(&mut b, format, w, h).unwrap();
            let layout = format.plane_layout(w, h, ALIGN).unwrap();
            assert_eq!(frame.plane_count(), layout.planes, "{format:?} {w}x{h}");
            for i in 0..layout.planes {
                let plane = frame.plane(i).unwrap();
                assert_eq!(plane.stride(), layout.strides[i]);
                assert_eq!(plane.as_slice().len(), layout.sizes[i]);
                assert_eq!(plane.as_slice().as_ptr().addr() % ALIGN, 0);
                assert_eq!(plane.rows(), format.plane_height(h, i as u8) as usize);
                assert!(plane.row_bytes() <= plane.stride());
                assert!(plane.row(plane.rows()).is_none());
            }
        }
    }
}

#[test]
fn every_row_of_an_allocated_plane_is_aligned() {
    let mut b = budget();
    let frame = Frame::alloc_video(&mut b, PixFmt::Yuv420p, 1919, 1079).unwrap();
    for i in 0..frame.plane_count() {
        let plane = frame.plane(i).unwrap();
        assert_eq!(plane.stride() % ALIGN, 0, "plane {i} stride not aligned");
        for y in 0..plane.rows() {
            assert_eq!(plane.row(y).unwrap().as_ptr().addr() % ALIGN, 0);
        }
    }
}

#[test]
fn hardware_formats_cannot_be_allocated() {
    let mut b = budget();
    let hw = PixFmt::all().iter().copied().find(|f| f.is_hw());
    if let Some(hw) = hw {
        assert!(Frame::alloc_video(&mut b, hw, 16, 16).is_err());
    }
}

#[test]
fn absurd_dimensions_are_rejected_not_allocated() {
    let mut b = Budget::new(Limits::strict());
    assert!(Frame::alloc_video(&mut b, PixFmt::Yuv420p, u32::MAX, u32::MAX).is_err());
    assert!(Frame::alloc_video(&mut b, PixFmt::Yuv420p, 1 << 20, 1 << 20).is_err());
}

#[test]
fn cloning_a_frame_shares_every_plane() {
    let mut b = budget();
    let frame = Frame::alloc_video(&mut b, PixFmt::Yuv420p, 32, 32).unwrap();
    let copy = frame.clone();
    assert!(!frame.is_writable());
    for i in 0..frame.plane_count() {
        let FrameData::Video { planes: a, .. } = &frame.data else {
            unreachable!()
        };
        let FrameData::Video { planes: c, .. } = &copy.data else {
            unreachable!()
        };
        assert!(a[i].data.ptr_eq(&c[i].data));
    }
}

#[test]
fn writing_one_plane_leaves_the_others_shared() {
    let mut b = budget();
    let mut frame = Frame::alloc_video(&mut b, PixFmt::Yuv420p, 32, 32).unwrap();
    let copy = frame.clone();

    frame.plane_mut(1).unwrap().fill(0x80);

    let FrameData::Video { planes: a, .. } = &frame.data else {
        unreachable!()
    };
    let FrameData::Video { planes: c, .. } = &copy.data else {
        unreachable!()
    };
    assert!(a[0].data.ptr_eq(&c[0].data), "luma was copied needlessly");
    assert!(!a[1].data.ptr_eq(&c[1].data), "chroma was not copied");
    assert!(a[2].data.ptr_eq(&c[2].data), "second chroma was copied");

    // The original's chroma is untouched.
    assert!(copy.plane(1).unwrap().as_slice().iter().all(|&x| x == 0));
    assert!(
        frame
            .plane(1)
            .unwrap()
            .row(0)
            .unwrap()
            .iter()
            .all(|&x| x == 0x80)
    );
}

#[test]
fn make_writable_uncouples_every_plane() {
    let mut b = budget();
    let mut frame = Frame::alloc_video(&mut b, PixFmt::Yuv420p, 16, 16).unwrap();
    let copy = frame.clone();
    assert!(!frame.is_writable());
    frame.make_writable();
    assert!(frame.is_writable());
    assert!(copy.is_writable());
}

#[test]
fn disjoint_plane_access_across_threads() {
    let mut b = budget();
    let mut frame = Frame::alloc_video(&mut b, PixFmt::Yuv420p, 128, 128).unwrap();

    let mut planes = frame.planes_mut();
    let (luma, chroma) = planes.split_at_mut(1);
    let (cb, cr) = chroma.split_at_mut(1);

    std::thread::scope(|s| {
        s.spawn(|| luma[0].fill(0x10));
        s.spawn(|| cb[0].fill(0x80));
        s.spawn(|| cr[0].fill(0x81));
    });
    drop(planes);

    assert!(
        frame
            .plane(0)
            .unwrap()
            .row(0)
            .unwrap()
            .iter()
            .all(|&x| x == 0x10)
    );
    assert!(
        frame
            .plane(1)
            .unwrap()
            .row(0)
            .unwrap()
            .iter()
            .all(|&x| x == 0x80)
    );
    assert!(
        frame
            .plane(2)
            .unwrap()
            .row(0)
            .unwrap()
            .iter()
            .all(|&x| x == 0x81)
    );
}

#[test]
fn bands_partition_a_plane() {
    let mut b = budget();
    let mut frame = Frame::alloc_video(&mut b, PixFmt::Gray8, 100, 50).unwrap();
    let plane = frame.plane_mut(0).unwrap();
    let rows = plane.rows();
    let bands = plane.split_bands(4);
    assert_eq!(bands.iter().map(PlaneMut::rows).sum::<usize>(), rows);

    let mut bands = bands;
    std::thread::scope(|s| {
        for (i, band) in bands.iter_mut().enumerate() {
            s.spawn(move || band.fill(i as u8));
        }
    });
    drop(bands);

    let view = frame.plane(0).unwrap();
    assert_eq!(view.row(0).unwrap()[0], 0);
    assert_eq!(view.row(49).unwrap()[0], 3);
}

#[test]
fn split_bands_survives_degenerate_counts() {
    let mut b = budget();
    let mut frame = Frame::alloc_video(&mut b, PixFmt::Gray8, 8, 4).unwrap();
    for n in [0usize, 1, 3, 4, 5, 100] {
        let plane = frame.plane_mut(0).unwrap();
        let rows = plane.rows();
        let bands = plane.split_bands(n);
        assert!(!bands.is_empty());
        assert_eq!(
            bands.iter().map(PlaneMut::rows).sum::<usize>(),
            rows,
            "n={n}"
        );
    }
}

#[test]
fn crop_is_metadata_and_costs_nothing() {
    let mut b = budget();
    let mut frame = Frame::alloc_video(&mut b, PixFmt::Yuv420p, 1920, 1088).unwrap();
    assert_eq!(frame.cropped_dimensions(), None);

    let before = frame.plane(0).unwrap().as_slice().as_ptr().addr();
    frame
        .set_crop(Crop {
            bottom: 8,
            ..Crop::NONE
        })
        .unwrap();
    assert_eq!(frame.cropped_dimensions(), Some((1920, 1080)));
    assert_eq!(
        frame.plane(0).unwrap().as_slice().as_ptr().addr(),
        before,
        "cropping moved bytes"
    );
    assert_eq!(frame.dimensions(), Some((1920, 1088)));
}

#[test]
fn odd_crops_are_rejected_for_subsampled_formats() {
    let mut b = budget();
    let mut yuv420 = Frame::alloc_video(&mut b, PixFmt::Yuv420p, 64, 64).unwrap();
    assert!(
        yuv420
            .set_crop(Crop {
                left: 1,
                ..Crop::NONE
            })
            .is_err()
    );
    assert!(
        yuv420
            .set_crop(Crop {
                top: 1,
                ..Crop::NONE
            })
            .is_err()
    );
    assert!(
        yuv420
            .set_crop(Crop {
                left: 2,
                ..Crop::NONE
            })
            .is_ok()
    );

    let mut yuv444 = Frame::alloc_video(&mut b, PixFmt::Yuv444p, 64, 64).unwrap();
    assert!(
        yuv444
            .set_crop(Crop {
                left: 1,
                ..Crop::NONE
            })
            .is_ok()
    );
}

#[test]
fn crops_that_leave_nothing_visible_are_rejected() {
    let mut b = budget();
    let mut frame = Frame::alloc_video(&mut b, PixFmt::Yuv444p, 64, 64).unwrap();
    assert!(
        frame
            .set_crop(Crop {
                left: 64,
                ..Crop::NONE
            })
            .is_err()
    );
    assert!(
        frame
            .set_crop(Crop {
                left: 32,
                right: 32,
                ..Crop::NONE
            })
            .is_err()
    );
}

#[test]
fn crop_on_audio_is_an_error() {
    let mut b = budget();
    let mut frame =
        Frame::alloc_audio(&mut b, SampleFmt::F32P, ChannelLayout::STEREO, 1024, 48_000).unwrap();
    assert!(
        frame
            .set_crop(Crop {
                top: 2,
                ..Crop::NONE
            })
            .is_err()
    );
    assert_eq!(frame.cropped_dimensions(), None);
}

#[test]
fn side_data_set_get_replace_remove() {
    let mut b = budget();
    let mut frame = Frame::alloc_video(&mut b, PixFmt::Rgb24, 8, 8).unwrap();
    assert!(frame.side_data(FrameSideDataKind::DisplayMatrix).is_none());

    frame.set_side_data(FrameSideData::DisplayMatrix([1; 9]));
    frame.set_side_data(FrameSideData::ContentLightLevel {
        max_cll: 1000,
        max_fall: 400,
    });
    assert_eq!(frame.side_data.len(), 2);

    // Replacing does not append.
    frame.set_side_data(FrameSideData::DisplayMatrix([2; 9]));
    assert_eq!(frame.side_data.len(), 2);
    assert!(matches!(
        frame.side_data(FrameSideDataKind::DisplayMatrix),
        Some(FrameSideData::DisplayMatrix(m)) if m[0] == 2
    ));

    assert!(
        frame
            .remove_side_data(FrameSideDataKind::DisplayMatrix)
            .is_some()
    );
    assert!(
        frame
            .remove_side_data(FrameSideDataKind::DisplayMatrix)
            .is_none()
    );
    assert_eq!(frame.side_data.len(), 1);
}

#[test]
fn planar_audio_gets_one_buffer_per_channel() {
    let mut b = budget();
    let frame =
        Frame::alloc_audio(&mut b, SampleFmt::F32P, ChannelLayout::STEREO, 1024, 48_000).unwrap();
    assert!(frame.is_audio());
    assert_eq!(frame.plane_count(), 2);
    for i in 0..2 {
        assert_eq!(frame.plane(i).unwrap().as_slice().len(), 1024 * 4);
        assert_eq!(
            frame.plane(i).unwrap().as_slice().as_ptr().addr() % ALIGN,
            0
        );
    }
}

#[test]
fn interleaved_audio_gets_exactly_one_buffer() {
    let mut b = budget();
    let frame =
        Frame::alloc_audio(&mut b, SampleFmt::S16, ChannelLayout::STEREO, 1024, 48_000).unwrap();
    assert_eq!(frame.plane_count(), 1);
    assert_eq!(frame.plane(0).unwrap().as_slice().len(), 1024 * 2 * 2);
}

#[test]
fn video_from_planes_validates_geometry() {
    let mut b = budget();
    let good = Frame::alloc_video(&mut b, PixFmt::Yuv420p, 32, 32).unwrap();
    let FrameData::Video { planes, .. } = good.data.clone() else {
        unreachable!()
    };
    assert!(Frame::video_from_planes(PixFmt::Yuv420p, 32, 32, planes.clone()).is_ok());
    // Same planes, claimed to be twice as tall: rejected.
    assert!(Frame::video_from_planes(PixFmt::Yuv420p, 32, 64, planes.clone()).is_err());
    // Wrong plane count.
    assert!(Frame::video_from_planes(PixFmt::Rgb24, 32, 32, planes).is_err());
}

#[test]
fn frame_pool_reaches_steady_state() {
    let pool = vaco_frame::FramePool::new(PoolConfig::default());
    let warm = pool.acquire_video(PixFmt::Yuv420p, 320, 240).unwrap();
    drop(warm);
    let after_warmup = pool.stats().allocations;
    assert_eq!(after_warmup, 3, "one allocation per plane");

    for _ in 0..50 {
        let f = pool.acquire_video(PixFmt::Yuv420p, 320, 240).unwrap();
        drop(f);
    }
    assert_eq!(
        pool.stats().allocations,
        after_warmup,
        "steady state allocated"
    );
    assert_eq!(pool.stats().hits, 150);
}

#[test]
fn frame_pool_discards_the_old_geometry() {
    let pool = vaco_frame::FramePool::new(PoolConfig::default());
    drop(pool.acquire_video(PixFmt::Yuv420p, 320, 240).unwrap());
    assert_eq!(pool.stats().retained_buffers, 3);

    // A resolution switch throws the cached class away rather than growing.
    drop(pool.acquire_video(PixFmt::Yuv420p, 640, 480).unwrap());
    let s = pool.stats();
    assert_eq!(s.retained_buffers, 3, "old geometry retained too");
    assert_eq!(s.allocations, 3);

    pool.clear();
    assert_eq!(pool.stats().retained_buffers, 0);
}

#[test]
fn pooled_audio_frames_recycle() {
    let pool = vaco_frame::FramePool::new(PoolConfig::default());
    drop(
        pool.acquire_audio(SampleFmt::F32P, ChannelLayout::STEREO, 1024, 48_000)
            .unwrap(),
    );
    let allocs = pool.stats().allocations;
    for _ in 0..20 {
        drop(
            pool.acquire_audio(SampleFmt::F32P, ChannelLayout::STEREO, 1024, 48_000)
                .unwrap(),
        );
    }
    assert_eq!(pool.stats().allocations, allocs);
}

#[test]
fn pooled_frames_outlive_their_pool() {
    let frame = {
        let pool = vaco_frame::FramePool::new(PoolConfig::default());
        pool.acquire_video(PixFmt::Yuv420p, 64, 64).unwrap()
    };
    assert_eq!(frame.plane_count(), 3);
    assert!(frame.plane(0).unwrap().row(0).is_some());
}
