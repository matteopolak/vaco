//! A content-addressed local object store.
//!
//! # What it is
//!
//! Every blob is named by the SHA-256 of its own bytes and stored at
//! `objects/<first-2-hex>/<64-hex>` under a root directory. Two callers
//! asking for the same content always get the same path, and a corrupted or
//! truncated write can never be mistaken for the real object, because its
//! hash simply would not match.
//!
//! # Why SHA-256 and not BLAKE3
//!
//! Plan 13 §2.5.2 specifies BLAKE3, but `blake3` is not in
//! `[workspace.dependencies]` and adding a dependency is a reviewed decision
//! (D10) this crate does not make unilaterally. `vaco-hash` already carries a
//! vetted, `sha2`-backed SHA-256 (used today for `-show_data_hash` and
//! `framehash`), so this store uses that instead. Functionally the two are
//! interchangeable for content addressing — both are cryptographic digests
//! with negligible collision probability at this corpus's scale — so this is
//! a documented substitution, not a silent one. Swapping the hash function
//! later is a one-function change ([`Store::hash`]); the on-disk layout
//! already namespaces by algorithm (`objects/sha256/..`) so a future BLAKE3
//! store can live alongside without migrating anything.
//!
//! # Where it lives on disk
//!
//! [`Store::default_root`] honours `VACO_CORPUS_CACHE` first, then falls back
//! to `$HOME/.cache/vaco/corpus` (or `$TMPDIR/vaco-corpus` if `HOME` is
//! unset, which only happens in a stripped test/CI environment). Nothing here
//! ever guesses a network is reachable — see `fetch.rs` for the opt-in gate.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use vaco_hash::HashAlgo;

/// A verified SHA-256 digest, lower-case hex, always 64 characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId(String);

impl ObjectId {
    /// Parse a 64-character lower-case hex string.
    #[must_use]
    pub fn parse(hex: &str) -> Option<Self> {
        let hex = hex.trim();
        if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            Some(Self(hex.to_ascii_lowercase()))
        } else {
            None
        }
    }

    /// Digest `data` directly.
    ///
    /// # Panics
    /// Never in practice: `HashAlgo::Sha256::digest_hex` only returns `None`
    /// for the five named-but-uncomputable algorithms documented on
    /// [`HashAlgo`] (`Murmur3`/`Ripemd*`), none of which this function ever
    /// selects.
    #[must_use]
    pub fn of(data: &[u8]) -> Self {
        // `digest_hex` only returns `None` for the five named-but-uncomputable
        // algorithms (`HashAlgo` module docs); `Sha256` is always computable.
        #[expect(
            clippy::unwrap_used,
            reason = "HashAlgo::Sha256::digest_hex is Some for every input; the None arm is for algorithms this crate never selects"
        )]
        let hex = HashAlgo::Sha256.digest_hex(data).unwrap();
        Self(hex)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Errors from a store operation.
#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    /// The bytes written do not hash to the id they were stored under.
    HashMismatch {
        expected: ObjectId,
        got: ObjectId,
    },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "corpus store I/O error: {e}"),
            Self::HashMismatch { expected, got } => {
                write!(
                    f,
                    "corpus store hash mismatch: expected {expected}, got {got}"
                )
            }
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// A content-addressed store rooted at one directory.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// A store rooted at an arbitrary directory — mainly for tests, which
    /// should never touch the real user cache.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The default root: `$VACO_CORPUS_CACHE`, else `$HOME/.cache/vaco/corpus`,
    /// else `$TMPDIR/vaco-corpus` (or `/tmp/vaco-corpus` if even `TMPDIR` is
    /// unset).
    #[must_use]
    pub fn default_root() -> PathBuf {
        if let Ok(dir) = std::env::var("VACO_CORPUS_CACHE") {
            return PathBuf::from(dir);
        }
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home)
                .join(".cache")
                .join("vaco")
                .join("corpus");
        }
        let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_owned());
        PathBuf::from(tmp).join("vaco-corpus")
    }

    /// A store at [`Store::default_root`].
    #[must_use]
    pub fn open_default() -> Self {
        Self::at(Self::default_root())
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The path an object with this id would live at, whether or not it
    /// exists yet.
    #[must_use]
    pub fn path_for(&self, id: &ObjectId) -> PathBuf {
        let hex = id.as_str();
        // `get(..2)` rather than indexing: `id` is always 64 ASCII hex chars
        // by construction (`ObjectId::parse`/`ObjectId::of`), but a slice
        // index would panic on any future change to that invariant, and a
        // `get` failure degrades to a flat layout instead.
        let prefix = hex.get(..2).unwrap_or("00");
        self.root
            .join("objects")
            .join("sha256")
            .join(prefix)
            .join(hex)
    }

    /// Whether an object is already present.
    #[must_use]
    pub fn has(&self, id: &ObjectId) -> bool {
        self.path_for(id).is_file()
    }

    /// Read a stored object, if present.
    ///
    /// # Errors
    /// Any I/O failure other than "not found", which returns `Ok(None)`.
    pub fn get(&self, id: &ObjectId) -> Result<Option<Vec<u8>>, StoreError> {
        match fs::read(self.path_for(id)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Store `data`, addressed by its own hash. Idempotent: storing the same
    /// bytes twice is a no-op the second time.
    ///
    /// Writes to a sibling temp file first and renames into place, so a
    /// reader can never observe a partially written object.
    ///
    /// # Errors
    /// Any I/O failure creating the object directory or writing the file.
    pub fn put(&self, data: &[u8]) -> Result<ObjectId, StoreError> {
        let id = ObjectId::of(data);
        let final_path = self.path_for(&id);
        if final_path.is_file() {
            return Ok(id);
        }
        let Some(parent) = final_path.parent() else {
            return Err(StoreError::Io(io::Error::other(
                "object path has no parent directory",
            )));
        };
        fs::create_dir_all(parent)?;
        let tmp_path = parent.join(format!(".tmp-{}", id.as_str()));
        fs::write(&tmp_path, data)?;
        fs::rename(&tmp_path, &final_path)?;
        Ok(id)
    }

    /// Store `data` under a claimed id, refusing a mismatch instead of
    /// silently storing under the true hash. This is the entry point
    /// `fetch.rs` uses: a corpus is a security boundary, so a downloaded
    /// blob that does not match its lock-file hash must never be adopted
    /// under any name, not even its own.
    ///
    /// # Errors
    /// [`StoreError::HashMismatch`] if `data` does not hash to `claimed`;
    /// otherwise as [`Store::put`].
    pub fn put_verified(&self, claimed: &ObjectId, data: &[u8]) -> Result<(), StoreError> {
        let actual = ObjectId::of(data);
        if actual != *claimed {
            return Err(StoreError::HashMismatch {
                expected: claimed.clone(),
                got: actual,
            });
        }
        self.put(data)?;
        Ok(())
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::{ObjectId, Store};

    #[test]
    fn parse_rejects_non_hex_and_wrong_length() {
        assert!(ObjectId::parse("not-hex").is_none());
        assert!(ObjectId::parse(&"a".repeat(63)).is_none());
        assert!(ObjectId::parse(&"a".repeat(64)).is_some());
    }

    #[test]
    fn of_matches_a_known_sha256() {
        // sha256("") — the empty-string vector everyone can check by hand.
        let id = ObjectId::of(b"");
        assert_eq!(
            id.as_str(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn put_then_get_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let id = store.put(b"hello corpus").unwrap();
        assert!(store.has(&id));
        assert_eq!(store.get(&id).unwrap(), Some(b"hello corpus".to_vec()));
    }

    #[test]
    fn put_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let a = store.put(b"same bytes").unwrap();
        let b = store.put(b"same bytes").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn put_verified_rejects_a_mismatch_and_stores_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let wrong = ObjectId::of(b"not the real content");
        let err = store.put_verified(&wrong, b"actual content").unwrap_err();
        assert!(matches!(err, super::StoreError::HashMismatch { .. }));
        assert!(!store.has(&wrong));
        assert!(!store.has(&ObjectId::of(b"actual content")));
    }

    #[test]
    fn put_verified_accepts_a_match() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let id = ObjectId::of(b"actual content");
        store.put_verified(&id, b"actual content").unwrap();
        assert!(store.has(&id));
    }

    #[test]
    fn different_ids_live_at_different_paths_sharded_by_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let a = ObjectId::of(b"one");
        let b = ObjectId::of(b"two");
        assert_ne!(store.path_for(&a), store.path_for(&b));
        assert!(
            store
                .path_for(&a)
                .starts_with(dir.path().join("objects").join("sha256"))
        );
    }
}
