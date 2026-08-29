//! RFC 8839 §5.1 `a=candidate` line parsing — just enough to pick a UDP
//! host/server-reflexive candidate to run an ICE connectivity check
//! against.
//!
//! `candidate:<foundation> <component-id> <transport> <priority>
//! <connection-address> <port> typ <cand-type> [rel-addr <addr>] [rel-port
//! <port>] *(<extension-name> <extension-value>)`. Only the fields a
//! connectivity check needs are kept.

use vaco_core::{Error, Result};

/// One usable candidate: a UDP address worth an ICE connectivity check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub component: u32,
    pub priority: u32,
    pub address: String,
    pub port: u16,
    /// `host`, `srflx`, `prflx`, `relay` (RFC 8839 §5.1.1).
    pub typ: String,
}

/// Parse the value of one `a=candidate` attribute (the part after the
/// colon, RFC 8839 §5.1's ABNF).
///
/// Relay (TURN) candidates are parsed like any other — this crate does not
/// implement TURN, so a caller filters them out itself via
/// [`Candidate::typ`], the same way it would filter by transport.
///
/// # Errors
/// [`Error::InvalidData`] if the line has fewer fields than the grammar
/// requires, or `component-id`/`priority`/`port` do not parse as integers —
/// every field here comes from the network (the WHIP answer).
pub fn parse(line: &str) -> Result<Candidate> {
    let bad = || Error::InvalidData("malformed a=candidate line");
    let mut fields = line.split_whitespace();
    let _foundation = fields.next().ok_or_else(bad)?;
    let component: u32 = fields.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
    let transport = fields.next().ok_or_else(bad)?.to_owned();
    let priority: u32 = fields.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
    let address = fields.next().ok_or_else(bad)?.to_owned();
    let port: u16 = fields.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
    // `typ <cand-type>` follows; skip anything else (rel-addr/rel-port,
    // ICE extensions like `ufrag`/`network-cost`) — none of them change
    // which address we dial.
    let mut typ = String::new();
    while let Some(tok) = fields.next() {
        if tok.eq_ignore_ascii_case("typ") {
            fields.next().unwrap_or_default().clone_into(&mut typ);
            break;
        }
    }
    if typ.is_empty() {
        return Err(bad());
    }
    // Only UDP candidates are usable at all: this crate's DTLS/SRTP path is
    // UDP-only, matching every WHIP peer measured so far.
    if !transport.eq_ignore_ascii_case("udp") {
        return Err(Error::Unsupported(
            "only UDP ICE candidates are supported",
        ));
    }
    Ok(Candidate {
        component,
        priority,
        address,
        port,
        typ,
    })
}

/// Parse every `a=candidate` line among `attrs`' values, skipping (not
/// erroring on) a line this crate cannot use — a `tcp` candidate or a
/// malformed line does not sink the whole answer when at least one usable
/// candidate exists.
#[must_use]
pub fn usable_candidates<'a>(values: impl Iterator<Item = &'a str>) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = values.filter_map(|v| parse(v).ok()).collect();
    // Component 1 (RTP, or RTP+RTCP under rtcp-mux) only — never dial a
    // component-2 (RTCP) address as if it carried media.
    out.retain(|c| c.component == 1);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_host_candidate() {
        let c = parse("2878742611 1 udp 2130706431 127.0.0.1 8189 typ host ufrag abcd").unwrap();
        assert_eq!(c.component, 1);
        assert_eq!(c.priority, 2_130_706_431);
        assert_eq!(c.address, "127.0.0.1");
        assert_eq!(c.port, 8189);
        assert_eq!(c.typ, "host");
    }

    #[test]
    fn rejects_tcp() {
        assert!(parse("1 1 tcp 1 127.0.0.1 9 typ host").is_err());
    }

    #[test]
    fn rejects_truncated_lines() {
        assert!(parse("").is_err());
        assert!(parse("1 1 udp 1 127.0.0.1").is_err());
        for n in 0..8 {
            let line = "2878742611 1 udp 2130706431 127.0.0.1 8189 typ host"
                .split_whitespace()
                .take(n)
                .collect::<Vec<_>>()
                .join(" ");
            let _ = parse(&line);
        }
    }

    #[test]
    fn usable_candidates_filters_component_and_bad_lines() {
        let lines = [
            "1 1 udp 2130706431 127.0.0.1 8189 typ host",
            "1 2 udp 2130706430 127.0.0.1 8190 typ host",
            "not a candidate at all",
            "1 1 tcp 1 10.0.0.1 9 typ host",
        ];
        let out = usable_candidates(lines.into_iter());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].address, "127.0.0.1");
    }
}
