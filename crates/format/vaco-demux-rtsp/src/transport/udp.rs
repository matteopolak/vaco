//! Opening the RTP/RTCP UDP socket pair a `SETUP` negotiated.
//!
//! **This is the module the crate's security posture rests on.** Every
//! socket here is opened by calling the already-registered `udp:`
//! [`vaco_protocol_core::Protocol`] through [`vaco_protocol_core::ProtocolEnv::check_scheme`]
//! — the exact call [`vaco_protocol_core::ProtocolRegistry::resolve`] makes
//! for any other nested open — so `-protocol_whitelist` not naming `udp`
//! refuses a `SETUP` before any socket is touched, exactly like a nested
//! `tcp:` open under `-protocol_whitelist tls` (`docs/io/vaco-protocol-tls.md`).
//! There is no path in this module that opens a `udp:` socket by
//! constructing a transport directly — doing so would be exactly the "skip
//! the gate" mistake `vaco-protocol-core`'s own docs warn a demuxer crate
//! could make.
//!
//! The **local** ports this crate binds to (to receive RTP/RTCP on) are
//! always chosen by [`bind_local_pair`] from `-min_port`/`-max_port` — a
//! server cannot make this crate bind a port of the server's choosing. The
//! **remote** address this crate later sends receiver reports to (unicast:
//! the server's own `server_port=`; multicast: the group the server named
//! in `destination=`) *is* server-chosen, which is the RTSP negotiation
//! working as specified (RFC 2326 §C.1.1) — the control that matters is
//! that this can only ever be a `udp:` open, gated the same way as any
//! other.

use vaco_io::{MediaSink, MediaSource};
use vaco_opts::Dict;
use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolRegistry};

/// The receive side of one negotiated UDP transport: an RTP socket and an
/// RTCP socket, plus the local ports actually bound (which may differ from
/// the first ones tried, if they were in use).
pub struct UdpReceivePair {
    pub rtp: Box<dyn MediaSource>,
    pub rtcp: Box<dyn MediaSource>,
    pub local_rtp_port: u16,
    pub local_rtcp_port: u16,
}

// Manual `Debug`: `Box<dyn MediaSource>` does not implement it.
impl std::fmt::Debug for UdpReceivePair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UdpReceivePair")
            .field("local_rtp_port", &self.local_rtp_port)
            .field("local_rtcp_port", &self.local_rtcp_port)
            .finish_non_exhaustive()
    }
}

fn open_source(
    registry: &ProtocolRegistry,
    env: &ProtocolEnv<'_>,
    host: &str,
    port: u16,
) -> vaco_protocol_core::Result<Box<dyn MediaSource>> {
    let url = format!("udp://{host}:{port}");
    registry.open(&url, IoFlags::READ, &Dict::new(), env)
}

/// Bind a local RTP/RTCP port pair for **unicast** receive, trying each
/// even port in `min_port..=max_port` (RTCP is that port plus one, RFC 3550
/// §11's even/odd convention) until one is free.
///
/// # Errors
/// Whatever the last bind attempt failed with, if every port in range is in
/// use or the range is empty.
pub fn bind_local_pair(
    registry: &ProtocolRegistry,
    env: &ProtocolEnv<'_>,
    min_port: u16,
    max_port: u16,
) -> vaco_protocol_core::Result<UdpReceivePair> {
    let mut last_err = None;
    let mut port = min_port;
    while port < max_port {
        match (
            open_source(registry, env, "0.0.0.0", port),
            open_source(registry, env, "0.0.0.0", port.saturating_add(1)),
        ) {
            (Ok(rtp), Ok(rtcp)) => {
                return Ok(UdpReceivePair {
                    rtp,
                    rtcp,
                    local_rtp_port: port,
                    local_rtcp_port: port.saturating_add(1),
                });
            }
            (Err(e), _) | (_, Err(e)) => last_err = Some(e),
        }
        port = port.saturating_add(2);
    }
    Err(
        last_err.unwrap_or(vaco_protocol_core::ProtocolError::Malformed {
            scheme: "udp",
            detail: "no local port in the configured range was available",
        }),
    )
}

/// Join a **multicast** group named by the server's `SETUP` response
/// (`destination=`/`port=`) for receiving — RFC 2326 §C.1.1: the server
/// names the group, not this crate.
///
/// # Errors
/// Whatever the underlying `udp:` open reports (including
/// [`vaco_protocol_core::ProtocolError::Denied`] when `udp` is not on the
/// caller's whitelist).
pub fn join_multicast(
    registry: &ProtocolRegistry,
    env: &ProtocolEnv<'_>,
    group: &str,
    rtp_port: u16,
    rtcp_port: u16,
) -> vaco_protocol_core::Result<UdpReceivePair> {
    let rtp = open_source(registry, env, group, rtp_port)?;
    let rtcp = open_source(registry, env, group, rtcp_port)?;
    Ok(UdpReceivePair {
        rtp,
        rtcp,
        local_rtp_port: rtp_port,
        local_rtcp_port: rtcp_port,
    })
}

/// Open a `MediaSink` for sending RTCP receiver reports to the server's
/// `server_port=` (unicast) — a write-side `udp:` open, connected to the
/// server-chosen address, still through the same whitelist gate.
///
/// # Errors
/// Whatever the underlying `udp:` open reports.
pub fn open_rtcp_sink(
    registry: &ProtocolRegistry,
    env: &ProtocolEnv<'_>,
    remote_host: &str,
    remote_rtcp_port: u16,
) -> vaco_protocol_core::Result<Box<dyn MediaSink>> {
    let url = format!("udp://{remote_host}:{remote_rtcp_port}");
    registry.create(&url, IoFlags::WRITE, &Dict::new(), env)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::CancelToken;

    fn registry_with_udp() -> ProtocolRegistry {
        let mut r = ProtocolRegistry::new();
        vaco_protocol_socket::register(&mut r);
        r
    }

    #[test]
    fn binds_a_local_pair_when_udp_is_whitelisted() {
        let registry = registry_with_udp();
        let cancel = CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["udp"]);
        let pair = bind_local_pair(&registry, &env, 40000, 40100).unwrap();
        assert!(pair.local_rtp_port >= 40000);
        assert_eq!(pair.local_rtcp_port, pair.local_rtp_port + 1);
    }

    #[test]
    fn refuses_when_udp_is_not_whitelisted() {
        let registry = registry_with_udp();
        let cancel = CancelToken::new();
        let env = ProtocolEnv::new(&registry, &cancel).with_whitelist(&["tcp"]);
        let err = bind_local_pair(&registry, &env, 40100, 40200).unwrap_err();
        assert!(matches!(
            err,
            vaco_protocol_core::ProtocolError::Denied { .. }
        ));
    }
}
