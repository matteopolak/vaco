//! [`HttpSink`]: `Protocol::create`'s chunked POST.
//!
//! # Why the body is buffered here rather than streamed as it is written
//!
//! `MediaSink` (`vaco-io`) has no `close`/`finish` method — only `write`,
//! `seek`, `position`, `is_seekable` and `flush`. An HTTP request, chunked or
//! not, needs exactly one moment where "no more body bytes are coming" is
//! knowable, so the request can be sent and a response read. Without a
//! dedicated finalization hook, [`HttpSink::flush`] is that moment: it sends
//! the entire buffered body as one `ureq` request when first called, using
//! [`ureq::SendBody::from_reader`] (no length hint) so `ureq` sends
//! `Transfer-Encoding: chunked` rather than a `Content-Length` — which is
//! "chunked on the wire", matching `-chunked_post`'s own name, even though
//! this crate buffers the whole body in memory first rather than streaming
//! each [`MediaSink::write`] call straight onto the socket as it happens.
//!
//! A truly incremental version (a background thread reading from a bounded
//! channel that `write` feeds and `ureq`'s body reader drains) was
//! considered and deferred: D5 says v0.1 has zero muxers, so nothing in this
//! project calls [`vaco_protocol_core::Protocol::create`] on this crate yet
//! (same statement `vaco-protocol-http`'s crate docs already made about the
//! read side before this existed), and a background-thread design is
//! meaningfully more moving parts to get right for zero current callers.
//! This is the honest, scoped version; the harder one is real follow-up work,
//! not an oversight.

use std::io::Cursor;
use std::time::Duration as StdDuration;

use vaco_core::{Error, Result as CoreResult};
use vaco_io::MediaSink;

use crate::headers;
use crate::options::HttpOptions;
use crate::transport;

/// An in-progress `POST`. Every byte written is buffered; the request is
/// sent on the first [`MediaSink::flush`] call.
pub struct HttpSink {
    target: String,
    credentials: Option<(String, String)>,
    opts: HttpOptions,
    timeout: Option<StdDuration>,
    buffer: Vec<u8>,
    pos: u64,
    sent: bool,
}

impl HttpSink {
    #[must_use]
    pub const fn new(
        target: String,
        credentials: Option<(String, String)>,
        opts: HttpOptions,
        timeout: Option<StdDuration>,
    ) -> Self {
        Self {
            target,
            credentials,
            opts,
            timeout,
            buffer: Vec::new(),
            pos: 0,
            sent: false,
        }
    }

    fn send_buffered(&mut self) -> CoreResult<()> {
        if self.sent {
            return Ok(());
        }
        self.sent = true;

        if !self.opts.chunked_post {
            // `-chunked_post 0` (a fixed-length `Content-Length` POST) is not
            // implemented — see the crate docs' "What is deliberately not
            // implemented". Reported now rather than silently sending
            // chunked anyway, which would be a wire-format the caller
            // explicitly asked us not to use.
            return Err(Error::Unsupported(
                "http: chunked_post=0 (fixed-length POST) is not implemented",
            ));
        }

        let creds = self
            .credentials
            .as_ref()
            .map(|(u, p)| (u.as_str(), p.as_str()));
        let mut hdrs = headers::build(&self.opts, None, creds);
        if !self.opts.content_type.is_empty() {
            hdrs.push(("Content-Type".to_owned(), self.opts.content_type.clone()));
        }

        let body = std::mem::take(&mut self.buffer);
        let mut reader = Cursor::new(body);
        let response =
            transport::send_body("POST", &self.target, &hdrs, self.timeout, &mut reader)
                .map_err(Error::from)?;

        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(Error::Io(std::io::Error::other(format!(
                "http status {status}"
            ))));
        }
        Ok(())
    }
}

impl std::fmt::Debug for HttpSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpSink")
            .field("target", &self.target)
            .field("pos", &self.pos)
            .field("sent", &self.sent)
            .finish_non_exhaustive()
    }
}

impl MediaSink for HttpSink {
    fn write(&mut self, buf: &[u8]) -> CoreResult<()> {
        if self.sent {
            return Err(Error::Io(std::io::Error::other(
                "HttpSink: cannot write after flush() has sent the request",
            )));
        }
        self.buffer.extend_from_slice(buf);
        self.pos = self.pos.saturating_add(buf.len() as u64);
        Ok(())
    }

    fn seek(&mut self, _pos: u64) -> CoreResult<u64> {
        Err(Error::NotSeekable)
    }

    fn position(&self) -> u64 {
        self.pos
    }

    fn is_seekable(&self) -> bool {
        false
    }

    fn flush(&mut self) -> CoreResult<()> {
        self.send_buffered()
    }
}
