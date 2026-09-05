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
use std::process::{Command, Output};
use std::time::Instant;

use vaco_corpus::ObjectId;

#[cfg(target_os = "linux")]
use crate::parse_gnu_time_v;
#[cfg(target_os = "macos")]
use crate::parse_macos_time_l;
use crate::resource::json_string;
use crate::{BenchError, CommandProvenance, ResourceObservation};

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
    /// CPU time and maximum resident memory observed for this child.
    pub resources: ResourceObservation,
    /// Exact command and version/configuration record for this child.
    pub provenance: CommandProvenance,
}

/// Render one stable, self-contained JSONL row for a macro child execution.
#[must_use]
pub fn macro_json_record(sample: &MacroSample) -> String {
    let argv = sample
        .provenance
        .argv
        .iter()
        .map(|argument| json_string(argument))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"schema\":1,\"suite\":\"macro\",\"scenario\":{},\"implementation\":{},\"round\":{},\"wall_ns\":{},\"cpu_seconds\":{},\"peak_rss_bytes\":{},\"argv\":[{}],\"version\":{}}}",
        json_string(&sample.scenario),
        json_string(sample.implementation.name()),
        sample.round,
        sample.wall_ns,
        sample.resources.cpu_seconds,
        sample.resources.peak_rss_bytes,
        argv,
        json_string(&sample.provenance.version),
    )
}

/// Validate host-independent properties of an authoritative macro manifest.
///
/// This deliberately validates only structure. The project has not yet
/// published the authoritative S1--S10 assets, command lines, or output
/// digests, so this function cannot manufacture those facts.
///
/// # Errors
///
/// Returns an error for duplicate IDs, unsupported S-series IDs, empty fields,
/// or templates that do not use exactly one whole `{input}` and `{output}`
/// argument.
pub fn validate_macro_manifest(scenarios: &[MacroScenario]) -> Result<(), BenchError> {
    let mut names = std::collections::BTreeSet::new();
    for scenario in scenarios {
        if !names.insert(&scenario.name) {
            return Err(BenchError::Macro(format!(
                "macro manifest repeats scenario {:?}",
                scenario.name
            )));
        }
        validate_scenario_id(&scenario.name)?;
        if scenario.asset.trim().is_empty() {
            return Err(BenchError::Macro(format!(
                "{} has an empty corpus asset",
                scenario.name
            )));
        }
        validate_template(&scenario.name, "vaco", &scenario.vaco)?;
        validate_template(&scenario.name, "reference", &scenario.reference)?;
    }
    Ok(())
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
    run_macro_scenario_with_launcher(scenario, input, rounds, sandbox, launch_with_resources)
}

fn run_macro_scenario_with_launcher<F>(
    scenario: &MacroScenario,
    input: &Path,
    rounds: usize,
    sandbox: &Path,
    launcher: F,
) -> Result<Vec<MacroSample>, BenchError>
where
    F: Fn(&[String], &Path) -> Result<(Output, ResourceObservation), BenchError>,
{
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
    validate_macro_manifest(std::slice::from_ref(scenario))?;
    let vaco_version = capture_version(&scenario.vaco)?;
    let reference_version = capture_version(&scenario.reference)?;
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
            let version = match implementation {
                Implementation::Vaco => &vaco_version,
                Implementation::Reference => &reference_version,
            };
            let (wall_ns, resources, argv) = run_template(
                template,
                input,
                &output,
                &scenario.expected_output,
                &launcher,
            )?;
            samples.push(MacroSample {
                scenario: scenario.name.clone(),
                implementation,
                round,
                wall_ns,
                resources,
                provenance: CommandProvenance {
                    argv,
                    version: version.clone(),
                },
            });
        }
    }
    Ok(samples)
}

fn run_template<F>(
    template: &CommandTemplate,
    input: &Path,
    output: &Path,
    expected: &ObjectId,
    launcher: &F,
) -> Result<(f64, ResourceObservation, Vec<String>), BenchError>
where
    F: Fn(&[String], &Path) -> Result<(Output, ResourceObservation), BenchError>,
{
    let _ = fs::remove_file(output);
    let argv = template.expand(input, output)?;
    let start = Instant::now();
    let (child, resources) = launcher(&argv, output)?;
    let elapsed = start.elapsed().as_secs_f64() * 1_000_000_000.0;
    if !child.status.success() {
        return Err(BenchError::Macro(format!(
            "{} exited with {}",
            template.program.display(),
            child.status
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
    Ok((elapsed, resources, argv))
}

impl CommandTemplate {
    fn expand(&self, input: &Path, output: &Path) -> Result<Vec<String>, BenchError> {
        let program = self.program.to_str().ok_or_else(|| {
            BenchError::Macro(format!(
                "{} is not valid UTF-8 and cannot be recorded exactly",
                self.program.display()
            ))
        })?;
        let input = input.to_str().ok_or_else(|| {
            BenchError::Macro(format!(
                "{} is not valid UTF-8 and cannot be recorded exactly",
                input.display()
            ))
        })?;
        let output = output.to_str().ok_or_else(|| {
            BenchError::Macro(format!(
                "{} is not valid UTF-8 and cannot be recorded exactly",
                output.display()
            ))
        })?;
        let mut argv = vec![program.to_owned()];
        argv.extend(self.args.iter().map(|argument| match argument.as_str() {
            "{input}" => input.to_owned(),
            "{output}" => output.to_owned(),
            _ => argument.clone(),
        }));
        Ok(argv)
    }
}

fn validate_scenario_id(name: &str) -> Result<(), BenchError> {
    let Some((series, configuration)) = name.split_once('/') else {
        return Err(BenchError::Macro(format!(
            "macro scenario {name:?} must be S1 through S10 plus a configuration ID"
        )));
    };
    let Some(number) = series
        .strip_prefix('S')
        .and_then(|value| value.parse::<u8>().ok())
    else {
        return Err(BenchError::Macro(format!(
            "macro scenario {name:?} must begin with S1 through S10"
        )));
    };
    if !(1..=10).contains(&number)
        || configuration
            .split('/')
            .any(|component| component.trim().is_empty())
    {
        return Err(BenchError::Macro(format!(
            "macro scenario {name:?} must be S1 through S10 plus a non-empty configuration ID"
        )));
    }
    Ok(())
}

fn validate_template(
    scenario: &str,
    implementation: &str,
    template: &CommandTemplate,
) -> Result<(), BenchError> {
    if template.program.as_os_str().is_empty() {
        return Err(BenchError::Macro(format!(
            "{scenario} {implementation} command has no program"
        )));
    }
    for placeholder in ["{input}", "{output}"] {
        let whole = template
            .args
            .iter()
            .filter(|argument| argument.as_str() == placeholder)
            .count();
        let embedded = template
            .args
            .iter()
            .any(|argument| argument.contains(placeholder) && argument.as_str() != placeholder);
        if whole != 1 || embedded {
            return Err(BenchError::Macro(format!(
                "{scenario} {implementation} command must contain exactly one whole {placeholder} argument"
            )));
        }
    }
    Ok(())
}

fn capture_version(template: &CommandTemplate) -> Result<String, BenchError> {
    let program = template.program.to_str().ok_or_else(|| {
        BenchError::Macro(format!(
            "{} is not valid UTF-8 and cannot be recorded exactly",
            template.program.display()
        ))
    })?;
    let version = Command::new(&template.program)
        .arg("-version")
        .output()
        .map_err(BenchError::Io)?;
    if !version.status.success() {
        return Err(BenchError::Macro(format!(
            "{program} -version exited {}",
            version.status
        )));
    }
    let record = if version.stdout.is_empty() {
        version.stderr
    } else {
        version.stdout
    };
    let version = String::from_utf8(record).map_err(|_| {
        BenchError::Macro(format!(
            "{program} -version did not produce UTF-8 provenance"
        ))
    })?;
    if version.trim().is_empty() {
        return Err(BenchError::Macro(format!(
            "{program} -version produced no version/configuration provenance"
        )));
    }
    Ok(version)
}

fn launch_with_resources(
    argv: &[String],
    _resource_output: &Path,
) -> Result<(Output, ResourceObservation), BenchError> {
    let Some((program, args)) = argv.split_first() else {
        return Err(BenchError::Macro("empty macro command".to_owned()));
    };
    #[cfg(target_os = "macos")]
    {
        let child = Command::new("/usr/bin/time")
            .arg("-l")
            .arg(program)
            .args(args)
            .output()
            .map_err(BenchError::Io)?;
        let diagnostics = String::from_utf8_lossy(&child.stderr);
        let resources = parse_macos_time_l(&diagnostics).map_err(BenchError::Macro)?;
        Ok((child, resources))
    }
    #[cfg(target_os = "linux")]
    {
        let time_output = _resource_output.with_extension("time");
        let _ = fs::remove_file(&time_output);
        let child = Command::new("/usr/bin/time")
            .args(["-v", "-o"])
            .arg(&time_output)
            .arg(program)
            .args(args)
            .output()
            .map_err(BenchError::Io)?;
        let text = fs::read_to_string(&time_output).map_err(|error| {
            BenchError::Macro(format!(
                "resource wrapper did not write {}: {error}",
                time_output.display()
            ))
        })?;
        fs::remove_file(&time_output).map_err(BenchError::Io)?;
        let resources = parse_gnu_time_v(&text).map_err(BenchError::Macro)?;
        Ok((child, resources))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (program, args, _resource_output);
        Err(BenchError::Macro(
            "macro resource wrapper requires macOS or Linux".to_owned(),
        ))
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "test fixture setup uses direct diagnostics"
)]
mod tests {
    use super::{
        CommandTemplate, Implementation, MacroScenario, macro_json_record, run_macro_scenario,
        run_macro_scenario_with_launcher, validate_macro_manifest,
    };
    use crate::{BenchError, ResourceObservation};
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
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

    #[cfg(unix)]
    fn fixture_program(directory: &std::path::Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let program = directory.join("fixture-command");
        fs::write(
            &program,
            "#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then\n  printf 'fixture version config=release\\n'\n  exit 0\nfi\ncp \"$1\" \"$2\"\n",
        )
        .expect("write fixture command");
        let mut permissions = fs::metadata(&program)
            .expect("read fixture command metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).expect("make fixture command executable");
        program
    }

    #[cfg(unix)]
    fn scenario(program: PathBuf) -> MacroScenario {
        let args = vec!["{input}".to_owned(), "{output}".to_owned()];
        MacroScenario {
            name: "S10/test".to_owned(),
            asset: "fixture".to_owned(),
            vaco: CommandTemplate {
                program: program.clone(),
                args: args.clone(),
            },
            reference: CommandTemplate { program, args },
            expected_output: ObjectId::of(b"fixture bytes"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn synthetic_controlled_runs_attach_provenance_and_alternate_order() {
        let directory = directory("alternating");
        let input = directory.join("input");
        fs::write(&input, b"fixture bytes").expect("write input");
        let program = fixture_program(&directory);
        let launcher = |argv: &[String], _: &std::path::Path| {
            let (program, arguments) = argv
                .split_first()
                .ok_or_else(|| BenchError::Macro("fixture argv is empty".to_owned()))?;
            let child = Command::new(program)
                .args(arguments)
                .output()
                .map_err(BenchError::Io)?;
            Ok((
                child,
                ResourceObservation {
                    cpu_seconds: 0.125,
                    peak_rss_bytes: 8192,
                },
            ))
        };
        let samples =
            run_macro_scenario_with_launcher(&scenario(program), &input, 11, &directory, launcher)
                .expect("run scenario");
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
        assert!(
            samples
                .iter()
                .all(|sample| (sample.resources.cpu_seconds - 0.125).abs() < f64::EPSILON)
        );
        assert!(
            samples
                .iter()
                .all(|sample| sample.resources.peak_rss_bytes == 8192)
        );
        assert!(
            samples
                .iter()
                .all(|sample| sample.provenance.argv.len() == 3)
        );
        assert!(
            samples
                .iter()
                .all(|sample| sample.provenance.version == "fixture version config=release\n")
        );
        let row = macro_json_record(samples.first().expect("one macro sample"));
        assert!(row.contains("\"suite\":\"macro\""));
        assert!(row.contains("\"peak_rss_bytes\":"));
        assert!(row.contains("fixture version config=release\\n"));
        fs::remove_dir_all(directory).expect("remove fixture directory");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_fewer_than_eleven_rounds() {
        let directory = directory("rounds");
        let input = directory.join("input");
        fs::write(&input, b"fixture bytes").expect("write input");
        let error = run_macro_scenario(
            &scenario(fixture_program(&directory)),
            &input,
            10,
            &directory,
        )
        .expect_err("round count must fail");
        assert!(error.to_string().contains("at least 11"));
        fs::remove_dir_all(directory).expect("remove fixture directory");
    }

    #[cfg(unix)]
    #[test]
    fn manifest_validation_rejects_structure_without_scenario_facts() {
        let directory = directory("manifest");
        let manifest = scenario(fixture_program(&directory));
        assert!(validate_macro_manifest(std::slice::from_ref(&manifest)).is_ok());
        let duplicate = validate_macro_manifest(&[manifest.clone(), manifest.clone()])
            .expect_err("duplicate scenario must fail");
        assert!(duplicate.to_string().contains("repeats scenario"));
        let mut malformed = manifest;
        malformed.name = "S11/test".to_owned();
        assert!(validate_macro_manifest(&[malformed]).is_err());
        let mut embedded = scenario(fixture_program(&directory));
        embedded.vaco.args = vec!["prefix-{input}".to_owned(), "{output}".to_owned()];
        assert!(validate_macro_manifest(&[embedded]).is_err());
        fs::remove_dir_all(directory).expect("remove fixture directory");
    }
}
