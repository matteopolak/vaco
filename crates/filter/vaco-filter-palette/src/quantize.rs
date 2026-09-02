//! A shared median-cut colour quantiser (Heckbert 1982 — a well-documented,
//! public algorithm; implemented from general algorithmic knowledge, not
//! transcribed from any implementation's source, per D6/D7).
//!
//! Used by [`crate::palettegen`] (histogram accumulated across a whole
//! stream), [`crate::elbg`] (histogram from one frame) and
//! [`crate::paletteuse`] (nearest-colour lookup only, no histogram side).

use std::collections::HashMap;

/// An 8-bit RGB colour. No alpha — every caller in this crate quantises
/// colour only; alpha is handled separately (or ignored) by each filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// A colour histogram: exact 8-bit RGB keys, so it never merges two
/// genuinely distinct colours, at the cost of a map bounded by the number
/// of distinct colours actually seen (real video frames: typically
/// thousands, not the full 16.7M address space).
#[derive(Debug, Default)]
pub struct Histogram {
    counts: HashMap<(u8, u8, u8), u64>,
}

impl Histogram {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, r: u8, g: u8, b: u8) {
        let entry = self.counts.entry((r, g, b)).or_insert(0);
        *entry = entry.saturating_add(1);
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    r: u8,
    g: u8,
    b: u8,
    count: u64,
}

/// Reduce `hist` to at most `k` representative colours by recursive median
/// cut: repeatedly split the box with the widest single-channel range at
/// its weighted median, until there are `k` boxes or nothing left worth
/// splitting, then average each box's actual colours.
///
/// Returns fewer than `k` colours if the histogram itself has fewer than
/// `k` distinct colours (`elbg` on a two-colour frame with
/// `codebook_length=256` should not synthesise 254 nonexistent colours).
#[must_use]
pub fn median_cut(hist: &Histogram, k: usize) -> Vec<Rgb> {
    let entries: Vec<Entry> = hist
        .counts
        .iter()
        .map(|(&(r, g, b), &count)| Entry { r, g, b, count })
        .collect();
    if entries.is_empty() || k == 0 {
        return Vec::new();
    }
    let mut boxes: Vec<Vec<Entry>> = vec![entries];
    while boxes.len() < k {
        let mut best_i = None;
        let mut best_range = 0u32;
        let mut best_axis = 0usize;
        for (i, b) in boxes.iter().enumerate() {
            if b.len() <= 1 {
                continue;
            }
            let (axis, range) = box_range(b);
            if range > best_range {
                best_range = range;
                best_i = Some(i);
                best_axis = axis;
            }
        }
        let Some(i) = best_i else { break };
        if i >= boxes.len() {
            break;
        }
        let b = boxes.remove(i);
        let (b1, b2) = split_box(b, best_axis);
        let progress = !b1.is_empty() && !b2.is_empty();
        if !b1.is_empty() {
            boxes.push(b1);
        }
        if !b2.is_empty() {
            boxes.push(b2);
        }
        if !progress {
            // A degenerate split (every remaining entry identical along
            // every axis) would loop forever re-selecting the same box;
            // stop rather than spin.
            break;
        }
    }
    boxes.iter().map(|b| average_color(b)).collect()
}

fn box_range(entries: &[Entry]) -> (usize, u32) {
    let mut r_min = 255u8;
    let mut r_max = 0u8;
    let mut g_min = 255u8;
    let mut g_max = 0u8;
    let mut b_min = 255u8;
    let mut b_max = 0u8;
    for e in entries {
        r_min = r_min.min(e.r);
        r_max = r_max.max(e.r);
        g_min = g_min.min(e.g);
        g_max = g_max.max(e.g);
        b_min = b_min.min(e.b);
        b_max = b_max.max(e.b);
    }
    let r_range = u32::from(r_max) - u32::from(r_min);
    let g_range = u32::from(g_max) - u32::from(g_min);
    let b_range = u32::from(b_max) - u32::from(b_min);
    if r_range >= g_range && r_range >= b_range {
        (0, r_range)
    } else if g_range >= b_range {
        (1, g_range)
    } else {
        (2, b_range)
    }
}

/// Split `entries` (sorted along `axis`) into two halves at the weighted
/// median — the split point where the cumulative pixel count first reaches
/// half the box's total, not the midpoint by distinct-colour count. Two
/// colours where one is far more common should not necessarily land in
/// different halves.
fn split_box(mut entries: Vec<Entry>, axis: usize) -> (Vec<Entry>, Vec<Entry>) {
    match axis {
        0 => entries.sort_unstable_by_key(|e| e.r),
        1 => entries.sort_unstable_by_key(|e| e.g),
        _ => entries.sort_unstable_by_key(|e| e.b),
    }
    let total: u64 = entries.iter().map(|e| e.count).sum();
    let mut acc = 0u64;
    let mut split_at = entries.len();
    for (pos, e) in entries.iter().enumerate() {
        acc = acc.saturating_add(e.count);
        if acc.saturating_mul(2) >= total {
            split_at = pos.saturating_add(1);
            break;
        }
    }
    let n = entries.len();
    let split_at = split_at.clamp(1, n.saturating_sub(1).max(1));
    let second = entries.split_off(split_at.min(n));
    (entries, second)
}

fn average_color(entries: &[Entry]) -> Rgb {
    let mut rs = 0u64;
    let mut gs = 0u64;
    let mut bs = 0u64;
    let mut count = 0u64;
    for e in entries {
        rs = rs.saturating_add(u64::from(e.r).saturating_mul(e.count));
        gs = gs.saturating_add(u64::from(e.g).saturating_mul(e.count));
        bs = bs.saturating_add(u64::from(e.b).saturating_mul(e.count));
        count = count.saturating_add(e.count);
    }
    if count == 0 {
        return Rgb::default();
    }
    #[allow(
        clippy::integer_division,
        reason = "component averages of u8-derived sums, truncation is the intended rounding"
    )]
    let (r, g, b) = (rs / count, gs / count, bs / count);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "each average is a weighted mean of u8 values, so it stays within 0..=255"
    )]
    Rgb {
        r: r as u8,
        g: g as u8,
        b: b as u8,
    }
}

/// The index into `palette` whose colour is closest (squared Euclidean RGB
/// distance) to `color`. `0` if `palette` is empty — callers must check
/// `is_empty()` themselves before trusting the index.
#[must_use]
pub fn nearest_index(palette: &[Rgb], color: Rgb) -> usize {
    let mut best = 0usize;
    let mut best_dist = u32::MAX;
    for (i, p) in palette.iter().enumerate() {
        let dr = i32::from(p.r) - i32::from(color.r);
        let dg = i32::from(p.g) - i32::from(color.g);
        let db = i32::from(p.b) - i32::from(color.b);
        #[allow(
            clippy::cast_sign_loss,
            reason = "a sum of three squared i32 deltas is always non-negative"
        )]
        let dist = dr
            .saturating_mul(dr)
            .saturating_add(dg.saturating_mul(dg))
            .saturating_add(db.saturating_mul(db)) as u32;
        if dist < best_dist {
            best_dist = dist;
            best = i;
        }
    }
    best
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn empty_histogram_produces_no_colours() {
        let hist = Histogram::new();
        assert!(median_cut(&hist, 256).is_empty());
    }

    #[test]
    fn fewer_distinct_colours_than_k_does_not_fabricate_extras() {
        let mut hist = Histogram::new();
        hist.add(255, 0, 0);
        hist.add(0, 255, 0);
        let palette = median_cut(&hist, 256);
        assert_eq!(palette.len(), 2);
    }

    #[test]
    fn a_dominant_colour_is_preserved_almost_exactly_at_a_small_k() {
        let mut hist = Histogram::new();
        for _ in 0..1000 {
            hist.add(10, 20, 30);
        }
        hist.add(200, 200, 200);
        let palette = median_cut(&hist, 2);
        assert_eq!(palette.len(), 2);
        assert!(palette.iter().any(|c| c.r == 10 && c.g == 20 && c.b == 30));
    }

    #[test]
    fn nearest_index_picks_the_closest_entry() {
        let palette = [
            Rgb { r: 0, g: 0, b: 0 },
            Rgb {
                r: 255,
                g: 255,
                b: 255,
            },
        ];
        assert_eq!(
            nearest_index(
                &palette,
                Rgb {
                    r: 10,
                    g: 10,
                    b: 10
                }
            ),
            0
        );
        assert_eq!(
            nearest_index(
                &palette,
                Rgb {
                    r: 250,
                    g: 250,
                    b: 250
                }
            ),
            1
        );
    }
}
