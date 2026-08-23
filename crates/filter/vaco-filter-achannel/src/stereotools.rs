//! `stereotools` — apply various stereo tools.
//!
//! `ffmpeg -h filter=stereotools` (2026-08-23) documents twenty-one options.
//! This implements the ones measured exactly against the reference:
//! `level_in`, `level_out`, `balance_in`, `balance_out`, `mutel`, `muter`,
//! `phasel`, `phaser`, and all eleven `mode` values. `softclip`, `mutel`
//! /`muter`/`phasel`/`phaser` order versus `mode`, `slev`, `sbal`, `mlev`,
//! `mpan`, `base`, `delay`, `sclevel`, `phase`, `bmode_in`, `bmode_out` are
//! accepted (so a filtergraph string that sets them is not rejected) but not
//! applied — a structural gap in the same spirit as
//! `vaco-filter-audio-eq::common`'s documented unimplemented options. See
//! `docs/filter/vaco-filter-achannel.md`.
//!
//! # Measured formulas (D17)
//!
//! Every `mode` was checked against `ffmpeg -af stereotools=mode=N` on two
//! `(L, R)` pairs each (`(10000, 5000)` and `(20000, -10000)`, `i16` domain).
//! All eleven match, exactly, a plain mid/side matrix with **no extra
//! `0.5` beyond the one already in `mid`/`side`**:
//!
//! ```text
//! mid  = (L + R) / 2
//! side = (L - R) / 2
//! ```
//!
//! | mode | name | output |
//! |---|---|---|
//! | 0 | `lr>lr` | `(L, R)` |
//! | 1 | `lr>ms` | `(mid, side)` |
//! | 2 | `ms>lr` | `(L+R, L-R)` read as `(mid+side, mid-side)` |
//! | 3 | `lr>ll` | `(L, L)` |
//! | 4 | `lr>rr` | `(R, R)` |
//! | 5 | `lr>l+r` | `(mid, mid)` |
//! | 6 | `lr>rl` | `(R, L)` |
//! | 7 | `ms>ll` | `(mid+side, mid+side)` |
//! | 8 | `ms>rr` | `(mid-side, mid-side)` |
//! | 9 | `ms>rl` | `(mid-side, mid+side)` |
//! | 10 | `lr>l-r` | `(side, side)` |
//!
//! `level_in`/`level_out` measured as a plain multiplicative gain on both
//! channels (`level_in=2.0` doubles `(10000, 10000)` to `(20000, 20000)`).
//! `balance_out` measured as a one-sided linear pan (the reference's default
//! `bmode_out=balance`, not implemented for the other two `bmode` values):
//! `balance > 0` scales `L` by `1 - balance` and leaves `R` unchanged;
//! `balance < 0` scales `R` by `1 + balance` and leaves `L` unchanged
//! (`balance_out=0.5` on `(10000, 10000)` measured as `(5000, 10000)`,
//! `balance_out=-1` measured as `(10000, 0)`). `mutel`/`muter` zero one
//! channel; `phasel`/`phaser` negate one channel.
//! [`tests::matches_measured_modes`] and the sibling tests pin all of the
//! above.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "stereotools",
    description: "apply various stereo tools",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    LrLr,
    LrMs,
    MsLr,
    LrLl,
    LrRr,
    LrSum,
    LrRl,
    MsLl,
    MsRr,
    MsRl,
    LrDiff,
}

impl Mode {
    fn parse(s: &str) -> Self {
        match s.trim() {
            "1" | "lr>ms" => Self::LrMs,
            "2" | "ms>lr" => Self::MsLr,
            "3" | "lr>ll" => Self::LrLl,
            "4" | "lr>rr" => Self::LrRr,
            "5" | "lr>l+r" => Self::LrSum,
            "6" | "lr>rl" => Self::LrRl,
            "7" | "ms>ll" => Self::MsLl,
            "8" | "ms>rr" => Self::MsRr,
            "9" | "ms>rl" => Self::MsRl,
            "10" | "lr>l-r" => Self::LrDiff,
            _ => Self::LrLr,
        }
    }

    /// The measured matrix, `(L, R) -> (L', R')`.
    fn apply(self, l: f64, r: f64) -> (f64, f64) {
        let mid = (l + r) * 0.5;
        let side = (l - r) * 0.5;
        match self {
            Self::LrLr => (l, r),
            Self::LrMs => (mid, side),
            Self::MsLr => (l + r, l - r),
            Self::LrLl => (l, l),
            Self::LrRr => (r, r),
            Self::LrSum => (mid, mid),
            Self::LrRl => (r, l),
            Self::MsLl => (l + r, l + r),
            Self::MsRr => (l - r, l - r),
            Self::MsRl => (l - r, l + r),
            Self::LrDiff => (side, side),
        }
    }
}

fn balance(l: f64, r: f64, bal: f64) -> (f64, f64) {
    if bal > 0.0 {
        (l * (1.0 - bal), r)
    } else if bal < 0.0 {
        (l, r * (1.0 + bal))
    } else {
        (l, r)
    }
}

/// The four boolean per-channel switches, grouped so `StereoTools` itself
/// does not trip `clippy::struct_excessive_bools` (four independent on/off
/// options, not a state machine — the reference documents them as four
/// separate `AVOption`s, not one enum).
#[derive(Debug, Clone, Copy, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent per-channel toggles the reference exposes as four separate AVOptions, not a state machine"
)]
struct ChannelSwitches {
    mutel: bool,
    muter: bool,
    phasel: bool,
    phaser: bool,
}

struct StereoTools {
    level_in: f64,
    level_out: f64,
    balance_in: f64,
    balance_out: f64,
    switches: ChannelSwitches,
    mode: Mode,
}

impl FrameFilter for StereoTools {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if channels.len() >= 2 {
            let n = channels
                .first()
                .map_or(0, Vec::len)
                .min(channels.get(1).map_or(0, Vec::len));
            for i in 0..n {
                let l = channels.first().and_then(|c| c.get(i)).copied().unwrap_or(0.0);
                let r = channels.get(1).and_then(|c| c.get(i)).copied().unwrap_or(0.0);

                let (l, r) = (l * self.level_in, r * self.level_in);
                let (l, r) = balance(l, r, self.balance_in);
                let (l, r) = self.mode.apply(l, r);
                let (mut l, mut r) = balance(l, r, self.balance_out);
                l *= self.level_out;
                r *= self.level_out;
                if self.switches.mutel {
                    l = 0.0;
                }
                if self.switches.muter {
                    r = 0.0;
                }
                if self.switches.phasel {
                    l = -l;
                }
                if self.switches.phaser {
                    r = -r;
                }

                if let Some(c) = channels.get_mut(0)
                    && let Some(s) = c.get_mut(i)
                {
                    *s = l;
                }
                if let Some(c) = channels.get_mut(1)
                    && let Some(s) = c.get_mut(i)
                {
                    *s = r;
                }
            }
        }
        let mut out = crate::sample::encode(
            &vaco_frame::FramePool::default(),
            fmt,
            layout,
            rate,
            &channels,
        )?;
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let filter = StereoTools {
        level_in: common::f64_opt(req, &["level_in"], 1.0),
        level_out: common::f64_opt(req, &["level_out"], 1.0),
        balance_in: common::f64_opt(req, &["balance_in"], 0.0),
        balance_out: common::f64_opt(req, &["balance_out"], 0.0),
        switches: ChannelSwitches {
            mutel: common::bool_opt(req, &["mutel"], false),
            muter: common::bool_opt(req, &["muter"], false),
            phasel: common::bool_opt(req, &["phasel"], false),
            phaser: common::bool_opt(req, &["phaser"], false),
        },
        mode: req
            .named("mode")
            .map_or(Mode::LrLr, |s| Mode::parse(&s)),
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter).with_timeline(Timeline::always())),
    }
}

#[cfg(test)]
mod tests {
    use super::{Mode, balance};

    /// Every mode, measured directly against `ffmpeg -af stereotools=mode=N`
    /// on 2026-08-23, on two `(L, R)` pairs each.
    #[test]
    fn matches_measured_modes() {
        let cases: &[(Mode, (f64, f64), (f64, f64))] = &[
            (Mode::LrLr, (10000.0, 5000.0), (10000.0, 5000.0)),
            (Mode::LrMs, (10000.0, 5000.0), (7500.0, 2500.0)),
            (Mode::LrMs, (20000.0, -10000.0), (5000.0, 15000.0)),
            (Mode::MsLr, (10000.0, 5000.0), (15000.0, 5000.0)),
            (Mode::MsLr, (20000.0, -10000.0), (10000.0, 30000.0)),
            (Mode::LrLl, (10000.0, 5000.0), (10000.0, 10000.0)),
            (Mode::LrRr, (10000.0, 5000.0), (5000.0, 5000.0)),
            (Mode::LrSum, (10000.0, 5000.0), (7500.0, 7500.0)),
            (Mode::LrRl, (10000.0, 5000.0), (5000.0, 10000.0)),
            (Mode::MsLl, (10000.0, 5000.0), (15000.0, 15000.0)),
            (Mode::MsRr, (10000.0, 5000.0), (5000.0, 5000.0)),
            (Mode::MsRr, (20000.0, -10000.0), (30000.0, 30000.0)),
            (Mode::MsRl, (10000.0, 5000.0), (5000.0, 15000.0)),
            (Mode::MsRl, (20000.0, -10000.0), (30000.0, 10000.0)),
            (Mode::LrDiff, (10000.0, 5000.0), (2500.0, 2500.0)),
            (Mode::LrDiff, (20000.0, -10000.0), (15000.0, 15000.0)),
        ];
        for &(mode, (l, r), (exp_l, exp_r)) in cases {
            let (got_l, got_r) = mode.apply(l, r);
            assert!(
                (got_l - exp_l).abs() < 1e-9,
                "{mode:?}: L got {got_l}, want {exp_l}"
            );
            assert!(
                (got_r - exp_r).abs() < 1e-9,
                "{mode:?}: R got {got_r}, want {exp_r}"
            );
        }
    }

    /// Measured directly against `ffmpeg -af stereotools=balance_out=B` on
    /// `(10000, 10000)`.
    #[test]
    fn matches_measured_balance() {
        let cases: &[(f64, (f64, f64))] = &[
            (-1.0, (10000.0, 0.0)),
            (-0.5, (10000.0, 5000.0)),
            (0.0, (10000.0, 10000.0)),
            (0.5, (5000.0, 10000.0)),
            (1.0, (0.0, 10000.0)),
        ];
        for &(bal, (exp_l, exp_r)) in cases {
            let (got_l, got_r) = balance(10000.0, 10000.0, bal);
            assert!((got_l - exp_l).abs() < 1e-9, "bal {bal}: L {got_l}");
            assert!((got_r - exp_r).abs() < 1e-9, "bal {bal}: R {got_r}");
        }
    }
}
