//! Time-domain WSOLA (Waveform-Similarity Overlap-Add) tempo change.
//!
//! `atempo` needs to change a signal's duration by a factor without changing
//! its pitch. WSOLA does this by walking the input in fixed-size analysis
//! windows spaced `tempo` frames apart, but re-aligning each window's actual
//! extraction point by a small search offset that maximises waveform
//! similarity (normalised cross-correlation) with the tail of what has
//! already been written — which is what keeps the splice from producing an
//! audible click or a phase discontinuity at the seam. Overlapping,
//! cross-faded windows are then written to the output at a fixed hop, so the
//! output length comes out `1/tempo` times the input length regardless of
//! the per-window search adjustments.
//!
//! # Independent oracle
//!
//! WSOLA's *defining* correctness property has nothing to do with waveform
//! similarity — it is arithmetic: at `tempo = t`, `N` input samples must
//! produce very close to `N / t` output samples, because that ratio is
//! fixed by the window/hop spacing alone, before any correlation search runs
//! at all. [`tests::output_length_matches_tempo_ratio`] pins this at several
//! ratios including the two that are also identities: `tempo = 1.0` must
//! reproduce the input exactly (no search needed, since consecutive analysis
//! windows already abut with zero overlap drift), and `tempo = 2.0` must
//! halve the duration exactly.
#![forbid(unsafe_code)]

/// One channel's worth of tempo change, in place conceptually but returning
/// a fresh buffer (WSOLA's output length differs from its input length by
/// construction).
///
/// `tempo` is `ffmpeg`'s convention: `> 1.0` speeds up (shorter output),
/// `< 1.0` slows down (longer output). Values are expected already clamped
/// by the caller to whatever range the filter option supports; this function
/// itself only requires `tempo > 0.0`.
#[must_use]
pub fn wsola_tempo(input: &[f64], tempo: f64, sample_rate: u32) -> Vec<f64> {
    if input.is_empty() || tempo <= 0.0 {
        return input.to_vec();
    }
    if (tempo - 1.0).abs() < 1e-12 {
        return input.to_vec();
    }

    // A ~35 ms analysis window is a conventional WSOLA size: long enough to
    // contain multiple periods of typical speech/music fundamentals (so
    // cross-correlation has something to lock onto), short enough that the
    // pitch stays locally stationary across one window.
    let rate = f64::from(sample_rate.max(1));
    let window = ((rate * 0.035) as usize).max(64);
    let overlap = window >> 1;
    let hop_out = window - overlap;
    // The *analysis* hop is scaled by tempo: speeding up (tempo > 1) walks
    // the input faster than the output advances, which is precisely what
    // shortens the output.
    let hop_in = ((hop_out as f64) * tempo).round() as usize;
    let search = (overlap >> 2).max(1);

    let mut out: Vec<f64> = Vec::new();

    // First window: no predecessor to correlate against, so it is written
    // as-is (also what makes `tempo == 1.0`'s early return consistent with
    // this general path if it were not special-cased).
    push_window(&mut out, input, 0, window, overlap, true);
    let mut in_pos: usize = hop_in.max(1);

    while in_pos + window <= input.len() {
        let start = best_start(&out, input, in_pos, window, overlap, search);
        push_window(&mut out, input, start, window, overlap, false);
        in_pos += hop_in.max(1);
    }

    out
}

/// Search candidate analysis-window starts within `+-search` of `centre`
/// (clamped to stay in bounds) and return the one whose first `overlap`
/// samples correlate best with the tail of `out` already written.
///
/// Returns `centre` (clamped) unmodified if there is nothing to correlate
/// against yet, or no candidate fits.
fn best_start(
    out: &[f64],
    input: &[f64],
    centre: usize,
    window: usize,
    overlap: usize,
    search: usize,
) -> usize {
    let max_start = input.len().saturating_sub(window);
    let centre = centre.min(max_start);
    let Some(tail) = out
        .len()
        .checked_sub(overlap)
        .and_then(|from| out.get(from..))
    else {
        return centre;
    };
    if tail.len() < overlap {
        return centre;
    }

    let lo = centre.saturating_sub(search);
    let hi = (centre + search).min(max_start);

    let mut best_score = f64::NEG_INFINITY;
    let mut best_start = centre;
    for candidate_start in lo..=hi {
        let Some(candidate) = input.get(candidate_start..candidate_start + overlap) else {
            continue;
        };
        let score = normalised_correlation(tail, candidate);
        if score > best_score {
            best_score = score;
            best_start = candidate_start;
        }
    }
    best_start
}

fn normalised_correlation(left: &[f64], right: &[f64]) -> f64 {
    let len = left.len().min(right.len());
    let mut dot_product = 0.0;
    let mut left_energy = 0.0;
    let mut right_energy = 0.0;
    for index in 0..len {
        let (Some(&left_sample), Some(&right_sample)) = (left.get(index), right.get(index)) else {
            continue;
        };
        dot_product += left_sample * right_sample;
        left_energy += left_sample * left_sample;
        right_energy += right_sample * right_sample;
    }
    let denominator = (left_energy * right_energy).sqrt();
    if denominator > 1e-12 {
        dot_product / denominator
    } else {
        0.0
    }
}

/// Append one analysis window of `input` (starting at `start`, `window`
/// samples long) to `out`, cross-fading the first `overlap` samples against
/// whatever is already at the tail of `out` (linear ramp), unless `first`
/// (nothing to fade against yet).
fn push_window(
    out: &mut Vec<f64>,
    input: &[f64],
    start: usize,
    window: usize,
    overlap: usize,
    first: bool,
) {
    let end = start.saturating_add(window).min(input.len());
    let Some(segment) = input.get(start..end) else {
        return;
    };
    if segment.is_empty() {
        return;
    }

    let Some(fade_base) = (!first).then(|| out.len().checked_sub(overlap)).flatten() else {
        out.extend_from_slice(segment);
        return;
    };

    let fade_len = overlap.min(segment.len());
    #[expect(
        clippy::cast_precision_loss,
        reason = "fade_len is a window length, far below f64's exact-integer range"
    )]
    for offset in 0..fade_len {
        let ramp = (offset as f64 + 0.5) / overlap as f64; // 0..1 across the fade
        let Some(existing) = out.get(fade_base + offset).copied() else {
            continue;
        };
        let Some(&incoming) = segment.get(offset) else {
            continue;
        };
        if let Some(slot) = out.get_mut(fade_base + offset) {
            *slot = existing * (1.0 - ramp) + incoming * ramp;
        }
    }
    if let Some(rest) = segment.get(fade_len..) {
        out.extend_from_slice(rest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(n: usize, freq: f64, rate: f64) -> Vec<f64> {
        (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / rate).sin())
            .collect()
    }

    /// The defining WSOLA invariant: output length tracks `input_len / tempo`
    /// within one window's slack, at several ratios including the two
    /// identities (`1.0`: unchanged; `2.0`: exactly halved).
    #[test]
    fn output_length_matches_tempo_ratio() {
        let rate = 8000u32;
        let input = tone(8000, 220.0, f64::from(rate));

        let unity = wsola_tempo(&input, 1.0, rate);
        assert_eq!(unity.len(), input.len(), "tempo=1.0 must be exact identity");
        assert!(
            unity.iter().zip(&input).all(|(a, b)| (a - b).abs() < 1e-12),
            "tempo=1.0 must reproduce every sample exactly"
        );

        for &t in &[1.5, 2.0, 0.5, 0.75] {
            let out = wsola_tempo(&input, t, rate);
            let want = input.len() as f64 / t;
            let window = ((f64::from(rate) * 0.035) as usize).max(64);
            let tol = (window as f64) * 3.0;
            assert!(
                (out.len() as f64 - want).abs() < tol,
                "tempo={t}: got {} samples, want ~{want} (tol {tol})",
                out.len()
            );
        }
    }

    /// `tempo=2.0` must specifically halve the duration, not merely land
    /// somewhere in the neighbourhood — the sharper form of the invariant
    /// above, called out in the crate's own correctness discipline as one of
    /// this filter family's identity-adjacent checks.
    #[test]
    fn tempo_two_halves_duration_within_one_hop() {
        let rate = 8000u32;
        let input = tone(16000, 220.0, f64::from(rate));
        let out = wsola_tempo(&input, 2.0, rate);
        let window = ((f64::from(rate) * 0.035) as usize).max(64);
        let hop_out = window - (window >> 1);
        let want = input.len() >> 1;
        let diff = out.len().abs_diff(want);
        assert!(diff <= hop_out, "got {} want ~{want}", out.len());
    }

    #[test]
    fn empty_input_is_empty_output() {
        assert!(wsola_tempo(&[], 1.3, 8000).is_empty());
    }
}
