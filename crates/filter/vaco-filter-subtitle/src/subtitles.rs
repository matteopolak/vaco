//! `subtitles` — like `ass`, but dispatches on the file extension: a
//! `.ass`/`.ssa` file gets the full typeset renderer
//! ([`crate::ass_filter::render_at`]); anything else falls back to the
//! "layout-and-draw" path plan 16 SS6.3 describes for SRT/`WebVTT`/
//! `MicroDVD`/SAMI/plain `SubStation` — this pass implements that fallback
//! for **SRT only** ([`crate::text`]'s bottom-centred simple-text
//! rendering over [`vaco_format_subtitle`]'s own SRT timing parser).
//! `WebVTT`/`MicroDVD`/SAMI are a real, stated gap, not attempted: each
//! needs its own cue-splitting rule and none is implemented here.

use vaco_core::{Duration, MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_filter_text::TextRenderer;
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::text::{SimpleTextStyle, composite_simple_text};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "subtitles",
    description: "Render a subtitle file onto the input video.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(
    name = "subtitles",
    help = "Render a subtitle file onto the input video"
)]
pub(crate) struct Opts {
    #[opt(name = "filename", alias = "f", help = "set the subtitle file to render", default = String::new(), flags(video, filtering))]
    pub filename: String,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

struct SrtCue {
    start: Duration,
    end: Duration,
    text: String,
}

fn frame_time(pts: vaco_core::Timestamp, time_base: vaco_core::Rational) -> Duration {
    pts.to_duration(time_base).unwrap_or(Duration::ZERO)
}

fn parse_srt(text: &str) -> Vec<SrtCue> {
    let mut cues = Vec::new();
    for block in text.split("\r\n\r\n").flat_map(|b| b.split("\n\n")) {
        let mut lines = block.lines();
        let mut timing = None;
        let mut body_lines: Vec<&str> = Vec::new();
        for line in &mut lines {
            if let Some((start, end)) =
                vaco_format_subtitle::time::parse_srt_timing_line(line.trim())
            {
                timing = Some((start, end));
                break;
            }
        }
        if timing.is_none() {
            continue;
        }
        for line in lines {
            body_lines.push(line);
        }
        let Some((start, end)) = timing else { continue };
        cues.push(SrtCue {
            start,
            end,
            text: body_lines.join("\n"),
        });
    }
    cues
}

enum Source {
    Ass(vaco_ass::Script),
    Srt(Vec<SrtCue>),
}

pub(crate) struct Filter {
    source: Source,
    renderer: TextRenderer,
}

impl Filter {
    fn new(opts: &Opts) -> std::result::Result<Self, String> {
        if opts.filename.is_empty() {
            return Err("subtitles: filename is required".to_owned());
        }
        let bytes = std::fs::read(&opts.filename)
            .map_err(|e| format!("subtitles: could not read `{}`: {e}", opts.filename))?;
        let (utf8, _) = vaco_format_subtitle::encoding::decode_to_utf8_bytes(&bytes);
        let text = String::from_utf8_lossy(&utf8).into_owned();
        let ext = std::path::Path::new(&opts.filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let source = if ext == "ass" || ext == "ssa" {
            Source::Ass(vaco_ass::parse(&text))
        } else {
            Source::Srt(parse_srt(&text))
        };
        Ok(Self {
            source,
            renderer: TextRenderer::new(),
        })
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { height, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        let dur = frame_time(input.pts, input.time_base);
        let mut out = input;
        match &self.source {
            Source::Ass(script) => {
                crate::ass_filter::render_at(script, &mut self.renderer, &mut out, dur)?;
            }
            Source::Srt(cues) => {
                let style = SimpleTextStyle::for_frame_height(height);
                for cue in cues {
                    if cue.start <= dur && dur < cue.end {
                        composite_simple_text(&mut self.renderer, &mut out, &cue.text, &style)?;
                    }
                }
            }
        }
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, "subtitles"),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn frame_time_retains_an_awkward_input_clock() {
        let ticks = 9_007_199_254_740_993_i64;
        let base = vaco_core::Rational::new(1_001, 30_000);
        assert_eq!(
            frame_time(vaco_core::Timestamp::new(ticks), base),
            Duration::from_ticks(ticks, base).unwrap_or(Duration::ZERO)
        );
    }

    #[test]
    fn missing_filename_is_a_clean_error() {
        let req = Instantiate {
            name: "subtitles",
            instance: "subtitles",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }

    #[test]
    fn parses_a_two_cue_srt_file() {
        let srt = "1\n00:00:01,000 --> 00:00:04,000\nHello\n\n2\n00:00:05,000 --> 00:00:08,000\nWorld\nSecond line\n";
        let cues = parse_srt(srt);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "Hello");
        assert_eq!(cues[1].text, "World\nSecond line");
    }

    #[test]
    fn srt_extension_selects_the_simple_text_path() {
        let dir = std::env::temp_dir();
        let path = dir.join("vaco_subtitles_test.srt");
        std::fs::write(&path, "1\n00:00:00,000 --> 00:00:05,000\nHi\n").unwrap();
        let req = Instantiate {
            name: "subtitles",
            instance: "subtitles",
            args: Some(&format!("filename={}", path.display())),
            arguments: &[],
        };
        assert!(create(&req).is_ok());
        let _ = std::fs::remove_file(&path);
    }
}
