//! Tier-by-tier differential coverage for H.264 motion compensation.

use vaco_codec_dsp_mc::h264::{BiWeight, ChromaJob, H264McKernels, UniWeight};
use vaco_simd::{Caps, KernelSet, Tier};

use crate::Kernel;

pub(crate) fn available_tiers() -> Vec<Tier> {
    let caps = Caps::detect();
    [
        Tier::Scalar,
        Tier::Sse2,
        Tier::Sse42,
        Tier::Avx2,
        Tier::Avx512,
        Tier::Neon,
    ]
    .into_iter()
    .filter(|&tier| tier.is_scalar() || caps.capped_at(tier).is_some())
    .collect()
}

#[derive(Debug, Clone)]
pub struct LumaCase {
    tier: Tier,
    src: Box<[[u8; 21]; 21]>,
    width: usize,
    height: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct H264LumaKernel;

impl Kernel for H264LumaKernel {
    const NAME: &'static str = "vaco-codec-dsp-mc::h264_luma_half_raw";
    type Case = LumaCase;
    type Lane = i32;

    fn cases() -> Vec<Self::Case> {
        available_tiers()
            .into_iter()
            .flat_map(|tier| {
                [4usize, 8, 16].into_iter().flat_map(move |width| {
                    [1usize, 4, 16, 21].into_iter().map(move |height| LumaCase {
                        tier,
                        src: Box::new(core::array::from_fn(|y| {
                            core::array::from_fn(|x| ((x * 53 + y * 97 + x * y * 7) & 255) as u8)
                        })),
                        width,
                        height,
                    })
                })
            })
            .collect()
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        luma(case, Tier::Scalar)
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        luma(case, case.tier)
    }
}

fn luma(case: &LumaCase, tier: Tier) -> Vec<i32> {
    let mut out = [[0i32; 16]; 21];
    (H264McKernels::for_tier(tier).luma_half_raw)(&case.src, case.width, case.height, &mut out);
    out.into_iter()
        .take(case.height)
        .flat_map(|row| row.into_iter().take(case.width))
        .collect()
}

#[derive(Debug, Clone)]
pub struct ChromaCase {
    tier: Tier,
    jobs: Vec<ChromaJob>,
}

#[derive(Debug, Clone, Copy)]
pub struct H264ChromaKernel;

impl Kernel for H264ChromaKernel {
    const NAME: &'static str = "vaco-codec-dsp-mc::h264_chroma_batch";
    type Case = ChromaCase;
    type Lane = u8;

    fn cases() -> Vec<Self::Case> {
        available_tiers()
            .into_iter()
            .flat_map(|tier| {
                [1usize, 3, 4, 15, 16, 17, 64]
                    .into_iter()
                    .map(move |len| ChromaCase {
                        tier,
                        jobs: (0..len)
                            .map(|index| ChromaJob {
                                src: core::array::from_fn(|y| {
                                    core::array::from_fn(|x| {
                                        ((index * 31 + x * 47 + y * 73 + x * y * 11) & 255) as u8
                                    })
                                }),
                                frac_x: (index & 7) as u8,
                                frac_y: ((index >> 3) & 7) as u8,
                            })
                            .collect(),
                    })
            })
            .collect()
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        chroma(case, Tier::Scalar)
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        chroma(case, case.tier)
    }
}

fn chroma(case: &ChromaCase, tier: Tier) -> Vec<u8> {
    let mut out = vec![[[0u8; 2]; 2]; case.jobs.len()];
    (H264McKernels::for_tier(tier).chroma_batch)(&case.jobs, &mut out);
    out.into_iter().flatten().flatten().collect()
}

#[derive(Debug, Clone)]
pub struct UniCase {
    tier: Tier,
    src: Vec<u8>,
    stride: usize,
    width: usize,
    height: usize,
    params: UniWeight,
}

#[derive(Debug, Clone, Copy)]
pub struct H264UniWeightKernel;

impl Kernel for H264UniWeightKernel {
    const NAME: &'static str = "vaco-codec-dsp-mc::h264_weight_uni";
    type Case = UniCase;
    type Lane = u8;

    fn cases() -> Vec<Self::Case> {
        let params = [
            UniWeight::IDENTITY,
            UniWeight {
                weight: 15,
                offset: -3,
                log2_denom: 4,
            },
            UniWeight {
                weight: -128,
                offset: 127,
                log2_denom: 7,
            },
        ];
        available_tiers()
            .into_iter()
            .flat_map(|tier| {
                [2usize, 4, 8, 16].into_iter().flat_map(move |width| {
                    params.into_iter().map(move |params| {
                        let height = 4;
                        let stride = width + 3;
                        UniCase {
                            tier,
                            src: (0..stride * height)
                                .map(|i| ((i * 67 + width * 19) & 255) as u8)
                                .collect(),
                            stride,
                            width,
                            height,
                            params,
                        }
                    })
                })
            })
            .collect()
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        uni(case, Tier::Scalar)
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        uni(case, case.tier)
    }
}

fn uni(case: &UniCase, tier: Tier) -> Vec<u8> {
    let mut out = vec![0xA5; case.stride * case.height];
    (H264McKernels::for_tier(tier).weight_uni)(
        &case.src,
        case.stride,
        &mut out,
        case.stride,
        case.width,
        case.height,
        case.params,
    );
    out
}

#[derive(Debug, Clone)]
pub struct BiCase {
    tier: Tier,
    src0: Vec<u8>,
    src1: Vec<u8>,
    stride: usize,
    width: usize,
    height: usize,
    params: BiWeight,
}

#[derive(Debug, Clone, Copy)]
pub struct H264BiWeightKernel;

impl Kernel for H264BiWeightKernel {
    const NAME: &'static str = "vaco-codec-dsp-mc::h264_weight_bi";
    type Case = BiCase;
    type Lane = u8;

    fn cases() -> Vec<Self::Case> {
        let params = [
            BiWeight::AVERAGE,
            BiWeight {
                weight0: 128,
                weight1: 128,
                offset: -128,
                log2_denom: 0,
            },
            BiWeight {
                weight0: 48,
                weight1: 16,
                offset: 0,
                log2_denom: 5,
            },
        ];
        available_tiers()
            .into_iter()
            .flat_map(|tier| {
                [2usize, 4, 8, 16].into_iter().flat_map(move |width| {
                    params.into_iter().map(move |params| {
                        let height = 4;
                        let stride = width + 3;
                        BiCase {
                            tier,
                            src0: (0..stride * height)
                                .map(|i| ((i * 67 + width * 19) & 255) as u8)
                                .collect(),
                            src1: (0..stride * height)
                                .map(|i| ((i * 101 + width * 13) & 255) as u8)
                                .collect(),
                            stride,
                            width,
                            height,
                            params,
                        }
                    })
                })
            })
            .collect()
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        bi(case, Tier::Scalar)
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        bi(case, case.tier)
    }
}

fn bi(case: &BiCase, tier: Tier) -> Vec<u8> {
    let mut out = vec![0x5A; case.stride * case.height];
    (H264McKernels::for_tier(tier).weight_bi)(
        &case.src0,
        case.stride,
        &case.src1,
        case.stride,
        &mut out,
        case.stride,
        case.width,
        case.height,
        case.params,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Differential;

    #[test]
    fn every_available_h264_mc_tier_is_bit_exact() {
        Differential::<H264LumaKernel>::run().assert_clean();
        Differential::<H264ChromaKernel>::run().assert_clean();
        Differential::<H264UniWeightKernel>::run().assert_clean();
        Differential::<H264BiWeightKernel>::run().assert_clean();
    }
}
