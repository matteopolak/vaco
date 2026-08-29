//! The PR-12c / issue #563 acceptance test, read literally: *"the
//! shared-memory transport round-trips between two local processes."*
//!
//! `harness = false` (see `Cargo.toml`) because this test needs a real
//! `fn main` it controls: it spawns **two genuinely separate OS processes** by
//! re-executing this very binary (`Command::new(current_exe())`) with a role
//! environment variable, rather than two threads sharing one address space.
//! Two threads would exercise the same `UnixDatagram`s the same way but would
//! prove nothing about the filesystem-path rendezvous or cross-process
//! delivery the acceptance criterion actually asks about.
//!
//! Per `planning/AGENT-CONSTRAINTS.md`: no step here is allowed to "skip on
//! error". Both roles are this same compiled test binary — there is no
//! external tool whose absence could be mistaken for success — so every
//! non-zero exit and every timeout is a genuine, reported failure.
//!
//! `unwrap`/`expect`/`panic` are the correct way to fail a `harness = false`
//! test binary (there is no `#[test]` machinery to return a `Result` to) —
//! this file is test orchestration, not the production code the workspace
//! lint otherwise targets.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "manual-main harness=false test binary; failing loudly IS the test"
)]

use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

use vaco_io::CancelToken;
use vaco_opts::Dict;
use vaco_protocol_core::{IoFlags, ProtocolEnv, ProtocolRegistry};

const ROLE_VAR: &str = "VACO_SHARED_TEST_ROLE";
const NAME_VAR: &str = "VACO_SHARED_TEST_NAME";
const PAYLOAD: &[u8] = b"vaco-shared-round-trip";

/// How long the producer keeps re-sending. Generous relative to the
/// consumer's own subscribe timeout below so a slow CI scheduler cannot make
/// this flaky in the direction that matters (a spurious failure): the
/// consumer's registration can land anywhere in this window and will still
/// see the *next* write.
const PRODUCER_DURATION: Duration = Duration::from_secs(6);
const PRODUCER_INTERVAL: Duration = Duration::from_millis(20);

/// The consumer's own subscribe-handshake timeout, passed as the `timeout`
/// option (milliseconds). Comfortably inside `PRODUCER_DURATION`.
const CONSUMER_SUBSCRIBE_TIMEOUT_MS: &str = "5000";

/// Wall-clock bound the *orchestrator* enforces on each child, independent of
/// whatever timeout the child believes it is honouring. This is what turns "a
/// bug makes the child hang forever" into a reported test failure instead of
/// a wedged `cargo test` process.
const WATCHDOG: Duration = Duration::from_secs(15);

fn build_registry() -> ProtocolRegistry {
    let mut registry = ProtocolRegistry::new();
    vaco_protocol_shared::register(&mut registry);
    registry
}

fn run_producer(name: &str) -> i32 {
    let registry = build_registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&registry, &cancel);
    let url = format!("shared:{name}");

    let mut sink = match registry.create(&url, IoFlags::WRITE, &Dict::new(), &env) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("producer: create({url}) failed: {e}");
            return 1;
        }
    };

    let deadline = Instant::now() + PRODUCER_DURATION;
    while Instant::now() < deadline {
        if let Err(e) = sink.write(PAYLOAD) {
            eprintln!("producer: write failed: {e}");
            return 1;
        }
        std::thread::sleep(PRODUCER_INTERVAL);
    }
    0
}

fn run_consumer(name: &str) -> i32 {
    let registry = build_registry();
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&registry, &cancel);
    let url = format!("shared:{name}");

    let mut opts = Dict::new();
    opts.set("timeout", CONSUMER_SUBSCRIBE_TIMEOUT_MS);

    let mut source = match registry.open(&url, IoFlags::READ, &opts, &env) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("consumer: open({url}) failed: {e}");
            return 1;
        }
    };

    let mut buf = [0u8; 256];
    let n = match source.read(&mut buf) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("consumer: read failed: {e}");
            return 1;
        }
    };

    let Some(received) = buf.get(..n) else {
        eprintln!("consumer: read reported {n} bytes into a {}-byte buffer", buf.len());
        return 1;
    };
    if received == PAYLOAD {
        0
    } else {
        eprintln!("consumer: payload mismatch: got {received:?}, want {PAYLOAD:?}");
        1
    }
}

/// Poll `child` up to `WATCHDOG`, killing and reporting it as a failure if it
/// never exits — a hang is a failure here, not something the harness waits
/// out silently.
fn wait_bounded(label: &str, mut child: Child) -> Result<ExitStatus, String> {
    let deadline = Instant::now() + WATCHDOG;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(e) => return Err(format!("{label}: try_wait failed: {e}")),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{label}: did not exit within {WATCHDOG:?} — killed"
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn spawn_role(role: &str, name: &str) -> std::io::Result<Child> {
    let exe = std::env::current_exe()?;
    Command::new(exe)
        .env(ROLE_VAR, role)
        .env(NAME_VAR, name)
        .spawn()
}

fn orchestrate() {
    // Unique per test run so repeated or concurrent runs (this machine hosts
    // other agents' work in the same tree) never collide on one rendezvous
    // directory.
    let name = format!(
        "test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );

    // Start the consumer first: its subscribe handshake retries for
    // `CONSUMER_SUBSCRIBE_TIMEOUT_MS`, so it is fine for the producer to bind
    // its `register` socket slightly later. Starting the consumer first is
    // the harder ordering to get right and is exactly what a real multi-process
    // fan-out needs to tolerate.
    let consumer = spawn_role("consumer", &name).expect("failed to spawn consumer process");
    let producer = spawn_role("producer", &name).expect("failed to spawn producer process");

    let producer_status =
        wait_bounded("producer", producer).unwrap_or_else(|e| panic!("{e}"));
    let consumer_status =
        wait_bounded("consumer", consumer).unwrap_or_else(|e| panic!("{e}"));

    assert!(
        producer_status.success(),
        "producer process exited with {producer_status:?}"
    );
    assert!(
        consumer_status.success(),
        "consumer process exited with {consumer_status:?} — the round trip did not complete"
    );
}

fn main() {
    match std::env::var(ROLE_VAR).ok().as_deref() {
        Some("producer") => {
            let name = std::env::var(NAME_VAR).expect("producer role requires the name env var");
            std::process::exit(run_producer(&name));
        }
        Some("consumer") => {
            let name = std::env::var(NAME_VAR).expect("consumer role requires the name env var");
            std::process::exit(run_consumer(&name));
        }
        Some(other) => panic!("unknown {ROLE_VAR}: {other}"),
        None => orchestrate(),
    }
}
