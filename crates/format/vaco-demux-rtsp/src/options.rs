//! `-h demuxer=rtsp` option surface.
//!
//! Names, defaults and flag values are transcribed from `ffmpeg -h
//! demuxer=rtsp` (8.1), read as observed behaviour of a shipped binary
//! (D6/D7/D17) — not from memory, not from the plan:
//!
//! ```text
//! Demuxer rtsp [RTSP input]:
//! RTSP demuxer AVOptions:
//!   -initial_pause     <boolean>    .D......... do not start playing the stream immediately (default false)
//!   -rtsp_transport    <flags>      ED......... set RTSP transport protocols (default 0)
//!      udp                          ED......... UDP
//!      tcp                          ED......... TCP
//!      udp_multicast                .D......... UDP multicast
//!      http                         .D......... HTTP tunneling
//!      https                        .D......... HTTPS tunneling
//!   -rtsp_flags        <flags>      .D......... set RTSP flags (default 0)
//!      filter_src                   .D......... only receive packets from the negotiated peer IP
//!      listen                       .D......... wait for incoming connections
//!      prefer_tcp                   ED......... try RTP via TCP first, if available
//!      satip_raw                    .D......... export raw MPEG-TS stream instead of demuxing
//!   -allowed_media_types <flags>      .D......... set media types to accept from the server (default video+audio+data+subtitle)
//!      video                        .D......... Video
//!      audio                        .D......... Audio
//!      data                         .D......... Data
//!      subtitle                     .D......... Subtitle
//!   -min_port          <int>        ED......... set minimum local UDP port (from 0 to 65535) (default 5000)
//!   -max_port          <int>        ED......... set maximum local UDP port (from 0 to 65535) (default 65000)
//!   -listen_timeout    <int>        .D......... set maximum timeout (in seconds) to wait for incoming connections (-1 is infinite, imply flag listen) (from INT_MIN to INT_MAX) (default -1)
//!   -timeout           <int64>      .D......... set timeout (in microseconds) of socket I/O operations (from INT_MIN to I64_MAX) (default 0)
//!   -reorder_queue_size <int>        .D......... set number of packets to buffer for handling of reordered packets (from -1 to INT_MAX) (default -1)
//!   -buffer_size       <int>        ED......... Underlying protocol send/receive buffer size (from -1 to INT_MAX) (default -1)
//!   -user_agent        <string>     .D......... override User-Agent header (default "Lavf62.12.100")
//! ```
//!
//! (`-ca_file`/`-cafile`/`-tls_verify`/`-verify`/`-cert_file`/`-cert`/
//! `-key_file`/`-key`/`-verifyhost` also appear, for `rtsps://` — **not
//! implemented in this pass**: this crate connects `tcp:` only today.
//! `rtsps://` reports [`vaco_core::Error::Unsupported`] rather than silently
//! falling back to plaintext, which would be the wrong failure mode for a
//! security option. `vaco-protocol-tls` already has everything a future pass
//! needs — `crate::connection::connect_tcp` is written so that swapping in
//! `vaco_protocol_tls::connect::handshake` after it is a small, local
//! change.)
//!
//! Two named constants earned real attention:
//!
//! * `rtsp_transport`'s default is `0` — **none of the five flags** — which
//!   the reference documents nowhere as "try udp then tcp" or similar; it is
//!   simply empty, and this crate's own [`RtspOptions::default`] preference
//!   order (documented on [`crate::transport`]) is this crate's choice, not
//!   an observed one, because an empty flag set has no observable ordering
//!   to measure.
//! * `allowed_media_types`'s default is **all four** media types, spelled
//!   out in the help text itself (`video+audio+data+subtitle`) rather than
//!   `0` the way every other flags option here is — transcribed exactly
//!   rather than "corrected" to `0`.

use vaco_opts::{Options, opt_flags};

opt_flags! {
    /// `-rtsp_transport`.
    #[unit = "rtsp_transport"]
    pub struct RtspTransportFlags: u64 {
        const UDP = 1 << 0 => "udp";
        const TCP = 1 << 1 => "tcp";
        const UDP_MULTICAST = 1 << 2 => "udp_multicast";
        const HTTP = 1 << 3 => "http";
        const HTTPS = 1 << 4 => "https";
    }
}

opt_flags! {
    /// `-rtsp_flags`.
    #[unit = "rtsp_flags"]
    pub struct RtspFlags: u64 {
        const FILTER_SRC = 1 << 0 => "filter_src";
        const LISTEN = 1 << 1 => "listen";
        const PREFER_TCP = 1 << 2 => "prefer_tcp";
        const SATIP_RAW = 1 << 3 => "satip_raw";
    }
}

opt_flags! {
    /// `-allowed_media_types`.
    #[unit = "allowed_media_types"]
    pub struct AllowedMediaTypes: u64 {
        const VIDEO = 1 << 0 => "video";
        const AUDIO = 1 << 1 => "audio";
        const DATA = 1 << 2 => "data";
        const SUBTITLE = 1 << 3 => "subtitle";
    }
}

/// `-h demuxer=rtsp`.
#[derive(Debug, Clone, PartialEq, Options)]
#[options(name = "rtsp", help = "RTSP input")]
pub struct RtspOptions {
    #[opt(
        name = "initial_pause",
        help = "do not start playing the stream immediately",
        default = false,
        flags(decoding)
    )]
    pub initial_pause: bool,

    #[opt(
        name = "rtsp_transport",
        help = "set RTSP transport protocols",
        unit = "rtsp_transport",
        default = RtspTransportFlags::empty(),
        default_repr = "0",
        flags(param)
    )]
    pub rtsp_transport: RtspTransportFlags,

    #[opt(
        name = "rtsp_flags",
        help = "set RTSP flags",
        unit = "rtsp_flags",
        default = RtspFlags::empty(),
        default_repr = "0",
        flags(decoding)
    )]
    pub rtsp_flags: RtspFlags,

    #[opt(
        name = "allowed_media_types",
        help = "set media types to accept from the server",
        unit = "allowed_media_types",
        default = AllowedMediaTypes::VIDEO
            .union(AllowedMediaTypes::AUDIO)
            .union(AllowedMediaTypes::DATA)
            .union(AllowedMediaTypes::SUBTITLE),
        default_repr = "video+audio+data+subtitle",
        flags(decoding)
    )]
    pub allowed_media_types: AllowedMediaTypes,

    #[opt(
        name = "min_port",
        help = "set minimum local UDP port",
        default = 5000,
        range = 0..=65535,
        flags(param)
    )]
    pub min_port: i32,

    #[opt(
        name = "max_port",
        help = "set maximum local UDP port",
        default = 65000,
        range = 0..=65535,
        flags(param)
    )]
    pub max_port: i32,

    /// Seconds; `-1` is infinite and implies `listen` mode (server mode —
    /// this crate is a client only, so this option is accepted for
    /// interface parity and otherwise unused; see the crate docs).
    #[opt(
        name = "listen_timeout",
        help = "set maximum timeout (in seconds) to wait for incoming connections",
        default = -1,
        range = i32::MIN..=i32::MAX,
        flags(decoding)
    )]
    pub listen_timeout: i32,

    /// Microseconds; `0` means no timeout.
    #[opt(
        name = "timeout",
        help = "set timeout (in microseconds) of socket I/O operations",
        default = 0_i64,
        flags(decoding)
    )]
    pub timeout: i64,

    #[opt(
        name = "reorder_queue_size",
        help = "set number of packets to buffer for handling of reordered packets",
        default = -1,
        range = -1..=i32::MAX,
        flags(decoding)
    )]
    pub reorder_queue_size: i32,

    #[opt(
        name = "buffer_size",
        help = "Underlying protocol send/receive buffer size",
        default = -1,
        range = -1..=i32::MAX,
        flags(param)
    )]
    pub buffer_size: i32,

    /// The reference's own default is its build string (`Lavf62.12.100`),
    /// which this project has no equivalent version stamp for and would be
    /// misleading to copy verbatim (D9 covers *names*, not a borrowed
    /// version identity) — `vaco`'s own `CARGO_PKG_VERSION` is used instead.
    #[opt(
        name = "user_agent",
        help = "override User-Agent header",
        default = concat!("vaco/", env!("CARGO_PKG_VERSION")).to_owned(),
        default_repr = "vaco/0.1.0",
        flags(decoding)
    )]
    pub user_agent: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_reference() {
        let o = RtspOptions::default();
        assert!(!o.initial_pause);
        assert_eq!(o.rtsp_transport, RtspTransportFlags::empty());
        assert_eq!(o.rtsp_flags, RtspFlags::empty());
        assert_eq!(
            o.allowed_media_types,
            AllowedMediaTypes::VIDEO
                .union(AllowedMediaTypes::AUDIO)
                .union(AllowedMediaTypes::DATA)
                .union(AllowedMediaTypes::SUBTITLE)
        );
        assert_eq!(o.min_port, 5000);
        assert_eq!(o.max_port, 65000);
        assert_eq!(o.listen_timeout, -1);
        assert_eq!(o.timeout, 0);
        assert_eq!(o.reorder_queue_size, -1);
        assert_eq!(o.buffer_size, -1);
    }
}
