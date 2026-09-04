#![forbid(unsafe_code)]
//! `ipfs:`/`ipns:` fetch content- or name-addressed data through an HTTP
//! gateway. They are input-only and use the ordinary `http:`/`https:` protocol
//! through the same [`vaco_protocol_core::ProtocolEnv`].
//!
//! Measured against `ffmpeg 8.1`, gateway precedence is `-gateway`,
//! `$IPFS_GATEWAY`, the `gateway` file under `$IPFS_PATH`, then the one under
//! `$HOME/.ipfs`.
//! The `$IPFS_PATH` lookup reproduces the reference quirk of concatenating
//! `gateway` without a separator; a trailing slash is required. Resolved
//! gateway values have trailing whitespace and `/` removed.
//!
//! `ipfs://` without a CID fails before gateway discovery. A capture for
//! `ipfs://QmCid/video.mp4` rewrites the request to:
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
//! `ipns:` has the same shape with `/ipns/`. Both descriptors are measured
//! with an empty default whitelist and a decoding-only `-gateway` option.

pub mod gateway;
pub mod options;
pub mod protocol;

pub use protocol::{IPFS_PROTOCOL, IPNS_PROTOCOL, IpfsProtocol, IpnsProtocol};
