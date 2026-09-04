//! `concat`: a **demuxer**, not a muxer.
//!
//! # Measured
//!
//! `ffmpeg -muxers | grep concat` prints nothing; `ffmpeg -demuxers | grep
//! concat` prints `D   concat          Virtual concatenation script`
//! (ffmpeg 8.1). So [`DEMUXER_CONCAT`] is a
//! [`vaco_format_core::DemuxerDesc`], registered from this crate anyway per
//! the brief that asked for it — the crate is named `vaco-mux-stream` for
//! the other five registrations, and this one just does not fit that name.
//!
//! # Two layers, for the same reason `vaco-demux-image2` has two
//!
//! [`script`] is pure text-in, structured-directives-out — no I/O, builds for
//! `wasm32-unknown-unknown` with no caveats, and is what
//! `fuzz/fuzz_targets/vaco_mux_stream_concat_script.rs` drives directly.
//!
//! This module is the layer above it that actually demuxes, and it hits the
//! same wall `vaco-demux-image2`'s `multi.rs` already documented:
//! [`vaco_format_core::DemuxerDesc::open`] is `fn(Box<dyn MediaSource>, &dyn
//! ParserProvider) -> Result<Box<dyn Demuxer>>` — one already-open source (the
//! concat *script itself*), no filename, and — one problem deeper than
//! image2's — no way to open the *other* files the script names at all.
//! [`ConcatSource`] is this crate's own version of the same seam
//! [`vaco_format_core::BsfProvider`]/[`vaco_format_core::ParserProvider`] use
//! one layer up: a caller with `vaco-registry` in scope (an embedder,
//! `vaco-cli`) implements it by probing and opening each named path; this
//! module's own tests supply a fake that opens in-memory fixtures.
//!
//! [`ConcatDemuxer::open_script`] is the real entry point, taking a
//! `&dyn ConcatSource` it only needs during construction (every file is
//! opened eagerly, once, up front — see [`ConcatDemuxer::open_script`]'s
//! docs for why that is also the simpler design, not just the one the
//! borrowed-provider signature allows). [`DEMUXER_CONCAT`]'s `open` cannot
//! call it — it has no [`ConcatSource`] to pass — so it parses the script
//! (a genuinely useful validation on its own) and then reports the gap with
//! [`vaco_core::Error::Unsupported`] rather than pretending to demux nothing
//! forever.
//!
//! # What is faithfully modelled and what is approximate
//!
//! * The script grammar ([`script`]) is measured carefully — see its module
//!   docs — and is the part a fuzz target exercises.
//! * `file`, `duration`, `inpoint`, `outpoint` are honoured: files are
//!   concatenated in listed order, packets outside `[inpoint, outpoint)` are
//!   dropped, and each file's contribution to the running timestamp offset
//!   is its `duration` directive if given, else its own demuxer's reported
//!   [`vaco_format_core::Demuxer::duration`], else zero (documented rather
//!   than guessed — a file with neither produces overlapping, not
//!   sequential, timestamps for whatever follows it).
//! * **Every file is assumed to expose the same number of streams, in the
//!   same order**, which [`ConcatDemuxer::streams`] reports from the first
//!   file alone. This is the overwhelmingly common real use of `concat`
//!   (successive segments of one encode) and was not relaxed for a more
//!   general N-to-M stream reconciliation, which the reference's own
//!   implementation is documented (by its own `-safe`/`-auto_convert`
//!   options) to care a great deal about and this crate's probing budget did
//!   not stretch to.
//! * `option`, `file_packet_metadata`, `stream`/`exact_stream_id` parse
//!   ([`script::Directive`]) but are **not semantically wired up**: `option`
//!   because it configures the demuxer's own `AVOption` table, which this
//!   crate does not reflect; the other two because their exact effect was not
//!   pinned down by probing (see `script`'s module docs). They are visible on
//!   [`FileEntry`] for a caller that wants them, unused by
//!   [`ConcatDemuxer`] itself.
//! * `-auto_convert` (bitstream reformatting between segments with different
//!   Annex-B/length-prefix framing) is not implemented — it needs a
//!   [`vaco_format_core::BsfProvider`], which `open_script` does not take
//!   today. [`ConcatOptions::auto_convert`] is stored and otherwise ignored.

pub mod script;

use vaco_core::{Duration, Error, Result, Rounding, Timestamp};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, ParserProvider, SeekFlags, SeekTarget, Stream,
};
use vaco_io::MediaSource;

pub use script::{Directive, Script, ScriptError};

/// `-safe`/`-auto_convert`/`-segment_time_metadata`, the three
/// `ffmpeg -h demuxer=concat` names (measured).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConcatOptions {
    /// Default `true`. Rejects an `option` directive in the script
    /// ([`script::parse`]'s `safe` argument) and — in
    /// [`ConcatDemuxer::open_script`] — a `file` path that is absolute or
    /// escapes upward via `..`, matching the measured
    /// `Unsafe file name '<path>'` rejection.
    pub safe: bool,
    /// Default `true`. Stored, not acted on — see the module docs.
    pub auto_convert: bool,
    /// Default `false`. Stored, not acted on: this crate has no packet
    /// side-data channel wired up for it yet.
    pub segment_time_metadata: bool,
}

impl Default for ConcatOptions {
    fn default() -> Self {
        Self {
            safe: true,
            auto_convert: true,
            segment_time_metadata: false,
        }
    }
}

/// One resolved `file` entry: the path plus every directive that followed it
/// before the next `file` (or end of script).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FileEntry {
    pub path: String,
    pub duration: Option<Duration>,
    pub inpoint: Option<Duration>,
    pub outpoint: Option<Duration>,
    /// `file_packet_metadata` lines, in order. Not interpreted — see the
    /// module docs.
    pub packet_metadata: Vec<(String, String)>,
    /// `stream`/`exact_stream_id` lines, in order, as
    /// `("stream", "")`/`("exact_stream_id", "<id>")` pairs. Not
    /// interpreted — see the module docs.
    pub stream_directives: Vec<(&'static str, String)>,
}

/// Turn a parsed [`Script`] into a resolved [`FileEntry`] list.
///
/// # Errors
/// Never today — every directive [`script::parse`] accepts attaches
/// somewhere. Kept fallible because a future directive with a real ordering
/// requirement (e.g. one that must not precede any `file`) should be able to
/// report that here without a signature change.
pub fn resolve_entries(script: &Script) -> Result<Vec<FileEntry>> {
    let mut entries: Vec<FileEntry> = Vec::new();
    for line in &script.lines {
        match &line.directive {
            Directive::File(path) => entries.push(FileEntry {
                path: path.clone(),
                ..FileEntry::default()
            }),
            Directive::Duration(d) => {
                if let Some(e) = entries.last_mut() {
                    e.duration = Some(*d);
                }
            }
            Directive::Inpoint(d) => {
                if let Some(e) = entries.last_mut() {
                    e.inpoint = Some(*d);
                }
            }
            Directive::Outpoint(d) => {
                if let Some(e) = entries.last_mut() {
                    e.outpoint = Some(*d);
                }
            }
            Directive::FilePacketMetadata(k, v) => {
                if let Some(e) = entries.last_mut() {
                    e.packet_metadata.push((k.clone(), v.clone()));
                }
            }
            Directive::Stream => {
                if let Some(e) = entries.last_mut() {
                    e.stream_directives.push(("stream", String::new()));
                }
            }
            Directive::ExactStreamId(id) => {
                if let Some(e) = entries.last_mut() {
                    e.stream_directives.push(("exact_stream_id", id.clone()));
                }
            }
            Directive::FfconcatVersion(_) | Directive::Option(_, _) => {}
        }
    }
    Ok(entries)
}

/// Supplies demuxers for the files a concat script names.
///
/// See the module docs for why this exists: the registry seam
/// ([`DemuxerDesc::open`]) has no way to open a second file, and reaching
/// through `vaco-registry` directly would cycle (`vaco-registry` depends on
/// every format crate, this one included).
pub trait ConcatSource {
    /// Open `path` (already resolved relative to the script's own
    /// directory, or absolute, exactly as the script wrote it — this trait
    /// does no path resolution of its own).
    ///
    /// # Errors
    /// Whatever opening and probing that path produces.
    fn open(&self, path: &str) -> Result<Box<dyn Demuxer>>;
}

/// Whether `path` is one [`ConcatOptions::safe`] rejects: absolute, or
/// containing a `..` component. Measured: `-safe 0` is required for an
/// absolute path (`Unsafe file name '/tmp/seg1.ts'` without it); `..` is
/// this crate's own conservative extension of the same rule (untested
/// against the reference directly, since a same-directory fixture set has no
/// natural `..` case to probe) rather than a gap left silently open.
#[must_use]
pub fn is_unsafe_path(path: &str) -> bool {
    std::path::Path::new(path).is_absolute()
        || std::path::Path::new(path)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
}

/// The `concat` demuxer: N inner demuxers, opened eagerly, read in sequence
/// with timestamps rewritten onto one continuous timeline.
pub struct ConcatDemuxer {
    streams: Vec<Stream>,
    inners: Vec<Box<dyn Demuxer>>,
    entries: Vec<FileEntry>,
    current: usize,
    /// Per output-stream cumulative offset, in that stream's own time base.
    offset_ticks: Vec<i64>,
}

impl core::fmt::Debug for ConcatDemuxer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ConcatDemuxer")
            .field("files", &self.entries.len())
            .field("current", &self.current)
            .field("streams", &self.streams.len())
            .finish_non_exhaustive()
    }
}

impl ConcatDemuxer {
    /// Parse `script_text` and open every `file` entry through `source`,
    /// eagerly, in listed order.
    ///
    /// # Why eager rather than lazy
    ///
    /// `source` is borrowed only for the duration of this call, matching how
    /// [`vaco_format_core::ParserProvider`] is handed to a demuxer's own
    /// `open` (a plain `fn` pointer, whose borrowed-reference parameter
    /// cannot outlive that one call) — a lazily-opening design would need to
    /// *own* the provider for the object's whole lifetime instead, a needless
    /// complication when eagerly resolving every path up front also gets a
    /// total duration for free and fails fast on a missing file rather than
    /// midway through a read.
    ///
    /// # Errors
    /// [`ScriptError`] (wrapped) for a malformed script; whatever `source`
    /// returns for a file it could not open; [`vaco_core::Error::Unsupported`]
    /// for a path [`is_unsafe_path`] rejects under `options.safe`.
    pub fn open_script(
        script_text: &str,
        options: ConcatOptions,
        source: &dyn ConcatSource,
    ) -> Result<Self> {
        let script = script::parse(script_text, options.safe).map_err(|e| Error::Option {
            name: "concat".into(),
            detail: e.to_string(),
        })?;
        let entries = resolve_entries(&script)?;
        let mut inners = Vec::new();
        for entry in &entries {
            if options.safe && is_unsafe_path(&entry.path) {
                return Err(Error::Option {
                    name: "concat".into(),
                    detail: format!("Unsafe file name '{}'", entry.path),
                });
            }
            inners.push(source.open(&entry.path)?);
        }
        let streams = inners
            .first()
            .map(|d| d.streams().to_vec())
            .unwrap_or_default();
        let offset_ticks = vec![0i64; streams.len()];
        Ok(Self {
            streams,
            inners,
            entries,
            current: 0,
            offset_ticks,
        })
    }

    /// Whether `pts` (in stream `idx`'s own time base) falls inside
    /// `[inpoint, outpoint)` for the entry currently playing. `None` (no
    /// pts) is never trimmed — there is nothing to compare.
    fn in_trim_window(&self, idx: usize, pts: Timestamp) -> bool {
        let Some(entry) = self.entries.get(self.current) else {
            return true;
        };
        let Some(tb) = self.streams.get(idx).map(|s| s.time_base) else {
            return true;
        };
        let Some(d) = pts.to_duration(tb) else {
            return true;
        };
        if let Some(inpoint) = entry.inpoint
            && d.as_micros() < inpoint.as_micros()
        {
            return false;
        }
        if let Some(outpoint) = entry.outpoint
            && d.as_micros() >= outpoint.as_micros()
        {
            return false;
        }
        true
    }

    /// This entry's contribution to the running offset: its `duration`
    /// directive if stated, else its own demuxer's reported duration, else
    /// zero. See the module docs for why zero is not a guess dressed up as
    /// one.
    fn entry_span(&self) -> Duration {
        let Some(entry) = self.entries.get(self.current) else {
            return Duration::from_micros(0);
        };
        if let Some(d) = entry.duration {
            return d;
        }
        self.inners
            .get(self.current)
            .and_then(vaco_format_core::Demuxer::duration)
            .unwrap_or(Duration::from_micros(0))
    }

    /// Advance `offset_ticks` by the current file's span, then move to the
    /// next file. Returns `false` once there is no next file.
    fn advance(&mut self) -> bool {
        let span = self.entry_span();
        for (idx, offset) in self.offset_ticks.iter_mut().enumerate() {
            let tb = self
                .streams
                .get(idx)
                .map_or(vaco_format_core::time::TIME_BASE_Q, |s| s.time_base);
            if let Some(ticks) = span.to_ticks(tb) {
                *offset = offset.saturating_add(ticks);
            }
        }
        self.current += 1;
        self.current < self.inners.len()
    }

    fn rescale_out(&self, idx: usize, ts: Timestamp) -> Timestamp {
        let Some(entry_tb) = self
            .inners
            .get(self.current)
            .and_then(|d| d.streams().get(idx))
            .map(|s| s.time_base)
        else {
            return ts;
        };
        let Some(out_tb) = self.streams.get(idx).map(|s| s.time_base) else {
            return ts;
        };
        let rescaled = ts.rescale(entry_tb, out_tb, Rounding::default());
        let offset = self.offset_ticks.get(idx).copied().unwrap_or(0);
        rescaled.offset(offset)
    }
}

impl Demuxer for ConcatDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<vaco_packet::Packet> {
        loop {
            let Some(inner) = self.inners.get_mut(self.current) else {
                return Err(Error::Eof);
            };
            match inner.read_packet() {
                Ok(mut pkt) => {
                    let idx = pkt.stream_index as usize;
                    if idx >= self.streams.len() {
                        // This file exposes more streams than the first one
                        // did; see the module docs' "same shape" assumption.
                        continue;
                    }
                    if !self.in_trim_window(idx, pkt.pts) {
                        continue;
                    }
                    pkt.pts = self.rescale_out(idx, pkt.pts);
                    pkt.dts = self.rescale_out(idx, pkt.dts);
                    return Ok(pkt);
                }
                Err(Error::Eof) => {
                    if !self.advance() {
                        return Err(Error::Eof);
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        let _ = (target, flags);
        Err(Error::Unsupported(
            "concat: seeking across a virtual concatenation is not implemented",
        ))
    }

    fn duration(&self) -> Option<Duration> {
        let mut total = 0i64;
        for (i, entry) in self.entries.iter().enumerate() {
            let span = entry.duration.or_else(|| {
                self.inners
                    .get(i)
                    .and_then(vaco_format_core::Demuxer::duration)
            })?;
            total = total.checked_add(span.as_micros())?;
        }
        Some(Duration::from_micros(total))
    }
}

/// Cheap content sniff: any script that parses as at least one `file` entry
/// under the strictest (`safe`) reading. Deliberately does not try to open
/// any referenced file — detection must stay strict without touching the
/// filesystem, matching the demux/detect split `vaco-demux-raw`'s AV1
/// lesson established: a demuxer's own leniency must never leak into
/// what claims a file.
fn probe_concat(data: &ProbeData<'_>) -> ProbeScore {
    let Ok(text) = core::str::from_utf8(data.buf) else {
        return ProbeScore::NONE;
    };
    match script::parse(text, true) {
        Ok(script)
            if script
                .lines
                .iter()
                .any(|l| matches!(l.directive, Directive::File(_))) =>
        {
            ProbeScore::MAX
        }
        _ => ProbeScore::NONE,
    }
}

/// The registry `open` path. Parses the script (a real check, not a stub)
/// and then reports the structural gap described in the module docs — it
/// has no [`ConcatSource`] to open anything with. A caller that has one
/// should use [`ConcatDemuxer::open_script`] directly instead of going
/// through the registry.
fn open_concat(
    mut source: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    let mut text = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match source.read(&mut chunk) {
            Ok(0) | Err(Error::Eof) => break,
            Ok(n) => text.extend_from_slice(chunk.get(..n).unwrap_or_default()),
            Err(e) => return Err(e),
        }
    }
    let text =
        String::from_utf8(text).map_err(|_| Error::InvalidData("concat script is not UTF-8"))?;
    // A genuinely malformed script is still reported precisely, even though
    // this path cannot demux anything.
    script::parse(&text, true).map_err(|e| Error::Option {
        name: "concat".into(),
        detail: e.to_string(),
    })?;
    Err(Error::Unsupported(
        "concat: the registry `open` path has no way to open the files a script names; use ConcatDemuxer::open_script with a ConcatSource",
    ))
}

/// `concat`'s declared flags.
///
/// [`FormatFlags::TS_DISCONT`]: timestamps legitimately jump at a file
/// boundary whose span was unknown (see the module docs — this is an
/// architectural judgement call, not something read off the reference's own
/// internal flags, which no CLI surface exposes).
/// [`FormatFlags::GENERIC_INDEX`]: this demuxer builds no index of its own,
/// so the core's generic one is the only seek path available in principle
/// (`seek` itself is still [`Error::Unsupported`] today — see
/// [`ConcatDemuxer::seek`]).
pub const FLAGS: FormatFlags = FormatFlags::GENERIC_INDEX.union(FormatFlags::TS_DISCONT);

/// `concat`: `ffmpeg -demuxers | grep concat` -> `Virtual concatenation
/// script`.
pub static DEMUXER_CONCAT: DemuxerDesc = DemuxerDesc {
    name: "concat",
    long_name: "Virtual concatenation script",
    extensions: &["concat", "ffconcat"],
    mime_types: &[],
    flags: FLAGS,
    probe: probe_concat,
    open: open_concat,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use vaco_core::{MediaType, Rational};
    use vaco_limits::{Budget, Limits};
    use vaco_packet::{Packet, PacketFlags};

    /// A demuxer that yields a fixed list of `(pts_ticks, duration_ticks)`
    /// packets on stream 0, then EOF.
    struct FakeDemuxer {
        streams: Vec<Stream>,
        frames: Vec<i64>,
        next: usize,
        total_duration: Option<Duration>,
    }

    impl Demuxer for FakeDemuxer {
        fn streams(&self) -> &[Stream] {
            &self.streams
        }
        fn read_packet(&mut self) -> Result<Packet> {
            let Some(&pts) = self.frames.get(self.next) else {
                return Err(Error::Eof);
            };
            self.next += 1;
            let mut budget = Budget::new(Limits::permissive());
            let mut p = Packet::from_slice(&mut budget, b"x")
                .map_err(|_| Error::Unsupported("test packet alloc"))?;
            p.stream_index = 0;
            p.pts = Timestamp::new(pts);
            p.dts = Timestamp::new(pts);
            p.flags = PacketFlags::KEY;
            Ok(p)
        }
        fn seek(&mut self, _t: SeekTarget, _f: SeekFlags) -> Result<()> {
            Err(Error::Unsupported("fake"))
        }
        fn duration(&self) -> Option<Duration> {
            self.total_duration
        }
    }

    fn video_stream(time_base: Rational) -> Stream {
        Stream::new(0, MediaType::Video, time_base)
    }

    struct FakeSource {
        files: Mutex<HashMap<String, Vec<i64>>>,
        time_base: Rational,
        duration: Option<Duration>,
    }

    impl ConcatSource for FakeSource {
        fn open(&self, path: &str) -> Result<Box<dyn Demuxer>> {
            let files = self.files.lock().map_err(|_| Error::Unsupported("lock"))?;
            let frames = files
                .get(path)
                .cloned()
                .ok_or(Error::Unsupported("no such fixture"))?;
            Ok(Box::new(FakeDemuxer {
                streams: vec![video_stream(self.time_base)],
                frames,
                next: 0,
                total_duration: self.duration,
            }))
        }
    }

    fn fixture(
        pairs: &[(&str, &[i64])],
        time_base: Rational,
        duration: Option<Duration>,
    ) -> FakeSource {
        let mut files = HashMap::new();
        for (name, frames) in pairs {
            files.insert((*name).to_owned(), frames.to_vec());
        }
        FakeSource {
            files: Mutex::new(files),
            time_base,
            duration,
        }
    }

    #[test]
    fn concatenates_two_files_offsetting_the_second_by_the_firsts_duration() {
        let source = fixture(
            &[("a.ts", &[0, 1000, 2000]), ("b.ts", &[0, 1000])],
            Rational::new(1, 1000),
            Some(Duration::from_micros(3_000_000)),
        );
        let mut d = ConcatDemuxer::open_script(
            "file 'a.ts'\nfile 'b.ts'\n",
            ConcatOptions {
                safe: false,
                ..Default::default()
            },
            &source,
        )
        .unwrap();
        let mut pts = Vec::new();
        loop {
            match d.read_packet() {
                Ok(p) => pts.push(p.pts.ticks().unwrap()),
                Err(Error::Eof) => break,
                Err(e) => unreachable!("read_packet failed: {e}"),
            }
        }
        // File a: 0,1000,2000 (ms). File b's own pts 0,1000 offset by a's
        // 3s (3000ms) duration.
        assert_eq!(pts, vec![0, 1000, 2000, 3000, 4000]);
    }

    #[test]
    fn duration_directive_overrides_the_inner_demuxers_own_duration() {
        let source = fixture(
            &[("a.ts", &[0, 1000]), ("b.ts", &[0])],
            Rational::new(1, 1000),
            Some(Duration::from_micros(999_999_999)), // would be wrong if used
        );
        let mut d = ConcatDemuxer::open_script(
            "file 'a.ts'\nduration 5\nfile 'b.ts'\n",
            ConcatOptions {
                safe: false,
                ..Default::default()
            },
            &source,
        )
        .unwrap();
        let mut pts = Vec::new();
        while let Ok(p) = d.read_packet() {
            pts.push(p.pts.ticks().unwrap());
        }
        assert_eq!(pts, vec![0, 1000, 5000]);
    }

    #[test]
    fn inpoint_and_outpoint_trim_packets_outside_the_window() {
        let source = fixture(
            &[("a.ts", &[0, 500, 1000, 1500, 2000])],
            Rational::new(1, 1000),
            None,
        );
        let mut d = ConcatDemuxer::open_script(
            "file 'a.ts'\ninpoint 0.5\noutpoint 1.6\n",
            ConcatOptions {
                safe: false,
                ..Default::default()
            },
            &source,
        )
        .unwrap();
        let mut pts = Vec::new();
        while let Ok(p) = d.read_packet() {
            pts.push(p.pts.ticks().unwrap());
        }
        assert_eq!(pts, vec![500, 1000, 1500]);
    }

    #[test]
    fn an_absolute_path_is_rejected_when_safe() {
        let source = fixture(&[("/tmp/a.ts", &[0])], Rational::new(1, 1000), None);
        let err =
            ConcatDemuxer::open_script("file '/tmp/a.ts'\n", ConcatOptions::default(), &source)
                .unwrap_err();
        assert!(err.to_string().contains("Unsafe file name"));
    }

    #[test]
    fn an_absolute_path_is_allowed_when_unsafe() {
        let source = fixture(&[("/tmp/a.ts", &[0])], Rational::new(1, 1000), None);
        assert!(
            ConcatDemuxer::open_script(
                "file '/tmp/a.ts'\n",
                ConcatOptions {
                    safe: false,
                    ..Default::default()
                },
                &source
            )
            .is_ok()
        );
    }

    #[test]
    fn probe_recognises_a_script_and_rejects_prose() {
        let script = b"file 'a.ts'\nfile 'b.ts'\n";
        let data = ProbeData::new(script);
        assert_eq!(probe_concat(&data), ProbeScore::MAX);

        let prose = b"This is just some ordinary text, not a concat script at all.";
        let data = ProbeData::new(prose);
        assert_eq!(probe_concat(&data), ProbeScore::NONE);
    }

    #[test]
    fn the_registry_open_path_validates_the_script_and_reports_the_gap() {
        use vaco_format_core::NoParsers;
        let src = Box::new(vaco_io::MemorySource::new(b"file 'a.ts'\n".to_vec()));
        let Err(err) = open_concat(src, &NoParsers) else {
            unreachable!("open_concat must fail with no ConcatSource")
        };
        assert!(err.to_string().contains("ConcatSource"));

        let bad = Box::new(vaco_io::MemorySource::new(b"frobnicate\n".to_vec()));
        let Err(err) = open_concat(bad, &NoParsers) else {
            unreachable!("a malformed script must fail to parse")
        };
        assert!(err.to_string().contains("unknown keyword"));
    }

    #[test]
    fn is_unsafe_path_flags_absolute_and_parent_dir() {
        assert!(is_unsafe_path("/etc/passwd"));
        assert!(is_unsafe_path("../secret.ts"));
        assert!(!is_unsafe_path("seg1.ts"));
        assert!(!is_unsafe_path("sub/seg1.ts"));
    }

    #[test]
    fn descriptor_declares_non_empty_flags() {
        // `cargo test -p vaco-probe` rejects `FormatFlags::empty()` on any
        // registered demuxer; this is the same check kept local.
        assert_ne!(DEMUXER_CONCAT.flags, FormatFlags::empty());
        assert!(DEMUXER_CONCAT.matches_name("concat"));
    }
}
