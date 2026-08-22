//! What a CABAC bin costs, and which inner-loop shape is actually fastest.
//!
//! CABAC decoding is inherently serial — each bin's arithmetic depends on the
//! previous one's — so there is no vectorisation to be had. Everything is in the
//! scalar inner loop, and two independent choices define it:
//!
//! | Axis | Options |
//! |---|---|
//! | the MPS/LPS decision | **branchy** (as clause 9.3.3.2.1 writes it) or **branchless** (masked select, `valMPS` flip folded into the table) |
//! | renormalisation | **per-bit** (as `RenormD` writes it) or **whole-width** (`leading_zeros`, one `BitReader::get(n)`) |
//!
//! All four combinations are benchmarked, all four written in this file so a
//! crate boundary or an inlining decision cannot be mistaken for an algorithmic
//! difference, and all four verified bin-for-bin against the shipped engine
//! before any timing is taken.
//!
//! # Both corpora matter
//!
//! A branch predictor's fortunes depend entirely on how skewed the stream is,
//! and real CABAC streams are skewed — that is why the coder exists. So the
//! measurement is taken twice:
//!
//! - **`skewed`** — ~6% ones, which is what a well-adapted context produces and
//!   what most of a real slice looks like.
//! - **`even`** — ~50% ones, the near-incompressible case: high-entropy residual
//!   data, and the case a branch predictor cannot help with.
//!
//! A shape that wins on one and loses badly on the other is the wrong choice
//! whatever its headline number.
//!
//! Run with `cargo bench -p vaco-codec-cabac`.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::inline_always,
    clippy::cast_possible_wrap,
    clippy::integer_division,
    missing_debug_implementations,
    unreachable_pub,
    reason = "benchmark code: the candidate engines exist only in this file"
)]

use std::sync::LazyLock;

use divan::counter::ItemsCount;
use vaco_bitstream::BitReader;
use vaco_codec_cabac::{
    CabacDecoder, CabacEncoder, ContextInit, ContextModel, init_contexts,
    tables::{LPS_RANGE, RANGE_TAB_LPS, TRANS, TRANS_IDX_LPS, TRANS_IDX_MPS},
};

fn main() {
    verify();
    divan::main();
}

/// Bins per measured run.
const N: usize = 8192;
/// Contexts in the working set — roughly what an H.264 macroblock touches.
const CTX: usize = 64;

struct Corpus {
    bytes: Vec<u8>,
    bins: Vec<u32>,
    ctx_idx: Vec<usize>,
    inits: Vec<ContextModel>,
}

/// Build a corpus with a given probability of a one bin, encoded by the real
/// encoder so the decoder walks genuinely adapted states.
fn corpus(seed: u64, one_in: u64) -> Corpus {
    let mut state = seed;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    let inits: Vec<ContextModel> = (0..CTX)
        .map(|i| {
            ContextModel::init_h264(
                (i16::try_from(i).unwrap_or(0) % 60) - 20,
                (i16::try_from(i).unwrap_or(0) % 90) - 30,
                28,
            )
        })
        .collect();

    let mut bins = Vec::new();
    let mut ctx_idx = Vec::new();
    for _ in 0..N {
        let r = next();
        ctx_idx.push((r >> 3) as usize % CTX);
        bins.push(u32::from((r % one_in) == 0));
    }

    let mut ctxs = inits.clone();
    let mut enc = CabacEncoder::new();
    for (i, &b) in ctx_idx.iter().zip(bins.iter()) {
        enc.encode_decision(&mut ctxs[*i], b);
    }
    enc.encode_terminate(1);
    let mut bytes = enc.finish();
    bytes.resize(bytes.len() + 64, 0);

    Corpus {
        bytes,
        bins,
        ctx_idx,
        inits,
    }
}

/// ~6% ones: a well-adapted context, which is most of a real slice.
static SKEWED: LazyLock<Corpus> = LazyLock::new(|| corpus(0x2545_F491_4F6C_DD1D, 16));
/// ~50% ones: high-entropy residual, where a predictor cannot help.
static EVEN: LazyLock<Corpus> = LazyLock::new(|| corpus(0x9E37_79B9_7F4A_7C15, 2));

fn pick(which: &str) -> &'static Corpus {
    if which == "skewed" { &SKEWED } else { &EVEN }
}

/// A bypass-only stream, for the bypass benchmarks.
static BYPASS: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let mut enc = CabacEncoder::new();
    let mut state = 0x1234_5678_9ABC_DEF0u64;
    for _ in 0..N {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        enc.encode_bypass((state & 1) as u32);
    }
    enc.encode_terminate(1);
    let mut v = enc.finish();
    v.resize(v.len() + 64, 0);
    v
});

// -------------------------------------------------------------- the candidates

/// The four candidate inner loops, over the same state the shipped engine keeps.
struct Engine<'a> {
    range: u32,
    offset: u32,
    reader: BitReader<'a>,
}

impl<'a> Engine<'a> {
    fn new(data: &'a [u8]) -> Self {
        let mut reader = BitReader::new(data);
        let offset = reader.get(9);
        Self {
            range: 510,
            offset: offset.min(509),
            reader,
        }
    }

    /// Whole-width renormalisation: one `leading_zeros`, one shift, one
    /// `BitReader::get(n)` with a *variable* width.
    #[inline(always)]
    fn renorm_wide(&mut self) {
        let n = self.range.leading_zeros().wrapping_sub(23) & 7;
        self.range <<= n;
        self.offset = (self.offset << n) | self.reader.get(n);
    }

    /// `RenormD` as clause 9.3.3.2.2 writes it: one bit at a time. Each read has
    /// a *constant* width, which is the part that turns out to matter.
    #[inline(always)]
    fn renorm_bitwise(&mut self) {
        while self.range < 256 {
            self.range <<= 1;
            self.offset = (self.offset << 1) | self.reader.get_bit();
        }
    }

    /// The specification's decision: two state variables, an MPS/LPS branch, and
    /// the `pStateIdx == 0` `valMPS` flip inside the LPS arm.
    #[inline(always)]
    fn decide_branchy(&mut self, ctx: &mut ContextModel) -> u32 {
        let mut p = usize::from(ctx.state_idx());
        let mut mps = u32::from(ctx.mps());
        let q = ((self.range >> 6) & 3) as usize;
        let lps = u32::from(RANGE_TAB_LPS[p][q]);
        self.range -= lps;
        let bin;
        if self.offset >= self.range {
            bin = 1 - mps;
            self.offset -= self.range;
            self.range = lps;
            if p == 0 {
                mps = 1 - mps;
            }
            p = usize::from(TRANS_IDX_LPS[p]);
        } else {
            bin = mps;
            p = usize::from(TRANS_IDX_MPS[p]);
        }
        *ctx = ContextModel::new(p as u8, mps == 1);
        bin
    }

    /// The packed-state decision: masked selects, no branch on the outcome, the
    /// `valMPS` flip folded into the transition table.
    #[inline(always)]
    fn decide_branchless(&mut self, ctx: &mut ContextModel) -> u32 {
        let state = usize::from(ctx.packed());
        let q = ((self.range >> 6) & 3) as usize;
        let lps_range = u32::from(LPS_RANGE[(state >> 1) * 4 + q]);
        self.range -= lps_range;
        let lps = u32::from(self.offset >= self.range);
        let mask = 0u32.wrapping_sub(lps);
        self.offset -= self.range & mask;
        self.range = (lps_range & mask) | (self.range & !mask);
        *ctx = ContextModel::from_packed(TRANS[((lps as usize) << 8) | state]);
        (state as u32 & 1) ^ lps
    }

    #[inline(always)]
    fn branchy_wide(&mut self, ctx: &mut ContextModel) -> u32 {
        let b = self.decide_branchy(ctx);
        self.renorm_wide();
        b
    }
    #[inline(always)]
    fn branchy_bitwise(&mut self, ctx: &mut ContextModel) -> u32 {
        let b = self.decide_branchy(ctx);
        self.renorm_bitwise();
        b
    }
    #[inline(always)]
    fn branchless_wide(&mut self, ctx: &mut ContextModel) -> u32 {
        let b = self.decide_branchless(ctx);
        self.renorm_wide();
        b
    }
    #[inline(always)]
    fn branchless_bitwise(&mut self, ctx: &mut ContextModel) -> u32 {
        let b = self.decide_branchless(ctx);
        self.renorm_bitwise();
        b
    }
}

/// Every candidate must decode the corpus identically to the shipped engine, or
/// the timings compare different computations.
fn verify() {
    for c in [&*SKEWED, &*EVEN] {
        let mut ctxs = c.inits.clone();
        let mut d = CabacDecoder::new(&c.bytes);
        for (i, &want) in c.ctx_idx.iter().zip(c.bins.iter()) {
            assert_eq!(d.decode_decision(&mut ctxs[*i]), want, "shipped engine");
        }
        assert_eq!(d.decode_terminate(), 1);

        macro_rules! check {
            ($m:ident) => {{
                let mut ctxs = c.inits.clone();
                let mut e = Engine::new(&c.bytes);
                for (i, &want) in c.ctx_idx.iter().zip(c.bins.iter()) {
                    assert_eq!(
                        e.$m(&mut ctxs[*i]),
                        want,
                        concat!(stringify!($m), " diverged")
                    );
                }
            }};
        }
        check!(branchy_wide);
        check!(branchy_bitwise);
        check!(branchless_wide);
        check!(branchless_bitwise);
    }
}

// ------------------------------------------------------------------ the runs

macro_rules! candidate {
    ($name:ident, $method:ident) => {
        #[divan::bench(args = ["skewed", "even"])]
        fn $name(bencher: divan::Bencher<'_, '_>, which: &str) {
            let c = pick(which);
            bencher.counter(ItemsCount::new(N)).bench_local(|| {
                let mut ctxs = c.inits.clone();
                let mut e = Engine::new(&c.bytes);
                let mut acc = 0u32;
                for &i in &c.ctx_idx {
                    acc = acc.wrapping_add(e.$method(&mut ctxs[i]));
                }
                acc
            });
        }
    };
}

candidate!(a_branchy_wide, branchy_wide);
candidate!(b_branchy_bitwise, branchy_bitwise);
candidate!(c_branchless_wide, branchless_wide);
candidate!(d_branchless_bitwise, branchless_bitwise);

/// The engine as shipped. Must land on whichever candidate above it is written
/// as; if it does not, something other than the algorithm is being measured.
#[divan::bench(args = ["skewed", "even"])]
fn e_shipped(bencher: divan::Bencher<'_, '_>, which: &str) {
    let c = pick(which);
    bencher.counter(ItemsCount::new(N)).bench_local(|| {
        let mut ctxs = c.inits.clone();
        let mut d = CabacDecoder::new(&c.bytes);
        let mut acc = 0u32;
        for &i in &c.ctx_idx {
            acc = acc.wrapping_add(d.decode_decision(&mut ctxs[i]));
        }
        acc
    });
}

// -------------------------------------------------------------------- bypass

#[divan::bench]
fn bypass_one_at_a_time(bencher: divan::Bencher<'_, '_>) {
    let data = &*BYPASS;
    bencher.counter(ItemsCount::new(N)).bench_local(|| {
        let mut d = CabacDecoder::new(data);
        let mut acc = 0u32;
        for _ in 0..N {
            acc = acc.wrapping_add(d.decode_bypass());
        }
        acc
    });
}

/// The same bins, eight at a time — one `BitReader::get(8)` instead of eight
/// `get_bit`s, with the comparison chain unchanged.
#[divan::bench]
fn bypass_eight_at_a_time(bencher: divan::Bencher<'_, '_>) {
    let data = &*BYPASS;
    bencher.counter(ItemsCount::new(N)).bench_local(|| {
        let mut d = CabacDecoder::new(data);
        let mut acc = 0u32;
        for _ in 0..(N / 8) {
            acc = acc.wrapping_add(d.decode_bypass_bits(8));
        }
        acc
    });
}

#[divan::bench]
fn terminate(bencher: divan::Bencher<'_, '_>) {
    let c = &*SKEWED;
    bencher.counter(ItemsCount::new(N)).bench_local(|| {
        let mut d = CabacDecoder::new(&c.bytes);
        let mut acc = 0u32;
        for _ in 0..N {
            acc = acc.wrapping_add(d.decode_terminate());
        }
        acc
    });
}

#[divan::bench]
fn encode_decision(bencher: divan::Bencher<'_, '_>) {
    let c = &*SKEWED;
    bencher.counter(ItemsCount::new(N)).bench_local(|| {
        let mut ctxs = c.inits.clone();
        let mut e = CabacEncoder::new();
        for (i, &b) in c.ctx_idx.iter().zip(c.bins.iter()) {
            e.encode_decision(&mut ctxs[*i], b);
        }
        e.finish()
    });
}

/// Context-set initialisation: 1024 contexts is roughly an H.264 slice's worth,
/// and it happens once per slice, so it is worth knowing it is not accidentally
/// expensive.
#[divan::bench]
fn init_1024_contexts(bencher: divan::Bencher<'_, '_>) {
    let inits: Vec<ContextInit> = (0..1024)
        .map(|i| ContextInit::new((i as i16 % 60) - 20, (i as i16 % 90) - 30))
        .collect();
    let mut dst = vec![ContextModel::UNINITIALISED; 1024];
    bencher
        .counter(ItemsCount::new(1024usize))
        .bench_local(|| init_contexts(&mut dst, divan::black_box(&inits), 28));
}
