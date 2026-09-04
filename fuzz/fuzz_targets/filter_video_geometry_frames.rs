//! Fuzz real `vaco-filter-video-geometry` filters with real frame data.
//!
//! The companion `filter_video_geometry_options` target stops at option
//! parsing. This target chooses one of the six registered geometry filters,
//! renders a valid graph description for it, then drives a bounded Gray8 frame
//! through the public `parse_and_build` -> attach -> configure -> send/receive
//! path. That is deliberately the path `-vf` users reach: registry creation,
//! graph wiring, format negotiation, scheduler activation, and the filter's
//! byte-moving implementation all participate.
//!
//! The controls only choose valid dimensions/options, so a rejected graph is a
//! finding rather than routine fuzz noise. Frame bytes remain entirely fuzzed.
//! Each run asserts more than no-panic:
//!
//! * the bounded driver reaches EOF and observes no graph contract violation;
//! * every one-input geometry filter emits exactly one Gray8 frame, whose
//!   negotiated shape and timestamp match its documented operation;
//! * every input/output plane exposes only bounded, addressable rows; and
//! * a fresh graph produces byte-identical output and row geometry for the same
//!   case.
//!
//! fuzz-crate: vaco-filter-video-geometry
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::{Error, Rational, Timestamp};
use vaco_filter_core::negotiate::{AutoConvert, FormatSet, NodeFormats};
use vaco_filter_core::{GraphStatus, LinkFormat, Progress};
use vaco_filter_graph::parse_and_build;
use vaco_filter_video_geometry::GeometryRegistry;
use vaco_frame::{Frame, FrameData};
use vaco_pixfmt::PixFmt;

/// Geometry filters are exercised on small, varied pictures rather than using
/// fuzz time to allocate a single enormous frame.
const MAX_DRIVER_STEPS: usize = 128;
const MAX_INPUT_DIMENSION_U8: u8 = 64;
const MAX_PAD_GROWTH_U8: u8 = 32;
const MAX_OUTPUT_DIMENSION: u32 = MAX_INPUT_DIMENSION_U8 as u32 + MAX_PAD_GROWTH_U8 as u32;
const PLANE_ALIGNMENT: usize = 64;
const MAX_ALLOCATED_ROW_BYTES: usize =
    (MAX_OUTPUT_DIMENSION as usize).div_ceil(PLANE_ALIGNMENT) * PLANE_ALIGNMENT;
const MAX_ALLOCATED_PLANE_BYTES: usize = MAX_ALLOCATED_ROW_BYTES * MAX_OUTPUT_DIMENSION as usize;

#[derive(Clone, Copy)]
enum Geometry {
    Crop {
        width: u32,
        height: u32,
        x: u32,
        y: u32,
    },
    HFlip,
    Pad {
        width: u32,
        height: u32,
        x: u32,
        y: u32,
    },
    Scale {
        width: u32,
        height: u32,
        flags: &'static str,
    },
    Transpose {
        direction: &'static str,
    },
    VFlip,
}

impl Geometry {
    fn from_data(data: &[u8]) -> (Self, u32, u32) {
        let byte = |index| data.get(index).copied().unwrap_or(0);
        let source_width = u32::from(byte(1) % MAX_INPUT_DIMENSION_U8) + 1;
        let source_height = u32::from(byte(2) % MAX_INPUT_DIMENSION_U8) + 1;
        let geometry = match byte(0) % 6 {
            0 => {
                let width = u32::from(byte(3)) % source_width + 1;
                let height = u32::from(byte(4)) % source_height + 1;
                let x = u32::from(byte(5)) % (source_width - width + 1);
                let y = u32::from(byte(6)) % (source_height - height + 1);
                Self::Crop {
                    width,
                    height,
                    x,
                    y,
                }
            }
            1 => Self::HFlip,
            2 => {
                let x_room = u32::from(byte(3) % MAX_PAD_GROWTH_U8);
                let y_room = u32::from(byte(4) % MAX_PAD_GROWTH_U8);
                Self::Pad {
                    width: source_width + x_room,
                    height: source_height + y_room,
                    x: u32::from(byte(5)) % (x_room + 1),
                    y: u32::from(byte(6)) % (y_room + 1),
                }
            }
            3 => Self::Scale {
                width: u32::from(byte(3) % MAX_INPUT_DIMENSION_U8) + 1,
                height: u32::from(byte(4) % MAX_INPUT_DIMENSION_U8) + 1,
                flags: ["neighbor", "bilinear", "bicubic", "lanczos"][usize::from(byte(5)) % 4],
            },
            4 => Self::Transpose {
                direction: ["cclock_flip", "clock", "cclock", "clock_flip"]
                    [usize::from(byte(3)) % 4],
            },
            _ => Self::VFlip,
        };
        (geometry, source_width, source_height)
    }

    fn graph(self) -> String {
        match self {
            Self::Crop {
                width,
                height,
                x,
                y,
            } => format!("crop=w={width}:h={height}:x={x}:y={y}"),
            Self::HFlip => "hflip".to_owned(),
            Self::Pad {
                width,
                height,
                x,
                y,
            } => format!("pad=w={width}:h={height}:x={x}:y={y}"),
            Self::Scale {
                width,
                height,
                flags,
            } => {
                format!("scale=w={width}:h={height}:flags={flags}")
            }
            Self::Transpose { direction } => format!("transpose=dir={direction}"),
            Self::VFlip => "vflip".to_owned(),
        }
    }

    const fn output_shape(self, source_width: u32, source_height: u32) -> (u32, u32) {
        match self {
            Self::Crop { width, height, .. }
            | Self::Pad { width, height, .. }
            | Self::Scale { width, height, .. } => (width, height),
            Self::Transpose { .. } => (source_height, source_width),
            Self::HFlip | Self::VFlip => (source_width, source_height),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    width: u32,
    height: u32,
    row_stride: usize,
    allocated_bytes: usize,
    pts: Timestamp,
    time_base: Rational,
    pixels: Vec<u8>,
}

fn gray_source_formats() -> NodeFormats {
    NodeFormats {
        inputs: Vec::new(),
        outputs: vec![FormatSet::video_exact(PixFmt::Gray8)],
        ties: Vec::new(),
        label: "fuzz-input".to_owned(),
    }
}

fn any_video_sink() -> NodeFormats {
    NodeFormats {
        inputs: vec![FormatSet::default()],
        outputs: Vec::new(),
        ties: Vec::new(),
        label: "fuzz-output".to_owned(),
    }
}

fn gray_link(width: u32, height: u32) -> LinkFormat {
    LinkFormat::Video {
        format: PixFmt::Gray8,
        width,
        height,
        time_base: Rational::new(1, 25),
        frame_rate: Rational::new(25, 1),
        sample_aspect_ratio: Rational::ONE,
        color: vaco_color::ColorInfo::default(),
    }
}

fn frame_from_data(graph: &vaco_filter_core::Graph, width: u32, height: u32, data: &[u8]) -> Frame {
    let Ok(mut frame) = graph.pool().acquire_video(PixFmt::Gray8, width, height) else {
        panic!("a bounded Gray8 fuzz frame must allocate");
    };
    assert_plane_bounds(&frame, width, height, "input");
    {
        let Some(mut plane) = frame.plane_mut(0) else {
            panic!("a Gray8 frame must have a luma plane");
        };
        for y in 0..plane.rows() {
            let Some(row) = plane.row_mut(y) else {
                panic!("a declared plane row must be addressable");
            };
            let row_len = row.len();
            for (x, pixel) in row.iter_mut().enumerate() {
                let index = y.saturating_mul(row_len).saturating_add(x);
                *pixel = data
                    .get(index % data.len().max(1))
                    .copied()
                    .unwrap_or(index.to_le_bytes()[0].wrapping_mul(17));
            }
        }
    }
    frame.pts = Timestamp::new(0);
    frame.time_base = Rational::new(1, 25);
    frame
}

fn assert_plane_bounds(frame: &Frame, expected_width: u32, expected_height: u32, stage: &str) {
    assert_eq!(frame.plane_count(), 1, "{stage} Gray8 plane count");
    let Some(plane) = frame.plane(0) else {
        panic!("{stage} Gray8 frame must have a luma plane");
    };
    let expected_row_bytes = expected_width as usize;
    let expected_rows = expected_height as usize;
    assert_eq!(plane.row_bytes(), expected_row_bytes, "{stage} row width");
    assert_eq!(plane.rows(), expected_rows, "{stage} row count");
    assert!(
        plane.row(expected_rows).is_none(),
        "{stage} exposed a row beyond its declared height"
    );
    assert_eq!(
        plane.rows_iter().count(),
        expected_rows,
        "{stage} row iterator does not cover the declared height"
    );
    assert!(
        plane.stride() >= plane.row_bytes(),
        "{stage} stride is narrower than its visible row"
    );
    assert!(
        plane.stride() <= MAX_ALLOCATED_ROW_BYTES,
        "{stage} stride exceeded the fuzz allocation bound"
    );
    let allocated = plane.as_slice().len();
    let required = plane.rows().saturating_mul(plane.stride());
    assert!(
        allocated >= required,
        "{stage} plane does not contain all allocated rows"
    );
    assert!(
        allocated <= MAX_ALLOCATED_PLANE_BYTES,
        "{stage} plane allocation exceeded the fuzz bound"
    );
}

fn snapshot(frame: Frame, expected_width: u32, expected_height: u32) -> Snapshot {
    let FrameData::Video {
        format,
        width,
        height,
        ..
    } = &frame.data
    else {
        panic!("a video geometry filter emitted a non-video frame");
    };
    assert_eq!(
        *format,
        PixFmt::Gray8,
        "geometry changed the negotiated format"
    );
    assert_eq!((*width, *height), (expected_width, expected_height));
    assert_eq!(
        frame.pts,
        Timestamp::new(0),
        "geometry changed the frame timestamp"
    );
    assert_eq!(frame.time_base, Rational::new(1, 25));

    assert_plane_bounds(&frame, expected_width, expected_height, "output");

    let Some(plane) = frame.plane(0) else {
        panic!("a Gray8 output must have a luma plane");
    };
    assert_eq!(plane.rows(), expected_height as usize);
    let mut pixels = Vec::new();
    for y in 0..plane.rows() {
        let Some(row) = plane.row(y) else {
            panic!("a declared output row must be addressable");
        };
        assert_eq!(
            row.len(),
            expected_width as usize,
            "output row has wrong width"
        );
        pixels.extend_from_slice(row);
    }
    Snapshot {
        width: *width,
        height: *height,
        row_stride: plane.stride(),
        allocated_bytes: plane.as_slice().len(),
        pts: frame.pts,
        time_base: frame.time_base,
        pixels,
    }
}

fn drive(geometry: Geometry, source_width: u32, source_height: u32, data: &[u8]) -> Snapshot {
    let registry = GeometryRegistry;
    let graph_text = geometry.graph();
    let mut built = parse_and_build(&graph_text, &registry).unwrap_or_else(|error| {
        panic!("{graph_text:?} should build: {}", error.render(&graph_text))
    });
    assert_eq!(
        built.open_inputs.len(),
        1,
        "one-input geometry graph changed shape"
    );
    assert_eq!(
        built.open_outputs.len(),
        1,
        "one-output geometry graph changed shape"
    );
    let source = built
        .attach_source(
            0,
            gray_source_formats(),
            gray_link(source_width, source_height),
        )
        .unwrap_or_else(|error| panic!("{graph_text:?}: attach source: {error}"));
    assert!(
        built.open_inputs.is_empty(),
        "{graph_text:?}: source attachment left an open input"
    );
    let sink = built
        .attach_sink(0, any_video_sink())
        .unwrap_or_else(|error| panic!("{graph_text:?}: attach sink: {error}"));
    assert!(
        built.open_outputs.is_empty(),
        "{graph_text:?}: sink attachment left an open output"
    );
    built
        .configure(&registry, AutoConvert::None)
        .unwrap_or_else(|error| panic!("{graph_text:?}: negotiation failed: {error}"));

    let (expected_width, expected_height) = geometry.output_shape(source_width, source_height);
    let negotiated = built
        .graph
        .sink_format(sink)
        .unwrap_or_else(|error| panic!("{graph_text:?}: no sink format: {error}"));
    let LinkFormat::Video {
        format,
        width,
        height,
        time_base,
        ..
    } = negotiated
    else {
        panic!("{graph_text:?}: video graph negotiated a non-video sink");
    };
    assert_eq!(
        *format,
        PixFmt::Gray8,
        "{graph_text:?}: negotiation changed format"
    );
    assert_eq!((*width, *height), (expected_width, expected_height));
    assert_eq!(*time_base, Rational::new(1, 25));

    let frame = frame_from_data(&built.graph, source_width, source_height, data);
    if let Err(rejected) = built.graph.send(source, frame) {
        panic!(
            "{graph_text:?}: source rejected valid frame: {:?}",
            rejected.error
        );
    }
    built
        .graph
        .close_source(source, Timestamp::new(1))
        .unwrap_or_else(|error| panic!("{graph_text:?}: close source: {error}"));

    let mut outputs = Vec::new();
    let mut finished = false;
    for _ in 0..MAX_DRIVER_STEPS {
        loop {
            match built.graph.recv(sink) {
                Ok(frame) => {
                    assert!(
                        outputs.len() < 2,
                        "{graph_text:?}: geometry emitted more than one frame"
                    );
                    outputs.push(snapshot(frame, expected_width, expected_height));
                }
                Err(Error::NeedMoreInput) => break,
                Err(Error::Eof) => {
                    finished = true;
                    break;
                }
                Err(error) => panic!("{graph_text:?}: sink failed: {error}"),
            }
        }
        if finished {
            break;
        }
        match built.graph.run_once() {
            Ok(Progress::Stepped) => {}
            Ok(Progress::Quiescent) => match built.graph.classify() {
                GraphStatus::Eof => finished = true,
                status => panic!("{graph_text:?}: stopped before EOF: {status:?}"),
            },
            Err(error) => panic!("{graph_text:?}: filter failed: {error}"),
        }
    }
    assert!(finished, "{graph_text:?}: driver did not reach EOF");
    assert!(
        built.graph.violations().is_empty(),
        "{graph_text:?}: {:?}",
        built.graph.violations()
    );
    assert_eq!(
        outputs.len(),
        1,
        "{graph_text:?}: geometry did not preserve frame count"
    );
    outputs
        .pop()
        .unwrap_or_else(|| panic!("{graph_text:?}: missing output"))
}

fuzz_target!(|data: &[u8]| {
    let (geometry, source_width, source_height) = Geometry::from_data(data);
    let first = drive(geometry, source_width, source_height, data);
    let second = drive(geometry, source_width, source_height, data);
    assert_eq!(first, second, "geometry output was not deterministic");
});
