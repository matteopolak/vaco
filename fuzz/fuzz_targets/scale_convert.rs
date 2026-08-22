//! `vaco-scale` planning and execution over arbitrary format pairs and sizes.
//!
//! Every input here is attacker-controlled in real life: the source format and
//! size come from a sequence header, the destination from a command line, and
//! the option string from a filtergraph. Degenerate geometry — zero, one, odd,
//! enormous, and every subsampling boundary — is exactly where a scaler reads
//! past the end of a row, so the target sweeps it deliberately rather than
//! hoping the mutator finds it.
//!
//! A finding is a panic, an unbounded allocation, a non-termination, or an
//! output plane the conversion wrote past. Byte values are not checked here:
//! `tests/reference.rs` is where fidelity is measured.
//! fuzz-crate: vaco-scale
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_limits::Limits;
use vaco_pixfmt::PixFmt;
use vaco_scale::exec::{DstPlane, SrcPlane};
use vaco_scale::{ImageSpec, ScaleOptions, Scaler};

#[derive(Arbitrary, Debug)]
struct Input {
    src_format: u16,
    dst_format: u16,
    src_w: u16,
    src_h: u16,
    dst_w: u16,
    dst_h: u16,
    /// Colour signalling code points, straight from a VUI.
    primaries: u8,
    transfer: u8,
    matrix: u8,
    range: u8,
    /// A filtergraph-style option string.
    options: String,
    /// Source bytes; short on purpose, so the reader's truncation path runs.
    payload: Vec<u8>,
    tiny_budget: bool,
}

/// Keep the picture small enough that the fuzzer explores shapes rather than
/// spending its time on memory bandwidth, but wide enough to cross every
/// subsampling boundary and every SIMD tail.
fn dim(v: u16) -> u32 {
    u32::from(v % 67)
}

fuzz_target!(|input: Input| {
    let all = PixFmt::all();
    let sfmt = all[input.src_format as usize % all.len()];
    let dfmt = all[input.dst_format as usize % all.len()];
    let (sw, sh) = (dim(input.src_w), dim(input.src_h));
    let (dw, dh) = (dim(input.dst_w), dim(input.dst_h));

    let mut color = vaco_color::ColorInfo::default();
    if let Some(p) = vaco_color::ColorPrimaries::from_u8(input.primaries) {
        color.primaries = p;
    }
    if let Some(t) = vaco_color::TransferCharacteristic::from_u8(input.transfer) {
        color.transfer = t;
    }
    if let Some(m) = vaco_color::MatrixCoefficients::from_u8(input.matrix) {
        color.matrix = m;
    }
    if let Some(r) = vaco_color::ColorRange::from_u8(input.range) {
        color.range = r;
    }

    let mut opts = ScaleOptions::default();
    // A rejected option string must not stop the conversion: the reference
    // accepts far more than we implement, and refusing is the caller's choice.
    let _ = opts.parse(&input.options);
    // The pool is not what is under test and spawning threads per iteration
    // would dominate the run.
    opts.threads = 0;

    let src_spec = ImageSpec::new(sfmt, sw, sh).with_color(color);
    let dst_spec = ImageSpec::new(dfmt, dw, dh).with_color(color);
    let limits = if input.tiny_budget {
        Limits::tiny()
    } else {
        Limits::permissive()
    };

    let Ok(mut scaler) = Scaler::with_limits(&src_spec, &dst_spec, &opts, limits) else {
        return;
    };

    // Build source planes at exactly the geometry the format demands, filled
    // from the payload so a short payload still produces a well-formed picture.
    let Ok(src_layout) = sfmt.plane_layout(sw.max(1), sh.max(1), 64) else {
        return;
    };
    let Ok(dst_layout) = dfmt.plane_layout(dw.max(1), dh.max(1), 64) else {
        return;
    };
    if src_layout.total > 1 << 22 || dst_layout.total > 1 << 22 {
        return;
    }

    let mut src_bufs: Vec<Vec<u8>> = Vec::new();
    for p in 0..sfmt.plane_count() {
        let n = src_layout.sizes[p];
        let mut buf = vec![0u8; n];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = input
                .payload
                .get(i % input.payload.len().max(1))
                .copied()
                .unwrap_or((i * 31) as u8);
        }
        src_bufs.push(buf);
    }
    let mut dst_bufs: Vec<Vec<u8>> = (0..dfmt.plane_count())
        .map(|p| vec![0xAAu8; dst_layout.sizes[p]])
        .collect();

    let srcs: Vec<SrcPlane<'_>> = src_bufs
        .iter()
        .enumerate()
        .map(|(p, d)| SrcPlane {
            data: d,
            stride: src_layout.strides[p],
        })
        .collect();
    let mut dsts: Vec<DstPlane<'_>> = dst_bufs
        .iter_mut()
        .enumerate()
        .map(|(p, d)| DstPlane {
            data: d,
            stride: dst_layout.strides[p],
        })
        .collect();

    // Either it converts or it reports; a panic is the finding.
    let _ = scaler.scale_planes(&srcs, &mut dsts);

    // Whatever happened, `explain()` must describe it without panicking — it is
    // reachable from `-v debug` on the same untrusted geometry.
    let _ = scaler.explain();
    let _ = scaler.is_noop();
});
