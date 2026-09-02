//! The generic format-level option set.
//!
//! Every demuxer and muxer in the project sees these, in addition to whatever
//! private options it declares. The option *names*, their types, their defaults
//! and their named constants are interface facts (D9) and are reproduced
//! exactly: a user migrating a script types `-fflags +genpts` and it has to
//! mean the same thing.
//!
//! # Where the values came from
//!
//! Not from a plan and not from memory: from `ffmpeg -h full`'s
//! `AVFormatContext AVOptions` block on the pinned reference (8.1), read as
//! black-box observed behaviour of a shipped binary, which is exactly what D6
//! and D7 permit. Three consequences worth recording, since an older survey
//! got them wrong:
//!
//! * `fflags` has **twelve** constants, not fourteen. There is no `nonblock`
//!   and no `shortest`. `flush_packets`, `bitexact` and `autobsf` are
//!   encoding-side.
//! * `fdebug` has **one** constant, `ts`. There is no `id3v2`.
//! * `recursion_limit` does not exist on the reference at all. We keep it
//!   anyway — it is a security bound on nested demuxer opens (concat lists,
//!   HLS variants), it is enforced here so no nested demuxer can forget it, and
//!   being a strict superset breaks no script (D17's converse case).
//!
//! # Which of these this crate actually consumes
//!
//! The table is one object because that is what the CLI's grouping model needs,
//! but the options are honoured in three different places. [`FormatOptions`]
//! documents each field with the rule it feeds:
//!
//! | Consumer | Options |
//! |---|---|
//! | [`crate::probe`] | `formatprobesize`, `format_whitelist`, `skip_initial_bytes` |
//! | [`crate::discovery`] | `probesize`, `analyzeduration`, `fpsprobesize`, `max_ts_probe`, `max_probe_packets`, `codec_whitelist`, `max_streams` |
//! | [`crate::time`] | `fflags`, `correct_ts_overflow`, `duration_probesize`, `skip_estimate_duration_from_pts`, `use_wallclock_as_timestamps` |
//! | [`crate::seek`] | `seek2any`, `indexmem`, `fflags` (`ignidx`, `fastseek`) |
//! | [`crate::interleave`] | `max_interleave_delta`, `audio_preload`, `chunk_duration`, `chunk_size`, `avoid_negative_ts`, `output_ts_offset` |
//! | carried, consumed elsewhere | `avioflags`, `packetsize`, `cryptokey`, `fdebug`, `start_time_realtime`, `flush_packets`, `metadata_header_padding`, `strict`, `dump_separator`, `protocol_whitelist`, `protocol_blacklist` |
//! | parsed, refused if non-default (rule I; see [`FormatOptions::validate`]) | `rtbufsize`, `max_delay`, `err_detect` |

use vaco_core::Duration;
use vaco_opts::{Binary, ConstDesc, Options, opt_flags};

opt_flags! {
    /// I/O layer behaviour.
    #[unit = "avioflags"]
    pub struct AvioFlags: u64 {
        /// reduce buffering
        const DIRECT = 1 << 0 => "direct";
    }
}

opt_flags! {
    /// Format-layer behaviour switches.
    #[unit = "fflags"]
    pub struct FFlags: u64 {
        /// reduce the latency by flushing out packets immediately
        const FLUSH_PACKETS = 1 << 0 => "flush_packets";
        /// ignore index
        const IGNIDX = 1 << 1 => "ignidx";
        /// generate pts
        const GENPTS = 1 << 2 => "genpts";
        /// do not fill in missing values that can be exactly calculated
        const NOFILLIN = 1 << 3 => "nofillin";
        /// disable AVParsers, this needs nofillin too
        const NOPARSE = 1 << 4 => "noparse";
        /// ignore dts
        const IGNDTS = 1 << 5 => "igndts";
        /// discard corrupted frames
        const DISCARDCORRUPT = 1 << 6 => "discardcorrupt";
        /// try to interleave outputted packets by dts
        const SORTDTS = 1 << 7 => "sortdts";
        /// fast but inaccurate seeks
        const FASTSEEK = 1 << 8 => "fastseek";
        /// reduce the latency introduced by optional buffering
        const NOBUFFER = 1 << 9 => "nobuffer";
        /// do not write random/volatile data
        const BITEXACT = 1 << 10 => "bitexact";
        /// add needed bsfs automatically
        const AUTOBSF = 1 << 11 => "autobsf";
    }
}

opt_flags! {
    /// Debug tracing selectors. Mapped onto `tracing` targets, not onto a
    /// bespoke printer.
    #[unit = "fdebug"]
    pub struct FDebugFlags: u64 {
        /// timestamps
        const TS = 1 << 0 => "ts";
    }
}

opt_flags! {
    /// How hard to look for corruption, and what to do about it.
    #[unit = "err_detect"]
    pub struct ErrDetectFlags: u64 {
        /// verify embedded CRCs
        const CRCCHECK = 1 << 0 => "crccheck";
        /// detect bitstream specification deviations
        const BITSTREAM = 1 << 1 => "bitstream";
        /// detect improper bitstream length
        const BUFFER = 1 << 2 => "buffer";
        /// abort decoding on minor error detection
        const EXPLODE = 1 << 3 => "explode";
        /// ignore errors
        const IGNORE_ERR = 1 << 4 => "ignore_err";
        /// consider things that violate the spec, are fast to check and have not been seen in the wild as errors
        const CAREFUL = 1 << 5 => "careful";
        /// consider all spec non compliancies as errors
        const COMPLIANT = 1 << 6 => "compliant";
        /// consider things that a sane encoder shouldn't do as an error
        const AGGRESSIVE = 1 << 7 => "aggressive";
    }
}

/// Named constants for `strict`.
pub const STRICT_CONSTS: &[ConstDesc] = &[
    ConstDesc::new(
        "very",
        "strictly conform to a older more strict version of the spec or reference software",
        "strict",
        2,
    ),
    ConstDesc::new(
        "strict",
        "strictly conform to all the things in the spec no matter what the consequences",
        "strict",
        1,
    ),
    ConstDesc::new("normal", "", "strict", 0),
    ConstDesc::new("unofficial", "allow unofficial extensions", "strict", -1),
    ConstDesc::new(
        "experimental",
        "allow non-standardized experimental variants",
        "strict",
        -2,
    ),
];

/// Named constants for `avoid_negative_ts`. See [`AvoidNegativeTs`].
pub const AVOID_NEGATIVE_TS_CONSTS: &[ConstDesc] = &[
    ConstDesc::new(
        "auto",
        "enabled when required by target format",
        "avoid_negative_ts",
        -1,
    ),
    ConstDesc::new(
        "disabled",
        "do not change timestamps",
        "avoid_negative_ts",
        0,
    ),
    ConstDesc::new(
        "make_non_negative",
        "shift timestamps so they are non negative",
        "avoid_negative_ts",
        1,
    ),
    ConstDesc::new(
        "make_zero",
        "shift timestamps so they start at 0",
        "avoid_negative_ts",
        2,
    ),
];

/// The resolved form of the `avoid_negative_ts` option.
///
/// `auto` is not a policy, it is a request for one, so it does not appear here:
/// [`AvoidNegativeTs::resolve`] turns the option's integer into a decision using
/// the muxer's [`FormatFlags`](crate::FormatFlags), and everything downstream
/// works with the decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AvoidNegativeTs {
    /// Pass timestamps through untouched.
    #[default]
    Disabled,
    /// Shift only if the first DTS is negative.
    MakeNonNegative,
    /// Shift so the first DTS is exactly zero, in either direction.
    MakeZero,
}

impl AvoidNegativeTs {
    /// Resolve the option value against the muxer's flags.
    ///
    /// `auto` (-1) becomes [`Self::MakeNonNegative`] unless the container can
    /// represent negative timestamps, in which case it becomes
    /// [`Self::Disabled`]. Any unrecognised value is treated as `auto`, which
    /// is the safe direction: a container that cannot store a negative
    /// timestamp must not be handed one.
    #[must_use]
    pub const fn resolve(value: i32, ts_negative_ok: bool) -> Self {
        match value {
            0 => Self::Disabled,
            1 => Self::MakeNonNegative,
            2 => Self::MakeZero,
            _ if ts_negative_ok => Self::Disabled,
            _ => Self::MakeNonNegative,
        }
    }
}

/// The 39-option generic format table.
///
/// Declaration order is the reference's, because it is the order
/// `-h demuxer=…` prints in and that output is part of the D5 contract.
#[allow(
    clippy::struct_excessive_bools,
    reason = "the reference's option table has this many booleans and every name is an \
              interface fact; grouping them into a flags type would change the CLI"
)]
#[derive(Debug, Clone, PartialEq, Options)]
#[options(
    name = "AVFormatContext",
    help = "generic container-level options shared by every demuxer and muxer"
)]
pub struct FormatOptions {
    /// I/O layer behaviour. Consumed by [`vaco_io::IoOptions`].
    #[opt(name = "avioflags", help = "", unit = "avioflags",
          default = AvioFlags::empty(), default_repr = "0", flags(param))]
    pub avioflags: AvioFlags,

    /// Byte budget for stream discovery ([`crate::discovery`]).
    #[opt(name = "probesize", help = "set probing size", default = 5_000_000_i64,
          range = 32_i64..=i64::MAX, flags(decoding))]
    pub probesize: i64,

    /// Ceiling for the format-detection retry loop ([`crate::probe`] R7).
    #[opt(name = "formatprobesize", help = "number of bytes to probe file format",
          default = PROBE_BUF_MAX as i32, range = 0..=i32::MAX, flags(decoding))]
    pub formatprobesize: i32,

    /// Fixed output packet size; MPEG-PS/TS only.
    #[opt(name = "packetsize", help = "set packet size", default = 0,
          range = 0..=i32::MAX, flags(encoding))]
    pub packetsize: i32,

    /// The behaviour switches. Consumed all over [`crate::time`] and
    /// [`crate::seek`].
    #[opt(name = "fflags", help = "", unit = "fflags",
          default = FFlags::AUTOBSF, default_repr = "autobsf", flags(param))]
    pub fflags: FFlags,

    /// Permit landing on a non-keyframe even when the caller did not ask for it.
    #[opt(
        name = "seek2any",
        help = "allow seeking to non-keyframes on demuxer level when supported",
        default = false,
        flags(decoding)
    )]
    pub seek2any: bool,

    /// Media-time budget for stream discovery, in microseconds. Zero means the
    /// per-format default.
    #[opt(name = "analyzeduration",
          help = "specify how many microseconds are analyzed to probe the input",
          default = 0_i64, range = 0_i64..=i64::MAX, flags(decoding))]
    pub analyzeduration: i64,

    /// Raw decryption key, consumed by container-level encryption.
    #[opt(name = "cryptokey", help = "decryption key", flags(decoding))]
    pub cryptokey: Binary,

    /// Per-stream index memory cap ([`crate::seek::PacketIndex`]).
    #[opt(name = "indexmem", help = "max memory used for timestamp index (per stream)",
          default = 1_048_576, range = 0..=i32::MAX, flags(decoding))]
    pub indexmem: i32,

    /// Realtime capture buffer cap. Devices only.
    #[opt(name = "rtbufsize", help = "max memory used for buffering real-time frames",
          default = 3_041_280, range = 0..=i32::MAX, flags(decoding))]
    pub rtbufsize: i32,

    /// Debug tracing selectors.
    #[opt(name = "fdebug", help = "print specific debug info", unit = "fdebug",
          default = FDebugFlags::empty(), default_repr = "0", flags(param))]
    pub fdebug: FDebugFlags,

    /// Muxing/demuxing delay bound, microseconds.
    #[opt(name = "max_delay", help = "maximum muxing or demuxing delay in microseconds",
          default = -1, range = -1..=i32::MAX, flags(param))]
    pub max_delay: i32,

    /// Wall clock corresponding to PTS 0. Suppressed by `fflags +bitexact`.
    #[opt(name = "start_time_realtime", help = "wall-clock time when stream begins (PTS==0)",
          default = i64::MIN, default_repr = "I64_MIN",
          range = i64::MIN..=i64::MAX, flags(encoding))]
    pub start_time_realtime: i64,

    /// Frames used to establish the frame rate. -1 means the default.
    #[opt(name = "fpsprobesize", help = "number of frames used to probe fps",
          default = -1, range = -1..=i32::MAX, flags(decoding))]
    pub fpsprobesize: i32,

    /// Bias audio packets earlier for interleaving purposes only.
    #[opt(name = "audio_preload",
          help = "microseconds by which audio packets should be interleaved earlier",
          default = 0, range = 0..=i32::MAX, flags(encoding))]
    pub audio_preload: i32,

    /// Chunk length target ([`crate::interleave::ChunkPolicy`]).
    #[opt(name = "chunk_duration", help = "microseconds for each chunk", default = 0,
          range = 0..=i32::MAX, flags(encoding))]
    pub chunk_duration: i32,

    /// Chunk size target.
    #[opt(name = "chunk_size", help = "size in bytes for each chunk", default = 0,
          range = 0..=i32::MAX, flags(encoding))]
    pub chunk_size: i32,

    /// Corruption detection policy. `f_err_detect` is the deprecated spelling
    /// and is accepted as an alias of the same field.
    #[opt(name = "err_detect", alias = "f_err_detect", help = "set error detection flags",
          unit = "err_detect", default = ErrDetectFlags::CRCCHECK,
          default_repr = "crccheck", flags(decoding))]
    pub err_detect: ErrDetectFlags,

    /// Deliberately violates the determinism rule; excluded from the
    /// conformance corpus.
    #[opt(
        name = "use_wallclock_as_timestamps",
        help = "use wallclock as timestamps",
        default = false,
        flags(decoding)
    )]
    pub use_wallclock_as_timestamps: bool,

    /// Applied at the I/O layer before probing ([`crate::probe`] R10).
    #[opt(name = "skip_initial_bytes",
          help = "set number of bytes to skip before reading header and frames",
          default = 0_i64, range = 0_i64..=i64::MAX, flags(decoding))]
    pub skip_initial_bytes: i64,

    /// Enable the mid-stream wraparound correction ([`crate::time`] R9).
    #[opt(
        name = "correct_ts_overflow",
        help = "correct single timestamp overflows",
        default = true,
        flags(decoding)
    )]
    pub correct_ts_overflow: bool,

    /// Flush the I/O context after each packet. -1 is the format default.
    #[opt(name = "flush_packets",
          help = "enable flushing of the I/O context after each packet",
          default = -1, range = -1..=1, flags(encoding))]
    pub flush_packets: i32,

    /// Reserved bytes in the written metadata header.
    #[opt(name = "metadata_header_padding",
          help = "set number of bytes to be written as padding in a metadata header",
          default = -1, range = -1..=i32::MAX, flags(encoding))]
    pub metadata_header_padding: i32,

    /// Added to every output timestamp ([`crate::interleave`] M2).
    #[opt(name = "output_ts_offset", help = "set output timestamp offset",
          default = Duration::ZERO, default_repr = "0", flags(encoding))]
    pub output_ts_offset: Duration,

    /// Sparse-stream escape threshold ([`crate::interleave`] N3), microseconds.
    #[opt(name = "max_interleave_delta", help = "maximum buffering duration for interleaving",
          default = 10_000_000_i64, range = 0_i64..=i64::MAX, flags(encoding))]
    pub max_interleave_delta: i64,

    /// Standards compliance. `f_strict` is the deprecated spelling.
    #[opt(name = "strict", alias = "f_strict", help = "how strictly to follow the standards",
          unit = "strict", consts = STRICT_CONSTS, default = 0, default_repr = "normal",
          range = i32::MIN..=i32::MAX, flags(param))]
    pub strict: i32,

    /// Packets read while waiting for a first timestamp.
    #[opt(name = "max_ts_probe",
          help = "maximum number of packets to read while waiting for the first timestamp",
          default = 50, range = 0..=i32::MAX, flags(decoding))]
    pub max_ts_probe: i32,

    /// Output timestamp shifting policy ([`crate::interleave`] M3). Resolve it
    /// with [`AvoidNegativeTs::resolve`] rather than matching on the integer.
    #[opt(name = "avoid_negative_ts", help = "shift timestamps so they start at 0",
          unit = "avoid_negative_ts", consts = AVOID_NEGATIVE_TS_CONSTS,
          default = -1, default_repr = "auto", range = -1..=2, flags(encoding))]
    pub avoid_negative_ts: i32,

    /// Consumed by the CLI's info dump; carried here because it lives on the
    /// context.
    #[opt(name = "dump_separator", help = "set information dump field separator",
          default = String::new(), default_repr = ", ", flags(param))]
    pub dump_separator: String,

    /// Comma-separated. A stream whose codec is not listed is reported without
    /// one.
    #[opt(
        name = "codec_whitelist",
        help = "List of decoders that are allowed to be used",
        flags(decoding)
    )]
    pub codec_whitelist: String,

    /// Comma-separated. Filters the candidate set *before* probing
    /// ([`crate::probe`] R9).
    #[opt(
        name = "format_whitelist",
        help = "List of demuxers that are allowed to be used",
        flags(decoding)
    )]
    pub format_whitelist: String,

    /// Comma-separated. Enforced by the protocol layer.
    #[opt(
        name = "protocol_whitelist",
        help = "List of protocols that are allowed to be used",
        flags(decoding)
    )]
    pub protocol_whitelist: String,

    /// Comma-separated. Wins over the whitelist.
    #[opt(
        name = "protocol_blacklist",
        help = "List of protocols that are not allowed to be used",
        flags(decoding)
    )]
    pub protocol_blacklist: String,

    /// Hard cap. Exceeding it is an error, not a truncation.
    #[opt(name = "max_streams", help = "maximum number of streams", default = 1000,
          range = 0..=i32::MAX, flags(decoding))]
    pub max_streams: i32,

    /// Suppress the tail-scan duration estimate ([`crate::time`] R15).
    #[opt(
        name = "skip_estimate_duration_from_pts",
        help = "skip duration calculation in estimate_timings_from_pts",
        default = false,
        flags(decoding)
    )]
    pub skip_estimate_duration_from_pts: bool,

    /// Packets fed to a codec parser per stream during discovery.
    #[opt(name = "max_probe_packets", help = "Maximum number of packets to probe a codec",
          default = 2500, range = 0..=i32::MAX, flags(decoding))]
    pub max_probe_packets: i32,

    /// Bytes read from the tail for the `FromPts` duration estimate. Zero means
    /// the built-in default, [`DEFAULT_DURATION_PROBESIZE`].
    #[opt(name = "duration_probesize",
          help = "Maximum number of bytes to probe the durations of the streams",
          default = 0_i64, range = 0_i64..=i64::MAX, flags(decoding))]
    pub duration_probesize: i64,

    /// Depth cap on nested demuxer opens: concat lists, HLS variant playlists,
    /// DASH periods, `tee`.
    ///
    /// **Ours, not the reference's.** Meant as a security bound enforced here
    /// rather than per format so that no nested demuxer can forget it -- but
    /// nothing in this tree actually reads this field today. The real depth
    /// cap for the one nested-open path that exists so far,
    /// `vaco-format-adaptive::RemoteAccess`/`WriteAccess`'s own
    /// `recursion_limit` (an unrelated field of the same name, forwarded into
    /// `vaco_protocol_core::ProtocolEnv`), is a separate, hardcoded constant
    /// this field does not feed. [`FormatOptions::validate`] refuses a
    /// non-default value rather than continuing to claim the enforcement
    /// this doc used to (found by `cargo xtask reachability-check`'s rule I).
    #[opt(name = "recursion_limit", help = "maximum depth of nested demuxer opens",
          default = 10, range = 0..=1000, flags(decoding))]
    pub recursion_limit: i32,
}

/// Smallest buffer the format-detection retry loop starts from.
pub const PROBE_BUF_MIN: usize = 2048;

/// Largest buffer the retry loop will grow to, and the default of
/// `formatprobesize` — measured from the reference, which reports
/// `(default 1048576)`.
pub const PROBE_BUF_MAX: usize = 1 << 20;

/// The built-in `duration_probesize`, used when the option is left at zero.
///
/// The reference's own value is unverified — a rough "around 250 kB". We have
/// not measured it either, so this is **our** choice, recorded as such rather
/// than presented as a reproduction: 250 KiB is large enough to hold the tail
/// index of any container we ship and small enough that the estimate costs
/// one read.
pub const DEFAULT_DURATION_PROBESIZE: u64 = 250 * 1024;

impl FormatOptions {
    /// Split a comma-separated list option into its entries, trimmed, with
    /// empties dropped.
    ///
    /// Returns `None` for an unset (empty) option, which is *not* the same as
    /// an empty list: an unset whitelist permits everything, an empty one
    /// permits nothing.
    #[must_use]
    pub fn list(value: &str) -> Option<impl Iterator<Item = &str>> {
        if value.is_empty() {
            return None;
        }
        Some(value.split(',').map(str::trim).filter(|s| !s.is_empty()))
    }

    /// Whether `name` passes the `format_whitelist`.
    #[must_use]
    pub fn format_allowed(&self, name: &str) -> bool {
        Self::list(&self.format_whitelist).is_none_or(|mut it| it.any(|n| n == name))
    }

    /// Whether `name` passes the `codec_whitelist`.
    #[must_use]
    pub fn codec_allowed(&self, name: &str) -> bool {
        Self::list(&self.codec_whitelist).is_none_or(|mut it| it.any(|n| n == name))
    }

    /// The format-probe ceiling, clamped into `[PROBE_BUF_MIN, PROBE_BUF_MAX]`.
    #[must_use]
    pub fn probe_ceiling(&self) -> usize {
        usize::try_from(self.formatprobesize)
            .unwrap_or(PROBE_BUF_MAX)
            .clamp(PROBE_BUF_MIN, PROBE_BUF_MAX)
    }

    /// `duration_probesize`, with zero meaning [`DEFAULT_DURATION_PROBESIZE`].
    #[must_use]
    pub fn effective_duration_probesize(&self) -> u64 {
        match u64::try_from(self.duration_probesize) {
            Ok(0) | Err(_) => DEFAULT_DURATION_PROBESIZE,
            Ok(n) => n,
        }
    }

    /// Whether the timestamp-generation rules ([`crate::time`] R19, R21, R22)
    /// are enabled.
    #[must_use]
    pub const fn fills_in_timestamps(&self) -> bool {
        !self.fflags.contains(FFlags::NOFILLIN)
    }

    /// `+noparse` requires `+nofillin`; the reference documents the dependency
    /// and silently tolerates the mismatch. We reject it, because silently
    /// repairing an option set is how a user ends up with output they did not
    /// ask for.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::Option`] naming `fflags`.
    pub fn validate(&self) -> vaco_core::Result<()> {
        if self.fflags.contains(FFlags::NOPARSE) && !self.fflags.contains(FFlags::NOFILLIN) {
            return Err(vaco_core::Error::Option {
                name: "fflags".to_owned(),
                detail: "+noparse requires +nofillin".to_owned(),
            });
        }
        // `rtbufsize`/`max_delay`/`err_detect`: this doc's own table used to
        // list these as "carried, consumed elsewhere" -- measured: zero
        // `.rtbufsize`/`.max_delay`/`.err_detect` reads anywhere under
        // `crates/` outside this file. Found by `cargo xtask
        // reachability-check`'s rule I; refusing a non-default value rather
        // than continuing to advertise a knob that does nothing.
        if self.rtbufsize != 3_041_280 {
            return Err(vaco_core::Error::Option {
                name: "rtbufsize".to_owned(),
                detail: "parsed but not consumed by any demuxer, muxer or I/O layer in this build; \
                         refusing rather than silently ignoring it".to_owned(),
            });
        }
        if self.max_delay != -1 {
            return Err(vaco_core::Error::Option {
                name: "max_delay".to_owned(),
                detail: "parsed but not consumed by any demuxer, muxer or I/O layer in this build; \
                         refusing rather than silently ignoring it".to_owned(),
            });
        }
        if self.err_detect != ErrDetectFlags::CRCCHECK {
            return Err(vaco_core::Error::Option {
                name: "err_detect".to_owned(),
                detail: "parsed but not consumed by any demuxer, muxer or I/O layer in this build; \
                         refusing rather than silently ignoring it".to_owned(),
            });
        }
        // `recursion_limit` (default 10): ours, not the reference's, meant as
        // a security bound on nested demuxer opens (concat lists, HLS variant
        // playlists, DASH periods, `tee`) enforced here so no nested demuxer
        // can forget it. But nothing does: the one nested-open path that
        // exists today, `vaco-format-adaptive::RemoteAccess`/`WriteAccess`,
        // has its own hardcoded `recursion_limit` forwarded into
        // `vaco_protocol_core::ProtocolEnv`, a separate value this field does
        // not feed (measured: zero `.recursion_limit` reads on this type
        // anywhere under `crates/`). Same reasoning as the three fields
        // above.
        if self.recursion_limit != 10 {
            return Err(vaco_core::Error::Option {
                name: "recursion_limit".to_owned(),
                detail: "parsed but not consumed by any demuxer, muxer or I/O layer in this build; \
                         refusing rather than silently ignoring it".to_owned(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::field_reassign_with_default,
    clippy::cast_possible_wrap,
    clippy::unnecessary_wraps,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_opts::{OptionsExt, schema_of};

    #[test]
    fn defaults_match_the_reference() {
        let o = FormatOptions::default();
        assert_eq!(o.probesize, 5_000_000);
        assert_eq!(o.formatprobesize, 1_048_576);
        assert_eq!(o.indexmem, 1_048_576);
        assert_eq!(o.rtbufsize, 3_041_280);
        assert_eq!(o.max_delay, -1);
        assert_eq!(o.fpsprobesize, -1);
        assert_eq!(o.max_ts_probe, 50);
        assert_eq!(o.max_streams, 1000);
        assert_eq!(o.max_probe_packets, 2500);
        assert_eq!(o.max_interleave_delta, 10_000_000);
        assert_eq!(o.avoid_negative_ts, -1);
        assert_eq!(o.start_time_realtime, i64::MIN);
        assert_eq!(o.fflags, FFlags::AUTOBSF);
        assert_eq!(o.err_detect, ErrDetectFlags::CRCCHECK);
        assert!(o.correct_ts_overflow);
        assert!(!o.seek2any);
    }

    /// `rtbufsize`/`max_delay`/`err_detect`/`recursion_limit` parse but
    /// nothing in this tree's demuxers, muxers or I/O layer reads any of
    /// them yet -- this doc's own table and the `recursion_limit` field doc
    /// used to claim otherwise. `validate` refuses a non-default value
    /// instead of silently dropping it. Regression for `cargo xtask
    /// reachability-check`'s rule I.
    #[test]
    fn validate_refuses_four_unconsumed_generic_options() {
        let base = FormatOptions::default();
        assert!(base.validate().is_ok());

        let mut o = base.clone();
        o.rtbufsize = 1;
        assert!(o.validate().is_err());

        let mut o = base.clone();
        o.max_delay = 0;
        assert!(o.validate().is_err());

        let mut o = base.clone();
        o.err_detect = ErrDetectFlags::empty();
        assert!(o.validate().is_err());

        let mut o = base.clone();
        o.recursion_limit = 5;
        assert!(o.validate().is_err());
    }

    #[test]
    fn option_names_are_the_reference_set() {
        // The exact list from `ffmpeg -h full`, in its order, plus our one
        // documented extension at the end.
        let expected = [
            "avioflags",
            "probesize",
            "formatprobesize",
            "packetsize",
            "fflags",
            "seek2any",
            "analyzeduration",
            "cryptokey",
            "indexmem",
            "rtbufsize",
            "fdebug",
            "max_delay",
            "start_time_realtime",
            "fpsprobesize",
            "audio_preload",
            "chunk_duration",
            "chunk_size",
            "err_detect",
            "use_wallclock_as_timestamps",
            "skip_initial_bytes",
            "correct_ts_overflow",
            "flush_packets",
            "metadata_header_padding",
            "output_ts_offset",
            "max_interleave_delta",
            "strict",
            "max_ts_probe",
            "avoid_negative_ts",
            "dump_separator",
            "codec_whitelist",
            "format_whitelist",
            "protocol_whitelist",
            "protocol_blacklist",
            "max_streams",
            "skip_estimate_duration_from_pts",
            "max_probe_packets",
            "duration_probesize",
            "recursion_limit",
        ];
        let got: Vec<&str> = schema_of::<FormatOptions>()
            .options
            .iter()
            .map(|o| o.name)
            .collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn deprecated_spellings_are_aliases() {
        let mut o = FormatOptions::default();
        o.set_str("f_strict", "experimental").unwrap();
        assert_eq!(o.strict, -2);
        o.set_str("f_err_detect", "+explode").unwrap();
        assert!(o.err_detect.contains(ErrDetectFlags::EXPLODE));
    }

    #[test]
    fn fflags_parse_by_name() {
        let mut o = FormatOptions::default();
        o.set_str("fflags", "+genpts+igndts").unwrap();
        assert!(o.fflags.contains(FFlags::GENPTS));
        assert!(o.fflags.contains(FFlags::IGNDTS));
        // `+` is additive over the default.
        assert!(o.fflags.contains(FFlags::AUTOBSF));
    }

    #[test]
    fn noparse_without_nofillin_is_rejected() {
        let mut o = FormatOptions::default();
        assert!(o.validate().is_ok());
        o.fflags.insert(FFlags::NOPARSE);
        assert!(o.validate().is_err());
        o.fflags.insert(FFlags::NOFILLIN);
        assert!(o.validate().is_ok());
    }

    #[test]
    fn whitelists_distinguish_unset_from_empty() {
        let mut o = FormatOptions::default();
        assert!(o.format_allowed("mp4"));
        o.format_whitelist = "matroska,mp4".to_owned();
        assert!(o.format_allowed("mp4"));
        assert!(!o.format_allowed("mpegts"));
        o.format_whitelist = ",".to_owned();
        assert!(!o.format_allowed("mp4"));
    }

    #[test]
    fn avoid_negative_ts_auto_follows_the_container() {
        assert_eq!(
            AvoidNegativeTs::resolve(-1, false),
            AvoidNegativeTs::MakeNonNegative
        );
        assert_eq!(
            AvoidNegativeTs::resolve(-1, true),
            AvoidNegativeTs::Disabled
        );
        assert_eq!(
            AvoidNegativeTs::resolve(0, false),
            AvoidNegativeTs::Disabled
        );
        assert_eq!(AvoidNegativeTs::resolve(2, true), AvoidNegativeTs::MakeZero);
        // An out-of-range value is treated as auto, never as "disabled".
        assert_eq!(
            AvoidNegativeTs::resolve(99, false),
            AvoidNegativeTs::MakeNonNegative
        );
    }

    #[test]
    fn probe_ceiling_is_clamped() {
        let mut o = FormatOptions::default();
        assert_eq!(o.probe_ceiling(), PROBE_BUF_MAX);
        o.formatprobesize = 0;
        assert_eq!(o.probe_ceiling(), PROBE_BUF_MIN);
        o.formatprobesize = i32::MAX;
        assert_eq!(o.probe_ceiling(), PROBE_BUF_MAX);
    }
}
