//! [`FtpProtocol`] — the `ftp:` scheme, URL parsing, and the registry entry.

use std::time::Duration;

use vaco_io::{MediaSink, MediaSource};
use vaco_opts::{Dict, OptionsExt, Schema, schema_of};
use vaco_protocol_core::{
    IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result, Url,
};
use vaco_protocol_socket::url::HostPort;

use crate::control::Session;
use crate::options::FtpOptions;
use crate::sink::FtpSink;
use crate::source::FtpSource;

/// Default FTP control port (RFC 959).
const DEFAULT_PORT: u16 = 21;

/// `[user[:pass]@]host[:port]/path`, split from `url.rest`. `pub` (and
/// `parse_url` with it) so the fuzz target for this crate can drive the URL
/// parser directly with arbitrary bytes.
#[derive(Debug)]
pub struct FtpUrl {
    host: HostPort,
    userinfo: Option<(String, Option<String>)>,
    path: String,
}

/// # Errors
/// [`ProtocolError::Malformed`] if `rest` names no parseable host.
pub fn parse_url(rest: &str) -> Result<FtpUrl> {
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (userinfo, hostport) = match authority.split_once('@') {
        Some((info, hp)) => {
            let (user, pass) = match info.split_once(':') {
                Some((u, p)) => (u.to_owned(), Some(p.to_owned())),
                None => (info.to_owned(), None),
            };
            (Some((user, pass)), hp)
        }
        None => (None, authority),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().unwrap_or(DEFAULT_PORT)),
        None => (hostport, DEFAULT_PORT),
    };
    if host.is_empty() {
        return Err(ProtocolError::Malformed {
            scheme: "ftp",
            detail: "expected ftp://host[:port]/path",
        });
    }
    Ok(FtpUrl {
        host: HostPort {
            host: host.to_owned(),
            port,
        },
        userinfo,
        // Measured: the full path (including the leading `/`) is used
        // verbatim in SIZE/REST/RETR/STOR — no CWD-relative navigation.
        path: format!("/{path}"),
    })
}

/// Resolve the login user/password per the measured precedence: URL
/// userinfo, then the matching `-ftp-*` option, then (for the password,
/// only when the resolved user is `anonymous`) `-ftp-anonymous-password`,
/// finally the reference's own measured default `nopassword`.
fn credentials(url: &FtpUrl, opts: &FtpOptions) -> (String, String) {
    let user = url
        .userinfo
        .as_ref()
        .map(|(u, _)| u.clone())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| {
            if opts.user.is_empty() {
                "anonymous".to_owned()
            } else {
                opts.user.clone()
            }
        });

    let url_password = url.userinfo.as_ref().and_then(|(_, p)| p.clone());
    let password = url_password.unwrap_or_else(|| {
        if !opts.password.is_empty() {
            opts.password.clone()
        } else if user == "anonymous" {
            if opts.anonymous_password.is_empty() {
                "nopassword".to_owned()
            } else {
                opts.anonymous_password.clone()
            }
        } else {
            String::new()
        }
    });
    (user, password)
}

fn options(opts: &Dict) -> Result<FtpOptions> {
    let mut parsed = FtpOptions::default();
    parsed
        .apply_dict(opts)
        .map_err(|_| ProtocolError::Malformed {
            scheme: "ftp",
            detail: "bad option value",
        })?;
    Ok(parsed)
}

fn timeout_of(opts: &FtpOptions) -> Option<Duration> {
    (opts.timeout >= 0).then(|| Duration::from_micros(u64::try_from(opts.timeout).unwrap_or(0)))
}

/// Connect, log in, and run the measured `TYPE`/`FEAT`/`PWD` setup sequence.
fn open_session(url: &FtpUrl, opts: &FtpOptions, env: &ProtocolEnv<'_>) -> Result<Session> {
    let timeout = timeout_of(opts);
    let mut session = Session::connect(&url.host, timeout, env)?;
    let (user, password) = credentials(url, opts);
    session.login(&user, &password)?;
    session.setup()?;
    Ok(session)
}

/// The `ftp:` protocol.
#[derive(Debug, Clone, Copy, Default)]
pub struct FtpProtocol;

impl Protocol for FtpProtocol {
    fn open(
        &self,
        url: &Url,
        _flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        let ftp_url = parse_url(&url.rest)?;
        let parsed = options(opts)?;
        let timeout = timeout_of(&parsed);
        let session = open_session(&ftp_url, &parsed, env)?;
        let source = FtpSource::open(session, ftp_url.host.host, ftp_url.path, timeout, env)?;
        Ok(Box::new(source))
    }

    fn create(
        &self,
        url: &Url,
        _flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        let ftp_url = parse_url(&url.rest)?;
        let parsed = options(opts)?;
        let timeout = timeout_of(&parsed);
        let session = open_session(&ftp_url, &parsed, env)?;
        let sink = FtpSink::open(
            session,
            ftp_url.host.host,
            ftp_url.path,
            timeout,
            parsed.write_seekable,
            env,
        )?;
        Ok(Box::new(sink))
    }
}

fn ftp_schema() -> &'static Schema {
    schema_of::<FtpOptions>()
}

/// The registry entry for `ftp:`.
pub static FTP_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "ftp",
    long_name: "File Transfer Protocol",
    // `-protocols` lists `ftp` under both `Input:` and `Output:`.
    flags: ProtocolFlags {
        network: true,
        nested_scheme: true,
        server_capable: false,
        readable: true,
        writable: true,
    },
    // Measured empty: `-protocol_whitelist ftp` alone refuses the nested
    // `tcp` open with `Protocol 'tcp' not on whitelist 'ftp'!`.
    default_whitelist: &[],
    options: Some(ftp_schema),
    proto: &FtpProtocol,
};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_host_and_path() {
        let u = parse_url("//127.0.0.1:12121/pub/file.bin").unwrap();
        assert_eq!(u.host.host, "127.0.0.1");
        assert_eq!(u.host.port, 12121);
        assert_eq!(u.path, "/pub/file.bin");
        assert!(u.userinfo.is_none());
    }

    #[test]
    fn defaults_the_port_to_21() {
        let u = parse_url("//ftp.example.com/file.bin").unwrap();
        assert_eq!(u.host.port, 21);
    }

    #[test]
    fn parses_userinfo() {
        let u = parse_url("//bob:secret@host/f").unwrap();
        assert_eq!(
            u.userinfo,
            Some(("bob".to_owned(), Some("secret".to_owned())))
        );
    }

    #[test]
    fn credentials_default_to_anonymous_nopassword() {
        let url = parse_url("//host/f").unwrap();
        let opts = FtpOptions::default();
        assert_eq!(
            credentials(&url, &opts),
            ("anonymous".to_owned(), "nopassword".to_owned())
        );
    }

    #[test]
    fn url_userinfo_overrides_options() {
        let url = parse_url("//bob:secret@host/f").unwrap();
        let opts = FtpOptions {
            user: "other".to_owned(),
            password: "otherpass".to_owned(),
            ..Default::default()
        };
        assert_eq!(
            credentials(&url, &opts),
            ("bob".to_owned(), "secret".to_owned())
        );
    }

    #[test]
    fn anonymous_password_option_is_used_only_for_anonymous_user() {
        let url = parse_url("//host/f").unwrap();
        let opts = FtpOptions {
            anonymous_password: "me@example.com".to_owned(),
            ..Default::default()
        };
        assert_eq!(
            credentials(&url, &opts),
            ("anonymous".to_owned(), "me@example.com".to_owned())
        );
    }

    #[test]
    fn default_whitelist_is_empty() {
        assert!(FTP_PROTOCOL.default_whitelist.is_empty());
    }
}
