//! Write-then-read is the identity, over arbitrary write scripts.
//!
//! The single most valuable property in the crate, run at fuzzer speed rather
//! than at proptest's 256 cases.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_bitstream::{BitReader, BitWriter, GolombRead};

#[derive(Arbitrary, Debug)]
enum Field {
    Fixed(u8, u32),
    Wide(u8, u64),
    Signed(u8, i32),
    Ue(u32),
    Se(i32),
    Zeros(u16),
}

fuzz_target!(|fields: Vec<Field>| {
    let mut w = BitWriter::new();
    for f in &fields {
        match *f {
            Field::Fixed(n, v) => w.put(u32::from(n % 33), v),
            Field::Wide(n, v) => w.put_long(u32::from(n % 65), v),
            Field::Signed(n, v) => w.put_signed(u32::from(n % 33), v),
            Field::Ue(v) => w.ue(v.min(u32::MAX - 1)),
            Field::Se(v) => w.se(v.max(-i32::MAX)),
            Field::Zeros(n) => w.put_zeros(u32::from(n)),
        }
    }
    let expect_bits = w.bit_len();
    let bytes = w.finish();
    assert!((bytes.len() as u64) * 8 >= expect_bits);

    let mut r = BitReader::new(&bytes);
    for f in &fields {
        match *f {
            Field::Fixed(n, v) => {
                let n = u32::from(n % 33);
                let want = if n == 0 {
                    0
                } else if n == 32 {
                    v
                } else {
                    v & ((1u32 << n) - 1)
                };
                assert_eq!(r.get(n), want, "fixed {n}");
            }
            Field::Wide(n, v) => {
                let n = u32::from(n % 65);
                let want = if n == 0 {
                    0
                } else if n == 64 {
                    v
                } else {
                    v & ((1u64 << n) - 1)
                };
                assert_eq!(r.get_long(n), want, "wide {n}");
            }
            Field::Signed(n, v) => {
                let n = u32::from(n % 33);
                let want = if n == 0 { 0 } else { (v << (32 - n)) >> (32 - n) };
                assert_eq!(r.get_signed(n), want, "signed {n}");
            }
            Field::Ue(v) => assert_eq!(r.ue(), v.min(u32::MAX - 1)),
            Field::Se(v) => assert_eq!(r.se(), v.max(-i32::MAX)),
            Field::Zeros(n) => {
                let mut left = u32::from(n);
                while left > 32 {
                    assert_eq!(r.get(32), 0);
                    left -= 32;
                }
                assert_eq!(r.get(left), 0);
            }
        }
    }
    assert!(!r.overrun(), "the writer produced fewer bits than it claimed");
});
