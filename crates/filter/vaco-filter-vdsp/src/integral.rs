//! Integral images (summed-area tables): O(1) rectangle-sum queries after an
//! O(`w*h`) build, the standard alternative to [`crate::boxblur`]'s running
//! sum for callers that need many different, possibly overlapping rectangle
//! sums over the *same* plane rather than one full-plane blur (a guided
//! filter's local mean/variance, an adaptive-threshold window, `smartblur`'s
//! per-pixel local statistics).

/// A summed-area table over one `width × height` 8-bit plane.
///
/// `table[y][x] = sum of every sample at (x' <= x, y' <= y)`, stored with a
/// one-row/one-column zero border so every real cell's rectangle sum is a
/// plain four-term lookup with no edge special-casing.
#[derive(Debug, Clone)]
pub struct Integral {
    data: Vec<u64>,
    stride: usize,
    width: usize,
    height: usize,
}

impl Integral {
    /// Build the table from a row-major `width × height` plane.
    #[must_use]
    pub fn build(plane: &[u8], width: usize, height: usize) -> Self {
        let stride = width.saturating_add(1);
        let mut data = vec![0u64; stride.saturating_mul(height.saturating_add(1))];
        for y in 0..height {
            let mut row_sum: u64 = 0;
            for x in 0..width {
                let sample = plane.get(y.saturating_mul(width).saturating_add(x)).copied().unwrap_or(0);
                row_sum = row_sum.saturating_add(u64::from(sample));
                let above = data.get(y.saturating_mul(stride).saturating_add(x + 1)).copied().unwrap_or(0);
                if let Some(cell) = data.get_mut((y + 1).saturating_mul(stride).saturating_add(x + 1)) {
                    *cell = row_sum.saturating_add(above);
                }
            }
        }
        Self { data, stride, width, height }
    }

    #[must_use]
    fn at(&self, x: usize, y: usize) -> u64 {
        self.data.get(y.saturating_mul(self.stride).saturating_add(x)).copied().unwrap_or(0)
    }

    /// Sum of the rectangle `(x, y)..(x+w, y+h)`, clipped to the table's own
    /// bounds. A rectangle entirely outside the table sums to `0`.
    #[must_use]
    pub fn rect_sum(&self, x: usize, y: usize, w: usize, h: usize) -> u64 {
        let x0 = x.min(self.width);
        let y0 = y.min(self.height);
        let x1 = x.saturating_add(w).min(self.width);
        let y1 = y.saturating_add(h).min(self.height);
        if x1 <= x0 || y1 <= y0 {
            return 0;
        }
        // Inclusion-exclusion on the four corner prefix sums.
        self.at(x1, y1) + self.at(x0, y0) - self.at(x1, y0) - self.at(x0, y1)
    }

    /// The rectangle's arithmetic mean, or `0` for a zero-area rectangle.
    #[must_use]
    #[allow(clippy::integer_division, reason = "arithmetic mean over an integer sample count")]
    pub fn rect_mean(&self, x: usize, y: usize, w: usize, h: usize) -> f64 {
        let area = w.saturating_mul(h);
        if area == 0 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss, reason = "display-scale mean; plane sizes are far below 2^53")]
        {
            self.rect_sum(x, y, w, h) as f64 / area as f64
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn full_rect_sum_equals_the_plain_sum() {
        let data = [1u8, 2, 3, 4, 5, 6]; // 3x2
        let table = Integral::build(&data, 3, 2);
        let expected: u64 = data.iter().map(|&v| u64::from(v)).sum();
        assert_eq!(table.rect_sum(0, 0, 3, 2), expected);
    }

    #[test]
    fn single_pixel_rects_match_the_source_sample() {
        let data = [10u8, 20, 30, 40];
        let table = Integral::build(&data, 2, 2);
        assert_eq!(table.rect_sum(0, 0, 1, 1), 10);
        assert_eq!(table.rect_sum(1, 0, 1, 1), 20);
        assert_eq!(table.rect_sum(0, 1, 1, 1), 30);
        assert_eq!(table.rect_sum(1, 1, 1, 1), 40);
    }

    #[test]
    fn disjoint_quadrants_sum_to_the_whole() {
        let width = 4;
        let height = 4;
        let data: Vec<u8> = (1..=16).collect();
        let table = Integral::build(&data, width, height);
        let quadrants = table.rect_sum(0, 0, 2, 2)
            + table.rect_sum(2, 0, 2, 2)
            + table.rect_sum(0, 2, 2, 2)
            + table.rect_sum(2, 2, 2, 2);
        assert_eq!(quadrants, table.rect_sum(0, 0, 4, 4));
    }

    #[test]
    fn out_of_bounds_rect_clips_rather_than_panics() {
        let data = [5u8; 4];
        let table = Integral::build(&data, 2, 2);
        assert_eq!(table.rect_sum(10, 10, 5, 5), 0);
        assert_eq!(table.rect_sum(1, 1, 100, 100), 5); // clips to the single in-bounds cell
    }

    #[test]
    fn rect_mean_of_a_uniform_plane_is_that_value() {
        let data = [42u8; 9];
        let table = Integral::build(&data, 3, 3);
        assert!((table.rect_mean(0, 0, 3, 3) - 42.0).abs() < 1e-9);
        assert!((table.rect_mean(5, 5, 0, 0) - 0.0).abs() < 1e-9);
    }
}
