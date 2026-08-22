//! `ByteReader` against arbitrary bytes and an arbitrary access script.
//!
//! The byte reader is what every container parser sits on, so its sticky-overrun
//! contract needs the same coverage as the bit reader's: never panic, never read
//! out of bounds, never report bytes remaining after a truncated read.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_bitstream::ByteReader;

#[derive(Arbitrary, Debug)]
enum Op {
    U8,
    I8,
    Be16,
    Le16,
    Be24,
    Le24,
    Be32,
    Le32,
    Be64,
    Le64,
    F32Be,
    F64Be,
    Bytes(u16),
    Skip(usize),
    Seek(usize),
    Sub(u16),
    Rest,
}

#[derive(Arbitrary, Debug)]
struct Input {
    data: Vec<u8>,
    script: Vec<Op>,
}

fuzz_target!(|input: Input| {
    let mut r = ByteReader::new(&input.data);
    for op in &input.script {
        match *op {
            Op::U8 => {
                r.u8();
            }
            Op::I8 => {
                r.i8();
            }
            Op::Be16 => {
                r.be16();
            }
            Op::Le16 => {
                r.le16();
            }
            Op::Be24 => {
                r.be24();
            }
            Op::Le24 => {
                r.le24();
            }
            Op::Be32 => {
                r.be32();
            }
            Op::Le32 => {
                r.le32();
            }
            Op::Be64 => {
                r.be64();
            }
            Op::Le64 => {
                r.le64();
            }
            Op::F32Be => {
                r.f32_be();
            }
            Op::F64Be => {
                r.f64_be();
            }
            Op::Bytes(n) => {
                let s = r.bytes(usize::from(n));
                assert!(s.len() <= usize::from(n));
                assert!(s.len() <= input.data.len());
            }
            Op::Skip(n) => r.skip(n),
            Op::Seek(n) => r.seek(n),
            Op::Sub(n) => {
                let mut sub = r.sub(usize::from(n));
                // A sub-reader can never see past its own window.
                let taken = sub.bytes(usize::MAX);
                assert!(taken.len() <= usize::from(n));
            }
            Op::Rest => {
                assert!(r.rest().len() <= input.data.len());
            }
        }
        assert!(r.pos() <= input.data.len(), "the cursor escaped the buffer");
        if r.overrun() {
            assert_eq!(r.remaining(), 0);
            assert!(r.check().is_err());
        } else {
            assert_eq!(r.remaining(), input.data.len() - r.pos());
        }
    }
});
