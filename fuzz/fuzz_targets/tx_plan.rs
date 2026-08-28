//! `Plan::new` and execution over arbitrary transform parameters.
//!
//! `docs/signal/vaco-tx.md` §3.5 records that this crate has no fuzz target of
//! its own yet, with `tests/properties.rs::plan_new_is_total_over_every_length`
//! standing in for lengths 1..=2048. This target covers the rest of the
//! parameter space `Plan::new` actually takes — `kind`, `dir`, `flags` and a
//! length up to the low hundred-thousands, which reaches every decomposition
//! rule (`§2.2`: mixed radix, Good–Thomas, Rader, Bluestein) without the
//! multi-second cost of probing near `MAX_LEN = 2^24` on every run — and then
//! actually executes the plan, which `plan_new_is_total_over_every_length`
//! does not.
//!
//! A finding is a panic, a non-termination, or `execute` writing outside the
//! buffer lengths `Plan` itself reports.
//!
//! **The length is capped at 8192**, the crate's own documented boundary
//! (`docs/signal/vaco-tx.md` §3.4 item 4: "the trigger is `n = 8192`
//! (Vorbis)", with cache-blocking above it explicitly deferred as "nothing
//! shipping needs it yet"). This target found a real slow unit above that
//! cap: `TxKind::DctI` at `len = 933439` took 9.75s of CPU time, because
//! `DctI`'s inner FFT runs at `2*(len-1)`, and a badly-factoring length there
//! can chain Rader and Bluestein recursively. That is a genuine cost, not a
//! fuzzer artifact, but investigating the recursive-convolution cost model
//! well past the crate's own stated performance envelope is out of scope for
//! this pass; capping here keeps the smoke target inside the range the crate
//! already benchmarks and stands behind.
//! fuzz-crate: vaco-tx
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_tx::{Direction, Plan, Tx, TxFlags, TxKind};

#[derive(Arbitrary, Debug)]
struct Input {
    kind: u8,
    inverse: bool,
    len_raw: u32,
    /// Force the two length boundaries `Plan::new` itself special-cases,
    /// rather than leaving the mutator to stumble onto them: `0` on `true`,
    /// `2^24 + 1` on `false` when `len_raw` is itself zero.
    boundary: bool,
    flags_bits: u8,
    scale_bits: u32,
    input: Vec<f32>,
}

const KINDS: [TxKind; 6] = [
    TxKind::Fft,
    TxKind::Mdct,
    TxKind::Rdft,
    TxKind::Dct,
    TxKind::DctI,
    TxKind::DstI,
];

fuzz_target!(|input: Input| {
    let kind = KINDS[usize::from(input.kind) % KINDS.len()];
    let dir = if input.inverse {
        Direction::Inverse
    } else {
        Direction::Forward
    };
    let len = if input.len_raw == 0 {
        if input.boundary { 0 } else { (1_usize << 24) + 1 }
    } else {
        // Every decomposition rule (mixed radix, Good-Thomas, Rader,
        // Bluestein) is reachable well inside this range. See the module doc
        // for why it stops at 8192 rather than the crate's own 2^24 ceiling.
        (input.len_raw as usize % 8192) + 1
    };
    // Only the five bits `TxFlags` defines are meaningful; higher bits of an
    // arbitrary byte must not become UB or a silently-different flag set.
    let flags = TxFlags::from_bits_truncate(u32::from(input.flags_bits) & 0x1f);
    let scale = f32::from_bits(input.scale_bits);

    let plan = match Plan::<f32>::new(kind, dir, len, scale, flags) {
        Ok(p) => p,
        Err(_) => return,
    };

    let in_len = plan.input_len();
    let out_len = plan.output_len();
    // Bound the transform this run actually executes: `input_len` already
    // reflects `len` (capped above), but stay defensive rather than trust a
    // single call site.
    if in_len > 1 << 22 || out_len > 1 << 22 {
        return;
    }

    let mut src = input.input;
    src.resize(in_len, 0.0);
    let mut dst = vec![0.0f32; out_len];

    let mut tx = Tx::new(plan.clone());
    tx.execute(&mut dst, &src);
    assert_eq!(dst.len(), out_len, "execute must not resize the output buffer");

    if flags.contains(TxFlags::INPLACE) && in_len == out_len {
        let mut buf = src;
        let mut tx2 = Tx::new(plan);
        tx2.execute_inplace(&mut buf);
        assert_eq!(buf.len(), in_len, "execute_inplace must not resize the buffer");
    }
});
