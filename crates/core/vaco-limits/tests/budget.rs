//! [`Budget`], [`Reservation`], [`IncrementalVec`], fuel and [`ProgressGuard`].
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "test code: a panic is the assertion mechanism"
)]

use vaco_limits::{Budget, IncrementalVec, LimitError, Limits, ProgressGuard};

#[test]
fn alloc_charges_and_release_credits() {
    let mut b = Budget::new(Limits::strict());
    let v: Vec<u8> = b.alloc(1024).unwrap();
    assert_eq!(v.len(), 1024);
    assert_eq!(b.committed(), 1024);
    assert_eq!(b.pending(), 0);
    assert_eq!(b.peak(), 1024);

    b.release(1024);
    assert_eq!(b.committed(), 0);
    // The high-water mark is history and does not move back.
    assert_eq!(b.peak(), 1024);

    // Over-releasing saturates rather than underflowing.
    b.release(u64::MAX);
    assert_eq!(b.committed(), 0);
}

#[test]
fn element_size_is_accounted_for() {
    let mut b = Budget::new(Limits::strict());
    let v: Vec<u32> = b.alloc(100).unwrap();
    assert_eq!(v.len(), 100);
    assert_eq!(b.committed(), 400);

    // A count that overflows when scaled by the element size is an error, not a
    // wrap that would then pass the cap check.
    let mut b = Budget::new(Limits::permissive());
    assert_eq!(b.alloc::<u64>(usize::MAX), Err(LimitError::Overflow));
}

#[test]
fn both_caps_are_enforced() {
    let limits = Limits::strict()
        .with_alloc_single(1000)
        .with_alloc_total(1500);
    let mut b = Budget::new(limits);

    assert!(matches!(
        b.alloc::<u8>(1001),
        Err(LimitError::Exceeded {
            limit: "max_alloc_single",
            ..
        })
    ));
    let _a: Vec<u8> = b.alloc(900).unwrap();
    assert!(matches!(
        b.alloc::<u8>(900),
        Err(LimitError::Exceeded {
            limit: "max_alloc_total",
            ..
        })
    ));
    assert_eq!(b.available(), 600);
}

#[test]
fn a_dropped_reservation_releases_its_hold() {
    let mut b = Budget::new(Limits::strict().with_alloc_total(1000));
    {
        let r = b.reserve(600).unwrap();
        assert_eq!(r.bytes(), 600);
        // While it is held it counts against the total.
        assert_eq!(r.bytes(), 600);
    }
    assert_eq!(b.pending(), 0);
    assert_eq!(b.committed(), 0);
    // The whole budget is available again.
    assert!(b.reserve(1000).is_ok());
}

#[test]
fn a_held_reservation_counts_against_the_total() {
    let mut b = Budget::new(Limits::strict().with_alloc_total(1000));
    let r = b.reserve(600).unwrap();
    // A second reservation cannot ignore the first.
    r.commit();
    assert_eq!(b.committed(), 600);
    assert_eq!(b.pending(), 0);
    assert!(b.reserve(600).is_err());
    assert!(b.reserve(400).is_ok());
}

#[test]
fn a_reservation_cannot_allocate_more_than_it_reserved() {
    let mut b = Budget::new(Limits::strict());
    let r = b.reserve(100).unwrap();
    assert_eq!(r.alloc::<u8>(200), Err(LimitError::Overflow));
    // The failed attempt released the hold.
    assert_eq!(b.pending(), 0);
    assert_eq!(b.committed(), 0);

    let r = b.reserve(100).unwrap();
    let v: Vec<u8> = r.alloc(100).unwrap();
    assert_eq!(v.len(), 100);
    assert_eq!(b.committed(), 100);
}

#[test]
fn two_phase_reservation_defeats_declared_length_amplification() {
    // The scenario: a 16-byte file whose box header claims 4 GiB.
    let mut b = Budget::new(Limits::strict());
    let declared: u64 = 4 << 30;
    assert!(b.reserve(declared).is_err());
    assert_eq!(b.committed(), 0);
    assert_eq!(b.pending(), 0);
}

#[test]
fn incremental_growth_tracks_delivery_not_declaration() {
    let mut b = Budget::new(Limits::strict());
    let mut v: IncrementalVec<u8> = IncrementalVec::new(1 << 30);
    for _ in 0..10 {
        v.push_slice(&mut b, &[7u8; 100]).unwrap();
    }
    assert_eq!(v.len(), 1000);
    assert!(v.as_slice().iter().all(|&x| x == 7));
    // Charged for what arrived (times the growth factor), never for the 1 GiB
    // the header claimed.
    assert!(v.charged() < 8192, "charged {}", v.charged());
    assert_eq!(v.charged(), b.committed());
    assert_eq!(v.declared(), 1 << 30);

    // Delivering more than declared is refused.
    let mut v: IncrementalVec<u8> = IncrementalVec::new(4);
    v.push_slice(&mut b, &[1, 2, 3, 4]).unwrap();
    assert!(matches!(
        v.push_slice(&mut b, &[5]),
        Err(LimitError::Exceeded {
            limit: "declared_size",
            ..
        })
    ));
    assert_eq!(v.into_vec(), vec![1, 2, 3, 4]);

    // Growth is still capped by the budget.
    let mut tiny = Budget::new(Limits::tiny().with_alloc_total(64));
    let mut v: IncrementalVec<u8> = IncrementalVec::new(1 << 20);
    assert!(v.push_slice(&mut tiny, &[0u8; 4096]).is_err());
}

#[test]
fn fuel_is_deterministic_and_sticky() {
    let mut b = Budget::new(Limits::strict().with_fuel(100));
    for _ in 0..100 {
        b.consume_fuel(1).unwrap();
    }
    assert_eq!(b.fuel_remaining(), 0);
    assert!(matches!(
        b.consume_fuel(1),
        Err(LimitError::FuelExhausted { spent: 101 })
    ));
    // Still exhausted; a loop cannot spin its way back into credit.
    assert!(b.consume_fuel(0).is_err());

    b.refuel();
    assert_eq!(b.fuel_spent(), 0);
    assert!(b.consume_fuel(50).is_ok());

    // A single huge charge saturates rather than wrapping into credit.
    let mut b = Budget::new(Limits::strict().with_fuel(10));
    assert!(b.consume_fuel(u64::MAX).is_err());
    assert!(b.consume_fuel(u64::MAX).is_err());
}

#[test]
fn fuel_exhaustion_replays_identically() {
    // The property that makes a fuzz finding useful: same input, same point.
    let run = || {
        let mut b = Budget::new(Limits::strict().with_fuel(1000));
        let mut steps = 0u64;
        while b.consume_fuel(7).is_ok() {
            steps += 1;
        }
        (steps, b.fuel_spent())
    };
    assert_eq!(run(), run());
    assert_eq!(run().0, 142); // 142 * 7 = 994; the 143rd charge would reach 1001.
}

#[test]
fn frame_dimension_checks_are_up_front_and_checked() {
    let b = Budget::new(Limits::strict());
    assert_eq!(b.check_frame(1920, 1080, 4), Ok(8_294_400));
    assert!(b.check_frame(100_000, 1080, 4).is_err());
    assert!(b.check_frame(1920, 100_000, 4).is_err());
    // Within the dimension caps but over the frame cap.
    assert!(matches!(
        b.check_frame(8192, 8192, 4),
        Err(LimitError::Exceeded {
            limit: "max_frame_bytes",
            ..
        })
    ));
    // Overflow is caught before any cap.
    assert_eq!(
        b.check_frame(65_535, 65_535, u32::MAX),
        Err(LimitError::Exceeded {
            limit: "max_dimension (width)",
            requested: 65_535,
            cap: 8192
        })
    );
    let wide = Budget::new(Limits::permissive());
    assert_eq!(wide.check_frame(65_536, 65_536, u32::MAX), {
        // 65536 * 65536 * 4294967295 does fit in u64, so the frame cap catches it.
        Err(LimitError::Exceeded {
            limit: "max_frame_bytes",
            requested: 65_536u64 * 65_536 * u64::from(u32::MAX),
            cap: wide.limits().max_frame_bytes,
        })
    });
}

#[test]
fn named_count_checks_use_the_configured_caps() {
    let b = Budget::new(Limits::strict());
    assert!(b.check_streams(255).is_ok());
    assert!(b.check_streams(257).is_err());
    assert!(b.check_channels(64).is_ok());
    assert!(b.check_channels(65).is_err());
    assert!(b.check_sample_rate(48_000).is_ok());
    assert!(b.check_sample_rate(1 << 30).is_err());
    assert!(b.check_side_data(64).is_ok());
    assert!(b.check_side_data(65).is_err());
    assert!(b.check_probe_bytes(1 << 20).is_ok());
    assert!(b.check_probe_bytes(1 << 21).is_err());
    assert!(b.check_metadata_bytes(1 << 20).is_ok());
    assert!(b.check_metadata_bytes(1 << 21).is_err());
}

#[test]
fn deadline_is_checked_only_when_configured() {
    let b = Budget::new(Limits::strict());
    assert!(b.check_deadline().is_ok());

    let past = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(1))
        .unwrap();
    let b = Budget::new(Limits::strict().with_deadline(past));
    assert_eq!(b.check_deadline(), Err(LimitError::DeadlineExceeded));

    let future = std::time::Instant::now() + std::time::Duration::from_secs(3600);
    let b = Budget::new(Limits::strict().with_deadline(future));
    assert!(b.check_deadline().is_ok());
}

#[test]
fn progress_guard_tolerates_stalls_but_not_a_run_of_them() {
    let mut g = ProgressGuard::new();
    for _ in 0..1000 {
        g.tick(true).unwrap();
    }
    assert_eq!(g.stalls(), 0);
    for _ in 0..ProgressGuard::DEFAULT_MAX_STALLS {
        g.tick(false).unwrap();
    }
    assert!(matches!(g.tick(false), Err(LimitError::NoProgress { .. })));

    // A single success clears the run.
    let mut g = ProgressGuard::with_max_stalls(3);
    g.tick(false).unwrap();
    g.tick(false).unwrap();
    g.tick(true).unwrap();
    assert_eq!(g.stalls(), 0);
    g.tick(false).unwrap();
    g.reset();
    assert_eq!(g.stalls(), 0);
}

#[test]
fn progress_guard_derives_progress_from_position() {
    let mut g = ProgressGuard::with_max_stalls(2);
    g.tick_position(0).unwrap();
    g.tick_position(10).unwrap();
    g.tick_position(20).unwrap();
    // A component that claims to work while its cursor stands still.
    g.tick_position(20).unwrap();
    g.tick_position(20).unwrap();
    assert!(g.tick_position(20).is_err());

    // Going backwards is not progress either.
    let mut g = ProgressGuard::with_max_stalls(1);
    g.tick_position(100).unwrap();
    g.tick_position(50).unwrap();
    assert!(g.tick_position(10).is_err());
}

#[test]
fn limit_errors_map_onto_the_core_taxonomy() {
    let e: vaco_core::Error = LimitError::Exceeded {
        limit: "max_alloc_total",
        requested: 10,
        cap: 5,
    }
    .into();
    assert!(matches!(e, vaco_core::Error::LimitExceeded { .. }));
    assert!(!e.is_recoverable());

    for e in [
        LimitError::Overflow,
        LimitError::AllocFailed { bytes: 1 },
        LimitError::FuelExhausted { spent: 1 },
        LimitError::DeadlineExceeded,
        LimitError::NoProgress { ticks: 1 },
    ] {
        let mapped: vaco_core::Error = e.into();
        assert!(matches!(mapped, vaco_core::Error::LimitExceeded { .. }));
        // Every variant renders without panicking.
        assert!(!format!("{e}").is_empty());
    }
}

#[test]
fn the_presets_are_ordered() {
    let tiny = Limits::tiny();
    let strict = Limits::strict();
    let permissive = Limits::permissive();
    assert!(tiny.max_alloc_total < strict.max_alloc_total);
    assert!(strict.max_alloc_total < permissive.max_alloc_total);
    assert!(tiny.fuel < strict.fuel && strict.fuel < permissive.fuel);
    assert_eq!(Limits::default(), strict);
}
