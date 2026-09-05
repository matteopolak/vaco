#![forbid(unsafe_code)]
//! `icecast:` — the Icecast/SHOUTcast source-client protocol: `SOURCE` or
//! `PUT` a stream to a mount point, output-only. No RFC; a documented
//! de-facto convention (Icecast's own "Source Client" docs describe the
//! server side of the same handshake), but every detail this crate encodes
//! was independently measured against the reference client's wire behavior
//! rather than read from that documentation — see [`Vaco-Provenance`] in the
//! landing commit.
//!
//! `icecast://[user[:pass]@]host[:port]/mount` is output-only. It sends an
//! Icecast `SOURCE`/`PUT` request over `tcp:` (or `tls:` under `-tls 1`) and,
//! in modern mode, waits for `100 Continue` before streaming the body.
//!
//! Measurements against `ffmpeg 8.1` with local fake HTTP servers established
//! the compatibility contract: legacy mode sends `SOURCE` and no wait; modern
//! mode sends `PUT` with `Expect: 100-continue` and blocks until the response.
//!
//! The measured default ports are 80 and 443 under `-tls 1`, rather than the
//! conventional Icecast port 8000.
//!
//! Header order matches the reference; `Expect` is omitted only in legacy
//! mode. Optional `Ice-Name`, `Ice-Description`, `Ice-URL`, and `Ice-Genre`
//! lines disappear entirely when unset, while `Ice-Public` and `Icy-MetaData`
//! remain present. The exact captured block is documented in [`request`].
//!
//! URL userinfo overrides `-password`, and the measured default username is
//! `source`. The reference reports no default whitelist, so callers must grant
//! `tcp` or `tls` explicitly.
//!
//! The duplex handshake cannot use [`Protocol::create`]'s one-direction return
//! type, so the implementation dials directly and calls `env.check_scheme` for
//! `tcp`, or `tls` then `tcp` under `-tls 1`, reusing the TLS connector.

pub mod options;
pub mod protocol;
pub mod request;

pub use protocol::{ICECAST_PROTOCOL, IcecastProtocol};
