//! `noise` — add per-component pseudo-random noise.
//!
//! `ffmpeg -h filter=noise` (2026-08-28): four independent components
//! (`c0`..`c3`, one per plane), each with `_seed` (`-1..=INT_MAX`, default
//! `-1` = "seed from local entropy"), `_strength`/`s` (`0..=100`, default
//! `0` = no noise), and `_flags`/`f` (`a` averaged, `p` (semi)regular
//! pattern, `t` temporal, `u` uniform). `all_seed`/`all_strength`/`alls`/
//! `all_flags`/`allf` set every component's corresponding suboption in one
//! go. Timeline-capable.
//!
//! # Measured: `strength=0` (the default) is an exact no-op
//!
//! ```text
//! ffmpeg -bitexact -f lavfi -i "color=gray:s=8x8:d=1:r=1" \
//!   -vf "format=gray,noise" -f rawvideo -pix_fmt gray -
//! ```
//!
//! produced byte-identical output to the same pipeline with no `noise`
//! filter at all. This is the one framecrc-exact case this module ships:
//! with every component's strength at its default of `0`, no random draw
//! ever happens and the frame passes through unmodified — verified here,
//! not assumed, because a filter that always touches its RNG state even at
//! `strength=0` would *not* have this property.
//!
//! # Not measured, and not attempted: the actual noise sequence
//!
//! `all_seed`/`c0_seed`/etc. only make sense as *reproducibility* knobs if
//! this crate reproduces the reference's own generator, and doing that would
//! mean reading the reference's source (D7) — this project's whole
//! differential story depends on not doing that. `strength > 0` is
//! therefore implemented (per-pixel additive noise, `u`niform by default,
//! `a`veraged as a 3-sample mean for a smoother spread, `p`attern as a fixed
//! deterministic tile rather than a fresh draw, `t`emporal reusing one noise
//! frame across the whole stream instead of redrawing every frame) but is
//! **not** framecrc-verified against the reference at any `strength > 0`,
//! exactly the outcome `vaco-filter-temporal::random` already documents for
//! the same reason. `seed=-1`'s reference behaviour ("local entropy",
//! i.e. non-deterministic even in the reference) is replaced here with a
//! fixed default seed, a deliberate divergence in the direction of
//! reproducible pipelines rather than an attempt to match "no seed set".

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::rng::SplitMix64;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "noise",
    description: "Add noise",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

const DEFAULT_SEED_WHEN_UNSET: u64 = 0xC0FF_EE00_D15E_ED00;

bitflags::bitflags! {
    /// The reference's `a`/`p`/`t`/`u` flag letters, one bit each.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    struct FlagSet: u8 {
        const AVERAGED = 1 << 0;
        const PATTERN  = 1 << 1;
        const TEMPORAL = 1 << 2;
        const UNIFORM  = 1 << 3;
    }
}

impl FlagSet {
    fn parse(text: &str) -> std::result::Result<Self, String> {
        let mut set = Self::empty();
        if text.is_empty() {
            return Ok(set);
        }
        for token in text.split('+') {
            match token {
                "a" => set |= Self::AVERAGED,
                "p" => set |= Self::PATTERN,
                "t" => set |= Self::TEMPORAL,
                "u" => set |= Self::UNIFORM,
                "" => {}
                other => return Err(format!("noise: bad flag `{other}`")),
            }
        }
        Ok(set)
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "noise", help = "Add noise")]
pub(crate) struct Opts {
    #[opt(name = "all_seed", help = "set component #0 noise seed", default = -1, range = -1..=2_147_483_647, flags(video, filtering))]
    pub all_seed: i64,
    #[opt(name = "all_strength", alias = "alls", help = "set component #0 strength", default = 0, range = 0..=100, flags(video, filtering))]
    pub all_strength: i64,
    #[opt(name = "all_flags", alias = "allf", help = "set component #0 flags", default = String::new(), flags(video, filtering))]
    pub all_flags: String,
    #[opt(name = "c0_seed", help = "set component #0 noise seed", default = -1, range = -1..=2_147_483_647, flags(video, filtering))]
    pub c0_seed: i64,
    #[opt(name = "c0_strength", alias = "c0s", help = "set component #0 strength", default = 0, range = 0..=100, flags(video, filtering))]
    pub c0_strength: i64,
    #[opt(name = "c0_flags", alias = "c0f", help = "set component #0 flags", default = String::new(), flags(video, filtering))]
    pub c0_flags: String,
    #[opt(name = "c1_seed", help = "set component #1 noise seed", default = -1, range = -1..=2_147_483_647, flags(video, filtering))]
    pub c1_seed: i64,
    #[opt(name = "c1_strength", alias = "c1s", help = "set component #1 strength", default = 0, range = 0..=100, flags(video, filtering))]
    pub c1_strength: i64,
    #[opt(name = "c1_flags", alias = "c1f", help = "set component #1 flags", default = String::new(), flags(video, filtering))]
    pub c1_flags: String,
    #[opt(name = "c2_seed", help = "set component #2 noise seed", default = -1, range = -1..=2_147_483_647, flags(video, filtering))]
    pub c2_seed: i64,
    #[opt(name = "c2_strength", alias = "c2s", help = "set component #2 strength", default = 0, range = 0..=100, flags(video, filtering))]
    pub c2_strength: i64,
    #[opt(name = "c2_flags", alias = "c2f", help = "set component #2 flags", default = String::new(), flags(video, filtering))]
    pub c2_flags: String,
    #[opt(name = "c3_seed", help = "set component #3 noise seed", default = -1, range = -1..=2_147_483_647, flags(video, filtering))]
    pub c3_seed: i64,
    #[opt(name = "c3_strength", alias = "c3s", help = "set component #3 strength", default = 0, range = 0..=100, flags(video, filtering))]
    pub c3_strength: i64,
    #[opt(name = "c3_flags", alias = "c3f", help = "set component #3 flags", default = String::new(), flags(video, filtering))]
    pub c3_flags: String,
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

#[derive(Debug, Clone, Copy)]
struct Component {
    seed: u64,
    strength: i32,
    flags: FlagSet,
}

impl Component {
    fn resolve(
        seed: i64,
        strength: i64,
        flags_text: &str,
        all_seed: i64,
        all_strength: i64,
        all_flags: FlagSet,
    ) -> std::result::Result<Self, String> {
        let effective_seed = if seed == -1 { all_seed } else { seed };
        let seed = if effective_seed < 0 {
            DEFAULT_SEED_WHEN_UNSET
        } else {
            #[allow(
                clippy::cast_sign_loss,
                reason = "effective_seed >= 0 was just checked"
            )]
            {
                effective_seed as u64
            }
        };
        let effective_strength = if strength != 0 { strength } else { all_strength };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "range = 0..=100 is enforced by the option schema"
        )]
        let strength = effective_strength as i32;
        let flags = if flags_text.is_empty() {
            all_flags
        } else {
            FlagSet::parse(flags_text)?
        };
        Ok(Self {
            seed,
            strength,
            flags,
        })
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    components: [Component; 4],
    /// Per-component cached noise, keyed by plane, only populated (and
    /// reused thereafter) for a component whose flags include `t`emporal.
    temporal_cache: [Option<Vec<i32>>; 4],
    rngs: [SplitMix64; 4],
}

impl Filter {
    fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let all_flags = FlagSet::parse(&opts.all_flags)?;
        let components = [
            Component::resolve(
                opts.c0_seed,
                opts.c0_strength,
                &opts.c0_flags,
                opts.all_seed,
                opts.all_strength,
                all_flags,
            )?,
            Component::resolve(
                opts.c1_seed,
                opts.c1_strength,
                &opts.c1_flags,
                opts.all_seed,
                opts.all_strength,
                all_flags,
            )?,
            Component::resolve(
                opts.c2_seed,
                opts.c2_strength,
                &opts.c2_flags,
                opts.all_seed,
                opts.all_strength,
                all_flags,
            )?,
            Component::resolve(
                opts.c3_seed,
                opts.c3_strength,
                &opts.c3_flags,
                opts.all_seed,
                opts.all_strength,
                all_flags,
            )?,
        ];
        let rngs = components.map(|c| SplitMix64::new(c.seed));
        Ok(Self {
            components,
            temporal_cache: [None, None, None, None],
            rngs,
        })
    }

    /// One pixel's noise excursion for `component`, drawing fresh, averaging
    /// three draws, or reading a fixed pattern per its flags.
    fn draw(rng: &mut SplitMix64, comp: &Component, tile_index: usize) -> i32 {
        if comp.flags.contains(FlagSet::PATTERN) {
            // A fixed, deterministic tile rather than a fresh random draw —
            // "(semi)regular pattern" per the reference's own option help.
            let phase = common::to_i32(tile_index % 7) - 3;
            #[allow(
                clippy::integer_division,
                reason = "an approximate, deliberately coarse tile pattern; \
                          exactness is not the point, see this module's doc"
            )]
            {
                phase * comp.strength / 3
            }
        } else if comp.flags.contains(FlagSet::AVERAGED) {
            let sum: i32 = (0..3).map(|_| rng.next_signed(comp.strength)).sum();
            #[allow(
                clippy::integer_division,
                reason = "a 3-sample mean for a smoother spread, not a precise average"
            )]
            {
                sum / 3
            }
        } else {
            rng.next_signed(comp.strength)
        }
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        if common::ensure_8bit_addressable(format).is_err() {
            return Ok(FrameOut::One(input));
        }
        if self.components.iter().all(|c| c.strength == 0) {
            return Ok(FrameOut::One(input));
        }
        let Some(LinkFormat::Video { width, height, .. }) = ctx.input_link(0).cloned() else {
            return Ok(FrameOut::One(input));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        for p in 0..format.plane_count() {
            let p8 = p as u8;
            let comp_idx = usize::from(p8).min(3);
            let Some(comp) = self.components.get(comp_idx).copied() else {
                continue;
            };
            let pw = common::to_i32(format.plane_width(width, p8));
            let ph = common::to_i32(format.plane_height(height, p8));
            let Some(src_plane) = input.plane(p) else {
                continue;
            };
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            if comp.strength == 0 {
                for y in 0..ph {
                    let Ok(uy) = usize::try_from(y) else { continue };
                    if let (Some(src_row), Some(dst_row)) =
                        (src_plane.row(uy), dst_plane.row_mut(uy))
                    {
                        let n = dst_row.len().min(src_row.len());
                        if let (Some(d), Some(s)) = (dst_row.get_mut(..n), src_row.get(..n)) {
                            d.copy_from_slice(s);
                        }
                    }
                }
                continue;
            }
            #[allow(
                clippy::cast_sign_loss,
                clippy::cast_possible_truncation,
                reason = "pw/ph are >= 0 plane dimensions from the frame pool"
            )]
            let pixel_count = (pw.max(0) as usize).saturating_mul(ph.max(0) as usize);
            let cached = if comp.flags.contains(FlagSet::TEMPORAL) {
                let needs_fill = self
                    .temporal_cache
                    .get(comp_idx)
                    .and_then(Option::as_ref)
                    .is_none_or(|c: &Vec<i32>| c.len() != pixel_count);
                if needs_fill {
                    let table: Vec<i32> = (0..pixel_count)
                        .map(|i| {
                            self.rngs
                                .get_mut(comp_idx)
                                .map_or(0, |rng| Self::draw(rng, &comp, i))
                        })
                        .collect();
                    if let Some(slot) = self.temporal_cache.get_mut(comp_idx) {
                        *slot = Some(table);
                    }
                }
                self.temporal_cache.get(comp_idx).and_then(Clone::clone)
            } else {
                None
            };
            let mut tile_index = 0usize;
            for y in 0..ph {
                let Ok(uy) = usize::try_from(y) else { continue };
                let Some(src_row) = src_plane.row(uy) else {
                    continue;
                };
                let Some(dst_row) = dst_plane.row_mut(uy) else {
                    continue;
                };
                let n = dst_row.len().min(src_row.len());
                for x in 0..n {
                    let Some(&src) = src_row.get(x) else { continue };
                    let Some(dst) = dst_row.get_mut(x) else {
                        continue;
                    };
                    let delta = if let Some(table) = &cached {
                        table.get(tile_index).copied().unwrap_or(0)
                    } else {
                        self.rngs
                            .get_mut(comp_idx)
                            .map_or(0, |rng| Self::draw(rng, &comp, tile_index))
                    };
                    *dst = (i32::from(src) + delta).clamp(0, 255) as u8;
                    tile_index += 1;
                }
            }
        }
        common::copy_frame_meta(&mut out, &input);
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn zero_strength_everywhere_is_a_no_op_config() {
        let opts = Opts::default();
        let filter = Filter::new(&opts).unwrap();
        assert!(filter.components.iter().all(|c| c.strength == 0));
    }

    #[test]
    fn all_strength_propagates_to_every_component_without_its_own() {
        let opts = Opts {
            all_strength: 40,
            ..Opts::default()
        };
        let filter = Filter::new(&opts).unwrap();
        assert!(filter.components.iter().all(|c| c.strength == 40));
    }

    #[test]
    fn a_components_own_strength_overrides_all_strength() {
        let opts = Opts {
            all_strength: 40,
            c1_strength: 10,
            ..Opts::default()
        };
        let filter = Filter::new(&opts).unwrap();
        assert_eq!(filter.components[0].strength, 40);
        assert_eq!(filter.components[1].strength, 10);
    }

    #[test]
    fn flag_set_rejects_an_unknown_letter() {
        assert!(FlagSet::parse("q").is_err());
    }

    #[test]
    fn flag_set_parses_every_documented_letter() {
        let set = FlagSet::parse("a+p+t+u").unwrap();
        assert_eq!(
            set,
FlagSet::AVERAGED | FlagSet::PATTERN | FlagSet::TEMPORAL | FlagSet::UNIFORM
        );
    }

    #[test]
    fn negative_seed_falls_back_to_the_fixed_default() {
        let opts = Opts::default();
        let filter = Filter::new(&opts).unwrap();
        assert_eq!(filter.components[0].seed, DEFAULT_SEED_WHEN_UNSET);
    }

    #[test]
    fn an_explicit_seed_is_used_verbatim() {
        let opts = Opts {
            c2_seed: 99,
            ..Opts::default()
        };
        let filter = Filter::new(&opts).unwrap();
        assert_eq!(filter.components[2].seed, 99);
    }

    proptest::proptest! {
        /// Invariant: whatever the draw mode, a single excursion never
        /// exceeds the component's own `strength` in magnitude, so the
        /// final pixel is never more than `strength` away from the input
        /// before clamping.
        #[test]
        fn a_draw_never_exceeds_its_own_strength(
            seed in proptest::num::u64::ANY,
            strength in 0i32..=100,
            flags_bits in 0u8..16,
        ) {
            let comp = Component {
                seed,
                strength,
                flags: FlagSet::from_bits_truncate(flags_bits),
            };
            let mut rng = SplitMix64::new(seed);
            for i in 0..8usize {
                let delta = Filter::draw(&mut rng, &comp, i);
                proptest::prop_assert!(delta.abs() <= strength);
            }
        }

        /// Whatever the input byte and the component's strength, the
        /// applied result always stays a valid sample.
        #[test]
        fn noised_output_always_stays_in_byte_range(
            input in 0u8..=255,
            delta in -100i32..=100,
        ) {
            let out = (i32::from(input) + delta).clamp(0, 255);
            proptest::prop_assert!((0..=255).contains(&out));
        }
    }
}
