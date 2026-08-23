//! RFC 2326 §12.39 / RFC 7826 §18.54: the `Transport:` header, and the four
//! modes it can name.
//!
//! # The four modes
//!
//! | Mode | Wire shape | Socket(s) this crate opens |
//! |---|---|---|
//! | UDP unicast | `RTP/AVP;unicast;client_port=<a>-<b>` | Two `udp:` sockets, locally bound to a port pair this crate chose from `-min_port`/`-max_port` |
//! | TCP interleaved | `RTP/AVP/TCP;unicast;interleaved=<a>-<b>` | None — reuses the open RTSP control connection, `$`-framed (RFC 2326 §10.12) |
//! | UDP multicast | `RTP/AVP;multicast;destination=<addr>;port=<a>-<b>;ttl=<n>` | Two `udp:` sockets joining the server-named multicast group |
//! | HTTP tunnelling | Same `RTP/AVP/TCP;unicast;interleaved=` shape, carried inside [`crate::http_tunnel`]'s two HTTP legs instead of a bare TCP socket | None of its own — see [`crate::http_tunnel`] |
//!
//! See the crate's top-level docs for exactly what a server is allowed to
//! name in each case and why that is safe.
//!
//! [`RtspOptions::rtsp_transport`](crate::options::RtspOptions::rtsp_transport)
//! being empty by default (§ this module's parent) means *this crate*
//! decides the offer order when a caller does not name one:
//! `udp`, `tcp`, `udp_multicast`, in that order — UDP unicast first because
//! it is what every RTSP server this crate was checked against prefers,
//! TCP interleaved as the NAT/firewall-friendly fallback, multicast last
//! because a client cannot request it without already knowing the stream is
//! multicast-capable. HTTP tunnelling is never offered automatically — a
//! caller must ask for it explicitly, since it means opening two
//! connections instead of one for no benefit on a network where UDP or
//! plain TCP already works.

pub mod udp;

use vaco_core::{Error, Result};

/// Which of the four modes a negotiated (or offered) transport uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportMode {
    UdpUnicast,
    UdpMulticast,
    TcpInterleaved,
    Http,
}

/// One parsed (or to-be-built) `Transport:` header value. Fields are
/// `Option` because a `SETUP` response only ever fills in the ones its
/// negotiated mode actually uses (RFC 2326 §12.39's grammar is a single
/// production with every parameter optional).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransportSpec {
    /// `RTP/AVP`, `RTP/AVP/TCP`, `RTP/AVP/UDP`, ... — kept verbatim rather
    /// than parsed into parts, since this crate always emits and expects
    /// exactly `RTP/AVP` or `RTP/AVP/TCP`.
    pub profile: String,
    pub multicast: bool,
    pub client_port: Option<(u16, u16)>,
    pub server_port: Option<(u16, u16)>,
    pub interleaved: Option<(u8, u8)>,
    pub destination: Option<String>,
    pub source: Option<String>,
    pub ttl: Option<u8>,
    pub ssrc: Option<u32>,
    /// A parameter this module does not model as a typed field
    /// (`mode=PLAY`, `layers=`, ...), preserved verbatim in case a caller
    /// needs it.
    pub other: Vec<(String, Option<String>)>,
}

impl TransportSpec {
    #[must_use]
    pub fn mode(&self) -> TransportMode {
        if self.multicast {
            TransportMode::UdpMulticast
        } else if self.profile.ends_with("/TCP") || self.interleaved.is_some() {
            TransportMode::TcpInterleaved
        } else {
            TransportMode::UdpUnicast
        }
    }

    /// Build the client's offer for `SETUP`, per [`TransportMode`].
    #[must_use]
    pub fn offer(mode: TransportMode, client_ports: (u16, u16), channels: (u8, u8)) -> Self {
        match mode {
            TransportMode::UdpUnicast => Self {
                profile: "RTP/AVP".to_owned(),
                client_port: Some(client_ports),
                ..Self::default()
            },
            TransportMode::UdpMulticast => Self {
                profile: "RTP/AVP".to_owned(),
                multicast: true,
                ..Self::default()
            },
            TransportMode::TcpInterleaved | TransportMode::Http => Self {
                profile: "RTP/AVP/TCP".to_owned(),
                interleaved: Some(channels),
                ..Self::default()
            },
        }
    }

    #[must_use]
    pub fn to_header_value(&self) -> String {
        let mut parts = vec![self.profile.clone()];
        parts.push(if self.multicast {
            "multicast".to_owned()
        } else {
            "unicast".to_owned()
        });
        if let Some((a, b)) = self.client_port {
            parts.push(format!("client_port={a}-{b}"));
        }
        if let Some((a, b)) = self.server_port {
            parts.push(format!("server_port={a}-{b}"));
        }
        if let Some((a, b)) = self.interleaved {
            parts.push(format!("interleaved={a}-{b}"));
        }
        if let Some(dest) = &self.destination {
            parts.push(format!("destination={dest}"));
        }
        if let Some(src) = &self.source {
            parts.push(format!("source={src}"));
        }
        if let Some(ttl) = self.ttl {
            parts.push(format!("ttl={ttl}"));
        }
        if let Some(ssrc) = self.ssrc {
            parts.push(format!("ssrc={ssrc:08x}"));
        }
        for (k, v) in &self.other {
            match v {
                Some(v) => parts.push(format!("{k}={v}")),
                None => parts.push(k.clone()),
            }
        }
        parts.join(";")
    }
}

fn parse_port_range<T: std::str::FromStr>(v: &str) -> Option<(T, T)> {
    let (a, b) = v.split_once('-')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// Parse a full `Transport:` header value, which may name several
/// comma-separated alternatives (a server sometimes echoes more than one).
/// The caller (`crate::session`) picks the first one whose [`TransportSpec::mode`]
/// it actually asked for.
///
/// # Errors
/// [`Error::InvalidData`] if the value has no comma-separated specs at all
/// (an empty header) — individual malformed parameters within a spec are
/// simply skipped rather than failing the whole header, since a server
/// naming one parameter this crate does not recognise should not block
/// negotiation on the ones it does.
pub fn parse(value: &str) -> Result<Vec<TransportSpec>> {
    let specs: Vec<TransportSpec> = value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_one)
        .collect();
    if specs.is_empty() {
        return Err(Error::InvalidData(
            "Transport header names no transport spec",
        ));
    }
    Ok(specs)
}

fn parse_one(spec: &str) -> TransportSpec {
    let mut out = TransportSpec::default();
    for (i, field) in spec.split(';').enumerate() {
        let field = field.trim();
        if i == 0 {
            field.clone_into(&mut out.profile);
            continue;
        }
        let (key, value) = match field.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (field, None),
        };
        match (key, value) {
            ("multicast", None) => out.multicast = true,
            ("unicast", None) => out.multicast = false,
            ("client_port", Some(v)) => out.client_port = parse_port_range(v),
            ("server_port" | "port", Some(v)) => out.server_port = parse_port_range(v),
            ("interleaved", Some(v)) => out.interleaved = parse_port_range(v),
            ("destination", Some(v)) => out.destination = Some(v.to_owned()),
            ("source", Some(v)) => out.source = Some(v.to_owned()),
            ("ttl", Some(v)) => out.ttl = v.parse().ok(),
            ("ssrc", Some(v)) => out.ssrc = u32::from_str_radix(v, 16).ok(),
            (k, v) => out.other.push((k.to_owned(), v.map(str::to_owned))),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn parses_udp_unicast() {
        let specs = parse("RTP/AVP;unicast;client_port=4588-4589;server_port=6256-6257").unwrap();
        assert_eq!(specs.len(), 1);
        let s = &specs[0];
        assert_eq!(s.mode(), TransportMode::UdpUnicast);
        assert_eq!(s.client_port, Some((4588, 4589)));
        assert_eq!(s.server_port, Some((6256, 6257)));
    }

    #[test]
    fn parses_tcp_interleaved() {
        let specs = parse("RTP/AVP/TCP;unicast;interleaved=0-1").unwrap();
        assert_eq!(specs[0].mode(), TransportMode::TcpInterleaved);
        assert_eq!(specs[0].interleaved, Some((0, 1)));
    }

    #[test]
    fn parses_multicast_with_ttl() {
        let specs = parse("RTP/AVP;multicast;destination=239.1.1.1;port=3456-3457;ttl=16").unwrap();
        let s = &specs[0];
        assert_eq!(s.mode(), TransportMode::UdpMulticast);
        assert_eq!(s.destination.as_deref(), Some("239.1.1.1"));
        assert_eq!(s.ttl, Some(16));
    }

    #[test]
    fn round_trips_an_offer() {
        let offer = TransportSpec::offer(TransportMode::UdpUnicast, (5000, 5001), (0, 1));
        let value = offer.to_header_value();
        let parsed = parse(&value).unwrap();
        assert_eq!(parsed[0].client_port, Some((5000, 5001)));
    }

    #[test]
    fn rejects_empty_header() {
        assert!(parse("").is_err());
    }

    proptest::proptest! {
        #[test]
        fn parse_never_panics(s in ".{0,300}") {
            let _ = parse(&s);
        }
    }
}
