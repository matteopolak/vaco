//! The budget invariant, under arbitrary operation sequences.
#![allow(
    clippy::unwrap_used,
    reason = "test code: a panic is the assertion mechanism"
)]

use proptest::prelude::*;
use vaco_limits::{Budget, IncrementalVec, Limits};

#[derive(Debug, Clone)]
enum Op {
    Charge(u64),
    Release(u64),
    ReserveAndCommit(u64),
    ReserveAndDrop(u64),
    Fuel(u64),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u64..4096).prop_map(Op::Charge),
        (0u64..4096).prop_map(Op::Release),
        (0u64..4096).prop_map(Op::ReserveAndCommit),
        (0u64..4096).prop_map(Op::ReserveAndDrop),
        (0u64..1000).prop_map(Op::Fuel),
    ]
}

proptest! {
    /// The cap is never exceeded, whatever order the operations arrive in, and
    /// nothing underflows.
    #[test]
    fn the_cap_holds_under_any_operation_sequence(ops in proptest::collection::vec(op(), 0..200)) {
        let limits = Limits::strict().with_alloc_total(1 << 16).with_alloc_single(1 << 12);
        let cap = limits.max_alloc_total;
        let single = limits.max_alloc_single;
        let mut b = Budget::new(limits);

        for op in &ops {
            match *op {
                Op::Charge(n) => { let _ = b.charge(n); }
                Op::Release(n) => b.release(n),
                Op::ReserveAndCommit(n) => {
                    if let Ok(r) = b.reserve(n) { r.commit(); }
                }
                Op::ReserveAndDrop(n) => { let _ = b.reserve(n); }
                Op::Fuel(n) => { let _ = b.consume_fuel(n); }
            }
            prop_assert!(b.committed() + b.pending() <= cap);
            prop_assert!(b.available() <= cap);
            prop_assert_eq!(b.pending(), 0, "no reservation outlives its statement");
            prop_assert!(b.peak() >= b.committed());
        }
        // A single allocation over the per-allocation cap is always refused.
        prop_assert!(b.charge(single + 1).is_err());
    }

    /// An `IncrementalVec` never charges more than a constant factor over what
    /// was actually delivered, however large the declared size.
    #[test]
    fn incremental_charges_track_delivery(
        chunks in proptest::collection::vec(0usize..64, 0..64),
        declared in 0u32..u32::MAX,
    ) {
        let mut b = Budget::new(Limits::permissive());
        let declared = declared as usize;
        let mut v: IncrementalVec<u8> = IncrementalVec::new(declared);
        let mut delivered = 0usize;
        for &n in &chunks {
            let src = vec![0u8; n];
            if v.push_slice(&mut b, &src).is_ok() {
                delivered += n;
            }
        }
        prop_assert_eq!(v.len(), delivered);
        // Geometric growth from a 32-byte floor: at most 2x plus the floor.
        prop_assert!(
            v.charged() <= (delivered as u64) * 2 + 64,
            "charged {} for {} delivered",
            v.charged(),
            delivered
        );
    }

    /// Fuel accounting is exact and monotone.
    #[test]
    fn fuel_is_monotone(costs in proptest::collection::vec(0u64..100, 0..200), budget in 0u64..5000) {
        let mut b = Budget::new(Limits::strict().with_fuel(budget));
        let mut expected = 0u64;
        for &c in &costs {
            expected = expected.saturating_add(c);
            let ok = b.consume_fuel(c).is_ok();
            prop_assert_eq!(b.fuel_spent(), expected);
            prop_assert_eq!(ok, expected <= budget);
        }
    }
}
