//! `-h protocol=ipfs` / `-h protocol=ipns`'s option surface, measured against
//! `ffmpeg 8.1` — identical for both schemes:
//!
//! ```text
//! IPFS Gateway AVOptions:
//!   -gateway           <string>     .D......... The gateway to ask for IPFS data.
//! ```
//!
//! `.D.` (decoding only) confirms the direction independently of
//! `-protocols`, which lists both `ipfs` and `ipns` under `Input:` only —
//! there is no `ipfs:`/`ipns:` output in the reference.

use vaco_opts::Options;

/// `-h protocol=ipfs` / `-h protocol=ipns`.
#[derive(Debug, Clone, PartialEq, Options)]
#[options(name = "ipfs", help = "IPFS Gateway")]
pub struct IpfsOptions {
    #[opt(
        name = "gateway",
        help = "The gateway to ask for IPFS data.",
        default = String::new(),
        default_repr = "",
        flags(decoding)
    )]
    pub gateway: String,
}
