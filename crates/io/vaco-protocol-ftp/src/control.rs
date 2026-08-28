//! The control connection: login, `TYPE`/`FEAT`/`PWD`, `REST`/`SIZE`, and
//! `EPSV`/`PASV` negotiation.
//!
//! # Why a persistent `BufReader` here and not a byte-at-a-time reader
//!
//! `vaco_protocol_dial::read_header_block` reads one byte at a time because
//! its callers hand the same socket back as a raw tunnel afterward, so any
//! read-ahead would be stranded. The FTP control connection has no such
//! handoff — it is reused for every command for the life of the session — so
//! a `BufReader` kept alive as a `Session` field never loses a byte.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

use vaco_core::Error as CoreError;
use vaco_protocol_core::{ProtocolEnv, ProtocolError, Result};
use vaco_protocol_socket::url::HostPort;

/// One parsed FTP response: the three-digit code and the text of its final
/// (or only) line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FtpResponse {
    pub code: u16,
    pub text: String,
}

impl FtpResponse {
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.code < 400
    }
}

/// Bytes a single control response may reasonably use.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// A logged-in FTP control connection.
pub struct Session {
    reader: BufReader<TcpStream>,
    timeout: Option<Duration>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

impl Session {
    /// Connect the control channel.
    ///
    /// # Errors
    /// [`ProtocolError::Denied`] if `"tcp"` is not permitted by `env`;
    /// otherwise whatever the connection attempt or greeting failed with.
    pub fn connect(hp: &HostPort, timeout: Option<Duration>, env: &ProtocolEnv<'_>) -> Result<Self> {
        let stream = vaco_protocol_dial::dial_tcp(hp, timeout, env)?;
        let mut session = Self {
            reader: BufReader::new(stream),
            timeout,
        };
        let greeting = session.read_response()?;
        if !greeting.is_success() {
            return Err(ProtocolError::Malformed {
                scheme: "ftp",
                detail: "server did not send a 2xx greeting",
            });
        }
        Ok(session)
    }

    /// Read one response, handling RFC 959's multi-line form (`NNN-...`
    /// continued until a line starting with the same code and a space).
    fn read_response(&mut self) -> Result<FtpResponse> {
        let first = self.read_line()?;
        let code = first
            .get(..3)
            .and_then(|c| c.parse::<u16>().ok())
            .ok_or(ProtocolError::Malformed {
                scheme: "ftp",
                detail: "server response did not start with a three-digit code",
            })?;
        let is_multiline = first.as_bytes().get(3) == Some(&b'-');
        if !is_multiline {
            let text = first.get(4..).unwrap_or("").trim_end().to_owned();
            return Ok(FtpResponse { code, text });
        }
        let terminator_prefix = format!("{code} ");
        loop {
            let line = self.read_line()?;
            if line.starts_with(&terminator_prefix) {
                let text = line
                    .get(terminator_prefix.len()..)
                    .unwrap_or("")
                    .trim_end()
                    .to_owned();
                return Ok(FtpResponse { code, text });
            }
        }
    }

    fn read_line(&mut self) -> Result<String> {
        let mut line = String::new();
        let mut total = 0usize;
        loop {
            let n = self.reader.read_line(&mut line)?;
            if n == 0 {
                return Err(CoreError::UnexpectedEof.into());
            }
            total += n;
            if total > MAX_RESPONSE_BYTES {
                return Err(ProtocolError::Malformed {
                    scheme: "ftp",
                    detail: "server response exceeded the size limit",
                });
            }
            if line.ends_with('\n') {
                return Ok(line);
            }
        }
    }

    fn command(&mut self, line: &str) -> Result<FtpResponse> {
        self.reader.get_mut().write_all(line.as_bytes())?;
        self.reader.get_mut().write_all(b"\r\n")?;
        self.read_response()
    }

    /// `USER`/`PASS`.
    ///
    /// # Errors
    /// [`ProtocolError::Malformed`] if login is refused.
    pub fn login(&mut self, user: &str, password: &str) -> Result<()> {
        let user_resp = self.command(&format!("USER {user}"))?;
        // 230: logged in without a password prompt at all (measured on some
        // servers); 331: password required, the common case this crate's
        // own fake server exercises.
        if user_resp.code == 331 {
            let pass_resp = self.command(&format!("PASS {password}"))?;
            if !pass_resp.is_success() {
                return Err(ProtocolError::Malformed {
                    scheme: "ftp",
                    detail: "server refused the login password",
                });
            }
        } else if !user_resp.is_success() {
            return Err(ProtocolError::Malformed {
                scheme: "ftp",
                detail: "server refused the login user",
            });
        }
        Ok(())
    }

    /// `TYPE I`, `FEAT`, `PWD` — sent in that order to match the measured
    /// transcript exactly. Responses are read (to keep the control
    /// connection's request/response pairing intact) but not otherwise
    /// acted on: see the crate docs on why this crate does not implement
    /// `CWD`-relative navigation or feature-gated command selection.
    ///
    /// # Errors
    /// Propagates a connection failure; a non-2xx `TYPE I` response is
    /// treated as fatal (binary transfer is this crate's only mode), `FEAT`
    /// and `PWD` are not.
    pub fn setup(&mut self) -> Result<()> {
        let type_resp = self.command("TYPE I")?;
        if !type_resp.is_success() {
            return Err(ProtocolError::Malformed {
                scheme: "ftp",
                detail: "server refused TYPE I (binary mode)",
            });
        }
        let _ = self.command("FEAT")?;
        let _ = self.command("PWD")?;
        Ok(())
    }

    /// `REST <offset>`, probing (or setting up) a resume point. Measured:
    /// sent unconditionally before every transfer, including `REST 0` for a
    /// fresh, non-resumed open.
    ///
    /// A non-2xx/3xx response is not fatal — some servers do not support
    /// `REST` at all, and the reference itself does not appear to abort on
    /// refusal (untested against a real such server; see the crate docs).
    ///
    /// # Errors
    /// Propagates a connection failure only.
    pub fn rest(&mut self, offset: u64) -> Result<FtpResponse> {
        self.command(&format!("REST {offset}"))
    }

    /// `SIZE <path>`, returning the remote size if the server answers with
    /// one.
    ///
    /// # Errors
    /// Propagates a connection failure only; a size the server refuses to
    /// give (unsupported, or the path does not exist) is `Ok(None)`, not an
    /// error — `SIZE` failing does not mean `RETR` will.
    pub fn size(&mut self, path: &str) -> Result<Option<u64>> {
        let resp = self.command(&format!("SIZE {path}"))?;
        if !resp.is_success() {
            return Ok(None);
        }
        Ok(resp.text.trim().parse::<u64>().ok())
    }

    /// Negotiate a passive data connection: `EPSV` first, falling back to
    /// `PASV` if the server rejects it — measured (a fake server answering
    /// `500` to `EPSV` gets a `PASV` retry, and the resulting address is
    /// used).
    ///
    /// # Errors
    /// [`ProtocolError::Malformed`] if neither negotiation produces a
    /// parseable address.
    pub fn passive(&mut self, control_host: &str) -> Result<HostPort> {
        let epsv = self.command("EPSV")?;
        if epsv.is_success()
            && let Some(port) = parse_epsv(&epsv.text)
        {
            return Ok(HostPort {
                host: control_host.to_owned(),
                port,
            });
        }
        let pasv = self.command("PASV")?;
        parse_pasv(&pasv.text).ok_or(ProtocolError::Malformed {
            scheme: "ftp",
            detail: "server's PASV/EPSV response did not contain a parseable address",
        })
    }

    /// `RETR <path>`, expecting the `1xx` that precedes the data transfer.
    ///
    /// # Errors
    /// [`ProtocolError::Malformed`] if the server refuses outright.
    pub fn retr(&mut self, path: &str) -> Result<()> {
        self.expect_transfer_start(&format!("RETR {path}"))
    }

    /// `STOR <path>`, expecting the `1xx` that precedes the data transfer.
    ///
    /// # Errors
    /// [`ProtocolError::Malformed`] if the server refuses outright.
    pub fn stor(&mut self, path: &str) -> Result<()> {
        self.expect_transfer_start(&format!("STOR {path}"))
    }

    fn expect_transfer_start(&mut self, line: &str) -> Result<()> {
        let resp = self.command(line)?;
        if (100..200).contains(&resp.code) {
            Ok(())
        } else {
            Err(ProtocolError::Malformed {
                scheme: "ftp",
                detail: "server refused to start the data transfer",
            })
        }
    }

    /// Read the final response after the data connection has closed
    /// (`226 Transfer complete`, typically).
    ///
    /// # Errors
    /// Propagates a connection failure. A non-2xx final response is not
    /// itself turned into an error here — the caller already has whatever
    /// bytes came over the data connection, and a truncated-but-reported
    /// transfer is a data-integrity question for the caller, not a reason
    /// to discard bytes already read.
    pub fn finish_transfer(&mut self) -> Result<FtpResponse> {
        self.read_response()
    }

    /// Best-effort `QUIT`. Errors are not surfaced — a caller closing its
    /// connection does not need to know the server's goodbye failed.
    pub fn quit(&mut self) {
        let _ = self.command("QUIT");
    }

    /// The timeout every data connection this session opens should also use.
    #[must_use]
    pub const fn timeout(&self) -> Option<Duration> {
        self.timeout
    }
}

/// Parse `227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)`-shaped text into a
/// [`HostPort`].
#[must_use]
pub fn parse_pasv(text: &str) -> Option<HostPort> {
    let open = text.find('(')?;
    let close = text.get(open..)?.find(')')? + open;
    let fields = text.get(open + 1..close)?.split(',');
    // Every field of a genuine `227` response is one byte (0..=255) of an
    // IPv4 address or a 16-bit port split high/low — `u8`, not `u16`.
    // Measured by fuzzing: parsing each field as `u16` let a malicious
    // response supply a value up to 65535 for `p1`, and `p1 * 256`
    // overflowed `u16` and panicked under the fuzz profile's overflow
    // checks. `u8` makes that value class unrepresentable instead of
    // catching it after the fact.
    let nums: Vec<u8> = fields.filter_map(|f| f.trim().parse().ok()).collect();
    let [h1, h2, h3, h4, p1, p2] = nums.as_slice() else {
        return None;
    };
    Some(HostPort {
        host: format!("{h1}.{h2}.{h3}.{h4}"),
        port: u16::from(*p1) * 256 + u16::from(*p2),
    })
}

/// Parse `229 Entering Extended Passive Mode (|||port|)`-shaped text (RFC
/// 2428) and return just the port — the host is always the control
/// connection's own peer for `EPSV`.
#[must_use]
pub fn parse_epsv(text: &str) -> Option<u16> {
    let open = text.find('(')?;
    let close = text[open..].find(')')? + open;
    let inner = text.get(open + 1..close)?;
    let delim = inner.chars().next()?;
    inner.trim_matches(delim).parse().ok()
}

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
    fn parses_pasv_address() {
        let hp = parse_pasv("Entering Passive Mode (127,0,0,1,47,100)").unwrap();
        assert_eq!(hp.host, "127.0.0.1");
        assert_eq!(hp.port, 47 * 256 + 100);
    }

    #[test]
    fn parses_epsv_port() {
        assert_eq!(
            parse_epsv("Entering Extended Passive Mode (|||12122|)"),
            Some(12122)
        );
    }

    #[test]
    fn epsv_parse_never_panics_on_garbage() {
        for s in ["", "(", ")", "(||)", "(abc)", "no parens at all"] {
            let _ = parse_epsv(s);
            let _ = parse_pasv(s);
        }
    }

    /// Found by fuzzing (`fuzz/fuzz_targets/protocol_ftp_parse.rs`,
    /// `fuzz/seeds/protocol_ftp_parse/`): a PASV field larger than 255
    /// parsed cleanly as `u16` and then overflowed computing `p1 * 256`
    /// under the fuzz profile's overflow checks. Each field of a real `227`
    /// response is one byte, so out-of-range fields must fail to parse, not
    /// merely fail to overflow.
    #[test]
    fn oversized_pasv_fields_do_not_overflow() {
        assert!(parse_pasv("(1,1,1,1,77777,1)").is_none());
        assert!(parse_pasv("(1,1,1,1,1,77777)").is_none());
        assert!(parse_pasv("(1,1,1,1,65535,65535)").is_none());
        // The real boundary: 255 is a valid byte, 256 is not.
        assert_eq!(
            parse_pasv("(127,0,0,1,255,255)"),
            Some(HostPort {
                host: "127.0.0.1".to_owned(),
                port: 255 * 256 + 255,
            })
        );
        assert!(parse_pasv("(127,0,0,1,256,0)").is_none());
    }
}
