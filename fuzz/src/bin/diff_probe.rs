//! The differential prober (XF-04, plan 13 §2.4): mutate real media, run it
//! through `vaco-probe` and the reference `ffprobe`, and classify the result.
//!
//! # What "agreement" means here
//!
//! Byte-identity is the right assertion for a remux and is already how the
//! conformance harness works elsewhere in this project. It is the wrong
//! assertion for a *mutated* file: a corrupted input can legitimately produce
//! different output, or none, on a perfectly correct implementation. What both
//! sides owe each other instead is:
//!
//! - **Acceptance agreement.** If the reference rejects a file, we should
//!   reject it too, and vice versa. Disagreement here (`stricter`/`laxer`) is
//!   as much a finding as disagreement about content, per the classification
//!   table below.
//! - **Content agreement, when both accept.** `-of flat -show_format
//!   -show_streams` gives a flat `key=value` list per side; every key present
//!   on one side and not the other, and every key both sides state with
//!   different values, is a finding. Text output rather than JSON, because a
//!   flat line is trivial to diff without a JSON parser — this crate has none,
//!   and D_no-new-deps is not worth spending on one.
//! - **Crashes and hangs are never agreement.** A crash on our side is the
//!   highest-priority finding this tool can produce; a crash on the
//!   reference's side is recorded and never acted on (we do not file upstream
//!   bugs from fuzzing without a human looking first).
//!
//! Error *text* is deliberately never compared, only error *category*
//! (accept/reject) — the reference's wording is not a contract.
//!
//! # Why a subprocess per input, not an in-process fuzz target
//!
//! Shelling out to `ffprobe` costs milliseconds per call, three to four orders
//! of magnitude slower than an in-process libFuzzer iteration. That is the
//! right trade here: a campaign that runs a few hundred cases *through the
//! real reference binary* is worth more than a million that never touch it.
//! The 175 targets alongside this one already cover the "never panic"
//! property at libFuzzer speed; this tool covers "and says the same thing",
//! which needs the reference in the loop and therefore cannot run at that
//! speed. `campaign` reports its own measured exec/s so this trade-off stays
//! visible rather than assumed.
//!
//! # Mutation
//!
//! Two tiers, selected with `--mutator`. `generic` (the default, and the
//! only one earlier campaigns ever ran) is structure-blind: bitflip, byte
//! overwrite, truncate, byte insertion, chunk duplication, and overwriting
//! four bytes with a length-field-shaped "interesting value" — sufficient to
//! find the classes of bug a generic byte fuzzer finds (truncation handling,
//! arithmetic on attacker-controlled lengths), but it reaches a length field
//! or a page/tag header only by luck, at a rate proportional to how much of
//! the file that structure occupies.
//!
//! `aware` (see [`structural_offsets`]) finds every chunk/box length field,
//! Ogg page header field and FLV tag size field it can recognise by shape —
//! not a real parser, three pattern scanners — and biases mutation toward
//! those offsets. Still the cheap tier: a real box/EBML/TS-aware mutator
//! that understands nesting and per-format field widths is its own
//! multi-day piece of work, not attempted here. Falls back to `generic`
//! verbatim on a seed with no recognised structure, and reproducibility
//! under `--rng-seed` holds for both.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("campaign") => cmd_campaign(&args[1..]),
        Some("replay") => cmd_replay(&args[1..]),
        _ => Err(
            "usage:\n  \
             diff_probe campaign --seed-dir <dir> --vaco-probe <path> [--ffprobe <path>] \
             [--iterations N | --seconds S] [--out <dir>] [--timeout-ms N] [--rng-seed N] \
             [--mutator generic|aware] [--baseline <path> [--update-baseline]]\n  \
             diff_probe replay <file> --vaco-probe <path> [--ffprobe <path>] [--timeout-ms N]"
                .to_owned(),
        ),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("diff_probe: {e}");
            ExitCode::FAILURE
        }
    }
}

// --------------------------------------------------------------------- args

/// Pulls `--flag value` pairs out of an arg list. Deliberately not a crate:
/// the whole surface is a handful of optional flags.
struct Args<'a>(&'a [String]);

impl<'a> Args<'a> {
    fn get(&self, flag: &str) -> Option<&'a str> {
        self.0
            .iter()
            .position(|a| a == flag)
            .and_then(|i| self.0.get(i + 1))
            .map(String::as_str)
    }

    fn positional(&self) -> Option<&'a str> {
        self.0.iter().find(|a| !a.starts_with("--")).map(String::as_str)
    }
}

// ------------------------------------------------------------------- rng

/// SplitMix64. Deterministic given a seed, which matters only for
/// reproducing a *campaign*, not for reproducing a finding — every mutant
/// that produces one is written to disk verbatim, so replaying it never needs
/// the generator that made it.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..bound`. `bound == 0` always returns `0`.
    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next_u64() % bound as u64) as usize
        }
    }
}

// -------------------------------------------------------------- mutation

/// One or more generic mutations applied to a copy of `input`.
fn mutate(input: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut buf = input.to_vec();
    if buf.is_empty() {
        buf.push(rng.next_u64() as u8);
    }
    let ops = rng.below(3) + 1;
    for _ in 0..ops {
        apply_one_mutation(&mut buf, rng);
    }
    buf
}

const INTERESTING_WORDS: [[u8; 4]; 6] = [
    [0x00, 0x00, 0x00, 0x00],
    [0xff, 0xff, 0xff, 0xff],
    [0x00, 0x00, 0x00, 0x01],
    [0x7f, 0xff, 0xff, 0xff],
    [0x80, 0x00, 0x00, 0x00],
    [0xff, 0xff, 0xff, 0xfe],
];

fn apply_one_mutation(buf: &mut Vec<u8>, rng: &mut Rng) {
    if buf.is_empty() {
        buf.push(rng.next_u64() as u8);
        return;
    }
    match rng.below(6) {
        0 => {
            // bitflip
            let i = rng.below(buf.len());
            let bit = rng.below(8);
            buf[i] ^= 1 << bit;
        }
        1 => {
            // overwrite one byte
            let i = rng.below(buf.len());
            buf[i] = rng.next_u64() as u8;
        }
        2 => {
            // truncate at a random boundary, at least one byte kept
            let new_len = rng.below(buf.len()) + 1;
            buf.truncate(new_len);
        }
        3 => {
            // insert random bytes
            let at = rng.below(buf.len() + 1);
            let n = rng.below(16) + 1;
            let bytes: Vec<u8> = (0..n).map(|_| rng.next_u64() as u8).collect();
            buf.splice(at..at, bytes);
        }
        4 => {
            // duplicate a chunk elsewhere: simulates a repeated/duplicated box
            let start = rng.below(buf.len());
            let max_len = (buf.len() - start).min(64);
            let len = rng.below(max_len) + 1;
            let chunk: Vec<u8> = buf[start..start + len].to_vec();
            let at = rng.below(buf.len() + 1);
            buf.splice(at..at, chunk);
        }
        _ => {
            // overwrite four bytes with a length-field-shaped value
            if buf.len() >= 4 {
                let at = rng.below(buf.len() - 3);
                let word = INTERESTING_WORDS[rng.below(INTERESTING_WORDS.len())];
                buf[at..at + 4].copy_from_slice(&word);
            } else {
                let i = rng.below(buf.len());
                buf[i] = 0xff;
            }
        }
    }
}

// ------------------------------------------------------- format-aware

/// Byte offsets `apply_one_mutation_at` can hit that a structure-blind
/// mutator would only find by luck: a chunk/box length field, an Ogg page's
/// segment count, an FLV tag's data-size field. Cheap-tier, not a parser —
/// three pattern scanners, none of which need to know a *format*, only the
/// shape "tag then length" or "known magic then fixed offsets" that RIFF,
/// IFF/AIFF, ISOBMFF, CAF and W64 chunks, Ogg pages and FLV tags all share
/// one version or another of. Missing a real container here just means that
/// mutation falls back to a generic one, never a wrong offset: every offset
/// returned is bounds-checked against `buf.len()` before use.
fn structural_offsets(buf: &[u8]) -> Vec<(usize, usize)> {
    let mut hits = Vec::new();
    hits.extend(chunk_length_fields(buf));
    hits.extend(ogg_page_fields(buf));
    hits.extend(flv_tag_size_fields(buf));
    hits
}

/// A 4-byte ASCII tag (`RIFF`, `moov`, `SSND`, ...) next to a 4- or 8-byte
/// integer that, read as a length from that point, does not run past the
/// end of the buffer. Tries the length field on both sides of the tag
/// (RIFF/CAF put it after; ISOBMFF puts it before) and both endiannesses,
/// since this scanner does not know which container it is looking at.
/// Returns `(offset, width)` pairs for the length field itself.
fn chunk_length_fields(buf: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if buf.len() < 8 {
        return out;
    }
    for i in 0..=buf.len().saturating_sub(8) {
        if !is_ascii_tag(&buf[i..i + 4]) {
            continue;
        }
        // tag then 4-byte length (RIFF, ISOBMFF's type-then-nothing does not
        // apply here since ISOBMFF is size-then-type, handled below).
        if let Some(len) = buf.get(i + 4..i + 8) {
            check_len_field(buf, i + 4, 4, len, i + 8, &mut out);
        }
        // tag then 8-byte length (CAF).
        if let Some(len) = buf.get(i + 4..i + 12) {
            check_len_field(buf, i + 4, 8, len, i + 12, &mut out);
        }
        // 4-byte length then tag (ISOBMFF box, W64-style-adjacent).
        if i >= 4
            && let Some(len) = buf.get(i - 4..i)
        {
            check_len_field(buf, i - 4, 4, len, i + 4, &mut out);
        }
    }
    out
}

fn is_ascii_tag(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| (0x20..=0x7e).contains(&b))
}

fn check_len_field(
    buf: &[u8],
    at: usize,
    width: usize,
    len_bytes: &[u8],
    body_start: usize,
    out: &mut Vec<(usize, usize)>,
) {
    for be in [false, true] {
        let value: u64 = match (width, be) {
            (4, false) => u64::from(u32::from_le_bytes(len_bytes[..4].try_into().unwrap_or_default())),
            (4, true) => u64::from(u32::from_be_bytes(len_bytes[..4].try_into().unwrap_or_default())),
            (8, false) => u64::from_le_bytes(len_bytes[..8].try_into().unwrap_or_default()),
            (8, true) => u64::from_be_bytes(len_bytes[..8].try_into().unwrap_or_default()),
            _ => continue,
        };
        // A length that plausibly stays in-bounds (or legitimately runs to
        // EOF, which several of these formats spell as 0) is evidence this
        // is a real length field rather than four ASCII-tag-adjacent bytes
        // that happen to look like one.
        let in_bounds = value == 0 || body_start.saturating_add(value as usize) <= buf.len() + 4096;
        if in_bounds {
            out.push((at, width));
            return;
        }
    }
}

/// `OggS` page headers: the segment count byte (offset 26) and the 4-byte
/// checksum (offset 22) — mutating either changes how many segments a
/// reader thinks the page has, or invalidates a checksum some readers do
/// and do not verify.
fn ogg_page_fields(buf: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(pos) = find_from(buf, b"OggS", i) {
        if pos + 27 <= buf.len() {
            out.push((pos + 22, 4)); // checksum
            out.push((pos + 26, 1)); // page_segments
        }
        i = pos + 4;
    }
    out
}

/// An FLV tag stream: `TagType(1) DataSize(3 BE) Timestamp(3 BE)
/// TimestampExtended(1) StreamID(3) Data[DataSize] PreviousTagSize(4)`,
/// repeating from byte 13 (after the 9-byte file header and first
/// `PreviousTagSize0`). Walked rather than pattern-matched, since a tag has
/// no magic of its own — stops at the first tag whose declared size would
/// run past the buffer, which is exactly the state a mutant is expected to
/// reach quickly.
fn flv_tag_size_fields(buf: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if buf.len() < 13 || &buf[0..3] != b"FLV" {
        return out;
    }
    let mut pos = 13;
    while pos + 11 <= buf.len() {
        let size = u32::from_be_bytes([0, buf[pos + 1], buf[pos + 2], buf[pos + 3]]) as usize;
        out.push((pos + 1, 3));
        let next = pos + 11 + size + 4;
        if size == 0 || next <= pos || next > buf.len() {
            break;
        }
        pos = next;
    }
    out
}

fn find_from(buf: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= buf.len() {
        return None;
    }
    buf[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Like [`mutate`], but when the input has any recognisable chunk/page/tag
/// structure, biases toward overwriting a length-shaped field there instead
/// of a uniformly random byte. Falls back to [`mutate`] verbatim when no
/// structure is found, so every seed is still mutable.
fn mutate_aware(input: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut buf = input.to_vec();
    if buf.is_empty() {
        buf.push(rng.next_u64() as u8);
        return buf;
    }
    let offsets = structural_offsets(&buf);
    let ops = rng.below(3) + 1;
    for _ in 0..ops {
        if !offsets.is_empty() && rng.below(10) < 7 {
            let (at, width) = offsets[rng.below(offsets.len())];
            if at + width <= buf.len() {
                if width >= 4 {
                    let word = INTERESTING_WORDS[rng.below(INTERESTING_WORDS.len())];
                    buf[at..at + 4].copy_from_slice(&word);
                } else {
                    buf[at] = rng.next_u64() as u8;
                }
                continue;
            }
        }
        apply_one_mutation(&mut buf, rng);
    }
    buf
}

// --------------------------------------------------------------- running

#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Exited(i32),
    Signaled(i32),
    TimedOut,
}

struct Observed {
    status: Status,
    stdout: String,
}

/// Runs `bin -v quiet -of flat -show_format -show_streams <input>` and
/// classifies how it ended. A subprocess, not a library call, on both sides —
/// plan 13 §2.4.2 is explicit that linking either implementation in-process
/// is the wrong move (licence, clean room, and the CLI layer being part of
/// what must match).
fn probe_command(bin: &Path, input: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.args(["-v", "quiet", "-of", "flat", "-show_format", "-show_streams"])
        .arg(input)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

fn run_probe(bin: &Path, input: &Path, timeout: Duration) -> Result<Observed, String> {
    run_with_timeout(probe_command(bin, input), timeout)
        .map_err(|e| format!("running {}: {e}", bin.display()))
}

/// Runs both sides of one comparison concurrently instead of one after the
/// other. Measured throughput (~55 pairs/s) is dominated by process spawn,
/// not decode cost — three to four orders of magnitude slower than an
/// in-process fuzz target, per this module's own doc comment — and running
/// `ours` to completion before `reference` even starts wastes exactly that
/// overhead twice per iteration for no reason: nothing about classification
/// needs one process to finish before the other begins. Spawning both before
/// waiting on either is the entire change; each side's own wait-with-timeout
/// is otherwise identical to [`run_with_timeout`], including a hang on one
/// side never blocking detection of the other's result.
fn run_probe_pair(
    vaco_probe: &Path,
    ffprobe: &Path,
    input: &Path,
    timeout: Duration,
) -> (Result<Observed, String>, Result<Observed, String>) {
    let a = probe_command(vaco_probe, input).spawn();
    let b = probe_command(ffprobe, input).spawn();
    let a = a
        .map_err(|e| format!("running {}: {e}", vaco_probe.display()))
        .and_then(|child| {
            wait_child(child, timeout).map_err(|e| format!("running {}: {e}", vaco_probe.display()))
        });
    let b = b
        .map_err(|e| format!("running {}: {e}", ffprobe.display()))
        .and_then(|child| {
            wait_child(child, timeout).map_err(|e| format!("running {}: {e}", ffprobe.display()))
        });
    (a, b)
}

/// Spawns `cmd`, waits up to `timeout`, and kills it (by pid, via the `kill`
/// utility — no signal-handling dependency needed for that) if it has not
/// finished. A hang is not a shrug: it is a real denial-of-service finding
/// for anything reading untrusted media, exactly as much as a crash.
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> std::io::Result<Observed> {
    let child = cmd.spawn()?;
    wait_child(child, timeout)
}

/// The waiting half of [`run_with_timeout`], split out so [`run_probe_pair`]
/// can spawn two children before either one blocks on a wait.
fn wait_child(child: std::process::Child, timeout: Duration) -> std::io::Result<Observed> {
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = child.wait_with_output();
        let _ = tx.send(out);
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            let status = classify_exit(out.status);
            Ok(Observed { status, stdout })
        }
        Ok(Err(e)) => Err(e),
        Err(_) => {
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
            let _ = rx.recv_timeout(Duration::from_secs(2));
            Ok(Observed {
                status: Status::TimedOut,
                stdout: String::new(),
            })
        }
    }
}

#[cfg(unix)]
fn classify_exit(status: std::process::ExitStatus) -> Status {
    use std::os::unix::process::ExitStatusExt as _;
    match status.signal() {
        Some(sig) => Status::Signaled(sig),
        None => Status::Exited(status.code().unwrap_or(-1)),
    }
}

#[cfg(not(unix))]
fn classify_exit(status: std::process::ExitStatus) -> Status {
    Status::Exited(status.code().unwrap_or(-1))
}

// ---------------------------------------------------------- flat parsing

fn parse_flat(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

// --------------------------------------------------------------- verdict

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Both rejected, or both accepted and every field matched.
    Agree,
    /// Both accepted; at least one field differs. Highest-priority soft
    /// finding — this is what most of this tool exists to catch.
    Mismatch,
    /// We rejected a file the reference accepted.
    Stricter,
    /// We accepted a file the reference rejected.
    Laxer,
    /// We crashed, hung, or otherwise did not simply exit. Hard finding.
    OurCrash,
    /// The reference crashed or hung. Recorded, never acted on.
    RefCrash,
}

fn is_bad(status: &Status) -> bool {
    matches!(status, Status::Signaled(_) | Status::TimedOut)
}

fn classify(ours: &Observed, reference: &Observed) -> Verdict {
    if is_bad(&ours.status) {
        return Verdict::OurCrash;
    }
    if is_bad(&reference.status) {
        return Verdict::RefCrash;
    }
    let our_ok = ours.status == Status::Exited(0);
    let ref_ok = reference.status == Status::Exited(0);
    match (our_ok, ref_ok) {
        (true, true) => {
            if parse_flat(&ours.stdout) == parse_flat(&reference.stdout) {
                Verdict::Agree
            } else {
                Verdict::Mismatch
            }
        }
        (false, false) => Verdict::Agree,
        (false, true) => Verdict::Stricter,
        (true, false) => Verdict::Laxer,
    }
}

/// Every field where the two sides disagree, formatted for a human: the key,
/// what we said, what the reference said. `None`/absent is spelled out
/// rather than silently missing from the report.
fn field_diff(ours: &Observed, reference: &Observed) -> String {
    let (om, rm) = (parse_flat(&ours.stdout), parse_flat(&reference.stdout));
    let mut keys: Vec<&String> = om.keys().chain(rm.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut out = String::new();
    for k in keys {
        match (om.get(k), rm.get(k)) {
            (Some(a), Some(b)) if a != b => {
                let _ = writeln!(out, "{k}: ours={a} reference={b}");
            }
            (Some(a), None) => {
                let _ = writeln!(out, "{k}: ours={a} reference=<absent>");
            }
            (None, Some(b)) => {
                let _ = writeln!(out, "{k}: ours=<absent> reference={b}");
            }
            _ => {}
        }
    }
    out
}

// ----------------------------------------------------------------- fnv

/// FNV-1a, for a stable, dependency-free content id — not a security hash,
/// just a short name a finding can be found again by.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

// -------------------------------------------------------------- reports

fn save_finding(
    out_dir: &Path,
    family: &str,
    seed_name: &str,
    mutant: &[u8],
    ours: &Observed,
    reference: &Observed,
    verdict: Verdict,
) -> Result<PathBuf, String> {
    fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    let id = format!("{:016x}", fnv1a64(mutant));
    let bin_path = out_dir.join(format!("{id}.bin"));
    fs::write(&bin_path, mutant).map_err(|e| format!("{}: {e}", bin_path.display()))?;

    let mut report = String::new();
    let _ = writeln!(report, "id = \"{id}\"");
    let _ = writeln!(report, "family = \"{family}\"");
    let _ = writeln!(report, "seed = \"{seed_name}\"");
    let _ = writeln!(report, "verdict = \"{verdict:?}\"");
    let _ = writeln!(report, "our_status = \"{:?}\"", ours.status);
    let _ = writeln!(report, "reference_status = \"{:?}\"", reference.status);
    if verdict == Verdict::Mismatch {
        let diff = field_diff(ours, reference);
        let _ = writeln!(report, "\n# field-by-field disagreement");
        for line in diff.lines() {
            let _ = writeln!(report, "# {line}");
        }
    }
    let toml_path = out_dir.join(format!("{id}.toml"));
    fs::write(&toml_path, report).map_err(|e| format!("{}: {e}", toml_path.display()))?;
    Ok(bin_path)
}

// ------------------------------------------------------------------ io

struct Seed {
    name: String,
    bytes: Vec<u8>,
}

fn load_seeds(dir: &Path) -> Result<Vec<Seed>, String> {
    let read = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut seeds = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        seeds.push(Seed { name, bytes });
    }
    seeds.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(seeds)
}

// ------------------------------------------------------------- commands

#[derive(Default)]
struct Tally {
    agree: u64,
    mismatch: u64,
    stricter: u64,
    laxer: u64,
    our_crash: u64,
    ref_crash: u64,
}

impl Tally {
    fn record(&mut self, v: Verdict) {
        match v {
            Verdict::Agree => self.agree += 1,
            Verdict::Mismatch => self.mismatch += 1,
            Verdict::Stricter => self.stricter += 1,
            Verdict::Laxer => self.laxer += 1,
            Verdict::OurCrash => self.our_crash += 1,
            Verdict::RefCrash => self.ref_crash += 1,
        }
    }

    fn hard_findings(&self) -> u64 {
        self.mismatch + self.our_crash
    }

    /// Field name, value pairs, in the same order the campaign already
    /// prints them — the single source of truth [`save_baseline`] and
    /// [`compare_baseline`] both read through, so a field added to the
    /// struct cannot silently go unrecorded in one but not the other.
    fn fields(&self) -> [(&'static str, u64); 6] {
        [
            ("agree", self.agree),
            ("mismatch", self.mismatch),
            ("stricter", self.stricter),
            ("laxer", self.laxer),
            ("our_crash", self.our_crash),
            ("ref_crash", self.ref_crash),
        ]
    }
}

// -------------------------------------------------------------- baseline

/// A stored campaign's tallies, one `family.field=value` line each — the
/// same flat `key=value` shape `parse_flat` already reads campaign output
/// in, reused here rather than inventing a second format. Meant to be
/// committed: a later, unattended run diffs its own tally against this file
/// and reports *drift* — a changed count — rather than a human having to
/// notice a number looks different from last time.
fn load_baseline(path: &Path) -> BTreeMap<String, u64> {
    let Ok(text) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    text.lines()
        .filter_map(|l| l.split_once('='))
        .filter_map(|(k, v)| v.trim().parse::<u64>().ok().map(|n| (k.trim().to_owned(), n)))
        .collect()
}

/// Reports every field that moved from the stored baseline, and returns
/// whether anything did. A campaign is only comparable to a baseline taken
/// with the same seed count and `--iterations` — different `--rng-seed`
/// values are the whole reproducibility story `Rng`'s own doc comment
/// already makes, so a mismatched seed is not this function's problem to
/// detect, only the caller's to keep consistent between runs.
fn compare_baseline(baseline: &BTreeMap<String, u64>, family: &str, tally: &Tally) -> bool {
    let mut drifted = false;
    for (field, value) in tally.fields() {
        let key = format!("{family}.{field}");
        match baseline.get(&key) {
            Some(&old) if old == value => println!("  baseline: {key}={value} (unchanged)"),
            Some(&old) => {
                println!("  baseline: {key} drifted {old} -> {value}");
                drifted = true;
            }
            None => println!("  baseline: {key} has no stored value (new family?)"),
        }
    }
    drifted
}

/// Replaces every `family.*` line in `baseline` with the current tally,
/// leaving every other family's lines untouched — the same "load, edit only
/// my own keys, write back" shape the private-index recipe uses for a
/// shared planning doc, applied to a shared data file instead.
fn save_baseline(path: &Path, baseline: &mut BTreeMap<String, u64>, family: &str, tally: &Tally) -> Result<(), String> {
    for (field, value) in tally.fields() {
        baseline.insert(format!("{family}.{field}"), value);
    }
    let mut out = String::new();
    for (k, v) in baseline {
        let _ = writeln!(out, "{k}={v}");
    }
    fs::write(path, out).map_err(|e| format!("{}: {e}", path.display()))
}

fn cmd_campaign(argv: &[String]) -> Result<ExitCode, String> {
    let args = Args(argv);
    let seed_dir = args.get("--seed-dir").ok_or("--seed-dir is required")?;
    let vaco_probe = args.get("--vaco-probe").ok_or("--vaco-probe is required")?;
    let ffprobe = args.get("--ffprobe").unwrap_or("ffprobe");
    let out_dir = args
        .get("--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("fuzz/seeds/diff/findings"));
    let timeout = Duration::from_millis(
        args.get("--timeout-ms")
            .map(|s| s.parse().map_err(|_| "bad --timeout-ms"))
            .transpose()?
            .unwrap_or(5000),
    );
    let iterations: Option<u64> = args
        .get("--iterations")
        .map(|s| s.parse().map_err(|_| "bad --iterations"))
        .transpose()?;
    let seconds: Option<f64> = args
        .get("--seconds")
        .map(|s| s.parse().map_err(|_| "bad --seconds"))
        .transpose()?;
    let aware = matches!(args.get("--mutator"), Some("aware"));
    let baseline_path = args.get("--baseline").map(PathBuf::from);
    let update_baseline = argv.iter().any(|a| a == "--update-baseline");
    let rng_seed: u64 = args
        .get("--rng-seed")
        .map(|s| s.parse().map_err(|_| "bad --rng-seed"))
        .transpose()?
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0xC0FF_EE00_C0FF_EE00)
        });

    let seed_dir = PathBuf::from(seed_dir);
    let family = seed_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_owned());
    let seeds = load_seeds(&seed_dir)?;
    if seeds.is_empty() {
        return Err(format!("no seed files in {}", seed_dir.display()));
    }
    println!(
        "diff_probe campaign: family={family} seeds={} rng-seed={rng_seed:#x} mutator={}",
        seeds.len(),
        if aware { "aware" } else { "generic" }
    );

    let vaco_probe = PathBuf::from(vaco_probe);
    let ffprobe = PathBuf::from(ffprobe);
    let tmp_dir = std::env::temp_dir().join(format!("vaco-diff-probe-{}", std::process::id()));
    fs::create_dir_all(&tmp_dir).map_err(|e| format!("{}: {e}", tmp_dir.display()))?;

    let mut rng = Rng::new(rng_seed);
    let mut tally = Tally::default();
    let start = Instant::now();
    let mut n: u64 = 0;
    loop {
        if let Some(max) = iterations
            && n >= max
        {
            break;
        }
        if let Some(s) = seconds
            && start.elapsed().as_secs_f64() >= s
        {
            break;
        }
        if iterations.is_none() && seconds.is_none() && n >= 500 {
            break;
        }
        n += 1;
        let seed = &seeds[rng.below(seeds.len())];
        let mutant = if aware {
            mutate_aware(&seed.bytes, &mut rng)
        } else {
            mutate(&seed.bytes, &mut rng)
        };
        let path = tmp_dir.join(format!("m{n}.bin"));
        fs::write(&path, &mutant).map_err(|e| format!("{}: {e}", path.display()))?;

        let (ours, reference) = run_probe_pair(&vaco_probe, &ffprobe, &path, timeout);
        let ours = ours?;
        let reference = reference?;
        let verdict = classify(&ours, &reference);
        tally.record(verdict);

        if matches!(verdict, Verdict::Mismatch | Verdict::OurCrash) {
            let saved = save_finding(&out_dir, &family, &seed.name, &mutant, &ours, &reference, verdict)?;
            println!(
                "  [{n}] {verdict:?} from seed {} -> {}",
                seed.name,
                saved.display()
            );
            if verdict == Verdict::Mismatch {
                for line in field_diff(&ours, &reference).lines().take(6) {
                    println!("        {line}");
                }
            }
        }
        let _ = fs::remove_file(&path);
    }
    let elapsed = start.elapsed();
    let _ = fs::remove_dir_all(&tmp_dir);

    let execs_per_sec = n as f64 / elapsed.as_secs_f64().max(0.001);
    println!(
        "diff_probe campaign done: n={n} elapsed={:.1}s execs/s={execs_per_sec:.1}",
        elapsed.as_secs_f64()
    );
    println!(
        "  agree={} mismatch={} stricter={} laxer={} our_crash={} ref_crash={}",
        tally.agree, tally.mismatch, tally.stricter, tally.laxer, tally.our_crash, tally.ref_crash
    );

    // Checking against a baseline changes what "failure" means. Without one,
    // a plain campaign run fails when it finds something, since nothing else
    // has looked at these mismatches yet. With one, the baseline is the
    // record that they were already found and are already tracked — so the
    // question this exit code answers becomes "did the count change since
    // last time", not "is the count nonzero", or every family with any known
    // mismatch would fail every cadence run forever regardless of whether
    // anything moved.
    let failed = if let Some(path) = &baseline_path {
        let mut baseline = load_baseline(path);
        if update_baseline {
            save_baseline(path, &mut baseline, &family, &tally)?;
            println!("  baseline: wrote {family}.* to {}", path.display());
            false
        } else {
            compare_baseline(&baseline, &family, &tally)
        }
    } else {
        tally.hard_findings() > 0
    };

    Ok(if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

fn cmd_replay(argv: &[String]) -> Result<ExitCode, String> {
    let args = Args(argv);
    let input = args.positional().ok_or("replay needs a file path")?;
    let vaco_probe = args.get("--vaco-probe").ok_or("--vaco-probe is required")?;
    let ffprobe = args.get("--ffprobe").unwrap_or("ffprobe");
    let timeout = Duration::from_millis(
        args.get("--timeout-ms")
            .map(|s| s.parse().map_err(|_| "bad --timeout-ms"))
            .transpose()?
            .unwrap_or(5000),
    );

    let input: &OsStr = OsStr::new(input);
    let ours = run_probe(Path::new(vaco_probe), Path::new(input), timeout)?;
    let reference = run_probe(Path::new(ffprobe), Path::new(input), timeout)?;
    let verdict = classify(&ours, &reference);
    println!("verdict: {verdict:?}");
    println!("ours:      {:?}", ours.status);
    println!("reference: {:?}", reference.status);
    if verdict == Verdict::Mismatch {
        print!("{}", field_diff(&ours, &reference));
    }
    Ok(if matches!(verdict, Verdict::Agree) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutate_never_panics_on_small_inputs() {
        for input in [&b""[..], &b"a"[..], &b"ab"[..], &b"abcd"[..]] {
            let mut rng = Rng::new(1);
            for _ in 0..64 {
                let out = mutate(input, &mut rng);
                assert!(!out.is_empty(), "mutation produced an empty buffer");
            }
        }
    }

    #[test]
    fn mutate_is_deterministic_given_the_same_seed() {
        let input = b"the quick brown fox jumps".to_vec();
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..32 {
            assert_eq!(mutate(&input, &mut a), mutate(&input, &mut b));
        }
    }

    #[test]
    fn mutate_usually_changes_the_input() {
        let input: Vec<u8> = (0..256u32).map(|b| b as u8).collect();
        let mut rng = Rng::new(7);
        let changed = (0..64).filter(|_| mutate(&input, &mut rng) != input).count();
        assert!(changed > 32, "mutation left the input alone too often: {changed}/64");
    }

    #[test]
    fn parse_flat_reads_key_value_lines() {
        let text = "format.filename=\"a.mp4\"\nstreams.stream.0.index=0\n";
        let m = parse_flat(text);
        assert_eq!(m.get("format.filename"), Some(&"\"a.mp4\"".to_owned()));
        assert_eq!(m.get("streams.stream.0.index"), Some(&"0".to_owned()));
        assert_eq!(m.len(), 2);
    }

    fn observed(status: Status, stdout: &str) -> Observed {
        Observed {
            status,
            stdout: stdout.to_owned(),
        }
    }

    #[test]
    fn both_accept_and_agree() {
        let a = observed(Status::Exited(0), "x=1\n");
        let b = observed(Status::Exited(0), "x=1\n");
        assert_eq!(classify(&a, &b), Verdict::Agree);
    }

    #[test]
    fn both_accept_but_a_field_differs_is_a_mismatch() {
        let a = observed(Status::Exited(0), "x=1\n");
        let b = observed(Status::Exited(0), "x=2\n");
        assert_eq!(classify(&a, &b), Verdict::Mismatch);
        assert_eq!(field_diff(&a, &b).trim(), "x: ours=1 reference=2");
    }

    #[test]
    fn both_reject_is_agreement_regardless_of_stderr_text() {
        let a = observed(Status::Exited(1), "");
        let b = observed(Status::Exited(183), "");
        assert_eq!(classify(&a, &b), Verdict::Agree);
    }

    #[test]
    fn we_reject_what_the_reference_accepts_is_stricter() {
        let a = observed(Status::Exited(1), "");
        let b = observed(Status::Exited(0), "x=1\n");
        assert_eq!(classify(&a, &b), Verdict::Stricter);
    }

    #[test]
    fn we_accept_what_the_reference_rejects_is_laxer() {
        let a = observed(Status::Exited(0), "x=1\n");
        let b = observed(Status::Exited(1), "");
        assert_eq!(classify(&a, &b), Verdict::Laxer);
    }

    #[test]
    fn our_crash_outranks_every_other_classification() {
        let a = observed(Status::Signaled(11), "");
        let b = observed(Status::Exited(1), "");
        assert_eq!(classify(&a, &b), Verdict::OurCrash);
        // Even when the reference also had a bad day, ours is reported first.
        let b_bad = observed(Status::TimedOut, "");
        assert_eq!(classify(&a, &b_bad), Verdict::OurCrash);
    }

    #[test]
    fn reference_crash_is_recorded_not_ours() {
        let a = observed(Status::Exited(0), "x=1\n");
        let b = observed(Status::Signaled(6), "");
        assert_eq!(classify(&a, &b), Verdict::RefCrash);
    }

    #[test]
    fn run_with_timeout_reports_a_normal_exit() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "exit 3"]);
        let observed = run_with_timeout(cmd, Duration::from_secs(5)).expect("spawn");
        assert_eq!(observed.status, Status::Exited(3));
    }

    #[test]
    fn run_with_timeout_kills_a_hanging_process() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "sleep 30"]);
        let observed = run_with_timeout(cmd, Duration::from_millis(200)).expect("spawn");
        assert_eq!(observed.status, Status::TimedOut);
    }

    #[test]
    fn run_with_timeout_reports_a_signal_as_signaled_not_exited() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "kill -ABRT $$"]);
        let observed = run_with_timeout(cmd, Duration::from_secs(5)).expect("spawn");
        assert!(matches!(observed.status, Status::Signaled(_)));
    }

    #[test]
    fn spawning_two_children_before_waiting_runs_them_concurrently() {
        // Two 300ms sleeps, spawned before either is waited on: total wall
        // time should be close to one sleep, not the sum of both — the
        // entire point of `run_probe_pair` over two sequential
        // `run_with_timeout` calls.
        let start = Instant::now();
        let mut a = Command::new("/bin/sh");
        a.args(["-c", "sleep 0.3"]);
        let mut b = Command::new("/bin/sh");
        b.args(["-c", "sleep 0.3"]);
        let a = a.spawn().expect("spawn a");
        let b = b.spawn().expect("spawn b");
        let oa = wait_child(a, Duration::from_secs(5)).expect("wait a");
        let ob = wait_child(b, Duration::from_secs(5)).expect("wait b");
        assert_eq!(oa.status, Status::Exited(0));
        assert_eq!(ob.status, Status::Exited(0));
        assert!(
            start.elapsed() < Duration::from_millis(550),
            "two 300ms children spawned together took {:?}, expected well under 600ms",
            start.elapsed()
        );
    }

    #[test]
    fn run_probe_pair_reports_each_sides_own_exit_status() {
        // Stand-ins for `vaco-probe`/`ffprobe`: neither understands the flat
        // `-v quiet -of flat ...` args `probe_command` appends, but `true`/
        // `false` ignore all arguments and exit on their own status alone,
        // which is all this test needs from them.
        let (a, b) = run_probe_pair(
            Path::new("/usr/bin/true"),
            Path::new("/usr/bin/false"),
            Path::new("input.bin"),
            Duration::from_secs(5),
        );
        assert_eq!(a.expect("true should spawn").status, Status::Exited(0));
        assert_eq!(b.expect("false should spawn").status, Status::Exited(1));
    }

    // -------------------------------------------------------- baseline

    fn tally(agree: u64, mismatch: u64) -> Tally {
        Tally {
            agree,
            mismatch,
            stricter: 0,
            laxer: 0,
            our_crash: 0,
            ref_crash: 0,
        }
    }

    #[test]
    fn load_baseline_of_a_missing_file_is_empty_not_an_error() {
        assert!(load_baseline(Path::new("/nonexistent/does-not-exist.baseline")).is_empty());
    }

    #[test]
    fn save_then_load_baseline_round_trips_every_field() {
        let dir = std::env::temp_dir().join(format!("diff-probe-baseline-test-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("baseline.txt");
        let mut map = BTreeMap::new();
        save_baseline(&path, &mut map, "mp4", &tally(335, 21)).expect("save");
        let loaded = load_baseline(&path);
        assert_eq!(loaded.get("mp4.agree"), Some(&335));
        assert_eq!(loaded.get("mp4.mismatch"), Some(&21));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_baseline_does_not_disturb_another_familys_lines() {
        let dir = std::env::temp_dir().join(format!("diff-probe-baseline-test2-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("baseline.txt");
        let mut map = BTreeMap::new();
        save_baseline(&path, &mut map, "mp4", &tally(335, 21)).expect("save mp4");
        save_baseline(&path, &mut map, "wav", &tally(4, 196)).expect("save wav");
        let loaded = load_baseline(&path);
        assert_eq!(loaded.get("mp4.agree"), Some(&335), "wav's save must not erase mp4's line");
        assert_eq!(loaded.get("wav.agree"), Some(&4));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compare_baseline_reports_no_drift_when_the_tally_matches() {
        let mut baseline = BTreeMap::new();
        for (k, v) in tally(335, 21).fields() {
            baseline.insert(format!("mp4.{k}"), v);
        }
        assert!(!compare_baseline(&baseline, "mp4", &tally(335, 21)));
    }

    #[test]
    fn compare_baseline_reports_drift_when_a_field_moved() {
        let mut baseline = BTreeMap::new();
        for (k, v) in tally(335, 21).fields() {
            baseline.insert(format!("mp4.{k}"), v);
        }
        assert!(compare_baseline(&baseline, "mp4", &tally(335, 34)));
    }

    #[test]
    fn compare_baseline_is_not_drift_for_a_family_with_no_stored_baseline() {
        // A brand-new family has nothing to compare against yet; that is a
        // reason to record one with --update-baseline, not a regression.
        assert!(!compare_baseline(&BTreeMap::new(), "ogg", &tally(3, 0)));
    }

    #[test]
    fn fnv1a64_is_stable_and_distinguishes_inputs() {
        assert_eq!(fnv1a64(b"abc"), fnv1a64(b"abc"));
        assert_ne!(fnv1a64(b"abc"), fnv1a64(b"abd"));
    }

    // ------------------------------------------------------ format-aware

    /// A minimal RIFF/WAVE: `RIFF` + 4-byte LE size + `WAVE` + one `fmt `
    /// chunk with its own 4-byte LE size, body all zero.
    fn tiny_riff() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&20u32.to_le_bytes()); // "WAVE" + fmt chunk header+body
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&8u32.to_le_bytes());
        buf.extend_from_slice(&[0u8; 8]);
        buf
    }

    #[test]
    fn chunk_length_fields_finds_the_riff_size_and_the_fmt_size() {
        let buf = tiny_riff();
        let offsets: Vec<usize> = chunk_length_fields(&buf).into_iter().map(|(at, _)| at).collect();
        assert!(offsets.contains(&4), "RIFF's own size field at offset 4: {offsets:?}");
        assert!(
            offsets.contains(&16),
            "fmt chunk's size field right after its tag: {offsets:?}"
        );
    }

    #[test]
    fn chunk_length_fields_ignores_a_buffer_with_no_tag_shape() {
        let buf: Vec<u8> = (0..64u32).map(|b| (b % 256) as u8).collect();
        // Not asserting empty — four low-value bytes can coincidentally look
        // ASCII-tag-shaped — only that it does not explode and stays small
        // relative to the input, i.e. it is not matching everywhere.
        assert!(chunk_length_fields(&buf).len() < buf.len());
    }

    #[test]
    fn ogg_page_fields_finds_the_segment_count_and_checksum() {
        let mut buf = vec![0u8; 30];
        buf[0..4].copy_from_slice(b"OggS");
        let offsets = ogg_page_fields(&buf);
        assert!(offsets.contains(&(22, 4)), "checksum: {offsets:?}");
        assert!(offsets.contains(&(26, 1)), "page_segments: {offsets:?}");
    }

    #[test]
    fn ogg_page_fields_finds_every_page_not_just_the_first() {
        let mut buf = vec![0u8; 60];
        buf[0..4].copy_from_slice(b"OggS");
        buf[30..34].copy_from_slice(b"OggS");
        assert_eq!(ogg_page_fields(&buf).len(), 4, "two pages, two fields each");
    }

    #[test]
    fn flv_tag_size_fields_walks_the_tag_stream() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"FLV\x01\x00\x00\x00\x00\x09");
        buf.extend_from_slice(&0u32.to_be_bytes()); // PreviousTagSize0
        // One tag: type=8 (audio), DataSize=5, timestamp+ext+streamid, 5 data bytes.
        buf.push(8);
        buf.extend_from_slice(&[0x00, 0x00, 0x05]); // DataSize = 5
        buf.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // timestamp+ext+streamid
        buf.extend_from_slice(&[0u8; 5]); // data
        buf.extend_from_slice(&16u32.to_be_bytes()); // PreviousTagSize1
        let offsets = flv_tag_size_fields(&buf);
        assert_eq!(offsets, vec![(14, 3)], "DataSize field of the one tag");
    }

    #[test]
    fn flv_tag_size_fields_is_empty_for_a_non_flv_buffer() {
        assert!(flv_tag_size_fields(b"not an flv file at all, just text").is_empty());
    }

    #[test]
    fn mutate_aware_never_panics_on_small_or_structured_inputs() {
        for input in [&b""[..], &b"a"[..], &tiny_riff()[..]] {
            let mut rng = Rng::new(3);
            for _ in 0..64 {
                let out = mutate_aware(input, &mut rng);
                assert!(!out.is_empty());
            }
        }
    }

    #[test]
    fn mutate_aware_is_deterministic_given_the_same_seed() {
        let input = tiny_riff();
        let mut a = Rng::new(99);
        let mut b = Rng::new(99);
        for _ in 0..32 {
            assert_eq!(mutate_aware(&input, &mut a), mutate_aware(&input, &mut b));
        }
    }

    #[test]
    fn mutate_aware_falls_back_to_generic_on_unstructured_input() {
        // No recognisable tag/page/tag-stream shape at all: every offset
        // list is empty, so this must behave exactly like `mutate`.
        let input = vec![1u8, 2, 3];
        let mut a = Rng::new(5);
        let mut b = Rng::new(5);
        assert_eq!(mutate_aware(&input, &mut a), mutate(&input, &mut b));
    }
}
