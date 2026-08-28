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
//! Generic, structure-blind operators only (bitflip, byte overwrite,
//! truncate, byte insertion, chunk duplication, and overwriting four bytes
//! with a length-field-shaped "interesting value"). A box/EBML/TS-aware
//! mutator reaches deeper code per input, but is its own multi-day piece of
//! work — this is deliberately the cheap tier, sufficient to find the classes
//! of bug a generic byte fuzzer finds (truncation handling, arithmetic on
//! attacker-controlled lengths), not a replacement for one.

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
             [--iterations N | --seconds S] [--out <dir>] [--timeout-ms N] [--rng-seed N]\n  \
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
fn run_probe(bin: &Path, input: &Path, timeout: Duration) -> Result<Observed, String> {
    let mut cmd = Command::new(bin);
    cmd.args(["-v", "quiet", "-of", "flat", "-show_format", "-show_streams"])
        .arg(input)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    run_with_timeout(cmd, timeout).map_err(|e| format!("running {}: {e}", bin.display()))
}

/// Spawns `cmd`, waits up to `timeout`, and kills it (by pid, via the `kill`
/// utility — no signal-handling dependency needed for that) if it has not
/// finished. A hang is not a shrug: it is a real denial-of-service finding
/// for anything reading untrusted media, exactly as much as a crash.
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> std::io::Result<Observed> {
    let child = cmd.spawn()?;
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
        "diff_probe campaign: family={family} seeds={} rng-seed={rng_seed:#x}",
        seeds.len()
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
        let mutant = mutate(&seed.bytes, &mut rng);
        let path = tmp_dir.join(format!("m{n}.bin"));
        fs::write(&path, &mutant).map_err(|e| format!("{}: {e}", path.display()))?;

        let ours = run_probe(&vaco_probe, &path, timeout)?;
        let reference = run_probe(&ffprobe, &path, timeout)?;
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

    Ok(if tally.hard_findings() > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
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
    fn fnv1a64_is_stable_and_distinguishes_inputs() {
        assert_eq!(fnv1a64(b"abc"), fnv1a64(b"abc"));
        assert_ne!(fnv1a64(b"abc"), fnv1a64(b"abd"));
    }
}
