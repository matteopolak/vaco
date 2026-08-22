//! `vaco-codec-cabac`: the arithmetic decoding engine against arbitrary bytes.
//!
//! CABAC is the deepest point untrusted data reaches in an H.264 or HEVC
//! decoder, and its failure modes are not the ones a bitstream parser has. Four
//! properties are asserted, and the first two are the ones that would otherwise
//! be vulnerabilities:
//!
//! 1. **The engine invariant, `ivlOffset < ivlCurrRange`.** This is what bounds
//!    `ivlOffset`. Break it and `DecodeBypass` becomes `x ↦ 2x + 1 − range`,
//!    which doubles away every bin until it overflows — a panic under the
//!    overflow checks this profile enables. It already caught one real bug in
//!    `decode_terminate`, so it is asserted after *every* operation rather than
//!    at the end.
//! 2. **Termination.** Bypass runs, truncated-unary prefixes and `EGk` prefixes
//!    are all terminated by the bitstream, which means an adversarial one
//!    terminates none of them. Every such loop has a ceiling; libFuzzer's own
//!    timeout is what proves it.
//! 3. **`range` stays in 2..=510**, the interval the renormalisation shift is
//!    proved against.
//! 4. **Encode/decode is the identity.** The encoder is driven from the same
//!    arbitrary input, then decoded back — which exercises carry propagation and
//!    the state machine from the other side, and is the only oracle available
//!    for a full bin sequence.
//! fuzz-crate: vaco-codec-cabac
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_codec_cabac::{CabacDecoder, CabacEncoder, ContextInit, ContextModel, init_contexts};

#[derive(Arbitrary, Debug)]
enum Op {
    Decision(u8),
    Bypass,
    BypassBits(u8),
    Terminate,
    Tu(u8, u8),
    Egk(u8),
    Uegk(u8, u8, u8, bool),
}

#[derive(Arbitrary, Debug)]
struct Input {
    data: Vec<u8>,
    script: Vec<Op>,
    /// Context initialisation is driven from the input too — `(m, n, qp)` are
    /// derived from tables, not from a bitstream, but the derivation must be
    /// total for every triple regardless.
    inits: Vec<(i16, i16, i8)>,
    qp: i8,
    /// A bin sequence for the encoder round trip.
    bins: Vec<(bool, u8)>,
}

/// Contexts touched by the script. 64 is roughly an H.264 macroblock's working
/// set, and a `u8` index modulo 64 reaches all of them.
const CTX: usize = 64;

fuzz_target!(|input: Input| {
    // ---- context initialisation must be total ------------------------------
    let inits: Vec<ContextInit> = input
        .inits
        .iter()
        .take(CTX)
        .map(|&(m, n, _)| ContextInit::new(m, n))
        .collect();
    let mut ctxs = [ContextModel::UNINITIALISED; CTX];
    init_contexts(&mut ctxs, &inits, input.qp);
    for c in &ctxs {
        assert!(c.state_idx() <= 63, "context initialisation left {c:?}");
        assert!(c.packed() < 128);
        assert_eq!(ContextModel::from_packed(c.packed()), *c);
    }
    for &(m, n, qp) in input.inits.iter().take(256) {
        assert!(ContextModel::init_h264(m, n, qp).state_idx() <= 63);
        assert!(ContextModel::init_hevc(m as u8, qp).state_idx() <= 63);
    }

    // ---- the engine invariant, after every operation ------------------------
    let mut dec = CabacDecoder::new(&input.data);
    for (n, op) in input.script.iter().take(8192).enumerate() {
        match *op {
            Op::Decision(c) => {
                dec.decode_decision(&mut ctxs[usize::from(c) % CTX]);
            }
            Op::Bypass => {
                dec.decode_bypass();
            }
            Op::BypassBits(w) => {
                dec.decode_bypass_bits(u32::from(w));
            }
            Op::Terminate => {
                dec.decode_terminate();
            }
            Op::Tu(c, c_max) => {
                let got = dec.decode_tu(&mut ctxs[usize::from(c) % CTX], u32::from(c_max));
                assert!(got <= u32::from(c_max), "TU returned {got} above cMax {c_max}");
            }
            Op::Egk(k) => {
                dec.decode_bypass_egk(u32::from(k));
            }
            Op::Uegk(c, u_coff, k, signed) => {
                dec.decode_uegk(
                    &mut ctxs[usize::from(c) % CTX],
                    u32::from(u_coff),
                    u32::from(k),
                    signed,
                );
            }
        }
        assert!(
            dec.offset() < dec.range(),
            "engine invariant broken after op {n} ({op:?}): offset {} range {}",
            dec.offset(),
            dec.range()
        );
        assert!(
            (2..=510).contains(&dec.range()),
            "range {} left its proved interval after op {n} ({op:?})",
            dec.range()
        );
    }

    // ---- decoding is a pure function of the input --------------------------
    let replay = {
        let mut d = CabacDecoder::new(&input.data);
        let mut c = ContextModel::from_packed(input.qp as u8);
        let mut out = Vec::new();
        for i in 0..64u32 {
            out.push(if i % 2 == 0 {
                d.decode_decision(&mut c)
            } else {
                d.decode_bypass()
            });
        }
        out
    };
    let replay2 = {
        let mut d = CabacDecoder::new(&input.data);
        let mut c = ContextModel::from_packed(input.qp as u8);
        let mut out = Vec::new();
        for i in 0..64u32 {
            out.push(if i % 2 == 0 {
                d.decode_decision(&mut c)
            } else {
                d.decode_bypass()
            });
        }
        out
    };
    assert_eq!(replay, replay2, "decoding is not deterministic");

    // ---- encode then decode is the identity --------------------------------
    let bins: Vec<(bool, usize)> = input
        .bins
        .iter()
        .take(4096)
        .map(|&(b, c)| (b, usize::from(c) % CTX))
        .collect();

    let start: Vec<ContextModel> = (0..CTX)
        .map(|i| ContextModel::from_packed(input.qp.wrapping_add(i as i8) as u8))
        .collect();

    let mut enc_ctxs = start.clone();
    let mut enc = CabacEncoder::new();
    for &(b, c) in &bins {
        enc.encode_decision(&mut enc_ctxs[c], u32::from(b));
    }
    enc.encode_terminate(1);
    let overflowed = enc.overflowed();
    let bytes = enc.finish();

    if !overflowed {
        let mut dec_ctxs = start;
        let mut dec = CabacDecoder::new(&bytes);
        for (i, &(b, c)) in bins.iter().enumerate() {
            assert_eq!(
                dec.decode_decision(&mut dec_ctxs[c]),
                u32::from(b),
                "round trip lost bin {i}"
            );
        }
        assert_eq!(dec.decode_terminate(), 1, "round trip lost the terminator");
        assert!(dec.terminated());
    }
});
