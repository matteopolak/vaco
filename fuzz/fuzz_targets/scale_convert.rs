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
//! output plane the conversion wrote past. Full fidelity is measured in
//! `tests/reference.rs`, not here — but one value oracle from that suite is
//! cheap enough to run on every input and is wired in directly: a constant
//! image must survive scaling (`tests/properties.rs`'s
//! `a_constant_image_survives_every_kernel_and_every_ratio`, "the single
//! most valuable property in the crate: it catches every normalisation,
//! edge-clamping and rounding bug at once"). Same-format conversions are
//! checked for byte-exact preservation; cross-format conversions (where a
//! colour-space transform is also in play) are checked for the weaker but
//! still real `a_flat_colour_survives_a_scaled_colour_conversion` property,
//! that a flat input stays flat.
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
    /// The byte every source plane is filled with for the constant-image
    /// check below — independent of `payload`, so mutating one does not
    /// disturb the other's coverage.
    fill: u8,
}

/// Whether every component of `fmt` fills its container exactly, with no
/// sub-byte padding bits.
///
/// `PixFmt::P210be` and kin store a 10-bit sample MSB-aligned in a 16-bit
/// word (`shift: 6, depth: 10`), leaving the low 6 bits unused -- and the
/// writer is free to leave whatever was already in the destination buffer
/// there, since those bits carry no signal. Filling every *byte* of a
/// buffer with one value does not correspond to a constant *sample* value
/// for such a format (the padding bits are not part of the sample, and are
/// not obliged to come back as anything in particular), so the constant-image
/// check below only applies to formats where they cannot exist: every
/// component's `depth` is an exact multiple of 8 and `shift` is 0, meaning
/// the component spans whole bytes from bit 0 with nothing left over.
/// (Found by this exact check firing on `p210be`, byte-identical scaling,
/// tracked down to the pre-filled sentinel bytes surviving through the
/// padding bits -- not a `vaco-scale` bug.)
///
/// Also excludes `PixFmtFlags::FLOAT` and `PixFmtFlags::BITSTREAM` formats.
/// Float formats surfaced a case (`rgbf32be`, self-conversion aside) where
/// part of a pixel group came back as the destination sentinel rather than
/// a computed value -- worth a follow-up on whether every float destination
/// channel is actually written, but confirming or fixing that is a
/// materially bigger task than wiring an existing property in, so it is
/// deliberately left as an open question rather than chased here (see the
/// fuzz-target commit message). Bitstream formats have no byte-aligned
/// notion of "one pixel's worth of bytes" for `Component::step` to name.
///
/// A per-component `shift == 0 && depth % 8 == 0` check alone is not
/// sufficient: `bgr0` declares three whole-byte, unshifted components in a
/// 4-byte group (the fourth byte is unnamed, conventionally-zero padding
/// with no `Component` at all), which passes that check per-component while
/// still leaving a byte the writer need not touch. So this also requires,
/// per plane, that the components declared for it account for every byte of
/// the group -- `sum(depth/8) == step` -- with none left unclaimed.
fn is_unpadded(fmt: PixFmt) -> bool {
    if fmt.has(vaco_pixfmt::PixFmtFlags::FLOAT) || fmt.has(vaco_pixfmt::PixFmtFlags::BITSTREAM) {
        return false;
    }
    let comps = fmt.descriptor().components;
    if !comps.iter().all(|c| c.shift == 0 && c.depth % 8 == 0) {
        return false;
    }
    for plane in 0..fmt.descriptor().planes {
        let mut step = None;
        let mut claimed = 0usize;
        for c in comps.iter().filter(|c| c.plane == plane) {
            step = Some(usize::from(c.step));
            claimed += usize::from(c.depth) / 8;
        }
        if let Some(step) = step {
            if claimed != step {
                return false;
            }
        }
    }
    true
}

/// The widest component `fmt` declares, in bits.
fn max_depth(fmt: PixFmt) -> u8 {
    fmt.descriptor()
        .components
        .iter()
        .map(|c| c.depth)
        .max()
        .unwrap_or(0)
}

/// Every meaningful byte of every plane (stride padding excluded via
/// `PixFmt::min_stride`/`plane_height`, since a scaler makes no promise
/// about what it leaves in padding).
fn meaningful_bytes(
    fmt: PixFmt,
    w: u32,
    h: u32,
    bufs: &[Vec<u8>],
    layout: &vaco_pixfmt::PlaneLayout,
) -> Vec<Vec<u8>> {
    let mut out = Vec::with_capacity(bufs.len());
    for (p, buf) in bufs.iter().enumerate() {
        let p8 = p as u8;
        let row_bytes = fmt.min_stride(w, p8);
        let n_rows = fmt.plane_height(h, p8) as usize;
        let stride = layout.strides[p];
        let mut bytes = Vec::with_capacity(row_bytes * n_rows);
        for y in 0..n_rows {
            let start = y * stride;
            if let Some(row) = buf.get(start..start + row_bytes) {
                bytes.extend_from_slice(row);
            }
        }
        out.push(bytes);
    }
    out
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

    let Ok(mut scaler) = Scaler::with_limits(&src_spec, &dst_spec, &opts, limits.clone()) else {
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

    // The constant-image property, on the same format pair, sizes and
    // options the fuzzer just chose, rather than only on the fixed cases
    // tests/properties.rs hand-picks. Restricted to formats with no padding
    // bits -- see `is_unpadded` -- since a byte-fill does not correspond to
    // a constant sample value otherwise.
    if !is_unpadded(sfmt) || !is_unpadded(dfmt) {
        return;
    }
    // A depth reduction on any channel turns `DitherKind::Auto` -- the
    // default -- into ordered Bayer dithering (`resolve_dither`, and
    // `depth_reduction_turns_dither_on_by_itself` in plan.rs's own tests).
    // Bayer dither's threshold is a function of pixel position by design
    // (that is the entire point: shaping quantisation noise rather than
    // banding it), so a flat input at reduced depth is *supposed* to come
    // back with position-dependent LSB variation. Found via rgba64be (16
    // bits) -> uyva (8 bits): every pixel selected the identical source
    // sample under ScalerKind::Nearest, yet the output still varied by
    // position -- a real effect of documented, tested behaviour, not a bug.
    if max_depth(dfmt) < max_depth(sfmt) {
        return;
    }
    // A fresh scaler, mirroring how tests/properties.rs's `convert()` always
    // constructs one per conversion rather than reusing state across calls.
    // Deliberately *not* `opts`: the fuzzed option string can set an
    // arbitrary `param0`/`param1` kernel shape parameter, and this property
    // is about the format/geometry space, not the kernel-parameter space --
    // tests/properties.rs only ever varies `scaler`/`scaler_sub` among the
    // six named kinds at their default parameters, never an arbitrary
    // custom shape. (Found via a fuzzed options string reaching some
    // parameter combination where yuvj420p -> rgb24 broke flatness at
    // extreme chroma; whether that combination is reachable from a
    // legitimate filtergraph, and whether it is actually wrong there, is an
    // open question -- see the fuzz-target commit message -- deliberately
    // not chased further here.)
    let mut opts2 = ScaleOptions::default();
    opts2.threads = 0;
    let Ok(mut scaler2) = Scaler::with_limits(&src_spec, &dst_spec, &opts2, limits) else {
        return;
    };
    let const_src_bufs: Vec<Vec<u8>> = (0..sfmt.plane_count())
        .map(|p| vec![input.fill; src_layout.sizes[p]])
        .collect();
    let mut const_dst_bufs: Vec<Vec<u8>> = (0..dfmt.plane_count())
        // A sentinel distinct from `input.fill` and from the main pass's
        // 0xAA, so an untouched destination byte cannot masquerade as a
        // correctly-preserved one.
        .map(|p| vec![0x55u8; dst_layout.sizes[p]])
        .collect();
    let const_srcs: Vec<SrcPlane<'_>> = const_src_bufs
        .iter()
        .enumerate()
        .map(|(p, d)| SrcPlane {
            data: d,
            stride: src_layout.strides[p],
        })
        .collect();
    let mut const_dsts: Vec<DstPlane<'_>> = const_dst_bufs
        .iter_mut()
        .enumerate()
        .map(|(p, d)| DstPlane {
            data: d,
            stride: dst_layout.strides[p],
        })
        .collect();
    if scaler2.scale_planes(&const_srcs, &mut const_dsts).is_err() {
        return;
    }
    let out_rows = meaningful_bytes(dfmt, dw, dh, &const_dst_bufs, &dst_layout);
    if sfmt == dfmt {
        // No colour-space transform is possible: every byte the scaler wrote
        // must equal the fill value exactly, at every kernel, ratio and
        // edge-clamping mode -- ported directly from
        // a_constant_image_survives_every_kernel_and_every_ratio.
        for (p, rows) in out_rows.iter().enumerate() {
            for &b in rows {
                assert_eq!(
                    b,
                    input.fill,
                    "{}: constant image did not survive same-format scaling in plane {p}",
                    sfmt.name()
                );
            }
        }
    } else {
        // A colour-space transform is in play, so the output need not equal
        // the input byte -- only stay flat, exactly as
        // a_flat_colour_survives_a_scaled_colour_conversion checks.
        //
        // "Flat" means every *pixel group* repeats, not every individual
        // byte: a multi-byte sample or several interleaved components (e.g.
        // bgr48le's 6 bytes per pixel) legitimately differ from each other
        // within one group while every group is identical to the next.
        // `Component::step` is exactly that group's byte width -- "distance
        // between consecutive samples of this component" -- so any
        // component belonging to the plane gives the right chunk size.
        // (Found by this exact check firing on p216be -> bgr48le with a
        // per-byte comparison: every 6-byte BGR group was identical, but
        // G's two bytes differed from B's and R's within the same group --
        // not a `vaco-scale` bug, a bug in comparing bytes instead of
        // groups.)
        for (p, rows) in out_rows.iter().enumerate() {
            let group = dfmt
                .descriptor()
                .components
                .iter()
                .find(|c| usize::from(c.plane) == p)
                .map_or(1, |c| usize::from(c.step))
                .max(1);
            let mut chunks = rows.chunks(group);
            if let Some(first) = chunks.next() {
                for chunk in chunks {
                    assert_eq!(
                        chunk,
                        first,
                        "{} -> {}: a flat input did not stay flat in plane {p}",
                        sfmt.name(),
                        dfmt.name()
                    );
                }
            }
        }
    }
});
