//! Fuzz all real `vaco-filter-convolve` implementations with real frame data.
//!
//! `filter_convolve_options` stops after constructing each filter. This target
//! renders only valid, bounded options, builds the public filtergraph, attaches
//! real Gray8 sources and a sink, negotiates it, and drives fuzzed pixels through
//! the scheduler. It includes `morpho`'s second structuring-element input, so all
//! twelve registered filters reach their frame-processing implementation.
//!
//! Every case must terminate at EOF without graph-contract violations, emit one
//! same-sized frame with preserved timing and bounded storage, and reproduce the
//! same visible bytes in a fresh graph. The max/min/average morphology filters
//! also carry their independent pointwise monotonicity invariants.
//!
//! fuzz-crate: vaco-filter-convolve
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_core::{Duration, Error, Rational, Timestamp};
use vaco_filter_convolve::ConvolveRegistry;
use vaco_filter_core::negotiate::{AutoConvert, FormatSet, NodeFormats};
use vaco_filter_core::{GraphStatus, LinkFormat, Progress};
use vaco_filter_graph::parse_and_build;
use vaco_frame::{Frame, FrameData};
use vaco_pixfmt::PixFmt;

const MAX_DIMENSION_U8: u8 = 32;
const MAX_STRUCTURE_RADIUS_U8: u8 = 3;
const MAX_DRIVER_STEPS: usize = 256;
const PLANE_ALIGNMENT: usize = 64;
const MAX_ALLOCATED_ROW_BYTES: usize =
    (MAX_DIMENSION_U8 as usize).div_ceil(PLANE_ALIGNMENT) * PLANE_ALIGNMENT;
const MAX_ALLOCATED_PLANE_BYTES: usize = MAX_ALLOCATED_ROW_BYTES * MAX_DIMENSION_U8 as usize;

#[derive(Clone, Copy, Debug)]
enum Case {
    Convolution {
        kernel: &'static str,
        mode: &'static str,
        rdiv: &'static str,
        bias: &'static str,
    },
    Deflate {
        threshold: u16,
    },
    Dilation {
        coordinates: u8,
        threshold: u16,
    },
    Erosion {
        coordinates: u8,
        threshold: u16,
    },
    Inflate {
        threshold: u16,
    },
    Kirsch {
        scale: &'static str,
        delta: i32,
    },
    Median {
        radius: u8,
        radius_v: u8,
        percentile: &'static str,
    },
    Morpho {
        mode: MorphoMode,
        freeze: bool,
    },
    Prewitt {
        scale: &'static str,
        delta: i32,
    },
    Roberts {
        scale: &'static str,
        delta: i32,
    },
    Scharr {
        scale: &'static str,
        delta: i32,
    },
    Sobel {
        scale: &'static str,
        delta: i32,
    },
}

#[derive(Clone, Copy, Debug)]
enum MorphoMode {
    Erode,
    Dilate,
    Open,
    Close,
    Gradient,
    Tophat,
    Blackhat,
}

impl MorphoMode {
    const fn name(self) -> &'static str {
        match self {
            Self::Erode => "erode",
            Self::Dilate => "dilate",
            Self::Open => "open",
            Self::Close => "close",
            Self::Gradient => "gradient",
            Self::Tophat => "tophat",
            Self::Blackhat => "blackhat",
        }
    }
}

impl Case {
    fn from_data(data: &[u8]) -> (Self, u32, u32) {
        const THRESHOLDS: &[u16] = &[0, 1, 16, 255, 256, 65_535];
        const EDGE_SCALES: &[&str] = &["0", "0.25", "0.5", "1", "2", "8", "64", "65535"];
        const EDGE_DELTAS: &[i32] = &[0, -65_535, -32, 32, 65_535];
        let byte = |index| data.get(index).copied().unwrap_or(0);
        let width = u32::from(byte(1) % MAX_DIMENSION_U8) + 1;
        let height = u32::from(byte(2) % MAX_DIMENSION_U8) + 1;
        let threshold = THRESHOLDS[usize::from(byte(4)) % THRESHOLDS.len()];
        let scale = EDGE_SCALES[usize::from(byte(5)) % EDGE_SCALES.len()];
        let delta = EDGE_DELTAS[usize::from(byte(6)) % EDGE_DELTAS.len()];
        let case = match byte(0) % 12 {
            0 => {
                const KERNELS: &[(&str, &str)] = &[
                    ("1", "square"),
                    ("1 2 1", "row"),
                    ("1 2 1", "column"),
                    ("0 1 0 1 4 1 0 1 0", "square"),
                    ("-1 0 1 -2 0 2 -1 0 1", "square"),
                ];
                const RDIVS: &[&str] = &["0", "0.25", "0.5", "1", "2", "16"];
                const BIASES: &[&str] = &["0", "-32", "-0.5", "0.5", "32"];
                let (kernel, mode) = KERNELS[usize::from(byte(3)) % KERNELS.len()];
                Self::Convolution {
                    kernel,
                    mode,
                    rdiv: RDIVS[usize::from(byte(4)) % RDIVS.len()],
                    bias: BIASES[usize::from(byte(5)) % BIASES.len()],
                }
            }
            1 => Self::Deflate { threshold },
            2 => Self::Dilation {
                coordinates: byte(3),
                threshold,
            },
            3 => Self::Erosion {
                coordinates: byte(3),
                threshold,
            },
            4 => Self::Inflate { threshold },
            5 => Self::Kirsch { scale, delta },
            6 => {
                const PERCENTILES: &[&str] = &["0", "0.25", "0.5", "0.75", "1"];
                Self::Median {
                    radius: byte(3) % 4 + 1,
                    radius_v: byte(4) % 5,
                    percentile: PERCENTILES[usize::from(byte(5)) % PERCENTILES.len()],
                }
            }
            7 => {
                let mode = match byte(3) % 7 {
                    0 => MorphoMode::Erode,
                    1 => MorphoMode::Dilate,
                    2 => MorphoMode::Open,
                    3 => MorphoMode::Close,
                    4 => MorphoMode::Gradient,
                    5 => MorphoMode::Tophat,
                    _ => MorphoMode::Blackhat,
                };
                Self::Morpho {
                    mode,
                    freeze: byte(4) & 1 != 0,
                }
            }
            8 => Self::Prewitt { scale, delta },
            9 => Self::Roberts { scale, delta },
            10 => Self::Scharr { scale, delta },
            _ => Self::Sobel { scale, delta },
        };
        (case, width, height)
    }

    fn graph(self) -> String {
        match self {
            Self::Convolution {
                kernel,
                mode,
                rdiv,
                bias,
            } => format!("convolution=0m='{kernel}':0mode={mode}:0rdiv={rdiv}:0bias={bias}"),
            Self::Deflate { threshold } => format!("deflate=threshold0={threshold}"),
            Self::Dilation {
                coordinates,
                threshold,
            } => format!("dilation=coordinates={coordinates}:threshold0={threshold}"),
            Self::Erosion {
                coordinates,
                threshold,
            } => format!("erosion=coordinates={coordinates}:threshold0={threshold}"),
            Self::Inflate { threshold } => format!("inflate=threshold0={threshold}"),
            Self::Kirsch { scale, delta } => Self::edge_graph("kirsch", scale, delta),
            Self::Median {
                radius,
                radius_v,
                percentile,
            } => format!("median=radius={radius}:radiusV={radius_v}:percentile={percentile}"),
            Self::Morpho { mode, freeze } => format!(
                "morpho=mode={}:structure={}",
                mode.name(),
                if freeze { "first" } else { "all" }
            ),
            Self::Prewitt { scale, delta } => Self::edge_graph("prewitt", scale, delta),
            Self::Roberts { scale, delta } => Self::edge_graph("roberts", scale, delta),
            Self::Scharr { scale, delta } => Self::edge_graph("scharr", scale, delta),
            Self::Sobel { scale, delta } => Self::edge_graph("sobel", scale, delta),
        }
    }

    fn edge_graph(name: &str, scale: &str, delta: i32) -> String {
        format!("{name}=scale={scale}:delta={delta}")
    }

    const fn input_count(self) -> usize {
        if matches!(self, Self::Morpho { .. }) {
            2
        } else {
            1
        }
    }

    fn assert_semantics(self, input: &[u8], output: &[u8]) {
        assert_eq!(
            input.len(),
            output.len(),
            "filter changed visible byte count"
        );
        let relation = match self {
            Self::Dilation { .. } | Self::Inflate { .. } => Some(core::cmp::Ordering::Greater),
            Self::Erosion { .. } | Self::Deflate { .. } => Some(core::cmp::Ordering::Less),
            Self::Morpho {
                mode: MorphoMode::Dilate,
                ..
            } => Some(core::cmp::Ordering::Greater),
            Self::Morpho {
                mode: MorphoMode::Erode,
                ..
            } => Some(core::cmp::Ordering::Less),
            _ => None,
        };
        if let Some(ordering) = relation {
            for (&before, &after) in input.iter().zip(output) {
                match ordering {
                    core::cmp::Ordering::Greater => {
                        assert!(after >= before, "extensive filter decreased a pixel")
                    }
                    core::cmp::Ordering::Less => {
                        assert!(after <= before, "anti-extensive filter increased a pixel")
                    }
                    core::cmp::Ordering::Equal => unreachable!(),
                }
            }
        }
        if matches!(
            self,
            Self::Deflate { threshold: 0 }
                | Self::Dilation { threshold: 0, .. }
                | Self::Erosion { threshold: 0, .. }
                | Self::Inflate { threshold: 0 }
        ) {
            assert_eq!(output, input, "a zero threshold must suppress every change");
        }
        if matches!(self, Self::Median { .. })
            && let (Some(&minimum), Some(&maximum)) = (input.iter().min(), input.iter().max())
        {
            assert!(
                output
                    .iter()
                    .all(|&pixel| pixel >= minimum && pixel <= maximum),
                "an order statistic escaped the input value range"
            );
        }
        if matches!(
            self,
            Self::Convolution {
                kernel: "1",
                rdiv: "0" | "1",
                bias: "0",
                ..
            }
        ) {
            assert_eq!(output, input, "the scalar identity kernel changed pixels");
        }
        let zero_scale_delta = match self {
            Self::Kirsch { scale: "0", delta }
            | Self::Prewitt { scale: "0", delta }
            | Self::Roberts { scale: "0", delta }
            | Self::Scharr { scale: "0", delta }
            | Self::Sobel { scale: "0", delta } => Some(delta),
            _ => None,
        };
        if let Some(delta) = zero_scale_delta {
            let expected = u8::try_from(delta.clamp(0, 255)).unwrap_or(0);
            assert!(
                output.iter().all(|&pixel| pixel == expected),
                "scale=0 did not reduce the edge operator to its clipped delta"
            );
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
    duration: Duration,
    time_base: Rational,
    pixels: Vec<u8>,
}

fn gray_source_formats(label: &str) -> NodeFormats {
    NodeFormats {
        inputs: Vec::new(),
        outputs: vec![FormatSet::video_exact(PixFmt::Gray8)],
        ties: Vec::new(),
        label: label.to_owned(),
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

fn fill_frame(
    graph: &vaco_filter_core::Graph,
    width: u32,
    height: u32,
    data: &[u8],
    force_centre: bool,
) -> Frame {
    let Ok(mut frame) = graph.pool().acquire_video(PixFmt::Gray8, width, height) else {
        panic!("a bounded Gray8 fuzz frame must allocate");
    };
    assert_plane_bounds(&frame, width, height, "input");
    let centre_x = width as usize / 2;
    let centre_y = height as usize / 2;
    if let Some(mut plane) = frame.plane_mut(0) {
        for y in 0..plane.rows() {
            if let Some(row) = plane.row_mut(y) {
                let row_len = row.len();
                for (x, pixel) in row.iter_mut().enumerate() {
                    let index = y.saturating_mul(row_len).saturating_add(x);
                    *pixel = data
                        .get(index % data.len().max(1))
                        .copied()
                        .unwrap_or(index.to_le_bytes()[0].wrapping_mul(29));
                    if force_centre && x == centre_x && y == centre_y {
                        *pixel = 255;
                    }
                }
            }
        }
    }
    frame.pts = Timestamp::new(0);
    frame.time_base = Rational::new(1, 25);
    frame.set_duration_ticks(1);
    frame
}

fn visible_pixels(frame: &Frame) -> Vec<u8> {
    let Some(plane) = frame.plane(0) else {
        panic!("a Gray8 frame must have a luma plane");
    };
    plane
        .rows_iter()
        .flat_map(|row| row.iter().copied())
        .collect()
}

fn payload(data: &[u8], offset: usize) -> &[u8] {
    data.get(offset..)
        .filter(|bytes| !bytes.is_empty())
        .unwrap_or(data)
}

fn assert_plane_bounds(frame: &Frame, width: u32, height: u32, stage: &str) {
    assert_eq!(frame.plane_count(), 1, "{stage} Gray8 plane count");
    let Some(plane) = frame.plane(0) else {
        panic!("{stage} Gray8 frame must have a luma plane");
    };
    assert_eq!(plane.row_bytes(), width as usize, "{stage} row width");
    assert_eq!(plane.rows(), height as usize, "{stage} row count");
    assert!(
        plane.row(height as usize).is_none(),
        "{stage} exposed an extra row"
    );
    assert_eq!(plane.rows_iter().count(), height as usize);
    assert!(plane.stride() >= plane.row_bytes());
    assert!(plane.stride() <= MAX_ALLOCATED_ROW_BYTES);
    let allocated = plane.as_slice().len();
    assert!(allocated >= plane.rows().saturating_mul(plane.stride()));
    assert!(allocated <= MAX_ALLOCATED_PLANE_BYTES);
}

fn snapshot(frame: Frame, width: u32, height: u32) -> Snapshot {
    let FrameData::Video {
        format,
        width: actual_width,
        height: actual_height,
        ..
    } = &frame.data
    else {
        panic!("a convolve filter emitted a non-video frame");
    };
    assert_eq!(*format, PixFmt::Gray8);
    assert_eq!((*actual_width, *actual_height), (width, height));
    assert_eq!(frame.pts, Timestamp::new(0));
    assert_eq!(frame.duration_ticks(), 1);
    assert_eq!(frame.time_base, Rational::new(1, 25));
    assert_plane_bounds(&frame, width, height, "output");
    let Some(plane) = frame.plane(0) else {
        panic!("a Gray8 output must have a luma plane");
    };
    Snapshot {
        width,
        height,
        row_stride: plane.stride(),
        allocated_bytes: plane.as_slice().len(),
        pts: frame.pts,
        duration: frame.duration,
        time_base: frame.time_base,
        pixels: visible_pixels(&frame),
    }
}

fn drive(case: Case, width: u32, height: u32, data: &[u8]) -> (Vec<u8>, Snapshot) {
    let registry = ConvolveRegistry;
    let graph_text = case.graph();
    let mut built = parse_and_build(&graph_text, &registry).unwrap_or_else(|error| {
        panic!("{graph_text:?} should build: {}", error.render(&graph_text))
    });
    assert_eq!(built.open_inputs.len(), case.input_count());
    assert_eq!(built.open_outputs.len(), 1);

    let main = built
        .attach_source(
            0,
            gray_source_formats("fuzz-main"),
            gray_link(width, height),
        )
        .unwrap_or_else(|error| panic!("{graph_text:?}: attach main: {error}"));
    let structure = if case.input_count() == 2 {
        let radius = u32::from(
            data.get(7).copied().unwrap_or(0) % MAX_STRUCTURE_RADIUS_U8.saturating_add(1),
        );
        let side = radius.saturating_mul(2).saturating_add(1);
        Some((
            built
                .attach_source(
                    0,
                    gray_source_formats("fuzz-structure"),
                    gray_link(side, side),
                )
                .unwrap_or_else(|error| panic!("{graph_text:?}: attach structure: {error}")),
            side,
        ))
    } else {
        None
    };
    let sink = built
        .attach_sink(0, any_video_sink())
        .unwrap_or_else(|error| panic!("{graph_text:?}: attach sink: {error}"));
    assert!(built.open_inputs.is_empty());
    assert!(built.open_outputs.is_empty());
    built
        .configure(&registry, AutoConvert::None)
        .unwrap_or_else(|error| panic!("{graph_text:?}: negotiation failed: {error}"));

    let LinkFormat::Video {
        format,
        width: output_width,
        height: output_height,
        time_base,
        ..
    } = built
        .graph
        .sink_format(sink)
        .unwrap_or_else(|error| panic!("{graph_text:?}: no sink format: {error}"))
    else {
        panic!("{graph_text:?}: video graph negotiated a non-video sink");
    };
    assert_eq!(*format, PixFmt::Gray8);
    assert_eq!((*output_width, *output_height), (width, height));
    assert_eq!(*time_base, Rational::new(1, 25));

    let main_frame = fill_frame(&built.graph, width, height, payload(data, 16), false);
    let input = visible_pixels(&main_frame);
    if let Err(rejected) = built.graph.send(main, main_frame) {
        panic!(
            "{graph_text:?}: main source rejected frame: {:?}",
            rejected.error
        );
    }
    built
        .graph
        .close_source(main, Timestamp::new(1))
        .unwrap_or_else(|error| panic!("{graph_text:?}: close main: {error}"));

    if let Some((source, side)) = structure {
        let structure_frame = fill_frame(&built.graph, side, side, payload(data, 8), true);
        if let Err(rejected) = built.graph.send(source, structure_frame) {
            panic!(
                "{graph_text:?}: structure source rejected frame: {:?}",
                rejected.error
            );
        }
        built
            .graph
            .close_source(source, Timestamp::new(1))
            .unwrap_or_else(|error| panic!("{graph_text:?}: close structure: {error}"));
    }

    let mut outputs = Vec::new();
    let mut finished = false;
    for _ in 0..MAX_DRIVER_STEPS {
        loop {
            match built.graph.recv(sink) {
                Ok(frame) => {
                    assert!(outputs.len() < 2, "{graph_text:?}: emitted too many frames");
                    outputs.push(snapshot(frame, width, height));
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
        "{graph_text:?}: did not preserve frame count"
    );
    let output = outputs
        .pop()
        .unwrap_or_else(|| panic!("{graph_text:?}: missing output"));
    (input, output)
}

fuzz_target!(|data: &[u8]| {
    let (case, width, height) = Case::from_data(data);
    let (input, first) = drive(case, width, height, data);
    let (_, second) = drive(case, width, height, data);
    assert_eq!(first, second, "{case:?} output was not deterministic");
    case.assert_semantics(&input, &first.pixels);
});
