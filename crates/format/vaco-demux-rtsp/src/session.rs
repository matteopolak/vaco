//! The RTSP state machine: `OPTIONS`/`DESCRIBE`/`SETUP`/`PLAY`/`PAUSE`/
//! `TEARDOWN`, session ids, and `GET_PARAMETER` keepalive.
//!
//! One [`RtspSession`] per connection. It does not know about transports at
//! all beyond carrying whatever `Transport:` string a caller hands `setup`
//! — [`crate::transport`] and [`crate::demux`] own the socket/interleaving
//! decisions this crate's security posture is about; this module is purely
//! the request/response protocol on top of [`crate::connection::RtspConnection`].

use std::collections::VecDeque;
use std::time::Duration;

use vaco_core::{Error, Result};
use vaco_protocol_core::ProtocolEnv;

use crate::auth::{self, Challenge};
use crate::connection::{ConnMessage, RtspConnection, connect_tcp, host_port};
use crate::message::{Method, Request, Response};

/// Everything a `SETUP` response can hand back that [`crate::demux`] needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupResult {
    pub session_id: String,
    /// Session keepalive interval in seconds, from `Session: <id>;timeout=<n>`
    /// (RFC 2326 §12.37) — `None` when the server did not name one, in
    /// which case RFC 2326's own default of 60 seconds applies.
    pub timeout_secs: Option<u32>,
    pub transport: crate::transport::TransportSpec,
}

/// An open RTSP control connection and everything needed to keep issuing
/// requests on it: the `CSeq` counter, the negotiated session id, and
/// whatever authentication challenge the server issued.
pub struct RtspSession {
    conn: RtspConnection,
    base_uri: String,
    cseq: u32,
    session_id: Option<String>,
    credentials: Option<(String, String)>,
    challenge: Option<Challenge>,
    /// The `User-Agent` header value to stamp on every outgoing request, or
    /// empty to send none — `RtspOptions::user_agent`'s value, set via
    /// [`Self::set_user_agent`]. Not defaulted to that option's own default
    /// here: this module does not depend on `crate::options`, so the empty
    /// string (send nothing) is this type's own default, and
    /// [`crate::demux::RtspDemuxer::open`] is what actually applies the
    /// option.
    user_agent: String,
    /// Interleaved frames that arrived while waiting for a response to some
    /// other request — RFC 2326 does not forbid the server from sending
    /// media the instant it likes, so a frame can race a `SETUP` response.
    /// Drained by [`crate::demux::RtspDemuxer`].
    pub pending_frames: VecDeque<(u8, Vec<u8>)>,
}

impl std::fmt::Debug for RtspSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RtspSession")
            .field("base_uri", &self.base_uri)
            .field("cseq", &self.cseq)
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

impl RtspSession {
    /// Connect to `rtsp_url`'s host:port over plain `tcp:` and construct a
    /// session ready for `OPTIONS`/`DESCRIBE`. `rtsp_url` must already have
    /// its `rtsp://` scheme stripped — callers hold the full URL for
    /// `SETUP`'s `a=control` resolution, this only needs the authority.
    ///
    /// # Errors
    /// Whatever [`connect_tcp`] reports, including
    /// [`vaco_protocol_core::ProtocolError::Denied`] when `tcp` is not on
    /// `env`'s whitelist.
    pub fn connect(
        rtsp_url: &str,
        timeout: Option<Duration>,
        env: &ProtocolEnv<'_>,
    ) -> Result<Self> {
        let authority = rtsp_url
            .strip_prefix("rtsp://")
            .ok_or(Error::InvalidData("expected an rtsp:// URL"))?;
        let (authority, _path) = authority.split_once('/').unwrap_or((authority, ""));
        let hp = host_port(authority).map_err(map_protocol_err)?;
        let tcp = connect_tcp(&hp, timeout, env).map_err(map_protocol_err)?;
        Ok(Self {
            conn: RtspConnection::from_stream(tcp),
            base_uri: rtsp_url.to_owned(),
            cseq: 0,
            session_id: None,
            credentials: None,
            challenge: None,
            user_agent: String::new(),
            pending_frames: VecDeque::new(),
        })
    }

    /// Build a session directly from an already-connected [`RtspConnection`]
    /// — the path every test in this crate uses (a loopback
    /// `TcpStream`/mock server), and what an HTTP-tunnelled session uses
    /// once [`crate::http_tunnel::HttpTunnelStream`] is wrapped.
    #[must_use]
    pub fn from_connection(conn: RtspConnection, base_uri: impl Into<String>) -> Self {
        Self {
            conn,
            base_uri: base_uri.into(),
            cseq: 0,
            session_id: None,
            credentials: None,
            challenge: None,
            user_agent: String::new(),
            pending_frames: VecDeque::new(),
        }
    }

    #[must_use]
    pub fn base_uri(&self) -> &str {
        &self.base_uri
    }

    pub fn set_credentials(&mut self, username: impl Into<String>, password: impl Into<String>) {
        self.credentials = Some((username.into(), password.into()));
    }

    /// Set the `User-Agent` header [`Self::roundtrip`] stamps on every
    /// outgoing request from now on. An empty string sends none — the
    /// reference's own behaviour when built without a version string to
    /// report is out of scope for a clean-room reimplementation to probe,
    /// so this crate simply omits the header rather than guessing.
    pub fn set_user_agent(&mut self, user_agent: impl Into<String>) {
        self.user_agent = user_agent.into();
    }

    fn next_cseq(&mut self) -> u32 {
        self.cseq += 1;
        self.cseq
    }

    /// Send `req` (stamping `CSeq` and `Session` automatically) and return
    /// the matching response, retrying once with `Authorization` if the
    /// server answers `401` and this session has credentials.
    ///
    /// # Errors
    /// [`Error::InvalidData`] on a non-matching `CSeq` (a protocol
    /// violation this crate does not try to recover from) or a `401` this
    /// session cannot answer (no credentials, or an unsupported challenge
    /// scheme); otherwise whatever [`RtspConnection::read_message`] reports.
    pub fn roundtrip(&mut self, mut req: Request) -> Result<Response> {
        if !self.user_agent.is_empty() {
            req.headers.push("User-Agent", self.user_agent.clone());
        }
        if let Some(session_id) = self.session_id.clone() {
            req.headers.push("Session", session_id);
        }
        if let Some(challenge) = &self.challenge
            && let Some((user, pass)) = &self.credentials
        {
            let value = auth::authorization(challenge, user, pass, req.method.as_str(), &req.uri);
            req.headers.push("Authorization", value);
        }

        let cseq = self.next_cseq();
        req.headers.push("CSeq", cseq.to_string());
        self.conn.send(&req)?;

        let resp = loop {
            match self.conn.read_message()? {
                ConnMessage::Interleaved(ch, data) => self.pending_frames.push_back((ch, data)),
                ConnMessage::Response(resp, _body) => {
                    if resp.cseq() != Some(cseq) {
                        return Err(Error::InvalidData(
                            "RTSP response CSeq does not match the request",
                        ));
                    }
                    break resp;
                }
            }
        };

        if resp.status == 401 {
            if req.headers.get("Authorization").is_some() {
                // This request already carried credentials and still got a
                // 401 — retrying again would spin forever on a genuinely
                // wrong password. Report it instead.
                return Ok(resp);
            }
            let Some(value) = resp.headers.get("WWW-Authenticate") else {
                return Err(Error::InvalidData(
                    "401 response named no WWW-Authenticate challenge",
                ));
            };
            let Some(challenge) = auth::parse_challenge(value) else {
                return Err(Error::Unsupported(
                    "WWW-Authenticate scheme is not Basic or Digest",
                ));
            };
            if self.credentials.is_none() {
                return Err(Error::InvalidData(
                    "server requires authentication and none was configured",
                ));
            }
            self.challenge = Some(challenge);
            // Exactly one retry: `roundtrip_authenticated` re-sends the
            // same request with a fresh CSeq and (now that `self.challenge`
            // is `Some`) an `Authorization` header attached, and the check
            // above stops a second failure from retrying again.
            return self.roundtrip_authenticated(req);
        }

        if let Some(sid) = resp.headers.get("Session") {
            let id = sid.split(';').next().unwrap_or(sid).trim().to_owned();
            self.session_id = Some(id);
        }

        Ok(resp)
    }

    fn roundtrip_authenticated(&mut self, mut req: Request) -> Result<Response> {
        req.headers.0.retain(|(k, _)| {
            !k.eq_ignore_ascii_case("cseq")
                && !k.eq_ignore_ascii_case("authorization")
                && !k.eq_ignore_ascii_case("session")
        });
        self.roundtrip(req)
    }

    /// `OPTIONS` — also this crate's `GET_PARAMETER`-unsupported keepalive
    /// fallback (RFC 2326 §10.8 makes `OPTIONS` valid at any time, unlike
    /// `GET_PARAMETER`, which some servers reject with `501`).
    ///
    /// # Errors
    /// As [`RtspSession::roundtrip`].
    pub fn options(&mut self) -> Result<Response> {
        self.roundtrip(Request::new(Method::Options, self.base_uri.clone()))
    }

    /// `DESCRIBE` — returns the raw SDP body text; `crate::demux` parses it.
    ///
    /// # Errors
    /// As [`RtspSession::roundtrip`], plus [`Error::InvalidData`] if the
    /// response body is not valid UTF-8 or the server did not answer `200`.
    pub fn describe(&mut self) -> Result<String> {
        let mut req = Request::new(Method::Describe, self.base_uri.clone());
        req.headers.push("Accept", "application/sdp");
        let cseq = self.next_cseq();
        req.headers.push("CSeq", cseq.to_string());
        self.conn.send(&req)?;
        let (resp, body) = loop {
            match self.conn.read_message()? {
                ConnMessage::Interleaved(ch, data) => self.pending_frames.push_back((ch, data)),
                ConnMessage::Response(resp, body) => break (resp, body),
            }
        };
        if resp.status != 200 {
            return Err(Error::InvalidData("DESCRIBE did not return 200"));
        }
        String::from_utf8(body).map_err(|_| Error::InvalidData("DESCRIBE body is not valid UTF-8"))
    }

    /// `SETUP` for one track's control URL, offering `transport`.
    ///
    /// # Errors
    /// As [`RtspSession::roundtrip`], plus [`Error::InvalidData`] if the
    /// server did not answer `200` or named no `Transport:`/`Session:`.
    pub fn setup(
        &mut self,
        track_uri: &str,
        transport: &crate::transport::TransportSpec,
    ) -> Result<SetupResult> {
        let mut req = Request::new(Method::Setup, track_uri.to_owned());
        req.headers.push("Transport", transport.to_header_value());
        let resp = self.roundtrip(req)?;
        if resp.status != 200 {
            return Err(Error::InvalidData("SETUP did not return 200"));
        }
        let session_header = resp
            .headers
            .get("Session")
            .ok_or(Error::InvalidData("SETUP response named no Session"))?;
        let (session_id, timeout_secs) = match session_header.split_once(';') {
            Some((id, rest)) => {
                let timeout = rest
                    .trim()
                    .strip_prefix("timeout=")
                    .and_then(|v| v.trim().parse().ok());
                (id.trim().to_owned(), timeout)
            }
            None => (session_header.trim().to_owned(), None),
        };
        let transport_value = resp
            .headers
            .get("Transport")
            .ok_or(Error::InvalidData("SETUP response named no Transport"))?;
        let specs = crate::transport::parse(transport_value)?;
        let transport = specs.into_iter().next().ok_or(Error::InvalidData(
            "SETUP response's Transport header is empty",
        ))?;
        Ok(SetupResult {
            session_id,
            timeout_secs,
            transport,
        })
    }

    /// `PLAY`, optionally with an RFC 2326 §12.29 `Range:` header (`None`
    /// plays from the current position, matching the reference's own
    /// default of "no Range means start now").
    ///
    /// # Errors
    /// As [`RtspSession::roundtrip`].
    pub fn play(&mut self, range: Option<&str>) -> Result<Response> {
        let mut req = Request::new(Method::Play, self.base_uri.clone());
        if let Some(range) = range {
            req.headers.push("Range", range.to_owned());
        }
        self.roundtrip(req)
    }

    /// # Errors
    /// As [`RtspSession::roundtrip`].
    pub fn pause(&mut self) -> Result<Response> {
        self.roundtrip(Request::new(Method::Pause, self.base_uri.clone()))
    }

    /// `TEARDOWN`. Errors from this call are usually not worth propagating
    /// as a hard failure — a caller tearing down a session that is about to
    /// be dropped anyway is better served by "best effort", but this
    /// returns [`Result`] rather than swallowing errors silently so a
    /// caller that does care can see them.
    ///
    /// # Errors
    /// As [`RtspSession::roundtrip`].
    pub fn teardown(&mut self) -> Result<Response> {
        self.roundtrip(Request::new(Method::Teardown, self.base_uri.clone()))
    }

    /// The next interleaved frame — drains [`RtspSession::pending_frames`]
    /// first (frames that arrived while a `roundtrip` was waiting for its
    /// own response), then reads the connection directly. A stray
    /// `Response` arriving here (nothing this crate sends should provoke
    /// one outside a `roundtrip` call) is skipped rather than erroring,
    /// since a keepalive racing a `PLAY`-triggered data burst is a timing
    /// accident, not a protocol violation.
    ///
    /// # Errors
    /// As [`RtspConnection::read_message`].
    pub fn read_frame(&mut self) -> Result<(u8, Vec<u8>)> {
        if let Some(frame) = self.pending_frames.pop_front() {
            return Ok(frame);
        }
        loop {
            match self.conn.read_message()? {
                ConnMessage::Interleaved(ch, data) => return Ok((ch, data)),
                ConnMessage::Response(_, _) => {}
            }
        }
    }

    /// `GET_PARAMETER` with no body — RFC 2326 §10.8's recommended
    /// keepalive ping. Falls back to [`RtspSession::options`] on a `501
    /// Not Implemented`, since not every server implements a bodyless
    /// `GET_PARAMETER` (measured against several embedded RTSP cameras).
    ///
    /// # Errors
    /// As [`RtspSession::roundtrip`].
    pub fn keepalive(&mut self) -> Result<Response> {
        let resp = self.roundtrip(Request::new(Method::GetParameter, self.base_uri.clone()))?;
        if resp.status == 501 {
            return self.options();
        }
        Ok(resp)
    }
}

/// Fold a [`vaco_protocol_core::ProtocolError`] into [`Error`]. Used only
/// where the caller's return type is already `vaco_core::Result` and
/// cannot carry the richer type — [`crate::http_tunnel::HttpTunnelStream::connect`]
/// is the path that keeps [`vaco_protocol_core::ProtocolError::Denied`]
/// distinguishable all the way out, for exactly the reason its own docs
/// give; this function is for callers that do not need that distinction
/// (a session `connect` failure is reported to a user either way, not
/// programmatically branched on).
fn map_protocol_err(e: vaco_protocol_core::ProtocolError) -> Error {
    match e {
        vaco_protocol_core::ProtocolError::Io(inner) => inner,
        vaco_protocol_core::ProtocolError::Denied { .. } => {
            Error::Unsupported("RTSP control connection was refused by the protocol whitelist")
        }
        _ => Error::InvalidData("RTSP control connection could not be established"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    fn read_request_head(server: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            server.read_exact(&mut byte).unwrap();
            buf.push(byte[0]);
            if buf.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn options_round_trip_reads_matching_cseq() {
        let (client, mut server) = pair();
        let handle = thread::spawn(move || {
            let head = read_request_head(&mut server);
            assert!(head.starts_with("OPTIONS rtsp://host/stream RTSP/1.0\r\n"));
            let cseq_line = head.lines().find(|l| l.starts_with("CSeq:")).unwrap();
            let cseq = cseq_line.trim_start_matches("CSeq:").trim();
            server
                .write_all(
                    format!("RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nPublic: OPTIONS, DESCRIBE\r\n\r\n")
                        .as_bytes(),
                )
                .unwrap();
        });
        let mut session =
            RtspSession::from_connection(RtspConnection::from_stream(client), "rtsp://host/stream");
        let resp = session.options().unwrap();
        assert_eq!(resp.status, 200);
        handle.join().unwrap();
    }

    #[test]
    fn setup_parses_session_and_transport() {
        let (client, mut server) = pair();
        let handle = thread::spawn(move || {
            let head = read_request_head(&mut server);
            let cseq = head
                .lines()
                .find(|l| l.starts_with("CSeq:"))
                .unwrap()
                .trim_start_matches("CSeq:")
                .trim()
                .to_owned();
            server.write_all(format!(
                "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nSession: 12345678;timeout=60\r\nTransport: RTP/AVP;unicast;client_port=5000-5001;server_port=6000-6001\r\n\r\n"
            ).as_bytes()).unwrap();
        });
        let mut session =
            RtspSession::from_connection(RtspConnection::from_stream(client), "rtsp://host/stream");
        let offer = crate::transport::TransportSpec::offer(
            crate::transport::TransportMode::UdpUnicast,
            (5000, 5001),
            (0, 1),
        );
        let result = session.setup("rtsp://host/stream/track1", &offer).unwrap();
        assert_eq!(result.session_id, "12345678");
        assert_eq!(result.timeout_secs, Some(60));
        assert_eq!(result.transport.server_port, Some((6000, 6001)));
        handle.join().unwrap();
    }

    #[test]
    fn authenticates_after_a_401_with_digest() {
        let (client, mut server) = pair();
        let handle = thread::spawn(move || {
            let _first = read_request_head(&mut server);
            server.write_all(
                b"RTSP/1.0 401 Unauthorized\r\nCSeq: 1\r\nWWW-Authenticate: Digest realm=\"x\", nonce=\"n\"\r\n\r\n",
            ).unwrap();
            let second = read_request_head(&mut server);
            assert!(second.contains("Authorization: Digest"));
            server
                .write_all(b"RTSP/1.0 200 OK\r\nCSeq: 2\r\n\r\n")
                .unwrap();
        });
        let mut session =
            RtspSession::from_connection(RtspConnection::from_stream(client), "rtsp://host/stream");
        session.set_credentials("user", "pass");
        let resp = session.options().unwrap();
        assert_eq!(resp.status, 200);
        handle.join().unwrap();
    }
}
