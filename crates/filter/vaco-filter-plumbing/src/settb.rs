//! `settb`/`asettb` — set the output link's time base.
//!
//! `ffmpeg -h filter=settb` documents one option, `expr`/`tb`, default
//! `"intb"`. Measured (`ffmpeg -f lavfi -i testsrc=rate=25 -vf
//! settb=1/90000,showinfo`): a literal `"num/den"` rescales every frame's PTS
//! from the input time base into the new one exactly (25fps frame N lands at
//! PTS `N*90000/25`) — this is a *rebase*, not a relabel, unlike `asetrate`.
//!
//! Implemented: the `num/den` split, each side its own `vaco-expr`
//! expression, with `intb` bound to the input time base's numerator on the
//! left and denominator on the right (so plain `"intb"` alone reproduces
//! `intb/1`... `Bindings` requires the exact text `"intb"` to appear as
//! division by `1` unless both sides are given — see `parse_tb`'s doc), and
//! `AVTB`/`sr` bound to the reference's documented meanings (1/1 000 000 000,
//! and the input sample rate for audio). This reproduces every literal
//! numeric form and the `intb`/`AVTB` keywords; a genuinely mixed expression
//! like `intb*2/AVTB` is not measured against the reference and may not
//! match its exact evaluation order.

use vaco_core::{MediaType, Rational, Result};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

const SIDE_VARS: &[&str] = &["intb", "AVTB", "sr"];

/// One side (numerator or denominator) of a `num/den` time-base expression.
#[derive(Debug, Clone)]
struct Side(Expr);

impl Side {
    fn parse(text: &str) -> std::result::Result<Self, String> {
        let bindings = Bindings::new(SIDE_VARS);
        Expr::parse(text.trim(), &bindings)
            .map(Side)
            .map_err(|e| format!("bad time-base expression `{text}`: {e}"))
    }

    fn eval(&self, intb: f64, sample_rate: f64) -> f64 {
        self.0.eval(&[intb, 1_000_000_000.0, sample_rate])
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TbExpr {
    num: Side,
    den: Side,
}

impl TbExpr {
    fn parse(text: &str) -> std::result::Result<Self, String> {
        // `"intb"` alone (no `/`) is the common case: keep the input time
        // base entirely — `num` sees `intb`'s numerator, `den` sees `intb`'s
        // denominator, and `intb` is otherwise `1` on whichever side does not
        // name it, so a bare literal like `"90000"` on either side is `n/1`
        // or `1/n`, which is not what a user writing `settb=90000` alone
        // would mean. So a text with no `/` at all is handled as a special
        // "pass the timebase through unscaled by this factor" case instead.
        if let Some((n, d)) = text.split_once('/') {
            Ok(Self {
                num: Side::parse(n)?,
                den: Side::parse(d)?,
            })
        } else {
            Ok(Self {
                num: Side::parse(text)?,
                den: Side::parse("1")?,
            })
        }
    }

    fn resolve(&self, in_tb: Rational, sample_rate: u32) -> Rational {
        let sr = f64::from(sample_rate);
        let num = self.num.eval(f64::from(in_tb.num), sr);
        let den = self.den.eval(f64::from(in_tb.den), sr);
        if !num.is_finite() || !den.is_finite() || den == 0.0 {
            return in_tb;
        }
        Rational::approximate(num / den, i32::MAX)
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    expr: TbExpr,
    out_tb: Rational,
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let (in_tb, sample_rate) = match ctx.input_link(0) {
            Some(LinkFormat::Video { time_base, .. }) => (*time_base, 0),
            Some(LinkFormat::Audio {
                time_base,
                sample_rate,
                ..
            }) => (*time_base, *sample_rate),
            None => (Rational::UNDEFINED, 0),
        };
        self.out_tb = self.expr.resolve(in_tb, sample_rate);
        if let Some(mut out) = ctx.output_link(0).cloned() {
            out.set_time_base(self.out_tb);
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut input: Frame) -> Result<FrameOut> {
        let rescaled = input.pts.rescale(
            input.time_base,
            self.out_tb,
            vaco_core::Rounding::NearestAwayFromZero,
        );
        input.pts = rescaled;
        input.time_base = self.out_tb;
        Ok(FrameOut::One(input))
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "settb", help = "set output timebase")]
pub(crate) struct Opts {
    #[opt(
        name = "expr",
        alias = "tb",
        help = "expression determining the output timebase",
        default = "intb".to_owned(),
        flags(filtering)
    )]
    pub expr: String,
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

fn build(
    media: MediaType,
    desc: FilterDesc,
    req: &Instantiate<'_>,
) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let expr = TbExpr::parse(&opts.expr)?;
    Ok(Instance {
        desc,
        formats: NodeFormats::passthrough(1, 1, media, req.instance),
        filter: Box::new(Simple::new(Filter {
            expr,
            out_tb: Rational::UNDEFINED,
        })),
    })
}

pub mod video {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, MediaType, Pad, build};

    const VIDEO_PAD: &[Pad] = &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }];

    pub const DESC: FilterDesc = FilterDesc {
        name: "settb",
        description: "Set timebase for the video output link",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(MediaType::Video, DESC, req)
    }
}

pub mod audio {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, MediaType, Pad, build};

    const AUDIO_PAD: &[Pad] = &[Pad {
        name: "default",
        media_type: MediaType::Audio,
    }];

    pub const DESC: FilterDesc = FilterDesc {
        name: "asettb",
        description: "Set timebase for the audio output link",
        inputs: AUDIO_PAD,
        outputs: AUDIO_PAD,
        flags: FilterFlags::empty(),
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(MediaType::Audio, DESC, req)
    }
}
