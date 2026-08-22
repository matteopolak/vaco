//! The auto-conversion **policy**: which converter fixes what, and what it
//! should produce.
//!
//! `vaco-filter-core` owns the mechanism — it finds the link with no common
//! format, coalesces every property that conflicts there into one request, and
//! splices in whatever comes back. It must not know that a filter called
//! `scale` exists, because layer 5a cannot depend on layer 5b. So the policy
//! lives here.
//!
//! # What gets inserted
//!
//! | Media | Properties | Converter |
//! |---|---|---|
//! | video | pixel format | `scale`, with the `sws_flags=` prefix as its arguments |
//! | audio | sample format, sample rate, channel layout | `aresample` |
//! | either | hardware context | **nothing** — see below |
//!
//! Coalescing is core's: a link that disagrees about sample format *and* rate
//! *and* layout gets one `aresample`, not three, because the factory is handed
//! the whole property set at once.
//!
//! **Hardware contexts are never auto-converted.** A CPU↔GPU link is an error
//! naming `hwupload`/`hwdownload`/`hwmap` as the fix, exactly as upstream.
//! Auto-inserting a device transfer would hide a per-frame `PCIe` round trip
//! behind a silent default. There is no hardware property in the frozen
//! `Property` enum yet, so today this is a statement of intent rather than a
//! branch — but the `None` return that produces the diagnostic is already the
//! default for anything not in the table.
//!
//! # What the converter should produce
//!
//! The chosen output format is **not** a guess. `vaco_filter_core::negotiate::loss`
//! carries a 35-row corpus measured against the reference, with the tier order
//! pinned by compile-time assertions:
//!
//! > chroma-total > alpha > depth > colour model > chroma coarsening > packing
//!
//! This module does not re-derive any of it. It asks `loss::best_video`,
//! `loss::best_audio_format` and `loss::best_rate` which of the downstream's
//! accepted values costs least, coming from what upstream offers.
//!
//! # Where the options come from
//!
//! Three sources, in increasing precedence, matching upstream:
//!
//! 1. the application's `-sws_flags` / `-aresample_swr_opts`;
//! 2. the `sws_flags=…;` graph-string prefix, which `vaco-filter-graph` parses
//!    and hands to every auto-inserted `scale` in that graph;
//! 3. nothing per link — auto-inserted converters are not individually
//!    addressable. A user who needs per-link control writes `scale` explicitly,
//!    which is the documented answer upstream too.

use vaco_chlayout::ChannelLayout;
use vaco_core::MediaType;
use vaco_filter_core::negotiate::{
    Constraint, ConverterFactory, ConverterSpec, FormatSet, NodeFormats, Property, loss,
};

/// The filter that fixes video format mismatches.
pub const VIDEO_CONVERTER: &str = "scale";
/// The filter that fixes audio format, rate and layout mismatches.
pub const AUDIO_CONVERTER: &str = "aresample";

/// The default policy: `scale` for video, `aresample` for audio.
#[derive(Debug, Clone, Default)]
pub struct DefaultConverters {
    /// Applied to every auto-inserted `scale`. From `-sws_flags` and the
    /// `sws_flags=` graph prefix.
    pub sws_opts: String,
    /// Applied to every auto-inserted `aresample`. From `-aresample_swr_opts`.
    pub swr_opts: String,
}

impl DefaultConverters {
    /// Build a policy with the options an auto-inserted converter should carry.
    #[must_use]
    pub const fn new(sws_opts: String, swr_opts: String) -> Self {
        Self { sws_opts, swr_opts }
    }

    /// The arguments an auto-inserted `filter` should be given.
    ///
    /// Needed because `Graph::configure_converting` rebuilds the
    /// [`ConverterSpec`] with an empty `args` before handing it to the builder,
    /// so the arguments this factory produced never reach the filter. Recorded
    /// as a signature gap in `docs/filter/vaco-filter-graph.md`.
    #[must_use]
    pub fn args_for(&self, filter: &str) -> String {
        match filter {
            VIDEO_CONVERTER => self.sws_opts.clone(),
            AUDIO_CONVERTER => self.swr_opts.clone(),
            _ => String::new(),
        }
    }
}

impl ConverterFactory for DefaultConverters {
    fn converter(
        &self,
        media: MediaType,
        properties: &[Property],
        upstream: &FormatSet,
        downstream: &FormatSet,
    ) -> Option<ConverterSpec> {
        let filter = match media {
            MediaType::Video => VIDEO_CONVERTER,
            MediaType::Audio => AUDIO_CONVERTER,
            // Subtitle, data and attachment pads carry nothing negotiable, and
            // a hardware-context mismatch is deliberately not repaired.
            _ => return None,
        };
        if !properties
            .iter()
            .any(|p| Property::for_media(media).contains(p))
        {
            return None;
        }
        let output = target(media, upstream, downstream);
        Some(ConverterSpec {
            filter,
            args: self.args_for(filter),
            formats: NodeFormats::converter(upstream.clone(), output, ""),
        })
    }
}

/// What the converter's output pad should declare.
///
/// One resolved value per property the downstream constrains, chosen by the
/// measured loss model; properties the downstream leaves open are copied from
/// upstream so the converter is a no-op for them.
fn target(media: MediaType, upstream: &FormatSet, downstream: &FormatSet) -> FormatSet {
    let mut out = FormatSet::default();
    for property in Property::for_media(media) {
        match property {
            Property::PixelFormat => {
                out.pixel_formats = pick(
                    upstream.pixel_formats.as_ref(),
                    downstream.pixel_formats.as_ref(),
                    loss::best_video,
                );
            }
            Property::SampleFormat => {
                out.sample_formats = pick(
                    upstream.sample_formats.as_ref(),
                    downstream.sample_formats.as_ref(),
                    loss::best_audio_format,
                );
            }
            Property::SampleRate => {
                out.sample_rates = pick(
                    upstream.sample_rates.as_ref(),
                    downstream.sample_rates.as_ref(),
                    loss::best_rate,
                );
            }
            Property::ChannelLayout => {
                out.channel_layouts = pick_layout(
                    upstream.channel_layouts.as_ref(),
                    downstream.channel_layouts.as_ref(),
                );
            }
        }
    }
    out
}

/// Resolve one property to a single value the converter will produce.
fn pick<T, F>(
    up: Option<&Constraint<T>>,
    down: Option<&Constraint<T>>,
    best: F,
) -> Option<Constraint<T>>
where
    T: Clone + PartialEq,
    F: Fn(T, &[T]) -> Option<T>,
{
    // `None` and `Constraint::Any` both mean "downstream accepts anything":
    // pass upstream's choice through, so the converter does not silently change
    // a property nobody asked about.
    let candidates = down.map_or(&[][..], Constraint::candidates);
    if candidates.is_empty() {
        return up.cloned();
    }
    // The source's own value is whatever negotiation would pick upstream: the
    // first candidate in preference order.
    let from = up.and_then(|c| c.candidates().first().cloned());
    let chosen = match from {
        Some(f) => best(f, candidates).or_else(|| candidates.first().cloned()),
        None => candidates.first().cloned(),
    };
    chosen.map(Constraint::Exact)
}

/// Channel layouts have no measured loss model yet, so the rule is the simple
/// one the reference also starts from: keep the source layout if the
/// destination accepts it, otherwise take the destination's first preference.
fn pick_layout(
    up: Option<&Constraint<ChannelLayout>>,
    down: Option<&Constraint<ChannelLayout>>,
) -> Option<Constraint<ChannelLayout>> {
    let candidates = down.map_or(&[][..], Constraint::candidates);
    if candidates.is_empty() {
        return up.cloned();
    }
    let from = up.and_then(|c| c.candidates().first());
    if let Some(f) = from
        && candidates.contains(f)
    {
        return Some(Constraint::Exact(f.clone()));
    }
    candidates.first().cloned().map(Constraint::Exact)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use vaco_pixfmt::PixFmt;
    use vaco_sampfmt::SampleFmt;

    use super::*;

    fn spec(media: MediaType, up: &FormatSet, down: &FormatSet) -> Option<ConverterSpec> {
        DefaultConverters::default().converter(media, Property::for_media(media), up, down)
    }

    #[test]
    fn video_mismatches_get_a_scale() {
        let s = spec(
            MediaType::Video,
            &FormatSet::video_exact(PixFmt::Rgb24),
            &FormatSet::video_exact(PixFmt::Gray8),
        )
        .unwrap();
        assert_eq!(s.filter, VIDEO_CONVERTER);
        assert_eq!(
            s.formats.outputs[0].pixel_formats,
            Some(Constraint::Exact(PixFmt::Gray8))
        );
    }

    #[test]
    fn audio_mismatches_get_one_aresample_for_all_three_properties() {
        let s = spec(
            MediaType::Audio,
            &FormatSet::audio_exact(SampleFmt::S16, 44100, ChannelLayout::MONO),
            &FormatSet::audio_exact(SampleFmt::F32P, 48000, ChannelLayout::MONO),
        )
        .unwrap();
        assert_eq!(s.filter, AUDIO_CONVERTER);
        let out = &s.formats.outputs[0];
        assert_eq!(out.sample_formats, Some(Constraint::Exact(SampleFmt::F32P)));
        assert_eq!(out.sample_rates, Some(Constraint::Exact(48000)));
    }

    #[test]
    fn the_converter_ties_nothing() {
        // A converter with tied pads cannot converge; the termination argument
        // in `vaco-filter-core` rests on this.
        let s = spec(
            MediaType::Video,
            &FormatSet::video_exact(PixFmt::Rgb24),
            &FormatSet::video_exact(PixFmt::Gray8),
        )
        .unwrap();
        assert!(s.formats.ties.is_empty());
    }

    #[test]
    fn the_cheapest_candidate_wins_and_it_is_the_measured_one() {
        // From `yuv444p10le`, one bit of depth costs more than a whole colour
        // model: `loss` measured the reference taking `rgb48le`.
        let s = spec(
            MediaType::Video,
            &FormatSet::video_exact(PixFmt::Yuv444p10le),
            &FormatSet::video_list([PixFmt::Yuv444p9le, PixFmt::Rgb48le]),
        )
        .unwrap();
        assert_eq!(
            s.formats.outputs[0].pixel_formats,
            Some(Constraint::Exact(PixFmt::Rgb48le))
        );
    }

    #[test]
    fn an_unconstrained_downstream_passes_the_source_through() {
        let s = spec(
            MediaType::Video,
            &FormatSet::video_exact(PixFmt::Rgb24),
            &FormatSet::default(),
        )
        .unwrap();
        assert_eq!(
            s.formats.outputs[0].pixel_formats,
            Some(Constraint::Exact(PixFmt::Rgb24))
        );
    }

    #[test]
    fn subtitle_pads_are_never_converted() {
        assert!(
            DefaultConverters::default()
                .converter(
                    MediaType::Subtitle,
                    &[Property::PixelFormat],
                    &FormatSet::default(),
                    &FormatSet::default()
                )
                .is_none()
        );
    }

    #[test]
    fn the_sws_prefix_reaches_the_scale_it_is_meant_for() {
        let f = DefaultConverters::new("bicubic+accurate_rnd".into(), "resampler=soxr".into());
        assert_eq!(f.args_for(VIDEO_CONVERTER), "bicubic+accurate_rnd");
        assert_eq!(f.args_for(AUDIO_CONVERTER), "resampler=soxr");
        assert_eq!(f.args_for("hflip"), "");
    }
}
