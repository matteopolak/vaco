//! Turn one `Representation`'s `Addressing` into an ordered list of media
//! segments — the point where `SegmentTemplate`/`SegmentList`/`SegmentBase`
//! stop being three different XML shapes and become one
//! `vaco_demux_hls`-shaped segment list.

use vaco_core::{Duration, Error, Result};
use vaco_format_adaptive::ByteRange;
use vaco_limits::Budget;

use crate::mpd::{Addressing, Representation};

/// One DASH media segment, already resolved to an absolute URL.
#[derive(Debug, Clone)]
pub struct DashSegment {
    pub uri: String,
    pub duration: Duration,
    pub byte_range: Option<ByteRange>,
}

/// The one initialization segment a representation's media segments need
/// read first, when addressing states one.
#[derive(Debug, Clone)]
pub struct InitSegment {
    pub uri: String,
    pub byte_range: Option<ByteRange>,
}

/// Enumerate `representation`'s segments, resolving every URL against
/// `base_url` (the effective `BaseURL` chain: MPD -> Period -> `AdaptationSet`
/// -> Representation, already folded into one string by the caller — see
/// `crate::demux::effective_base_url`).
///
/// `period_end`, in the template's own timescale ticks, resolves a final
/// `SegmentTimeline` entry's `r="-1"` (see
/// `vaco_format_adaptive::timeline::expand`); pass `None` for a period with
/// no stated duration.
///
/// # Errors
/// [`Error::Unsupported`] when `representation` states no addressing at all
/// (a schema violation this crate declines to guess at); whatever
/// `vaco_format_adaptive::timeline::expand` reports for a `SegmentTimeline`
/// that is itself invalid or too large.
pub fn enumerate(
    representation: &Representation,
    base_url: &str,
    period_end_seconds: Option<f64>,
    budget: &mut Budget,
) -> Result<(Option<InitSegment>, Vec<DashSegment>)> {
    let Addressing {
        template,
        list,
        base,
    } = &representation.addressing;

    if let Some(t) = template {
        return enumerate_template(representation, t, base_url, period_end_seconds, budget);
    }
    if let Some(l) = list {
        return Ok(enumerate_list(representation, l, base_url));
    }
    if let Some(b) = base {
        return Ok(enumerate_base(representation, b, base_url));
    }
    Err(Error::Unsupported(
        "DASH Representation names no SegmentTemplate, SegmentList or SegmentBase",
    ))
}

fn resolve(base: &str, reference: &str) -> String {
    vaco_format_adaptive::resolve(base, reference)
}

fn enumerate_template(
    representation: &Representation,
    template: &crate::mpd::SegmentTemplate,
    base_url: &str,
    period_end_seconds: Option<f64>,
    budget: &mut Budget,
) -> Result<(Option<InitSegment>, Vec<DashSegment>)> {
    let timescale = template.timescale.max(1);
    let init = template.initialization.as_ref().map(|pattern| {
        let name = crate::mpd::substitute(
            pattern,
            &representation.id,
            representation.bandwidth,
            None,
            None,
        );
        InitSegment {
            uri: resolve(base_url, &name),
            byte_range: None,
        }
    });
    let Some(media_pattern) = &template.media else {
        return Ok((init, Vec::new()));
    };

    let timings: Vec<vaco_format_adaptive::SegmentTiming> =
        if let Some(entries) = &template.timeline {
            let period_end = period_end_seconds.map(|s| (s * timescale as f64) as u64);
            vaco_format_adaptive::expand(entries, period_end, budget)?
        } else {
            // No SegmentTimeline: segments are `duration` ticks apart, from
            // `startNumber`, for as many as the period's stated length allows.
            // A live MPD with neither a SegmentTimeline nor a stated period
            // duration cannot be enumerated without the wall clock; this
            // returns an empty list rather than guessing (see the crate docs).
            let Some(dur) = template.duration else {
                return Ok((init, Vec::new()));
            };
            let Some(period_secs) = period_end_seconds else {
                return Ok((init, Vec::new()));
            };
            let total_ticks = (period_secs * timescale as f64) as u64;
            let count = total_ticks.div_ceil(dur.max(1));
            budget.consume_fuel(count)?;
            (0..count)
                .map(|i| vaco_format_adaptive::SegmentTiming {
                    start: i.saturating_mul(dur),
                    duration: dur,
                })
                .collect()
        };

    let mut out = Vec::new();
    for (i, timing) in timings.iter().enumerate() {
        let number = template.start_number.saturating_add(i as u64);
        let name = crate::mpd::substitute(
            media_pattern,
            &representation.id,
            representation.bandwidth,
            Some(number),
            Some(timing.start),
        );
        out.push(DashSegment {
            uri: resolve(base_url, &name),
            duration: ticks_to_duration(timing.duration, timescale),
            byte_range: None,
        });
    }
    Ok((init, out))
}

fn enumerate_list(
    representation: &Representation,
    list: &crate::mpd::SegmentList,
    base_url: &str,
) -> (Option<InitSegment>, Vec<DashSegment>) {
    let _ = representation;
    let init = list
        .initialization
        .as_ref()
        .map(|(uri, range)| InitSegment {
            uri: resolve(base_url, uri),
            byte_range: *range,
        });
    let timescale = list.timescale.max(1);
    let duration_ticks = list.duration.unwrap_or(0);
    let segments = list
        .urls
        .iter()
        .map(|u| DashSegment {
            uri: resolve(base_url, &u.media),
            duration: ticks_to_duration(duration_ticks, timescale),
            byte_range: u.media_range,
        })
        .collect();
    (init, segments)
}

fn enumerate_base(
    representation: &Representation,
    base: &crate::mpd::SegmentBase,
    base_url: &str,
) -> (Option<InitSegment>, Vec<DashSegment>) {
    // `SegmentBase` addresses one whole file per representation, indexed by
    // a `sidx` box referenced through `indexRange` for byte-accurate
    // sub-segment seeking. Parsing `sidx` is not implemented (see the crate
    // docs): this reports the representation as a single segment covering
    // the whole file, which a caller gets a correct — if coarse-grained —
    // read from, and loses only the ability to seek within it without
    // decoding forward.
    let uri = representation
        .base_url
        .clone()
        .map_or_else(|| base_url.to_owned(), |b| resolve(base_url, &b));
    let init = base
        .initialization
        .as_ref()
        .map(|(src, range)| InitSegment {
            uri: src
                .clone()
                .map_or_else(|| uri.clone(), |s| resolve(base_url, &s)),
            byte_range: *range,
        });
    (
        init,
        vec![DashSegment {
            uri,
            duration: Duration::ZERO,
            byte_range: None,
        }],
    )
}

fn ticks_to_duration(ticks: u64, timescale: u64) -> Duration {
    Duration::from_fraction(i128::from(ticks), i128::from(timescale.max(1)))
        .unwrap_or(Duration::ZERO)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::mpd::{Addressing, SegmentList, SegmentTemplate, SegmentUrl};
    use vaco_format_adaptive::TimelineEntry;
    use vaco_limits::Limits;

    #[test]
    fn ticks_retain_submicrosecond_timescales() {
        assert_eq!(ticks_to_duration(1, 10_000_000).as_ratio(), (1, 10_000_000));
    }

    fn rep(addressing: Addressing) -> Representation {
        Representation {
            id: "v0".to_owned(),
            bandwidth: 500_000,
            width: None,
            height: None,
            frame_rate: None,
            codecs: Vec::new(),
            mime_type: None,
            base_url: None,
            addressing,
        }
    }

    #[test]
    fn template_with_timeline_produces_one_segment_per_expanded_entry() {
        let template = SegmentTemplate {
            media: Some("s-$Time$.m4s".to_owned()),
            initialization: Some("init.m4s".to_owned()),
            timescale: 1000,
            duration: None,
            start_number: 1,
            timeline: Some(vec![TimelineEntry {
                t: Some(0),
                d: 2000,
                r: Some(2),
            }]),
        };
        let r = rep(Addressing {
            template: Some(template),
            ..Addressing::default()
        });
        let mut b = Budget::new(Limits::permissive());
        let (init, segs) = enumerate(&r, "http://a/", None, &mut b).unwrap();
        assert_eq!(init.unwrap().uri, "http://a/init.m4s");
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].uri, "http://a/s-0.m4s");
        assert_eq!(segs[1].uri, "http://a/s-2000.m4s");
        assert_eq!(segs[2].uri, "http://a/s-4000.m4s");
        assert_eq!(segs[0].duration.as_micros(), 2_000_000);
    }

    #[test]
    fn template_without_timeline_uses_period_duration_and_number() {
        let template = SegmentTemplate {
            media: Some("c-$Number%03d$.m4s".to_owned()),
            initialization: None,
            timescale: 1,
            duration: Some(2),
            start_number: 1,
            timeline: None,
        };
        let r = rep(Addressing {
            template: Some(template),
            ..Addressing::default()
        });
        let mut b = Budget::new(Limits::permissive());
        let (_, segs) = enumerate(&r, "http://a/", Some(6.0), &mut b).unwrap();
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].uri, "http://a/c-001.m4s");
        assert_eq!(segs[2].uri, "http://a/c-003.m4s");
    }

    #[test]
    fn segment_list_carries_its_own_byte_ranges() {
        let list = SegmentList {
            duration: Some(2),
            timescale: 1,
            initialization: Some((
                "init.mp4".to_owned(),
                Some(ByteRange {
                    offset: 0,
                    length: 800,
                }),
            )),
            urls: vec![
                SegmentUrl {
                    media: "chunk.mp4".to_owned(),
                    media_range: Some(ByteRange {
                        offset: 800,
                        length: 500,
                    }),
                },
                SegmentUrl {
                    media: "chunk.mp4".to_owned(),
                    media_range: Some(ByteRange {
                        offset: 1300,
                        length: 500,
                    }),
                },
            ],
        };
        let r = rep(Addressing {
            list: Some(list),
            ..Addressing::default()
        });
        let mut b = Budget::new(Limits::permissive());
        let (init, segs) = enumerate(&r, "http://a/", None, &mut b).unwrap();
        assert_eq!(
            init.unwrap().byte_range,
            Some(ByteRange {
                offset: 0,
                length: 800
            })
        );
        assert_eq!(segs.len(), 2);
        assert_eq!(
            segs[1].byte_range,
            Some(ByteRange {
                offset: 1300,
                length: 500
            })
        );
    }

    #[test]
    fn no_addressing_at_all_is_unsupported() {
        let r = rep(Addressing::default());
        let mut b = Budget::new(Limits::permissive());
        assert!(enumerate(&r, "http://a/", None, &mut b).is_err());
    }

    #[test]
    fn live_template_with_no_timeline_and_no_period_duration_is_empty_not_a_guess() {
        let template = SegmentTemplate {
            media: Some("c-$Number$.m4s".to_owned()),
            initialization: None,
            timescale: 1,
            duration: Some(2),
            start_number: 1,
            timeline: None,
        };
        let r = rep(Addressing {
            template: Some(template),
            ..Addressing::default()
        });
        let mut b = Budget::new(Limits::permissive());
        let (_, segs) = enumerate(&r, "http://a/", None, &mut b).unwrap();
        assert!(segs.is_empty());
    }
}
