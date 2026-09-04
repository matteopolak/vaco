//! [`CryptoSource`] — streaming AES-128-CBC decryption over a nested
//! [`MediaSource`].
//!
//! # The "hold one block back" design
//!
//! CBC decryption of block *i* only needs ciphertext block *i* and the
//! previous ciphertext block (or the IV) — it does **not** need to know
//! whether block *i* is the last one. But whether to strip padding does need
//! to know that. So this reader always keeps the most recently decrypted
//! block un-released ([`CryptoSource::held`]) until either another block
//! arrives (release the held one *unpadded* — it was not last) or the inner
//! source reports EOF (release the held one *with* [`crate::cipher::unpad`]
//! applied — it was). Memory use is therefore two blocks (32 bytes) plus
//! whatever the caller's own buffer is, never the whole file. The bound is a
//! `const`, not an option, even though nothing here is attacker-sized.
//!
//! # What is NOT implemented, and why that is an honest line to draw
//!
//! [`MediaSource::seek`] requires the inner source to be both seekable *and*
//! to report [`MediaSource::size`] — CBC decryption is only cheaply
//! addressable when the total block count is known, because the *last*
//! block needs unpadding and an arbitrary seek must know whether it landed on
//! it. Every real caller of `crypto:` opens a `file:` URL, which satisfies
//! both; a seek through a forward-only or size-unknown nested transport
//! returns [`vaco_core::Error::NotSeekable`] rather than guessing.

use vaco_core::{Error, Result};
use vaco_io::{MediaSource, Seekability};

use crate::cipher::{self, BLOCK};
use crate::options::KeyMaterial;

/// Read up to one block from `inner`. `Ok(n)` for `0 < n < BLOCK` means the
/// ciphertext file is not a whole number of blocks — malformed, since every
/// ciphertext this protocol (or the reference) ever writes is block-aligned.
fn read_block(inner: &mut dyn MediaSource, buf: &mut [u8; BLOCK]) -> Result<usize> {
    let mut n = 0;
    while n < BLOCK {
        let Some(rest) = buf.get_mut(n..) else {
            break;
        };
        match inner.read(rest)? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Streaming AES-128-CBC decryption over `inner`.
pub struct CryptoSource {
    inner: Box<dyn MediaSource>,
    key: [u8; BLOCK],
    iv: [u8; BLOCK],
    /// The CBC chain value: the ciphertext block most recently consumed, or
    /// `iv` before the first one.
    chain: [u8; BLOCK],
    /// The most recently decrypted block, not yet known to be the last one.
    held: Option<[u8; BLOCK]>,
    /// Bytes ready to hand to the caller: either a released (non-last) block
    /// in full, or the final block already truncated by [`cipher::unpad`].
    pending: Vec<u8>,
    pending_off: usize,
    /// Set once the inner source has reported EOF and `held` has been
    /// unpadded and moved into `pending`.
    finished: bool,
    pos: u64,
}

impl std::fmt::Debug for CryptoSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `key`/`iv`/`chain`/`held`/`pending` are never printed — see
        // `crate::options`'s module docs on why key material must not reach
        // a log line.
        f.debug_struct("CryptoSource")
            .field("pos", &self.pos)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl CryptoSource {
    /// Wrap `inner`, an already-opened nested [`MediaSource`], for decryption
    /// under `material`.
    #[must_use]
    pub fn new(inner: Box<dyn MediaSource>, material: KeyMaterial) -> Self {
        Self {
            inner,
            key: material.key,
            iv: material.iv,
            chain: material.iv,
            held: None,
            pending: Vec::new(),
            pending_off: 0,
            finished: false,
            pos: 0,
        }
    }

    fn pending_ready(&self) -> bool {
        self.pending_off < self.pending.len()
    }

    /// Pull one more ciphertext block from `inner`, decrypt it, and update
    /// `held`/`pending`/`finished` per the module docs.
    fn advance(&mut self) -> Result<()> {
        let mut raw = [0u8; BLOCK];
        let n = read_block(self.inner.as_mut(), &mut raw)?;
        if n == 0 {
            // True EOF. Release whatever was held, unpadded.
            if let Some(last) = self.held.take() {
                let keep = cipher::unpad(&last);
                self.pending = last.get(..keep).unwrap_or(&[]).to_vec();
                self.pending_off = 0;
            }
            self.finished = true;
            return Ok(());
        }
        if n != BLOCK {
            return Err(Error::InvalidData(
                "crypto: ciphertext is not a whole number of AES blocks",
            ));
        }
        let plain = cipher::decrypt_block(&self.key, &mut self.chain, &raw);
        if let Some(prev) = self.held.replace(plain) {
            self.pending = prev.to_vec();
            self.pending_off = 0;
        }
        Ok(())
    }
}

impl MediaSource for CryptoSource {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        while !self.pending_ready() && !self.finished {
            self.advance()?;
        }
        let Some(available) = self.pending.get(self.pending_off..) else {
            return Ok(0);
        };
        let n = available.len().min(buf.len());
        let (Some(src), Some(dst)) = (available.get(..n), buf.get_mut(..n)) else {
            return Ok(0);
        };
        dst.copy_from_slice(src);
        self.pending_off += n;
        self.pos += n as u64;
        Ok(n)
    }

    fn seek(&mut self, pos: u64) -> Result<u64> {
        // See the module docs: only supported when the nested source is
        // seekable and reports its own size.
        if self.inner.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }
        let Some(total) = self.inner.size() else {
            return Err(Error::NotSeekable);
        };
        if total == 0 || !total.is_multiple_of(BLOCK as u64) {
            return Err(Error::InvalidData(
                "crypto: ciphertext is not a whole number of AES blocks",
            ));
        }
        let total_blocks = total >> cipher::BLOCK_SHIFT;
        let block_index = pos >> cipher::BLOCK_SHIFT;
        let within = usize::try_from(pos & (BLOCK as u64 - 1)).unwrap_or(0);

        if block_index >= total_blocks {
            // Seeking at or past the end: nothing more to read.
            self.finished = true;
            self.held = None;
            self.pending.clear();
            self.pending_off = 0;
            self.pos = pos;
            return Ok(pos);
        }

        // The chain input for `block_index` is the previous ciphertext block,
        // or `iv` for block 0.
        let chain = if block_index == 0 {
            self.iv
        } else {
            self.inner.seek((block_index - 1) * BLOCK as u64)?;
            let mut prev = [0u8; BLOCK];
            if read_block(self.inner.as_mut(), &mut prev)? != BLOCK {
                return Err(Error::InvalidData(
                    "crypto: ciphertext is not a whole number of AES blocks",
                ));
            }
            prev
        };
        self.inner.seek(block_index * BLOCK as u64)?;
        let mut target = [0u8; BLOCK];
        if read_block(self.inner.as_mut(), &mut target)? != BLOCK {
            return Err(Error::InvalidData(
                "crypto: ciphertext is not a whole number of AES blocks",
            ));
        }
        self.chain = chain;
        let plain = cipher::decrypt_block(&self.key, &mut self.chain, &target);

        if block_index + 1 == total_blocks {
            // This is the final block: unpad it now, since we know for
            // certain there is nothing after it.
            let keep = cipher::unpad(&plain);
            self.pending = plain.get(..keep).unwrap_or(&[]).to_vec();
            self.held = None;
            self.finished = true;
        } else {
            self.pending = plain.to_vec();
            self.held = None;
            self.finished = false;
        }
        self.pending_off = within.min(self.pending.len());
        self.pos = pos;
        Ok(pos)
    }

    fn position(&self) -> u64 {
        self.pos
    }

    /// Deliberately `None`, always.
    ///
    /// The true decrypted length is only knowable once the final block has
    /// been decrypted and unpadded — computing it up front would mean
    /// reading (and buffering, or seeking and re-seeking) the tail of the
    /// file from inside a `&self` method, which [`MediaSource::size`]'s
    /// signature does not allow without interior mutability this crate has
    /// no other need for. Whether the reference itself reports an exact,
    /// pre-adjusted `avio_size()` for `crypto:` (some protocols do, by
    /// peeking ahead at open time) is **not measured** — every measurement
    /// in this crate's docs used sequential reads to true EOF, which do not
    /// depend on `size()` at all. `None` is the honest answer for "not
    /// established"; callers must not treat an unpopulated field as a result.
    fn size(&self) -> Option<u64> {
        None
    }

    fn seekability(&self) -> Seekability {
        if self.inner.seekability() == Seekability::None || self.inner.size().is_none() {
            Seekability::None
        } else {
            self.inner.seekability()
        }
    }

    fn peek(&mut self, len: usize) -> Result<&[u8]> {
        while self.pending.len().saturating_sub(self.pending_off) < len && !self.finished {
            self.advance()?;
        }
        Ok(self
            .pending
            .get(self.pending_off..)
            .unwrap_or(&[])
            .get(..len)
            .unwrap_or_else(|| self.pending.get(self.pending_off..).unwrap_or(&[])))
    }
}
