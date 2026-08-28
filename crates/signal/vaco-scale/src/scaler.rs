//! The public entry point.

use std::sync::Arc;

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};

use crate::exec::{self, DstPlane, SrcPlane};
use crate::options::ScaleOptions;
use crate::plan::Plan;
use crate::special::{self, mono_polarity};
use crate::spec::ImageSpec;
use vaco_pixfmt::PixFmt;

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
        let plan_src = plan_spec(src);
        let plan_dst = plan_spec(dst);
        let plan = Plan::build(&mut budget, &plan_src, &plan_dst, opts)?;
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
    ///
    /// For a `monowhite`/`monoblack`/float destination (`special`'s three
    /// proxied families), this reports the *proxy* format the underlying
    /// [`Plan`] actually runs — `gray8`/`gray16le`/`rgb48le` — not the format
    /// [`Scaler::with_limits`] was called with, since nothing in this crate
    /// past construction ever sees the real one again.
    #[must_use]
    pub fn src_spec(&self) -> &ImageSpec {
        &self.plan.src
    }

    /// The destination description this scaler is configured for.
    ///
    /// See [`Scaler::src_spec`]'s note on the three proxied format families.
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
        let real_src_spec = spec_of(src)?;
        let real_dst_spec = spec_of(dst)?;
        let plan_src_spec = plan_spec(&real_src_spec);
        let plan_dst_spec = plan_spec(&real_dst_spec);
        if plan_src_spec != self.plan.src || plan_dst_spec != self.plan.dst {
            let opts = self.opts.clone();
            let mut budget = Budget::new(self.budget.limits().clone());
            self.plan = Plan::build(&mut budget, &plan_src_spec, &plan_dst_spec, &opts)?;
            self.budget = budget;
        }

        // `special`'s three proxied format families never reach `geometry` at
        // all: a float source is unpacked into a `gray16le`/`rgb48le` frame
        // before this runs, and a `monowhite`/`monoblack`/float destination
        // writes into a `gray8`/`gray16le`/`rgb48le` frame that gets packed
        // or rescaled into the caller's real one afterwards.
        let src_proxy;
        let effective_src: &Frame = if special::float_info(real_src_spec.format).is_some() {
            src_proxy = special::float_frame_to_proxy(src, &mut self.budget)?;
            &src_proxy
        } else {
            src
        };

        if plan_dst_spec.format == real_dst_spec.format {
            return run_plan(&self.plan, &self.budget, self.pool.as_deref(), effective_src, dst);
        }

        let mut dst_proxy = Frame::alloc_video(
            &mut self.budget,
            plan_dst_spec.format,
            plan_dst_spec.width,
            plan_dst_spec.height,
        )?;
        run_plan(
            &self.plan,
            &self.budget,
            self.pool.as_deref(),
            effective_src,
            &mut dst_proxy,
        )?;

        if let Some(polarity) = mono_polarity(real_dst_spec.format) {
            special::pack_mono(&dst_proxy, dst, polarity)
        } else if special::float_info(real_dst_spec.format).is_some() {
            special::proxy_to_float_frame(&dst_proxy, dst)
        } else {
            Err(Error::InvalidData(
                "scaler: destination proxy did not match any known special format",
            ))
        }
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

/// Run `plan` from `src`'s planes into `dst`'s, the part of
/// [`Scaler::scale_frame`] shared by the direct path and the proxied one.
fn run_plan(
    plan: &Plan,
    budget: &Budget,
    pool: Option<&rayon::ThreadPool>,
    src: &Frame,
    dst: &mut Frame,
) -> Result<()> {
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

    exec::run(plan, budget, &src_planes, &mut dst_planes, pool)
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

/// The `ImageSpec` [`Plan::build`] should actually target for `real`.
///
/// `geometry` refuses `BITSTREAM` and `FLOAT` formats outright (`plan 17
/// §A.1`'s "everything above this module is `i32` component planes" leaves no
/// room for either), so `real` itself only when it is none of the three
/// families `special` bridges through a proxy; a `gray8` proxy for
/// `monowhite`/`monoblack` (a thresholding decision, not a format the plan
/// runs pixels through) and a `gray16le`/`rgb48le` proxy — matched to the
/// float format's own channel count — for the eight float formats.
fn plan_spec(real: &ImageSpec) -> ImageSpec {
    if mono_polarity(real.format).is_some() {
        return ImageSpec {
            format: PixFmt::Gray8,
            ..*real
        };
    }
    if let Some(info) = special::float_info(real.format) {
        return ImageSpec {
            format: info.proxy(),
            ..*real
        };
    }
    *real
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
///
/// True for the eight float formats even though `geometry` refuses them —
/// see [`supports_output`]'s note; `monowhite`/`monoblack` are not included
/// here, since `special` only ever produces them, never reads them.
#[must_use]
pub fn supports_input(fmt: vaco_pixfmt::PixFmt) -> bool {
    special::float_info(fmt).is_some() || crate::geometry::supports_input(fmt)
}

/// Whether this crate can write `fmt`.
///
/// True for `monowhite`/`monoblack` and the eight float formats even though
/// `geometry` itself refuses all ten — `special` reaches them through a
/// proxy format, and this is the caller-facing contract, not an internal
/// implementation detail.
#[must_use]
pub fn supports_output(fmt: vaco_pixfmt::PixFmt) -> bool {
    mono_polarity(fmt).is_some()
        || special::float_info(fmt).is_some()
        || crate::geometry::supports_output(fmt)
}

/// Whether a conversion between two descriptions can be planned.
///
/// A real predicate: it attempts the plan and reports whether it succeeded,
/// rather than consulting a table that can drift from what the code does.
/// Tests the proxy `Plan` for `special`'s three format families — see
/// [`plan_spec`] — so this agrees with what [`Scaler::with_limits`] actually
/// does rather than with `geometry`'s narrower, pre-proxy view.
#[must_use]
pub fn supports_conversion(src: &ImageSpec, dst: &ImageSpec, opts: &ScaleOptions) -> bool {
    let mut budget = Budget::new(Limits::permissive());
    let plan_src = plan_spec(src);
    let plan_dst = plan_spec(dst);
    Plan::build(&mut budget, &plan_src, &plan_dst, opts).is_ok()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_pixfmt::PixFmt;

    fn gray8(width: u32, height: u32, values: &[u8]) -> Frame {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Gray8, width, height).unwrap();
        let FrameData::Video { planes, .. } = &mut frame.data else {
            unreachable!()
        };
        let row = planes[0].data.make_mut();
        row[..values.len()].copy_from_slice(values);
        frame
    }

    #[test]
    fn scaler_reaches_monowhite_end_to_end() {
        let values = [0u8, 255, 76, 179, 30, 220, 100, 150];
        let src = gray8(8, 1, &values);
        let src_spec = ImageSpec::new(PixFmt::Gray8, 8, 1);
        let dst_spec = ImageSpec::new(PixFmt::MonoWhite, 8, 1);
        let mut scaler = Scaler::new(&src_spec, &dst_spec, &ScaleOptions::default()).unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let mut dst = Frame::alloc_video(&mut budget, PixFmt::MonoWhite, 8, 1).unwrap();
        scaler.scale_frame(&src, &mut dst).unwrap();

        let FrameData::Video { planes, .. } = &dst.data else {
            unreachable!()
        };
        let byte = planes[0].data.as_slice()[0];
        let mut expected = 0u8;
        for (x, &v) in values.iter().enumerate() {
            let threshold = special::mono_threshold(x, 0);
            if u16::from(v) < threshold {
                expected |= 0x80 >> x;
            }
        }
        assert_eq!(byte, expected);
    }

    #[test]
    fn scaler_reaches_grayf32_end_to_end_and_back() {
        // Dither off: this checks the float bridge is lossless for 8-bit
        // values round-tripped through 16 bits of headroom, not this crate's
        // (deliberate, tested elsewhere) depth-reduction dithering.
        let opts = ScaleOptions {
            dither: crate::options::DitherKind::None,
            ..ScaleOptions::default()
        };
        let values = [0u8, 64, 128, 192, 255];
        let src = gray8(5, 1, &values);
        let src_spec = ImageSpec::new(PixFmt::Gray8, 5, 1);
        let float_spec = ImageSpec::new(PixFmt::Grayf32le, 5, 1);
        let mut to_float = Scaler::new(&src_spec, &float_spec, &opts).unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let mut floaty = Frame::alloc_video(&mut budget, PixFmt::Grayf32le, 5, 1).unwrap();
        to_float.scale_frame(&src, &mut floaty).unwrap();

        // And back: float source -> gray8 destination should round-trip the
        // 8-bit values exactly, since expand-then-reduce through 16 bits of
        // headroom cannot lose an 8-bit value.
        let mut back = Scaler::new(&float_spec, &src_spec, &opts).unwrap();
        let mut budget2 = Budget::new(Limits::permissive());
        let mut roundtrip = Frame::alloc_video(&mut budget2, PixFmt::Gray8, 5, 1).unwrap();
        back.scale_frame(&floaty, &mut roundtrip).unwrap();

        let FrameData::Video { planes, .. } = &roundtrip.data else {
            unreachable!()
        };
        let got = &planes[0].data.as_slice()[..5];
        // Off by at most 1: full-scale bit-replication expansion (8->16) and
        // add-half-then-shift reduction (16->8) are not exact inverses of
        // each other in general — a property of `dither::expand_depth`/
        // `reduce_depth`, not of the float bridge this test exercises.
        for (i, (&g, &v)) in got.iter().zip(values.iter()).enumerate() {
            assert!(
                g.abs_diff(v) <= 1,
                "index {i}: got {g}, expected {v} (+/-1)"
            );
        }
    }

    #[test]
    fn supports_output_reports_the_three_proxied_families() {
        assert!(supports_output(PixFmt::MonoWhite));
        assert!(supports_output(PixFmt::MonoBlack));
        assert!(supports_output(PixFmt::Grayf32le));
        assert!(supports_output(PixFmt::Rgbf16be));
    }

    #[test]
    fn supports_conversion_agrees_with_a_real_attempt() {
        let src = ImageSpec::new(PixFmt::Bgr24, 4, 4);
        let dst = ImageSpec::new(PixFmt::MonoWhite, 4, 4);
        assert!(supports_conversion(&src, &dst, &ScaleOptions::default()));
    }
}
