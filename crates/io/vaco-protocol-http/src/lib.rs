//! The `http:` and `https:` protocols: ranged reads, whitelist-gated redirects,
//! reconnection, custom headers, cookies, ICY metadata de-interleaving, HTTP
//! Basic auth from URL userinfo, and the `-reconnect*` option family.
//!
//! [`vaco_protocol_core::Protocol`] is implemented over `ureq`. The portable
//! pieces (`options`, `url`, `headers`, `parse`, and `reconnect`) handle values;
//! `transport`, `source`, and `protocol` own sockets, response handling, and
//! redirect dispatch. `Protocol::create` uses [`post::HttpSink`] for buffered,
//! chunked `POST` requests.
//!
//! Redirect targets are resolved here and sent back through
//! [`vaco_protocol_core::ProtocolEnv::check_scheme`], so a server cannot bypass
//! the whitelist. `Icy-MetaData: 1` responses are de-interleaved before bytes
//! reach a demuxer; the latest metadata remains on
//! [`source::HttpSource::icy_metadata`].
//!
//! The crate is native-only because its transport opens sockets. `HttpOptions`
//! mirrors the implemented `-h protocol=http` options and honours
//! [`vaco_protocol_core::ProtocolEnv::rw_timeout`] per request; see the module
//! docs in `options` and `post` for deliberate limits and configuration details.

#![forbid(unsafe_code)]

pub mod headers;
pub mod options;
pub mod parse;
pub mod post;
pub mod protocol;
pub mod reconnect;
pub mod source;
pub mod transport;
pub mod url;

pub use options::HttpOptions;
pub use post::HttpSink;
pub use protocol::{HTTP_PROTOCOL, HTTPS_PROTOCOL, HttpProtocol};
pub use source::HttpSource;

use vaco_protocol_core::ProtocolRegistry;

/// Register `http:` and `https:`.
///
/// `vaco-registry` calls this; so does every test that needs a real (or, per
/// this crate's own tests, a locally-bound) HTTP open.
pub fn register(registry: &mut ProtocolRegistry) {
    registry.register(&HTTP_PROTOCOL);
    registry.register(&HTTPS_PROTOCOL);
}
