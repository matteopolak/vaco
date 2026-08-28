//! Unit tests for frame allocation, plane views, cropping and side data.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a panic is the assertion mechanism"
)]

use vaco_chlayout::ChannelLayout;
use vaco_frame::{
    Crop, Frame, FrameData, FrameSideData, FrameSideDataKind, MotionVector, PlaneMut, SubtitleRect,
};
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
fn repeat_pict_defaults_to_zero_and_set_zero_removes_the_entry() {
    let mut b = budget();
    let mut frame = Frame::alloc_video(&mut b, PixFmt::Rgb24, 8, 8).unwrap();
    assert_eq!(frame.repeat_pict(), 0);
    assert!(frame.side_data(FrameSideDataKind::Pulldown).is_none());

    frame.set_repeat_pict(2);
    assert_eq!(frame.repeat_pict(), 2);
    assert!(matches!(
        frame.side_data(FrameSideDataKind::Pulldown),
        Some(FrameSideData::Pulldown(2))
    ));

    // 0 removes the entry rather than attaching a no-op `Pulldown(0)` --
    // `repeat_pict() == 0` and "no entry" stay indistinguishable, matching
    // the getter's own documented contract.
    frame.set_repeat_pict(0);
    assert_eq!(frame.repeat_pict(), 0);
    assert!(frame.side_data(FrameSideDataKind::Pulldown).is_none());
}

#[test]
fn metadata_is_empty_by_default_and_costs_no_side_data_entry() {
    let mut b = budget();
    let frame = Frame::alloc_video(&mut b, PixFmt::Rgb24, 8, 8).unwrap();
    assert_eq!(frame.metadata(), &[] as &[(String, String)]);
    assert_eq!(frame.metadata_get("lavfi.freezedetect.freeze_start"), None);
    // A frame that never calls `set_metadata` carries no side-data entry for
    // it at all — not an empty one. Gap 11: an empty collection at
    // construction must not be observable as "known to be nothing".
    assert!(frame.side_data(FrameSideDataKind::Metadata).is_none());
}

#[test]
fn metadata_preserves_insertion_order_and_overwrites_in_place() {
    let mut b = budget();
    let mut frame = Frame::alloc_video(&mut b, PixFmt::Rgb24, 8, 8).unwrap();

    frame.set_metadata("lavfi.signalstats.YMIN", "22");
    frame.set_metadata("lavfi.signalstats.YAVG", "59.6797");
    frame.set_metadata("lavfi.signalstats.YMAX", "210");
    // Overwriting an existing key updates its value without moving it to the
    // end or appending a duplicate — `av_dict_set`'s behaviour, measured.
    frame.set_metadata("lavfi.signalstats.YMIN", "23");

    assert_eq!(
        frame.metadata(),
        &[
            ("lavfi.signalstats.YMIN".to_string(), "23".to_string()),
            ("lavfi.signalstats.YAVG".to_string(), "59.6797".to_string()),
            ("lavfi.signalstats.YMAX".to_string(), "210".to_string()),
        ]
    );
    assert_eq!(frame.metadata_get("lavfi.signalstats.YAVG"), Some("59.6797"));
    assert_eq!(frame.metadata_get("lavfi.signalstats.nonexistent"), None);
}

#[test]
fn metadata_coexists_with_typed_side_data() {
    let mut b = budget();
    let mut frame = Frame::alloc_video(&mut b, PixFmt::Rgb24, 8, 8).unwrap();
    frame.set_side_data(FrameSideData::DisplayMatrix([1; 9]));
    frame.set_metadata("lavfi.freezedetect.freeze_start", "0");
    assert_eq!(frame.side_data.len(), 2);
    assert!(frame.side_data(FrameSideDataKind::DisplayMatrix).is_some());
    assert_eq!(frame.metadata_get("lavfi.freezedetect.freeze_start"), Some("0"));
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

#[test]
fn log_lines_are_empty_by_default_and_cost_no_side_data_entry() {
    let mut b = budget();
    let frame = Frame::alloc_video(&mut b, PixFmt::Rgb24, 8, 8).unwrap();
    assert_eq!(frame.log_lines(), &[] as &[String]);
    // Gap 11's own "empty collection at construction" caution applies here
    // too: a frame that never pushes a line carries no `Log` entry at all.
    assert!(frame.side_data(FrameSideDataKind::Log).is_none());
}

#[test]
fn log_lines_preserve_push_order() {
    let mut b = budget();
    let mut frame = Frame::alloc_video(&mut b, PixFmt::Rgb24, 8, 8).unwrap();
    frame.push_log_line("n:0 pts:0 pts_time:0");
    frame.push_log_line("color_range:unknown color_space:unknown");
    assert_eq!(
        frame.log_lines(),
        &["n:0 pts:0 pts_time:0".to_string(), "color_range:unknown color_space:unknown".to_string()]
    );
}

#[test]
fn log_lines_coexist_with_metadata_as_separate_channels() {
    let mut b = budget();
    let mut frame = Frame::alloc_video(&mut b, PixFmt::Rgb24, 8, 8).unwrap();
    frame.set_metadata("lavfi.signalstats.YMIN", "22");
    frame.push_log_line("n:0");
    assert_eq!(frame.side_data.len(), 2);
    assert_eq!(frame.metadata_get("lavfi.signalstats.YMIN"), Some("22"));
    assert_eq!(frame.log_lines(), &["n:0".to_string()]);
}

#[test]
fn motion_vectors_side_data_round_trips_through_set_and_remove() {
    let mut b = budget();
    let mut frame = Frame::alloc_video(&mut b, PixFmt::Rgb24, 8, 8).unwrap();
    assert!(frame.side_data(FrameSideDataKind::MotionVectors).is_none());

    let mv = MotionVector { source: -1, w: 16, h: 16, dst_x: 32, dst_y: 48, src_x: 30, src_y: 50 };
    frame.set_side_data(FrameSideData::MotionVectors(vec![mv]));
    let Some(FrameSideData::MotionVectors(mvs)) = frame.side_data(FrameSideDataKind::MotionVectors) else {
        unreachable!("just attached a MotionVectors entry");
    };
    assert_eq!(mvs.as_slice(), [mv]);
    assert!(frame.remove_side_data(FrameSideDataKind::MotionVectors).is_some());
    assert!(frame.side_data(FrameSideDataKind::MotionVectors).is_none());
}

#[test]
fn subtitle_frame_reports_no_video_or_audio_shape() {
    use smallvec::smallvec;
    let frame = Frame::from_data(FrameData::Subtitle {
        rects: smallvec![SubtitleRect::text(10, 10, 100, 20, false, "hi")],
    });
    assert!(frame.is_subtitle());
    assert!(!frame.is_video());
    assert!(!frame.is_audio());
    assert_eq!(frame.pixel_format(), None);
    assert_eq!(frame.dimensions(), None);
    assert_eq!(frame.plane_count(), 0);
    assert!(frame.plane(0).is_none());
}

#[test]
fn subtitle_frame_carries_more_than_one_rect() {
    use smallvec::smallvec;
    let mut frame = Frame::from_data(FrameData::Subtitle {
        rects: smallvec![
            SubtitleRect::text(0, 0, 0, 0, false, "line one"),
            SubtitleRect::text(0, 0, 0, 0, true, "forced line"),
        ],
    });
    let FrameData::Subtitle { rects } = &mut frame.data else {
        unreachable!("just constructed a Subtitle frame");
    };
    assert_eq!(rects.len(), 2);
    assert!(!rects[0].forced);
    assert!(rects[1].forced);
}
