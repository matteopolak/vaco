//! Validate an LLVM PGO profile without compiling the workspace.
//!
//! The profile parser is shared with the fixture-driven Python tests and keeps
//! this gate independent of Cargo's (large) build graph. `llvm-profdata` remains
//! the authority for reading the binary profile; Python only applies the
//! repository's coverage and anti-overfit policy to its textual dump.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{Task, repo_root};

fn llvm_profdata() -> Result<String, String> {
    if let Ok(tool) = std::env::var("LLVM_PROFDATA") {
        if !tool.is_empty() {
            return Ok(tool);
        }
    }

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let sysroot = Command::new(&rustc)
        .args(["--print", "sysroot"])
        .output()
        .map_err(|e| format!("profile-coverage: could not query {rustc}: {e}"))?;
    let version = Command::new(&rustc)
        .arg("-vV")
        .output()
        .map_err(|e| format!("profile-coverage: could not query {rustc}: {e}"))?;
    if sysroot.status.success() && version.status.success() {
        let sysroot = String::from_utf8_lossy(&sysroot.stdout).trim().to_owned();
        let version_text = String::from_utf8_lossy(&version.stdout);
        let host = version_text
            .lines()
            .find_map(|line| line.strip_prefix("host: "));
        if let Some(host) = host {
            let bundled = PathBuf::from(sysroot)
                .join("lib/rustlib")
                .join(host)
                .join("bin/llvm-profdata");
            if bundled.is_file() {
                return Ok(bundled.to_string_lossy().into_owned());
            }
        }
    }
    Ok("llvm-profdata".to_owned())
}

pub fn run(args: &[String]) -> Task {
    let profile = args
        .first()
        .ok_or("profile-coverage needs a path to a .profdata file")?;
    let manifest = args
        .get(1)
        .map_or_else(|| repo_root().join("profile/workload.toml"), PathBuf::from);
    if !manifest.exists() {
        return Err(format!(
            "profile-coverage: manifest not found: {}",
            manifest.display()
        ));
    }
    let tool = llvm_profdata()?;
    let output = Command::new(&tool)
        .args(["show", "--all-functions", "--counts", profile])
        .output()
        .map_err(|e| format!("profile-coverage: could not run {tool}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "profile-coverage: {tool} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("profile-coverage: clock failed: {e}"))?
        .as_nanos();
    let dump = std::env::temp_dir().join(format!("vaco-pgo-{}-{stamp}.txt", std::process::id()));
    fs::write(&dump, &output.stdout).map_err(|e| format!("profile-coverage: {dump:?}: {e}"))?;
    let result = Command::new("python3")
        .current_dir(repo_root())
        .args(["scripts/pgo.py", "check-profile"])
        .arg(&manifest)
        .arg(&dump)
        .status()
        .map_err(|e| format!("profile-coverage: could not run Python checker: {e}"));
    let _ = fs::remove_file(&dump);
    result.and_then(|status| {
        if status.success() {
            Ok(())
        } else {
            Err("profile-coverage: coverage or anti-overfit check failed".to_owned())
        }
    })
}
