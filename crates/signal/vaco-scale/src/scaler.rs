//! The public entry point.

use std::sync::Arc;

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};

use crate::exec::{self, DstPlane, SrcPlane};
use crate::options::ScaleOptions;
use crate::plan::Plan;
use crate::spec::ImageSpec;

/// A configured conversion.
///
/// Construction does all the expensive work — format decomposition, colour
/// derivation, coefficient generation — so the per-frame path allocates only
/// band scratch. Reuse one `Scaler` across a stream; constructing one per frame
/// rebuilds every filter bank.
///
/// # Threads
///
/// `threads = 0` and `threads = 1` both run on the calling thread. That is a
/// deliberate difference from the reference, which reads `0` as "auto": a
/// library that silently spawns a thread pool inside a filter graph that is
/// already parallel makes things slower, not faster, and the caller is the one
/// who knows. Set `threads` above 1 to opt in; the pool is built once, here.
#[derive(Debug)]
pub struct Scaler {
    plan: Plan,
    budget: Budget,
    opts: ScaleOptions,
    pool: Option<Arc<rayon::ThreadPool>>,
}

impl Scaler {
    /// Build a scaler for a fixed conversion.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] for a pixel format or matrix outside the
    /// implemented set, [`Error::InvalidData`] for a degenerate size, and
    /// [`Error::LimitExceeded`] if the coefficient banks exceed the budget.
    pub fn new(src: &ImageSpec, dst: &ImageSpec, opts: &ScaleOptions) -> Result<Self> {
        Self::with_limits(src, dst, opts, Limits::permissive())
    }

    /// [`Scaler::new`] with an explicit allocation policy.
    ///
    /// # Errors
    ///
    /// As [`Scaler::new`].
    pub fn with_limits(
        src: &ImageSpec,
        dst: &ImageSpec,
        opts: &ScaleOptions,
        limits: Limits,
    ) -> Result<Self> {
        let mut budget = Budget::new(limits);
        let plan = Plan::build(&mut budget, src, dst, opts)?;
        let pool = build_pool(opts.threads)?;
        Ok(Self {
            plan,
            budget,
            opts: opts.clone(),
            pool,
        })
    }

    /// True when the configured conversion copies its input unchanged.
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.plan.is_noop()
    }

    /// The source description this scaler is configured for.
    #[must_use]
    pub fn src_spec(&self) -> &ImageSpec {
        &self.plan.src
    }

    /// The destination description this scaler is configured for.
    #[must_use]
    pub fn dst_spec(&self) -> &ImageSpec {
        &self.plan.dst
    }

    /// A human-readable dump of the lowered plan, for `-v debug` and tests.
    #[must_use]
    pub fn explain(&self) -> String {
        self.plan.explain()
    }

    /// Options this scaler accepts but does not act on.
    #[must_use]
    pub fn unimplemented_options(&self) -> Vec<&'static str> {
        self.opts.unimplemented()
    }

    /// Convert `src` into `dst`, reconfiguring if either frame's description has
    /// changed.
    ///
    /// # Errors
    ///
    /// As [`Scaler::new`], plus [`Error::InvalidData`] if a frame is audio, has
    /// the wrong plane count, or has a plane too short for its own geometry.
    pub fn scale_frame(&mut self, src: &Frame, dst: &mut Frame) -> Result<()> {
        let src_spec = spec_of(src)?;
        let dst_spec = spec_of(dst)?;
        if src_spec != self.plan.src || dst_spec != self.plan.dst {
            let opts = self.opts.clone();
            let mut budget = Budget::new(self.budget.limits().clone());
            self.plan = Plan::build(&mut budget, &src_spec, &dst_spec, &opts)?;
            self.budget = budget;
        }

        let mut src_planes: Vec<SrcPlane<'_>> = Vec::new();
        for i in 0..src.plane_count() {
            let Some(p) = src.plane(i) else {
                return Err(Error::InvalidData("source plane missing"));
            };
            src_planes.push(SrcPlane {
                data: p.as_slice(),
                stride: p.stride(),
            });
        }

        let FrameData::Video { planes, .. } = &mut dst.data else {
            return Err(Error::InvalidData("destination frame is not video"));
        };
        let mut dst_planes: Vec<DstPlane<'_>> = Vec::new();
        for p in planes.iter_mut() {
            let stride = p.stride;
            dst_planes.push(DstPlane {
                data: p.data.make_mut(),
                stride,
            });
        }

        exec::run(
            &self.plan,
            &self.budget,
            &src_planes,
            &mut dst_planes,
            self.pool.as_deref(),
        )
    }

    /// Convert raw planes, for callers that do not hold a [`Frame`].
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the plane count or a plane's length does not
    /// match the configured conversion.
    pub fn scale_planes(&mut self, src: &[SrcPlane<'_>], dst: &mut [DstPlane<'_>]) -> Result<()> {
        exec::run(&self.plan, &self.budget, src, dst, self.pool.as_deref())
    }
}

fn build_pool(threads: i32) -> Result<Option<Arc<rayon::ThreadPool>>> {
    if threads <= 1 {
        return Ok(None);
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads.max(1) as usize)
        .build()
        .map_err(|_| Error::Unsupported("could not build a worker pool"))?;
    Ok(Some(Arc::new(pool)))
}

/// The [`ImageSpec`] a frame describes.
fn spec_of(frame: &Frame) -> Result<ImageSpec> {
    let FrameData::Video {
        format,
        width,
        height,
        ..
    } = frame.data
    else {
        return Err(Error::InvalidData("frame is not video"));
    };
    Ok(ImageSpec {
        format,
        width,
        height,
        color: frame.color,
    })
}

/// Whether this crate can read `fmt`.
#[must_use]
pub fn supports_input(fmt: vaco_pixfmt::PixFmt) -> bool {
    crate::geometry::supports_input(fmt)
}

/// Whether this crate can write `fmt`.
#[must_use]
pub fn supports_output(fmt: vaco_pixfmt::PixFmt) -> bool {
    crate::geometry::supports_output(fmt)
}

/// Whether a conversion between two descriptions can be planned.
///
/// A real predicate: it attempts the plan and reports whether it succeeded,
/// rather than consulting a table that can drift from what the code does.
#[must_use]
pub fn supports_conversion(src: &ImageSpec, dst: &ImageSpec, opts: &ScaleOptions) -> bool {
    let mut budget = Budget::new(Limits::permissive());
    Plan::build(&mut budget, src, dst, opts).is_ok()
}
