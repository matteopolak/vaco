//! The `md5:` protocol — an output that discards its bytes and reports their
//! MD5 digest when writing finishes.
//!
//! # Not the same thing as `-f md5`
//!
//! `vaco-mux-hash`'s `WholeHashMuxer::md5` is a **muxer** (`-f md5`) and prints
//! `MD5=<hex>\n`. `md5:` is a **protocol** — a URL scheme any muxer can write
//! through — and measured against `ffmpeg 8.1` it prints something different:
//! a bare lower-case hex digest with no `MD5=` label.
//!
//! ```text
//! $ ffmpeg -f lavfi -i testsrc=size=32x32:rate=1:duration=1 -f rawvideo md5:
//! 8fbd8482c70a0669a30408f2219104ba
//! ```
//!
//! `md5:` with an empty `rest` writes the digest to standard output;
//! `md5:some/path` writes it to that path instead, verified with
//! `-f nut "md5:myoutput.md5"` producing a file containing just the hex line.
//!
//! # Security
//!
//! The destination in `md5:rest` is itself an open: a bare path resolves to
//! `file` (rule U1) and is routed through the *same* [`ProtocolEnv`] this
//! protocol was given, one level deeper — so root confinement (rule U2) and
//! the whitelist gate apply to where the digest is written exactly as they
//! would to any other nested open. `default_whitelist` is empty: writing the
//! digest to a scheme the caller has not separately allowed is refused, the
//! same as every other wrapping protocol in this workspace (see
//! `vaco-protocol-wrap`'s crate docs for the measurement this follows).

use vaco_hash::HashAlgo;
use vaco_io::{MediaSink, WriterSink};
use vaco_opts::Dict;
use vaco_protocol_core::{
    IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result, Url,
};

/// A sink that hashes everything written to it and, once, emits the digest to
/// a real destination instead of the bytes themselves.
pub struct Md5Sink {
    dest: Box<dyn MediaSink>,
    /// `None` once the digest has been emitted — see [`Md5Sink::finish`],
    /// which mirrors `vaco_hash`'s own "take, consume once" shape ([its
    /// `RunningHash::finish_hex`] takes `self` by value).
    hasher: Option<vaco_hash::RunningHash>,
    pos: u64,
}

impl std::fmt::Debug for Md5Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Md5Sink")
            .field("pos", &self.pos)
            .field("finished", &self.hasher.is_none())
            .finish_non_exhaustive()
    }
}

impl Md5Sink {
    /// Wrap `dest`, the real destination the digest text will be written to.
    #[must_use]
    pub fn new(dest: Box<dyn MediaSink>) -> Self {
        Self {
            dest,
            // `Md5` is always computable (`vaco-hash` names it first and it
            // has no D10 dependency gap), so this `Option` only exists to let
            // `finish` consume by value; it is never observed as `None` before
            // the first (and only) call.
            hasher: HashAlgo::Md5.running(),
            pos: 0,
        }
    }

    /// Emit the digest, if it has not already been emitted.
    ///
    /// Idempotent by design, the same as `vaco_mux_hash`'s whole-file muxers:
    /// a caller that flushes more than once (an explicit flush, then the
    /// `Drop` backstop) must not write the line twice or panic on the second
    /// try.
    fn finish(&mut self) -> vaco_core::Result<()> {
        let Some(hasher) = self.hasher.take() else {
            return Ok(());
        };
        let mut line = hasher.finish_hex();
        line.push('\n');
        self.dest.write(line.as_bytes())?;
        self.dest.flush()
    }
}

impl MediaSink for Md5Sink {
    fn write(&mut self, buf: &[u8]) -> vaco_core::Result<()> {
        if let Some(h) = self.hasher.as_mut() {
            h.update(buf);
        }
        self.pos = self.pos.saturating_add(buf.len() as u64);
        Ok(())
    }

    fn seek(&mut self, pos: u64) -> vaco_core::Result<u64> {
        let _ = pos;
        Err(vaco_core::Error::NotSeekable)
    }

    fn position(&self) -> u64 {
        self.pos
    }

    fn is_seekable(&self) -> bool {
        // A hash is order-sensitive; accepting a seek and silently continuing
        // to feed bytes in write order would compute a digest that does not
        // match what a seeking muxer thinks it wrote. Refusing is the honest
        // answer, and matches `WriterSink`'s stance for the same reason.
        false
    }

    fn flush(&mut self) -> vaco_core::Result<()> {
        self.finish()
    }
}

impl Drop for Md5Sink {
    fn drop(&mut self) {
        // Best-effort backstop, exactly `IoWriter`'s own rationale: a caller
        // is expected to flush explicitly and check the result, so this exists
        // only to avoid silently emitting nothing on an early-return path.
        let _ = self.finish();
    }
}

/// The `md5:` protocol. Output-only: the reference lists it only under
/// `Output` protocols (`ffmpeg -protocols`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Md5Protocol;

impl Protocol for Md5Protocol {
    fn open(
        &self,
        _url: &Url,
        _flags: IoFlags,
        _opts: &Dict,
        _env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn vaco_io::MediaSource>> {
        // Output-only, per the reference's own `-protocols` listing (`md5` is
        // under `Output` and not `Input`). There is nothing to read: the
        // protocol's whole job is discarding bytes, not producing them.
        Err(ProtocolError::Unsupported {
            scheme: "md5",
            operation: "reading (md5: is an output-only protocol)",
        })
    }

    fn create(
        &self,
        url: &Url,
        _flags: IoFlags,
        _opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSink>> {
        let dest: Box<dyn MediaSink> = if url.rest.is_empty() {
            Box::new(WriterSink::new(std::io::stdout()))
        } else {
            // Same `env`, not a fresh one: `ProtocolRegistry::resolve` is the
            // one place depth increments and the whitelist is checked, and
            // reusing `env` (rather than rebuilding an unrestricted one) is
            // what keeps this a nested open rather than a bypass.
            env.registry
                .create(&url.rest, IoFlags::WRITE, &Dict::new(), env)?
        };
        Ok(Box::new(Md5Sink::new(dest)))
    }
}

/// The registry entry for `md5:`.
pub static MD5_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "md5",
    long_name: "MD5 testing",
    flags: ProtocolFlags {
        network: false,
        // It opens one further URL — its own destination — so a
        // `-protocol_whitelist` preset that recurses needs to know that.
        nested_scheme: true,
        server_capable: false,
    },
    default_whitelist: &[],
    options: None,
    proto: &Md5Protocol,
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
    fn digest_matches_the_well_known_check_value() {
        // The standard MD5 test vector for "abc".
        let shared = vaco_io::SharedDynBuf::new();
        let mut m = Md5Sink::new(Box::new(shared.clone()));
        m.write(b"abc").unwrap();
        m.flush().unwrap();
        let out = shared.snapshot();
        assert_eq!(
            std::str::from_utf8(&out).unwrap().trim_end(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
    }

    #[test]
    fn emits_to_the_named_destination_not_bytes() {
        // `vaco_io::SharedDynBuf` is a `MediaSink` that can still be read back
        // after being boxed elsewhere, which is exactly what `Md5Sink::new`
        // needs a handle to.
        let shared = vaco_io::SharedDynBuf::new();
        let mut m = Md5Sink::new(Box::new(shared.clone()));
        m.write(b"abc").unwrap();
        m.write(b"def").unwrap();
        m.flush().unwrap();

        let out = shared.snapshot();
        let text = std::str::from_utf8(&out).unwrap();
        assert_eq!(text.trim_end(), "e80b5017098950fc58aad83c8c14978e");
        // The bytes written to the sink were never in `out` — only the digest.
        assert!(!out.starts_with(b"abcdef"));
    }

    #[test]
    fn flush_is_idempotent() {
        let shared = vaco_io::SharedDynBuf::new();
        let mut m = Md5Sink::new(Box::new(shared.clone()));
        m.write(b"x").unwrap();
        m.flush().unwrap();
        let first = shared.snapshot();
        m.flush().unwrap();
        assert_eq!(shared.snapshot(), first, "a second flush must not re-emit");
    }

    #[test]
    fn seeking_is_refused() {
        let shared = vaco_io::SharedDynBuf::new();
        let mut m = Md5Sink::new(Box::new(shared));
        assert!(!m.is_seekable());
        assert!(m.seek(0).is_err());
    }

    #[test]
    fn empty_rest_targets_stdout() {
        let r = vaco_protocol_core::ProtocolRegistry::new();
        let cancel = vaco_io::CancelToken::new();
        let env = ProtocolEnv::new(&r, &cancel);
        let url = vaco_protocol_core::split_url("md5:");
        // Only asserts the open succeeds; stdout's own bytes are not ours to
        // capture in a unit test.
        assert!(
            Md5Protocol
                .create(&url, IoFlags::WRITE, &Dict::new(), &env)
                .is_ok()
        );
    }
}
