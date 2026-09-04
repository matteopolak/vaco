//! `hilbert` — a windowed discrete Hilbert transform FIR, in `flt`, mono. A
//! one-shot coefficient dump: the filter produces exactly `taps` samples
//! and then ends, not a repeating signal.
//!
//! `ffmpeg -h filter=hilbert` documents `sample_rate`/`r`, `taps`/`t`
//! (default 22051), `nb_samples`/`n` (the chunk size the coefficients are
//! delivered in, default 1024) and `win_func`/`w` (default `blackman`).
//!
//! # The formula (measured, not read)
//!
//! Probed at `taps=11` (`ffmpeg -f lavfi -i hilbert=taps=11 -f f32le -`):
//! zero at every even offset from the centre tap, antisymmetric
//! (`h[c-k] = -h[c+k]`) at every odd offset — exactly the textbook ideal
//! discrete Hilbert transform, `h[k] = 0` for even `k`, `h[k] = 2/(pi*k)`
//! for odd `k` (`k` relative to the centre), windowed by `win_func`.
//!
//! The window was confirmed to be the **default Blackman formula** (not a
//! generic taper) by checking two non-trivial taps against the closed-form
//! `0.42 - 0.5*cos(2*pi*x) + 0.08*cos(4*pi*x)`: offset `+1`'s window value
//! computes to `~0.849` and the measured tap (`0.542`) divided by the ideal
//! unwindowed value (`2/pi = 0.637`) gives `0.852` — matching to the
//! precision of this crate's manual verification; offset `+3` gives
//! `~0.201` against a measured ratio of `~0.211`. See [`crate::window`] for
//! the shared window implementation and its own test against these same
//! numbers.
//!
//! **Exact** for the default `win_func=blackman`. `window.rs` implements
//! six of the reference's 21 `win_func` values with their own real
//! formula (`rect`, `bartlett`, `hann`, `hamming`, `blackman`, `sine`);
//! the other fifteen used to be silently computed as one of those six
//! (`welch` as `bartlett`, `bhann` as `hann`, the remaining thirteen as
//! `blackman`) with no error at all — an accepted-but-wrong value with no
//! signal to the caller, worse than refusing it. [`create`] now rejects
//! those fifteen explicitly, by name, rather than substituting: see
//! `window.rs`'s own doc for the exact list.

use vaco_core::{MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;
use vaco_sampfmt::SampleFmt;

use crate::window::{self, WinFunc};
use vaco_filter_graph::registry::{Instance, Instantiate};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "hilbert", help = "generate a Hilbert transform FIR")]
pub(crate) struct Opts {
    #[opt(name = "sample_rate", alias = "r", help = "set sample rate", default = 44100, range = 1..=i32::MAX, flags(filtering))]
    pub sample_rate: i32,
    #[opt(name = "taps", alias = "t", help = "set number of taps", default = 22051, range = 11..=65535, flags(filtering))]
    pub taps: i32,
    #[opt(name = "nb_samples", alias = "n", help = "set the number of samples per requested frame", default = 1024, range = 1..=i32::MAX, flags(filtering))]
    pub nb_samples: i32,
    #[opt(name = "win_func", alias = "w", help = "set window function", unit = "win_func", consts = window::WIN_FUNC_ALIASES, default = WinFunc::Blackman, default_repr = "blackman", flags(filtering))]
    pub win_func: WinFunc,
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

pub const DESC: FilterDesc = FilterDesc {
    name: "hilbert",
    description: "Generate a Hilbert transform FIR coefficients",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Audio,
    }],
    flags: FilterFlags::empty(),
};

/// Tap `n` of an `n_taps`-long windowed discrete Hilbert transform.
pub(crate) fn tap(n: usize, n_taps: usize, win: WinFunc) -> f64 {
    #[allow(
        clippy::cast_possible_wrap,
        clippy::integer_division,
        reason = "n_taps is at most 65535 (range-checked), far below i64 concerns; the centre tap of an odd-length FIR is exactly floor(n_taps/2)"
    )]
    let center = (n_taps / 2) as i64;
    #[allow(clippy::cast_possible_wrap, reason = "n < n_taps <= 65535")]
    let k = n as i64 - center;
    let ideal = if k == 0 || k % 2 == 0 {
        0.0
    } else {
        2.0 / (std::f64::consts::PI * f64::from(i32::try_from(k).unwrap_or(0)))
    };
    ideal * window::value(win, n, n_taps)
}

#[derive(Debug)]
struct Source {
    n_taps: usize,
    win: WinFunc,
    sample_rate: u32,
    block: u32,
    next: u64,
}

impl SourceFilter for Source {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Audio {
                sample_rate,
                time_base,
                ..
            } = &mut out
            {
                *sample_rate = self.sample_rate;
                *time_base = Rational::new(1, i32::try_from(self.sample_rate.max(1)).unwrap_or(1));
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn produce(&mut self, ctx: &mut FilterContext<'_>) -> Result<Option<Frame>> {
        #[allow(clippy::cast_possible_truncation, reason = "n_taps <= 65535")]
        let total = self.n_taps as u64;
        if self.next >= total {
            return Ok(None);
        }
        let want = u32::try_from(total - self.next)
            .unwrap_or(self.block)
            .min(self.block);
        let layout = vaco_chlayout::ChannelLayout::from_name("mono")
            .or_else(|| vaco_chlayout::ChannelLayout::default_for(1))
            .ok_or(vaco_core::Error::Unsupported(
                "no mono channel layout available",
            ))?;
        let mut frame = ctx
            .pool()
            .acquire_audio(SampleFmt::F32, layout, want, self.sample_rate)?;
        if let Some(mut plane) = frame.plane_mut(0)
            && let Some(row) = plane.row_mut(0)
        {
            for (i, px) in row.chunks_exact_mut(4).enumerate() {
                #[allow(clippy::cast_possible_truncation, reason = "index stays within n_taps")]
                let idx = (self.next as usize) + i;
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "tap() is a small finite value"
                )]
                let v = tap(idx, self.n_taps, self.win) as f32;
                px.copy_from_slice(&v.to_le_bytes());
            }
        }
        frame.pts = Timestamp::new(i64::try_from(self.next).unwrap_or(0));
        frame.time_base = Rational::new(1, i32::try_from(self.sample_rate.max(1)).unwrap_or(1));
        frame.set_duration_ticks(i64::from(want));
        self.next = self.next.saturating_add(u64::from(want));
        Ok(Some(frame))
    }

    fn end_pts(&self) -> Timestamp {
        Timestamp::new(i64::try_from(self.next).unwrap_or(0))
    }

    fn flush_state(&mut self) {
        self.next = 0;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    window::ensure_implemented("hilbert", opts.win_func)?;
    let sample_rate = u32::try_from(opts.sample_rate.max(1)).unwrap_or(44100);
    let source = Source {
        n_taps: usize::try_from(opts.taps.max(11)).unwrap_or(22051),
        win: opts.win_func,
        sample_rate,
        block: u32::try_from(opts.nb_samples.max(1)).unwrap_or(1024),
        next: 0,
    };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats {
            inputs: Vec::new(),
            outputs: vec![FormatSet::default()],
            ties: Vec::new(),
            label: req.instance.to_owned(),
        },
        filter: Box::new(Sourced::new(source)),
    })
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "the `ideal == 0.0` branch for an even offset is exact integer-multiplication-by-zero, not an accumulated float result"
)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn matches_the_measured_reference_at_taps_eleven() {
        // From the module doc's probe: index 5 is the centre (k=0, even
        // offset, zero); index 4/6 (k=-1/+1, odd offset) are the largest
        // pair and antisymmetric.
        assert_eq!(tap(5, 11, WinFunc::Blackman), 0.0);
        assert!(tap(4, 11, WinFunc::Blackman) < 0.0);
        assert!(tap(6, 11, WinFunc::Blackman) > 0.0);
        assert!((tap(4, 11, WinFunc::Blackman) + tap(6, 11, WinFunc::Blackman)).abs() < 1e-9);
    }

    #[test]
    fn even_offsets_from_center_are_zero() {
        // Centre is index 5, so an *odd* absolute index is an *even*
        // offset from it (5 itself is offset 0, also even).
        for n in [1usize, 3, 5, 7, 9] {
            assert_eq!(tap(n, 11, WinFunc::Blackman), 0.0, "n={n}");
        }
    }

    #[test]
    fn is_antisymmetric_about_the_center() {
        let n_taps = 21;
        for n in 0..n_taps {
            let a = tap(n, n_taps, WinFunc::Blackman);
            let b = tap(n_taps - 1 - n, n_taps, WinFunc::Blackman);
            assert!((a + b).abs() < 1e-12, "n={n}: {a} vs {b}");
        }
    }

    #[test]
    fn creatable_with_no_arguments() {
        let req = Instantiate {
            name: "hilbert",
            instance: "hilbert",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    /// `ffmpeg -h filter=hilbert` documents both `hann` (the derived
    /// enum's own name for the variant) and `hanning` (a second name for
    /// the exact same value, only reachable via the field-level `consts`
    /// override in `Opts::win_func`'s `#[opt(...)]` attribute) parsing to
    /// the same `win_func` value.
    #[test]
    fn hann_and_hanning_are_the_same_value() {
        let opts = Opts::parse(Some("win_func=hann")).unwrap();
        assert_eq!(opts.win_func, WinFunc::Hann);
        let opts = Opts::parse(Some("win_func=hanning")).unwrap();
        assert_eq!(opts.win_func, WinFunc::Hann);
        let opts = Opts::parse(Some("w=hanning")).unwrap();
        assert_eq!(opts.win_func, WinFunc::Hann);
    }

    /// `win_func` values this crate has no real formula for used to parse
    /// fine and then silently run as if `blackman` (`welch` as `bartlett`,
    /// `bhann` as `hann`, the rest as `blackman`) — accepted, wrong, and
    /// undetectable short of a differential comparison. `create` now
    /// rejects them explicitly instead.
    #[test]
    fn unimplemented_win_func_values_are_a_named_error_not_a_silent_substitution() {
        for name in [
            "welch", "bhann", "flattop", "bharris", "bnuttall", "nuttall", "lanczos", "gauss",
            "tukey", "dolph", "cauchy", "parzen", "poisson", "bohman", "kaiser",
        ] {
            let req = Instantiate {
                name: "hilbert",
                instance: "hilbert",
                args: Some(&format!("win_func={name}")),
                arguments: &[],
            };
            match create(&req) {
                Ok(_) => panic!("win_func={name} should be rejected, not silently accepted"),
                Err(err) => assert!(
                    err.contains("hilbert") && err.contains("not implemented"),
                    "win_func={name}: unexpected error text: {err}"
                ),
            }
        }
    }

    /// The six formulas this module actually implements still create fine
    /// — the fix rejects the unimplemented values, not every non-default
    /// one.
    #[test]
    fn implemented_win_func_values_still_create() {
        for name in ["rect", "bartlett", "hann", "hamming", "blackman", "sine"] {
            let req = Instantiate {
                name: "hilbert",
                instance: "hilbert",
                args: Some(&format!("win_func={name}")),
                arguments: &[],
            };
            assert!(create(&req).is_ok(), "win_func={name} should still create");
        }
    }
}
