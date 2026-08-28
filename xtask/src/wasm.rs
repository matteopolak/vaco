//! Every library still builds for `wasm32-unknown-unknown` (D18).
//!
//! # Why per-crate and not `--workspace`
//!
//! `cargo check --workspace --target wasm32-unknown-unknown` fails, and not for
//! a reason that matters: `proptest` pulls `rand_core` and `tempfile` pull
//! `getrandom`, which `compile_error!`s on wasm without its `js` feature. Those
//! are **dev**-dependencies — `cargo tree -e normal -i getrandom` finds nothing
//! — so no shipped library is affected, but a workspace-wide check resolves one
//! unified feature graph and drags them in anyway.
//!
//! Building each library on its own is therefore both the accurate question and
//! the one we can actually answer: *does this crate, as shipped, compile for
//! wasm?* Test binaries are explicitly out of scope; we do not run the suite on
//! wasm.
//!
//! # The allowlist is the interesting part
//!
//! A crate is portable unless it is on [`NATIVE_ONLY`]. That default is the
//! point: a new crate is portable, and making one native-only is a deliberate,
//! reviewed act that leaves a note saying why — the same shape as the unsafe
//! audit's exemption list. The alternative default would let OS coupling spread
//! silently, which is exactly what D18 exists to prevent.

use std::process::Command;

use crate::{Task, crates, repo_root};

/// Crates that legitimately cannot build for wasm, each with the reason.
///
/// Kept as short as the sockets/TLS layer actually requires, and no shorter:
/// `vaco-time` exists so that the clock — the one thing that genuinely panics
/// on wasm — is behind a single door instead of being a reason to add entries
/// here. Every entry below is a real, measured build failure (`E0308`/`E0061`/
/// `E0583` for `socket2`, `getrandom`'s own `compile_error!` for
/// `rustls-rustcrypto`), not a precaution.
const NATIVE_ONLY: &[(&str, &str)] = &[
    (
        "vaco-protocol-http",
        "ureq + rustls is a socket-and-TLS stack; wasm32-unknown-unknown has no \
         sockets, and a browser port would go through fetch rather than this crate. \
         Portability here means a *different* protocol implementation behind the \
         same `vaco-protocol-core` trait, which is the D11 adapter rule doing its \
         job — not a wasm build of this one.",
    ),
    (
        "vaco-protocol-socket",
        "depends on socket2 for tcp:/udp:/udplite:/unix:. Measured, not assumed: \
         a throwaway crate depending on socket2 alone fails to build for \
         wasm32-unknown-unknown with nine E0308/E0061/E0583 errors inside \
         socket2 itself (it assumes std::net::{TcpStream,TcpListener,UdpSocket} \
         exist, which they do not on that target — std::net alone does compile \
         there, as a stub returning io::Error rather than a compile_error!, but \
         socket2 does not). A wasm build reaches a socket through the host \
         runtime's own APIs (WebSocket/WebTransport), a different protocol \
         behind the same `vaco-protocol-core` trait — the same D11 argument as \
         vaco-protocol-http's entry above.",
    ),
    (
        "vaco-protocol-tls",
        "depends on rustls-rustcrypto, which pulls getrandom without wasm's js \
         feature enabled and fails wasm32-unknown-unknown on getrandom's own \
         hard compile_error! (measured directly against a throwaway crate \
         depending on rustls-rustcrypto alone — the same wall vaco-registry's \
         own vaco-component.toml fragments already document for \
         vaco-protocol-http). A wasm build reaches TLS through the browser's \
         own TLS-terminated fetch/WebSocket, not this crate.",
    ),
    (
        "vaco-demux-rtsp",
        "RTSP's control connection is inherently duplex (send a request, read \
         its response) before there is anything to hand a caller, so — exactly \
         like vaco-protocol-tls's own connect module — this crate connects its \
         own std::net::TcpStream directly via vaco_protocol_socket::addr::connect \
         rather than going through vaco-protocol-core's read-only Protocol::open, \
         and its SETUP-negotiated UDP transports go through \
         vaco_protocol_socket's registered udp: protocol. Both paths pull in \
         vaco-protocol-socket, which is itself NATIVE_ONLY above for depending \
         on socket2 (measured: E0583 'file not found for module `sys`' when \
         cargo check --target wasm32-unknown-unknown is run on this crate, the \
         same underlying wall one level removed). A wasm build reaches RTSP \
         through the host runtime's own duplex socket API, a different \
         transport behind the same seam, not a wasm build of this crate.",
    ),
    (
        "vaco-protocol-httpproxy",
        "depends on vaco-protocol-socket for HostPort/addr::connect (the CONNECT \
         handshake dials its own duplex TcpStream directly, exactly like \
         vaco-protocol-tls, rather than through the registry's one-directional \
         Protocol::open/create — see the crate docs), so it inherits the same \
         socket2 wall vaco-protocol-socket's own NATIVE_ONLY entry documents \
         (measured: cargo build --target wasm32-unknown-unknown on this crate \
         fails inside socket2 itself with E0583 'file not found for module \
         `sys`'). A wasm build reaches an HTTP proxy tunnel through the host \
         runtime's own fetch/WebSocket machinery, not this crate.",
    ),
];

const TARGET: &str = "wasm32-unknown-unknown";

pub fn run(_check: bool) -> Task {
    let root = repo_root();

    // Fail loudly rather than passing vacuously when the target is missing.
    let installed = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map_err(|e| format!("rustup: {e}"))?;
    if !String::from_utf8_lossy(&installed.stdout).contains(TARGET) {
        return Err(format!(
            "the {TARGET} target is not installed; run `rustup target add {TARGET}`"
        ));
    }

    let mut failed = Vec::new();
    let mut checked = 0_usize;

    for (_layer, name, path) in crates() {
        if let Some((_, why)) = NATIVE_ONLY.iter().find(|(n, _)| *n == name) {
            println!("  skip {name}: {why}");
            continue;
        }
        // Binary-only crates have no library to check; `--lib` errors on them.
        if !path.join("src/lib.rs").exists() {
            continue;
        }
        let out = Command::new("cargo")
            .current_dir(&root)
            .args([
                "build",
                "-p",
                &name,
                "--lib",
                "--target",
                TARGET,
                "--target-dir",
                "/tmp/vaco-wasm",
                "-q",
            ])
            .output()
            .map_err(|e| format!("cargo: {e}"))?;
        checked += 1;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let mut after = err.lines().skip_while(|l| !l.starts_with("error"));
            let first = after.next().unwrap_or("(no error line)").to_string();
            // Blame the crate the error is IN, not the one we asked cargo for.
            // Building `-p a` also builds `a`'s dependencies, so the first error
            // is often in a different crate — and a gate that names the wrong
            // one sends people to the wrong file. Take it from the `-->` path.
            //
            // Scan from the error line, not from the top: a warning carries a
            // `-->` too, and warnings come first. The version that searched all
            // of stderr blamed `vaco-protocol-wrap` — whose only crime was an
            // unused import — for four `socket2` failures it had no part in.
            let blame = after
                .take_while(|l| !l.starts_with("error") && !l.starts_with("warning"))
                .find_map(|l| l.trim().strip_prefix("--> "))
                .and_then(blame_from_path)
                .unwrap_or_else(|| name.clone());
            failed.push((blame, first));
        }
    }

    if !failed.is_empty() {
        let mut msg = format!(
            "{} crate(s) no longer build for {TARGET} (D18):\n",
            failed.len()
        );
        for (name, why) in &failed {
            msg.push_str(&format!("  {name}: {why}\n"));
        }
        msg.push_str(
            "\nPut the OS-coupled part behind an abstraction rather than adding \
             an entry to NATIVE_ONLY. `vaco-time` is the worked example: the \
             clock is the one API that genuinely panics on wasm, and it lives in \
             one crate so the port is one file.",
        );
        return Err(msg);
    }
    println!("wasm-check: {checked} libraries build for {TARGET}");
    // Mid-wave this can report an agent's half-written crate as a wasm failure.
    // That is not a false positive worth engineering away: the gate's job is the
    // committed tree, and CI is where it runs.
    Ok(())
}

/// The workspace crate a `--> path` points into, if it points into one.
///
/// Only `crates/<area>/<crate>/…` counts. The first version took the third
/// path component unconditionally, which is right for a repo-relative path and
/// nonsense for anything else: an error inside a registry dependency lives at
/// `/Users/<you>/.cargo/registry/…`, so the gate blamed a crate called
/// **`matthew`** and sent the reader to a directory that does not exist.
///
/// Returning `None` for a path we do not recognise lets the caller fall back to
/// the crate it actually asked cargo for, which is a worse answer than the
/// truth and a much better one than a username.
fn blame_from_path(loc: &str) -> Option<String> {
    let mut parts = loc.split('/');
    if parts.next()? != "crates" {
        return None;
    }
    let _area = parts.next()?;
    let krate = parts.next()?;
    krate.starts_with("vaco-").then(|| krate.to_owned())
}

#[cfg(test)]
mod tests {
    use super::blame_from_path;

    #[test]
    fn a_repo_relative_path_names_its_crate() {
        assert_eq!(
            blame_from_path("crates/io/vaco-protocol-socket/src/sys.rs:3:1").as_deref(),
            Some("vaco-protocol-socket")
        );
    }

    /// The bug this function exists for: an error inside a registry dependency
    /// lives under `/Users/<you>/.cargo/registry/…`, and taking the third path
    /// component blamed a crate named after the developer.
    #[test]
    fn an_absolute_registry_path_blames_nobody_rather_than_the_username() {
        for loc in [
            "/Users/matthew/.cargo/registry/src/index.crates.io-1949cf/socket2-0.6.0/src/sys.rs",
            "/some/other/place/lib.rs",
            "crates/io/not-a-vaco-crate/src/lib.rs",
            "crates",
            "",
        ] {
            assert_eq!(blame_from_path(loc), None, "{loc}");
        }
    }
}
