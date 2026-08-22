//! Differential measurement against the reference binary (D6, D11).
//!
//! # The rule
//!
//! The reference is an **oracle we query, never a source we read** — plan 13
//! §1.7.2. Every expected value here is produced by running `ffmpeg` at test
//! time and thrown away afterwards; there are no golden files, and nothing
//! reference-derived enters the repository.
//!
//! # Reading the output
//!
//! `cargo test -p vaco-scale --test reference -- --nocapture` prints the
//! fidelity table that `docs/signal/vaco-scale.md` records. Each row reports the
//! number of differing bytes, the maximum absolute difference and the PSNR, so a
//! regression shows up as a number moving rather than as a pass turning into a
//! fail.
//!
//! Absence of `ffmpeg` is a **skip**, never a failure: a contributor without it
//! still runs `cargo test`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::many_single_char_names,
    clippy::float_cmp,
    clippy::integer_division,
    clippy::needless_range_loop,
    clippy::field_reassign_with_default,
    clippy::unreadable_literal,
    clippy::cast_possible_wrap,
    clippy::wildcard_imports,
    clippy::print_stdout,
    clippy::too_many_arguments,
    reason = "a measurement harness whose output is the deliverable"
)]

use std::io::Write as _;
use std::process::{Command, Stdio};

use vaco_pixfmt::PixFmt;
use vaco_scale::exec::{DstPlane, SrcPlane};
use vaco_scale::{ImageSpec, ScaleOptions, Scaler};

/// The reference binary, if one is on `PATH` or named by the environment.
fn reference() -> Option<String> {
    let bin = std::env::var("VACO_REF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_owned());
    let ok = Command::new(&bin)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?
        .success();
    ok.then_some(bin)
}

/// Bytes of one packed (stride == `min_stride`) raw image.
fn raw_len(fmt: PixFmt, w: u32, h: u32) -> usize {
    (0..fmt.plane_count())
        .map(|p| fmt.min_stride(w, p as u8) * fmt.plane_height(h, p as u8) as usize)
        .sum()
}

/// Run the reference over `data`, returning packed raw output.
fn run_reference(
    bin: &str,
    sfmt: PixFmt,
    sw: u32,
    sh: u32,
    data: &[u8],
    vf: &str,
    dfmt: PixFmt,
) -> Option<Vec<u8>> {
    let mut child = Command::new(bin)
        .args(["-hide_banner", "-loglevel", "error", "-f", "rawvideo"])
        .args(["-pix_fmt", sfmt.name()])
        .args(["-s", &format!("{sw}x{sh}")])
        .args(["-i", "pipe:0", "-vf", vf, "-f", "rawvideo"])
        .args(["-pix_fmt", dfmt.name(), "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let owned = data.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&owned));
    let out = child.wait_with_output().ok()?;
    let _ = writer.join();
    out.status.success().then_some(out.stdout)
}

/// Our own conversion, produced as packed raw bytes so it lines up with the
/// reference's output byte for byte.
fn run_ours(
    sfmt: PixFmt,
    sw: u32,
    sh: u32,
    data: &[u8],
    dfmt: PixFmt,
    dw: u32,
    dh: u32,
    opts: &ScaleOptions,
    src_color: vaco_color::ColorInfo,
    dst_color: vaco_color::ColorInfo,
) -> Option<Vec<u8>> {
    let src_spec = ImageSpec::new(sfmt, sw, sh).with_color(src_color);
    let dst_spec = ImageSpec::new(dfmt, dw, dh).with_color(dst_color);
    let mut scaler = Scaler::new(&src_spec, &dst_spec, opts).ok()?;

    let mut src_bufs: Vec<Vec<u8>> = Vec::new();
    let mut at = 0usize;
    for p in 0..sfmt.plane_count() {
        let stride = sfmt.min_stride(sw, p as u8);
        let rows = sfmt.plane_height(sh, p as u8) as usize;
        let n = stride * rows;
        src_bufs.push(data.get(at..at + n)?.to_vec());
        at += n;
    }
    let mut dst_bufs: Vec<Vec<u8>> = (0..dfmt.plane_count())
        .map(|p| vec![0u8; dfmt.min_stride(dw, p as u8) * dfmt.plane_height(dh, p as u8) as usize])
        .collect();

    {
        let srcs: Vec<SrcPlane<'_>> = src_bufs
            .iter()
            .enumerate()
            .map(|(p, d)| SrcPlane {
                data: d,
                stride: sfmt.min_stride(sw, p as u8),
            })
            .collect();
        let mut dsts: Vec<DstPlane<'_>> = dst_bufs
            .iter_mut()
            .enumerate()
            .map(|(p, d)| DstPlane {
                data: d,
                stride: dfmt.min_stride(dw, p as u8),
            })
            .collect();
        scaler.scale_planes(&srcs, &mut dsts).ok()?;
    }
    Some(dst_bufs.concat())
}

/// Comparison result for one case.
#[derive(Debug, Clone, Copy, Default)]
struct Verdict {
    bytes: usize,
    differing: usize,
    max_abs: u32,
    psnr: f64,
    /// Differences where we saturate and the reference emits 0 — its own
    /// out-of-range table overrun, which we deliberately do not reproduce.
    /// See `vaco_scale::REFERENCE_CLIP_DIVERGENCE`.
    clip_bug: usize,
}

impl Verdict {
    /// Differences that are ours to explain, i.e. excluding the reference's own
    /// clipping defect.
    fn real(&self) -> usize {
        self.differing.saturating_sub(self.clip_bug)
    }

    fn grade(&self) -> &'static str {
        if self.real() == 0 {
            "Exact"
        } else if self.max_abs <= 2 {
            "Equivalent"
        } else {
            "Divergent"
        }
    }
}

/// Compare as samples of `depth` bits, so a 16-bit format is not judged by its
/// byte differences.
fn compare(a: &[u8], b: &[u8], depth: u8, big_endian: bool) -> Verdict {
    compare_masked(a, b, depth, big_endian, &|_| true)
}

/// [`compare`], counting only samples `keep` accepts.
fn compare_masked(
    a: &[u8],
    b: &[u8],
    depth: u8,
    big_endian: bool,
    keep: &dyn Fn(usize) -> bool,
) -> Verdict {
    let step = if depth > 8 { 2 } else { 1 };
    let n = a.len().min(b.len()) / step;
    let mut differing = 0usize;
    let mut max_abs = 0u32;
    let mut sse = 0f64;
    let mut clip_bug = 0usize;
    let mut counted = 0usize;
    let peak = f64::from((1u32 << depth.min(16)) - 1);
    let top = (1u32 << depth.min(16)) - 1;
    for i in 0..n {
        if !keep(i) {
            continue;
        }
        counted += 1;
        let (x, y) = if step == 1 {
            (u32::from(a[i]), u32::from(b[i]))
        } else if big_endian {
            (
                (u32::from(a[2 * i]) << 8) | u32::from(a[2 * i + 1]),
                (u32::from(b[2 * i]) << 8) | u32::from(b[2 * i + 1]),
            )
        } else {
            (
                u32::from(a[2 * i]) | (u32::from(a[2 * i + 1]) << 8),
                u32::from(b[2 * i]) | (u32::from(b[2 * i + 1]) << 8),
            )
        };
        if x != y {
            differing += 1;
            if x == top && y == 0 {
                clip_bug += 1;
            } else {
                max_abs = max_abs.max(x.abs_diff(y));
            }
            let d = f64::from(x) - f64::from(y);
            sse += d * d;
        }
    }
    let mse = if counted == 0 {
        0.0
    } else {
        sse / counted as f64
    };
    let psnr = if mse == 0.0 {
        f64::INFINITY
    } else {
        20.0 * peak.log10() - 10.0 * mse.log10()
    };
    Verdict {
        bytes: counted,
        differing,
        max_abs,
        psnr,
        clip_bug,
    }
}

/// A deterministic, structure-rich test image: gradients, edges and noise.
fn test_image(fmt: PixFmt, w: u32, h: u32, seed: u32) -> Vec<u8> {
    let mut out = vec![0u8; raw_len(fmt, w, h)];
    let mut state = seed | 1;
    let depth = fmt.max_depth();
    let wide = depth > 8;
    let mask = if depth >= 16 {
        0xffffu32
    } else {
        (1u32 << depth) - 1
    };
    let mut i = 0usize;
    while i < out.len() {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        // Two thirds structure, one third noise: noise alone hides phase and
        // siting bugs, structure alone hides rounding bugs.
        let structured = ((i * 7) % 256) as u32;
        let raw = if state.is_multiple_of(3) {
            state
        } else {
            structured
        };
        if wide {
            // Values must be legal at the format's own depth, or the comparison
            // measures the two implementations' out-of-range policies instead of
            // their arithmetic.
            let v = ((raw & mask) as u16).to_le_bytes();
            let v = if fmt.is_big_endian() {
                ((raw & mask) as u16).to_be_bytes()
            } else {
                v
            };
            if let (Some(a), Some(b)) = (out.get_mut(i), v.first()) {
                *a = *b;
            }
            if let (Some(a), Some(b)) = (out.get_mut(i + 1), v.get(1)) {
                *a = *b;
            }
            i += 2;
        } else {
            if let Some(a) = out.get_mut(i) {
                *a = raw as u8;
            }
            i += 1;
        }
    }
    out
}

struct Case {
    name: &'static str,
    sfmt: PixFmt,
    dfmt: PixFmt,
    size: (u32, u32, u32, u32),
    filter_opts: &'static str,
    vf_extra: &'static str,
}

const CASES: &[Case] = &[
    Case {
        name: "yuv444p -> rgb24, bt709 tv->pc",
        sfmt: PixFmt::Yuv444p,
        dfmt: PixFmt::Rgb24,
        size: (128, 96, 128, 96),
        filter_opts: "",
        vf_extra: ":in_range=tv:out_range=pc:in_color_matrix=bt709",
    },
    Case {
        name: "rgb24 -> yuv444p, bt709 pc->tv",
        sfmt: PixFmt::Rgb24,
        dfmt: PixFmt::Yuv444p,
        size: (128, 96, 128, 96),
        filter_opts: "",
        vf_extra: ":in_range=pc:out_range=tv:out_color_matrix=bt709",
    },
    Case {
        name: "yuv420p -> rgb24, bt709 tv->pc",
        sfmt: PixFmt::Yuv420p,
        dfmt: PixFmt::Rgb24,
        size: (128, 96, 128, 96),
        filter_opts: "",
        vf_extra: ":in_range=tv:out_range=pc:in_color_matrix=bt709",
    },
    Case {
        name: "rgb24 -> yuv420p, bt709 pc->tv",
        sfmt: PixFmt::Rgb24,
        dfmt: PixFmt::Yuv420p,
        size: (128, 96, 128, 96),
        filter_opts: "",
        vf_extra: ":in_range=pc:out_range=tv:out_color_matrix=bt709",
    },
    Case {
        name: "rgb24 -> bgr24 (permutation)",
        sfmt: PixFmt::Rgb24,
        dfmt: PixFmt::Bgr24,
        size: (128, 96, 128, 96),
        filter_opts: "",
        vf_extra: "",
    },
    Case {
        name: "yuv420p -> nv12 (repack)",
        sfmt: PixFmt::Yuv420p,
        dfmt: PixFmt::Nv12,
        size: (128, 96, 128, 96),
        filter_opts: "",
        vf_extra: "",
    },
    Case {
        name: "rgba -> bgra (permutation)",
        sfmt: PixFmt::Rgba,
        dfmt: PixFmt::Bgra,
        size: (128, 96, 128, 96),
        filter_opts: "",
        vf_extra: "",
    },
    Case {
        name: "yuv420p -> yuv422p (chroma upsample)",
        sfmt: PixFmt::Yuv420p,
        dfmt: PixFmt::Yuv422p,
        size: (128, 96, 128, 96),
        filter_opts: "",
        vf_extra: "",
    },
    Case {
        name: "yuv444p -> yuv420p (chroma downsample)",
        sfmt: PixFmt::Yuv444p,
        dfmt: PixFmt::Yuv420p,
        size: (128, 96, 128, 96),
        filter_opts: "",
        vf_extra: "",
    },
    Case {
        name: "yuv420p 2x down, bilinear",
        sfmt: PixFmt::Yuv420p,
        dfmt: PixFmt::Yuv420p,
        size: (128, 96, 64, 48),
        filter_opts: "scaler=bilinear",
        vf_extra: ":flags=bilinear",
    },
    Case {
        name: "yuv420p 2x up, bilinear",
        sfmt: PixFmt::Yuv420p,
        dfmt: PixFmt::Yuv420p,
        size: (64, 48, 128, 96),
        filter_opts: "scaler=bilinear",
        vf_extra: ":flags=bilinear",
    },
    Case {
        name: "yuv420p 3:2 down, bicubic",
        sfmt: PixFmt::Yuv420p,
        dfmt: PixFmt::Yuv420p,
        size: (192, 144, 128, 96),
        filter_opts: "",
        vf_extra: ":flags=bicubic",
    },
    Case {
        name: "yuv420p 2x up, bicubic",
        sfmt: PixFmt::Yuv420p,
        dfmt: PixFmt::Yuv420p,
        size: (64, 48, 128, 96),
        filter_opts: "",
        vf_extra: ":flags=bicubic",
    },
    Case {
        name: "yuv420p 4x down, lanczos",
        sfmt: PixFmt::Yuv420p,
        dfmt: PixFmt::Yuv420p,
        size: (256, 192, 64, 48),
        filter_opts: "scaler=lanczos",
        vf_extra: ":flags=lanczos",
    },
    Case {
        name: "yuv420p 2x down, area",
        sfmt: PixFmt::Yuv420p,
        dfmt: PixFmt::Yuv420p,
        size: (128, 96, 64, 48),
        filter_opts: "scaler=area",
        vf_extra: ":flags=area",
    },
    Case {
        name: "yuv420p10le -> yuv420p, dither off",
        sfmt: PixFmt::Yuv420p10le,
        dfmt: PixFmt::Yuv420p,
        size: (128, 96, 128, 96),
        filter_opts: "sws_dither=none",
        vf_extra: ":sws_dither=none",
    },
    Case {
        name: "yuv420p -> yuv420p10le (widen)",
        sfmt: PixFmt::Yuv420p,
        dfmt: PixFmt::Yuv420p10le,
        size: (128, 96, 128, 96),
        filter_opts: "",
        vf_extra: "",
    },
    Case {
        name: "gray -> rgb24",
        sfmt: PixFmt::Gray8,
        dfmt: PixFmt::Rgb24,
        size: (128, 96, 128, 96),
        filter_opts: "",
        vf_extra: ":in_range=tv:out_range=pc",
    },
    Case {
        name: "yuv420p -> rgb24, 2x down, bicubic",
        sfmt: PixFmt::Yuv420p,
        dfmt: PixFmt::Rgb24,
        size: (128, 96, 64, 48),
        filter_opts: "",
        vf_extra: ":in_range=tv:out_range=pc:in_color_matrix=bt709:flags=bicubic",
    },
    Case {
        name: "yuv420p -> rgba",
        sfmt: PixFmt::Yuv420p,
        dfmt: PixFmt::Rgba,
        size: (128, 96, 128, 96),
        filter_opts: "",
        vf_extra: ":in_range=tv:out_range=pc:in_color_matrix=bt709",
    },
];

/// Compare only samples at least four away from any edge of plane 0.
fn interior_verdict(case: &Case, dw: u32, dh: u32, got: &[u8], want: &[u8], depth: u8) -> Verdict {
    let step = if depth > 8 { 2usize } else { 1 };
    let stride = case.dfmt.min_stride(dw, 0) / step;
    let rows = case.dfmt.plane_height(dh, 0) as usize;
    let margin = 4usize;
    if stride <= 2 * margin || rows <= 2 * margin {
        return Verdict::default();
    }
    let keep = move |i: usize| {
        if i >= stride * rows {
            return false;
        }
        let (y, x) = (i / stride, i % stride);
        x >= margin && x + margin < stride && y >= margin && y + margin < rows
    };
    compare_masked(got, want, depth, case.dfmt.is_big_endian(), &keep)
}

fn colors_for(extra: &str, dst: bool) -> vaco_color::ColorInfo {
    use vaco_color::{ColorInfo, ColorRange, MatrixCoefficients};
    let mut c = ColorInfo::default();
    let key_range = if dst { "out_range=" } else { "in_range=" };
    let key_matrix = if dst {
        "out_color_matrix="
    } else {
        "in_color_matrix="
    };
    if let Some(rest) = extra.split(key_range).nth(1) {
        let v = rest.split(':').next().unwrap_or("");
        c.range = ColorRange::from_name(v).unwrap_or_default();
    }
    if let Some(rest) = extra.split(key_matrix).nth(1) {
        let v = rest.split(':').next().unwrap_or("");
        c.matrix = MatrixCoefficients::from_name(v).unwrap_or_default();
    }
    // A conversion that names only one side's matrix means both sides speak it.
    if c.matrix == MatrixCoefficients::Unspecified {
        for key in ["in_color_matrix=", "out_color_matrix="] {
            if let Some(rest) = extra.split(key).nth(1) {
                let v = rest.split(':').next().unwrap_or("");
                if let Some(m) = MatrixCoefficients::from_name(v) {
                    c.matrix = m;
                }
            }
        }
    }
    c
}

#[test]
fn fidelity_against_the_reference() {
    let Some(bin) = reference() else {
        println!("SKIP: no reference ffmpeg on PATH or in VACO_REF_FFMPEG");
        return;
    };

    println!();
    println!(
        "| {:<44} | {:>10} | {:>7} | {:>8} | {:>8} | {:<10} |",
        "conversion", "differing", "max err", "PSNR dB", "interior", "grade"
    );
    println!(
        "|{:-<46}|{:-<12}|{:-<9}|{:-<10}|{:-<10}|{:-<12}|",
        "", "", "", "", "", ""
    );

    let mut worst = Vec::new();
    for case in CASES {
        let (sw, sh, dw, dh) = case.size;
        let data = test_image(case.sfmt, sw, sh, 0x1234_5678);
        let vf = format!("scale={dw}:{dh}{}", case.vf_extra);
        let Some(want) = run_reference(&bin, case.sfmt, sw, sh, &data, &vf, case.dfmt) else {
            println!("| {:<44} | {:>10} |", case.name, "ref failed");
            continue;
        };
        let mut opts = ScaleOptions::default();
        if !case.filter_opts.is_empty() {
            use vaco_opts::OptionsExt as _;
            opts.set_from_string(case.filter_opts, "=", ":")
                .expect("options parse");
        }
        let src_color = colors_for(case.vf_extra, false);
        let dst_color = colors_for(case.vf_extra, true);
        let Some(got) = run_ours(
            case.sfmt, sw, sh, &data, case.dfmt, dw, dh, &opts, src_color, dst_color,
        ) else {
            println!("| {:<44} | {:>10} |", case.name, "ours failed");
            continue;
        };
        assert_eq!(
            got.len(),
            want.len(),
            "{}: output size disagrees with the reference",
            case.name
        );
        let depth = case.dfmt.max_depth();
        let v = compare(&got, &want, depth, case.dfmt.is_big_endian());
        // The interior view excludes a four-sample border, which is where a
        // filter's edge rule shows and nothing else does. Separating the two
        // turns "6% of samples differ" into the far more useful "the interior is
        // identical and the edge rule is not ours".
        let interior = interior_verdict(case, dw, dh, &got, &want, depth);
        println!(
            "| {:<44} | {:>10} | {:>7} | {:>8} | {:>8} | {:<10} |",
            case.name,
            format!("{}/{}", v.real(), v.bytes),
            v.max_abs,
            if v.psnr.is_infinite() {
                "inf".to_owned()
            } else {
                format!("{:.1}", v.psnr)
            },
            format!("{}/{}", interior.real(), interior.max_abs),
            v.grade()
        );
        if v.grade() == "Divergent" {
            worst.push((case.name, v));
        }
    }
    println!();
    for (name, v) in &worst {
        println!("divergent: {name} max {} psnr {:.1}", v.max_abs, v.psnr);
    }
}

/// The conversions this crate commits to being byte-identical on.
///
/// Separate from the table above so that a regression in a graded-Exact path is
/// a **failure**, not a number that moved. Everything here is measured, not
/// asserted on faith — see `docs/signal/vaco-scale.md`.
#[test]
fn paths_graded_exact_stay_exact() {
    let Some(bin) = reference() else {
        println!("SKIP: no reference ffmpeg");
        return;
    };
    let exact: &[(&str, PixFmt, PixFmt, &str)] = &[
        (
            "yuv444p->rgb24 tv->pc bt709",
            PixFmt::Yuv444p,
            PixFmt::Rgb24,
            ":in_range=tv:out_range=pc:in_color_matrix=bt709",
        ),
        (
            "yuv444p->rgb24 tv->pc bt470bg",
            PixFmt::Yuv444p,
            PixFmt::Rgb24,
            ":in_range=tv:out_range=pc:in_color_matrix=bt470bg",
        ),
        (
            "rgb24->yuv444p pc->tv bt709",
            PixFmt::Rgb24,
            PixFmt::Yuv444p,
            ":in_range=pc:out_range=tv:out_color_matrix=bt709",
        ),
        ("rgb24->bgr24", PixFmt::Rgb24, PixFmt::Bgr24, ""),
        ("rgba->argb", PixFmt::Rgba, PixFmt::Argb, ""),
        ("yuv420p->nv12", PixFmt::Yuv420p, PixFmt::Nv12, ""),
        ("yuv422p->yuyv422", PixFmt::Yuv422p, PixFmt::Yuyv422, ""),
        ("gray->gray16le", PixFmt::Gray8, PixFmt::Gray16le, ""),
    ];
    let (w, h) = (96u32, 64u32);
    for (name, sfmt, dfmt, extra) in exact {
        let data = test_image(*sfmt, w, h, 0x9e37_79b9);
        let vf = format!("scale={w}:{h}{extra}");
        let Some(want) = run_reference(&bin, *sfmt, w, h, &data, &vf, *dfmt) else {
            panic!("{name}: reference run failed");
        };
        let got = run_ours(
            *sfmt,
            w,
            h,
            &data,
            *dfmt,
            w,
            h,
            &ScaleOptions::default(),
            colors_for(extra, false),
            colors_for(extra, true),
        )
        .expect("our conversion");
        let v = compare(&got, &want, dfmt.max_depth(), dfmt.is_big_endian());
        assert_eq!(
            v.real(),
            0,
            "{name} is graded Exact but {} of {} samples differ (max {}, \
             {} of them the reference's own clipping defect)",
            v.real(),
            v.bytes,
            v.max_abs,
            v.clip_bug
        );
    }
}
