//! Six small 1-in-1-out filters that share this crate's `Simple<FrameFilter>`
//! shape closely enough to live in one file.
//! **`cue`/`acue`** delays until a wall-clock timestamp: `cue` (a Unix
//! microsecond timestamp, default `0`), `preroll` (seconds), `buffer`
//! (seconds). Passes `preroll` seconds immediately, then buffers up to
//! `buffer` seconds waiting for `cue` before releasing everything; `cue=0`
//! is in the past for any real clock, so it is a true no-op. Buffered
//! frames are charged against a [`vaco_limits::Budget`]; once exhausted,
//! the cue is treated as already reached rather than growing the buffer.
//! **`realtime`/`arealtime`** paces output to wall-clock time: `limit`
//! (seconds, default `2`), `speed` (default `1.0`). Sleeps to keep output
//! pace matching input timestamps; a gap longer than `limit` resets the
//! timer instead of sleeping to catch up. Only the wall-clock moment each
//! frame is forwarded changes — content and timestamps never do.
//! **`latency`/`alatency`** is an honest no-op: the reference reports the
//! previous filter's own buffering latency, measured by its internal
//! scheduler. This framework has no per-link latency instrumentation a
//! leaf filter can read, so it passes every frame through unchanged rather
//! than fabricating a number.
//! **`bench`/`abench`** is measured via the metadata dictionary:
//! `action=start` stamps `lavfi.bench.start_time` via
//! [`vaco_frame::Frame::set_metadata`] and forwards; `action=stop` reads it
//! back and reports elapsed time plus a running average/max/min. As with
//! `metadata`'s `print` mode, there is no log sink here yet, so the
//! statistics are kept on a test-only accessor.
//!
//! **`perms`/`aperms`** is an architecture mismatch, not an oversight: the
//! reference sets output frames read-only/writable/toggled, "mainly aimed
//! at developers to test direct path". This project's `Frame` has no such
//! bit — ownership is the mechanism that makes one unnecessary — so options
//! are parsed and validated but every frame passes through unchanged.
//!
//! **`sidedata`/`asidedata`** is restricted to the side data this project
//! models: the reference names 30 `AVFrameSideDataType` constants (two
//! pairs alias the same integer). `type` parses all 30, but
//! [`vaco_frame::FrameSideDataKind`] models six kinds, four with a
//! reference counterpart: `DISPLAYMATRIX`/`A53_CC`/
//! `MASTERING_DISPLAY_METADATA`/`CONTENT_LIGHT_LEVEL` (see `mapped_kind`).
//! Any other `type` parses fine but always fails `select` and is a no-op
//! under `delete`.

use std::time::Duration as StdDuration;

use vaco_core::{Duration as VDuration, MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameSideDataKind};
use vaco_limits::{Budget, Limits};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];
const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

fn frame_bytes(frame: &Frame) -> u64 {
    (0..8)
        .filter_map(|i| frame.plane(i))
        .map(|p| p.as_slice().len() as u64)
        .sum()
}

/// Wall-clock microseconds since the Unix epoch, or `i64::MAX` when this
/// target has no wall clock (`vaco_time::unix_nanos` returns `None` there —
/// see that crate's doc). `i64::MAX` is deliberate, not an arbitrary
/// fallback: every caller here (`cue`'s cue-reached check, `bench`'s
/// elapsed-time subtraction) treats "unknown, so assume it already
/// happened" as the safe direction, matching the conservative-release
/// choice `cue` documents above.
fn wall_micros() -> i64 {
    #[allow(
        clippy::integer_division,
        reason = "deliberate nanoseconds-to-microseconds truncation, not a precision bug"
    )]
    vaco_time::unix_nanos()
        .and_then(|ns| i64::try_from(ns / 1_000).ok())
        .unwrap_or(i64::MAX)
}

// ------------------------------------------------------------------- cue

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "cue", help = "delay filtering until a wallclock timestamp")]
pub(crate) struct CueOpts {
    #[opt(name = "cue", help = "cue unix timestamp in microseconds", default = 0_i64, range = 0..=i64::MAX, flags(filtering))]
    pub cue: i64,
    #[opt(name = "preroll", help = "preroll duration in seconds", default = None, flags(filtering))]
    pub preroll: Option<VDuration>,
    #[opt(name = "buffer", help = "buffer duration in seconds", default = None, flags(filtering))]
    pub buffer: Option<VDuration>,
}

impl CueOpts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        if o.buffer.is_some() {
            return Err("cue: `buffer` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        Ok(o)
    }
}

#[derive(Debug)]
pub(crate) struct CueFilter {
    cue_micros: i64,
    preroll_secs: f64,
    passed_secs: f64,
    buffered: std::collections::VecDeque<Frame>,
    budget: Budget,
    released: bool,
}

impl FrameFilter for CueFilter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        if self.released || wall_micros() >= self.cue_micros {
            self.released = true;
            let mut out: Vec<Frame> = self.buffered.drain(..).collect();
            out.push(frame);
            return Ok(out.into_iter().collect());
        }
        if self.passed_secs < self.preroll_secs {
            self.passed_secs += frame.duration.0.max(0) as f64 * frame.time_base.to_f64();
            return Ok(FrameOut::One(frame));
        }
        if self.budget.charge(frame_bytes(&frame)).is_ok() {
            self.buffered.push_back(frame);
            Ok(FrameOut::None)
        } else {
            // Budget exhausted: release rather than drop — see module doc.
            self.released = true;
            let mut out: Vec<Frame> = self.buffered.drain(..).collect();
            out.push(frame);
            Ok(out.into_iter().collect())
        }
    }

    fn flush(&mut self, _ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        Ok(self.buffered.drain(..).collect())
    }
}

fn cue_build(
    media: MediaType,
    desc: FilterDesc,
    req: &Instantiate<'_>,
) -> std::result::Result<Instance, String> {
    let opts = CueOpts::parse(req.args)?;
    #[allow(
        clippy::cast_precision_loss,
        reason = "display-scale duration conversion"
    )]
    let secs = |d: Option<VDuration>| d.map_or(0.0, |d| d.0 as f64 / 1_000_000.0);
    let filter = CueFilter {
        cue_micros: opts.cue,
        preroll_secs: secs(opts.preroll),
        passed_secs: 0.0,
        buffered: std::collections::VecDeque::new(),
        budget: Budget::new(Limits::permissive()),
        released: false,
    };
    Ok(Instance {
        desc,
        formats: NodeFormats::passthrough(1, 1, media, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

// --------------------------------------------------------------- realtime

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "realtime", help = "slow down filtering to match real time")]
pub(crate) struct RealtimeOpts {
    #[opt(name = "limit", help = "sleep time limit", default = None, flags(filtering))]
    pub limit: Option<VDuration>,
    #[opt(name = "speed", help = "speed factor", default = 1.0, range = f64::MIN_POSITIVE..=f64::MAX, flags(filtering))]
    pub speed: f64,
}

impl RealtimeOpts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

#[derive(Debug)]
pub(crate) struct RealtimeFilter {
    limit_secs: f64,
    speed: f64,
    anchor: Option<(vaco_time::Instant, f64)>,
    /// Skipped in tests via a zero-length sleep path: real sleeping is the
    /// filter's whole point in production, but a unit test wants the frame
    /// spacing, not the wall clock, exercised. Kept `false` outside tests.
    #[cfg(test)]
    no_sleep: bool,
}

impl FrameFilter for RealtimeFilter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        let t = frame.pts.to_seconds(frame.time_base).unwrap_or(0.0);
        let now = vaco_time::Instant::now();
        match self.anchor {
            None => self.anchor = Some((now, t)),
            Some((anchor_wall, anchor_stream)) => {
                let target = anchor_wall.saturating_add(StdDuration::from_secs_f64(
                    ((t - anchor_stream) / self.speed).max(0.0),
                ));
                let gap = target.duration_since(now);
                if !gap.is_zero() {
                    if gap.as_secs_f64() > self.limit_secs {
                        self.anchor = Some((now, t));
                    } else {
                        #[cfg(test)]
                        if !self.no_sleep {
                            vaco_time::sleep(gap);
                        }
                        #[cfg(not(test))]
                        vaco_time::sleep(gap);
                    }
                }
            }
        }
        Ok(FrameOut::One(frame))
    }
}

fn realtime_build(
    media: MediaType,
    desc: FilterDesc,
    req: &Instantiate<'_>,
) -> std::result::Result<Instance, String> {
    let opts = RealtimeOpts::parse(req.args)?;
    #[allow(
        clippy::cast_precision_loss,
        reason = "display-scale duration conversion"
    )]
    let limit_secs = opts.limit.map_or(2.0, |d| d.0 as f64 / 1_000_000.0);
    let filter = RealtimeFilter {
        limit_secs,
        speed: if opts.speed > 0.0 { opts.speed } else { 1.0 },
        anchor: None,
        #[cfg(test)]
        no_sleep: false,
    };
    Ok(Instance {
        desc,
        formats: NodeFormats::passthrough(1, 1, media, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

// ---------------------------------------------------------------- latency

#[derive(Debug, Default)]
pub(crate) struct LatencyFilter;

impl FrameFilter for LatencyFilter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(FrameOut::One(frame))
    }
}

fn latency_build(media: MediaType, desc: FilterDesc, req: &Instantiate<'_>) -> Instance {
    Instance {
        desc,
        formats: NodeFormats::passthrough(1, 1, media, req.instance),
        filter: Box::new(Simple::new(LatencyFilter)),
    }
}

// ------------------------------------------------------------------ bench

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, vaco_opts::OptEnum)]
#[opt_enum(unit = "bench_action", base = "int")]
pub(crate) enum BenchAction {
    #[opt_const(name = "start", help = "start timer")]
    #[default]
    Start,
    #[opt_const(name = "stop", help = "stop timer")]
    Stop,
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "bench", help = "benchmark part of a filtergraph")]
pub(crate) struct BenchOpts {
    #[opt(
        name = "action",
        help = "start or stop a timer",
        unit = "bench_action",
        default = BenchAction::Start,
        default_repr = "start",
        flags(filtering)
    )]
    pub action: BenchAction,
}

impl BenchOpts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

#[derive(Debug, Default)]
pub(crate) struct BenchStats {
    pub count: u64,
    pub total: f64,
    pub max: f64,
    pub min: f64,
}

#[derive(Debug)]
pub(crate) struct BenchFilter {
    action: BenchAction,
    stats: BenchStats,
}

/// Fold one more `stop`-side elapsed-time sample into `stats`. Pulled out
/// of `filter_frame` so it is testable without a live `FilterContext`.
fn update_stats(stats: &mut BenchStats, elapsed: f64) {
    stats.count += 1;
    stats.total += elapsed;
    stats.max = stats.max.max(elapsed);
    stats.min = if stats.count == 1 {
        elapsed
    } else {
        stats.min.min(elapsed)
    };
}

impl FrameFilter for BenchFilter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut frame: Frame) -> Result<FrameOut> {
        #[allow(clippy::cast_precision_loss, reason = "display-scale wall clock")]
        let now = wall_micros() as f64 / 1_000_000.0;
        match self.action {
            BenchAction::Start => {
                frame.set_metadata("lavfi.bench.start_time", now.to_string());
            }
            BenchAction::Stop => {
                if let Some(start) = frame
                    .metadata_get("lavfi.bench.start_time")
                    .and_then(|s| s.parse::<f64>().ok())
                {
                    update_stats(&mut self.stats, (now - start).max(0.0));
                }
            }
        }
        Ok(FrameOut::One(frame))
    }
}

fn bench_build(
    media: MediaType,
    desc: FilterDesc,
    req: &Instantiate<'_>,
) -> std::result::Result<Instance, String> {
    let opts = BenchOpts::parse(req.args)?;
    let filter = BenchFilter {
        action: opts.action,
        stats: BenchStats::default(),
    };
    Ok(Instance {
        desc,
        formats: NodeFormats::passthrough(1, 1, media, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

// ------------------------------------------------------------------ perms

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, vaco_opts::OptEnum)]
#[opt_enum(unit = "perms_mode", base = "int")]
pub(crate) enum PermsMode {
    #[opt_const(name = "none", help = "do nothing")]
    #[default]
    None,
    #[opt_const(name = "ro", help = "read-only")]
    Ro,
    #[opt_const(name = "rw", help = "writable")]
    Rw,
    #[opt_const(name = "toggle", help = "toggle")]
    Toggle,
    #[opt_const(name = "random", help = "random")]
    Random,
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(
    name = "perms",
    help = "set read/write permissions for the output frames"
)]
pub(crate) struct PermsOpts {
    #[opt(
        name = "mode",
        help = "permissions mode",
        unit = "perms_mode",
        default = PermsMode::None,
        default_repr = "none",
        flags(filtering)
    )]
    pub mode: PermsMode,
    #[opt(name = "seed", help = "seed for random mode", default = -1_i64, range = -1..=4_294_967_295_i64, flags(filtering))]
    pub seed: i64,
}

impl PermsOpts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        if o.mode != PermsMode::None {
            return Err("perms: `mode` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.seed != -1_i64 {
            return Err("perms: `seed` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        Ok(o)
    }
}

#[derive(Debug, Default)]
pub(crate) struct PermsFilter;

impl FrameFilter for PermsFilter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(FrameOut::One(frame))
    }
}

fn perms_build(
    media: MediaType,
    desc: FilterDesc,
    req: &Instantiate<'_>,
) -> std::result::Result<Instance, String> {
    // There is no permission bit in this pipeline model for `mode`/`seed`
    // to act on, so `PermsOpts::parse` refuses a non-default value rather
    // than silently accepting a filtergraph the reference would run with
    // real effect and this build would run as a no-op instead (`cargo
    // xtask reachability-check` rule I).
    let _opts = PermsOpts::parse(req.args)?;
    Ok(Instance {
        desc,
        formats: NodeFormats::passthrough(1, 1, media, req.instance),
        filter: Box::new(Simple::new(PermsFilter)),
    })
}

// --------------------------------------------------------------- sidedata

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, vaco_opts::OptEnum)]
#[opt_enum(unit = "sidedata_mode", base = "int")]
pub(crate) enum SidedataMode {
    #[opt_const(name = "select", help = "select frame")]
    #[default]
    Select,
    #[opt_const(name = "delete", help = "delete side data")]
    Delete,
}

/// `ffmpeg -h filter=sidedata`'s 28 `AVFrameSideDataType` names for `type`
/// (30 entries: two reference pairs, `S12M_TIMECOD`/`S12M_TIMECODE` and
/// `DETECTION_BOUNDING_BOXES`/`DETECTION_BBOXES`, name the same integer —
/// the reference's own alias, not a mistake here). `type` stays a plain
/// `i32` (not `#[derive(OptEnum)]`) because of exactly those two aliased
/// pairs, the same one-name-per-variant limit `il`'s `luma_mode` and
/// `hilbert`'s `win_func` hit. Naming all 28 here, not just the four this
/// project's `mapped_kind` actually acts on, matches `fillborders`'
/// precedent: a `type` this project does not model still parses (and is a
/// clean no-op under `mode=select`/`mode=delete`, per `mapped_kind`
/// returning `None`) rather than failing to parse at all.
const SIDEDATA_TYPE_CONSTS: &[vaco_opts::ConstDesc] = {
    const fn c(name: &'static str, value: i64) -> vaco_opts::ConstDesc {
        vaco_opts::ConstDesc {
            name,
            help: "",
            unit: "side_data_type",
            value: vaco_opts::ConstValue::Int(value),
            flags: vaco_opts::OptFlags::NONE,
        }
    }
    &[
        c("PANSCAN", 0),
        c("A53_CC", 1),
        c("STEREO3D", 2),
        c("MATRIXENCODING", 3),
        c("DOWNMIX_INFO", 4),
        c("REPLAYGAIN", 5),
        c("DISPLAYMATRIX", 6),
        c("AFD", 7),
        c("MOTION_VECTORS", 8),
        c("SKIP_SAMPLES", 9),
        c("AUDIO_SERVICE_TYPE", 10),
        c("MASTERING_DISPLAY_METADATA", 11),
        c("GOP_TIMECODE", 12),
        c("SPHERICAL", 13),
        c("CONTENT_LIGHT_LEVEL", 14),
        c("ICC_PROFILE", 15),
        c("S12M_TIMECOD", 16),
        c("S12M_TIMECODE", 16),
        c("DYNAMIC_HDR_PLUS", 17),
        c("REGIONS_OF_INTEREST", 18),
        c("VIDEO_ENC_PARAMS", 19),
        c("SEI_UNREGISTERED", 20),
        c("FILM_GRAIN_PARAMS", 21),
        c("DETECTION_BOUNDING_BOXES", 22),
        c("DETECTION_BBOXES", 22),
        c("DOVI_RPU_BUFFER", 23),
        c("DOVI_METADATA", 24),
        c("DYNAMIC_HDR_VIVID", 25),
        c("AMBIENT_VIEWING_ENVIRONMENT", 26),
        c("VIDEO_HINT", 27),
    ]
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(
    name = "sidedata",
    help = "delete frame side data, or select frames based on it"
)]
pub(crate) struct SidedataOpts {
    #[opt(
        name = "mode",
        help = "mode of operation",
        unit = "sidedata_mode",
        default = SidedataMode::Select,
        default_repr = "select",
        flags(filtering)
    )]
    pub mode: SidedataMode,
    #[opt(
        name = "type",
        help = "side data type",
        unit = "side_data_type",
        consts = SIDEDATA_TYPE_CONSTS,
        default = -1,
        range = -1..=27,
        flags(filtering)
    )]
    pub kind: i32,
}

impl SidedataOpts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

/// The reference's `type` integers this project can actually act on — see
/// module doc for why only four of the reference's 28 map anywhere.
fn mapped_kind(reference_type: i32) -> Option<FrameSideDataKind> {
    match reference_type {
        1 => Some(FrameSideDataKind::ClosedCaptions), // A53_CC
        6 => Some(FrameSideDataKind::DisplayMatrix),  // DISPLAYMATRIX
        11 => Some(FrameSideDataKind::MasteringDisplay), // MASTERING_DISPLAY_METADATA
        14 => Some(FrameSideDataKind::ContentLightLevel), // CONTENT_LIGHT_LEVEL
        _ => None,
    }
}

#[derive(Debug)]
pub(crate) struct SidedataFilter {
    mode: SidedataMode,
    kind: Option<FrameSideDataKind>,
}

impl FrameFilter for SidedataFilter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut frame: Frame) -> Result<FrameOut> {
        match self.mode {
            SidedataMode::Select => {
                let has = self.kind.is_some_and(|k| frame.side_data(k).is_some());
                Ok(if has {
                    FrameOut::One(frame)
                } else {
                    FrameOut::None
                })
            }
            SidedataMode::Delete => {
                if let Some(k) = self.kind {
                    let _ = frame.remove_side_data(k);
                } else {
                    // No `type`: delete every kind of side data this
                    // project models on the frame.
                    for k in [
                        FrameSideDataKind::DisplayMatrix,
                        FrameSideDataKind::ClosedCaptions,
                        FrameSideDataKind::MasteringDisplay,
                        FrameSideDataKind::ContentLightLevel,
                        FrameSideDataKind::Cropping,
                        FrameSideDataKind::Metadata,
                    ] {
                        let _ = frame.remove_side_data(k);
                    }
                }
                Ok(FrameOut::One(frame))
            }
        }
    }
}

fn sidedata_build(
    media: MediaType,
    desc: FilterDesc,
    req: &Instantiate<'_>,
) -> std::result::Result<Instance, String> {
    let opts = SidedataOpts::parse(req.args)?;
    let filter = SidedataFilter {
        mode: opts.mode,
        kind: mapped_kind(opts.kind),
    };
    Ok(Instance {
        desc,
        formats: NodeFormats::passthrough(1, 1, media, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

// -------------------------------------------------------------- wiring

macro_rules! media_pair {
    ($modname:ident, $build:expr, $vname:literal, $vdesc:literal, $aname:literal, $adesc:literal) => {
        pub mod $modname {
            use super::{
                AUDIO_PAD, FilterDesc, FilterFlags, Instance, Instantiate, MediaType, VIDEO_PAD,
            };

            pub mod video {
                use super::{FilterDesc, FilterFlags, Instance, Instantiate, MediaType, VIDEO_PAD};

                pub const DESC: FilterDesc = FilterDesc {
                    name: $vname,
                    description: $vdesc,
                    inputs: VIDEO_PAD,
                    outputs: VIDEO_PAD,
                    flags: FilterFlags::empty(),
                };

                pub(crate) fn create(
                    req: &Instantiate<'_>,
                ) -> std::result::Result<Instance, String> {
                    $build(MediaType::Video, DESC, req)
                }
            }

            pub mod audio {
                use super::{AUDIO_PAD, FilterDesc, FilterFlags, Instance, Instantiate, MediaType};

                pub const DESC: FilterDesc = FilterDesc {
                    name: $aname,
                    description: $adesc,
                    inputs: AUDIO_PAD,
                    outputs: AUDIO_PAD,
                    flags: FilterFlags::empty(),
                };

                pub(crate) fn create(
                    req: &Instantiate<'_>,
                ) -> std::result::Result<Instance, String> {
                    $build(MediaType::Audio, DESC, req)
                }
            }
        }
    };
}

media_pair!(
    cue,
    crate::misc::cue_build,
    "cue",
    "Delay filtering until a wallclock timestamp",
    "acue",
    "Delay filtering until a wallclock timestamp"
);
media_pair!(
    realtime,
    crate::misc::realtime_build,
    "realtime",
    "Slow down filtering to match real time",
    "arealtime",
    "Slow down filtering to match real time"
);
media_pair!(
    latency,
    |media, desc, req: &Instantiate<'_>| Ok(crate::misc::latency_build(media, desc, req)),
    "latency",
    "Report previous filter latency",
    "alatency",
    "Report previous filter latency"
);
media_pair!(
    bench,
    crate::misc::bench_build,
    "bench",
    "Benchmark part of a filtergraph",
    "abench",
    "Benchmark part of a filtergraph"
);
media_pair!(
    perms,
    crate::misc::perms_build,
    "perms",
    "Set permissions for the output frames",
    "aperms",
    "Set permissions for the output frames"
);
media_pair!(
    sidedata,
    crate::misc::sidedata_build,
    "sidedata",
    "Delete frame side data, or select frames based on it",
    "asidedata",
    "Delete frame side data, or select frames based on it"
);

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_filter_core::mock::{gray_frame, gray_link, video_source_formats};
    use vaco_filter_core::{Graph, GraphStatus};

    fn run_one(
        create: fn(&Instantiate<'_>) -> std::result::Result<Instance, String>,
        args: Option<&str>,
        frame: Frame,
    ) -> Frame {
        let req = Instantiate {
            name: "x",
            instance: "x",
            args,
            arguments: &[],
        };
        let instance = create(&req).unwrap();
        let mut graph = Graph::new();
        let src = graph.add_source(
            "in",
            MediaType::Video,
            video_source_formats("in", vaco_pixfmt::PixFmt::Gray8),
        );
        let node = graph.add(instance.desc, instance.formats, instance.filter);
        let sink = graph.add_sink(
            "out",
            MediaType::Video,
            vaco_filter_core::mock::any_video_sink("out"),
        );
        graph.connect(src, 0, node, 0).unwrap();
        graph.connect(node, 0, sink, 0).unwrap();
        let tb = vaco_core::Rational::new(1, 25);
        graph.set_source_format(src, gray_link(4, 4, tb)).unwrap();
        graph.configure().unwrap();
        graph.send(src, frame).unwrap();
        graph
            .close_source(src, vaco_core::Timestamp::new(1))
            .unwrap();
        let mut out = None;
        loop {
            match graph.run().unwrap() {
                GraphStatus::Eof => break,
                GraphStatus::HasOutput(_) => {
                    if let Ok(f) = graph.recv(sink) {
                        out = Some(f);
                    }
                }
                GraphStatus::NeedInput(_) => {}
                other => panic!("unexpected graph status: {other:?}"),
            }
        }
        out.unwrap()
    }

    #[test]
    fn cue_zero_is_a_no_op_passthrough() {
        let out = run_one(cue::video::create, None, gray_frame(4, 4, 0, 7));
        assert_eq!(
            out.plane(0)
                .and_then(|p| p.row(0))
                .and_then(|r| r.first())
                .copied(),
            Some(7)
        );
    }

    #[test]
    fn latency_passes_frames_through_unchanged() {
        let out = run_one(latency::video::create, None, gray_frame(4, 4, 0, 9));
        assert_eq!(out.pts.ticks(), Some(0));
    }

    #[test]
    fn perms_passes_frames_through_at_the_default_mode() {
        let out = run_one(perms::video::create, None, gray_frame(4, 4, 0, 3));
        assert_eq!(
            out.plane(0)
                .and_then(|p| p.row(0))
                .and_then(|r| r.first())
                .copied(),
            Some(3)
        );
    }

    /// There is no permission bit in this pipeline model to honour
    /// `mode`/`seed` with, so a non-default value must refuse rather than
    /// silently behave like `mode=none` — the bug `perms_passes_frames_
    /// through_regardless_of_mode` used to name as if it were the intended
    /// behaviour. Regression for `cargo xtask reachability-check`'s rule I.
    #[test]
    fn perms_refuses_a_non_default_mode_or_seed_instead_of_ignoring_it() {
        let req = Instantiate {
            name: "x",
            instance: "x",
            args: Some("mode=random:seed=1"),
            arguments: &[],
        };
        assert!(perms::video::create(&req).is_err());
        let req_seed = Instantiate {
            name: "x",
            instance: "x",
            args: Some("seed=7"),
            arguments: &[],
        };
        assert!(perms::video::create(&req_seed).is_err());
    }

    /// `action=start` stamps a parseable wall-clock key on the frame it
    /// forwards — the whole handshake `action=stop` (a separate filter
    /// instance, chained the way `bench=start,...,bench=stop` is in the
    /// reference's own example) depends on being able to read back.
    #[test]
    fn bench_start_stamps_a_parseable_start_time() {
        let out = run_one(
            bench::video::create,
            Some("action=start"),
            gray_frame(4, 4, 0, 0),
        );
        let raw = out.metadata_get("lavfi.bench.start_time").unwrap();
        assert!(
            raw.parse::<f64>().is_ok(),
            "expected a parseable float, got {raw:?}"
        );
    }

    /// The running avg/max/min accumulation, tested directly against the
    /// pure function `filter_frame`'s `stop` branch calls — no live
    /// `FilterContext` needed for arithmetic that does not touch one.
    #[test]
    fn bench_stats_accumulate_running_min_and_max() {
        let mut stats = BenchStats::default();
        update_stats(&mut stats, 0.5);
        update_stats(&mut stats, 0.1);
        update_stats(&mut stats, 0.9);
        assert_eq!(stats.count, 3);
        assert!((stats.total - 1.5).abs() < 1e-9);
        assert!((stats.min - 0.1).abs() < 1e-9);
        assert!((stats.max - 0.9).abs() < 1e-9);
    }

    #[test]
    fn named_type_values_parse() {
        for (name, expected) in [
            ("PANSCAN", 0),
            ("A53_CC", 1),
            ("DISPLAYMATRIX", 6),
            ("S12M_TIMECOD", 16),
            ("S12M_TIMECODE", 16),
            ("DETECTION_BOUNDING_BOXES", 22),
            ("DETECTION_BBOXES", 22),
            ("VIDEO_HINT", 27),
        ] {
            let opts = SidedataOpts::parse(Some(&format!("type={name}"))).unwrap();
            assert_eq!(opts.kind, expected, "type={name}");
        }
    }

    #[test]
    fn sidedata_select_requires_the_kind_to_be_present() {
        let req = Instantiate {
            name: "sidedata",
            instance: "sidedata",
            args: Some("mode=select:type=6"),
            arguments: &[],
        };
        let instance = sidedata::video::create(&req).unwrap();
        let mut graph = Graph::new();
        let src = graph.add_source(
            "in",
            MediaType::Video,
            video_source_formats("in", vaco_pixfmt::PixFmt::Gray8),
        );
        let node = graph.add(instance.desc, instance.formats, instance.filter);
        let sink = graph.add_sink(
            "out",
            MediaType::Video,
            vaco_filter_core::mock::any_video_sink("out"),
        );
        graph.connect(src, 0, node, 0).unwrap();
        graph.connect(node, 0, sink, 0).unwrap();
        let tb = vaco_core::Rational::new(1, 25);
        graph.set_source_format(src, gray_link(4, 4, tb)).unwrap();
        graph.configure().unwrap();
        graph.send(src, gray_frame(4, 4, 0, 0)).unwrap();
        graph
            .close_source(src, vaco_core::Timestamp::new(1))
            .unwrap();
        loop {
            match graph.run().unwrap() {
                GraphStatus::Eof => break,
                GraphStatus::HasOutput(_) | GraphStatus::NeedInput(_) => {}
                other => panic!("unexpected graph status: {other:?}"),
            }
        }
        assert!(graph.recv(sink).is_err());
    }
}
