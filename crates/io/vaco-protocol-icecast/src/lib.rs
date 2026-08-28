#![forbid(unsafe_code)]
//! `icecast:` — the Icecast/SHOUTcast source-client protocol: `SOURCE` or
//! `PUT` a stream to a mount point, output-only. No RFC; a documented
//! de-facto convention (Icecast's own "Source Client" docs describe the
//! server side of the same handshake), but every detail this crate encodes
//! was independently measured against the reference client's wire behavior
//! rather than read from that documentation — see [`Vaco-Provenance`] in the
//! landing commit.
//!
//! # What it is
//!
//! `icecast://[user[:pass]@]host[:port]/mount` connects (`tcp:`, or `tls:`
//! under `-tls 1`), sends a `SOURCE`/`PUT` request with Icecast's `Ice-*`
//! headers and HTTP Basic auth, and — for the modern (`PUT`) mode — waits
//! for a `100 Continue` before treating the rest of the connection as the
//! stream body. `open()` is unsupported: `-h protocol=icecast` marks every
//! option encoding-only, and `-protocols` lists `icecast` under `Output:`
//! only.
//!
//! # Measured against `ffmpeg 8.1`, using local fake HTTP servers
//!
//! ## Two wire shapes, chosen by `-legacy_icecast`
//!
//! Legacy (`-legacy_icecast 1`): `SOURCE <path> HTTP/1.1`, headers, then the
//! body immediately — no `100-continue` wait, and (per the reference's own
//! debug log) the connection is dialled as plain `tcp:`, not routed through
//! its `http:` protocol at all. Modern (the default): `PUT <path> HTTP/1.1`
//! with `Expect: 100-continue`, and the client genuinely blocks: a fake
//! server that accepts the connection, reads the full header block, and
//! answers nothing receives no body within the capture window. The
//! reference's own debug log shows modern mode routed through its internal
//! `http:`/`https:` protocol (`[http @ ...] Setting default whitelist
//! 'http,https,tls,rtp,tcp,udp,crypto,httpproxy,data'`) — a C implementation
//! detail of *how* it issues the request, not a grant this crate's own
//! `default_whitelist` needs to mirror (see below).
//!
//! ## Default port is `80` (`443` under `-tls 1`), not the conventional
//! Icecast port `8000`
//!
//! ```text
//! $ ffmpeg -v debug -f lavfi -i sine -f mp3 icecast://h/mount
//! [tcp @ ...] Address <h> port 80
//!
//! $ ffmpeg -v debug -f lavfi -i sine -f mp3 -tls 1 icecast://h/mount
//! [tcp @ ...] Address <h> port 443
//! ```
//!
//! Consistent with modern mode's internal routing through `http:`/`https:`:
//! it inherits *that* protocol's default port. This would not have been
//! guessed from the scheme's name or from Icecast server conventions.
//!
//! ## Exact header order and omission rule
//!
//! Captured verbatim with every optional field set:
//!
//! ```text
//! PUT /mystream.mp3 HTTP/1.1
//! User-Agent: MyAgent/1.0
//! Accept: */*
//! Expect: 100-continue
//! Connection: close
//! Host: 127.0.0.1:19502
//! Content-Type: audio/mpeg
//! Icy-MetaData: 1
//! Ice-Name: MyStream
//! Ice-Description: A test stream
//! Ice-URL: http://example.com
//! Ice-Genre: Rock
//! Ice-Public: 1
//! Authorization: Basic c291cmNlOmhhY2ttZQ==
//! ```
//!
//! `Expect` is omitted in legacy mode; every other line and its position is
//! identical between the two modes. `Ice-Name`/`Ice-Description`/`Ice-URL`/
//! `Ice-Genre` are each omitted **entirely** (not sent empty) when their
//! option is unset — measured by unsetting one at a time and confirming only
//! that one line vanishes. `Ice-Public` and `Icy-MetaData` are always
//! present regardless of options.
//!
//! ## Auth
//!
//! URL userinfo overrides `-password` outright — measured via the
//! reference's own debug line, `Overwriting -password <pass> with URI
//! password!`, logged when both are given. The username defaults to the
//! literal `source` when the URL has no userinfo — measured by
//! base64-decoding the `Authorization` header in that case.
//!
//! ## `default_whitelist` is empty
//!
//! `[icecast @ ...] No default whitelist set` — the same shape as
//! `crypto:`/`tls:`/`httpproxy:`/`ftp:` in this workspace. A caller still
//! needs an explicit `tcp`/`tls` grant.
//!
//! # Security
//!
//! The `SOURCE`/`PUT` handshake is inherently duplex (write headers, then —
//! for modern mode — read a `100 Continue` before the body), which
//! `Protocol::create`'s one-direction return type cannot express, so, like
//! `tls:`/`httpproxy:`/`ftp:`/`gopher:` in this workspace, the connection is
//! dialled directly and `env.check_scheme` is called by hand (`"tcp"`, or
//! `"tls"` then `"tcp"` under `-tls 1`, reusing
//! `vaco_protocol_tls::connect::{connect_tcp, handshake}`) rather than going
//! through the registry.

pub mod options;
pub mod protocol;
pub mod request;

pub use protocol::{ICECAST_PROTOCOL, IcecastProtocol};
