//! Interleave / deinterleave between one packed buffer and N per-channel
//! planar slices.
//!
//! A decoder whose internal state is naturally per-channel (each channel's
//! predictor, transform or filter history lives in its own array) needs
//! this step once, right before handing samples to an output buffer that
//! expects an interleaved layout — or, for a planar output format, needs
//! nothing at all, which is exactly why this is a separate, optional step
//! rather than baked into every conversion function in `convert.rs`.

// `dst.len() / n_ch` and `src.len() / n_ch` both divide by a channel count
// already checked non-zero by an early return in the same function.
#![allow(
    clippy::integer_division,
    reason = "divisor is a channel count already checked non-zero"
)]

/// Interleave `channels.len()` per-channel slices into `dst`, `dst[i *
/// channels.len() + c] = channels[c][i]`.
///
/// Processes `min(dst.len() / channels.len(), each channel's length)`
/// frames. `channels` may be empty, in which case nothing is written.
pub fn interleave_f32(dst: &mut [f32], channels: &[&[f32]]) {
    interleave_generic(dst, channels, |v| v);
}

/// The inverse of [`interleave_f32`]: `channels[c][i] = src[i *
/// channels.len() + c]`.
pub fn deinterleave_f32(channels: &mut [&mut [f32]], src: &[f32]) {
    deinterleave_generic(channels, src, |v| v);
}

/// Interleave `channels.len()` per-channel `i16` slices into `dst`.
pub fn interleave_i16(dst: &mut [i16], channels: &[&[i16]]) {
    interleave_generic(dst, channels, |v| v);
}

/// The inverse of [`interleave_i16`].
pub fn deinterleave_i16(channels: &mut [&mut [i16]], src: &[i16]) {
    deinterleave_generic(channels, src, |v| v);
}

fn interleave_generic<T: Copy>(dst: &mut [T], channels: &[&[T]], id: impl Fn(T) -> T) {
    let n_ch = channels.len();
    if n_ch == 0 {
        return;
    }
    let frames = channels
        .iter()
        .map(|c| c.len())
        .min()
        .unwrap_or(0)
        .min(dst.len() / n_ch);
    for (frame, out_frame) in dst.chunks_exact_mut(n_ch).take(frames).enumerate() {
        for (c, slot) in out_frame.iter_mut().enumerate() {
            if let Some(v) = channels.get(c).and_then(|ch| ch.get(frame)) {
                *slot = id(*v);
            }
        }
    }
}

fn deinterleave_generic<T: Copy>(channels: &mut [&mut [T]], src: &[T], id: impl Fn(T) -> T) {
    let n_ch = channels.len();
    if n_ch == 0 {
        return;
    }
    let frames = channels
        .iter()
        .map(|c| c.len())
        .min()
        .unwrap_or(0)
        .min(src.len() / n_ch);
    for (frame, in_frame) in src.chunks_exact(n_ch).take(frames).enumerate() {
        for (c, v) in in_frame.iter().enumerate() {
            if let Some(slot) = channels.get_mut(c).and_then(|ch| ch.get_mut(frame)) {
                *slot = id(*v);
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "these conversions are pure data movement and are defined to be bit-exact"
)]
mod tests {
    use super::*;

    #[test]
    fn interleave_stereo_matches_hand_computed() {
        let left = [1.0f32, 2.0, 3.0];
        let right = [10.0f32, 20.0, 30.0];
        let mut dst = [0.0f32; 6];
        interleave_f32(&mut dst, &[&left, &right]);
        assert_eq!(dst, [1.0, 10.0, 2.0, 20.0, 3.0, 30.0]);
    }

    #[test]
    fn deinterleave_stereo_matches_hand_computed() {
        let src = [1.0f32, 10.0, 2.0, 20.0, 3.0, 30.0];
        let mut left = [0.0f32; 3];
        let mut right = [0.0f32; 3];
        {
            let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
            deinterleave_f32(&mut channels, &src);
        }
        assert_eq!(left, [1.0, 2.0, 3.0]);
        assert_eq!(right, [10.0, 20.0, 30.0]);
    }

    #[test]
    fn interleave_deinterleave_roundtrip_i16() {
        let ch0 = [1i16, 2, 3, 4];
        let ch1 = [-1i16, -2, -3, -4];
        let ch2 = [100i16, 200, 300, 400];
        let mut packed = [0i16; 12];
        interleave_i16(&mut packed, &[&ch0, &ch1, &ch2]);

        let mut o0 = [0i16; 4];
        let mut o1 = [0i16; 4];
        let mut o2 = [0i16; 4];
        {
            let mut channels: [&mut [i16]; 3] = [&mut o0, &mut o1, &mut o2];
            deinterleave_i16(&mut channels, &packed);
        }
        assert_eq!(o0, ch0);
        assert_eq!(o1, ch1);
        assert_eq!(o2, ch2);
    }

    #[test]
    fn zero_channels_is_a_no_op_not_a_panic() {
        let mut dst = [1.0f32, 2.0];
        interleave_f32(&mut dst, &[]);
        assert_eq!(dst, [1.0, 2.0]);
    }

    #[test]
    fn mismatched_channel_lengths_truncate_to_the_shortest() {
        let ch0 = [1.0f32, 2.0, 3.0];
        let ch1 = [10.0f32, 20.0]; // shorter
        let mut dst = [9.0f32; 6];
        interleave_f32(&mut dst, &[&ch0, &ch1]);
        // Only 2 frames worth get written; the rest keep their initial value.
        assert_eq!(dst, [1.0, 10.0, 2.0, 20.0, 9.0, 9.0]);
    }

    #[test]
    fn undersized_dst_writes_only_what_fits() {
        let ch0 = [1.0f32, 2.0, 3.0];
        let ch1 = [10.0f32, 20.0, 30.0];
        let mut dst = [0.0f32; 3]; // room for 1 frame only
        interleave_f32(&mut dst, &[&ch0, &ch1]);
        assert_eq!(dst, [1.0, 10.0, 0.0]);
    }

    fn same_bits_or_both_nan(a: f32, b: f32) -> bool {
        a.is_nan() && b.is_nan() || a == b
    }

    proptest::proptest! {
        #[test]
        fn interleave_deinterleave_roundtrip_any_shape(
            a in proptest::collection::vec(proptest::num::f32::ANY, 0..16),
            b in proptest::collection::vec(proptest::num::f32::ANY, 0..16),
        ) {
            let n = a.len().min(b.len());
            let a: Vec<f32> = a.into_iter().take(n).collect();
            let b: Vec<f32> = b.into_iter().take(n).collect();
            let mut packed = vec![0.0f32; n * 2];
            interleave_f32(&mut packed, &[a.as_slice(), b.as_slice()]);

            let mut oa = vec![0.0f32; n];
            let mut ob = vec![0.0f32; n];
            {
                let mut channels: [&mut [f32]; 2] = [&mut oa, &mut ob];
                deinterleave_f32(&mut channels, &packed);
            }
            for (expect, got) in a.iter().zip(oa.iter()) {
                proptest::prop_assert!(same_bits_or_both_nan(*expect, *got));
            }
            for (expect, got) in b.iter().zip(ob.iter()) {
                proptest::prop_assert!(same_bits_or_both_nan(*expect, *got));
            }
        }
    }
}
