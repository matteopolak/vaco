//! `-mpegts_*`, `-muxrate`, `-pes_payload_size`, `-pat_period`/`-sdt_period`/
//! `-pcr_period`, and `-mpegts_flags`.
//!
//! Names, defaults and the flag list are measured against
//! `ffmpeg -h muxer=mpegts` (ffmpeg 8.1, `LC_ALL=C`), not recalled — this
//! crate's docs keep the transcript. As with `vaco-mux-mp4`'s `MovOptions`,
//! this is not routed through [`vaco_format_core::FormatOptions`]: that type
//! is the options every container shares, and these are MPEG-TS-specific in
//! the same way `movflags` is MP4-specific. [`MpegTsMuxer::with_options`]
//! (`crate::mux`) is the entry point a caller who needs anything beyond the
//! registry's default construction uses.

bitflags::bitflags! {
    /// `-mpegts_flags`, one bit per flag exactly as the reference names them.
    ///
    /// The *names* are interface facts (D9); the bit values are ours.
    /// `omit_rai` was not in this issue's original brief and was found only
    /// by probing `-h muxer=mpegts` directly — measure, don't recall.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct MpegTsFlags: u32 {
        /// Reemit PAT/PMT before every packet, instead of only when
        /// `pat_period` has elapsed.
        const RESEND_HEADERS = 1 << 0;
        /// Use LATM/LOAS packetisation for AAC instead of ADTS.
        ///
        /// Not implemented by this muxer yet: LATM framing needs an
        /// AudioSpecificConfig rewrite this crate does not currently do (see
        /// the crate docs). The flag is accepted so a caller's option string
        /// round-trips; setting it has no effect beyond selecting
        /// [`crate::mux::MpegTsMuxer::add_stream`]'s stream_type answer for
        /// AAC (`0x11` LATM instead of `0x0F` ADTS), matching what the
        /// reference's PMT would say even though the payload framing itself
        /// is unmodified.
        const LATM = 1 << 1;
        /// Reemit PAT and PMT at each video key frame.
        const PAT_PMT_AT_FRAMES = 1 << 2;
        /// Conform to DVB System B instead of ATSC System A: AC-3/E-AC-3/DTS
        /// use the private `stream_type` `0x06` plus a registration
        /// descriptor rather than their own ATSC `stream_type` values (see
        /// `vaco_format_mpegts_tables::stream_type::for_codec`'s `dvb`
        /// parameter, which this flag drives directly).
        const SYSTEM_B = 1 << 3;
        /// Mark the very first packet on every PID discontinuous.
        const INITIAL_DISCONTINUITY = 1 << 4;
        /// Emit a Network Information Table. Not implemented: this muxer
        /// never writes a NIT regardless of this flag (see the crate docs).
        const NIT = 1 << 5;
        /// Disable the adaptation field's `random_access_indicator` this
        /// muxer would otherwise set on a keyframe packet.
        const OMIT_RAI = 1 << 6;
    }
}

/// `-mpegts_service_type`: EN 300 468 Table 87's `service_type` byte, named
/// the way the reference's `AVOption` enum spells each constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceType(pub u8);

impl ServiceType {
    pub const DIGITAL_TV: Self = Self(0x01);
    pub const DIGITAL_RADIO: Self = Self(0x02);
    pub const TELETEXT: Self = Self(0x03);
    pub const ADVANCED_CODEC_DIGITAL_RADIO: Self = Self(0x0A);
    pub const MPEG2_DIGITAL_HDTV: Self = Self(0x11);
    pub const ADVANCED_CODEC_DIGITAL_SDTV: Self = Self(0x16);
    pub const ADVANCED_CODEC_DIGITAL_HDTV: Self = Self(0x19);
    pub const HEVC_DIGITAL_HDTV: Self = Self(0x1F);
}

impl Default for ServiceType {
    fn default() -> Self {
        Self::DIGITAL_TV
    }
}

/// Everything this crate needs that is not a stream.
///
/// Every field defaults to the value `ffmpeg -h muxer=mpegts` reports for a
/// fresh invocation. `pmt_start_pid`/`start_pid` bounds (`32..=8186`) are the
/// reference's own; this crate does not re-validate them beyond fitting in
/// the 13-bit PID field, on the theory that a caller passing a value outside
/// the reference's documented range is not this muxer's problem to police.
///
/// `service_name`/`service_provider` are the one exception to "every field
/// has an AVOption": `-h muxer=mpegts` lists no such options at all (measured
/// 2026-08-23; there is no `-service_name`/`-service_provider` flag), so
/// these two are the reference's own hardcoded fallback strings rather than
/// a documented default — recovered by probing `-c copy -f mpegts`'s SDT
/// service descriptor directly (tag `0x48`: `provider_name_length=6
/// "FFmpeg"`, `service_name_length=9 "Service01"`), not by recalling them.
#[derive(Debug, Clone)]
pub struct MpegTsMuxOptions {
    pub transport_stream_id: u16,
    pub original_network_id: u16,
    pub service_id: u16,
    pub service_type: ServiceType,
    pub service_name: String,
    pub service_provider: String,
    pub pmt_start_pid: u16,
    pub start_pid: u16,
    /// `Some(true)`/`Some(false)` for an explicit `-mpegts_m2ts_mode`;
    /// `None` for `auto`, which this crate resolves by the output's own
    /// declared intent — [`crate::mux::MpegTsMuxer::with_options`]'s caller
    /// decides that from the filename, the way `ffmpeg`'s CLI layer does,
    /// since this trait has no filename to look at itself.
    pub m2ts_mode: Option<bool>,
    /// Bits per second, or `None` for the reference's "unset" sentinel
    /// (`-muxrate 1`, which is not a real rate and only exists because the
    /// option has no separate `bool` for "auto"). `None` means: never insert
    /// stuffing packets to hold a constant rate, and use the PES/PSI byte
    /// stream's own timing for M2TS arrival timestamps (see `crate::tsw`).
    pub muxrate_bps: Option<u64>,
    pub pes_payload_size: usize,
    pub flags: MpegTsFlags,
    /// PAT/PMT `version_number`, and SDT/NIT's too.
    pub tables_version: u8,
    /// Omit `PES_packet_length` on video packets (write `0`, "unbounded").
    pub omit_video_pes_length: bool,
    /// Milliseconds between PCR insertions on the PCR PID, or `None` for
    /// "auto" (`-pcr_period -1`). This crate's auto behaviour is a fixed
    /// [`crate::tsw::DEFAULT_PCR_PERIOD_MS`], simpler than the reference's
    /// frame-timing-aware schedule (see the crate docs) but within the
    /// specification's own hundred-millisecond ceiling either way.
    pub pcr_period_ms: Option<u32>,
    pub pat_period_ms: u32,
    pub sdt_period_ms: u32,
}

impl Default for MpegTsMuxOptions {
    fn default() -> Self {
        Self {
            transport_stream_id: 1,
            original_network_id: 0xFF01,
            service_id: 1,
            service_type: ServiceType::default(),
            // Measured, not an AVOption default — see this struct's doc comment.
            service_name: String::from("Service01"),
            service_provider: String::from("FFmpeg"),
            pmt_start_pid: 0x1000,
            start_pid: 0x0100,
            m2ts_mode: None,
            muxrate_bps: None,
            pes_payload_size: 2930,
            flags: MpegTsFlags::empty(),
            tables_version: 0,
            omit_video_pes_length: true,
            pcr_period_ms: None,
            pat_period_ms: 100,
            sdt_period_ms: 500,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_measured_reference() {
        let o = MpegTsMuxOptions::default();
        assert_eq!(o.transport_stream_id, 1);
        assert_eq!(o.original_network_id, 0xFF01);
        assert_eq!(o.service_id, 1);
        assert_eq!(o.service_type, ServiceType::DIGITAL_TV);
        assert_eq!(o.pmt_start_pid, 4096);
        assert_eq!(o.start_pid, 256);
        assert_eq!(o.pes_payload_size, 2930);
        assert!(o.flags.is_empty());
        assert_eq!(o.tables_version, 0);
        assert!(o.omit_video_pes_length);
        assert_eq!(o.pat_period_ms, 100);
        assert_eq!(o.sdt_period_ms, 500);
        assert_eq!(o.pcr_period_ms, None);
        assert_eq!(o.muxrate_bps, None);
        assert_eq!(o.m2ts_mode, None);
    }

    #[test]
    fn flag_names_match_the_reference_spelling() {
        let f = MpegTsFlags::RESEND_HEADERS | MpegTsFlags::SYSTEM_B;
        assert!(f.contains(MpegTsFlags::RESEND_HEADERS));
        assert!(f.contains(MpegTsFlags::SYSTEM_B));
        assert!(!f.contains(MpegTsFlags::NIT));
    }
}
