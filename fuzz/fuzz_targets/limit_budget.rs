//! `vaco-limits` under an arbitrary operation sequence.
//!
//! The point of this crate is that hostile sizes produce clean errors, so the
//! findings here are: a panic, an arithmetic overflow, an allocation that got
//! through, or a counter that drifted. `LimitError` is success.
//! fuzz-crate: vaco-limits
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_limits::{Budget, IncrementalVec, Limits, ProgressGuard};

#[derive(Arbitrary, Debug)]
enum Op {
    Alloc(usize),
    AllocWide(usize),
    Charge(u64),
    Release(u64),
    Reserve(u64),
    ReserveCommit(u64),
    ReserveAlloc(u64, usize),
    Fuel(u64),
    Refuel,
    Frame(u32, u32, u32),
    Streams(u64),
    Channels(u64),
    Push(u16),
    Progress(bool),
}

#[derive(Arbitrary, Debug)]
struct Input {
    preset: u8,
    declared: u32,
    script: Vec<Op>,
}

fuzz_target!(|input: Input| {
    // `strict` and `tiny` only. `permissive` allows a single 512 MiB allocation,
    // which is correct behaviour but trips libFuzzer's `-malloc_limit_mb` and
    // wastes the campaign on one enormous `Vec`. Plan 13 §2.2.2 specifies the
    // `limit_*` targets run under a deliberately small budget for this reason.
    let limits = if input.preset % 2 == 0 {
        Limits::tiny()
    } else {
        Limits::strict()
    };
    let cap = limits.max_alloc_total;
    let single = limits.max_alloc_single;
    let mut b = Budget::new(limits);
    let mut inc: IncrementalVec<u8> = IncrementalVec::new(input.declared as usize);
    let mut guard = ProgressGuard::new();
    let mut live: Vec<Vec<u8>> = Vec::new();

    for op in &input.script {
        match *op {
            Op::Alloc(n) => {
                if let Ok(v) = b.alloc::<u8>(n) {
                    assert_eq!(v.len(), n);
                    assert!(n as u64 <= single, "an allocation beat the single cap");
                    live.push(v);
                }
            }
            Op::AllocWide(n) => {
                if let Ok(v) = b.alloc::<u64>(n) {
                    assert_eq!(v.len(), n);
                    assert!((n as u64).saturating_mul(8) <= single);
                }
            }
            Op::Charge(n) => {
                let before = b.committed();
                if b.charge(n).is_ok() {
                    assert_eq!(b.committed(), before + n);
                } else {
                    assert_eq!(b.committed(), before, "a failed charge still charged");
                }
            }
            Op::Release(n) => b.release(n),
            Op::Reserve(n) => {
                // Dropped immediately: the hold must be released.
                let before = b.pending();
                let _ = b.reserve(n);
                assert_eq!(b.pending(), before, "a dropped reservation leaked");
            }
            Op::ReserveCommit(n) => {
                if let Ok(r) = b.reserve(n) {
                    assert_eq!(r.bytes(), n);
                    r.commit();
                }
                assert_eq!(b.pending(), 0);
            }
            Op::ReserveAlloc(n, count) => {
                if let Ok(r) = b.reserve(n) {
                    if let Ok(v) = r.alloc::<u8>(count) {
                        assert_eq!(v.len(), count);
                        assert!(count as u64 <= n, "allocated more than was reserved");
                    }
                }
                assert_eq!(b.pending(), 0);
            }
            Op::Fuel(n) => {
                let before = b.fuel_spent();
                let _ = b.consume_fuel(n);
                assert!(b.fuel_spent() >= before, "fuel went backwards");
            }
            Op::Refuel => b.refuel(),
            Op::Frame(w, h, bpp) => {
                if let Ok(bytes) = b.check_frame(w, h, bpp) {
                    assert_eq!(bytes, u64::from(w) * u64::from(h) * u64::from(bpp));
                    assert!(bytes <= b.limits().max_frame_bytes);
                }
            }
            Op::Streams(n) => {
                if b.check_streams(n).is_ok() {
                    assert!(n <= u64::from(b.limits().max_streams));
                }
            }
            Op::Channels(n) => {
                if b.check_channels(n).is_ok() {
                    assert!(n <= u64::from(b.limits().max_channels));
                }
            }
            Op::Push(n) => {
                let chunk = vec![0u8; usize::from(n)];
                let before = inc.len();
                if inc.push_slice(&mut b, &chunk).is_ok() {
                    assert_eq!(inc.len(), before + chunk.len());
                    assert!(inc.len() <= inc.declared());
                } else {
                    assert_eq!(inc.len(), before, "a failed push still appended");
                }
                // Never charged for the declared size, only for what arrived.
                assert!(
                    inc.charged() <= (inc.len() as u64) * 2 + 64,
                    "incremental growth charged more than a constant factor"
                );
            }
            Op::Progress(p) => {
                let _ = guard.tick(p);
            }
        }
        // The invariant, on every step.
        assert!(
            b.committed().saturating_add(b.pending()) <= cap,
            "the cumulative cap was breached"
        );
        assert!(b.peak() >= b.committed());
        assert!(b.available() <= cap);
    }
});
