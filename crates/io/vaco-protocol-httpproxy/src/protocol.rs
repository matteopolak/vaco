//! [`HttpProxyProtocol`] — the `httpproxy:` scheme, and the registry entry.

use vaco_io::{MediaSink, MediaSource, PeekSource, ReaderSource, WriterSink};
use vaco_opts::Dict;
use vaco_protocol_core::{
    IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolFlags, Result, Url,
};

use crate::connect;

/// The `httpproxy:` protocol.
#[derive(Debug, Clone, Copy, Default)]
pub struct HttpProxyProtocol;

impl Protocol for HttpProxyProtocol {
    fn open(
        &self,
        url: &Url,
        _flags: IoFlags,
        _opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        let target = connect::parse(&url.rest)?;
        let stream = connect::dial(&target, None, env)?;
        Ok(Box::new(PeekSource::new(ReaderSource::new(stream))))
    }

    fn create(
        &self,
        url: &Url,
        _flags: IoFlags,
        _opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        let target = connect::parse(&url.rest)?;
        let stream = connect::dial(&target, None, env)?;
        Ok(Box::new(WriterSink::new(stream)))
    }
}

/// The registry entry for `httpproxy:`.
pub static HTTPPROXY_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "httpproxy",
    long_name: "HTTP CONNECT proxy tunnel",
    // `-protocols` lists `httpproxy` under both `Input:` and `Output:`.
    flags: ProtocolFlags {
        network: true,
        nested_scheme: true,
        server_capable: false,
        readable: true,
        writable: true,
    },
    // Measured empty: `ffmpeg -v debug` prints "No default whitelist set" for
    // httpproxy, and an explicit `-protocol_whitelist httpproxy` alone does
    // not implicitly grant the nested `tcp` open either — see the crate
    // docs.
    default_whitelist: &[],
    // `-h protocol=httpproxy` reports "Unknown protocol" (8.1): no private
    // AVOptions at all, the same shape as `data:`/`md5:`.
    options: None,
    proto: &HttpProxyProtocol,
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
    fn default_whitelist_is_empty() {
        assert!(HTTPPROXY_PROTOCOL.default_whitelist.is_empty());
    }

    #[test]
    fn has_no_option_schema() {
        assert!(HTTPPROXY_PROTOCOL.options.is_none());
    }
}
