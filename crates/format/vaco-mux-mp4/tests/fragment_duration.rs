use vaco_core::{Duration, Rational};
use vaco_mux_mp4::MuxOptions;
use vaco_mux_mp4::fragmented::{FragmentedState, buffer_sample, should_flush};

#[test]
fn duration_threshold_is_seconds_not_a_native_tick_count() {
    let opts = MuxOptions {
        frag_duration: Some(Duration::from_micros(1_000_000)),
        ..MuxOptions::default()
    };
    for rate in [25, 90_000] {
        let time_base = Rational::new(1, rate);
        let rate = i64::from(rate);
        let start = 2 * rate;
        let mut state = FragmentedState::new(1);
        buffer_sample(&mut state, 0, vec![1], start, 0, false, 1);
        assert!(!should_flush(
            &state,
            &opts,
            0,
            start + rate - 1,
            time_base,
            false
        ));
        assert!(should_flush(
            &state,
            &opts,
            0,
            start + rate,
            time_base,
            false
        ));
        assert!(!should_flush(
            &state,
            &opts,
            1,
            start + rate,
            time_base,
            false
        ));
    }
}
