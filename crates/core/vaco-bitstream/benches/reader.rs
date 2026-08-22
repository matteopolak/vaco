//! What the safe reader costs, measured rather than assumed.
//!
//! `planning/11-foundations.md` §8.3 predicts 1-3% on header-parsing workloads
//! and well under 1% on decode, against C's unchecked over-read. We cannot
//! benchmark against C, so we benchmark against the safe alternatives we
//! actually had a choice between:
//!
//! | Variant | What it isolates |
//! |---|---|
//! | `padded` | the F9 design: 64-byte padding, so the tail path is never reached |
//! | `unpadded` | the same reader on a bare slice: the tail path runs at the end |
//! | `result_per_read` | F13's rejected option (a): identical cache, `Result` on every read |
//! | `checked_per_read` | no cache at all: one bounds-checked slice access per syntax element |
//! | `bytewise` | the textbook-safe reader: one bounds check per *bit* |
//!
//! Run with `cargo bench -p vaco-bitstream`.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::many_single_char_names,
    clippy::single_match_else,
    unreachable_pub,
    missing_debug_implementations,
    reason = "benchmark code: the baseline readers exist only in this file"
)]

use std::sync::LazyLock;

use divan::counter::BytesCount;
use vaco_bitstream::{BitReader, BitWriter, GolombRead, Padded, annexb};

fn main() {
    verify();
    divan::main();
}

/// The syntax the header benchmarks parse, expressed once per reader flavour.
macro_rules! parse_headers {
    ($r:expr, $n:expr, $ue:expr, $se:expr, $get:expr, $align:expr) => {{
        let r = &mut $r;
        let mut acc = 0u64;
        for _ in 0..$n {
            acc = acc.wrapping_add(u64::from($get(r, 8)));
            acc = acc.wrapping_add(u64::from($get(r, 8)));
            acc = acc.wrapping_add(u64::from($get(r, 8)));
            for _ in 0..4 {
                acc = acc.wrapping_add(u64::from($ue(r)));
            }
            $get(r, 1);
            $get(r, 1);
            for _ in 0..4 {
                acc = acc.wrapping_add(u64::from($ue(r)));
            }
            $get(r, 1);
            acc = acc.wrapping_add(u64::from($ue(r)));
            acc = acc.wrapping_add(u64::from($ue(r)));
            $get(r, 1);
            $get(r, 1);
            $get(r, 1);
            for _ in 0..4 {
                acc = acc.wrapping_add(u64::from($ue(r)));
            }
            for _ in 0..8 {
                acc = acc.wrapping_add($se(r) as i64 as u64);
            }
            // rbsp_trailing_bits: the stop bit, then realign.
            $get(r, 1);
            $align(r);
        }
        acc
    }};
}

// ---------------------------------------------------------------- the corpora

/// How many synthetic parameter sets the header workload parses per iteration.
const HEADERS: usize = 512;

/// A buffer of parameter-set-shaped syntax: fixed-width fields and Exp-Golomb,
/// in roughly the proportion an H.264 SPS uses. This is the D5 v0.1 workload —
/// `ffprobe` reads headers and nothing else, so it is the case where bit reading
/// is the largest share of total time.
static HEADER_STREAM: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let mut w = BitWriter::new();
    let mut s = 0x1234_5678_9ABC_DEF0u64;
    let mut rng = move || {
        s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        (s >> 33) as u32
    };
    for _ in 0..HEADERS {
        w.put(8, 100); // profile_idc
        w.put(8, 0); // constraint flags
        w.put(8, 41); // level_idc
        w.ue(rng() % 32); // seq_parameter_set_id
        w.ue(rng() % 4); // chroma_format_idc
        w.ue(rng() % 8); // bit_depth_luma_minus8
        w.ue(rng() % 8); // bit_depth_chroma_minus8
        w.put(1, 0);
        w.put(1, 0);
        w.ue(rng() % 13); // log2_max_frame_num_minus4
        w.ue(rng() % 3); // pic_order_cnt_type
        w.ue(rng() % 13);
        w.ue(rng() % 17); // max_num_ref_frames
        w.put(1, 0);
        w.ue(rng() % 512); // pic_width_in_mbs_minus1
        w.ue(rng() % 512); // pic_height_in_map_units_minus1
        w.put(1, 1);
        w.put(1, 0);
        w.put(1, 1);
        for _ in 0..4 {
            w.ue(rng() % 64); // frame cropping offsets
        }
        for _ in 0..8 {
            w.se((rng() % 64) as i32 - 32); // scaling-list-shaped deltas
        }
        w.rbsp_trailing();
    }
    w.finish()
});

/// 256 KiB of pseudo-random bytes: the decode-shaped workload, where bit reading
/// is a single-digit percentage of the work a real decoder does.
static BULK: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let mut s = 0x0BAD_C0FF_EE0D_DF00u64;
    (0..256 * 1024)
        .map(|_| {
            s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (s >> 33) as u8
        })
        .collect()
});

/// The same bytes with the padding attached, built once so the benchmark
/// measures reading and not `memcpy`.
static BULK_PADDED: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let mut v = BULK.clone();
    v.resize(BULK.len() + Padded::PAD, 0);
    v
});

static HEADER_PADDED: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let mut v = HEADER_STREAM.clone();
    v.resize(HEADER_STREAM.len() + Padded::PAD, 0);
    v
});

/// The same syntax, but as `HEADERS` separate buffers rather than one long one.
///
/// This is the shape `ffprobe` actually sees: a parameter set arrives as its own
/// NAL unit, tens of bytes long, and gets its own reader. A buffer that short is
/// *entirely* inside the last eight bytes, so the unpadded reader spends the
/// whole parse in the byte-at-a-time tail path. If F9's padding is worth
/// anything, it is worth it here.
static UNITS: LazyLock<Vec<Vec<u8>>> = LazyLock::new(|| {
    let mut out = Vec::new();
    let mut start = 0usize;
    // Re-derive the unit boundaries by parsing the concatenated corpus.
    let mut r = BitReader::new(&HEADER_STREAM);
    for _ in 0..HEADERS {
        let _ = parse_headers!(
            r,
            1usize,
            |r: &mut BitReader<'_>| r.ue(),
            |r: &mut BitReader<'_>| r.se(),
            |r: &mut BitReader<'_>, n| r.get(n),
            |r: &mut BitReader<'_>| r.align()
        );
        let end = (r.bit_pos() >> 3) as usize;
        out.push(HEADER_STREAM[start..end].to_vec());
        start = end;
    }
    out
});

static UNITS_PADDED: LazyLock<Vec<Vec<u8>>> = LazyLock::new(|| {
    UNITS
        .iter()
        .map(|u| {
            let mut v = u.clone();
            v.resize(u.len() + Padded::PAD, 0);
            v
        })
        .collect()
});

/// An Annex-B stream: 4 MiB of payload split into 1 KiB NAL units.
static ANNEXB: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let mut s = 0xFEED_FACE_CAFE_BEEFu64;
    let mut out = Vec::new();
    for _ in 0..4096 {
        out.extend_from_slice(&[0, 0, 0, 1]);
        for _ in 0..1024 {
            s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            // Never zero, so the scan cannot get lucky.
            out.push(((s >> 33) as u8) | 1);
        }
    }
    out
});

// -------------------------------------------------------------- the baselines

/// F13's rejected option (a): our exact cache and refill, `Result` on every read.
mod result_per_read {
    pub struct Reader<'a> {
        data: &'a [u8],
        pos: usize,
        cache: u64,
        cache_bits: u32,
    }

    #[derive(Debug)]
    pub struct Overrun;

    impl<'a> Reader<'a> {
        pub const fn new(data: &'a [u8]) -> Self {
            Self {
                data,
                pos: 0,
                cache: 0,
                cache_bits: 0,
            }
        }

        #[inline]
        pub fn bits_left(&self) -> u64 {
            (self.data.len() as u64)
                .saturating_mul(8)
                .saturating_sub((self.pos as u64).saturating_mul(8) - u64::from(self.cache_bits))
        }

        #[inline]
        fn refill(&mut self) {
            if self.cache_bits > 56 {
                return;
            }
            let mut chunk = 0u64;
            for i in 0..8usize {
                chunk = (chunk << 8) | u64::from(self.data.get(self.pos + i).copied().unwrap_or(0));
            }
            self.cache |= chunk >> self.cache_bits;
            let take = (64 - self.cache_bits) >> 3;
            self.pos += take as usize;
            self.cache_bits += take * 8;
        }

        #[inline]
        pub fn get(&mut self, n: u32) -> Result<u32, Overrun> {
            if n == 0 {
                return Ok(0);
            }
            if u64::from(n) > self.bits_left() {
                return Err(Overrun);
            }
            if self.cache_bits < n {
                self.refill();
            }
            let v = (self.cache >> (64 - n)) as u32;
            self.cache <<= n;
            self.cache_bits -= n;
            Ok(v)
        }

        /// Infallible, like ours: the refill zero-fills past the end, so a peek
        /// near the end is not an error. Only *consuming* reads return `Result`,
        /// which is the design F13 rejected and the thing being measured.
        #[inline]
        pub fn peek(&mut self, n: u32) -> u32 {
            if self.cache_bits < n {
                self.refill();
            }
            (self.cache >> (64 - n)) as u32
        }

        #[inline]
        pub fn skip(&mut self, n: u32) -> Result<(), Overrun> {
            self.get(n).map(|_| ())
        }

        #[inline]
        pub fn align(&mut self) {
            let _ = self.skip(self.cache_bits & 7);
        }

        #[inline]
        pub fn ue(&mut self) -> Result<u32, Overrun> {
            let lz = self.peek(32).leading_zeros();
            if lz > 31 {
                return Err(Overrun);
            }
            self.skip(lz + 1)?;
            let suffix = self.get(lz)?;
            Ok(((1u32 << lz) - 1).wrapping_add(suffix))
        }

        #[inline]
        pub fn se(&mut self) -> Result<i32, Overrun> {
            let k = self.ue()?;
            Ok(if k & 1 == 1 {
                (k.div_ceil(2)) as i32
            } else {
                -((k >> 1) as i32)
            })
        }
    }
}

/// No cache: one bounds-checked slice access per syntax element.
mod checked_per_read {
    pub struct Reader<'a> {
        data: &'a [u8],
        bit: u64,
        pub overrun: bool,
    }

    impl<'a> Reader<'a> {
        pub const fn new(data: &'a [u8]) -> Self {
            Self {
                data,
                bit: 0,
                overrun: false,
            }
        }

        #[inline]
        pub fn peek(&mut self, n: u32) -> u32 {
            let at = self.bit;
            let v = self.get(n);
            self.bit = at;
            v
        }

        #[inline]
        pub fn get(&mut self, n: u32) -> u32 {
            if n == 0 {
                return 0;
            }
            let start = (self.bit >> 3) as usize;
            let off = (self.bit & 7) as u32;
            let need = ((off + n).div_ceil(8)) as usize;
            let mut acc = 0u64;
            if let Some(s) = self.data.get(start..start + need) {
                for &b in s {
                    acc = (acc << 8) | u64::from(b);
                }
            } else {
                self.overrun = true;
                for i in 0..need {
                    acc = (acc << 8) | u64::from(self.data.get(start + i).copied().unwrap_or(0));
                }
            }
            let shift = (need as u32) * 8 - off - n;
            let mask = if n == 32 { u32::MAX } else { (1u32 << n) - 1 };
            self.bit += u64::from(n);
            ((acc >> shift) as u32) & mask
        }

        #[inline]
        pub fn skip(&mut self, n: u32) {
            self.bit += u64::from(n);
        }

        #[inline]
        pub fn align(&mut self) {
            self.bit = (self.bit + 7) & !7;
        }

        #[inline]
        pub fn ue(&mut self) -> u32 {
            let lz = self.peek(32).leading_zeros();
            if lz > 31 {
                self.overrun = true;
                return 0;
            }
            self.skip(lz + 1);
            ((1u32 << lz) - 1).wrapping_add(self.get(lz))
        }

        #[inline]
        pub fn se(&mut self) -> i32 {
            let k = self.ue();
            if k & 1 == 1 {
                (k.div_ceil(2)) as i32
            } else {
                -((k >> 1) as i32)
            }
        }
    }
}

/// The textbook-safe reader: one bounds check per bit.
mod bytewise {
    pub struct Reader<'a> {
        data: &'a [u8],
        bit: usize,
        pub overrun: bool,
    }

    impl<'a> Reader<'a> {
        pub const fn new(data: &'a [u8]) -> Self {
            Self {
                data,
                bit: 0,
                overrun: false,
            }
        }

        #[inline]
        pub fn get(&mut self, n: u32) -> u32 {
            let mut v = 0u32;
            for _ in 0..n {
                let byte = match self.data.get(self.bit >> 3) {
                    Some(&b) => b,
                    None => {
                        self.overrun = true;
                        0
                    }
                };
                v = (v << 1) | u32::from((byte >> (7 - (self.bit & 7))) & 1);
                self.bit += 1;
            }
            v
        }

        #[inline]
        pub fn align(&mut self) {
            self.bit = (self.bit + 7) & !7;
        }

        #[inline]
        pub fn ue(&mut self) -> u32 {
            let mut lz = 0u32;
            while self.get(1) == 0 {
                lz += 1;
                if lz > 31 {
                    self.overrun = true;
                    return 0;
                }
            }
            ((1u32 << lz) - 1).wrapping_add(self.get(lz))
        }

        #[inline]
        pub fn se(&mut self) -> i32 {
            let k = self.ue();
            if k & 1 == 1 {
                (k.div_ceil(2)) as i32
            } else {
                -((k >> 1) as i32)
            }
        }
    }
}

// ------------------------------------------------------- the header workload

#[divan::bench_group(name = "header_parse")]
mod header_parse {
    use super::{
        BitReader, BytesCount, GolombRead, HEADER_PADDED, HEADER_STREAM, HEADERS, Padded, bytewise,
        checked_per_read, result_per_read,
    };

    #[divan::bench]
    fn padded(bencher: divan::Bencher<'_, '_>) {
        let padded = Padded::new(&HEADER_PADDED, HEADER_STREAM.len()).unwrap();
        bencher
            .counter(BytesCount::new(HEADER_STREAM.len()))
            .bench_local(|| {
                let mut r = BitReader::new_padded(padded);
                parse_headers!(
                    r,
                    HEADERS,
                    |r: &mut BitReader<'_>| r.ue(),
                    |r: &mut BitReader<'_>| r.se(),
                    |r: &mut BitReader<'_>, n| r.get(n),
                    |r: &mut BitReader<'_>| r.align()
                )
            });
    }

    #[divan::bench]
    fn unpadded(bencher: divan::Bencher<'_, '_>) {
        bencher
            .counter(BytesCount::new(HEADER_STREAM.len()))
            .bench_local(|| {
                let mut r = BitReader::new(&HEADER_STREAM);
                parse_headers!(
                    r,
                    HEADERS,
                    |r: &mut BitReader<'_>| r.ue(),
                    |r: &mut BitReader<'_>| r.se(),
                    |r: &mut BitReader<'_>, n| r.get(n),
                    |r: &mut BitReader<'_>| r.align()
                )
            });
    }

    #[divan::bench]
    fn result_per_read(bencher: divan::Bencher<'_, '_>) {
        use result_per_read::Reader;
        bencher
            .counter(BytesCount::new(HEADER_STREAM.len()))
            .bench_local(|| {
                let mut r = Reader::new(&HEADER_STREAM);
                parse_headers!(
                    r,
                    HEADERS,
                    |r: &mut Reader<'_>| r.ue().unwrap_or(0),
                    |r: &mut Reader<'_>| r.se().unwrap_or(0),
                    |r: &mut Reader<'_>, n| r.get(n).unwrap_or(0),
                    |r: &mut Reader<'_>| r.align()
                )
            });
    }

    #[divan::bench]
    fn checked_per_read(bencher: divan::Bencher<'_, '_>) {
        use checked_per_read::Reader;
        bencher
            .counter(BytesCount::new(HEADER_STREAM.len()))
            .bench_local(|| {
                let mut r = Reader::new(&HEADER_STREAM);
                parse_headers!(
                    r,
                    HEADERS,
                    |r: &mut Reader<'_>| r.ue(),
                    |r: &mut Reader<'_>| r.se(),
                    |r: &mut Reader<'_>, n| r.get(n),
                    |r: &mut Reader<'_>| r.align()
                )
            });
    }

    #[divan::bench]
    fn bytewise(bencher: divan::Bencher<'_, '_>) {
        use bytewise::Reader;
        bencher
            .counter(BytesCount::new(HEADER_STREAM.len()))
            .bench_local(|| {
                let mut r = Reader::new(&HEADER_STREAM);
                parse_headers!(
                    r,
                    HEADERS,
                    |r: &mut Reader<'_>| r.ue(),
                    |r: &mut Reader<'_>| r.se(),
                    |r: &mut Reader<'_>, n| r.get(n),
                    |r: &mut Reader<'_>| r.align()
                )
            });
    }
}

// ------------------------------------------------------ the per-unit workload

/// One reader per short buffer — the case the padding exists for.
#[divan::bench_group(name = "per_unit_parse")]
mod per_unit_parse {
    use super::{
        BitReader, BytesCount, GolombRead, Padded, UNITS, UNITS_PADDED, bytewise, checked_per_read,
        result_per_read,
    };

    fn total_bytes() -> usize {
        UNITS.iter().map(Vec::len).sum()
    }

    #[divan::bench]
    fn padded(bencher: divan::Bencher<'_, '_>) {
        let readers: Vec<Padded<'_>> = UNITS_PADDED
            .iter()
            .zip(UNITS.iter())
            .map(|(p, u)| Padded::new(p, u.len()).unwrap())
            .collect();
        bencher
            .counter(BytesCount::new(total_bytes()))
            .bench_local(|| {
                let mut acc = 0u64;
                for p in &readers {
                    let mut r = BitReader::new_padded(*p);
                    acc = acc.wrapping_add(parse_headers!(
                        r,
                        1usize,
                        |r: &mut BitReader<'_>| r.ue(),
                        |r: &mut BitReader<'_>| r.se(),
                        |r: &mut BitReader<'_>, n| r.get(n),
                        |r: &mut BitReader<'_>| r.align()
                    ));
                }
                acc
            });
    }

    #[divan::bench]
    fn unpadded(bencher: divan::Bencher<'_, '_>) {
        bencher
            .counter(BytesCount::new(total_bytes()))
            .bench_local(|| {
                let mut acc = 0u64;
                for u in UNITS.iter() {
                    let mut r = BitReader::new(u);
                    acc = acc.wrapping_add(parse_headers!(
                        r,
                        1usize,
                        |r: &mut BitReader<'_>| r.ue(),
                        |r: &mut BitReader<'_>| r.se(),
                        |r: &mut BitReader<'_>, n| r.get(n),
                        |r: &mut BitReader<'_>| r.align()
                    ));
                }
                acc
            });
    }

    #[divan::bench]
    fn result_per_read(bencher: divan::Bencher<'_, '_>) {
        use result_per_read::Reader;
        bencher
            .counter(BytesCount::new(total_bytes()))
            .bench_local(|| {
                let mut acc = 0u64;
                for u in UNITS.iter() {
                    let mut r = Reader::new(u);
                    acc = acc.wrapping_add(parse_headers!(
                        r,
                        1usize,
                        |r: &mut Reader<'_>| r.ue().unwrap_or(0),
                        |r: &mut Reader<'_>| r.se().unwrap_or(0),
                        |r: &mut Reader<'_>, n| r.get(n).unwrap_or(0),
                        |r: &mut Reader<'_>| r.align()
                    ));
                }
                acc
            });
    }

    #[divan::bench]
    fn checked_per_read(bencher: divan::Bencher<'_, '_>) {
        use checked_per_read::Reader;
        bencher
            .counter(BytesCount::new(total_bytes()))
            .bench_local(|| {
                let mut acc = 0u64;
                for u in UNITS.iter() {
                    let mut r = Reader::new(u);
                    acc = acc.wrapping_add(parse_headers!(
                        r,
                        1usize,
                        |r: &mut Reader<'_>| r.ue(),
                        |r: &mut Reader<'_>| r.se(),
                        |r: &mut Reader<'_>, n| r.get(n),
                        |r: &mut Reader<'_>| r.align()
                    ));
                }
                acc
            });
    }

    #[divan::bench]
    fn bytewise(bencher: divan::Bencher<'_, '_>) {
        use bytewise::Reader;
        bencher
            .counter(BytesCount::new(total_bytes()))
            .bench_local(|| {
                let mut acc = 0u64;
                for u in UNITS.iter() {
                    let mut r = Reader::new(u);
                    acc = acc.wrapping_add(parse_headers!(
                        r,
                        1usize,
                        |r: &mut Reader<'_>| r.ue(),
                        |r: &mut Reader<'_>| r.se(),
                        |r: &mut Reader<'_>, n| r.get(n),
                        |r: &mut Reader<'_>| r.align()
                    ));
                }
                acc
            });
    }
}

// --------------------------------------------------------- the bulk workload

/// Fixed-width reads over 256 KiB — the shape of a decoder's residual and
/// coefficient reads, where every read is small and the widths repeat.
#[divan::bench_group(name = "bulk_fixed_width")]
mod bulk_fixed_width {
    use super::{
        BULK, BULK_PADDED, BitReader, BytesCount, Padded, bytewise, checked_per_read,
        result_per_read,
    };

    /// Widths chosen so the refill boundary is crossed at every alignment.
    const WIDTHS: [u32; 8] = [4, 8, 3, 12, 1, 16, 5, 7];
    const ROUNDS: usize = 16 * 1024;

    #[divan::bench]
    fn padded(bencher: divan::Bencher<'_, '_>) {
        let padded = Padded::new(&BULK_PADDED, BULK.len()).unwrap();
        bencher
            .counter(BytesCount::new(ROUNDS * 7))
            .bench_local(|| {
                let mut r = BitReader::new_padded(padded);
                let mut acc = 0u64;
                for _ in 0..ROUNDS {
                    for w in WIDTHS {
                        acc = acc.wrapping_add(u64::from(r.get(w)));
                    }
                }
                acc
            });
    }

    #[divan::bench]
    fn unpadded(bencher: divan::Bencher<'_, '_>) {
        bencher
            .counter(BytesCount::new(ROUNDS * 7))
            .bench_local(|| {
                let mut r = BitReader::new(&BULK);
                let mut acc = 0u64;
                for _ in 0..ROUNDS {
                    for w in WIDTHS {
                        acc = acc.wrapping_add(u64::from(r.get(w)));
                    }
                }
                acc
            });
    }

    #[divan::bench]
    fn result_per_read(bencher: divan::Bencher<'_, '_>) {
        bencher
            .counter(BytesCount::new(ROUNDS * 7))
            .bench_local(|| {
                let mut r = result_per_read::Reader::new(&BULK);
                let mut acc = 0u64;
                for _ in 0..ROUNDS {
                    for w in WIDTHS {
                        acc = acc.wrapping_add(u64::from(r.get(w).unwrap_or(0)));
                    }
                }
                acc
            });
    }

    #[divan::bench]
    fn checked_per_read(bencher: divan::Bencher<'_, '_>) {
        bencher
            .counter(BytesCount::new(ROUNDS * 7))
            .bench_local(|| {
                let mut r = checked_per_read::Reader::new(&BULK);
                let mut acc = 0u64;
                for _ in 0..ROUNDS {
                    for w in WIDTHS {
                        acc = acc.wrapping_add(u64::from(r.get(w)));
                    }
                }
                acc
            });
    }

    #[divan::bench]
    fn bytewise(bencher: divan::Bencher<'_, '_>) {
        bencher
            .counter(BytesCount::new(ROUNDS * 7))
            .bench_local(|| {
                let mut r = bytewise::Reader::new(&BULK);
                let mut acc = 0u64;
                for _ in 0..ROUNDS {
                    for w in WIDTHS {
                        acc = acc.wrapping_add(u64::from(r.get(w)));
                    }
                }
                acc
            });
    }
}

// ------------------------------------------------------ start-code scanning

#[divan::bench_group(name = "start_code_scan")]
mod start_code_scan {
    use super::{ANNEXB, BytesCount, annexb};

    #[divan::bench]
    fn word_skip(bencher: divan::Bencher<'_, '_>) {
        bencher
            .counter(BytesCount::new(ANNEXB.len()))
            .bench_local(|| annexb::nal_units(&ANNEXB).count());
    }

    /// The obvious three-byte-window scan, for scale.
    #[divan::bench]
    fn windows(bencher: divan::Bencher<'_, '_>) {
        bencher
            .counter(BytesCount::new(ANNEXB.len()))
            .bench_local(|| ANNEXB.windows(3).filter(|w| matches!(w, [0, 0, 1])).count());
    }
}

/// A benchmark that measures four readers doing *different* work measures
/// nothing. Before timing anything, parse the corpus once with each and require
/// identical results and a clean finish.
fn verify() {
    let padded = Padded::new(&HEADER_PADDED, HEADER_STREAM.len()).unwrap();
    let mut a = BitReader::new_padded(padded);
    let ra = parse_headers!(
        a,
        HEADERS,
        |r: &mut BitReader<'_>| r.ue(),
        |r: &mut BitReader<'_>| r.se(),
        |r: &mut BitReader<'_>, n| r.get(n),
        |r: &mut BitReader<'_>| r.align()
    );
    assert!(!a.overrun(), "padded reader overran the header corpus");
    assert_eq!(
        a.bit_pos(),
        (HEADER_STREAM.len() as u64) * 8,
        "the parse and the writer disagree about the syntax"
    );

    let mut b = BitReader::new(&HEADER_STREAM);
    let rb = parse_headers!(
        b,
        HEADERS,
        |r: &mut BitReader<'_>| r.ue(),
        |r: &mut BitReader<'_>| r.se(),
        |r: &mut BitReader<'_>, n| r.get(n),
        |r: &mut BitReader<'_>| r.align()
    );

    let mut c = result_per_read::Reader::new(&HEADER_STREAM);
    let rc = parse_headers!(
        c,
        HEADERS,
        |r: &mut result_per_read::Reader<'_>| r.ue().unwrap_or(0),
        |r: &mut result_per_read::Reader<'_>| r.se().unwrap_or(0),
        |r: &mut result_per_read::Reader<'_>, n| r.get(n).unwrap_or(0),
        |r: &mut result_per_read::Reader<'_>| r.align()
    );

    let mut d = checked_per_read::Reader::new(&HEADER_STREAM);
    let rd = parse_headers!(
        d,
        HEADERS,
        |r: &mut checked_per_read::Reader<'_>| r.ue(),
        |r: &mut checked_per_read::Reader<'_>| r.se(),
        |r: &mut checked_per_read::Reader<'_>, n| r.get(n),
        |r: &mut checked_per_read::Reader<'_>| r.align()
    );
    // `checked_per_read` has no zero-padded cache, so its 32-bit Exp-Golomb peek
    // reads past the end of the last codeword and flags. That is a property of
    // the baseline, not of the data: the values it produces still agree.

    let mut e = bytewise::Reader::new(&HEADER_STREAM);
    let re = parse_headers!(
        e,
        HEADERS,
        |r: &mut bytewise::Reader<'_>| r.ue(),
        |r: &mut bytewise::Reader<'_>| r.se(),
        |r: &mut bytewise::Reader<'_>, n| r.get(n),
        |r: &mut bytewise::Reader<'_>| r.align()
    );
    assert!(!e.overrun, "the bit-at-a-time baseline overran");

    assert_eq!((ra, ra, ra, ra), (rb, rc, rd, re), "the readers disagree");
}
