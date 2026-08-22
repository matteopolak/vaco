//! Plane views — the universal currency for pixel access.
//!
//! Kernels take a [`PlaneRef`] or a [`PlaneMut`], never a bare `&[u8]` obtained
//! from a [`Plane`](crate::Plane). That is a review-blocking rule rather than a
//! style preference (plan 11 §13.6): if planes ever become segmented to support
//! banded frame threading, a `PlaneRef` grows a banded representation and every
//! kernel keeps compiling, whereas a raw slice forecloses the option.

use vaco_core::{Error, Result};
use vaco_limits::Budget;
use vaco_pool::{ALIGN, Buffer};

use crate::Plane;

impl Plane {
    /// Wrap an existing buffer.
    #[must_use]
    pub const fn new(data: Buffer, stride: usize) -> Self {
        Self { data, stride }
    }

    /// Allocate `rows * stride` zeroed bytes.
    ///
    /// # Errors
    ///
    /// [`Error::LimitExceeded`] if the size overflows or a budget cap is hit.
    pub fn alloc(budget: &mut Budget, stride: usize, rows: usize) -> Result<Self> {
        let len = stride.checked_mul(rows).ok_or(Error::LimitExceeded {
            limit: "plane_bytes",
            requested: u64::MAX,
            cap: usize::MAX as u64,
        })?;
        Ok(Self {
            data: Buffer::alloc(budget, len)?,
            stride,
        })
    }

    /// How many whole rows the buffer holds at this stride.
    #[must_use]
    pub fn rows(&self) -> usize {
        if self.stride == 0 {
            0
        } else {
            self.data.len().checked_div(self.stride).unwrap_or(0)
        }
    }

    /// Whether row zero is on an [`ALIGN`] boundary and every row after it is
    /// too.
    ///
    /// Row zero always is; rows after it only when the stride is a multiple of
    /// [`ALIGN`], which is what [`Frame::alloc_video`](crate::Frame::alloc_video)
    /// arranges. Misalignment costs performance, not correctness — safe SIMD
    /// slice loads never require alignment — so this is advisory.
    #[must_use]
    pub fn is_row_aligned(&self) -> bool {
        self.data.is_aligned() && self.stride.is_multiple_of(ALIGN)
    }
}

/// Read-only view of one plane, with its geometry attached.
#[derive(Debug, Clone, Copy)]
pub struct PlaneRef<'a> {
    data: &'a [u8],
    stride: usize,
    rows: usize,
    row_bytes: usize,
}

/// Exclusive view of one plane.
///
/// `Send`, because it is a `&mut [u8]` and three `usize`s — which is what makes
/// the thread-scoped pattern in the crate docs compile.
#[derive(Debug)]
pub struct PlaneMut<'a> {
    data: &'a mut [u8],
    stride: usize,
    rows: usize,
    row_bytes: usize,
}

impl<'a> PlaneRef<'a> {
    pub(crate) const fn new(data: &'a [u8], stride: usize, rows: usize, row_bytes: usize) -> Self {
        Self {
            data,
            stride,
            rows,
            row_bytes,
        }
    }

    /// Every byte of the plane, padding rows included.
    #[must_use]
    pub const fn as_slice(&self) -> &'a [u8] {
        self.data
    }

    /// Bytes between consecutive rows.
    #[must_use]
    pub const fn stride(&self) -> usize {
        self.stride
    }

    /// Rows of picture data.
    #[must_use]
    pub const fn rows(&self) -> usize {
        self.rows
    }

    /// Meaningful bytes per row; `stride - row_bytes` is padding.
    #[must_use]
    pub const fn row_bytes(&self) -> usize {
        self.row_bytes
    }

    /// Row `y`, trimmed to [`PlaneRef::row_bytes`].
    ///
    /// `None` past the last row, so a kernel with an off-by-one produces a
    /// visible `None` rather than a panic or a neighbouring row.
    #[must_use]
    pub fn row(&self, y: usize) -> Option<&'a [u8]> {
        if y >= self.rows {
            return None;
        }
        let start = y.checked_mul(self.stride)?;
        let end = start.checked_add(self.row_bytes)?;
        self.data.get(start..end)
    }

    /// Every row in order.
    pub fn rows_iter(&self) -> impl Iterator<Item = &'a [u8]> + use<'a> {
        let (data, stride, row_bytes) = (self.data, self.stride, self.row_bytes);
        (0..self.rows).filter_map(move |y| {
            let start = y.checked_mul(stride)?;
            data.get(start..start.checked_add(row_bytes)?)
        })
    }
}

impl<'a> PlaneMut<'a> {
    pub(crate) const fn new(
        data: &'a mut [u8],
        stride: usize,
        rows: usize,
        row_bytes: usize,
    ) -> Self {
        Self {
            data,
            stride,
            rows,
            row_bytes,
        }
    }

    /// Downgrade to a read-only view.
    #[must_use]
    pub fn as_ref(&self) -> PlaneRef<'_> {
        PlaneRef::new(self.data, self.stride, self.rows, self.row_bytes)
    }

    /// Bytes between consecutive rows.
    #[must_use]
    pub const fn stride(&self) -> usize {
        self.stride
    }

    /// Rows of picture data.
    #[must_use]
    pub const fn rows(&self) -> usize {
        self.rows
    }

    /// Meaningful bytes per row.
    #[must_use]
    pub const fn row_bytes(&self) -> usize {
        self.row_bytes
    }

    /// Row `y`, trimmed to [`PlaneMut::row_bytes`].
    #[must_use]
    pub fn row(&self, y: usize) -> Option<&[u8]> {
        self.as_ref().row(y)
    }

    /// Row `y`, mutably.
    pub fn row_mut(&mut self, y: usize) -> Option<&mut [u8]> {
        if y >= self.rows {
            return None;
        }
        let start = y.checked_mul(self.stride)?;
        let end = start.checked_add(self.row_bytes)?;
        self.data.get_mut(start..end)
    }

    /// Disjoint mutable rows — what feeds slice-parallel filtering.
    ///
    /// Yields the padding along with each row, because splitting it off would
    /// need a second borrow of the same allocation.
    pub fn rows_mut(&mut self) -> impl Iterator<Item = &mut [u8]> {
        let stride = self.stride.max(1);
        self.data.chunks_mut(stride).take(self.rows)
    }

    /// Fill every meaningful byte with `value`, leaving row padding alone.
    pub fn fill(&mut self, value: u8) {
        for y in 0..self.rows {
            if let Some(row) = self.row_mut(y) {
                row.fill(value);
            }
        }
    }

    /// Split into at most `n` horizontal bands for scoped parallelism.
    ///
    /// Disjointness is proven by the compiler: `chunks_mut` yields
    /// non-overlapping `&mut [u8]`, so the bands can go to different threads
    /// with no runtime mechanism at all. Returns one band when `n` is 0 or 1.
    #[must_use]
    pub fn split_bands(self, n: usize) -> Vec<PlaneMut<'a>> {
        let n = n.max(1);
        if self.stride == 0 || self.rows == 0 {
            return vec![self];
        }
        let band_rows = self.rows.div_ceil(n);
        let chunk = band_rows.saturating_mul(self.stride);
        if chunk == 0 {
            return vec![self];
        }
        let (stride, row_bytes, mut left) = (self.stride, self.row_bytes, self.rows);
        self.data
            .chunks_mut(chunk)
            .map(move |band| {
                let rows = left.min(band.len().checked_div(stride).unwrap_or(0));
                left = left.saturating_sub(rows);
                PlaneMut::new(band, stride, rows, row_bytes)
            })
            .collect()
    }
}
