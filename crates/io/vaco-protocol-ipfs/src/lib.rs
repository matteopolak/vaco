#![forbid(unsafe_code)]
//! `ipfs:`/`ipns:` — fetch content-addressed (`ipfs:`) or name-addressed
//! (`ipns:`) IPFS data through an HTTP gateway. Input-only; no RFC (IPFS has
//! a specification of its own, but nothing this crate encodes was read from
//! it — every detail below was measured against the reference client's wire
//! behavior, clean-room per D6/D7/D17).
//!
//! # What it is
//!
//! `ipfs://<CID>/path` (or `ipns://<name>/path`) resolves a gateway — from
//! `-gateway`, `$IPFS_GATEWAY`, a `gateway` file under `$IPFS_PATH`, or one
//! under `$HOME/.ipfs`, in that order — and opens
//! `<gateway>/ipfs/<CID>/path` (or `/ipns/...`) through the ordinary
//! `http:`/`https:` protocol, one level deeper through the same
//! [`vaco_protocol_core::ProtocolEnv`]. There is no duplex handshake here
//! (unlike `httpproxy:`/`ftp:`/`gopher:`/`icecast:` in this workspace): this
//! is a plain nested open, the same shape as `vaco-protocol-local`'s `md5:`.
//!
//! # Measured against `ffmpeg 8.1`
//!
//! ## Gateway precedence and a genuine reference quirk
//!
//! `-gateway` wins outright; then `$IPFS_GATEWAY`; then a `gateway` file
//! under `$IPFS_PATH`; then one under `$HOME/.ipfs`. All four are confirmed
//! by the reference's own numbered help text and by its debug log skipping
//! straight past an unset source with no attempt to use it.
//!
//! The `$IPFS_PATH`-based lookup has a real bug this crate reproduces
//! faithfully rather than fixing: the reference concatenates `$IPFS_PATH`
//! with the literal string `gateway`, **inserting no path separator** —
//! `IPFS_PATH=/tmp/fake_ipfs` (no trailing slash) produces `The IPFS gateway
//! file (full uri: /tmp/fake_ipfsgateway) doesn't exist` in the reference's
//! own debug output. Only a `$IPFS_PATH` that already ends in `/` finds its
//! file. The `$HOME/.ipfs` fallback does **not** have this bug (the
//! reference builds that particular path itself, with a separator) — see
//! [`gateway::ipfs_path_gateway_file`] vs [`gateway::home_gateway_file`].
//!
//! A trailing `/` on a resolved gateway (from any of the four sources) is
//! stripped before use, and a gateway file's trailing whitespace/newline is
//! trimmed.
//!
//! ## A CID is required before gateway discovery even starts
//!
//! `ipfs://` with an empty path (no CID) fails immediately with `A CID must
//! be provided.` — measured with *no gateway configured at all*, and the
//! reference's debug log shows no `$IPFS_GATEWAY is empty.` line at all in
//! that case, meaning the CID check happens strictly before gateway
//! discovery is even attempted. This crate's `open_generic` checks the same
//! way, in the same order.
//!
//! ## Wire shape
//!
//! A raw-byte capture against a local fake HTTP server, gateway
//! `http://127.0.0.1:PORT`, url `ipfs://QmCid/video.mp4`:
//!
//! ```text
//! GET /ipfs/QmCid/video.mp4 HTTP/1.1
//! User-Agent: Lavf/<version>
//! Accept: */*
//! Range: bytes=0-
//! Connection: close
//! Host: 127.0.0.1:PORT
//! Icy-MetaData: 1
//! ```
//!
//! — exactly `vaco-protocol-http`'s own default GET request, confirming this
//! protocol does nothing more than rewrite the URL and hand it to `http:`/
//! `https:`. `ipns:` produces the identical shape with `/ipns/` instead of
//! `/ipfs/`.
//!
//! ## Direction, options, and `default_whitelist`
//!
//! `-protocols` lists `ipfs`/`ipns` under `Input:` only. `-h protocol=ipfs`
//! and `-h protocol=ipns` report an identical single option, `-gateway
//! <string>` (`.D.`, decoding only) — confirming the direction
//! independently. `default_whitelist` is measured empty for both
//! (`[ipfs @ ...] No default whitelist set`), the same shape as
//! `crypto:`/`tls:`/`httpproxy:`/`ftp:` in this workspace; the reference's
//! *internal* nested `http:` open carries its own separate grant, which is a
//! C implementation detail of the fetch, not something this crate's own
//! descriptor needs to mirror (same reasoning as `icecast:`'s docs).

pub mod gateway;
pub mod options;
pub mod protocol;

pub use protocol::{IPFS_PROTOCOL, IPNS_PROTOCOL, IpfsProtocol, IpnsProtocol};
