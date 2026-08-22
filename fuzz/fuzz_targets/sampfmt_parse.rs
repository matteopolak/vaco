//! Sample-format naming and the audio buffer arithmetic.
//!
//! Two untrusted inputs in one target. The name arrives from a command line;
//! the channel and sample counts arrive from a container header, and they are
//! multiplied together to size an allocation — which is the bug class D6 names
//! for a safe-Rust media stack. A wrap here would under-allocate a buffer the
//! caller then believes is big enough.
//! fuzz-crate: vaco-sampfmt
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_sampfmt::SampleFmt;

#[derive(Arbitrary, Debug)]
struct Input<'a> {
    name: &'a str,
    format: u8,
    channels: u32,
    samples: u32,
}

fuzz_target!(|input: Input<'_>| {
    // Parsing never panics, and never invents a format: there are no aliases,
    // so a successful parse must echo its own input back.
    if let Ok(fmt) = SampleFmt::from_name(input.name) {
        assert_eq!(fmt.name(), input.name);
    }

    let Some(&fmt) = SampleFmt::ALL.get(input.format as usize % SampleFmt::ALL.len()) else {
        return;
    };

    let planes = fmt.plane_count(input.channels);
    assert_eq!(planes, if fmt.is_planar() { input.channels } else { 1 });

    // Either the size is reported, or it is refused. It is never wrong.
    if let Some(plane) = fmt.plane_size(input.channels, input.samples) {
        let per_frame = if fmt.is_planar() {
            1
        } else {
            u64::from(input.channels)
        };
        let expected = fmt.bytes_per_sample() as u128
            * u128::from(per_frame)
            * u128::from(input.samples);
        assert_eq!(u128::from(plane as u64), expected);

        if let Ok(total) = fmt.buffer_size(input.channels, input.samples) {
            assert_eq!(
                u128::from(total as u64),
                u128::from(plane as u64) * u128::from(planes.max(1))
            );
        }
    }
});
