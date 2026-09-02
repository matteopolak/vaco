//! `-enc_time_base` (CL-21): the encoder's own time base.
//!
//! # Measured against the reference, and where its own `-h full` disagrees
//! # with its actual behaviour
//!
//! `ffmpeg 9.0.1 -h full` documents exactly two special values: `"0 = use
//! frame rate (video) or sample rate (audio), -1 = match source time base"`.
//! Measured directly (D6) on a real encode
//! (`ffmpeg -f lavfi -i … -enc_time_base:v <value> -c:v libx264 -f null -`):
//!
//! | value | `-h full` says | measured |
//! |---|---|---|
//! | `0` | media default | accepted |
//! | `-1` | match source time base | **refused**: `Invalid time base: -1` |
//! | `demux` | (undocumented) | accepted |
//! | `filter` | (undocumented) | accepted |
//! | an explicit rational (`1/24`) | — | accepted |
//! | anything else (`bogus`) | — | refused: `Invalid time base: bogus` |
//!
//! `-1` is real, run-ending output from the reference's own `--help`, and it
//! is simply wrong for this build of the reference — `planning/14-cli.md`
//! §6.4 Stage V's design-time table already named `demux`/`filter` as the
//! two named special values and never mentioned `-1` at all, so the
//! *implementation* plan matches the *measured* behaviour here; it is only
//! the reference's own generated `--help` text that is stale. Per D17 (match
//! the reference's actual behaviour, not its own claims about itself),
//! [`EncTimeBase::parse`] accepts `0`/`demux`/`filter`/an explicit rational
//! and refuses everything else, `-1` included, with the reference's own
//! wording.
//!
//! # What `demux` and `filter` resolve to here
//!
//! Nothing downstream of decode in this build currently tracks a
//! filtergraph's own output time base separately from the demuxer's — every
//! per-stream leg (`crate::exec::run_pipeline`) carries one `time_base`
//! value all the way from the demuxed stream's own declaration through
//! decode, conversion and any `-vf`/`-filter` graph. So `demux` and `filter`
//! are, honestly, the same value in this build today: both resolve to that
//! one carried `time_base`. A real, distinct filtergraph output time base
//! (for a graph whose sink negotiates a different one than its source) would
//! need `crate::filtergraph::SimpleGraph`/`crate::complexgraph::ComplexOutput`
//! to report it, which neither does at present.

use vaco_codec_core::{AudioParameters, VideoParameters};
use vaco_core::Rational;

/// `-enc_time_base`'s value, already validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncTimeBase {
    /// `0`: `1/frame_rate` for video, `1/sample_rate` for audio.
    Media,
    /// `demux`: the demuxed stream's own time base.
    Demux,
    /// `filter`: the filtergraph's output time base — see the module doc for
    /// why this build resolves it identically to [`EncTimeBase::Demux`].
    Filter,
    /// An explicit rational, already validated positive on both sides.
    Explicit(Rational),
}

impl EncTimeBase {
    /// Parse `-enc_time_base`'s argument.
    ///
    /// # Errors
    /// The reference's own `Invalid time base: <value>` wording — including
    /// for `-1`, which the reference's own `--help` claims is valid and its
    /// actual behaviour refuses; see the module doc.
    pub fn parse(s: &str) -> std::result::Result<Self, String> {
        match s {
            "0" => Ok(Self::Media),
            "demux" => Ok(Self::Demux),
            "filter" => Ok(Self::Filter),
            other => vaco_core::parse::rational(other)
                .filter(|r| r.num > 0 && r.den > 0)
                .map(Self::Explicit)
                .ok_or_else(|| format!("Invalid time base: {other}")),
        }
    }

    /// Resolve to the rational an `add_encoder` call should use.
    #[must_use]
    pub fn resolve(
        self,
        source_time_base: Rational,
        video: Option<&VideoParameters>,
        audio: Option<&AudioParameters>,
    ) -> Rational {
        match self {
            Self::Demux | Self::Filter => source_time_base,
            Self::Explicit(r) => r,
            Self::Media => {
                if let Some(v) = video
                    && v.frame_rate.num > 0
                {
                    v.frame_rate.inverse()
                } else if let Some(a) = audio
                    && a.sample_rate > 0
                {
                    Rational::new(1, i32::try_from(a.sample_rate).unwrap_or(i32::MAX))
                } else {
                    source_time_base
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn zero_demux_filter_and_a_rational_all_parse() {
        assert_eq!(EncTimeBase::parse("0").unwrap(), EncTimeBase::Media);
        assert_eq!(EncTimeBase::parse("demux").unwrap(), EncTimeBase::Demux);
        assert_eq!(EncTimeBase::parse("filter").unwrap(), EncTimeBase::Filter);
        assert_eq!(
            EncTimeBase::parse("1/24").unwrap(),
            EncTimeBase::Explicit(Rational::new(1, 24))
        );
    }

    /// Measured against the reference: `-1` is not a valid value on this
    /// build, despite `--help` claiming it is. See the module doc.
    #[test]
    fn minus_one_is_refused_like_the_reference_actually_refuses_it() {
        assert!(EncTimeBase::parse("-1").is_err());
        assert!(EncTimeBase::parse("bogus").is_err());
    }

    #[test]
    fn media_default_prefers_video_then_audio_then_source() {
        let tb = EncTimeBase::Media;
        let video = VideoParameters {
            frame_rate: Rational::new(30, 1),
            ..VideoParameters::default()
        };
        assert_eq!(
            tb.resolve(Rational::new(1, 90_000), Some(&video), None),
            Rational::new(1, 30)
        );
        let audio = AudioParameters {
            sample_rate: 48_000,
            ..AudioParameters::default()
        };
        assert_eq!(
            tb.resolve(Rational::new(1, 90_000), None, Some(&audio)),
            Rational::new(1, 48_000)
        );
        assert_eq!(
            tb.resolve(Rational::new(1, 90_000), None, None),
            Rational::new(1, 90_000)
        );
    }
}
