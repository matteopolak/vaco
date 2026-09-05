use vaco_core::{Duration, Rational, Timestamp};
use vaco_format_core::{Chapter, time::duration_from_rate};

#[test]
fn rate_duration_retains_ntsc_and_submicrosecond_periods() {
    assert_eq!(
        duration_from_rate(Rational::new(30_000, 1001)).map(Duration::as_ratio),
        Some((1001, 30_000))
    );
    assert_eq!(
        duration_from_rate(Rational::new(28_224_000, 1)).map(Duration::as_ratio),
        Some((1, 28_224_000))
    );
}

#[test]
fn chapter_duration_subtracts_exact_seconds() {
    let chapter = Chapter {
        id: 0,
        time_base: Rational::new(1, 28_224_000),
        start: Timestamp::new(1),
        end: Timestamp::new(2),
        metadata: Vec::new(),
    };
    assert_eq!(
        chapter.duration().map(Duration::as_ratio),
        Some((1, 28_224_000))
    );
}
