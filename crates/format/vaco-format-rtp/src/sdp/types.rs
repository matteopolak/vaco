//! The RFC 4566 object model: what [`super::parse::parse`] builds.

/// A parsed SDP session description (RFC 4566 §5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionDescription {
    /// `o=` — username, session id, session version, network type, address
    /// type, unicast address. Kept as the five fields after username, since
    /// `vaco-demux-rtsp` never needs to *change* an origin, only read it.
    pub origin: Option<Origin>,
    /// `s=` (mandatory in the RFC; `Some("")` if the line was present but
    /// empty, `None` if the whole line was missing — a fair number of real
    /// RTSP servers omit it).
    pub session_name: Option<String>,
    /// `i=` at the session level.
    pub information: Option<String>,
    /// `c=` at the session level — inherited by any `m=` block that has no
    /// `c=` of its own (RFC 4566 §5.7).
    pub connection: Option<Connection>,
    /// `b=` lines at the session level, `(bwtype, value)`.
    pub bandwidth: Vec<(String, u64)>,
    /// `a=` lines at the session level.
    pub attributes: Vec<Attribute>,
    pub media: Vec<MediaDescription>,
}

/// `o=<username> <sess-id> <sess-version> <nettype> <addrtype> <unicast-address>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub username: String,
    pub session_id: String,
    pub session_version: String,
    pub net_type: String,
    pub addr_type: String,
    pub address: String,
}

/// `c=<nettype> <addrtype> <connection-address>`, with the RFC 4566 §5.7
/// `/<ttl>[/<count>]` multicast suffix split out, since a demuxer opening
/// the socket needs the address on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub net_type: String,
    pub addr_type: String,
    pub address: String,
    /// TTL for a multicast address (IPv4 only — RFC 4566 §5.7).
    pub ttl: Option<u8>,
    /// Number of addresses in a multicast block, when the address itself
    /// carries a `/<count>` suffix.
    pub count: Option<u32>,
}

/// One `a=` line: `a=<attribute>` (a flag) or `a=<attribute>:<value>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    pub name: String,
    pub value: Option<String>,
}

impl Attribute {
    #[must_use]
    pub fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }
}

/// One `m=` block and everything nested under it up to the next `m=` or EOF.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaDescription {
    /// `video`, `audio`, `text`, `application`, `data` (RFC 4566 §5.14) —
    /// kept as a string since callers key on RTSP's own vocabulary, not a
    /// closed set this crate would have to keep in sync with the RFC.
    pub media: String,
    pub port: u16,
    /// Number of ports in the block, for `<port>/<number of ports>` (RFC
    /// 4566 §5.14) — layered RTP/RTCP pairs on consecutive ports.
    pub port_count: Option<u16>,
    /// `RTP/AVP`, `RTP/SAVP`, `RTP/AVPF`, ... — the third `m=` field.
    pub proto: String,
    /// The fourth-and-later `m=` fields: for `RTP/AVP`, payload-type
    /// numbers, in declaration order.
    pub formats: Vec<String>,
    pub information: Option<String>,
    pub connection: Option<Connection>,
    pub bandwidth: Vec<(String, u64)>,
    pub attributes: Vec<Attribute>,
}

impl MediaDescription {
    /// The value of the first `a=<name>:<value>` attribute, matched
    /// case-insensitively on the name (RFC 4566 attribute names are
    /// case-sensitive by the letter of the RFC, but every RTSP server this
    /// crate has been checked against sends `rtpmap`/`RTPMAP` inconsistently
    /// enough that exact-case matching is not worth the fragility).
    #[must_use]
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| a.is(name))
            .and_then(|a| a.value.as_deref())
    }

    /// Every `a=<name>:<value>` attribute with this name, in order — used
    /// for `a=rtpmap`/`a=fmtp`, which repeat once per payload type.
    pub fn attrs<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.attributes
            .iter()
            .filter(move |a| a.is(name))
            .filter_map(|a| a.value.as_deref())
    }

    /// Whether a bare (valueless) attribute flag is present, e.g. `a=recvonly`.
    #[must_use]
    pub fn has_flag(&self, name: &str) -> bool {
        self.attributes
            .iter()
            .any(|a| a.is(name) && a.value.is_none())
    }

    /// The `a=control` URL for this media block (RFC 2326 §C.1.1 / RFC 7826
    /// §18.41), which is what `SETUP` addresses — relative to the session's
    /// own `a=control` (or the `DESCRIBE` request URL) when it does not look
    /// like an absolute URL itself.
    #[must_use]
    pub fn control(&self) -> Option<&str> {
        self.attr("control")
    }
}

/// One `a=rtpmap:<pt> <encoding-name>/<clock-rate>[/<channels>]` line,
/// parsed out of [`MediaDescription::attrs`]`("rtpmap")`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpMap {
    pub payload_type: u8,
    pub encoding_name: String,
    pub clock_rate: u32,
    pub channels: Option<u16>,
}

/// Parse one `a=rtpmap` value (the part after the colon).
#[must_use]
pub fn parse_rtpmap(value: &str) -> Option<RtpMap> {
    let (pt_str, rest) = value.trim().split_once(char::is_whitespace)?;
    let payload_type: u8 = pt_str.trim().parse().ok()?;
    let mut parts = rest.trim().split('/');
    let encoding_name = parts.next()?.to_owned();
    let clock_rate: u32 = parts.next()?.parse().ok()?;
    let channels = parts.next().and_then(|c| c.parse().ok());
    Some(RtpMap {
        payload_type,
        encoding_name,
        clock_rate,
        channels,
    })
}

/// Parse one `a=fmtp:<pt> <parameters>` value into `(payload_type, params)`.
#[must_use]
pub fn parse_fmtp(value: &str) -> Option<(u8, &str)> {
    let (pt_str, rest) = value.trim().split_once(char::is_whitespace)?;
    let payload_type: u8 = pt_str.trim().parse().ok()?;
    Some((payload_type, rest.trim()))
}

/// Split a `fmtp` parameter string (`key=value;key=value`, RFC 4566/RFC
/// 6184 §8.1) into `(key, value)` pairs, trimming whitespace around both.
/// Tolerates a bare flag with no `=`, reporting it as `(flag, "")`.
pub fn fmtp_params(params: &str) -> impl Iterator<Item = (&str, &str)> {
    params.split(';').filter_map(|kv| {
        let kv = kv.trim();
        if kv.is_empty() {
            return None;
        }
        Some(match kv.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (kv, ""),
        })
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn parses_rtpmap_with_channels() {
        let m = parse_rtpmap("97 L16/44100/2").unwrap();
        assert_eq!(m.payload_type, 97);
        assert_eq!(m.encoding_name, "L16");
        assert_eq!(m.clock_rate, 44100);
        assert_eq!(m.channels, Some(2));
    }

    #[test]
    fn parses_rtpmap_without_channels() {
        let m = parse_rtpmap("96 H264/90000").unwrap();
        assert_eq!(m.channels, None);
    }

    #[test]
    fn parses_fmtp_params() {
        let (pt, params) = parse_fmtp("96 packetization-mode=1;profile-level-id=42e01f").unwrap();
        assert_eq!(pt, 96);
        let map: Vec<_> = fmtp_params(params).collect();
        assert_eq!(
            map,
            vec![("packetization-mode", "1"), ("profile-level-id", "42e01f")]
        );
    }
}
