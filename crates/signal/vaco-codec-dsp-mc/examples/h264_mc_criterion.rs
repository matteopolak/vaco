//! Single-kernel worker for `scripts/perf-h264-mc.py`.

use std::hint::black_box;
use std::time::Instant;

use vaco_codec_dsp_mc::fir::{self, TapSet, taps};
use vaco_codec_dsp_mc::h264::{BiWeight, ChromaJob, H264McKernels, UniWeight};
use vaco_simd::{Caps, KernelSet, Tier};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Variant {
    Scalar,
    Neon,
}

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let kernel = args.next().ok_or("missing kernel")?;
    let variant = match args.next().as_deref() {
        Some("scalar") => Variant::Scalar,
        Some("neon") => Variant::Neon,
        _ => return Err("variant must be scalar or neon".into()),
    };
    let iterations = args
        .next()
        .ok_or("missing iteration count")?
        .parse::<usize>()
        .map_err(|error| format!("invalid iteration count: {error}"))?;
    if args.next().is_some() || iterations == 0 {
        return Err("usage: h264_mc_criterion <kernel> <scalar|neon> <iterations>".into());
    }
    if Caps::detect().capped_at(Tier::Neon).is_none() {
        return Err("NEON is not available on this CPU".into());
    }

    let (elapsed_ns, checksum) = match kernel.as_str() {
        "fir-bilinear" => run_fir(&taps::BILINEAR, variant, iterations),
        "fir-h264" => run_fir(&taps::H264_LUMA_HALFPEL, variant, iterations),
        "h264-luma" => run_luma(variant, iterations),
        "h264-chroma" => run_chroma(variant, iterations),
        "h264-uni" => run_uni(variant, iterations),
        "h264-bi" => run_bi(variant, iterations),
        _ => return Err(format!("unknown kernel: {kernel}")),
    }?;

    println!(
        "{{\"kernel\":\"{kernel}\",\"variant\":\"{}\",\"iterations\":{iterations},\"elapsed_ns\":{elapsed_ns},\"checksum\":{checksum}}}",
        if variant == Variant::Scalar {
            "scalar"
        } else {
            "neon"
        }
    );
    Ok(())
}

fn run_fir<const N: usize>(
    taps: &TapSet<N>,
    variant: Variant,
    iterations: usize,
) -> Result<(u128, u64), String> {
    let src: Vec<u8> = (0..1920 + N - 1)
        .map(|i| ((i * 53 + i * i * 7) & 255) as u8)
        .collect();
    let mut scalar = vec![0u8; 1920];
    fir::fir_row_scalar_into(&src, taps, &mut scalar);
    let mut out = vec![0u8; 1920];
    let caps = Caps::detect()
        .capped_at(Tier::Neon)
        .ok_or("NEON disappeared during FIR setup")?;
    if variant == Variant::Neon {
        fir::fir_row(caps, &src, taps, &mut out);
        exact("FIR", &scalar, &out)?;
    }

    let start = Instant::now();
    for _ in 0..iterations {
        if variant == Variant::Scalar {
            fir::fir_row_scalar_into(black_box(&src), taps, black_box(&mut out));
        } else {
            fir::fir_row(caps, black_box(&src), taps, black_box(&mut out));
        }
        black_box(&out);
    }
    Ok((start.elapsed().as_nanos(), checksum_u8(&out)))
}

fn luma_source() -> [[u8; 21]; 21] {
    core::array::from_fn(|y| core::array::from_fn(|x| ((x * 53 + y * 97 + x * y * 7) & 255) as u8))
}

fn run_luma(variant: Variant, iterations: usize) -> Result<(u128, u64), String> {
    let src = luma_source();
    let scalar_kernel = H264McKernels::for_tier(Tier::Scalar).luma_half_raw;
    let neon_kernel = H264McKernels::for_tier(Tier::Neon).luma_half_raw;
    let mut scalar = [[0i32; 16]; 21];
    scalar_kernel(&src, 16, 21, &mut scalar);
    let mut out = [[0i32; 16]; 21];
    if variant == Variant::Neon {
        neon_kernel(&src, 16, 21, &mut out);
        exact("luma", scalar.as_flattened(), out.as_flattened())?;
    }
    let kernel = if variant == Variant::Scalar {
        scalar_kernel
    } else {
        neon_kernel
    };
    let start = Instant::now();
    for _ in 0..iterations {
        kernel(black_box(&src), 16, 21, black_box(&mut out));
        black_box(&out);
    }
    Ok((start.elapsed().as_nanos(), checksum_i32(out.as_flattened())))
}

fn chroma_jobs() -> [ChromaJob; 64] {
    core::array::from_fn(|index| ChromaJob {
        src: core::array::from_fn(|y| {
            core::array::from_fn(|x| ((index * 31 + x * 47 + y * 73 + x * y * 11) & 255) as u8)
        }),
        frac_x: (index & 7) as u8,
        frac_y: ((index >> 3) & 7) as u8,
    })
}

fn run_chroma(variant: Variant, iterations: usize) -> Result<(u128, u64), String> {
    let jobs = chroma_jobs();
    let scalar_kernel = H264McKernels::for_tier(Tier::Scalar).chroma_batch;
    let neon_kernel = H264McKernels::for_tier(Tier::Neon).chroma_batch;
    let mut scalar = [[[0u8; 2]; 2]; 64];
    scalar_kernel(&jobs, &mut scalar);
    let mut out = [[[0u8; 2]; 2]; 64];
    if variant == Variant::Neon {
        neon_kernel(&jobs, &mut out);
        exact(
            "chroma",
            scalar.as_flattened().as_flattened(),
            out.as_flattened().as_flattened(),
        )?;
    }
    let kernel = if variant == Variant::Scalar {
        scalar_kernel
    } else {
        neon_kernel
    };
    let start = Instant::now();
    for _ in 0..iterations {
        kernel(black_box(&jobs), black_box(&mut out));
        black_box(&out);
    }
    Ok((
        start.elapsed().as_nanos(),
        checksum_u8(out.as_flattened().as_flattened()),
    ))
}

fn prediction(seed: usize) -> [u8; 4096] {
    core::array::from_fn(|i| ((i * 67 + seed * 101 + i * seed * 3) & 255) as u8)
}

fn run_uni(variant: Variant, iterations: usize) -> Result<(u128, u64), String> {
    let src = prediction(3);
    let params = UniWeight {
        weight: 15,
        offset: -3,
        log2_denom: 4,
    };
    let scalar_kernel = H264McKernels::for_tier(Tier::Scalar).weight_uni;
    let neon_kernel = H264McKernels::for_tier(Tier::Neon).weight_uni;
    let mut scalar = [0u8; 4096];
    scalar_kernel(&src, 4096, &mut scalar, 4096, 4096, 1, params);
    let mut out = [0u8; 4096];
    if variant == Variant::Neon {
        neon_kernel(&src, 4096, &mut out, 4096, 4096, 1, params);
        exact("uni", &scalar, &out)?;
    }
    let kernel = if variant == Variant::Scalar {
        scalar_kernel
    } else {
        neon_kernel
    };
    let start = Instant::now();
    for _ in 0..iterations {
        kernel(
            black_box(&src),
            4096,
            black_box(&mut out),
            4096,
            4096,
            1,
            params,
        );
        black_box(&out);
    }
    Ok((start.elapsed().as_nanos(), checksum_u8(&out)))
}

fn run_bi(variant: Variant, iterations: usize) -> Result<(u128, u64), String> {
    let src0 = prediction(3);
    let src1 = prediction(11);
    let params = BiWeight {
        weight0: 48,
        weight1: 16,
        offset: 0,
        log2_denom: 5,
    };
    let scalar_kernel = H264McKernels::for_tier(Tier::Scalar).weight_bi;
    let neon_kernel = H264McKernels::for_tier(Tier::Neon).weight_bi;
    let mut scalar = [0u8; 4096];
    scalar_kernel(&src0, 4096, &src1, 4096, &mut scalar, 4096, 4096, 1, params);
    let mut out = [0u8; 4096];
    if variant == Variant::Neon {
        neon_kernel(&src0, 4096, &src1, 4096, &mut out, 4096, 4096, 1, params);
        exact("bi", &scalar, &out)?;
    }
    let kernel = if variant == Variant::Scalar {
        scalar_kernel
    } else {
        neon_kernel
    };
    let start = Instant::now();
    for _ in 0..iterations {
        kernel(
            black_box(&src0),
            4096,
            black_box(&src1),
            4096,
            black_box(&mut out),
            4096,
            4096,
            1,
            params,
        );
        black_box(&out);
    }
    Ok((start.elapsed().as_nanos(), checksum_u8(&out)))
}

fn exact<T: PartialEq>(name: &str, scalar: &[T], neon: &[T]) -> Result<(), String> {
    if scalar == neon {
        Ok(())
    } else {
        Err(format!("{name} scalar and NEON outputs differ"))
    }
}

fn checksum_u8(values: &[u8]) -> u64 {
    values.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &value| {
        (hash ^ u64::from(value)).wrapping_mul(0x100_0000_01b3)
    })
}

fn checksum_i32(values: &[i32]) -> u64 {
    values.iter().fold(0xcbf2_9ce4_8422_2325, |hash, value| {
        value.to_le_bytes().into_iter().fold(hash, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
        })
    })
}
