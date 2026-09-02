//! [`MotionRegistry`] — the `FilterRegistry` this crate's filters answer
//! through, same shape as `vaco-filter-artistic::registry`.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

const NAMES: &[&str] = &["framerate", "deshake", "stabdetect", "stabtransform"];

#[derive(Debug, Clone, Copy, Default)]
pub struct MotionRegistry;

impl FilterRegistry for MotionRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "framerate" => crate::framerate::create(req),
            "deshake" => crate::deshake::create(req),
            "stabdetect" => crate::stabdetect::create(req),
            "stabtransform" => crate::stabtransform::create(req),
            other => Err(format!("vaco-filter-motion: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = MotionRegistry;
        for &name in NAMES {
            // `stabtransform` genuinely requires a real transform file (see
            // its module doc, and `vidstabtransform`'s own identical
            // requirement) and is expected to fail cleanly without one.
            if name == "stabtransform" {
                let req = Instantiate {
                    name,
                    instance: name,
                    args: None,
                    arguments: &[],
                };
                assert!(registry.create(&req).is_err());
                continue;
            }
            // `stabdetect` succeeds with defaults, but its default `result`
            // path is the reference's own default filename
            // (`transforms.trf`) written to the working directory — fine
            // for a real run, not something this test should leave behind
            // in whatever directory `cargo test` happens to run from, so
            // it is pointed at a scratch path here instead.
            let args = (name == "stabdetect").then(|| {
                std::env::temp_dir()
                    .join(format!(
                        "vaco-motion-registry-test-{}.trf",
                        std::process::id()
                    ))
                    .to_string_lossy()
                    .into_owned()
            });
            let args_str = args.as_ref().map(|p| format!("result={p}"));
            let req = Instantiate {
                name,
                instance: name,
                args: args_str.as_deref(),
                arguments: &[],
            };
            assert!(
                registry.create(&req).is_ok(),
                "{name} should be creatable with defaults"
            );
            if let Some(p) = args {
                let _ = std::fs::remove_file(p);
            }
        }
    }

    #[test]
    fn an_unknown_name_is_a_clean_error_not_a_panic() {
        let registry = MotionRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }

    /// The real two-pass pipeline, end to end: `stabdetect` writes a file
    /// from a genuinely jittery sequence, `stabtransform` reads *that
    /// exact file* (not a hand-built one, unlike each filter's own
    /// narrower unit test) and the corrected sequence is measurably
    /// steadier than the raw input. This is the one test that would fail
    /// if the two filters' independently-designed file format somehow
    /// disagreed with each other despite each one's own tests passing.
    #[test]
    fn stabdetect_then_stabtransform_reduces_jitter_through_a_real_file() {
        use vaco_frame::FramePool;
        use vaco_pixfmt::PixFmt;

        fn shifted_frame(w: u32, h: u32, shift: i32) -> vaco_frame::Frame {
            let pool = FramePool::default();
            let mut f = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
            if let Some(mut p) = f.plane_mut(0) {
                for y in 0..h as usize {
                    if let Some(row) = p.row_mut(y) {
                        for (x, cell) in row.iter_mut().enumerate() {
                            #[allow(
                                clippy::cast_possible_truncation,
                                clippy::cast_possible_wrap,
                                reason = "test fixture, small bounded values"
                            )]
                            let v = (((x as i32 - shift).rem_euclid(256)) as u8)
                                .wrapping_add((y * 7) as u8);
                            *cell = v;
                        }
                    }
                }
            }
            f
        }

        let (w, h) = (64u32, 64u32);
        let jitters = [0i32, 6, -6, 6, -6, 6, -6];
        let raw: Vec<vaco_frame::Frame> = jitters.iter().map(|&s| shifted_frame(w, h, s)).collect();

        let path = std::env::temp_dir()
            .join(format!("vaco-motion-e2e-{}.trf", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let detect_opts = crate::stabdetect::Opts {
            result: path.clone(),
            ..Default::default()
        };
        let mut detector = crate::stabdetect::Filter::new(&detect_opts).unwrap();
        for f in &raw {
            detector.process(f.clone()).unwrap();
        }
        detector.reset();

        let transform_opts = crate::stabtransform::Opts {
            input: path.clone(),
            smoothing: 3,
            ..Default::default()
        };
        let mut transformer = crate::stabtransform::Filter::new(&transform_opts).unwrap();
        let pool = FramePool::default();
        let corrected: Vec<vaco_frame::Frame> = raw
            .iter()
            .map(|f| match transformer.process(&pool, f.clone()).unwrap() {
                vaco_filter_core::adapt::FrameOut::One(out) => out,
                _ => panic!("expected exactly one output frame"),
            })
            .collect();

        let raw_diff: u64 = raw
            .windows(2)
            .map(|w2| vaco_filter_vdsp::plane_sad(w2[0].plane(0).unwrap(), w2[1].plane(0).unwrap()))
            .sum();
        let corrected_diff: u64 = corrected
            .windows(2)
            .map(|w2| vaco_filter_vdsp::plane_sad(w2[0].plane(0).unwrap(), w2[1].plane(0).unwrap()))
            .sum();
        assert!(
            corrected_diff < raw_diff,
            "end-to-end stabdetect -> stabtransform should reduce jitter: raw={raw_diff} corrected={corrected_diff}"
        );
        let _ = std::fs::remove_file(&path);
    }
}
