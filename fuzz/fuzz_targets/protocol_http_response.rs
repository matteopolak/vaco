//! The response-parsing surface: everything in `vaco-protocol-http` that
//! takes bytes a remote server controls, driven with `arbitrary` rather than
//! raw bytes so the fuzzer explores realistic header-value-shaped and
//! `Location`-shaped strings without spending its whole budget on invalid
//! UTF-8 that `HeaderValue::to_str()` would already have rejected before any
//! of this code runs.
//!
//! Fuzzes the parts that take server bytes, not the socket (per the crate's
//! brief): `Content-Range`, `Retry-After`, the `-reconnect_on_http_error`
//! list, cookie/header-block parsing, `Location` resolution (RFC 3986 §5,
//! including dot-segment removal) and request-target construction. None of
//! this touches a `TcpStream` or `ureq::Agent`.
//!
//! A finding is a panic or a hang — this workspace is `forbid(unsafe_code)`,
//! so there is no memory corruption to find (plan 13 §2).
//! fuzz-crate: vaco-protocol-http

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_protocol_core::split_url;
use vaco_protocol_http::parse::{
    cookie_header, parse_content_range, parse_header_block, parse_reconnect_codes,
    parse_retry_after_secs,
};
use vaco_protocol_http::url::{remove_dot_segments, request_target, resolve_location, split_userinfo};

#[derive(Arbitrary, Debug)]
struct Input {
    content_range: String,
    retry_after: String,
    reconnect_codes: String,
    cookies: String,
    header_block: String,
    base: String,
    location: String,
    userinfo_target: String,
    dot_segments: String,
}

fuzz_target!(|input: Input| {
    let _ = parse_content_range(&input.content_range);
    let _ = parse_retry_after_secs(&input.retry_after);
    let _ = parse_reconnect_codes(&input.reconnect_codes);
    let _ = cookie_header(&input.cookies);
    let _ = parse_header_block(&input.header_block);

    // `resolve_location` is the security-relevant one: a `Location` header is
    // chosen by the server, and its output feeds directly into
    // `ProtocolEnv::check_scheme` (see `crate::protocol`). Idempotent on a
    // fixed point, per RFC 3986 §5.2.4 — if it already resolved to something
    // absolute, resolving it again against itself must not change it.
    let resolved = resolve_location(&input.base, &input.location);
    let parsed = split_url(&resolved);
    if parsed.scheme.is_some() {
        let resolved_again = resolve_location(&resolved, &resolved);
        assert_eq!(
            resolved_again, resolved,
            "resolving an absolute URL against itself must be a fixed point: \
             base={:?} location={:?} resolved={resolved:?}",
            input.base, input.location
        );
    }

    let once = remove_dot_segments(&input.dot_segments);
    let twice = remove_dot_segments(&once);
    assert_eq!(
        once, twice,
        "remove_dot_segments must be idempotent for {:?}",
        input.dot_segments
    );

    let _ = request_target(&split_url(&input.base));
    let (creds, rest) = split_userinfo(&input.userinfo_target);
    if creds.is_none() {
        assert_eq!(
            rest, input.userinfo_target,
            "no userinfo found must mean the target is returned unchanged"
        );
    }
});
