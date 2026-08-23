//! Parsing `//host:port[?opt=val&...]` out of [`vaco_protocol_core::Url::rest`].
//!
//! Not RFC 3986 — `vaco-protocol-core`'s own module docs say the project's URL
//! space is not either — but a small, deliberately strict subset of it: enough
//! to recover a host, a port and an inline query-option block from
//! `tcp://host:port`, `udp://host:port?pkt_size=1024` and an IPv6 literal
//! (`tcp://[::1]:1234`). This is the surface a redirect, playlist entry or
//! `hls:`/`dash:` segment reference can influence, so every function here is
//! total: a malformed input is a `None`/`Err`, never a panic.
//!
//! `unix:` does not use this module — see [`crate::unix`] — because its
//! `rest` is a filesystem path, not a `host:port` pair, and a path may
//! legitimately contain `?` or `:`.

use vaco_opts::Dict;

/// A parsed `host:port` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPort {
    pub host: String,
    pub port: u16,
}

/// Split `rest` (everything after the scheme's `:`) into a [`HostPort`] and a
/// [`Dict`] of `?key=value&...` options.
///
/// Accepts a leading `//` (present in every URL this crate has seen: `-h
/// protocol=tcp`'s examples are all `tcp://host:port`) but does not require
/// it, so `tcp:host:port` — which the reference also accepts — still parses.
///
/// # Errors
/// `None` when no `host:port` can be recovered at all (no `:`, or a `:` with
/// nothing after it). A missing/unparseable port is reported as `port: 0` by
/// the caller's own validation, not here — this function only recovers what
/// is syntactically present.
#[must_use]
pub fn parse(rest: &str) -> Option<(HostPort, Dict)> {
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let (authority, query) = match rest.split_once('?') {
        Some((a, q)) => (a, Some(q)),
        None => (rest, None),
    };

    let (host, port_str) = split_host_port(authority)?;
    let port: u16 = port_str.parse().ok()?;

    let mut opts = Dict::new();
    if let Some(q) = query {
        parse_query(q, &mut opts);
    }
    Some((
        HostPort {
            host: host.to_owned(),
            port,
        },
        opts,
    ))
}

/// `[ipv6]:port` or `host:port`. The bracket form is required for an IPv6
/// literal precisely because it contains its own `:`s — without it, `split
/// on the last colon` would be ambiguous between "the port separator" and
/// "one more hextet".
fn split_host_port(authority: &str) -> Option<(&str, &str)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let port = after.strip_prefix(':')?;
        // `[]:port` (an empty bracketed host) is not a real address; reject
        // it here rather than only in the non-bracket branch below, so both
        // forms agree on what counts as "no host at all". Found by fuzzing
        // (`fuzz/fuzz_targets/socket_url_parse.rs`): the asymmetry meant
        // `parse` accepted `[]:17` while the crate's own round-trip
        // reconstruction of that same `HostPort` could not represent it.
        if host.is_empty() || port.is_empty() {
            return None;
        }
        return Some((host, port));
    }
    // Plain `host:port`: the *last* colon separates them, so a bracket-less
    // (and therefore ambiguous, but the reference does not require brackets
    // for a bare numeric port after a hostname) input still recovers the
    // port a caller almost certainly meant.
    let colon = authority.rfind(':')?;
    let host = authority.get(..colon)?;
    let port = authority.get(colon + 1..)?;
    if host.is_empty() || port.is_empty() {
        return None;
    }
    Some((host, port))
}

/// `a=1&b=2` into `opts`. Unpaired tokens (`&flag&`) are ignored rather than
/// stored with an empty value guessed at — silently inventing a value for an
/// input that named none would be worse than dropping it, and every option
/// this crate defines has an explicit default already.
fn parse_query(q: &str, opts: &mut Dict) {
    for pair in q.split('&') {
        if pair.is_empty() {
            continue;
        }
        if let Some((k, v)) = pair.split_once('=') {
            opts.set(k, v);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn plain_host_port() {
        let (hp, opts) = parse("//127.0.0.1:1234").unwrap();
        assert_eq!(hp.host, "127.0.0.1");
        assert_eq!(hp.port, 1234);
        assert!(opts.is_empty());
    }

    #[test]
    fn without_leading_slashes() {
        let (hp, _) = parse("example.com:80").unwrap();
        assert_eq!(hp.host, "example.com");
        assert_eq!(hp.port, 80);
    }

    #[test]
    fn ipv6_literal() {
        let (hp, _) = parse("//[::1]:5555").unwrap();
        assert_eq!(hp.host, "::1");
        assert_eq!(hp.port, 5555);
    }

    #[test]
    fn query_options() {
        let (hp, opts) = parse("//239.0.0.1:1234?pkt_size=188&ttl=5").unwrap();
        assert_eq!(hp.host, "239.0.0.1");
        assert_eq!(hp.port, 1234);
        assert_eq!(opts.get("pkt_size"), Some("188"));
        assert_eq!(opts.get("ttl"), Some("5"));
    }

    #[test]
    fn rejects_missing_port() {
        assert!(parse("//host-with-no-port").is_none());
        assert!(parse("//host:").is_none());
        assert!(parse("//:1234").is_none());
        assert!(parse("").is_none());
    }

    /// Found by fuzzing: an empty bracketed host (`[]:port`) was accepted by
    /// the bracket branch of `split_host_port` even though the exact same
    /// "no host" shape is rejected in the non-bracket branch — an asymmetry
    /// that broke the fuzz target's own round-trip invariant, because the
    /// resulting `HostPort { host: "", .. }` could not be reconstructed back
    /// into a parseable URL.
    #[test]
    fn rejects_an_empty_bracketed_host() {
        assert!(parse("//[]:17").is_none());
        assert!(parse("[]:17").is_none());
    }

    #[test]
    fn rejects_non_numeric_port() {
        assert!(parse("//host:notaport").is_none());
    }

    #[test]
    fn unpaired_query_tokens_are_dropped_not_guessed() {
        let (_, opts) = parse("//h:1?flag&a=1").unwrap();
        assert_eq!(opts.get("a"), Some("1"));
        assert_eq!(opts.get("flag"), None);
    }

    #[test]
    fn never_panics_on_arbitrary_bytes() {
        // A cheap substitute for the fuzz target's coverage, run on every
        // `cargo test`: a grab-bag of the shapes most likely to confuse a
        // hand-rolled splitter.
        for s in [
            "?", "//", "[", "]", ":::", "//[:", "//]:1", "//[::]:", "a", "\u{0}", "//[",
        ] {
            let _ = parse(s);
        }
    }
}
