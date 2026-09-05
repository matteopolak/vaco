//! Whole-process, corpus-backed macro benchmark execution.
//!
//! A scenario supplies two command lines with `{input}` and `{output}`
//! placeholders. The runner writes the already SHA-256-verified corpus asset
//! to its sandbox, alternates Vaco/reference order every repetition, and
//! verifies both output files against one declared checksum before keeping any
//! timing. This is intentionally command-agnostic: S1--S10 differ in their
//! media arguments, but not in the safety rules for a useful measurement.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use vaco_corpus::ObjectId;

use crate::BenchError;

/// The two implementations measured for a macro scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Implementation {
    /// The Vaco command under test.
    Vaco,
    /// The reference command, normally `ffmpeg` or `dav1d` for AV1 decode.
    Reference,
}

impl Implementation {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Vaco => "vaco",
            Self::Reference => "reference",
        }
    }
}

/// One fully specified whole-process scenario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroScenario {
    /// Stable S1--S10 scenario/configuration identifier.
    pub name: String,
    /// SHA-256-verified corpus entry name.
    pub asset: String,
    /// Vaco executable and its template arguments.
    pub vaco: CommandTemplate,
    /// Reference executable and its template arguments.
    pub reference: CommandTemplate,
    /// SHA-256 of the required useful output from both commands.
    pub expected_output: ObjectId,
}

/// An executable plus arguments containing exact `{input}`/`{output}` tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandTemplate {
    /// Program path or command name.
    pub program: PathBuf,
    /// Arguments. Placeholder replacement happens only for whole arguments.
    pub args: Vec<String>,
}

/// One successful whole-process measurement.
#[derive(Debug, Clone, PartialEq)]
pub struct MacroSample {
    /// Scenario identifier.
    pub scenario: String,
    /// Which executable produced this sample.
    pub implementation: Implementation,
    /// Zero-based repetition number.
    pub round: usize,
    /// Monotonic wall time for the child process.
    pub wall_ns: f64,
}

/// Execute at least eleven alternating Vaco/reference repetitions.
///
/// The caller must run machine-control preflight before this function. This
/// separation lets fixture tests validate interleaving and output rejection on
/// any host while the CLI remains fail-closed for production macro runs.
///
/// # Errors
///
/// Returns an error for fewer than eleven rounds, an absent input, a failed
/// child command, a missing output, or an output that does not match the
/// declared useful-output digest.
pub fn run_macro_scenario(
    scenario: &MacroScenario,
    input: &Path,
    rounds: usize,
    sandbox: &Path,
) -> Result<Vec<MacroSample>, BenchError> {
    if rounds < 11 {
        return Err(BenchError::Macro(
            "macro benchmarks require at least 11 interleaved rounds".to_owned(),
        ));
    }
    if !input.is_file() {
        return Err(BenchError::Macro(format!(
            "{}: verified corpus input {} is not a file",
            scenario.name,
            input.display()
        )));
    }
    let mut samples = Vec::new();
    for round in 0..rounds {
        let order = if round % 2 == 0 {
            [Implementation::Vaco, Implementation::Reference]
        } else {
            [Implementation::Reference, Implementation::Vaco]
        };
        for implementation in order {
            let output = sandbox.join(format!("{}-{round}.out", implementation.name()));
            let template = match implementation {
                Implementation::Vaco => &scenario.vaco,
                Implementation::Reference => &scenario.reference,
            };
            let wall_ns = run_template(template, input, &output, &scenario.expected_output)?;
            samples.push(MacroSample {
                scenario: scenario.name.clone(),
                implementation,
                round,
                wall_ns,
            });
        }
    }
    Ok(samples)
}

fn run_template(
    template: &CommandTemplate,
    input: &Path,
    output: &Path,
    expected: &ObjectId,
) -> Result<f64, BenchError> {
    let _ = fs::remove_file(output);
    let mut command = Command::new(&template.program);
    for argument in &template.args {
        let value = match argument.as_str() {
            "{input}" => input.as_os_str().to_string_lossy().into_owned(),
            "{output}" => output.as_os_str().to_string_lossy().into_owned(),
            _ => argument.clone(),
        };
        command.arg(value);
    }
    let start = Instant::now();
    let status = command.status().map_err(BenchError::Io)?;
    let elapsed = start.elapsed().as_secs_f64() * 1_000_000_000.0;
    if !status.success() {
        return Err(BenchError::Macro(format!(
            "{} exited with {status}",
            template.program.display()
        )));
    }
    let bytes = fs::read(output).map_err(|error| {
        BenchError::Macro(format!(
            "{} did not produce {}: {error}",
            template.program.display(),
            output.display()
        ))
    })?;
    let actual = ObjectId::of(&bytes);
    if actual != *expected {
        return Err(BenchError::Macro(format!(
            "{} produced SHA-256 {actual}, expected {expected}",
            template.program.display()
        )));
    }
    Ok(elapsed)
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "test fixture setup uses direct diagnostics"
)]
mod tests {
    use super::{CommandTemplate, Implementation, MacroScenario, run_macro_scenario};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};
    use vaco_corpus::ObjectId;

    fn directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("vaco-macro-{label}-{nonce}"));
        fs::create_dir_all(&directory).expect("create fixture directory");
        directory
    }

    fn scenario() -> MacroScenario {
        let program = if cfg!(windows) { "cmd" } else { "sh" };
        let args = if cfg!(windows) {
            vec!["/C".to_owned(), "copy /Y {input} {output}".to_owned()]
        } else {
            vec![
                "-c".to_owned(),
                "cp \"$1\" \"$2\"".to_owned(),
                "copy".to_owned(),
                "{input}".to_owned(),
                "{output}".to_owned(),
            ]
        };
        MacroScenario {
            name: "S10/test".to_owned(),
            asset: "fixture".to_owned(),
            vaco: CommandTemplate {
                program: program.into(),
                args: args.clone(),
            },
            reference: CommandTemplate {
                program: program.into(),
                args,
            },
            expected_output: ObjectId::of(b"fixture bytes"),
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn alternates_order_and_checks_every_output() {
        let directory = directory("alternating");
        let input = directory.join("input");
        fs::write(&input, b"fixture bytes").expect("write input");
        let samples =
            run_macro_scenario(&scenario(), &input, 11, &directory).expect("run scenario");
        assert_eq!(samples.len(), 22);
        let observed: Vec<_> = samples
            .iter()
            .take(4)
            .map(|sample| sample.implementation)
            .collect();
        assert_eq!(
            observed,
            [
                Implementation::Vaco,
                Implementation::Reference,
                Implementation::Reference,
                Implementation::Vaco,
            ]
        );
        fs::remove_dir_all(directory).expect("remove fixture directory");
    }

    #[test]
    fn rejects_fewer_than_eleven_rounds() {
        let directory = directory("rounds");
        let input = directory.join("input");
        fs::write(&input, b"fixture bytes").expect("write input");
        let error = run_macro_scenario(&scenario(), &input, 10, &directory)
            .expect_err("round count must fail");
        assert!(error.to_string().contains("at least 11"));
        fs::remove_dir_all(directory).expect("remove fixture directory");
    }
}
