#![forbid(unsafe_code)]
//! `httpproxy:` — an HTTP `CONNECT` tunnel to a proxy.
//!
//! # What it is
//!
//! `httpproxy://[user:pass@]proxy-host:proxy-port/target-host:target-port`
//! connects to the proxy, issues `CONNECT target-host:target-port HTTP/1.1`,
//! and — once the proxy answers `2xx` — hands back the raw, now-tunnelled
//! TCP connection as a [`vaco_io::MediaSource`] (`open`) or
//! [`vaco_io::MediaSink`] (`create`). Everything after the handshake is an
//! uninterpreted byte pipe; a caller that wants to speak HTTP *through* the
//! tunnel (the usual case — `http:`/`https:` behind a proxy) does so itself.
//!
//! # Measured against `ffmpeg 8.1`
//!
//! `-h protocol=httpproxy` reports "Unknown protocol" — like `data:`/`md5:`,
//! it has no private `AVOption`s at all, so [`protocol::HTTPPROXY_PROTOCOL`]'s
//! `options` field is `None`.
//!
//! `-protocols` lists `httpproxy` under both `Input:` and `Output:`.
//!
//! The request/response shape (captured against a local loopback listener,
//! since there is no live proxy to test against — see the crate docs for
//! why that is a legitimate substitute here):
//!
//! ```text
//! CONNECT example.com:80 HTTP/1.1\r\n
//! Host: <proxy-host>:<proxy-port>\r\n
//! Connection: close\r\n
//! \r\n
//! ```
//!
//! Two things worth flagging because they are easy to get "obviously" wrong:
//!
//! 1. **`Host:` names the *proxy*, not the tunnel target.** A request line
//!    naming the target twice (once in `CONNECT`, once in `Host:`) is the
//!    more common shape in other HTTP proxy clients; this is not that.
//! 2. **No `Proxy-Authorization` on the first attempt**, even when the URL
//!    carries `user:pass@`. It appears only after a `407` response whose
//!    `Proxy-Authenticate` names `Basic`, on a **second, fresh** TCP
//!    connection (measured: two separate `Starting connection attempt`
//!    lines) carrying `Proxy-Authorization: Basic base64(user:pass)`.
//!
//! # Security
//!
//! [`protocol::HTTPPROXY_PROTOCOL`]'s nested `tcp:` open is **not** routed
//! through [`vaco_protocol_core::ProtocolRegistry`] — like `tls:`
//! (`vaco-protocol-tls`'s crate docs explain this precedent in full), the
//! `CONNECT` handshake is inherently duplex (write the request, read the
//! response) and [`vaco_protocol_core::Protocol::open`]/`create` each return
//! only one direction. [`connect::dial`] calls
//! [`vaco_protocol_core::ProtocolEnv::check_scheme`] with `"tcp"` by hand, at
//! exactly the point [`vaco_protocol_core::ProtocolRegistry::resolve`] would
//! have, so the whitelist property still holds even though the bytes never
//! pass through a registered `tcp:` [`vaco_protocol_core::Protocol`].
//! `default_whitelist` is measured empty (`ffmpeg -v debug`:
//! `[httpproxy @ ...] No default whitelist set`), matching every other
//! nested-opening protocol measured in this workspace so far.

pub mod connect;
pub mod protocol;

pub use protocol::{HTTPPROXY_PROTOCOL, HttpProxyProtocol};
