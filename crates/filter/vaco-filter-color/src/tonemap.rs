//! `tonemap` — HDR-to-SDR conversion through `vaco-scale`'s BT.2390 path.
//!
//! This filter intentionally has a narrow, explicit contract: PQ/HLG input
//! becomes BT.709 SDR at the requested display peak.  The pixel conversion,
//! mastering/content-light fallback order, BT.2390 EETF, gamut intent, and
//! tetrahedral lattice are all owned by `vaco-scale`; duplicating any one of
//! those here would create a second colour-management pipeline.

use vaco_color::{ColorPrimaries, MatrixCoefficients, TransferCharacteristic};
use vaco_core::{Error, MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData, FramePool, FrameSideData};
use vaco_pixfmt::PixFmt;
use vaco_scale::{
    ImageSpec, RenderingIntent, ScaleOptions, Scaler, supports_input, supports_output,
};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "tonemap",
    description: "Convert HDR video to BT.709 SDR with BT.2390",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "tonemap", help = "Convert HDR video to BT.709 SDR")]
pub(crate) struct Opts {
    #[opt(name = "peak", help = "target SDR display peak in nits", default = 100, range = 1..=10_000, flags(video, filtering))]
    pub peak: i32,
    #[opt(name = "intent", help = "rendering intent: perceptual, relative_colorimetric, saturation, absolute_colorimetric", default = "perceptual".to_owned(), flags(video, filtering))]
    pub intent: String,
    #[opt(name = "lut3d_size", help = "BT.2390/gamut LUT edge length", default = 33, range = 9..=65, flags(video, filtering))]
    pub lut3d_size: i32,
}

fn parse_intent(name: &str) -> std::result::Result<RenderingIntent, String> {
    match name {
        "perceptual" => Ok(RenderingIntent::Perceptual),
        "relative_colorimetric" => Ok(RenderingIntent::RelativeColorimetric),
        "saturation" => Ok(RenderingIntent::Saturation),
        "absolute_colorimetric" => Ok(RenderingIntent::AbsoluteColorimetric),
        _ => Err(format!("tonemap: unsupported intent `{name}`")),
    }
}

fn hdr_peaks(frame: &Frame) -> (Option<u32>, Option<u32>) {
    let mut mastering = None;
    let mut content_light = None;
    for side_data in &frame.side_data {
        match side_data {
            FrameSideData::MasteringDisplay(display) if mastering.is_none() => {
                let peak = display.max_luminance.to_f64();
                if peak.is_finite() && peak > 0.0 && peak <= f64::from(u32::MAX) {
                    mastering = Some(peak.round() as u32);
                }
            }
            FrameSideData::ContentLightLevel { max_cll, .. }
                if content_light.is_none() && *max_cll > 0 =>
            {
                content_light = Some(*max_cll);
            }
            _ => {}
        }
    }
    (mastering, content_light)
}

#[derive(Debug, Clone)]
pub(crate) struct Filter {
    peak: u32,
    scale_options: ScaleOptions,
}

impl Filter {
    fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let intent = parse_intent(&opts.intent)?;
        let peak = u32::try_from(opts.peak).map_err(|_| "tonemap: invalid `peak`".to_owned())?;
        let scale_options = ScaleOptions {
            intent,
            lut3d_size: opts.lut3d_size,
            ..ScaleOptions::default()
        };
        Ok(Self {
            peak,
            scale_options,
        })
    }

    fn output_color(format: PixFmt, input: vaco_color::ColorInfo) -> vaco_color::ColorInfo {
        vaco_color::ColorInfo {
            primaries: ColorPrimaries::Bt709,
            transfer: TransferCharacteristic::Bt709,
            matrix: if format.is_rgb() {
                MatrixCoefficients::Identity
            } else {
                MatrixCoefficients::Bt709
            },
            ..input
        }
    }

    fn map_frame(&self, pool: &FramePool, input: Frame) -> Result<Frame> {
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = input.data
        else {
            return Ok(input);
        };
        if !input.color.transfer.is_hdr() {
            return Ok(input);
        }
        if !supports_input(format) || !supports_output(format) {
            return Err(Error::Unsupported("tonemap: unsupported pixel format"));
        }
        let (mastering_peak, content_light_peak) = hdr_peaks(&input);
        let src = ImageSpec::new(format, width, height)
            .with_color(input.color)
            .with_hdr_peaks(mastering_peak, content_light_peak);
        let output_color = Self::output_color(format, input.color);
        let dst = ImageSpec::new(format, width, height)
            .with_color(output_color)
            .with_hdr_peaks(Some(self.peak), None);
        let mut scaler = Scaler::new(&src, &dst, &self.scale_options)?;
        let mut out = pool.acquire_video(format, width, height)?;
        scaler.scale_frame(&input, &mut out)?;
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        out.color = output_color;
        out.flags = input.flags;
        out.sample_aspect_ratio = input.sample_aspect_ratio;
        out.side_data = input
            .side_data
            .iter()
            .filter(|side_data| {
                !matches!(
                    side_data,
                    FrameSideData::MasteringDisplay(_) | FrameSideData::ContentLightLevel { .. }
                )
            })
            .cloned()
            .collect();
        Ok(out)
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        Ok(FrameOut::One(self.map_frame(ctx.pool(), input)?))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts: Opts = common::parse(req.args)?;
    let filter = Filter::new(&opts)?;
    let formats = FormatSet::video_list(
        PixFmt::all()
            .iter()
            .copied()
            .filter(|format| supports_input(*format) && supports_output(*format)),
    );
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::uniform(1, 1, MediaType::Video, &formats, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_filter_graph::registry::FilterRegistry;

    #[test]
    fn intent_names_are_the_scale_intents() {
        assert_eq!(parse_intent("perceptual"), Ok(RenderingIntent::Perceptual));
        assert_eq!(
            parse_intent("relative_colorimetric"),
            Ok(RenderingIntent::RelativeColorimetric)
        );
        assert!(parse_intent("hable").is_err());
    }

    #[test]
    fn mastering_peak_wins_over_content_light_peak() {
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 1, 1).unwrap();
        frame.side_data.push(FrameSideData::ContentLightLevel {
            max_cll: 4_000,
            max_fall: 400,
        });
        frame
            .side_data
            .push(FrameSideData::MasteringDisplay(Box::new(
                vaco_frame::MasteringDisplay {
                    primaries: [[vaco_core::Rational::ZERO; 2]; 3],
                    white_point: [vaco_core::Rational::ZERO; 2],
                    max_luminance: vaco_core::Rational::new(1_000, 1),
                    min_luminance: vaco_core::Rational::ZERO,
                },
            )));
        assert_eq!(hdr_peaks(&frame), (Some(1_000), Some(4_000)));
    }

    #[test]
    fn default_filter_selects_perceptual_33_cube_sdr() {
        let filter = Filter::new(&Opts::default()).unwrap();
        assert_eq!(filter.peak, 100);
        assert_eq!(filter.scale_options.intent, RenderingIntent::Perceptual);
        assert_eq!(filter.scale_options.lut3d_size, 33);
    }

    #[test]
    fn color_registry_constructs_the_public_tonemap_name() {
        let registry = crate::ColorRegistry;
        let instance = registry
            .create(&Instantiate {
                name: "tonemap",
                instance: "tonemap",
                args: None,
                arguments: &[],
            })
            .unwrap();
        assert_eq!(instance.desc.name, "tonemap");
        assert_eq!(instance.formats.inputs.len(), 1);
        assert_eq!(instance.formats.outputs.len(), 1);
    }

    #[test]
    fn hdr_frame_reaches_the_scale_bt2390_path_and_clears_hdr_side_data() {
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 1, 1).unwrap();
        frame.color.primaries = ColorPrimaries::Bt2020;
        frame.color.transfer = TransferCharacteristic::Smpte2084;
        frame.color.matrix = MatrixCoefficients::Identity;
        frame
            .plane_mut(0)
            .unwrap()
            .row_mut(0)
            .unwrap()
            .get_mut(..3)
            .unwrap()
            .copy_from_slice(&[220, 180, 140]);
        frame.side_data.push(FrameSideData::ContentLightLevel {
            max_cll: 1_000,
            max_fall: 400,
        });
        let out = Filter::new(&Opts::default())
            .unwrap()
            .map_frame(&FramePool::default(), frame)
            .unwrap();
        assert_eq!(out.color.transfer, TransferCharacteristic::Bt709);
        assert_eq!(out.color.primaries, ColorPrimaries::Bt709);
        assert!(out.side_data.is_empty());
        assert_ne!(out.plane(0).unwrap().row(0).unwrap(), &[220, 180, 140]);
    }
}
