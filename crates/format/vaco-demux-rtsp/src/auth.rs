//! RFC 2617 Basic and Digest authentication, exactly as RFC 2326 §4.4 says
//! RTSP borrows it from HTTP (RFC 7826 §22.2 continues that for 2.0).
//!
//! MD5 goes through `vaco-hash`, this workspace's single owner of it (D11)
//! — this crate does not declare `md-5` itself.

use std::fmt::Write as _;

use vaco_hash::HashAlgo;

/// A `WWW-Authenticate` challenge this crate knows how to answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Challenge {
    Basic {
        realm: String,
    },
    Digest {
        realm: String,
        nonce: String,
        opaque: Option<String>,
        qop: Option<String>,
    },
}

/// Split a `WWW-Authenticate` header value's `key=value` parameters,
/// tolerating both quoted and bare values (RFC 2617 §3.2.1's grammar
/// allows either for most parameters in practice, and real servers mix
/// them).
fn params(rest: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in rest.split(',') {
        let part = part.trim();
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"');
        out.push((k.trim().to_ascii_lowercase(), v.to_owned()));
    }
    out
}

/// Parse one `WWW-Authenticate` header value. A server may send several
/// (one Basic, one Digest); `crate::session` picks Digest when both are
/// offered, since it does not send the password in the clear.
#[must_use]
pub fn parse_challenge(value: &str) -> Option<Challenge> {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix("Digest ") {
        let p = params(rest);
        let get = |k: &str| p.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.clone());
        Some(Challenge::Digest {
            realm: get("realm")?,
            nonce: get("nonce")?,
            opaque: get("opaque"),
            qop: get("qop"),
        })
    } else if let Some(rest) = value.strip_prefix("Basic ") {
        let p = params(rest);
        let realm = p
            .iter()
            .find(|(k, _)| k == "realm")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        Some(Challenge::Basic { realm })
    } else {
        None
    }
}

fn md5_hex(parts: &[&str]) -> String {
    let joined = parts.join(":");
    HashAlgo::Md5
        .digest_hex(joined.as_bytes())
        .unwrap_or_default()
}

/// Build the `Authorization` header value for one request, given a
/// previously parsed [`Challenge`] and the credentials the caller
/// configured (`-user`/`-rtsp_transport` equivalents in this crate's own
/// option surface — see `crate::session`).
#[must_use]
pub fn authorization(
    challenge: &Challenge,
    username: &str,
    password: &str,
    method: &str,
    uri: &str,
) -> String {
    match challenge {
        Challenge::Basic { .. } => {
            format!(
                "Basic {}",
                crate::base64::encode(format!("{username}:{password}").as_bytes())
            )
        }
        Challenge::Digest {
            realm,
            nonce,
            opaque,
            qop,
        } => {
            let ha1 = md5_hex(&[username, realm, password]);
            let ha2 = md5_hex(&[method, uri]);
            let response = if qop.is_some() {
                // `nc`/`cnonce` fixed at `00000001`/a constant string: this
                // crate issues one request per nonce round-trip rather than
                // pipelining, so a counter that never advances is
                // acceptable — a real nonce-reuse concern only arises
                // across *many* requests under one nonce, which does not
                // happen here (`crate::session` re-authenticates per 401).
                md5_hex(&[&ha1, nonce, "00000001", "vaco", "auth", &ha2])
            } else {
                md5_hex(&[&ha1, nonce, &ha2])
            };
            let mut out = format!(
                "Digest username=\"{username}\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{uri}\", response=\"{response}\""
            );
            if let Some(opaque) = opaque {
                let _ = write!(out, ", opaque=\"{opaque}\"");
            }
            if qop.is_some() {
                out.push_str(", qop=auth, nc=00000001, cnonce=\"vaco\"");
            }
            out
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn parses_a_digest_challenge() {
        let c = parse_challenge(r#"Digest realm="example", nonce="abc123", opaque="xyz""#).unwrap();
        assert_eq!(
            c,
            Challenge::Digest {
                realm: "example".to_owned(),
                nonce: "abc123".to_owned(),
                opaque: Some("xyz".to_owned()),
                qop: None,
            }
        );
    }

    #[test]
    fn parses_a_basic_challenge() {
        let c = parse_challenge(r#"Basic realm="example""#).unwrap();
        assert_eq!(
            c,
            Challenge::Basic {
                realm: "example".to_owned()
            }
        );
    }

    #[test]
    fn unknown_scheme_is_none() {
        assert!(parse_challenge("Bearer token=abc").is_none());
    }

    #[test]
    fn digest_response_matches_rfc_2069_worked_example() {
        // RFC 2069 §2.4's classic worked example (no qop), reused here
        // because it is a public, well-known test vector, not something
        // FFmpeg-specific.
        let challenge = Challenge::Digest {
            realm: "testrealm@host.com".to_owned(),
            nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093".to_owned(),
            opaque: Some("5ccc069c403ebaf9f0171e9517f40e41".to_owned()),
            qop: None,
        };
        let header = authorization(
            &challenge,
            "Mufasa",
            "Circle Of Life",
            "GET",
            "/dir/index.html",
        );
        assert!(header.contains("response=\"670fd8c2df070c60b045671b8b24ff02\""));
    }

    #[test]
    fn basic_encodes_user_colon_password() {
        let header = authorization(
            &Challenge::Basic {
                realm: String::new(),
            },
            "u",
            "p",
            "GET",
            "/",
        );
        assert_eq!(header, format!("Basic {}", crate::base64::encode(b"u:p")));
    }
}
