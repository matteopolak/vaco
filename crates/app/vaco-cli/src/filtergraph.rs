//! CL-20: the simple per-output-stream filtergraph — `-vf`/`-af`/`-filter`,
//! `-s`, `-aspect`, `-pix_fmt` — and CL-25's shared plumbing.
//!
//! Plan 14 §6.6: "One graph is constructed per output stream, wired `decoder
//! -> [auto-conv] -> user graph -> [auto-conv] -> encoder`. `-s` on an output
//! appends a `scale` filter at the **end** of the graph; `-aspect`, `-pix_fmt`
//! ... insert filters at fixed positions."
//!
//! # What is implemented
//!
//! - `-vf`/`-af`/`-filter[:v|:a]` text, parsed and built through
//!   [`crate::filterreg::CliFilterRegistry`] (every registered filter
//!   reachable, per that module's doc).
//! - `-s WxH` (or a named abbreviation, `vaco_core::parse::image_size`),
//!   appended as `scale=w=W:h=H`.
//! - `-aspect` (`W:H`, `W/H`, or a bare decimal), appended as `setdar=dar=…`
//!   — the colon form is rewritten to `/` before it reaches the graph
//!   scanner, because `:` is that scanner's own argument separator.
//! - `-pix_fmt name` (the plain form), appended as `format=name`, which
//!   declares the constraint and lets auto-conversion supply the actual
//!   `scale`.
//! - Auto-conversion (`-auto_conversion_filters`, default on): whatever the
//!   user's chain and `-pix_fmt` leave unresolved between the decoder's
//!   reported format and the encoder's [`accepted_pix_fmts`] is closed by
//!   [`vaco_filter_graph::convert::DefaultConverters`], the same policy
//!   `vaco_filter_graph::build::BuiltGraph::configure` already implements.
//!
//! # What is not
//!
//! - `-pix_fmt`'s two special forms (a `+name` that turns an unselectable
//!   format into a hard error and disables conversion for that stream; a
//!   bare `+` that forces the input/graph format through with conversion
//!   disabled). The `+` is stripped and the remainder (if any) is used as an
//!   ordinary `-pix_fmt`, which is a real divergence, not a crash — noted in
//!   this crate's closing report for CL-20 rather than chased further, per
//!   this crate's brief.
//! - Multiple `-af`/`-vf` occurrences on one output as *separate* graphs
//!   (the reference's own behaviour): the last one wins, matching how `-c`
//!   already resolves repeated per-stream options in this crate.
//! - `-autoscale`/`-autorotate`/`apply_cropping`'s exact fixed positions
//!   relative to `-s`/`-pix_fmt` in the chain — user filters, then `-s`'s
//!   `scale`, then `-aspect`'s `setdar`, then `-pix_fmt`'s `format`, which is
//!   *a* defensible order but not verified against the reference's own
//!   ordering rule beyond what plan 14 §6.6 states.
//!
//! [`accepted_pix_fmts`]: vaco_codec_core::Encoder::accepted_pix_fmts

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{AudioParameters, VideoParameters};
use vaco_core::{MediaType, Rational};
use vaco_filter_core::negotiate::{AutoConvert, Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{Graph, LinkFormat, NodeId};
use vaco_pixfmt::PixFmt;
use vaco_sampfmt::SampleFmt;

use crate::filterreg::CliFilterRegistry;

/// Everything CL-20's options resolved to for one output stream.
#[derive(Debug, Clone, Default)]
pub struct SimpleGraphOptions {
    /// `-vf`/`-af`/`-filter[:v|:a]`'s text, last occurrence wins.
    pub filter_text: Option<String>,
    /// `-s`, already parsed.
    pub size: Option<(u32, u32)>,
    /// `-aspect`'s raw value, `:`-form rewritten to `/` and any other
    /// whitespace left alone — [`build`] does the rest.
    pub aspect: Option<String>,
    /// `-pix_fmt`'s value with a leading `+` stripped (see module docs for
    /// what that loses).
    pub pix_fmt: Option<String>,
    /// `-noauto_conversion_filters` was given (default: filters run).
    pub auto_conversion: bool,
    /// `-ar`, already parsed. The audio mirror of [`SimpleGraphOptions::size`]:
    /// where `-s` pins the video sink's dimensions, this pins the audio
    /// sink's sample rate, and the same auto-conversion machinery that
    /// inserts `scale` for the one inserts `aresample` for the other.
    pub sample_rate: Option<u32>,
    /// `-ac`, already parsed as a channel count. Resolved to a concrete
    /// [`ChannelLayout`] in [`build`] via [`ChannelLayout::default_for`],
    /// falling back to [`ChannelLayout::unspecified`] for a count no named
    /// layout has — matching the reference's own bare `<n>c` layout spec
    /// rather than rejecting an otherwise-valid channel count.
    pub channels: Option<u32>,
    /// `-sample_fmt`'s raw name, resolved against [`SampleFmt::from_name`] in
    /// [`build`] rather than here, so an unrecognised name is a build-time
    /// error with the stream in scope rather than a silently-dropped option.
    pub sample_fmt: Option<String>,
    /// `-fps_mode` (CL-21/#222), already parsed. Video-only, and resolved
    /// against `crate::exec::run_pipeline`'s `frames` tap directly
    /// (`crate::fps_mode::insert`), not through this module's own
    /// text-based `-vf` chain — see that module's doc for why. Lives on this
    /// struct only because `crate::exec::graph_options_of` already resolves
    /// every per-stream option through the same `stream_option` call this
    /// one needs.
    pub fps_mode: Option<crate::fps_mode::FpsMode>,
    /// `-enc_time_base` (CL-21/#222), already parsed. Same rationale as
    /// [`SimpleGraphOptions::fps_mode`] for why it lives here.
    pub enc_time_base: Option<crate::enc_time_base::EncTimeBase>,
}

impl SimpleGraphOptions {
    /// Whether any of these options requires building a graph at all — the
    /// common case (a plain `-c:v encoder`, nothing else) must keep using the
    /// caller's non-graph auto-conversion path unchanged.
    #[must_use]
    pub fn wants_graph(&self) -> bool {
        self.filter_text.is_some()
            || self.size.is_some()
            || self.aspect.is_some()
            || self.pix_fmt.is_some()
            || self.sample_rate.is_some()
            || self.channels.is_some()
            || self.sample_fmt.is_some()
    }
}

/// A configured, ready-to-schedule simple graph: exactly one source and one
/// sink, per plan 14 §6.6's "1-in/1-out" simple-graph contract.
#[derive(Debug)]
pub struct SimpleGraph {
    pub graph: Graph,
    pub source: NodeId,
    pub sink: NodeId,
}

/// Build the description text: user text, then `-s`'s `scale`, then
/// `-aspect`'s `setdar`, then `-pix_fmt`'s `format`, comma-chained. `None`
/// when [`SimpleGraphOptions::wants_graph`] would say false (nothing to
/// build), so callers should check that first.
fn describe(opts: &SimpleGraphOptions, media: MediaType) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(t) = &opts.filter_text
        && !t.is_empty()
    {
        parts.push(t.clone());
    }
    if media == MediaType::Video {
        if let Some((w, h)) = opts.size {
            parts.push(format!("scale=w={w}:h={h}"));
        }
        if let Some(aspect) = &opts.aspect {
            // `4:3` collides with the graph scanner's own `:` argument
            // separator; the reference's own `-aspect` grammar accepts `W:H`,
            // `W/H`, or a bare decimal, so the `:` form is rewritten before it
            // ever reaches `vaco_filter_graph::lex`.
            let rewritten = aspect.replace(':', "/");
            parts.push(format!("setdar=dar={rewritten}"));
        }
        if let Some(fmt) = &opts.pix_fmt {
            let name = fmt.strip_prefix('+').unwrap_or(fmt);
            if !name.is_empty() {
                parts.push(format!("format={name}"));
            }
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join(","))
}

/// A [`LinkFormat`] for a decoded video stream, from the demuxer/decoder's own
/// reported [`VideoParameters`] — the same source of truth
/// [`crate::exec::converter_target`] already treats as authoritative for the
/// non-graph auto-conversion path.
pub(crate) fn video_link(v: &VideoParameters, time_base: Rational) -> LinkFormat {
    LinkFormat::Video {
        format: v.format.unwrap_or(PixFmt::Yuv420p),
        width: v.width.max(1),
        height: v.height.max(1),
        time_base,
        frame_rate: if v.frame_rate.num > 0 {
            v.frame_rate
        } else {
            Rational::new(25, 1)
        },
        sample_aspect_ratio: if v.sample_aspect_ratio.num > 0 {
            v.sample_aspect_ratio
        } else {
            Rational::ONE
        },
        color: v.color,
    }
}

pub(crate) fn audio_link(a: &AudioParameters, time_base: Rational) -> LinkFormat {
    LinkFormat::Audio {
        format: a.format.unwrap_or(SampleFmt::S16),
        sample_rate: if a.sample_rate > 0 {
            a.sample_rate
        } else {
            44_100
        },
        layout: a.layout.clone().unwrap_or(ChannelLayout::STEREO),
        time_base,
    }
}

/// Build and configure one output stream's simple graph.
///
/// `accepted_pix_fmts` is the chosen encoder's own
/// [`vaco_codec_core::Encoder::accepted_pix_fmts`] (empty means "no
/// preference"), constraining the video sink exactly as before.
/// `accepted_sample_fmts` is its audio mirror
/// ([`vaco_codec_core::Encoder::accepted_sample_fmts`]) — the audio sink used
/// to carry no constraint at all, which is what let `-ar`/`-ac`/`-sample_fmt`
/// resolve to nothing: a link between an unconstrained `aresample` output and
/// an unconstrained sink has no source of truth for what it carries, and
/// [`vaco_filter_core::negotiate`] correctly refuses to invent one rather than
/// guess. The sink now carries `-ar`/`-ac`/`-sample_fmt` (via `opts`) and
/// falls back to `accepted_sample_fmts` for the sample format precisely where
/// the non-graph path already did, so routing a stream through this function
/// does not regress that behaviour.
///
/// # Errors
///
/// A message describing what failed to parse, instantiate or configure —
/// never a panic, since a graph description is untrusted input.
pub fn build(
    opts: &SimpleGraphOptions,
    media: MediaType,
    video: Option<&VideoParameters>,
    audio: Option<&AudioParameters>,
    time_base: Rational,
    accepted_pix_fmts: &[PixFmt],
    accepted_sample_fmts: &[SampleFmt],
) -> Result<SimpleGraph, String> {
    let text = describe(opts, media)
        .unwrap_or_else(|| if media == MediaType::Audio { "anull" } else { "null" }.to_owned());

    let registry = CliFilterRegistry;
    let mut built = vaco_filter_graph::parse_and_build(&text, &registry)
        .map_err(|e| format!("filtergraph: {}", e.render(&text)))?;

    if built.open_inputs.len() != 1 || built.open_outputs.len() != 1 {
        return Err(format!(
            "a simple filtergraph must have exactly one input and one output pad; \
             `{text}` has {} input(s) and {} output(s)",
            built.open_inputs.len(),
            built.open_outputs.len()
        ));
    }

    let format = match media {
        MediaType::Video => video_link(video.ok_or("a video graph needs video parameters")?, time_base),
        MediaType::Audio => audio_link(audio.ok_or("an audio graph needs audio parameters")?, time_base),
        _ => return Err("simple filtergraphs are only built for video and audio".to_owned()),
    };
    let src_formats = match &format {
        LinkFormat::Video { format, .. } => NodeFormats {
            outputs: vec![FormatSet::video_exact(*format)],
            label: "in".to_owned(),
            ..NodeFormats::default()
        },
        LinkFormat::Audio {
            format,
            sample_rate,
            layout,
            ..
        } => NodeFormats {
            outputs: vec![FormatSet::audio_exact(*format, *sample_rate, layout.clone())],
            label: "in".to_owned(),
            ..NodeFormats::default()
        },
    };
    let source = built
        .attach_source(0, src_formats, format)
        .map_err(|e| format!("attaching the source: {e}"))?;

    let sink_formats = if media == MediaType::Video {
        if accepted_pix_fmts.is_empty() {
            NodeFormats {
                inputs: vec![FormatSet::default()],
                label: "out".to_owned(),
                ..NodeFormats::default()
            }
        } else {
            NodeFormats {
                inputs: vec![FormatSet::video_list(accepted_pix_fmts.iter().copied())],
                label: "out".to_owned(),
                ..NodeFormats::default()
            }
        }
    } else if media == MediaType::Audio {
        let mut set = FormatSet::default();
        // `-sample_fmt` pins it exactly; absent that, fall back to the
        // encoder's own accepted list — the same rule the non-graph path
        // applies via `accepted_audio.first()`, so a stream that only asked
        // for `-ar`/`-ac` still lands on a format the encoder actually takes
        // rather than whatever the source happened to carry.
        if let Some(name) = &opts.sample_fmt {
            let fmt = SampleFmt::from_name(name)
                .map_err(|_| format!("unrecognised -sample_fmt value `{name}`"))?;
            set.sample_formats = Some(Constraint::Exact(fmt));
        } else if !accepted_sample_fmts.is_empty() {
            set.sample_formats =
                Some(Constraint::OneOf(accepted_sample_fmts.to_vec()).normalised());
        }
        if let Some(rate) = opts.sample_rate {
            set.sample_rates = Some(Constraint::Exact(rate));
        }
        if let Some(channels) = opts.channels {
            let layout = ChannelLayout::default_for(channels)
                .unwrap_or_else(|| ChannelLayout::unspecified(channels));
            set.channel_layouts = Some(Constraint::Exact(layout));
        }
        NodeFormats {
            inputs: vec![set],
            label: "out".to_owned(),
            ..NodeFormats::default()
        }
    } else {
        NodeFormats {
            inputs: vec![FormatSet::default()],
            label: "out".to_owned(),
            ..NodeFormats::default()
        }
    };
    let sink = built
        .attach_sink(0, sink_formats)
        .map_err(|e| format!("attaching the sink: {e}"))?;

    let mode = if opts.auto_conversion {
        AutoConvert::All
    } else {
        AutoConvert::None
    };
    built
        .configure(&registry, mode)
        .map_err(|e| format!("configuring the filtergraph: {e}"))?;

    Ok(SimpleGraph {
        graph: built.graph,
        source,
        sink,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;

    fn video_params() -> VideoParameters {
        VideoParameters {
            width: 64,
            height: 48,
            format: Some(PixFmt::Yuv420p),
            frame_rate: Rational::new(25, 1),
            sample_aspect_ratio: Rational::ONE,
            ..VideoParameters::default()
        }
    }

    #[test]
    fn no_options_wants_no_graph() {
        assert!(!SimpleGraphOptions::default().wants_graph());
    }

    #[test]
    fn size_alone_wants_a_graph_and_describes_a_scale() {
        let opts = SimpleGraphOptions {
            size: Some((320, 240)),
            auto_conversion: true,
            ..SimpleGraphOptions::default()
        };
        assert!(opts.wants_graph());
        assert_eq!(
            describe(&opts, MediaType::Video).as_deref(),
            Some("scale=w=320:h=240")
        );
    }

    #[test]
    fn aspect_colon_form_is_rewritten_to_a_slash() {
        let opts = SimpleGraphOptions {
            aspect: Some("4:3".to_owned()),
            auto_conversion: true,
            ..SimpleGraphOptions::default()
        };
        assert_eq!(
            describe(&opts, MediaType::Video).as_deref(),
            Some("setdar=dar=4/3")
        );
    }

    #[test]
    fn a_size_only_graph_builds_and_configures() {
        let opts = SimpleGraphOptions {
            size: Some((32, 32)),
            auto_conversion: true,
            ..SimpleGraphOptions::default()
        };
        let built = build(
            &opts,
            MediaType::Video,
            Some(&video_params()),
            None,
            Rational::new(1, 25),
            &[],
            &[],
        )
        .unwrap();
        let LinkFormat::Video {
            format,
            width,
            height,
            sample_aspect_ratio,
            ..
        } = built.graph.sink_format(built.sink).unwrap()
        else {
            panic!("expected a video link");
        };
        assert_eq!(*format, PixFmt::Yuv420p);
        assert_eq!((*width, *height), (32, 32));
        // 64x48 -> 32x32: `sar_new = sar_old * (in_w*out_h)/(in_h*out_w)` =
        // 1 * (64*32)/(48*32) = 4/3, keeping DAR fixed.
        assert_eq!(*sample_aspect_ratio, Rational::new(4, 3));
    }

    #[test]
    fn a_pix_fmt_request_is_honoured_via_auto_conversion() {
        let opts = SimpleGraphOptions {
            pix_fmt: Some("gray8".to_owned()),
            auto_conversion: true,
            ..SimpleGraphOptions::default()
        };
        let built = build(
            &opts,
            MediaType::Video,
            Some(&video_params()),
            None,
            Rational::new(1, 25),
            &[],
            &[],
        )
        .unwrap();
        let LinkFormat::Video { format, .. } = built.graph.sink_format(built.sink).unwrap() else {
            panic!("expected a video link");
        };
        assert_eq!(*format, PixFmt::Gray8);
    }

    #[test]
    fn user_text_with_no_other_options_builds_directly() {
        let opts = SimpleGraphOptions {
            filter_text: Some("hflip".to_owned()),
            auto_conversion: true,
            ..SimpleGraphOptions::default()
        };
        assert!(
            build(
                &opts,
                MediaType::Video,
                Some(&video_params()),
                None,
                Rational::new(1, 25),
                &[],
                &[],
            )
            .is_ok()
        );
    }

    #[test]
    fn an_unknown_filter_name_is_a_clean_error_not_a_panic() {
        let opts = SimpleGraphOptions {
            filter_text: Some("not_a_real_filter_name_xyz".to_owned()),
            auto_conversion: true,
            ..SimpleGraphOptions::default()
        };
        assert!(
            build(
                &opts,
                MediaType::Video,
                Some(&video_params()),
                None,
                Rational::new(1, 25),
                &[],
                &[],
            )
            .is_err()
        );
    }

    fn audio_params() -> AudioParameters {
        AudioParameters {
            sample_rate: 48_000,
            format: Some(SampleFmt::S16),
            layout: Some(ChannelLayout::MONO),
            ..AudioParameters::default()
        }
    }

    /// The regression this whole fix is for: `-ar` alone (no `-af`) must
    /// still produce a graph whose sink resolves to the requested rate,
    /// rather than the "format negotiation left a property unconstrained"
    /// [`vaco_core::Error::Unsupported`] an unconstrained audio sink used to
    /// leave nothing to resolve it with.
    #[test]
    fn a_sample_rate_request_alone_resolves_the_sink() {
        let opts = SimpleGraphOptions {
            sample_rate: Some(44_100),
            auto_conversion: true,
            ..SimpleGraphOptions::default()
        };
        let built = build(
            &opts,
            MediaType::Audio,
            None,
            Some(&audio_params()),
            Rational::new(1, 48_000),
            &[],
            &[],
        )
        .unwrap();
        let LinkFormat::Audio { sample_rate, .. } = built.graph.sink_format(built.sink).unwrap()
        else {
            panic!("expected an audio link");
        };
        assert_eq!(*sample_rate, 44_100);
    }

    /// `-ac` resolves to a concrete channel layout on the sink, and `-sample_fmt`
    /// alongside it resolves the format too — both from the sink alone, with
    /// no `-af` in the description at all.
    #[test]
    fn channel_count_and_sample_format_requests_resolve_the_sink() {
        let opts = SimpleGraphOptions {
            channels: Some(2),
            sample_fmt: Some("s32".to_owned()),
            auto_conversion: true,
            ..SimpleGraphOptions::default()
        };
        let built = build(
            &opts,
            MediaType::Audio,
            None,
            Some(&audio_params()),
            Rational::new(1, 48_000),
            &[],
            &[],
        )
        .unwrap();
        let LinkFormat::Audio {
            layout,
            format,
            ..
        } = built.graph.sink_format(built.sink).unwrap()
        else {
            panic!("expected an audio link");
        };
        assert_eq!(layout.channels, 2);
        assert_eq!(*format, SampleFmt::S32);
    }

    /// With no `-sample_fmt`, the sink still falls back to the encoder's own
    /// accepted list — the graph path must not regress the sample-format
    /// safety the non-graph path already had (E2E-GAPS 3).
    #[test]
    fn sample_rate_request_still_falls_back_to_the_encoders_accepted_format() {
        let opts = SimpleGraphOptions {
            sample_rate: Some(44_100),
            auto_conversion: true,
            ..SimpleGraphOptions::default()
        };
        let built = build(
            &opts,
            MediaType::Audio,
            None,
            Some(&audio_params()),
            Rational::new(1, 48_000),
            &[],
            &[SampleFmt::F32P],
        )
        .unwrap();
        let LinkFormat::Audio { format, .. } = built.graph.sink_format(built.sink).unwrap() else {
            panic!("expected an audio link");
        };
        assert_eq!(*format, SampleFmt::F32P);
    }
}
