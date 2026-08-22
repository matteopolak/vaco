//! `BitReader` against arbitrary bytes and an arbitrary read script.
//!
//! Findings are: a panic, a hang, or a disagreement between the padded and
//! unpadded readers. A truncated input is *not* a finding — the reader is
//! specified to return zeros and set overrun, and this target asserts it does.
//! fuzz-crate: vaco-bitstream
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_bitstream::{BitReader, Padded};

#[derive(Arbitrary, Debug)]
enum Op {
    Get(u8),
    GetLong(u8),
    GetSigned(u8),
    Peek(u8),
    Skip(u32),
    SkipLong(u64),
    SkipBytes(u16),
    Align,
    TryGet(u8),
    MarkRestore(u8),
    RemainingBytes,
}

#[derive(Arbitrary, Debug)]
struct Input {
    data: Vec<u8>,
    logical_len: u16,
    script: Vec<Op>,
}

/// Run one script against one reader, returning the values it produced.
fn run(r: &mut BitReader<'_>, script: &[Op]) -> Vec<u64> {
    let mut out = Vec::new();
    for op in script {
        match *op {
            Op::Get(n) => out.push(u64::from(r.get(u32::from(n % 33)))),
            Op::GetLong(n) => out.push(r.get_long(u32::from(n % 65))),
            Op::GetSigned(n) => out.push(r.get_signed(u32::from(n % 33)) as u32 as u64),
            Op::Peek(n) => out.push(u64::from(r.peek(u32::from(n % 33)))),
            Op::Skip(n) => r.skip(n),
            Op::SkipLong(n) => r.skip_long(n),
            Op::SkipBytes(n) => r.skip_bytes(usize::from(n)),
            Op::Align => r.align(),
            Op::TryGet(n) => out.push(match r.try_get(u32::from(n % 33)) {
                Ok(v) => u64::from(v),
                Err(_) => u64::MAX,
            }),
            Op::MarkRestore(n) => {
                // Save, read, restore, read again: the two reads must agree.
                let m = r.mark();
                let a = r.get(u32::from(n % 33));
                r.restore(m);
                let b = r.get(u32::from(n % 33));
                assert_eq!(a, b, "mark/restore is not a pure position save");
                out.push(u64::from(a));
            }
            Op::RemainingBytes => out.push(r.remaining_bytes().len() as u64),
        }
        // The whole contract, asserted on every single operation.
        assert_eq!(
            r.overrun(),
            r.bit_pos() > r.logical_bits() || r.check().is_err(),
            "overrun disagrees with the position"
        );
        if r.overrun() {
            assert_eq!(r.bits_left(), 0, "bits remain after an overrun");
        }
    }
    out
}

fuzz_target!(|input: Input| {
    let logical_len = usize::from(input.logical_len).min(input.data.len());
    let logical = &input.data[..logical_len];

    // Unpadded: the tail path runs at the end of the buffer.
    let mut a = BitReader::new(logical);
    let va = run(&mut a, &input.script);

    // Padded: the body path runs 56 bytes past the end. Values, overrun and
    // position must be indistinguishable. This is what keeps F9 honest.
    let mut scratch = Vec::new();
    let padded = Padded::from_slice_copying(logical, &mut scratch);
    let mut b = BitReader::new_padded(padded);
    let vb = run(&mut b, &input.script);

    assert_eq!(va, vb, "padded and unpadded readers disagree");
    assert_eq!(a.bit_pos(), b.bit_pos());
    assert_eq!(a.overrun(), b.overrun());
    assert_eq!(a.bits_left(), b.bits_left());

    // A window carved out of a longer buffer must behave like the slice of it.
    let mut c = BitReader::with_logical_len(&input.data, logical_len);
    let vc = run(&mut c, &input.script);
    assert_eq!(va.len(), vc.len());
});
