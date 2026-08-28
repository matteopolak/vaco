//! A borrowed, strided 8-bit sample view.
//!
//! Every comparison kernel in this crate reads two same-shaped blocks — a
//! current-frame block and a candidate reference block — out of larger
//! strided buffers (a decoded picture, a reconstructed reference frame).
//! [`Plane`] is the minimal borrow that lets a kernel walk rows without
//! copying, and without ever indexing past the end of the underlying buffer:
//! every accessor here returns a possibly-short or possibly-empty slice
//! rather than panicking, so a caller that mis-sizes a block gets a smaller
//! (still meaningful) answer instead of a crash. That matters because a
//! motion search tries hundreds of caller-computed candidate offsets per
//! block, some of which are legitimately close to a frame edge.

/// A borrowed view of one strided 8-bit plane, or a sub-block of one.
///
/// `stride` is the byte distance between the start of one row and the next
/// in `data`; it may exceed `width` (padding) but a well-formed view never
/// has `width` greater than `stride` — see [`Plane::sub`]'s doc for what
/// happens if a caller violates that.
#[derive(Debug, Clone, Copy)]
pub struct Plane<'a> {
    data: &'a [u8],
    stride: usize,
    width: usize,
    height: usize,
}

impl<'a> Plane<'a> {
    /// A view over a whole buffer: `height` rows of `stride` bytes each,
    /// `width` of which (per row, from the left) are meaningful samples.
    #[must_use]
    pub const fn new(data: &'a [u8], stride: usize, width: usize, height: usize) -> Self {
        Self {
            data,
            stride,
            width,
            height,
        }
    }

    /// Sample width of this view.
    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    /// Row count of this view.
    #[must_use]
    pub const fn height(&self) -> usize {
        self.height
    }

    /// Row `y`, truncated to at most [`Plane::width`] bytes.
    ///
    /// Returns a shorter slice — never panics — when `y` is out of range,
    /// when the arithmetic to locate the row would overflow, or when the
    /// backing buffer is shorter than the view claims (a caller passed a
    /// truncated or mis-strided buffer). A short result degrades whatever
    /// sums over it rather than stopping the caller.
    #[must_use]
    pub fn row(&self, y: usize) -> &'a [u8] {
        if y >= self.height {
            return &[];
        }
        let Some(start) = y.checked_mul(self.stride) else {
            return &[];
        };
        let Some(rest) = self.data.get(start..) else {
            return &[];
        };
        rest.get(..self.width).unwrap_or(rest)
    }

    /// A sub-block view: `w` × `h` samples starting at `(x, y)` in this
    /// view's own coordinate space.
    ///
    /// Returns `None` when the requested block's last row would read past
    /// the end of the backing buffer, or when the offset/size arithmetic
    /// overflows — the two ways a caller-computed motion-vector candidate
    /// can be out of bounds. `stride` is inherited unchanged, so `x + w`
    /// exceeding the parent's `width` (but not its `stride`) is not checked
    /// here: it only means the sub-block reads into what the parent
    /// considered padding, not past the buffer, and [`Plane::row`]'s own
    /// bound is what actually protects memory safety.
    #[must_use]
    pub fn sub(&self, x: usize, y: usize, w: usize, h: usize) -> Option<Plane<'a>> {
        if w == 0 || h == 0 {
            return Some(Plane {
                data: &[],
                stride: self.stride,
                width: 0,
                height: 0,
            });
        }
        let row_start = y.checked_mul(self.stride)?.checked_add(x)?;
        let last_row_start = h
            .checked_sub(1)?
            .checked_mul(self.stride)?
            .checked_add(row_start)?;
        let last_row_end = last_row_start.checked_add(w)?;
        if last_row_end > self.data.len() {
            return None;
        }
        let data = self.data.get(row_start..)?;
        Some(Plane {
            data,
            stride: self.stride,
            width: w,
            height: h,
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a panic is the assertion mechanism"
)]
mod tests {
    use super::*;

    fn buf() -> Vec<u8> {
        (0u8..40).collect() // 5 rows of 8
    }

    #[test]
    fn row_reads_the_declared_width_at_the_right_offset() {
        let data = buf();
        let p = Plane::new(&data, 8, 5, 5);
        assert_eq!(p.row(0), &[0, 1, 2, 3, 4]);
        assert_eq!(p.row(1), &[8, 9, 10, 11, 12]);
        assert_eq!(p.row(4), &[32, 33, 34, 35, 36]);
    }

    #[test]
    fn row_out_of_range_is_empty_not_a_panic() {
        let data = buf();
        let p = Plane::new(&data, 8, 5, 5);
        assert_eq!(p.row(5), &[] as &[u8]);
        assert_eq!(p.row(usize::MAX), &[] as &[u8]);
    }

    #[test]
    fn row_on_a_truncated_buffer_degrades_rather_than_panics() {
        let data = [0u8; 10]; // claims 5x5 with stride 8, but only 10 bytes exist
        let p = Plane::new(&data, 8, 5, 5);
        assert_eq!(p.row(0).len(), 5);
        assert_eq!(p.row(1).len(), 2); // only 2 bytes left after row 0's 8-byte stride
        assert_eq!(p.row(2).len(), 0);
    }

    #[test]
    fn sub_reads_a_shifted_window_with_the_parent_stride() {
        let data = buf();
        let p = Plane::new(&data, 8, 8, 5);
        let s = p.sub(2, 1, 3, 2).unwrap();
        assert_eq!(s.row(0), &[10, 11, 12]);
        assert_eq!(s.row(1), &[18, 19, 20]);
    }

    #[test]
    fn sub_past_the_buffer_end_is_none() {
        let data = buf();
        let p = Plane::new(&data, 8, 8, 5);
        assert!(p.sub(6, 4, 4, 1).is_none()); // row 4 only has 2 bytes left after col 6
        assert!(p.sub(0, 0, 1, 100).is_none());
    }

    #[test]
    fn sub_with_a_zero_dimension_is_a_defined_empty_view() {
        let data = buf();
        let p = Plane::new(&data, 8, 8, 5);
        let s = p.sub(0, 0, 0, 3).unwrap();
        assert_eq!(s.width(), 0);
        assert_eq!(s.row(0), &[] as &[u8]);
    }

    #[test]
    fn overflowing_offsets_are_none_not_a_panic() {
        let data = buf();
        let p = Plane::new(&data, 8, 8, 5);
        assert!(p.sub(usize::MAX, usize::MAX, 4, 4).is_none());
        assert!(p.sub(0, 0, usize::MAX, usize::MAX).is_none());
    }
}
