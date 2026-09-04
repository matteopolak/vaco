//! End-to-end CIE-scope oracle: a real RGB frame enters through the graph and
//! its BT.709-red chromaticity lands on the pixel measured from `ffmpeg`.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use vaco_color::ColorInfo;
use vaco_core::{MediaType, Rational, Result, Timestamp};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{Graph, GraphStatus, LinkFormat};
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_scope::ScopeRegistry;
use vaco_frame::FramePool;
use vaco_pixfmt::PixFmt;

#[test]
fn ciescope_maps_bt709_red_to_the_measured_xyy_pixel() -> Result<()> {
    let mut graph = Graph::new();
    let src = graph.add_source(
        "source",
        MediaType::Video,
        NodeFormats {
            outputs: vec![FormatSet::video_exact(PixFmt::Rgb24)],
            label: "source".into(),
            ..NodeFormats::default()
        },
    );

    let registry = ScopeRegistry;
    let req = Instantiate {
        name: "ciescope",
        instance: "cie",
        args: Some(
            "size=256:system=hdtv:cie=xyy:fill=0:gamuts=0:showwhite=0:intensity=0.001:corrgamma=0",
        ),
        arguments: &[],
    };
    let instance = registry
        .create(&req)
        .unwrap_or_else(|e| panic!("ciescope failed to instantiate: {e}"));
    let cie = graph.add(instance.desc, instance.formats, instance.filter);

    let sink = graph.add_sink(
        "sink",
        MediaType::Video,
        NodeFormats {
            inputs: vec![FormatSet::default()],
            label: "sink".into(),
            ..NodeFormats::default()
        },
    );
    graph.connect(src, 0, cie, 0)?;
    graph.connect(cie, 0, sink, 0)?;
    let time_base = Rational::new(1, 25);
    graph.set_source_format(
        src,
        LinkFormat::Video {
            format: PixFmt::Rgb24,
            width: 2,
            height: 2,
            time_base,
            frame_rate: time_base.inverse(),
            sample_aspect_ratio: Rational::ONE,
            color: ColorInfo::default(),
        },
    )?;
    graph.configure()?;

    let pool = FramePool::default();
    let mut frame = pool.acquire_video(PixFmt::Rgb24, 2, 2)?;
    {
        let mut rgb = frame.plane_mut(0).expect("rgb24 has one packed plane");
        for row in rgb.rows_mut() {
            for pixel in row.chunks_exact_mut(3) {
                pixel.copy_from_slice(&[255, 0, 0]);
            }
        }
    }
    frame.pts = Timestamp::new(0);
    frame.time_base = time_base;
    frame.set_duration_ticks(1);

    graph.send(src, frame)?;
    graph.close_source(src, Timestamp::new(1))?;
    graph.run()?;

    let out = graph.recv(sink)?;
    assert_eq!(out.pixel_format(), Some(PixFmt::Rgba64le));
    assert_eq!(out.dimensions(), Some((256, 256)));
    let plane = out.plane(0).expect("rgba64le has one packed plane");
    let rows: Vec<&[u8]> = plane.rows_iter().collect();
    let pixel = &rows[170][163 * 8..163 * 8 + 8];
    let channels = [
        u16::from_le_bytes([pixel[0], pixel[1]]),
        u16::from_le_bytes([pixel[2], pixel[3]]),
        u16::from_le_bytes([pixel[4], pixel[5]]),
        u16::from_le_bytes([pixel[6], pixel[7]]),
    ];

    // Black-box oracle, ffmpeg 9.0.1: BT.709 red maps to (163,170), and
    // four hits at intensity=.001 produce 4*floor(65535*.001)=260.
    assert_eq!(channels, [260, 260, 260, 65_535]);
    assert!(graph.violations().is_empty());
    assert_eq!(graph.run()?, GraphStatus::Eof);
    Ok(())
}
