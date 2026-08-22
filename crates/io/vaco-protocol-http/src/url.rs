//! Request-target construction, and resolving a `Location` header.
//!
//! Portable: string manipulation only, no I/O, no `ureq` types. This is the
//! part a `fetch`-based sibling would reuse unchanged (the browser's own
//! `fetch` resolves relative `Location` values itself, but a sibling crate
//! that wanted to inspect or re-dispatch a redirect through the same
//! whitelist gate this crate uses would need exactly this logic).
//!
//! # Why a `Location` is resolved here and not just handed to the transport
//!
//! `ureq` can follow redirects itself, but doing that would let a remote
//! server choose a URL that never passes back through
//! [`vaco_protocol_core::ProtocolEnv`] — which is precisely the whitelist
//! bypass rule W3 exists to prevent (see `vaco-protocol-core`'s crate docs).
//! So this crate disables `ureq`'s redirect following entirely
//! (`max_redirects(0)`, see `crate::transport`) and, on a 3xx response,
//! resolves the `Location` value into a full URL string *itself*, then hands
//! that string to [`vaco_protocol_core::ProtocolRegistry::open`] — the same
//! function every top-level open goes through — so a redirect to `file:` is
//! refused by the exact mechanism that refuses it anywhere else.

use vaco_protocol_core::{Url, split_url};

/// Reconstruct the absolute request-target string for `url`, as `HttpProtocol`
/// was asked to open it.
///
/// `url.scheme` is lower-cased on the way out: a scheme is case-insensitive
/// per RFC 3986 §3.1, but `Url` preserves whatever casing the caller typed, and
/// normalising here means the transport layer never has to think about it.
///
/// # Errors
/// [`TargetError::Nested`] if `url` names a `+`-joined nested scheme
/// (`http+something:`), which `http:`/`https:` do not define — that spelling
/// only makes sense when `http`/`https` is the *inner* half of someone else's
/// pair (`crypto+https:`), never the outer half.
pub fn request_target(url: &Url) -> Result<String, TargetError> {
    if url.nested.is_some() {
        return Err(TargetError::Nested);
    }
    let scheme = url
        .scheme
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    // Matches `Url`'s own `Display` order: scheme, args, `:`, rest.
    Ok(format!(
        "{scheme}{args}:{rest}",
        args = url.args,
        rest = url.rest
    ))
}

/// Split `user[:pass]@` userinfo off the authority of an absolute
/// `scheme://[user[:pass]@]host[:port]/path` target.
///
/// Returns `(Some((user, pass)), target_without_userinfo)` when userinfo is
/// present, `(None, target)` (a plain clone) otherwise. This runs only on
/// URLs the caller itself typed or read from its own playlist — never on a
/// `Location` header — so it does not need to be hardened against a hostile
/// author the way [`resolve_location`] does; it still never panics on
/// malformed input, simply by not finding an authority to split.
#[must_use]
pub fn split_userinfo(target: &str) -> (Option<(String, String)>, String) {
    let Some((scheme, rest)) = target.split_once("://") else {
        return (None, target.to_owned());
    };
    // The authority ends at the first '/', '?' or '#', whichever is first;
    // an '@' after that point belongs to the path/query, not to userinfo.
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let Some(authority) = rest.get(..authority_end) else {
        return (None, target.to_owned());
    };
    let Some(at) = authority.rfind('@') else {
        return (None, target.to_owned());
    };
    let Some(userinfo) = authority.get(..at) else {
        return (None, target.to_owned());
    };
    let Some(host_and_rest) = rest.get(at + 1..) else {
        return (None, target.to_owned());
    };
    let (user, pass) = match userinfo.split_once(':') {
        Some((u, p)) => (u.to_owned(), p.to_owned()),
        None => (userinfo.to_owned(), String::new()),
    };
    (Some((user, pass)), format!("{scheme}://{host_and_rest}"))
}

/// Why [`request_target`] refused a URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetError {
    /// `url.nested` was set.
    Nested,
}

/// Resolve a `Location` header value against the URL of the request that
/// produced it, returning a full URL string ready for
/// [`vaco_protocol_core::ProtocolRegistry::open`].
///
/// Handles, in order:
/// 1. **Absolute** (`https://host/path`, or any other `scheme:` spelling
///    [`vaco_protocol_core::split_url`] recognises, including a nested
///    `outer+inner:` pair): used verbatim. A redirect to a wholly different
///    scheme — `file:`, `ftp:`, anything — reaches this branch, which is
///    exactly why the caller must still push the result through the
///    whitelist rather than trusting "it came from `resolve_location`".
/// 2. **Protocol-relative** (`//host/path`): `base`'s scheme is prepended.
/// 3. **Absolute-path** (`/path`): `base`'s scheme and authority are kept, the
///    path is replaced.
/// 4. **Relative-path** (`path`, `../path`, `./path`, empty): merged with
///    `base`'s path per RFC 3986 §5.3's merge step, then dot-segments are
///    removed per §5.2.4.
///
/// `base` must be an absolute `http:`/`https:` target, e.g. the output of
/// [`request_target`]. Never panics; a `base` that is not actually absolute
/// falls back to treating the merge as against an empty path, which is safe
/// (it cannot happen in this crate's own call sites, which always resolve a
/// redirect against a URL this same module just built).
#[must_use]
pub fn resolve_location(base: &str, location: &str) -> String {
    let probe = split_url(location);
    if probe.scheme.is_some() {
        return location.to_owned();
    }
    // Rule S1 of `split_url` classifies `//host/path`, `/path` and a bare
    // relative path all as "no scheme" — `probe.rest` is `location` itself in
    // every one of those cases, byte for byte (the round-trip invariant
    // `split_url(s).to_string() == s` guarantees it for the `scheme: None`
    // arm specifically, since `Display` writes `rest` alone there).
    let reference = probe.rest.as_str();

    let (base_scheme, base_authority, base_path) = split_authority(base);

    if let Some(after_slashes) = reference.strip_prefix("//") {
        return format!("{base_scheme}://{after_slashes}");
    }

    if let Some(abs_path) = reference.strip_prefix('/') {
        let merged = remove_dot_segments(&format!("/{abs_path}"));
        return format!("{base_scheme}://{base_authority}{merged}");
    }

    let merged_path = merge_relative(base_path, reference);
    let merged = remove_dot_segments(&merged_path);
    format!("{base_scheme}://{base_authority}{merged}")
}

/// Split `target` (`scheme://authority/path...`) into its three parts.
///
/// Total: a `target` with no `://` yields an empty authority and the whole
/// remainder as path, which is a safe (if useless) answer rather than a
/// panic — this only ever runs on strings this crate built itself, but it
/// takes no risk on that.
fn split_authority(target: &str) -> (&str, &str, &str) {
    let Some((scheme, rest)) = target.split_once("://") else {
        return (target, "", "");
    };
    let Some(slash) = rest.find('/') else {
        return (scheme, rest, "");
    };
    let Some((authority, path)) = rest.split_at_checked(slash) else {
        return (scheme, rest, "");
    };
    (scheme, authority, path)
}

/// RFC 3986 §5.3's "merge" step for a relative-reference against a base path.
///
/// Replaces everything after the last `/` in `base_path` with `reference`. An
/// empty `base_path` (an origin-form request like `http://host` with no
/// path at all) merges against `/`, matching the RFC's special case for a
/// base with an undefined authority-relative path.
fn merge_relative(base_path: &str, reference: &str) -> String {
    let dir_end = base_path.rfind('/').map_or(0, |i| i + 1);
    let dir = base_path.get(..dir_end).unwrap_or("");
    if dir.is_empty() {
        format!("/{reference}")
    } else {
        format!("{dir}{reference}")
    }
}

/// RFC 3986 §5.2.4: remove `.` and `..` segments from an absolute path.
///
/// Implemented as the RFC's own two-buffer algorithm (input remaining, output
/// built so far) rather than a `Vec<&str>` segment stack, because the
/// standard's edge cases — a bare `.` or `..` input, an output that must never
/// pop below the root — are exactly the cases that are easy to get wrong when
/// re-derived by hand, and the RFC's steps are already the specification (D7:
/// no clean-room concern, this is an IETF text, not `FFmpeg`'s).
///
/// Terminates in `O(input.len())` steps: every branch below strictly shortens
/// `input` (the pop branches shorten it via the prefix strip; the "move one
/// segment" branch strips at least one byte from `input` into `output`), so
/// there is no scenario, malicious or otherwise, in which this loops more
/// than `input.len()` times.
#[must_use]
pub fn remove_dot_segments(input: &str) -> String {
    // Working buffer for step B/C's "replace the prefix with a single `/`":
    // rather than literally splicing a `/` back onto a `&str` slice (which
    // would need an owned buffer anyway once it no longer matches the
    // original `input`), each step below computes the new remainder directly.
    let mut input = input.to_owned();
    let mut output = String::new();
    while !input.is_empty() {
        if let Some(rest) = input.strip_prefix("../") {
            input = rest.to_owned();
        } else if let Some(rest) = input.strip_prefix("./") {
            input = rest.to_owned();
        } else if let Some(rest) = input.strip_prefix("/./") {
            // Step B: "/./..." -> "/...".
            input = format!("/{rest}");
        } else if input == "/." {
            "/".clone_into(&mut input);
        } else if let Some(rest) = input.strip_prefix("/../") {
            // Step C: "/../..." -> "/...", and drop the output's last segment.
            pop_last_segment(&mut output);
            input = format!("/{rest}");
        } else if input == "/.." {
            pop_last_segment(&mut output);
            "/".clone_into(&mut input);
        } else if input == "." || input == ".." {
            input.clear();
        } else {
            // Step E: move the first segment — the leading '/' (if any) plus
            // everything up to but not including the next '/' — to output.
            let search_from = usize::from(input.starts_with('/'));
            let next_slash = input.get(search_from..).and_then(|t| t.find('/'));
            let split_at = next_slash.map_or(input.len(), |i| i + search_from);
            let Some((seg, rest)) = input.split_at_checked(split_at) else {
                // `split_at` is always on a byte we just found via `find` on
                // this same string, or `input.len()`, so this cannot actually
                // trigger — but a graceful "stop" is safer than a panic if a
                // future edit ever gets that invariant wrong.
                break;
            };
            output.push_str(seg);
            input = rest.to_owned();
        }
    }
    output
}

/// Drop everything in `output` after (and including) its last `/`.
///
/// If there is no `/`, `output` is cleared — this only happens when the
/// input never had a legitimate absolute-path shape to begin with, and
/// clearing is the safe direction (produces `/whatever-comes-next` rather
/// than a string missing its leading slash).
fn pop_last_segment(output: &mut String) {
    match output.rfind('/') {
        Some(i) => output.truncate(i),
        None => output.clear(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn dot_segments_rfc_example_one() {
        // RFC 3986 §5.2.4 worked example.
        assert_eq!(remove_dot_segments("/a/b/c/./../../g"), "/a/g");
    }

    #[test]
    fn dot_segments_rfc_example_two() {
        assert_eq!(remove_dot_segments("mid/content=5/../6"), "mid/6");
    }

    #[test]
    fn dot_segments_bare_forms() {
        assert_eq!(remove_dot_segments("."), "");
        assert_eq!(remove_dot_segments(".."), "");
        assert_eq!(remove_dot_segments("/."), "/");
        assert_eq!(remove_dot_segments("/.."), "/");
        assert_eq!(remove_dot_segments(""), "");
    }

    #[test]
    fn dot_segments_cannot_climb_above_root() {
        assert_eq!(remove_dot_segments("/../../../etc/passwd"), "/etc/passwd");
    }

    #[test]
    fn absolute_location_is_used_verbatim() {
        assert_eq!(
            resolve_location("http://a/b/c", "https://other/x"),
            "https://other/x"
        );
        // The security-relevant case: a redirect to a different scheme
        // resolves to that scheme's URL unchanged. It is the *caller's* job
        // (HttpProtocol::open, via ProtocolEnv) to refuse it — this function
        // only builds the string.
        assert_eq!(
            resolve_location("http://a/b/c", "file:///etc/passwd"),
            "file:///etc/passwd"
        );
    }

    #[test]
    fn protocol_relative_location_keeps_the_base_scheme() {
        assert_eq!(
            resolve_location("https://a/b/c", "//other/x"),
            "https://other/x"
        );
    }

    #[test]
    fn absolute_path_location_keeps_scheme_and_authority() {
        assert_eq!(
            resolve_location("https://a:8443/b/c?q=1", "/x/y"),
            "https://a:8443/x/y"
        );
    }

    #[test]
    fn relative_path_location_merges_against_the_directory() {
        assert_eq!(
            resolve_location("https://a/b/c/d.mp4", "e.ts"),
            "https://a/b/c/e.ts"
        );
        assert_eq!(
            resolve_location("https://a/b/c/d.mp4", "../e.ts"),
            "https://a/b/e.ts"
        );
    }

    #[test]
    fn relative_path_against_a_rootless_base_gets_a_root() {
        assert_eq!(resolve_location("https://a", "x"), "https://a/x");
    }

    #[test]
    fn request_target_lowercases_the_scheme_and_rejects_nesting() {
        let u = split_url("HTTP://host/path");
        assert_eq!(request_target(&u).unwrap(), "http://host/path");

        // This module never sees "http+file" as `url` — only http/https
        // ever gets to HttpProtocol::open — but if it somehow did, refuse
        // rather than guess.
        assert!(matches!(
            request_target(&split_url("http+file:secret.bin")),
            Err(TargetError::Nested)
        ));
    }

    #[test]
    fn userinfo_is_split_off_and_the_rest_is_unchanged() {
        let (creds, rest) = split_userinfo("http://alice:s3cret@host:8080/path?q=1");
        assert_eq!(creds, Some(("alice".to_owned(), "s3cret".to_owned())));
        assert_eq!(rest, "http://host:8080/path?q=1");
    }

    #[test]
    fn userinfo_with_no_password_is_fine() {
        let (creds, rest) = split_userinfo("http://alice@host/path");
        assert_eq!(creds, Some(("alice".to_owned(), String::new())));
        assert_eq!(rest, "http://host/path");
    }

    #[test]
    fn no_userinfo_leaves_the_target_untouched() {
        let (creds, rest) = split_userinfo("http://host/path");
        assert_eq!(creds, None);
        assert_eq!(rest, "http://host/path");
    }

    #[test]
    fn an_at_sign_in_the_path_is_not_userinfo() {
        let (creds, rest) = split_userinfo("http://host/user@example.com");
        assert_eq!(creds, None);
        assert_eq!(rest, "http://host/user@example.com");
    }

    proptest::proptest! {
        /// `remove_dot_segments` never panics on arbitrary input — the
        /// property that matters most, since this function's whole job is to
        /// process a server-controlled `Location` header.
        #[test]
        fn remove_dot_segments_never_panics(s in ".*") {
            let _ = remove_dot_segments(&s);
        }

        /// RFC 3986's algorithm is idempotent: a path with no more dot
        /// segments to remove is a fixed point. Running it twice must equal
        /// running it once.
        #[test]
        fn remove_dot_segments_is_idempotent(s in "[a-z0-9./]{0,64}") {
            let once = remove_dot_segments(&s);
            let twice = remove_dot_segments(&once);
            proptest::prop_assert_eq!(once, twice);
        }

        /// `resolve_location` never panics for any `base`/`location` pair,
        /// including a `base` that is not actually well-formed — this is the
        /// function that turns a server-chosen `Location` header into a URL
        /// string, so it must be total.
        #[test]
        fn resolve_location_never_panics(base in ".*", location in ".*") {
            let _ = resolve_location(&base, &location);
        }

        /// `request_target` never panics for any scheme/rest/args
        /// combination `split_url` can produce.
        #[test]
        fn request_target_never_panics(s in ".*") {
            let _ = request_target(&split_url(&s));
        }

        /// `split_userinfo` never panics, and always returns a `target`
        /// component that is a substring-shaped reconstruction (never
        /// longer than the input plus the userinfo it removed).
        #[test]
        fn split_userinfo_never_panics(s in ".*") {
            let (creds, rest) = split_userinfo(&s);
            if creds.is_none() {
                proptest::prop_assert_eq!(rest, s);
            }
        }
    }
}
