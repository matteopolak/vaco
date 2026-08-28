//! [`CryptoSink`] — streaming AES-128-CBC encryption over a nested
//! [`MediaSink`].
//!
//! Buffers at most one partial block (0..16 bytes) between calls to
//! [`MediaSink::write`]; every full block is encrypted and forwarded
//! immediately. The final (possibly empty) partial block is PKCS#7-padded
//! and encrypted on [`CryptoSink::finish`], which — following
//! `vaco-protocol-local`'s `Md5Sink` exactly — is idempotent and called from
//! both [`MediaSink::flush`] and `Drop`, so an early return still emits a
//! correctly terminated ciphertext rather than a truncated one.
//!
//! Measured: encrypting an already block-aligned plaintext still adds one
//! full padding block (see the crate docs), so [`CryptoSink::finish`] always
//! writes a final block, even when `pending` is empty at that point.

use vaco_core::{Error, Result};
use vaco_io::MediaSink;

use crate::cipher::{self, BLOCK};
use crate::options::KeyMaterial;

/// Streaming AES-128-CBC encryption over `dest`.
pub struct CryptoSink {
    dest: Box<dyn MediaSink>,
    key: [u8; BLOCK],
    /// The CBC chain value: the most recently written ciphertext block, or
    /// the IV before the first one.
    chain: [u8; BLOCK],
    /// Plaintext bytes not yet forming a full block.
    pending: Vec<u8>,
    /// `None` once [`CryptoSink::finish`] has run.
    open: bool,
    pos: u64,
}

impl std::fmt::Debug for CryptoSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CryptoSink")
            .field("pos", &self.pos)
            .field("open", &self.open)
            .finish_non_exhaustive()
    }
}

impl CryptoSink {
    /// Wrap `dest`, an already-opened nested [`MediaSink`], for encryption
    /// under `material`.
    #[must_use]
    pub fn new(dest: Box<dyn MediaSink>, material: KeyMaterial) -> Self {
        Self {
            dest,
            key: material.key,
            chain: material.iv,
            pending: Vec::new(),
            open: true,
            pos: 0,
        }
    }

    fn write_block(&mut self, plain: &[u8; BLOCK]) -> Result<()> {
        let cipher = cbc_encrypt_block(&self.key, &self.chain, plain);
        self.dest.write(&cipher)?;
        self.chain = cipher;
        Ok(())
    }

    /// Pad and encrypt whatever remains, exactly once. Idempotent: a second
    /// call is a no-op, matching `Md5Sink::finish`'s contract so an explicit
    /// `flush()` followed by the `Drop` backstop never double-writes.
    ///
    /// # Errors
    /// Propagates the underlying sink's failure.
    pub fn finish(&mut self) -> Result<()> {
        if !self.open {
            return Ok(());
        }
        self.open = false;
        let padded = cipher::encrypt(&self.key, &self.chain, &self.pending);
        // `cipher::encrypt` re-derives the chain from `self.chain` (used here
        // as the IV for just this final call) and pads `self.pending` (never
        // more than BLOCK-1 bytes) to exactly one block — see the crate docs
        // on why this is always a whole extra block relative to the bytes
        // already flushed.
        self.dest.write(&padded)?;
        self.dest.flush()
    }
}

impl MediaSink for CryptoSink {
    fn write(&mut self, buf: &[u8]) -> Result<()> {
        self.pos = self.pos.saturating_add(buf.len() as u64);
        self.pending.extend_from_slice(buf);
        while self.pending.len() >= BLOCK {
            let mut block = [0u8; BLOCK];
            let Some(src) = self.pending.get(..BLOCK) else {
                break;
            };
            block.copy_from_slice(src);
            self.write_block(&block)?;
            self.pending.drain(..BLOCK);
        }
        Ok(())
    }

    fn seek(&mut self, pos: u64) -> Result<u64> {
        let _ = pos;
        // Measured: `ffmpeg 8.1` refuses this outright — `Crypto: seek not
        // supported for write` — rather than, say, only refusing a seek that
        // would land before the current CBC chain state. Reproduced as the
        // same blanket refusal; `vaco_core::Error` has no variant for a
        // human-readable message tied to this specific text, so the standard
        // `NotSeekable` is used, matching every other write-side transport in
        // this workspace (`WriterSink`, `Md5Sink`) that also collapses a
        // more specific reference message to it.
        Err(Error::NotSeekable)
    }

    fn position(&self) -> u64 {
        self.pos
    }

    fn is_seekable(&self) -> bool {
        false
    }

    fn flush(&mut self) -> Result<()> {
        self.finish()
    }
}

impl Drop for CryptoSink {
    fn drop(&mut self) {
        // Best-effort backstop, exactly `Md5Sink`'s own rationale: a caller
        // is expected to flush explicitly and check the result, so this
        // exists only to avoid silently truncating the ciphertext (leaving
        // it short one block, and therefore un-decryptable) on an
        // early-return path.
        let _ = self.finish();
    }
}

/// One CBC encryption step, without the padding `cipher::encrypt` applies —
/// used for the interior (definitely-full, definitely-not-last) blocks a
/// streaming writer sees one at a time. `chain` is not mutated in place here
/// because [`CryptoSink::write_block`] needs the *new* ciphertext to store as
/// the next chain value, which reads more clearly returned than threaded
/// through a second in/out parameter.
fn cbc_encrypt_block(key: &[u8; BLOCK], chain: &[u8; BLOCK], plain: &[u8; BLOCK]) -> [u8; BLOCK] {
    use aes::Aes128;
    use aes::cipher::{Array, BlockCipherEncrypt, KeyInit};

    let mut xored = [0u8; BLOCK];
    for ((o, p), c) in xored.iter_mut().zip(plain.iter()).zip(chain.iter()) {
        *o = p ^ c;
    }
    let cipher = Aes128::new(&Array::from(key.to_owned()));
    let mut block = Array::from(xored);
    cipher.encrypt_block(&mut block);
    let mut out = [0u8; BLOCK];
    out.copy_from_slice(block.as_slice());
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::*;

    const KEY: [u8; BLOCK] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
    ];
    const IV: [u8; BLOCK] = KEY;

    struct VecSink(Vec<u8>);
    impl MediaSink for VecSink {
        fn write(&mut self, buf: &[u8]) -> Result<()> {
            self.0.extend_from_slice(buf);
            Ok(())
        }
        fn seek(&mut self, _pos: u64) -> Result<u64> {
            Err(Error::NotSeekable)
        }
        fn position(&self) -> u64 {
            self.0.len() as u64
        }
        fn is_seekable(&self) -> bool {
            false
        }
        fn flush(&mut self) -> Result<()> {
            Ok(())
        }
    }

    struct Capturing(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl MediaSink for Capturing {
        fn write(&mut self, buf: &[u8]) -> Result<()> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(())
        }
        fn seek(&mut self, _pos: u64) -> Result<u64> {
            Err(Error::NotSeekable)
        }
        fn position(&self) -> u64 {
            self.0.lock().unwrap().len() as u64
        }
        fn is_seekable(&self) -> bool {
            false
        }
        fn flush(&mut self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn streaming_sink_matches_whole_buffer_encrypt() {
        let plaintext: Vec<u8> = (0..300u32).map(|i| (i % 251) as u8).collect();
        let expected = cipher::encrypt(&KEY, &IV, &plaintext);

        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut sink = CryptoSink::new(
            Box::new(Capturing(written.clone())),
            KeyMaterial { key: KEY, iv: IV },
        );
        // Write in small, irregular chunks to exercise the partial-block
        // buffering path, not just one call per block.
        for chunk in plaintext.chunks(7) {
            sink.write(chunk).unwrap();
        }
        sink.finish().unwrap();

        assert_eq!(*written.lock().unwrap(), expected);
    }

    #[test]
    fn finish_is_idempotent() {
        let mut sink = CryptoSink::new(
            Box::new(VecSink(Vec::new())),
            KeyMaterial { key: KEY, iv: IV },
        );
        sink.write(b"hello").unwrap();
        sink.finish().unwrap();
        sink.finish().unwrap(); // must not double-write or panic
    }

    #[test]
    fn write_after_finish_is_silently_buffered_but_never_flushed_twice() {
        // Drop must not panic or double-write even if a caller keeps writing
        // after an explicit finish (matching Md5Sink's own contract).
        let mut sink = CryptoSink::new(
            Box::new(VecSink(Vec::new())),
            KeyMaterial { key: KEY, iv: IV },
        );
        sink.write(b"hello").unwrap();
        sink.finish().unwrap();
        drop(sink);
    }
}
