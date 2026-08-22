//! [`HttpSource`]: the open stream, and where ranged reads and reconnection
//! actually happen.
//!
//! Not portable in the sense that it drives `ureq::Body`, but the only thing
//! it asks of `crate::transport` is `send(method, target, headers, timeout)
//! -> Response<Body>` — a `fetch`-based sibling would need a source shaped
//! exactly like this one, swapping only that one call and the `Read` it gets
//! back.
//!
//! # The central invariant: never report bytes from the wrong offset
//!
//! Every response this module accepts is classified by status code before a
//! single byte of it is handed to a caller:
//!
//! * `206` with a parseable `Content-Range` — genuinely ranged. `self.pos` is
//!   set from the header's own `start`, not assumed from what we asked for
//!   (measured: this is what protects against a server that satisfies a
//!   slightly different range than requested).
//! * `200` when the request was for byte `0` — indistinguishable from "the
//!   whole resource, which happens to start where we wanted": adopted as
//!   position `0`, marked not seekable from then on. This is the common case
//!   this crate's brief calls out by name: a server that does not support
//!   `Range` at all.
//! * `200` when the request was for a **non-zero** offset — the server
//!   ignored `Range` and is about to hand us the wrong bytes at the position
//!   we think we are at. Refused as an I/O error rather than silently
//!   consumed; see the module-level test `seek_when_range_is_ignored_errors_\
//!   instead_of_corrupting_position`.
//! * A redirect or a `4xx`/`5xx` reaching a `seek`/reconnect (as opposed to
//!   the initial open, which resolves redirects through the whitelist — see
//!   `crate::protocol`) is refused outright: there is no
//!   [`vaco_protocol_core::ProtocolEnv`] available at this layer to gate a
//!   new URL through, and silently following one without the gate is exactly
//!   what this crate exists not to do.

use std::io::{ErrorKind, Read};
use std::time::Duration as StdDuration;

use ureq::Body;
use ureq::http::Response;
use vaco_core::{Error, Result as CoreResult};
use vaco_io::{RawSource, Seekability};
use vaco_time::Instant;

use crate::headers::{self, RequestRange};
use crate::options::HttpOptions;
use crate::parse::{parse_content_range, parse_retry_after_secs};
use crate::reconnect::{self, Decision, Failure, State};
use crate::transport;

/// An open `http:`/`https:` read stream.
pub struct HttpSource {
    /// The *final*, post-redirect, userinfo-stripped target. Reconnects and
    /// seeks always go back to this exact URL, never re-resolving a
    /// redirect — see the module docs and `crate::protocol` for why.
    target: String,
    credentials: Option<(String, String)>,
    opts: HttpOptions,
    timeout: Option<StdDuration>,

    reader: Box<dyn Read + Send>,
    /// Logical position of the next byte [`RawSource::read`] will return.
    pos: u64,
    total_size: Option<u64>,
    seekable: bool,
    reconnect_state: State,
}

/// Why [`HttpSource::reopen_at`] could not hand back a usable body.
enum ReopenFailure {
    Io(Error),
    HttpStatus {
        code: u16,
        retry_after_secs: Option<u64>,
    },
    RangeIgnoredMidStream,
    RedirectedMidStream,
}

impl From<vaco_protocol_core::ProtocolError> for ReopenFailure {
    fn from(e: vaco_protocol_core::ProtocolError) -> Self {
        Self::Io(Error::from(e))
    }
}

impl HttpSource {
    /// Build a source from the already-resolved, already-redirect-free first
    /// response. `requested_start` is the offset `crate::protocol` asked for
    /// in that request, needed to classify a bare `200`.
    ///
    /// # Errors
    /// [`Error::Io`] if the server returned `200` (ignoring `Range`) for a
    /// non-zero `requested_start` — see the module docs.
    pub fn from_first_response(
        target: String,
        credentials: Option<(String, String)>,
        opts: HttpOptions,
        timeout: Option<StdDuration>,
        response: Response<Body>,
        requested_start: u64,
    ) -> CoreResult<Self> {
        let seekable_override = opts.seekable();
        let mut source = Self {
            target,
            credentials,
            opts,
            timeout,
            reader: Box::new(std::io::empty()),
            pos: 0,
            total_size: None,
            seekable: false,
            reconnect_state: State::new(),
        };
        source
            .adopt(response, requested_start, seekable_override)
            .map_err(error_of)?;
        Ok(source)
    }

    /// The logical byte offset the *first* byte this source will yield
    /// actually corresponds to — read from the server's own `Content-Range`
    /// when it supplied one, not assumed from what was requested. The caller
    /// (`crate::protocol`) uses this to start its wrapping `PeekSource` at
    /// the position this source is actually at, not the position it was
    /// asked to reach.
    #[must_use]
    pub const fn start_position(&self) -> u64 {
        self.pos
    }

    /// Classify `response` and, if acceptable, install it as the current
    /// reader. Shared by the initial open and every later reopen.
    fn adopt(
        &mut self,
        response: Response<Body>,
        requested_start: u64,
        seekable_override: crate::options::Seekable,
    ) -> Result<(), ReopenFailure> {
        use crate::options::Seekable;

        let status = response.status().as_u16();
        match status {
            200 if requested_start == 0 => {
                let total = response.body().content_length();
                self.total_size = total;
                self.seekable = matches!(seekable_override, Seekable::Always);
                self.pos = 0;
                self.reader = Box::new(response.into_body().into_reader());
                Ok(())
            }
            200 => Err(ReopenFailure::RangeIgnoredMidStream),
            206 => {
                let content_range = response
                    .headers()
                    .get(ureq::http::header::CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_content_range);
                let start = content_range.map_or(requested_start, |cr| cr.start);
                self.total_size = content_range.and_then(|cr| cr.total);
                self.seekable = !matches!(seekable_override, Seekable::Never);
                self.pos = start;
                self.reader = Box::new(response.into_body().into_reader());
                Ok(())
            }
            300..=399 => Err(ReopenFailure::RedirectedMidStream),
            _ => {
                let retry_after_secs = response
                    .headers()
                    .get(ureq::http::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_retry_after_secs);
                Err(ReopenFailure::HttpStatus {
                    code: status,
                    retry_after_secs,
                })
            }
        }
    }

    fn issue(&self, start: u64) -> Result<Response<Body>, vaco_protocol_core::ProtocolError> {
        let range = if matches!(self.opts.seekable(), crate::options::Seekable::Never) {
            None
        } else {
            let end_exclusive = (self.opts.end_offset > 0).then_some(self.opts.end_offset as u64);
            Some(RequestRange {
                start,
                end_exclusive,
            })
        };
        let creds = self
            .credentials
            .as_ref()
            .map(|(u, p)| (u.as_str(), p.as_str()));
        let hdrs = headers::build(&self.opts, range, creds);
        transport::send("GET", &self.target, &hdrs, self.timeout)
    }

    /// Reopen at `pos`, without following any redirect and without
    /// consulting a whitelist — see the module docs for why that is the
    /// deliberate limit of what a reconnect/seek can do.
    fn reopen_at(&mut self, pos: u64) -> Result<(), ReopenFailure> {
        let response = self.issue(pos)?;
        self.adopt(response, pos, self.opts.seekable())
    }

    /// Attempt a reconnect for `failure`, sleeping first if
    /// [`reconnect::decide`] says to. Returns `Ok(true)` if a new reader is
    /// now installed and the caller should retry its read, `Ok(false)` if
    /// reconnection is not applicable and the caller should treat this as a
    /// clean EOF, and `Err` if reconnection was attempted and exhausted or is
    /// not permitted for this failure and it should be reported.
    ///
    /// An explicit loop, not recursion: `-reconnect_max_retries` accepts
    /// values up to `i32::MAX`, and a persistently failing server must not
    /// turn that into a stack frame per attempt.
    fn try_reconnect(
        &mut self,
        mut failure: Failure,
        mut retry_after_secs: Option<u64>,
        propagate_on_giveup: Option<Error>,
    ) -> CoreResult<bool> {
        let mut giveup_error = propagate_on_giveup;
        loop {
            match reconnect::decide(
                &self.opts,
                &mut self.reconnect_state,
                failure,
                Instant::now(),
                retry_after_secs,
            ) {
                Decision::GiveUp => {
                    return match giveup_error {
                        Some(e) => Err(e),
                        None => Ok(false),
                    };
                }
                Decision::Retry { after } => {
                    std::thread::sleep(after);
                    match self.reopen_at(self.pos) {
                        Ok(()) => {
                            self.reconnect_state.reset();
                            return Ok(true);
                        }
                        Err(f) => {
                            failure = failure_of(&f);
                            retry_after_secs = retry_after_of(&f);
                            giveup_error = Some(error_of(f));
                        }
                    }
                }
            }
        }
    }
}

fn failure_of(f: &ReopenFailure) -> Failure {
    match f {
        ReopenFailure::Io(_) => Failure::StreamDropped,
        ReopenFailure::HttpStatus { code, .. } => Failure::HttpStatus(*code),
        ReopenFailure::RangeIgnoredMidStream | ReopenFailure::RedirectedMidStream => {
            // Neither is in the reference's reconnect vocabulary, and neither
            // is safe to retry blindly (retrying the same request against a
            // server that ignores Range, or a redirect we cannot re-gate,
            // would just repeat the same failure or silently change trust
            // boundaries) — reported as a plain stream failure, which
            // `reconnect_on_network_error`/`reconnect` do not cover unless
            // the caller asked for general `reconnect`.
            Failure::StreamDropped
        }
    }
}

fn retry_after_of(f: &ReopenFailure) -> Option<u64> {
    match f {
        ReopenFailure::HttpStatus {
            retry_after_secs, ..
        } => *retry_after_secs,
        _ => None,
    }
}

fn error_of(f: ReopenFailure) -> Error {
    match f {
        ReopenFailure::Io(e) => e,
        ReopenFailure::HttpStatus { code, .. } => Error::Io(std::io::Error::new(
            ErrorKind::InvalidData,
            format!("http status {code}"),
        )),
        ReopenFailure::RangeIgnoredMidStream => Error::Io(std::io::Error::other(
            "server ignored Range at a non-zero offset; refusing to read from the wrong position",
        )),
        ReopenFailure::RedirectedMidStream => Error::Io(std::io::Error::other(
            "server redirected during a seek/reconnect; cannot re-check the protocol whitelist \
             at this layer",
        )),
    }
}

impl std::fmt::Debug for HttpSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpSource")
            .field("target", &self.target)
            .field("pos", &self.pos)
            .field("total_size", &self.total_size)
            .field("seekable", &self.seekable)
            .finish_non_exhaustive()
    }
}

impl RawSource for HttpSource {
    fn read(&mut self, buf: &mut [u8]) -> CoreResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            match self.reader.read(buf) {
                Ok(0) => {
                    let total_known = self.total_size.is_some();
                    let reached_total = self.total_size.is_some_and(|t| self.pos >= t);
                    if reached_total {
                        return Ok(0);
                    }
                    let progressed =
                        self.try_reconnect(Failure::UnexpectedEof { total_known }, None, None)?;
                    if !progressed {
                        return Ok(0);
                    }
                }
                Ok(n) => {
                    self.pos = self.pos.saturating_add(n as u64);
                    self.reconnect_state.reset();
                    return Ok(n);
                }
                Err(e) => {
                    let err = Error::from(e);
                    let progressed = self.try_reconnect(Failure::StreamDropped, None, Some(err))?;
                    if !progressed {
                        // `try_reconnect` only returns `Ok(false)` when
                        // `propagate_on_giveup` was `None`, which it is not
                        // here — this arm is unreachable but costs nothing to
                        // keep total.
                        return Ok(0);
                    }
                }
            }
        }
    }

    fn seek(&mut self, pos: u64) -> CoreResult<u64> {
        if !self.seekable {
            return Err(Error::NotSeekable);
        }
        if pos == self.pos {
            return Ok(pos);
        }
        // `-short_seek_size`: a small forward seek reads-and-discards on the
        // existing connection instead of opening a new one. Bounded by the
        // option's own value, which is caller-configured, not attacker
        // controlled — the attacker chooses *when* a seek happens, not how
        // large this threshold is.
        let threshold = u64::from(self.opts.short_seek_size.max(0) as u32);
        if pos > self.pos && pos - self.pos <= threshold {
            let mut discard = pos - self.pos;
            let mut scratch = [0_u8; 4096];
            while discard > 0 {
                let want = discard.min(scratch.len() as u64) as usize;
                let Some(chunk) = scratch.get_mut(..want) else {
                    break;
                };
                let n = self.read(chunk)?;
                if n == 0 {
                    break;
                }
                discard = discard.saturating_sub(n as u64);
            }
            if discard == 0 {
                return Ok(self.pos);
            }
            // Ran dry before reaching the target: fall through to a real
            // reopen rather than reporting a wrong position.
        }

        self.reopen_at(pos).map_err(error_of)?;
        self.reconnect_state.reset();
        Ok(self.pos)
    }

    fn size(&self) -> Option<u64> {
        self.total_size
    }

    fn seekability(&self) -> Seekability {
        if self.seekable {
            Seekability::Expensive
        } else {
            Seekability::None
        }
    }
}
