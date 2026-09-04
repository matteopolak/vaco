#![forbid(unsafe_code)]
//! `gopher:` and `gophers:` — RFC 1436 and its TLS-wrapped variant. A request
//! sends `<selector>\r\n` (the type character is consumed) and then transfers
//! raw bytes; `gophers:` uses TLS for the connection.
//!
//! Measured against `ffmpeg 8.1`, item types `5`, `9`, and `s` are accepted;
//! every other RFC 1436 type is rejected after the TCP connection opens but
//! before the selector is sent. `check_type` therefore performs no I/O.
//!
//! Selector parsing consumes exactly the first character of the first path
//! segment. For `gopher://host/some/selector`, the wire request is
//! `/selector\r\n`, not `ome/selector\r\n`; see [`selector::parse`].
//!
//! The measured default whitelists are:
//! ```text
//! gopher:  gopher,tcp
//! gophers: gopher,gophers,tcp,tls
//! ```
//! This grant permits a menu's nested gopher resources. An explicit
//! `-protocol_whitelist gopher` still replaces the default and blocks `tcp`.
//!
//! `create()` sends the selector first; a raw capture of `gopher://host/9/out`
//! produced `/out\r\nhello output data` with muxed input.
//!
//! Because selector exchange is duplex while each protocol method returns one
//! direction, both protocols dial directly and check each used scheme by hand;
//! `gophers:` reuses [`vaco_protocol_tls::connect::{connect_tcp, handshake}`].

pub mod protocol;
pub mod selector;

pub use protocol::{GOPHER_PROTOCOL, GOPHERS_PROTOCOL, GopherProtocol, GophersProtocol};
