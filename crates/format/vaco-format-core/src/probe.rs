//! Format detection.
//!
//! Every registered demuxer is asked to score a bounded prefix of the input and
//! the highest score wins. That is the whole model, and it is unusually
//! testable: `ffprobe` prints the winning score as `probe_score`, so the *exact
//! numbers* are part of the byte-identical output contract (D5) rather than
//! internal folklore.
//!
//! # Measured, not assumed
//!
//! Two rules `planning/18-formats.md` marked as needing verification were
//! settled against the pinned reference (8.1) rather than guessed:
//!
//! * **A forced format reports `probe_score` 0, not 100.** `ffprobe -f matroska
//!   a.mkv` prints `0`; the plan's R8 predicted `MAX`. [`Probe::force`]
//!   reproduces the measured behaviour.
//! * **A MIME type never rescues a zero content score.** A Matroska file served
//!   over HTTP with `Content-Type: video/x-matroska` and its EBML magic
//!   corrupted fails to open, so the bonus in R3 lifts a non-zero score and
//!   nothing else.
//!
//! Observed scores for calibration, same reference: MP4 100, Matroska 100,
//! WAV 99, raw H.264 51, MPEG-TS 50. The score space really is used across its
//! whole range, which is why [`ProbeScore`] carries a convention table rather
//! than three constants.
//!
//! # Totality
//!
//! Everything here runs on attacker-chosen bytes before anything has been
//! validated, so every accessor is total: [`ProbeData::get`] and its typed
//! relatives return `Option`, never panic and never read out of range. The
//! `format_probe` fuzz target drives the whole engine over arbitrary input.

use vaco_core::{Error, Result};
use vaco_io::IoContext;

use crate::DemuxerDesc;
use crate::options::{FormatOptions, PROBE_BUF_MIN};

/// A prefix of the input, plus whatever the caller knows about its origin.
#[derive(Debug, Clone, Copy)]
pub struct ProbeData<'a> {
    pub buf: &'a [u8],
    pub filename: Option<&'a str>,
    pub mime_type: Option<&'a str>,
}

impl<'a> ProbeData<'a> {
    /// Zero bytes readable past the end of `buf`.
    ///
    /// Reproduced deliberately. The reference appends a zero-filled tail to its
    /// probe buffer so a probe function can read a fixed-size header without
    /// bounds checks. We do not need that for *safety* — nothing here can read
    /// out of range — but we need it for *fidelity*: on a six-byte file a probe
    /// that reads a sixteen-byte header sees ten zeros there and would see a
    /// short read here, producing a different score, a different chosen format
    /// and a different `probe_score` line.
    pub const PADDING: usize = 32;

    /// Probe data over `buf` with no filename and no MIME type.
    #[must_use]
    pub const fn new(buf: &'a [u8]) -> Self {
        Self {
            buf,
            filename: None,
            mime_type: None,
        }
    }

    /// Attach the source's filename, for extension matching.
    #[must_use]
    pub const fn with_filename(mut self, filename: &'a str) -> Self {
        self.filename = Some(filename);
        self
    }

    /// Attach a transport-supplied MIME type.
    ///
    /// Only ever set by a protocol that has one — an HTTP `Content-Type` — and
    /// never from a local file.
    #[must_use]
    pub const fn with_mime_type(mut self, mime: &'a str) -> Self {
        self.mime_type = Some(mime);
        self
    }

    /// Bytes actually read. Does **not** include [`Self::PADDING`].
    #[must_use]
    pub const fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether no bytes were read at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Byte at `i`; `0` for `len() <= i < len() + PADDING`; `None` beyond that.
    #[must_use]
    pub fn get(&self, i: usize) -> Option<u8> {
        match self.buf.get(i) {
            Some(&b) => Some(b),
            None if i < self.len().saturating_add(Self::PADDING) => Some(0),
            None => None,
        }
    }

    /// `n` bytes from `at`, into a caller-provided array, honouring the
    /// padding. `None` if the range runs past the padded end.
    fn read<const N: usize>(&self, at: usize) -> Option<[u8; N]> {
        let mut out = [0u8; N];
        for (k, slot) in out.iter_mut().enumerate() {
            *slot = self.get(at.checked_add(k)?)?;
        }
        Some(out)
    }

    /// The four-character code at `at`.
    #[must_use]
    pub fn tag(&self, at: usize) -> Option<[u8; 4]> {
        self.read::<4>(at)
    }

    /// Big-endian `u16` at `at`.
    #[must_use]
    pub fn rb16(&self, at: usize) -> Option<u16> {
        self.read::<2>(at).map(u16::from_be_bytes)
    }

    /// Little-endian `u16` at `at`.
    #[must_use]
    pub fn rl16(&self, at: usize) -> Option<u16> {
        self.read::<2>(at).map(u16::from_le_bytes)
    }

    /// Big-endian `u32` at `at`.
    #[must_use]
    pub fn rb32(&self, at: usize) -> Option<u32> {
        self.read::<4>(at).map(u32::from_be_bytes)
    }

    /// Little-endian `u32` at `at`.
    #[must_use]
    pub fn rl32(&self, at: usize) -> Option<u32> {
        self.read::<4>(at).map(u32::from_le_bytes)
    }

    /// Big-endian `u64` at `at`.
    #[must_use]
    pub fn rb64(&self, at: usize) -> Option<u64> {
        self.read::<8>(at).map(u64::from_be_bytes)
    }

    /// Little-endian `u64` at `at`.
    #[must_use]
    pub fn rl64(&self, at: usize) -> Option<u64> {
        self.read::<8>(at).map(u64::from_le_bytes)
    }

    /// Whether `magic` appears at `at`, honouring the padding.
    #[must_use]
    pub fn matches_at(&self, at: usize, magic: &[u8]) -> bool {
        magic
            .iter()
            .enumerate()
            .all(|(k, &b)| at.checked_add(k).and_then(|i| self.get(i)) == Some(b))
    }

    /// Whether the buffer begins with `magic`.
    #[must_use]
    pub fn starts_with(&self, magic: &[u8]) -> bool {
        self.matches_at(0, magic)
    }

    /// The first offset at or after `from`, and at most `limit`, where `magic`
    /// appears. Searches only real bytes, never the padding.
    #[must_use]
    pub fn find(&self, magic: &[u8], from: usize, limit: usize) -> Option<usize> {
        if magic.is_empty() {
            return None;
        }
        let end = limit.min(self.len());
        let last = end.checked_sub(magic.len())?;
        (from..=last).find(|&i| self.matches_at(i, magic))
    }

    /// The filename's extension, lower-cased and without the dot.
    ///
    /// Only the last component is considered, so a directory containing a dot
    /// cannot supply an extension to a file that has none.
    #[must_use]
    pub fn extension(&self) -> Option<String> {
        let name = self.filename?;
        let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
        let (_, ext) = base.rsplit_once('.')?;
        if ext.is_empty() {
            return None;
        }
        Some(ext.to_ascii_lowercase())
    }

    /// Whether the filename's extension is one of `list`.
    #[must_use]
    pub fn extension_matches(&self, list: &[&str]) -> bool {
        self.extension()
            .is_some_and(|e| list.iter().any(|c| c.eq_ignore_ascii_case(&e)))
    }
}

/// How confident a demuxer is that the input is its format.
///
/// `ffprobe` reports this value as `probe_score`, so it is part of the
/// byte-identical output contract (D5) and its exact scale matters, not just
/// its ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct ProbeScore(pub u8);

impl ProbeScore {
    pub const NONE: Self = Self(0);
    /// Filename extension matched, content did not confirm.
    pub const EXTENSION: Self = Self(50);
    /// Content matched with some ambiguity remaining.
    pub const CONTENT: Self = Self(75);
    /// An unambiguous signature was found.
    pub const MAX: Self = Self(100);

    /// At or below this, the engine reads more input and probes again (R7).
    pub const RETRY: Self = Self(25);
    /// The retry threshold for a probe that has already consumed a stream's
    /// worth of evidence and still wants more.
    pub const STREAM_RETRY: Self = Self(24);
    /// Added to a non-zero score whose descriptor claims the transport's MIME
    /// type.
    pub const MIME_BONUS: u8 = 30;

    // ---------------------------------------------------- convention table
    //
    // Published so that 368 independently written probe functions do not drift.
    // A test in each format crate is expected to assert that its probe only
    // ever returns a value from this table.

    /// Unambiguous magic at a fixed offset, plus a self-consistency check.
    pub const MAGIC_CHECKED: Self = Self::MAX;
    /// Unambiguous magic at a fixed offset, nothing further checked.
    pub const MAGIC: Self = Self(90);
    /// Magic found at a variable offset and internally consistent.
    pub const VARIABLE_OFFSET: Self = Self::CONTENT;

    /// `n` consecutive well-formed frames or packets: `min(100, 25 + 8n)`.
    ///
    /// The shape a self-synchronising format uses — MPEG-TS, MP3, ADTS. One
    /// frame is inside the retry band, two escape it, ten are conclusive.
    #[must_use]
    pub const fn repeating(n: u32) -> Self {
        let raw = 25u32.saturating_add(n.saturating_mul(8));
        Self(if raw > 100 { 100 } else { raw as u8 })
    }

    /// A plausible header with no magic: clamped into the 5..=25 retry band, so
    /// a weak guess can never outrank a real signature.
    #[must_use]
    pub const fn weak(raw: u8) -> Self {
        Self(if raw < 5 {
            5
        } else if raw > 25 {
            25
        } else {
            raw
        })
    }

    /// [`Self::EXTENSION`] when the filename's extension is one of `list`, else
    /// [`Self::NONE`].
    ///
    /// The frozen [`DemuxerDesc`] gives every demuxer a probe function, so the
    /// engine cannot tell "has no probe" from "probed and found nothing" and
    /// therefore awards no automatic extension bonus. A format that genuinely
    /// identifies itself by extension calls this from its own probe, which is
    /// both explicit and reviewable.
    #[must_use]
    pub fn from_extension(data: &ProbeData<'_>, list: &[&str]) -> Self {
        if data.extension_matches(list) {
            Self::EXTENSION
        } else {
            Self::NONE
        }
    }

    /// Whether this score can never win (R5).
    #[must_use]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }

    /// Whether the engine should read more and try again (R7).
    #[must_use]
    pub const fn needs_retry(self) -> bool {
        self.0 <= Self::RETRY.0
    }

    /// The raw value, as `probe_score` prints it.
    #[must_use]
    pub const fn value(self) -> u8 {
        self.0
    }

    /// Add, saturating at [`Self::MAX`].
    #[must_use]
    pub const fn saturating_add(self, n: u8) -> Self {
        let raw = self.0.saturating_add(n);
        Self(if raw > 100 { 100 } else { raw })
    }
}

impl core::fmt::Display for ProbeScore {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A demuxer and the score it earned.
#[derive(Debug, Clone, Copy)]
pub struct Scored<'a> {
    pub desc: &'a DemuxerDesc,
    pub score: ProbeScore,
}

/// The engine's verdict.
#[derive(Debug, Clone, Copy)]
pub struct Detected<'a> {
    pub desc: &'a DemuxerDesc,
    pub score: ProbeScore,
    /// Bytes the winning probe was shown. Zero for a forced format, which is
    /// never probed at all.
    pub probed_bytes: usize,
}

/// The scoring engine over a fixed candidate set.
#[derive(Debug, Clone, Copy)]
pub struct Probe<'a> {
    candidates: &'a [&'a DemuxerDesc],
    options: &'a FormatOptions,
}

impl<'a> Probe<'a> {
    /// Probe `candidates`, filtered and bounded by `options`.
    #[must_use]
    pub const fn new(candidates: &'a [&'a DemuxerDesc], options: &'a FormatOptions) -> Self {
        Self {
            candidates,
            options,
        }
    }

    /// The candidates that survive `format_whitelist` (R9).
    ///
    /// Filtering happens *before* any probe runs, so a whitelisted-out format
    /// never executes its parser on hostile bytes. That is the point of the
    /// option: a compromised playlist cannot pivot into a weird demuxer.
    fn allowed(&self) -> impl Iterator<Item = &'a DemuxerDesc> + '_ {
        self.candidates
            .iter()
            .copied()
            .filter(|d| self.options.format_allowed(d.name))
    }

    /// Score every allowed candidate, best first.
    ///
    /// Ordering is `(score descending, name ascending)`. The name key is what
    /// makes a tie deterministic. `planning/18-formats.md` R6 wanted an
    /// explicit `priority: i16` on the descriptor to break ties by review
    /// rather than by accident; the frozen [`DemuxerDesc`] has no such field,
    /// so the name is the tie-break we have. See the docs file — this is a
    /// reported gap, not a design choice.
    #[must_use]
    pub fn score_all(&self, data: &ProbeData<'_>) -> Vec<Scored<'a>> {
        let mut out: Vec<Scored<'a>> = self
            .allowed()
            .map(|desc| Scored {
                desc,
                score: Self::score_one(desc, data),
            })
            .collect();
        out.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.desc.name.cmp(b.desc.name))
        });
        out
    }

    /// One candidate's score, with the MIME bonus applied (R1, R3).
    fn score_one(desc: &DemuxerDesc, data: &ProbeData<'_>) -> ProbeScore {
        let base = (desc.probe)(data);
        // Measured: a MIME type never rescues a zero content score. Applying
        // the bonus here would make a mislabelled octet-stream open as
        // whatever the server said it was.
        if base.is_none() {
            return base;
        }
        let Some(mime) = data.mime_type else {
            return base;
        };
        if desc
            .mime_types
            .iter()
            .any(|m| m.eq_ignore_ascii_case(mime.trim()))
        {
            base.max(ProbeScore::EXTENSION)
                .saturating_add(ProbeScore::MIME_BONUS)
        } else {
            base
        }
    }

    /// The winner, or `None` when every candidate scored zero (R4, R5).
    #[must_use]
    pub fn best(&self, data: &ProbeData<'_>) -> Option<Detected<'a>> {
        let mut best: Option<Scored<'a>> = None;
        for desc in self.allowed() {
            let score = Self::score_one(desc, data);
            if score.is_none() {
                continue;
            }
            let better = match best {
                None => true,
                Some(cur) => score > cur.score || (score == cur.score && desc.name < cur.desc.name),
            };
            if better {
                best = Some(Scored { desc, score });
            }
        }
        best.map(|s| Detected {
            desc: s.desc,
            score: s.score,
            probed_bytes: data.len(),
        })
    }

    /// `-f <name>`: bypass probing entirely (R8).
    ///
    /// The named demuxer is selected without its probe being called and the
    /// reported score is [`ProbeScore::NONE`] — **measured**, against the plan,
    /// which predicted `MAX`. `ffprobe -f matroska a.mkv` prints
    /// `probe_score=0`.
    ///
    /// The whitelist still applies: forcing a format is a convenience, not an
    /// authorisation.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] when no candidate carries that name.
    pub fn force(&self, name: &str) -> Result<Detected<'a>> {
        self.allowed()
            .find(|d| d.name == name || d.name.split(',').any(|n| n == name))
            .map(|desc| Detected {
                desc,
                score: ProbeScore::NONE,
                probed_bytes: 0,
            })
            .ok_or(Error::Unsupported("no demuxer with that name"))
    }

    /// The full detection loop against a live source: probe, and while the best
    /// score is in the retry band and more input exists, double the window and
    /// probe again (R7).
    ///
    /// The window starts at [`PROBE_BUF_MIN`] and doubles to at most
    /// [`FormatOptions::probe_ceiling`]. Every read is a `peek`, so the source's
    /// position is unchanged on return whatever the outcome — which is what
    /// makes this work on a pipe, and why detection does not have to be undone
    /// when it fails.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when nothing scores above zero, or whatever the
    /// transport reports. A read that returns short is not an error: it means
    /// the file is smaller than the window, and the loop stops growing.
    pub fn detect(
        &self,
        io: &mut IoContext,
        filename: Option<&str>,
        mime_type: Option<&str>,
    ) -> Result<Detected<'a>> {
        let ceiling = self.options.probe_ceiling();
        let mut window = PROBE_BUF_MIN.min(ceiling);
        let mut best: Option<Detected<'a>> = None;
        loop {
            let buf = io.peek(window)?;
            let short = buf.len() < window;
            let data = ProbeData {
                buf,
                filename,
                mime_type,
            };
            let found = self.best(&data);
            if let Some(d) = found {
                best = Some(d);
                if !d.score.needs_retry() {
                    break;
                }
            }
            // Stop growing when the file is exhausted or the ceiling is hit.
            if short || window >= ceiling {
                break;
            }
            window = window.saturating_mul(2).min(ceiling);
        }
        best.ok_or(Error::InvalidData("no demuxer recognised this input"))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::field_reassign_with_default,
    clippy::cast_possible_wrap,
    clippy::unnecessary_wraps,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;
    use crate::test_support::{DESC_A, DESC_B, DESC_MIME, DESC_WEAK};

    fn opts() -> FormatOptions {
        FormatOptions::default()
    }

    #[test]
    fn padding_reads_as_zero_then_stops() {
        let d = ProbeData::new(b"ab");
        assert_eq!(d.get(0), Some(b'a'));
        assert_eq!(d.get(1), Some(b'b'));
        assert_eq!(d.get(2), Some(0));
        assert_eq!(d.get(2 + ProbeData::PADDING - 1), Some(0));
        assert_eq!(d.get(2 + ProbeData::PADDING), None);
    }

    #[test]
    fn typed_reads_span_the_padding() {
        let d = ProbeData::new(&[0x12, 0x34]);
        assert_eq!(d.rb32(0), Some(0x1234_0000));
        assert_eq!(d.rl32(0), Some(0x0000_3412));
        assert_eq!(d.rb16(0), Some(0x1234));
        // Wholly inside the padding is still readable.
        assert_eq!(d.rb64(2), Some(0));
        // Past it is not.
        assert_eq!(d.rb64(2 + ProbeData::PADDING - 4), None);
    }

    #[test]
    fn empty_input_is_total() {
        let d = ProbeData::new(&[]);
        assert!(d.is_empty());
        assert_eq!(d.get(0), Some(0));
        assert_eq!(d.tag(0), Some([0, 0, 0, 0]));
        assert!(!d.starts_with(b"x"));
        assert!(d.starts_with(&[0u8]));
        assert_eq!(d.find(b"x", 0, 1024), None);
    }

    #[test]
    fn extension_is_the_last_dot_of_the_last_component() {
        let f = "/a.dir/movie.MP4";
        assert_eq!(
            ProbeData::new(&[]).with_filename(f).extension().as_deref(),
            Some("mp4")
        );
        let g = "/a.dir/movie";
        assert_eq!(ProbeData::new(&[]).with_filename(g).extension(), None);
        let h = "trailing.";
        assert_eq!(ProbeData::new(&[]).with_filename(h).extension(), None);
    }

    #[test]
    fn find_never_matches_inside_the_padding() {
        let d = ProbeData::new(b"aXb");
        assert_eq!(d.find(b"X", 0, 1024), Some(1));
        // Two zero bytes exist only in the padding, so they are not findable.
        assert_eq!(d.find(&[0, 0], 0, 1024), None);
    }

    #[test]
    fn repeating_matches_the_convention_table() {
        assert_eq!(ProbeScore::repeating(0), ProbeScore(25));
        assert_eq!(ProbeScore::repeating(1), ProbeScore(33));
        assert_eq!(ProbeScore::repeating(10), ProbeScore(100));
        assert_eq!(ProbeScore::repeating(u32::MAX), ProbeScore::MAX);
        assert!(ProbeScore::repeating(0).needs_retry());
        assert!(!ProbeScore::repeating(1).needs_retry());
    }

    #[test]
    fn weak_stays_in_the_retry_band() {
        assert_eq!(ProbeScore::weak(0), ProbeScore(5));
        assert_eq!(ProbeScore::weak(200), ProbeScore(25));
        for raw in 0..=u8::MAX {
            assert!(ProbeScore::weak(raw).needs_retry());
        }
    }

    #[test]
    fn highest_score_wins() {
        let o = opts();
        let cands: &[&DemuxerDesc] = &[&DESC_A, &DESC_B];
        let p = Probe::new(cands, &o);
        let d = p.best(&ProbeData::new(b"AAAAdata")).unwrap();
        assert_eq!(d.desc.name, "fmt-a");
        assert_eq!(d.score, ProbeScore::MAX);
        let d = p.best(&ProbeData::new(b"BBBBdata")).unwrap();
        assert_eq!(d.desc.name, "fmt-b");
    }

    #[test]
    fn zero_never_wins() {
        let o = opts();
        let cands: &[&DemuxerDesc] = &[&DESC_A, &DESC_B];
        let p = Probe::new(cands, &o);
        assert!(p.best(&ProbeData::new(b"nothing at all")).is_none());
    }

    #[test]
    fn whitelist_filters_before_probing() {
        let mut o = opts();
        o.format_whitelist = "fmt-b".to_owned();
        let cands: &[&DemuxerDesc] = &[&DESC_A, &DESC_B];
        let p = Probe::new(cands, &o);
        assert!(p.best(&ProbeData::new(b"AAAAdata")).is_none());
        assert!(p.force("fmt-a").is_err());
        assert!(p.force("fmt-b").is_ok());
    }

    #[test]
    fn forced_format_scores_zero() {
        let o = opts();
        let cands: &[&DemuxerDesc] = &[&DESC_A, &DESC_B];
        let p = Probe::new(cands, &o);
        let d = p.force("fmt-a").unwrap();
        assert_eq!(d.score, ProbeScore::NONE);
        assert_eq!(d.probed_bytes, 0);
    }

    #[test]
    fn mime_bonus_lifts_a_nonzero_score_and_only_that() {
        let o = opts();
        let cands: &[&DemuxerDesc] = &[&DESC_MIME];
        let p = Probe::new(cands, &o);
        // Weak content evidence plus a matching MIME type.
        let data = ProbeData::new(b"MIMEx").with_mime_type("video/x-test");
        assert_eq!(p.best(&data).unwrap().score, ProbeScore(80));
        // The same MIME type over content that scores zero: still nothing.
        let data = ProbeData::new(b"zzzz").with_mime_type("video/x-test");
        assert!(p.best(&data).is_none());
        // Content evidence with a MIME type the descriptor does not claim.
        let data = ProbeData::new(b"MIMEx").with_mime_type("audio/ogg");
        assert_eq!(p.best(&data).unwrap().score, ProbeScore(20));
    }

    #[test]
    fn ties_break_by_name_deterministically() {
        // Both score identically on this input.
        let o = opts();
        let forward: &[&DemuxerDesc] = &[&DESC_A, &DESC_B];
        let reverse: &[&DemuxerDesc] = &[&DESC_B, &DESC_A];
        let data = ProbeData::new(b"TIE!");
        let a = Probe::new(forward, &o).best(&data).unwrap();
        let b = Probe::new(reverse, &o).best(&data).unwrap();
        assert_eq!(a.desc.name, b.desc.name);
        assert_eq!(a.desc.name, "fmt-a");
    }

    #[test]
    fn score_all_is_sorted_and_total() {
        let o = opts();
        let cands: &[&DemuxerDesc] = &[&DESC_B, &DESC_A, &DESC_WEAK];
        let scored = Probe::new(cands, &o).score_all(&ProbeData::new(b"AAAAdata"));
        assert_eq!(scored.len(), 3);
        assert!(scored.windows(2).all(|w| w[0].score >= w[1].score));
        assert_eq!(scored[0].desc.name, "fmt-a");
    }
}
